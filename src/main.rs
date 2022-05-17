mod cache;
mod client;
mod node;

use crate::cache::Cache;
use crate::client::{Body, Response, SlowClient};
use crate::node::{path_to_root, Node};
use clap::Parser;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use regex::{Regex, RegexBuilder};
use reqwest::Url;
use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::default::Default;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use url::Host::{Domain, Ipv4, Ipv6};

enum TaskResult<'a> {
    Page(ParseResult),
    Request((Result<Node<Page>, client::Error>, (String, SlowClient<'a>))),
}

type ParseResult = (Option<String>, Option<(Vec<Node<Page>>, u64, RequestData)>);
type RequestData = (Arc<Node<Page>>, Vec<Url>);

struct Page {
    url: Url,
    body: Body,
}

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Regex pattern to search for
    #[clap(required = true, value_name = "PATTERN")]
    pattern_re: Regex,

    /// URIs to start search from
    #[clap(multiple_occurrences = true, required = true, value_name = "URL")]
    urls: Vec<Url>,

    /// Limit search depth to NUM links from starting URL
    #[clap(short = 'd', long, default_value_t = 1, value_name = "NUM")]
    max_depth: u64,

    /// Search case insensitively
    #[clap(short = 'i', long)]
    ignore_case: bool,

    /// Exclude URLs matching regex pattern
    #[clap(long, value_name = "PATTERN")]
    exclude_urls_re: Option<Regex>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let re: &'static _ = Box::leak(Box::new(
        RegexBuilder::new(args.pattern_re.as_str())
            .case_insensitive(args.ignore_case)
            .build()
            .unwrap(),
    ));

    let exclude_urls_re: &'static _ = Box::leak(Box::new(args.exclude_urls_re));

    let master_client: &'static _ = Box::leak(Box::new(
        reqwest::Client::builder()
            // `timeout` doesn't work without `connect_timeout`.
            .connect_timeout(core::time::Duration::from_secs(60))
            .timeout(core::time::Duration::from_secs(60))
            .build()
            .expect("Failed to initialize web client"),
    ));

    let cache: &'static _ = Box::leak(Box::new(
        Cache::new("page-cache")
            .await
            .expect("Failed to initialize cache"),
    ));

    let mut wout = std::io::BufWriter::new(std::io::stdout());

    let progress = indicatif::MultiProgress::new();
    let progress_style = indicatif::ProgressStyle::default_bar()
        .template("{wide_bar} {pos:>7}/{len:<7} {msg}")
        .unwrap();
    let spinner_style = indicatif::ProgressStyle::default_bar()
        .template("{spinner} {wide_msg}")
        .unwrap();
    let pages_progress = progress.add(
        indicatif::ProgressBar::new(args.urls.len().try_into().unwrap_or(0))
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

    // Tokio uses number of CPU cores as default number of worker threads.
    // `tokio::runtime::Handle::current().metrics().num_workers()`
    // is only available in unstable Tokio.
    // A larger buffer isn't necessary faster.
    let page_buffer_size = num_cpus::get();

    let mut tasks = tokio::task::JoinSet::new();

    let request_task = |host, mut client, p, u: Url| {
        let spinner = progress.add(
            indicatif::ProgressBar::new_spinner()
                .with_style(spinner_style.clone())
                .with_message(u.to_string()),
        );
        async move {
            spinner.enable_steady_tick(Duration::from_millis(100));
            TaskResult::Request((
                get_with_cache(cache, &mut client, &u)
                    .await
                    .map(|body| Node::new(p, Page { url: u, body })),
                (host, client),
            ))
        }
    };

    // Making more than one request at a time
    // to a host
    // could result in repercussions,
    // like IP banning.
    // Most websites host all subdomains together,
    // so we to limit requests by domain,
    // not FQDN.
    let mut host_resources = HashMap::new();
    let add_url = |tasks: &mut tokio::task::JoinSet<_>,
                   host_resources: &mut HashMap<_, (BinaryHeap<_>, Option<_>)>,
                   p,
                   u| {
        let host = small_host_name(&u);
        match host_resources.get_mut(host) {
            Some((urls, client)) => match client.take() {
                Some(c) => tasks.spawn(request_task(host.to_owned(), c, p, u)),
                None => urls.push((p, u)),
            },
            None => {
                let host_ = host.to_owned();
                tasks.spawn(request_task(
                    host_.clone(),
                    SlowClient::new(master_client),
                    p,
                    u,
                ));
                host_resources.insert(host_, (BinaryHeap::new(), None));
            }
        };
    };

    let mut pages = BinaryHeap::new();
    let mut num_page_tasks = 0;

    let page_task = |page| async move {
        TaskResult::Page(parse_page(cache, args.max_depth, re, exclude_urls_re, page))
    };

    args.urls.into_iter().for_each(|u| match cache.get(&u) {
        Some(Ok(body)) => {
            let page = Node::new(None, Page { url: u, body });
            if num_page_tasks < page_buffer_size {
                num_page_tasks += 1;
                tasks.spawn(page_task(page))
            } else {
                pages.push(page)
            }
        }
        Some(Err(_)) => pages_progress.inc(1),
        None => {
            requests_progress.inc_length(1);
            add_url(&mut tasks, &mut host_resources, None, u);
        }
    });
    while let Some(res) = tasks.join_one().await.unwrap() {
        match res {
            TaskResult::Page((match_data, children_data)) => {
                num_page_tasks -= 1;
                pages_progress.inc(1);
                if let Some(s) = match_data {
                    tokio::task::block_in_place(|| {
                        progress.suspend(|| {
                            wout.write_all(s.as_bytes())
                                .and_then(|_| wout.write_all(b"\n"))
                                .and_then(|_| wout.flush())
                                .expect("Failed to print match");
                        })
                    });
                };

                if let Some((children, page_errors, (node, urls))) = children_data {
                    pages_progress.inc_length(
                        (children.len() + urls.len()).try_into().unwrap_or(0) + page_errors,
                    );
                    pages_progress.inc(page_errors);
                    for page in children {
                        pages.push(page);
                        // add_page(&mut tasks, &mut num_page_tasks, page);
                    }
                    requests_progress.inc_length(urls.len().try_into().unwrap_or(0));
                    for u in urls {
                        // TODO: add all URLs
                        // before starting request tasks,
                        // in case we have more than one URL
                        // for the same host.
                        add_url(&mut tasks, &mut host_resources, Some(Arc::clone(&node)), u);
                    }
                };

                // We want to add as many pages as possible
                // before picking the best pages
                // to start as tasks.
                while num_page_tasks < page_buffer_size {
                    match pages.pop() {
                        Some(page) => {
                            num_page_tasks += 1;
                            tasks.spawn(page_task(page));
                        }
                        None => break,
                    }
                }
            }
            TaskResult::Request((response, (host, client))) => {
                requests_progress.inc(1);
                match response {
                    Ok(page) => {
                        if num_page_tasks < page_buffer_size {
                            num_page_tasks += 1;
                            tasks.spawn(page_task(page))
                        } else {
                            pages.push(page)
                        }
                    }
                    Err(_) => pages_progress.inc(1),
                }

                match host_resources.get_mut(&host) {
                    Some((urls, holding_space)) => match urls.pop() {
                        Some((p, u)) => tasks.spawn(request_task(host, client, p, u)),
                        None => {
                            debug_assert!(holding_space.is_none());
                            _ = holding_space.insert(client);
                        }
                    },
                    None => panic!("Host resource invariant failed"),
                }
            }
        }
    }

    Ok(())
}

fn small_host_name(u: &Url) -> &str {
    match u.host() {
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
        Some(Ipv4(_)) => u.host_str().unwrap(),
        Some(Ipv6(_)) => u.host_str().unwrap(),
        None => "",
    }
}

async fn get_with_cache<'a>(
    cache: &Cache<Url, Response>,
    client: &mut SlowClient<'a>,
    u: &Url,
) -> Response {
    match cache.get(u) {
        Some(x) => x,
        None => get_and_cache_from_web(cache, client, u).await,
    }
}

async fn get_and_cache_from_web<'a>(
    cache: &Cache<Url, Response>,
    client: &mut SlowClient<'a>,
    u: &Url,
) -> Response {
    let body = client.get(u).await;

    // We would rather keep searching
    // than panic
    // or delay
    // from failed caching.
    let _ = cache.set(u, &body);

    body
}

fn parse_page(
    cache: &Cache<Url, Response>,
    max_depth: u64,
    re: &Regex,
    exclude_urls_re: &Option<Regex>,
    node: Node<Page>,
) -> ParseResult {
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
                    let match_data = re
                        .is_match(&inner_text(&dom))
                        .then(|| display_node_path(&node));

                    let children_data = if node.depth() < max_depth {
                        let node_ = Arc::new(node);
                        let node_path: HashSet<_> = path_to_root(&node_).map(|x| &x.url).collect();
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
                                    Page { url: u, body },
                                )),
                                Some(Err(_)) => page_errors += 1,
                                None => urls.push(u),
                            });
                        Some((children, page_errors, (node_, urls)))
                    } else {
                        None
                    };

                    (match_data, children_data)
                }
                None => (None, None),
            }
        }
        // TODO: decompress PDF if necessary.
        Body::Pdf(raw) => (re.is_match(raw).then(|| display_node_path(&node)), None),
        Body::Plain(text) => (re.is_match(text).then(|| display_node_path(&node)), None),
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
