use std::collections::hash_map::HashMap;
use std::hash::Hash;
use std::sync::RwLock;
use webgrep::cache::Cache;

pub struct MemCache<K, V> {
    inner: RwLock<HashMap<K, V>>,
}

impl<K, V> MemCache<K, V> {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
        }
    }

    pub fn clear(&self) {
        self.inner.write().unwrap().clear();
    }
}

impl<K: Clone + Eq + Hash, V: Clone> Cache<K, V> for MemCache<K, V> {
    fn get(&self, k: &K) -> Option<V> {
        self.inner.read().unwrap().get(k).map(|x| x.clone())
    }

    fn set(&self, k: &K, v: &V) -> Result<(), Box<dyn std::error::Error>> {
        self.inner.write().unwrap().insert(k.clone(), v.clone());
        Ok(())
    }
}
