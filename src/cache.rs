use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use tokio::task;

pub trait Cache<K, V> {
    fn get(&self, k: &K) -> Option<V>;
    fn set(&self, k: &K, v: &V) -> Result<(), Box<dyn std::error::Error>>;
}

pub struct FileCache<K, V> {
    dir: PathBuf,
    key: PhantomData<K>,
    value: PhantomData<V>,
}

impl<K: Hash, V> FileCache<K, V> {
    pub async fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let dir = std::env::var("XDG_CACHE_HOME")
            .map_or(
                Path::new(std::env::var("HOME")?.as_str()).join(".cache"),
                PathBuf::from,
            )
            .join("web-grep")
            .join(name);
        tokio::fs::create_dir_all(&dir).await?;
        Ok(Self {
            dir,
            key: PhantomData,
            value: PhantomData,
        })
    }

    fn key_path(&self, k: &K) -> PathBuf {
        self.dir.join(self.hash(k).to_string().as_str())
    }

    fn hash(&self, k: &K) -> u64 {
        let mut h = DefaultHasher::new();
        k.hash(&mut h);
        h.finish()
    }
}

impl<K: Hash, V: serde::ser::Serialize + serde::de::DeserializeOwned> Cache<K, V>
    for FileCache<K, V>
{
    fn get(&self, k: &K) -> Option<V> {
        // `bincode::deserialize_from` may panic
        // if file contents don't match expected format.
        task::block_in_place(|| std::fs::read(self.key_path(k)))
            .ok()
            .and_then(|x| bincode::deserialize(&x).ok())
    }

    fn set(&self, k: &K, v: &V) -> Result<(), Box<dyn std::error::Error>> {
        let path = self.key_path(k);
        task::block_in_place(|| {
            bincode::serialize_into(std::io::BufWriter::new(std::fs::File::create(path)?), v)
        })
        .map_err(|e| e.into())
    }
}
