use reqwest::Url;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use tokio::task;

type SerializableResponse = Result<String, String>;

pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    pub async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = std::env::var("XDG_CACHE_HOME")
            .map_or(
                Path::new(std::env::var("HOME")?.as_str()).join(".cache"),
                PathBuf::from,
            )
            .join("web-grep");
        tokio::fs::create_dir_all(&dir).await?;
        Ok(Self { dir })
    }

    pub async fn get(&self, u: &Url) -> Option<SerializableResponse> {
        // `bincode::deserialize_from` may panic
        // if file contents don't match expected format.
        tokio::fs::read(self.cache_path(u))
            .await
            .ok()
            .and_then(|x| bincode::deserialize(&x).ok())
    }

    pub async fn set(
        &self,
        u: &Url,
        body: &SerializableResponse,
    ) -> Result<(), Box<dyn std::error::Error>> {
        task::block_in_place(|| {
            bincode::serialize_into(
                std::io::BufWriter::new(std::fs::File::create(self.cache_path(u))?),
                body,
            )
            .map_err(|e| e.into())
        })
    }

    fn cache_path(&self, u: &Url) -> PathBuf {
        let mut filename = u.host_str().unwrap_or("nohost").to_owned();
        filename.push('-');
        filename.push_str(self.url_hash(u).to_string().as_str());
        self.dir.join(filename)
    }

    fn url_hash(&self, u: &Url) -> u64 {
        let mut s = DefaultHasher::new();
        u.hash(&mut s);
        s.finish()
    }
}
