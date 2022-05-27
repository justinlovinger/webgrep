mod cache;
mod client;

use crate::cache::MemCache;
use crate::client::{url_from_nums, PseudorandomClient};
use core::str::FromStr;
use quickcheck::{Arbitrary, Gen};
use regex::Regex;
use reqwest::Url;
use std::collections::HashMap;
use std::iter::repeat_with;
use std::num::{NonZeroU16, NonZeroUsize};
use std::time::Duration;
use webgrep::client::Response;
use webgrep::run;

const MAX_MAX_DEPTH: u64 = 2;

#[derive(Clone, Debug)]
struct RunParamsWithReducedDepth {
    run_params: RunParams,
    reduced_depth: u64,
}

impl RunParamsWithReducedDepth {
    pub fn get(&self) -> &RunParams {
        &self.run_params
    }

    pub fn get_reduced(&self) -> RunParams {
        let mut run_params = self.run_params.clone();
        run_params.max_depth = self.reduced_depth;
        run_params
    }
}

impl Arbitrary for RunParamsWithReducedDepth {
    fn arbitrary(g: &mut Gen) -> Self {
        let mut run_params = RunParams::arbitrary(g);
        run_params.max_depth = u64::arbitrary(g) % MAX_MAX_DEPTH + 1;
        Self {
            reduced_depth: u64::arbitrary(g) % run_params.max_depth,
            run_params,
        }
    }
}

#[derive(Clone, Debug)]
struct RunParams {
    client: &'static PseudorandomClient,
    page_threads: NonZeroUsize,
    exclude_urls_re: &'static Option<Regex>,
    max_depth: u64,
    search_re: &'static Regex,
    urls: Vec<Url>,
}

impl Arbitrary for RunParams {
    fn arbitrary(g: &mut Gen) -> Self {
        let min_links = u8::arbitrary(g) % 5;
        let domains = NonZeroU16::arbitrary(g);
        let paths_per_domain = NonZeroU16::arbitrary(g);
        let num_urls = usize::arbitrary(g) % 11;
        Self {
            // Too high `max_links`
            // with too high `max_depth`
            // can result in excessive time and memory usage.
            client: mk_static(PseudorandomClient::new(
                min_links,
                min_links + u8::arbitrary(g) % 5,
                domains,
                paths_per_domain,
            )),
            page_threads: NonZeroUsize::arbitrary(g),
            exclude_urls_re: mk_static(None),
            max_depth: u64::arbitrary(g) % (MAX_MAX_DEPTH + 1),
            search_re: mk_static(Regex::new(".").unwrap()),
            urls: repeat_with(|| {
                // Starting URLs should be in the same set of URLs
                // generated by `PseudorandomClient`.
                Url::from_str(&url_from_nums(
                    u16::arbitrary(g) % domains,
                    u16::arbitrary(g) % paths_per_domain,
                ))
                .unwrap()
            })
            .take(num_urls)
            .collect(),
        }
    }
}

#[quickcheck_async::tokio]
async fn run_is_idempotent_with_empty_cache(run_params: RunParams) {
    let cache1 = mk_static(MemCache::new());
    let cache2 = mk_static(MemCache::new());
    assert_eq!(
        line_occurences(&run_(&run_params, cache1).await),
        line_occurences(&run_(&run_params, cache2).await)
    );
    cache1.clear();
    cache2.clear();
}

#[quickcheck_async::tokio]
async fn run_is_idempotent_with_full_cache(run_params: RunParams) {
    let cache = mk_static(MemCache::new());
    run_(&run_params, cache).await;
    assert_eq!(
        line_occurences(&run_(&run_params, cache).await),
        line_occurences(&run_(&run_params, cache).await)
    );
    cache.clear();
}

#[quickcheck_async::tokio]
async fn run_is_idempotent_with_partial_cache(run_params: RunParamsWithReducedDepth) {
    let cache1 = mk_static(MemCache::new());
    run_(&run_params.get_reduced(), cache1).await;
    let cache2 = cache1.clone();
    assert_eq!(
        line_occurences(&run_(run_params.get(), cache1).await),
        line_occurences(&run_(run_params.get(), cache2).await)
    );
    cache1.clear();
    cache2.clear();
}

#[quickcheck_async::tokio]
async fn run_finds_the_same_matches_with_empty_or_full_cache(run_params: RunParams) {
    let cache = mk_static(MemCache::new());
    assert_eq!(
        line_occurences(&run_(&run_params, cache).await),
        line_occurences(&run_(&run_params, cache).await)
    );
    cache.clear();
}

#[quickcheck_async::tokio]
async fn run_finds_the_same_matches_with_empty_or_partial_cache(
    run_params: RunParamsWithReducedDepth,
) {
    let cache1 = mk_static(MemCache::new());
    let cache2 = mk_static(MemCache::new());
    run_(&run_params.get_reduced(), cache2).await;
    assert_eq!(
        line_occurences(&run_(run_params.get(), cache1).await),
        line_occurences(&run_(run_params.get(), cache2).await)
    );
    cache1.clear();
    cache2.clear();
}

async fn run_(params: &RunParams, cache: &'static MemCache<Url, Response>) -> Vec<u8> {
    let mut buffer = Vec::new();
    run(
        &mut buffer,
        indicatif::MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden()),
        cache,
        params.client,
        Duration::ZERO,
        params.page_threads,
        params.exclude_urls_re,
        params.max_depth,
        params.search_re,
        params.urls.clone(),
    )
    .await
    .unwrap();
    buffer
}

fn mk_static<T>(x: T) -> &'static T {
    Box::leak(Box::new(x))
}

fn line_occurences(buf: &[u8]) -> HashMap<&str, u32> {
    let mut map = HashMap::new();
    for line in std::str::from_utf8(&buf).unwrap().lines() {
        let counter = map.entry(line).or_insert(0);
        *counter += 1;
    }
    map
}
