use std::marker::PhantomData;
use std::path::{Path, PathBuf};

pub struct Cache<K, V> {
    tree: sled::Tree,
    key: PhantomData<K>,
    value: PhantomData<V>,
}

// `sled` `.get` may have to get from disk.
// `sled` `.set` may have to write to disk.
// However,
// benchmarks show better performance
// without `task::spawn_blocking`
// or `task::block_in_place`.

impl<K: std::convert::AsRef<str>, V: serde::ser::Serialize + serde::de::DeserializeOwned>
    Cache<K, V>
{
    pub async fn new(tree_name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let dir = std::env::var("XDG_CACHE_HOME").map_or(
            Path::new(std::env::var("HOME")?.as_str()).join(".cache"),
            PathBuf::from,
        );
        tokio::fs::create_dir_all(&dir).await?;
        Ok(Self {
            tree: sled::open(dir.join("web-grep"))?.open_tree(tree_name)?,
            key: PhantomData,
            value: PhantomData,
        })
    }

    pub fn get(&self, k: &K) -> Option<V> {
        self.tree
            .get(k.as_ref())
            .ok()
            .flatten()
            .and_then(|x| bincode::deserialize(&x).ok())
    }

    pub fn set(&self, k: &K, v: &V) -> Result<(), Box<dyn std::error::Error>> {
        self.tree
            .insert(k.as_ref(), bincode::serialize(v)?)
            .map(|_| ())
            .map_err(|e| e.into())
    }

    pub async fn flush(&self) -> sled::Result<()> {
        self.tree.flush_async().await.map(|_| ())
    }
}
