use reqwest::Url;
use serde::{Deserialize, Serialize};

const BODY_SIZE_LIMIT: u64 = 104857600; // bytes

pub type Response = Result<Body, Error>;

#[derive(Serialize, Deserialize)]
pub enum Body {
    Html(String),
    Pdf(String),
    Plain(String),
}

#[derive(Serialize, Deserialize)]
pub enum Error {
    InvalidContentType(String),
    ContentLengthTooLong(Option<u64>),
    Other(ReqwestError),
}

#[derive(Serialize, Deserialize)]
pub enum ReqwestError {
    Builder,
    Redirect,
    Status(u16),
    Timeout,
    Request,
    Connect,
    Body,
    Decode,
    Other(String),
}

impl From<reqwest::Error> for ReqwestError {
    fn from(e: reqwest::Error) -> ReqwestError {
        if e.is_builder() {
            ReqwestError::Builder
        } else if e.is_redirect() {
            ReqwestError::Redirect
        } else if e.is_status() {
            ReqwestError::Status(e.status().unwrap().as_u16())
        } else if e.is_timeout() {
            ReqwestError::Timeout
        } else if e.is_request() {
            ReqwestError::Request
        } else if e.is_connect() {
            ReqwestError::Connect
        } else if e.is_body() {
            ReqwestError::Body
        } else if e.is_decode() {
            ReqwestError::Decode
        } else {
            ReqwestError::Other(e.to_string())
        }
    }
}

#[async_trait::async_trait]
pub trait Client {
    async fn get(&self, url: &Url) -> Response;
}

#[async_trait::async_trait]
impl Client for reqwest::Client {
    async fn get(&self, url: &Url) -> Response {
        match self.get(url.as_ref()).send().await {
            Ok(r) => {
                // The default `content-type` is `application/octet-stream`,
                // <https://www.w3.org/Protocols/rfc2616/rfc2616-sec7.html#sec7.2.1>.
                let content_type = r
                    .headers()
                    .get("content-type")
                    .map_or("application/octet-stream", |x| x.to_str().unwrap_or(""));
                if content_type.contains("text/html") {
                    read_body(r).await.map(Body::Html)
                } else if content_type.contains("application/pdf") {
                    read_body(r).await.map(Body::Pdf)
                } else if content_type.contains("text/plain") {
                    read_body(r).await.map(Body::Plain)
                } else {
                    Err(Error::InvalidContentType(content_type.to_owned()))
                }
            }
            Err(e) => Err(Error::Other(e.into())),
        }
    }
}

async fn read_body(r: reqwest::Response) -> Result<String, Error> {
    if r.content_length().map_or(true, |x| x < BODY_SIZE_LIMIT) {
        // TODO: incrementally read with `chunk`,
        // short circuit if bytes gets too long,
        // and decode with source from `text_with_charset`.
        r.text().await.map_err(|e| Error::Other(e.into()))
    } else {
        Err(Error::ContentLengthTooLong(r.content_length()))
    }
}
