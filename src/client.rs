use crate::cache::{SerializableResponse, Cache};
use reqwest::Url;
use tokio::time;

const BODY_SIZE_LIMIT: u64 = 104857600; // bytes

pub struct CachingClient<'a> {
    client: SlowClient<'a>,
    cache: &'a Cache,
}

impl<'a> CachingClient<'a> {
    pub fn new(client: SlowClient<'a>, cache: &'a Cache) -> Self {
        Self { client, cache }
    }

    pub async fn get(&self, u: &Url) -> Option<String> {
        match self.cache.get(u).await {
            Some(x) => x,
            None => self.get_and_cache_from_web(u).await,
        }
        .ok()
    }

    async fn get_and_cache_from_web(&self, u: &Url) -> SerializableResponse {
        let body = self.client.get(u).await;

        // We would rather keep searching
        // than panic
        // or delay
        // from failed caching.
        let _ = self.cache.set(u, &body);

        body
    }
}

pub struct SlowClient<'a> {
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
