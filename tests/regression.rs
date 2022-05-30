mod cache;
mod common;

use crate::cache::MemCache;
use crate::common::{line_occurences, mk_static};
use lazy_static::__Deref;
use regex::Regex;
use reqwest::Url;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::str::FromStr;
use std::time::Duration;
use webgrep::client::{self, Body, Client, Response};
use webgrep::run;

#[tokio::test(flavor = "multi_thread")]
async fn run_finds_all_matches_with_empty_cache() {
    let cache = mk_static(MemCache::new());
    assert_eq!(&line_occurences(&run_(cache, 2).await), EXPECTED.deref());
    cache.clear();
}

#[tokio::test(flavor = "multi_thread")]
async fn run_finds_all_matches_with_full_cache() {
    let cache = mk_static(MemCache::new());
    run_(cache, 2).await;
    assert_eq!(&line_occurences(&run_(cache, 2).await), EXPECTED.deref());
    cache.clear();
}

#[tokio::test(flavor = "multi_thread")]
async fn run_finds_all_matches_with_partial_cache() {
    let cache = mk_static(MemCache::new());
    run_(cache, 1).await;
    assert_eq!(&line_occurences(&run_(cache, 2).await), EXPECTED.deref());
    cache.clear();
}

async fn run_(cache: &'static MemCache<Url, Response>, max_depth: usize) -> Vec<u8> {
    let mut buffer = Vec::new();
    run(
        &mut buffer,
        indicatif::MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::hidden()),
        cache,
        TEST_CLIENT.deref(),
        Duration::ZERO,
        NonZeroUsize::new(max_depth).unwrap(),
        mk_static(None),
        2,
        mk_static(Regex::new(".").unwrap()),
        vec![Url::from_str("http://foo.com").unwrap()],
    )
    .await
    .unwrap();
    buffer
}

lazy_static::lazy_static! {
    static ref EXPECTED: HashMap<&'static str, u32> = HashMap::from([
        ("http://foo.com/", 1),
        ("http://foo.com/ > http://bar.com/", 1),
        ("http://foo.com/ > http://foobar.com/", 1),
        ("http://foo.com/ > http://bar.com/ > http://foobar.com/", 1),
    ]);

    static ref TEST_CLIENT: MapClient = MapClient::new(HashMap::from([
        (
            Url::from_str("http://foo.com/").unwrap(),
            Body::Html(
                r#"foo<a href="http://bar.com/">1</a><a href="http://foobar.com/">2</a>"#.to_owned(),
            ),
        ),
        (
            Url::from_str("http://bar.com/").unwrap(),
            Body::Html(
                r#"bar<a href="http://foo.com/">1</a><a href="http://foobar.com/">2</a>"#.to_owned(),
            ),
        ),
        (
            Url::from_str("http://foobar.com/").unwrap(),
            Body::Html(r#"foobar"#.to_owned()),
        ),
    ]));
}

pub struct MapClient {
    map: HashMap<Url, Body>,
}

impl MapClient {
    pub const fn new(map: HashMap<Url, Body>) -> Self {
        Self { map }
    }
}

#[async_trait::async_trait]
impl Client for MapClient {
    async fn get(&self, url: &Url) -> Response {
        self.map
            .get(url)
            .map(|x| x.clone())
            .ok_or(client::Error::Other(client::ReqwestError::Status(404)))
    }
}
