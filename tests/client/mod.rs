use rand::distributions::{Distribution, Uniform};
use rand::rngs::SmallRng;
use rand::SeedableRng;
use reqwest::Url;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::num::NonZeroU16;
use webgrep::client::{Body, Client, Response};

pub struct PseudorandomClient {
    links_range: Uniform<u8>,
    domains_range: Uniform<u16>,
    paths_per_domain_range: Uniform<u16>,
}

impl PseudorandomClient {
    pub fn new(
        min_links: u8,
        max_links: u8,
        domains: NonZeroU16,
        paths_per_domain: NonZeroU16,
    ) -> Self {
        Self {
            links_range: Uniform::new(min_links, max_links + 1),
            domains_range: Uniform::new(0, domains.get()),
            paths_per_domain_range: Uniform::new(0, paths_per_domain.get()),
        }
    }
}

#[async_trait::async_trait]
impl Client for PseudorandomClient {
    async fn get(&self, url: &Url) -> Response {
        let mut rng = SmallRng::seed_from_u64(hash(url));
        let links = (0..self.links_range.sample(&mut rng))
            .map(|i| {
                format!(
                    r#"<a href="{}">{}</a>"#,
                    url_from_nums(
                        self.domains_range.sample(&mut rng),
                        self.paths_per_domain_range.sample(&mut rng)
                    ),
                    i
                )
            })
            .collect::<Vec<_>>()
            .join("");
        Ok(Body::Html(links))
    }
}

fn hash(url: &Url) -> u64 {
    let mut h = DefaultHasher::new();
    url.hash(&mut h);
    h.finish()
}

pub fn url_from_nums(domain: u16, path: u16) -> String {
    format!("http://{}.com/{}", domain, path)
}
