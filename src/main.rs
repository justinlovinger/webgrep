mod node;

use crate::node::{path_to_root, Node};
use clap::Parser;
use futures::future::FutureExt;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use regex::{Regex, RegexBuilder};
use reqwest::Url;
use std::collections::hash_map::DefaultHasher;
use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::default::Default;
use std::hash::{Hash, Hasher};
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::{task, time};
use url::Host::{Domain, Ipv4, Ipv6};

const CLEAR_CODE: &[u8] = b"\r\x1B[K";

const BODY_SIZE_LIMIT: u64 = 104857600; // bytes

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
async fn main() -> Result<(), reqwest::Error> {
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
            .unwrap(),
    ));

    let cache_dir: &'static _ = Box::leak(Box::new(
        std::env::var("XDG_CACHE_HOME")
            .map_or(
                Path::new(std::env::var("HOME").unwrap().as_str()).join(".cache"),
                PathBuf::from,
            )
            .join("web-grep"),
    ));
    std::fs::create_dir_all(cache_dir).unwrap();

    // Tokio uses number of CPU cores as default number of worker threads.
    // `tokio::runtime::Handle::current().metrics().num_workers()`
    // is only available in unstable Tokio.
    let page_buffer_size = 10 * num_cpus::get();

    let mut page_tasks = Vec::with_capacity(page_buffer_size);

    let mut host_resources = HashMap::new();
    let add_url =
        |node: Node<Url>,
         host_resources: &mut HashMap<_, (BinaryHeap<_>, _, Option<task::JoinHandle<_>>)>| {
            // Making more than one request at a time
            // to a host
            // could result in repercussions,
            // like IP banning.
            // Most websites host all subdomains together,
            // so we to limit requests by domain,
            // not FQDN.
            let host = match node.value().host() {
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
                Some(Ipv4(_)) => node.value().host_str().unwrap(),
                Some(Ipv6(_)) => node.value().host_str().unwrap(),
                None => "",
            };
            match host_resources.get_mut(host) {
                Some((urls, _, _)) => urls.push(node),
                None => {
                    let host_string = host.to_owned();
                    let mut urls = BinaryHeap::with_capacity(1);
                    urls.push(node);
                    host_resources.insert(
                        host_string,
                        (
                            urls,
                            // Mutex locking each host client
                            // avoids simultaneous requests to a host.
                            Arc::new(tokio::sync::Mutex::new(CachingClient::new(
                                SlowClient::new(master_client),
                                cache_dir,
                            ))),
                            Option::None,
                        ),
                    );
                }
            };
        };
    args.urls
        .into_iter()
        .map(|x| Node::new(None, x))
        .for_each(|x| add_url(x, &mut host_resources));

    let mut werr = io::BufWriter::new(io::stderr());
    loop {
        // Despite the name,
        // `now_or_never` doesn't make the `Future` return "never"
        // if it doesn't return "now".

        // We want to finish tasks
        // before queuing new requests
        // to maximize throughput
        // and search deeper nodes.

        swap_retain_mut(
            |x: &mut task::JoinHandle<_>| match x.now_or_never() {
                Some(Ok((mtext, mnodes))) => {
                    if let Some(text) = mtext {
                        let _ = werr.write_all(CLEAR_CODE);
                        let _ = werr.flush();
                        println!("{}", text);
                    };
                    if let Some(nodes) = mnodes {
                        for node in nodes {
                            add_url(node, &mut host_resources);
                        }
                    };
                    false
                }
                Some(Err(e)) => panic!("{}", e),
                None => true,
            },
            &mut page_tasks,
        );

        if page_tasks.len() < page_buffer_size {
            host_resources.retain(|_, (urls, client, task)| {
                if let Some(x) = task
                    .take()
                    .and_then(|mut task| match (&mut task).now_or_never() {
                        Some(Ok((mbody, node))) => {
                            if let Some(body) = mbody {
                                page_tasks.push(task::spawn(async move {
                                    parse_page(args.max_depth, re, exclude_urls_re, node, body)
                                }));
                            };
                            None
                        }
                        Some(Err(e)) => panic!("{}", e),
                        None => Some(task),
                    })
                    .or_else(|| {
                        urls.pop().map(|x| {
                            let client_ = Arc::clone(client);
                            task::spawn(
                                async move { (client_.lock().await.get(x.value()).await, x) },
                            )
                        })
                    })
                {
                    debug_assert!(task.is_none());
                    _ = task.insert(x);
                    true
                } else {
                    debug_assert!(task.is_none() && urls.is_empty());
                    // TODO: replace `false` with `client.client().time_remaining() > 0`.
                    false
                }
            });
        }

        // Search may spend a long time between matches.
        // We want to indicate progress,
        // but we don't want to clutter output.
        // Overflowing terminal width
        // may prevent clearing the line.
        let progress_line = {
            let (num_urls, num_request_tasks) = host_resources
                .values()
                .fold((0, 0), |(x, y), (urls, _, task)| {
                    (x + urls.len(), y + task.as_ref().map_or(0, |_| 1))
                });
            format!(
                "request tasks: {:>2}, page tasks: {:>2}, urls: {}",
                num_request_tasks,
                page_tasks.len(),
                num_urls
            )
        };
        let _ = werr.write_all(CLEAR_CODE);
        let _ = match terminal_size::terminal_size() {
            Some((terminal_size::Width(w), _)) => {
                let s = progress_line.as_bytes();
                // Slice is safe
                // because the string will never be longer than itself.
                werr.write_all(unsafe { s.get_unchecked(..std::cmp::min(s.len(), w.into())) })
            }
            None => werr.write_all(progress_line.as_bytes()),
        };
        let _ = werr.flush();

        if page_tasks.is_empty()
            && host_resources
                .values()
                .all(|(urls, _, task)| urls.is_empty() && task.is_none())
        {
            return Ok(());
        }

        // If we never yield,
        // tasks may never get time to complete,
        // and the program may stall.
        task::yield_now().await;
    }
}

// Like experimental `drain_filter`:
fn swap_retain_mut<F, T>(mut f: F, xs: &mut Vec<T>)
where
    F: FnMut(&mut T) -> bool,
{
    let mut i = 0;
    while i < xs.len() {
        if f(&mut xs[i]) {
            i += 1;
        } else {
            xs.swap_remove(i);
        };
    }
}

struct CachingClient<'a> {
    client: SlowClient<'a>,
    cache_dir: &'a Path,
}

type SerializableResponse = Result<String, String>;

impl<'a> CachingClient<'a> {
    pub fn new(client: SlowClient<'a>, cache_dir: &'a Path) -> Self {
        Self { client, cache_dir }
    }

    pub async fn get(&self, u: &Url) -> Option<String> {
        match self.get_from_cache(u).await {
            Some(x) => x,
            None => self.get_and_cache_from_web(u).await,
        }
        .ok()
    }

    async fn get_from_cache(&self, u: &Url) -> Option<SerializableResponse> {
        // `bincode::deserialize_from` may panic
        // if file contents don't match expected format.
        tokio::fs::read(self.cache_path(u))
            .await
            .ok()
            .and_then(|x| bincode::deserialize(&x).ok())
    }

    async fn get_and_cache_from_web(&self, u: &Url) -> SerializableResponse {
        let body = self.client.get(u).await;

        task::block_in_place(|| {
            // We would rather keep searching
            // than panic
            // or delay
            // from failed caching.
            if let Ok(file) = std::fs::File::create(self.cache_path(u)) {
                let _ = bincode::serialize_into(io::BufWriter::new(file), &body);
            }
        });

        body
    }

    fn cache_path(&self, u: &Url) -> PathBuf {
        let mut filename = u.host_str().unwrap_or("nohost").to_owned();
        filename.push('-');
        filename.push_str(self.url_hash(u).to_string().as_str());
        self.cache_dir.join(filename)
    }

    fn url_hash(&self, u: &Url) -> u64 {
        let mut s = DefaultHasher::new();
        u.hash(&mut s);
        s.finish()
    }
}

struct SlowClient<'a> {
    client: &'a reqwest::Client,
}

impl<'a> SlowClient<'a> {
    pub fn new(client: &'a reqwest::Client) -> Self {
        Self { client }
    }

    pub async fn get(&self, u: &Url) -> SerializableResponse {
        // Making web requests
        // at the speed of a computer
        // can have negative repercussions,
        // like IP banning.
        // TODO: sleep based on time since last request to this host:
        // let time_left = 1 - (time - last_time)
        // if time_left > 0 {
        //     time::sleep(time::Duration::from_secs(time_left)).await;
        // }
        time::sleep(time::Duration::from_secs(1)).await;
        // The type we serialize must match what is expected by `get_from_cache`.
        let body = match self.client.get(u.as_ref()).send().await {
            Ok(r) => {
                if r.content_length().map_or(true, |x| x < BODY_SIZE_LIMIT) {
                    // TODO: incrementally read with `chunk`,
                    // short circuit if bytes gets too long,
                    // and decode with source from `text_with_charset`.
                    r.text().await.map_err(|e| e.to_string())
                } else {
                    Err(format!(
                        "Response too long: {}",
                        r.content_length().unwrap_or(0)
                    ))
                }
            }
            Err(e) => Err(e.to_string()),
        };
        // TODO: update last request time.
        body
    }
}

fn parse_page(
    max_depth: u64,
    re: &Regex,
    exclude_urls_re: &Option<Regex>,
    node: Node<Url>,
    body: String,
) -> (Option<String>, Option<Vec<Node<Url>>>) {
    match html5ever::parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut body.as_bytes())
        .ok()
    {
        Some(dom) => {
            let mtext = if re.is_match(&inner_text(&dom)) {
                // `map(...).intersperse(" > ")` would be better,
                // but it is only available in nightly builds
                // as of 2022-04-18.
                Some(
                    node.path_from_root()
                        .iter()
                        .map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" > "),
                )
            } else {
                None
            };

            let mnodes = if node.depth() < max_depth {
                let node_ = Arc::new(node);
                let node_path: HashSet<_> = path_to_root(&node_).collect();
                Some(
                    links(node_.value(), &dom)
                        .into_iter()
                        // We don't need to know if a path cycles back on itself.
                        // For us,
                        // path cycles waste time and lead to infinite loops.
                        .filter(|x| !node_path.contains(&x))
                        // We're hoping the Rust compiler optimizes this branch
                        // out of the loop.
                        .filter(|x| {
                            exclude_urls_re
                                .as_ref()
                                .map_or(true, |re| !re.is_match(x.as_str()))
                        })
                        .map(|x| Node::new(Some(Arc::clone(&node_)), x))
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            };

            (mtext, mnodes)
        }
        None => (None, None),
    }
}

fn inner_text(dom: &RcDom) -> String {
    let mut text = String::new();
    walk_dom(
        &mut |data| {
            match data {
                NodeData::Text { ref contents } => {
                    text.push_str(contents.borrow().to_string().as_str());
                }
                NodeData::Element { ref name, .. } => {
                    // The contents of script tags are invisible
                    // and shouldn't be searched.
                    if name.local.as_ref() == "script" {
                        return false;
                    }
                }
                _ => {}
            }
            true
        },
        &dom.document,
    );
    text
}

// We only want unique links.
// `HashSet` takes care of this.
fn links(origin: &Url, dom: &RcDom) -> HashSet<Url> {
    let mut xs = HashSet::new();
    walk_dom(
        &mut |data| {
            if let NodeData::Element {
                ref name,
                ref attrs,
                ..
            } = data
            {
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
        for child in handle.children.borrow().iter() {
            walk_dom(f, child);
        }
    }
}
