use reqwest::Url;
use std::path::{Path, PathBuf};

pub type SerializableResponse = Result<String, String>;

pub struct Cache {
    tree: sled::Tree,
}

// `sled` `.get` may have to get from disk.
// `sled` `.set` may have to write to disk.
// However,
// benchmarks show better performance
// without `task::spawn_blocking`
// or `task::block_in_place`.

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

    pub fn get(&self, u: &Url) -> Option<SerializableResponse> {
        self.tree
            .get(u.as_str())
            .ok()
            .flatten()
            .and_then(|x| bincode::deserialize(&x).ok())
    }

    pub fn set(
        &self,
        u: &Url,
        body: &SerializableResponse,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let val = bincode::serialize(body)?;
        self.tree
            .insert(u.as_str(), val)
            .map(|_| ())
            .map_err(|e| e.into())
    }

    pub async fn flush(&self) -> sled::Result<()> {
        self.tree.flush_async().await.map(|_| ())
    }
}
