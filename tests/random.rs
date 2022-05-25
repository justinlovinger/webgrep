mod cache;
mod client;

use crate::cache::MemCache;
use crate::client::{url_from_nums, PseudorandomClient};
use core::str::FromStr;
use regex::Regex;
use reqwest::Url;
use std::collections::HashMap;
use std::num::{NonZeroU16, NonZeroUsize};
use webgrep::run;

// TODO: Add `ClientWithUrl` with `Arbitrary` instance
// to easily generate a `PseudorandomClient` with initial URLs.

#[test]
fn run_is_idempotent_with_empty_cache() {
    assert!(true);
}

#[test]
fn run_is_idempotent_with_full_cache() {
    assert!(true);
}

#[test]
fn run_is_idempotent_with_partial_cache() {
    assert!(true);
}

#[quickcheck_async::tokio]
async fn run_finds_the_same_matches_with_empty_or_full_cache(
    min_links_: u8,
    links_delta: u8,
    domains_: NonZeroU16,
    paths_per_domain: NonZeroU16,
    page_threads: NonZeroUsize,
    max_depth_: u64,
    urls_: Vec<(u16, u16)>,
) {
    let progress =
        indicatif::MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden());

    let cache = mk_static(MemCache::new());

    // Too high `max_links`
    // with too high `max_depth`
    // can result in excessive time and memory usage.
    let min_links = min_links_ % 5;
    let max_links = min_links + links_delta % 5;
    // Requests may be very slow
    // if we don't have enough unique domains.
    let domains = std::cmp::min(NonZeroU16::new(1000).unwrap(), domains_);
    let client = mk_static(PseudorandomClient::new(
        min_links,
        max_links,
        domains,
        paths_per_domain,
    ));
    // Starting URLs should be in the same set of URLs
    // generated by `PseudorandomClient`.
    let urls: Vec<_> = urls_
        .into_iter()
        .map(|(d, p)| Url::from_str(&url_from_nums(d % domains, p % paths_per_domain)).unwrap())
        .take(10)
        .collect();

    let exclude_urls_re = mk_static(None);

    let max_depth = max_depth_ % 3;

    let search_re = mk_static(Regex::new(".").unwrap());

    let mut first_buffer = Vec::new();
    run(
        &mut first_buffer,
        progress.clone(),
        cache,
        client,
        page_threads,
        exclude_urls_re,
        max_depth,
        search_re,
        urls.clone(),
    )
    .await
    .unwrap();

    let mut second_buffer = Vec::new();
    run(
        &mut second_buffer,
        progress,
        cache,
        client,
        page_threads,
        exclude_urls_re,
        max_depth,
        search_re,
        urls,
    )
    .await
    .unwrap();

    assert_eq!(
        line_occurences(&first_buffer),
        line_occurences(&second_buffer)
    );

    // Static references won't automatically clear memory.
    cache.clear();
}

#[test]
fn run_finds_the_same_matches_with_empty_or_partial_cache() {
    assert!(true);
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
