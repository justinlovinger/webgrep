use reqwest::Url;
use std::path::{Path, PathBuf};
use tokio::task;

pub type SerializableResponse = Result<String, String>;

pub struct Cache {
    tree: sled::Tree,
}

impl Cache {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = std::env::var("XDG_CACHE_HOME").map_or(
            Path::new(std::env::var("HOME")?.as_str()).join(".cache"),
            PathBuf::from,
        );
        tokio::fs::create_dir_all(&dir).await?;
        Ok(Self {
            tree: sled::open(dir.join("web-grep"))?.open_tree("page-cache")?,
        })
    }

    pub async fn get(&self, u: &Url) -> Option<SerializableResponse> {
        task::block_in_place(|| self.tree.get(u.as_str()))
            .ok()
            .flatten()
            .and_then(|x| bincode::deserialize(&x).ok())
    }

    pub async fn set(
        &self,
        u: &Url,
        body: &SerializableResponse,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let val = bincode::serialize(body)?;
        task::block_in_place(|| self.tree.insert(u.as_str(), val))
            .map(|_| ())
            .map_err(|e| e.into())
    }

    pub async fn flush(&self) -> sled::Result<()> {
        self.tree.flush_async().await.map(|_| ())
    }
}
