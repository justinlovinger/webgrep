mod cache;
mod client;
mod node;

use crate::cache::{Cache, SerializableResponse};
use crate::client::SlowClient;
use crate::node::{path_to_root, Node};
use clap::Parser;
use futures::future::FutureExt;
use html5ever::tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use regex::{Regex, RegexBuilder};
use reqwest::Url;
use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::default::Default;
use std::io;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use tokio::task;
use url::Host::{Domain, Ipv4, Ipv6};

const CLEAR_CODE: &[u8] = b"\r\x1B[K";

struct Page {
    url: Url,
    body: String,
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
            .expect("Failed to initialize web client"),
    ));

    let cache: &'static _ = Box::leak(Box::new(
        Cache::new().await.expect("Failed to initialize cache"),
    ));

    // Tokio uses number of CPU cores as default number of worker threads.
    // `tokio::runtime::Handle::current().metrics().num_workers()`
    // is only available in unstable Tokio.
    let node_buffer_size = 2 * num_cpus::get();

    // Making more than one request at a time
    // to a host
    // could result in repercussions,
    // like IP banning.
    // Most websites host all subdomains together,
    // so we to limit requests by domain,
    // not FQDN.
    // Mutex locking each host client
    // avoids simultaneous requests to a host.
    let mut host_clients = HashMap::new();
    let request_task = |host_clients: &mut HashMap<_, _>, p, u| {
        // TODO: only make client
        // if `get_with_cache` needs it.
        let client_ = Arc::clone(
            host_clients
                .entry(small_host_name(&u).to_owned())
                .or_insert_with(|| {
                    Arc::new(tokio::sync::Mutex::new(SlowClient::new(master_client)))
                }),
        );
        task::spawn(async move {
            get_with_cache(cache, &client_, &u)
                .await
                .map(|body| Node::new(p, Page { url: u, body }))
        })
    };
    let mut request_tasks = args
        .urls
        .into_iter()
        .map(|x| request_task(&mut host_clients, None, x))
        .collect();

    let mut nodes = BinaryHeap::new();
    let mut node_tasks = Vec::with_capacity(node_buffer_size);

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
            |x| match x.now_or_never() {
                Some(Ok(Some(node))) => {
                    nodes.push(node);
                    false
                }
                Some(Ok(None)) => false,
                Some(Err(e)) => panic!("{}", e),
                None => true,
            },
            &mut request_tasks,
        );

        swap_retain_mut(
            |x: &mut task::JoinHandle<_>| match x.now_or_never() {
                Some(Ok((mtext, murls))) => {
                    if let Some(text) = mtext {
                        let _ = werr.write_all(CLEAR_CODE);
                        let _ = werr.flush();
                        println!("{}", text);
                    };
                    if let Some((node, urls)) = murls {
                        for u in urls {
                            request_tasks.push(request_task(
                                &mut host_clients,
                                Some(Arc::clone(&node)),
                                u,
                            ));
                        }
                    };
                    false
                }
                Some(Err(e)) => panic!("{}", e),
                None => true,
            },
            &mut node_tasks,
        );

        while node_tasks.len() < node_buffer_size {
            match nodes.pop() {
                Some(node) => {
                    node_tasks.push(task::spawn(async move {
                        parse_page(args.max_depth, re, exclude_urls_re, node)
                    }));
                }
                None => break,
            }
        }

        // Host clients can accumulate over time,
        // but we only need actively used clients.
        host_clients.retain(|_, client| {
            Arc::strong_count(client) > 1
                || client
                    .try_lock()
                    .map_or(true, |x| x.time_remaining() > Duration::ZERO)
        });

        // Search may spend a long time between matches.
        // We want to indicate progress,
        // but we don't want to clutter output.
        // Overflowing terminal width
        // may prevent clearing the line.
        let progress_line = format!(
            "request tasks: {:>2}, node tasks: {:>2}, nodes: {}",
            request_tasks.len(),
            node_tasks.len(),
            nodes.len()
        );
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

        if nodes.is_empty() && node_tasks.is_empty() && request_tasks.is_empty() {
            cache.flush().await.expect("Failed to flush cache");
            return Ok(());
        }

        // If we never yield,
        // tasks may never get time to complete,
        // and the program may stall.
        task::yield_now().await;
    }
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
    cache: &Cache,
    client: &tokio::sync::Mutex<SlowClient<'a>>,
    u: &Url,
) -> Option<String> {
    match cache.get(u) {
        Some(x) => x,
        None => {
            let client = client.lock().await;
            // Multiple tasks may wait to make the same request,
            // so we should check the cache again
            // after obtaining a lock.
            match cache.get(u) {
                Some(x) => x,
                None => get_and_cache_from_web(cache, client, u).await,
            }
        }
    }
    .ok()
}

async fn get_and_cache_from_web<'a, 'b>(
    cache: &Cache,
    mut client: tokio::sync::MutexGuard<'a, SlowClient<'b>>,
    u: &Url,
) -> SerializableResponse {
    let body = client.get(u).await;

    // We would rather keep searching
    // than panic
    // or delay
    // from failed caching.
    let _ = cache.set(u, &body);

    body
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

#[allow(clippy::type_complexity)]
fn parse_page(
    max_depth: u64,
    re: &Regex,
    exclude_urls_re: &Option<Regex>,
    node: Node<Page>,
) -> (Option<String>, Option<(Arc<Node<Page>>, Vec<Url>)>) {
    match html5ever::parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .read_from(&mut node.value().body.as_bytes())
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
                        .map(|x| x.url.as_str())
                        .collect::<Vec<_>>()
                        .join(" > "),
                )
            } else {
                None
            };

            let mnodes = if node.depth() < max_depth {
                let node_ = Arc::new(node);
                let node_path: HashSet<_> = path_to_root(&node_).map(|x| &x.url).collect();
                let urls = links(&node_.value().url, &dom)
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
                    .collect::<Vec<_>>();
                Some((node_, urls))
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
