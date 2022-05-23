use crate::cache::Cache;
use crate::client::{Client, Response};
use crate::node::Node;
use crate::run::page::Page;
use regex::Regex;
use reqwest::Url;
use std::io::Write;

pub enum TaskResult<L: Client + 'static> {
    Page(crate::run::page::RunTicket),
    Request(crate::run::request::RunTicket<L>),
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    mut match_writer: impl Write,
    progress: indicatif::MultiProgress,
    cache: impl Cache<Url, Response> + Sync + 'static,
    client: impl Client + Sync + 'static,
    page_threads: usize,
    exclude_urls_re: Option<Regex>,
    max_depth: u64,
    search_re: Regex,
    urls: Vec<Url>,
) -> Result<(), Box<dyn std::error::Error>> {
    let progress_style = indicatif::ProgressStyle::default_bar()
        .template("{wide_bar} {pos:>7}/{len:<7} {msg}")
        .unwrap();
    let pages_progress = progress.add(
        indicatif::ProgressBar::new(urls.len().try_into().unwrap_or(0))
            .with_style(progress_style.clone())
            .with_message("Pages   ")
            .with_finish(indicatif::ProgressFinish::AndLeave),
    );
    let requests_progress = progress.add(
        indicatif::ProgressBar::new(0)
            .with_style(progress_style)
            .with_message("Requests")
            .with_finish(indicatif::ProgressFinish::AndLeave),
    );

    let cache_: &'static _ = Box::leak(Box::new(cache));

    let mut tasks = tokio::task::JoinSet::new();

    let mut page_runner =
        crate::run::page::Runner::new(cache_, max_depth, search_re, exclude_urls_re, page_threads);

    let mut request_runner = crate::run::request::Runner::new(cache_, client, &progress);

    urls.into_iter().for_each(|u| match cache_.get(&u) {
        Some(Ok(body)) => page_runner.push(&mut tasks, Node::new(None, Page::new(u, body))),
        Some(Err(_)) => pages_progress.inc(1),
        None => {
            requests_progress.inc_length(1);
            request_runner.push(&mut tasks, None, u);
        }
    });
    while let Some(res) = tasks.join_one().await.unwrap() {
        match res {
            TaskResult::Page(ticket) => {
                pages_progress.inc(1);
                let (match_data, children_data) = page_runner.redeem(&mut tasks, ticket);

                if let Some(s) = match_data {
                    tokio::task::block_in_place(|| {
                        progress.suspend(|| {
                            match_writer
                                .write_all(s.as_bytes())
                                .and_then(|_| match_writer.write_all(b"\n"))
                                .and_then(|_| match_writer.flush())
                                .expect("Failed to print match");
                        })
                    });
                };

                if let Some((good_cache_hits, bad_cache_hits, (parent, urls))) = children_data {
                    pages_progress.inc_length(
                        (good_cache_hits + urls.len()).try_into().unwrap_or(0) + bad_cache_hits,
                    );
                    pages_progress.inc(bad_cache_hits);
                    requests_progress.inc_length(urls.len().try_into().unwrap_or(0));
                    request_runner.extend(&mut tasks, &parent, urls);
                };
            }
            TaskResult::Request(ticket) => {
                requests_progress.inc(1);
                match request_runner.redeem(&mut tasks, ticket) {
                    Ok(page) => page_runner.push(&mut tasks, page),
                    Err(_) => pages_progress.inc(1),
                }
            }
        }
    }

    Ok(())
}

mod request {
    use crate::cache::Cache;
    use crate::client::{self, Client, Response};
    use crate::node::{Node, NodeParent};
    use crate::run::page::Page;
    use crate::run::TaskResult;
    use indicatif::{MultiProgress, ProgressStyle};
    use reqwest::Url;
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::task::JoinSet;
    use url::Host::{Domain, Ipv4, Ipv6};

    pub struct Runner<'a, C: Cache<Url, Response> + 'static, L: Client + 'static> {
        cache: &'static C,
        host_resources: HostResources<L>,
        master_client: &'static L,
        progress: &'a MultiProgress,
        spinner_style: ProgressStyle,
    }

    type HostResources<L> = HashMap<String, (BinaryHeap<RequestUrl>, ClientSlot<L>)>;
    type ClientSlot<L> = Option<SlowClient<'static, L>>;

    impl<'a, C: Cache<Url, Response> + Sync, L: Client + Sync> Runner<'a, C, L> {
        pub fn new(cache: &'static C, client: L, progress: &'a MultiProgress) -> Self {
            Self {
                cache,
                host_resources: HashMap::new(),
                master_client: Box::leak(Box::new(client)),
                progress,
                spinner_style: indicatif::ProgressStyle::default_bar()
                    .template("{spinner} {wide_msg}")
                    .unwrap(),
            }
        }

        pub fn redeem(
            &mut self,
            join_set: &mut JoinSet<TaskResult<L>>,
            ticket: RunTicket<L>,
        ) -> Result<Node<Page>, client::Error> {
            let (host, client) = ticket.1;
            match self.host_resources.get_mut(&host) {
                Some((urls, holding_space)) => match urls.pop() {
                    Some(RequestUrl(p, u)) => self.spawn(join_set, host, client, p, u),
                    None => {
                        debug_assert!(holding_space.is_none());
                        _ = holding_space.insert(client);
                    }
                },
                None => panic!("Host resource invariant failed"),
            }
            ticket.0
        }

        pub fn extend(
            &mut self,
            join_set: &mut JoinSet<TaskResult<L>>,
            parent: &Arc<Node<Page>>,
            urls: Vec<Url>,
        ) {
            // If `urls` contains more than one URL for a given host,
            // the first URL for that host may spawn a new task.
            // However,
            // we don't need to sort `urls`,
            // because nodes are sorted by depth,
            // and these URLs have the same parent
            // and therefore the same depth.
            // Furthermore,
            // we don't have to worry about a queued URL for a given host
            // having a greater value than one in `urls`,
            // because `push` won't spawn a task for a host
            // if URLs are queued for that host.
            for u in urls {
                self.push(join_set, Some(Arc::clone(parent)), u);
            }
        }

        pub fn push(
            &mut self,
            join_set: &mut JoinSet<TaskResult<L>>,
            parent: NodeParent<Page>,
            url: Url,
        ) {
            // Making more than one request at a time
            // to a host
            // could result in repercussions,
            // like IP banning.
            // Most websites host all subdomains together,
            // so we limit requests by domain,
            // not FQDN.
            let host = small_host_name(&url);
            match self.host_resources.get_mut(host) {
                Some((urls, client)) => match client.take() {
                    Some(c) => {
                        debug_assert!(urls.is_empty());
                        self.spawn(join_set, host.to_owned(), c, parent, url)
                    }
                    None => urls.push(RequestUrl(parent, url)),
                },
                None => {
                    let host_ = host.to_owned();
                    self.spawn(
                        join_set,
                        host_.clone(),
                        SlowClient::new(self.master_client),
                        parent,
                        url,
                    );
                    self.host_resources.insert(host_, (BinaryHeap::new(), None));
                }
            };
        }

        fn spawn(
            &self,
            join_set: &mut JoinSet<TaskResult<L>>,
            host: String,
            mut client: SlowClient<'static, L>,
            parent: NodeParent<Page>,
            url: Url,
        ) {
            let spinner = self.progress.add(
                indicatif::ProgressBar::new_spinner()
                    .with_style(self.spinner_style.clone())
                    .with_message(url.to_string()),
            );
            let cache = self.cache;
            join_set.spawn(async move {
                spinner.enable_steady_tick(Duration::from_millis(100));
                TaskResult::Request(RunTicket(
                    get_with_cache(cache, &mut client, &url)
                        .await
                        .map(|body| Node::new(parent, Page::new(url, body))),
                    (host, client),
                ))
            });
        }
    }

    pub struct RunTicket<L: Client + 'static>(
        Result<Node<Page>, client::Error>,
        (String, SlowClient<'static, L>),
    );

    struct RequestUrl(NodeParent<Page>, Url);

    impl Ord for RequestUrl {
        fn cmp(&self, other: &Self) -> Ordering {
            match &self.0 {
                Some(x) => match &other.0 {
                    Some(y) => x.depth().cmp(&y.depth()),
                    None => Ordering::Greater,
                },
                None => match other.0 {
                    Some(_) => Ordering::Less,
                    None => Ordering::Equal,
                },
            }
        }
    }

    impl PartialOrd for RequestUrl {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Eq for RequestUrl {}

    impl PartialEq for RequestUrl {
        fn eq(&self, other: &Self) -> bool {
            match &self.0 {
                Some(x) => match &other.0 {
                    Some(y) => x.depth() == y.depth(),
                    None => false,
                },
                None => other.0.is_none(),
            }
        }
    }

    fn small_host_name(url: &Url) -> &str {
        match url.host() {
            Some(Domain(x)) => {
                match x.rmatch_indices('.').nth(1) {
                    // Slice is safe,
                    // because `.` is one byte
                    // `rmatch_indices` always returns valid indices,
                    // and there will always be at least one character
                    // after the second match from the right.
                    Some((i, _)) => unsafe { x.get_unchecked(i + 1..) },
                    None => x,
                }
            }
            Some(Ipv4(_)) => url.host_str().unwrap(),
            Some(Ipv6(_)) => url.host_str().unwrap(),
            None => "",
        }
    }

    async fn get_with_cache<'a>(
        cache: &impl Cache<Url, Response>,
        client: &mut SlowClient<'a, impl Client>,
        url: &Url,
    ) -> Response {
        match cache.get(url) {
            Some(x) => x,
            None => get_and_cache_from_web(cache, client, url).await,
        }
    }

    async fn get_and_cache_from_web<'a>(
        cache: &impl Cache<Url, Response>,
        client: &mut SlowClient<'a, impl Client>,
        url: &Url,
    ) -> Response {
        let body = client.get(url).await;

        // We would rather keep searching
        // than panic
        // or delay
        // from failed caching.
        let _ = cache.set(url, &body);

        body
    }

    pub struct SlowClient<'a, L: Client> {
        client: &'a L,
        last_request_finished: Option<Instant>,
    }

    impl<'a, L: Client> SlowClient<'a, L> {
        pub fn new(client: &'a L) -> Self {
            Self {
                client,
                last_request_finished: None,
            }
        }

        pub fn time_remaining(&self) -> Duration {
            self.last_request_finished
                .and_then(|x| Duration::from_secs(1).checked_sub(x.elapsed()))
                .unwrap_or(Duration::ZERO)
        }

        pub async fn get(&mut self, url: &Url) -> Response {
            // Making web requests
            // at the speed of a computer
            // can have negative repercussions,
            // like IP banning.
            let time_remaining = self.time_remaining();
            if time_remaining > Duration::ZERO {
                tokio::time::sleep(time_remaining).await;
            }
            let body = self.client.get(url).await;
            self.last_request_finished = Some(Instant::now());
            body
        }
    }
}

mod page {
    use crate::cache::Cache;
    use crate::client::{Body, Client, Response};
    use crate::node::{path_to_root, Node};
    use crate::run::TaskResult;
    use html5ever::tendril::TendrilSink;
    use markup5ever_rcdom::{Handle, NodeData, RcDom};
    use regex::Regex;
    use reqwest::Url;
    use std::cmp::Ordering;
    use std::collections::BinaryHeap;
    use std::collections::HashSet;
    use std::default::Default;
    use std::ops::Deref;
    use std::sync::Arc;
    use tokio::task::JoinSet;

    pub struct Runner<C: Cache<Url, Response> + 'static> {
        cache: &'static C,
        max_depth: u64,
        search_re: &'static Regex,
        exclude_urls_re: &'static Option<Regex>,
        max_tasks: usize,
        num_tasks: usize,
        queue: BinaryHeap<PageNode>,
    }

    impl<C: Cache<Url, Response> + Sync> Runner<C> {
        pub fn new(
            cache: &'static C,
            max_depth: u64,
            search_re: Regex,
            exclude_urls_re: Option<Regex>,
            max_tasks: usize,
        ) -> Self {
            Self {
                cache,
                max_depth,
                search_re: Box::leak(Box::new(search_re)),
                exclude_urls_re: Box::leak(Box::new(exclude_urls_re)),
                max_tasks,
                num_tasks: 0,
                queue: BinaryHeap::new(),
            }
        }

        pub fn redeem(
            &mut self,
            join_set: &mut JoinSet<TaskResult<impl Client + Sync>>,
            ticket: RunTicket,
        ) -> RunOutput {
            self.num_tasks -= 1;
            (
                ticket.0,
                match ticket.1 {
                    Some((pages, bad_cache_hits, request_data)) => {
                        let good_cache_hits = pages.len();
                        self.extend(join_set, pages);
                        Some((good_cache_hits, bad_cache_hits, request_data))
                    }
                    None => {
                        if let Some(page) = self.queue.pop() {
                            self.spawn(join_set, page.into());
                        }
                        None
                    }
                },
            )
        }

        fn extend(
            &mut self,
            join_set: &mut JoinSet<TaskResult<impl Client + Sync>>,
            pages: Vec<Node<Page>>,
        ) {
            // We want to add as many pages as possible
            // before picking the best pages
            // to start as tasks,
            // but we don't want to unnecessarily add pages to the queue.
            let n = self.max_tasks - self.num_tasks;
            if self.queue.is_empty() && n >= pages.len() {
                for page in pages {
                    self.spawn(join_set, page);
                }
            } else if n >= pages.len() + self.queue.len() {
                for page in pages {
                    self.spawn(join_set, page);
                }
                while let Some(page) = self.queue.pop() {
                    self.spawn(join_set, page.into());
                }
                debug_assert!(self.queue.is_empty());
            } else {
                self.queue.extend(pages.into_iter().map(|page| page.into()));
                for _ in 0..n {
                    match self.queue.pop() {
                        Some(page) => self.spawn(join_set, page.into()),
                        None => break,
                    }
                }
                debug_assert_eq!(self.num_tasks, self.max_tasks);
                debug_assert!(!self.queue.is_empty());
            }
        }

        pub fn push(
            &mut self,
            join_set: &mut JoinSet<TaskResult<impl Client + Sync>>,
            page: Node<Page>,
        ) {
            if self.num_tasks < self.max_tasks {
                debug_assert!(self.queue.is_empty());
                self.spawn(join_set, page)
            } else {
                self.queue.push(page.into())
            }
        }

        fn spawn(
            &mut self,
            join_set: &mut JoinSet<TaskResult<impl Client + Sync>>,
            page: Node<Page>,
        ) {
            self.num_tasks += 1;
            let cache = self.cache;
            let max_depth = self.max_depth;
            let search_re = self.search_re;
            let exclude_urls_re = self.exclude_urls_re;
            join_set.spawn(async move {
                TaskResult::Page(parse_page(
                    cache,
                    max_depth,
                    search_re,
                    exclude_urls_re,
                    page,
                ))
            })
        }
    }

    pub struct RunTicket(
        MatchData,
        Option<(Vec<Node<Page>>, BadCacheHits, RequestData)>,
    );

    pub type RunOutput = (
        MatchData,
        Option<(GoodCacheHits, BadCacheHits, RequestData)>,
    );
    pub type MatchData = Option<String>;
    pub type GoodCacheHits = usize;
    pub type BadCacheHits = u64;
    pub type RequestData = (Arc<Node<Page>>, Vec<Url>);

    struct PageNode(Node<Page>);

    impl Deref for PageNode {
        type Target = Node<Page>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl From<Node<Page>> for PageNode {
        fn from(page: Node<Page>) -> Self {
            PageNode(page)
        }
    }

    impl From<PageNode> for Node<Page> {
        fn from(page: PageNode) -> Self {
            page.0
        }
    }

    impl Ord for PageNode {
        fn cmp(&self, other: &Self) -> Ordering {
            self.depth().cmp(&other.depth())
        }
    }

    impl PartialOrd for PageNode {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Eq for PageNode {}

    impl PartialEq for PageNode {
        fn eq(&self, other: &Self) -> bool {
            self.depth() == other.depth()
        }
    }

    pub struct Page {
        url: Url,
        body: Body,
    }

    impl Page {
        pub fn new(url: Url, body: Body) -> Self {
            Self { url, body }
        }
    }

    fn parse_page(
        cache: &impl Cache<Url, Response>,
        max_depth: u64,
        search_re: &Regex,
        exclude_urls_re: &Option<Regex>,
        node: Node<Page>,
    ) -> RunTicket {
        match &node.value().body {
            Body::Html(body) => {
                match html5ever::parse_document(RcDom::default(), Default::default())
                    .from_utf8()
                    .read_from(&mut body.as_bytes())
                    .ok()
                {
                    Some(dom) => {
                        // Matches may span DOM nodes,
                        // so we can't just check DOM nodes individually.
                        let match_data = search_re
                            .is_match(&inner_text(&dom))
                            .then(|| display_node_path(&node));

                        let children_data = if node.depth() < max_depth {
                            let node_ = Arc::new(node);
                            let node_path: HashSet<_> =
                                path_to_root(&node_).map(|x| &x.url).collect();
                            let mut children = Vec::new();
                            let mut page_errors = 0;
                            let mut urls = Vec::new();
                            links(&node_.value().url, &dom)
                                .into_iter()
                                // We don't need to know if a path cycles back on itself.
                                // For us,
                                // path cycles waste time and lead to infinite loops.
                                .filter(|u| !node_path.contains(&u))
                                // We're hoping the Rust compiler optimizes this branch
                                // out of the loop.
                                .filter(|u| {
                                    exclude_urls_re
                                        .as_ref()
                                        .map_or(true, |re| !re.is_match(u.as_str()))
                                })
                                .for_each(|u| match cache.get(&u) {
                                    Some(Ok(body)) => children.push(Node::new(
                                        Some(Arc::clone(&node_)),
                                        Page::new(u, body),
                                    )),
                                    Some(Err(_)) => page_errors += 1,
                                    None => urls.push(u),
                                });
                            Some((children, page_errors, (node_, urls)))
                        } else {
                            None
                        };

                        RunTicket(match_data, children_data)
                    }
                    None => RunTicket(None, None),
                }
            }
            // TODO: decompress PDF if necessary.
            Body::Pdf(raw) => RunTicket(
                search_re.is_match(raw).then(|| display_node_path(&node)),
                None,
            ),
            Body::Plain(text) => RunTicket(
                search_re.is_match(text).then(|| display_node_path(&node)),
                None,
            ),
        }
    }

    fn display_node_path(node: &Node<Page>) -> String {
        // `map(...).intersperse(" > ")` would be better,
        // but it is only available in nightly builds,
        // as of 2022-04-18.
        node.path_from_root()
            .iter()
            .map(|x| x.url.as_str())
            .collect::<Vec<_>>()
            .join(" > ")
    }

    fn inner_text(dom: &RcDom) -> String {
        let mut s = String::new();
        walk_dom(
            &mut |data| {
                match data {
                    NodeData::Text { contents } => {
                        s.push_str(contents.borrow().as_ref());
                    }
                    NodeData::Element { name, .. } => {
                        // We want to search like a person viewing the page,
                        // so we ignore invisible tags.
                        if ["head", "script"].contains(&name.local.as_ref()) {
                            return false;
                        }
                    }
                    _ => {}
                }
                true
            },
            &dom.document,
        );
        s
    }

    // We only want unique links.
    // `HashSet` takes care of this.
    fn links(origin: &Url, dom: &RcDom) -> HashSet<Url> {
        let mut xs = HashSet::new();
        walk_dom(
            &mut |data| {
                if let NodeData::Element { name, attrs, .. } = data {
                    if name.local.as_ref() == "a" {
                        attrs
                            .borrow()
                            .iter()
                            .filter(|x| x.name.local.as_ref() == "href")
                            .take(1) // An `a` tag shouldn't have more than one `href`
                            .filter_map(|x| origin.join(&x.value).ok())
                            .for_each(|x| {
                                xs.insert(x);
                            });
                    }
                }
                true
            },
            &dom.document,
        );
        xs
    }

    fn walk_dom<F>(f: &mut F, handle: &Handle)
    where
        F: FnMut(&NodeData) -> bool,
    {
        if f(&handle.data) {
            if let NodeData::Element {
                template_contents: Some(inner),
                ..
            } = &handle.data
            {
                walk_dom(f, inner);
            }
            for child in handle.children.borrow().iter() {
                walk_dom(f, child);
            }
        }
    }
}
