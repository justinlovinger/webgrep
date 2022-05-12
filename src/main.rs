mod cache;
mod client;
mod node;
mod task;

use crate::cache::Cache;
use crate::client::{Body, Response, SlowClient};
use crate::node::{path_to_root, Node};
use crate::task::TaskWithResource;
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
use url::Host::{Domain, Ipv4, Ipv6};

const CLEAR_CODE: &[u8] = b"\r\x1B[K";

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
        Cache::new("page-cache")
            .await
            .expect("Failed to initialize cache"),
    ));

    // Tokio uses number of CPU cores as default number of worker threads.
    // `tokio::runtime::Handle::current().metrics().num_workers()`
    // is only available in unstable Tokio.
    // A larger buffer isn't necessary faster.
    let node_buffer_size = num_cpus::get();

    // Making more than one request at a time
    // to a host
    // could result in repercussions,
    // like IP banning.
    // Most websites host all subdomains together,
    // so we to limit requests by domain,
    // not FQDN.

    let mut host_resources = HashMap::new();
    let add_url =
        |host_resources: &mut HashMap<_, (BinaryHeap<_>, TaskWithResource<_, _>)>, p, u| {
            let host = small_host_name(&u);
            match host_resources.get_mut(host) {
                Some((urls, _)) => urls.push((p, u)),
                None => {
                    let host_string = host.to_owned();
                    let mut urls = BinaryHeap::with_capacity(1);
                    urls.push((p, u));
                    host_resources.insert(
                        host_string,
                        (urls, TaskWithResource::new(SlowClient::new(master_client))),
                    );
                }
            };
        };
    args.urls
        .into_iter()
        .for_each(|u| add_url(&mut host_resources, None, u));

    let mut nodes = BinaryHeap::new();
    let mut node_tasks = Vec::with_capacity(node_buffer_size);

    // TODO: use async IO.
    let mut werr = io::BufWriter::new(io::stderr());
    let mut progress_i = std::u8::MAX;
    let mut progress_shown = false;

    let mut done_i = std::u16::MAX;

    loop {
        // We want to finish tasks
        // before queuing new requests
        // to maximize throughput
        // and search deeper nodes.

        swap_retain_mut(
            |x: &mut tokio::task::JoinHandle<_>| match x.now_or_never() {
                Some(Ok((match_data, children_data))) => {
                    if let Some(text) = match_data {
                        if progress_shown {
                            let _ = werr.write_all(CLEAR_CODE);
                            let _ = werr.flush();
                            progress_shown = false;
                        }
                        // TODO: use async IO.
                        println!("{}", text);
                    };
                    if let Some((children, (node, urls))) = children_data {
                        for node in children {
                            nodes.push(node);
                        }
                        for u in urls {
                            add_url(&mut host_resources, Some(Arc::clone(&node)), u);
                        }
                    };
                    false
                }
                Some(Err(e)) => panic!("{}", e),
                None => true,
            },
            &mut node_tasks,
        );

        // Host clients can accumulate over time,
        // but we only need actively used clients.
        host_resources.retain(|_, (urls, task)| {
            if let Some(Some(node)) = task.try_finish() {
                nodes.push(node);
            }
            task.try_start(|mut client| match urls.pop() {
                Some((p, u)) => Ok(tokio::task::spawn(async move {
                    (
                        get_with_cache(cache, &mut client, &u)
                            .await
                            .map(|body| Node::new(p, Page { url: u, body }))
                            .ok(),
                        client,
                    )
                })),
                None => Err(client),
            });
            task.is_working_or(|client| {
                debug_assert!(urls.is_empty());
                client.time_remaining() > Duration::ZERO
            })
        });

        while node_tasks.len() < node_buffer_size {
            match nodes.pop() {
                Some(node) => {
                    node_tasks.push(tokio::task::spawn(async move {
                        parse_page(cache, args.max_depth, re, exclude_urls_re, node)
                    }));
                }
                None => break,
            }
        }

        // We don't need to print progress
        // as often as we need to coordinate tasks.
        if !progress_shown || progress_i > 100 {
            progress_i = 0;

            // Search may spend a long time between matches.
            // We want to indicate progress,
            // but we don't want to clutter output.
            // Overflowing terminal width
            // may prevent clearing the line.
            let progress_line = {
                let (num_urls, num_request_tasks) =
                    host_resources
                        .values()
                        .fold((0, 0), |(x, y), (urls, task)| {
                            (x + urls.len(), if task.is_waiting() { y } else { y + 1 })
                        });
                format!(
                    "active requests: {:<2}, queued requests: {:<4}, pages searching: {:<2}, pages queued: {}",
                    num_request_tasks,
                    num_urls,
                    node_tasks.len(),
                    nodes.len()
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
            progress_shown = true;
        }
        progress_i += 1;

        // Completion only matters once.
        // We don't need to check often,
        // and loops will occur faster
        // without any tasks.
        if done_i >= 1000 {
            done_i = 0;
            if nodes.is_empty()
                && node_tasks.is_empty()
                && host_resources
                    .values()
                    .all(|(urls, task)| urls.is_empty() && task.is_waiting())
            {
                cache.flush().await.expect("Failed to flush cache");
                return Ok(());
            }
        }
        done_i += 1;

        // If we never yield,
        // tasks may never get time to complete,
        // and the program may stall.
        tokio::task::yield_now().await;
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

type RequestData = (Arc<Node<Page>>, Vec<Url>);

#[allow(clippy::type_complexity)]
fn parse_page(
    cache: &Cache<Url, Response>,
    max_depth: u64,
    re: &Regex,
    exclude_urls_re: &Option<Regex>,
    node: Node<Page>,
) -> (Option<String>, Option<(Vec<Node<Page>>, RequestData)>) {
    match &node.value().body {
        Body::Html(body) => {
            match html5ever::parse_document(RcDom::default(), Default::default())
                .from_utf8()
                .read_from(&mut body.as_bytes())
                .ok()
            {
                Some(dom) => {
                    let match_data = re
                        .is_match(&inner_text(&dom))
                        .then(|| display_node_path(&node));

                    let children_data = if node.depth() < max_depth {
                        let node_ = Arc::new(node);
                        let node_path: HashSet<_> = path_to_root(&node_).map(|x| &x.url).collect();
                        let mut children = Vec::new();
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
                                Some(Err(_)) => {}
                                None => urls.push(u),
                            });
                        Some((children, (node_, urls)))
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
