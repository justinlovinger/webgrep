use reqwest::Url;
use std::time::{Duration, Instant};

const BODY_SIZE_LIMIT: u64 = 104857600; // bytes

pub type SerializableResponse = Result<String, String>;

pub struct SlowClient<'a> {
    client: &'a reqwest::Client,
    last_request_finished: Option<Instant>,
}

impl<'a> SlowClient<'a> {
    pub fn new(client: &'a reqwest::Client) -> Self {
        Self {
            client,
            last_request_finished: None,
        }
    }

    pub async fn get(&mut self, u: &Url) -> SerializableResponse {
        // Making web requests
        // at the speed of a computer
        // can have negative repercussions,
        // like IP banning.
        let time_remaining = self.time_remaining();
        if time_remaining > Duration::ZERO {
            tokio::time::sleep(time_remaining).await;
        }
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
        self.last_request_finished = Some(Instant::now());
        body
    }

    pub fn time_remaining(&self) -> Duration {
        self.last_request_finished
            .and_then(|x| Duration::from_secs(1).checked_sub(x.elapsed()))
            .unwrap_or(Duration::ZERO)
    }
}
