use crate::watcher::{DirectoryWatcher, WatchedType};
use async_trait::async_trait;
use dashmap::DashMap;
use dotenvy::Error as DotenvError;
use rand::{RngCore, rng};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, RwLock};
use tracing::{error, info};

#[async_trait::async_trait]
pub trait SecretsManagerType: Send + Sync {
    async fn as_vec(&self) -> Vec<(String, String)> {
        let mut secrets = vec![];
        for key in self.keys() {
            if let Some(handle) = self.get(&key) {
                match self.reveal(handle).await {
                    Ok(Some(secret)) => secrets.push((key.to_string(), secret)),
                    Ok(None) => {} // no secret revealed
                    Err(_) => {}   // optionally log the error
                }
            }
        }
        secrets
    }
    fn get(&self, key: &str) -> Option<u32>;
    fn keys(&self) -> Vec<String>;
    async fn add_secret(&self, key: &str, secret: &str) -> Result<(), SecretsError>;
    async fn update_secret(&self, key: &str, secret: &str) -> Result<(), SecretsError>;
    async fn delete_secret(&self, key: &str) -> Result<(), SecretsError>;
    async fn reveal(&self, handle: u32) -> Result<Option<String>, SecretsError>;
    fn name(&self) -> &'static str;
    fn clone_box(&self) -> Arc<dyn SecretsManagerType>;
    fn debug_box(&self) -> String;
}
pub struct SecretsManager(pub Arc<dyn SecretsManagerType + Send + Sync>);

impl SecretsManager {
    pub fn into_inner(self) -> Arc<dyn SecretsManagerType> {
        self.0
    }

    pub async fn add_secret(&self, key: &str, value: &str) -> Result<(), SecretsError> {
        self.0.add_secret(key, value).await
    }

    pub async fn update_secret(&self, key: &str, value: &str) -> Result<(), SecretsError> {
        self.0.update_secret(key, value).await
    }

    pub async fn delete_secret(&self, key: &str) -> Result<(), SecretsError> {
        self.0.delete_secret(key).await
    }

    pub async fn get_secret(&self, key: &str) -> Result<Option<String>, SecretsError> {
        let handle = self.0.get(key);
        match handle {
            Some(handle) => self.0.reveal(handle).await,
            None => Ok(None),
        }
    }
}

impl Debug for EnvSecretsManager {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let keys: Vec<String> = self.keys();
        writeln!(f, "EnvSecretsManager {{ keys: {:?} }}", keys)
    }
}

impl Clone for SecretsManager {
    fn clone(&self) -> Self {
        SecretsManager(self.0.clone_box())
    }
}

impl std::fmt::Debug for SecretsManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.debug_box())
    }
}

impl Serialize for SecretsManager {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // You could serialize name + keys as a basic fallback
        let mut state = serializer.serialize_struct("SecretsManager", 2)?;
        state.serialize_field("name", self.0.name())?;
        state.serialize_field("keys", &self.0.keys())?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for SecretsManager {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Err(serde::de::Error::custom(
            "SecretsManager cannot be deserialized dynamically",
        ))
    }
}

#[derive(Debug, Clone)]
pub enum SecretsError {
    Upstream(String),
    Io(String),
    NotFound,
}

#[derive(Clone)]
struct DotenvHandler {
    path: PathBuf,
    mgr: Arc<EnvSecretsManager>,
}

#[async_trait]
impl WatchedType for DotenvHandler {
    fn is_relevant(&self, p: &Path) -> bool {
        // only our exact .env file
        p.ends_with(".env")
    }

    async fn on_create_or_modify(&self, _p: &Path) -> anyhow::Result<()> {
        // every time it changes, reload
        self.mgr.load_dotenv(&self.path);
        info!(".env reloaded at {}", self.path.display());
        Ok(())
    }

    async fn on_remove(&self, _p: &Path) -> anyhow::Result<()> {
        // if someone nukes the file out from under us, maybe clear all secrets?
        // or just log and ignore
        info!(".env {} removed; secrets left as-is", self.path.display());
        Ok(())
    }
}

#[derive(Clone)]
pub struct EnvSecretsManager {
    keys: Arc<RwLock<HashMap<String, u32>>>,
    secrets: Arc<RwLock<HashMap<u32, String>>>,
    env_path: Option<PathBuf>,
}

impl EnvSecretsManager {
    pub fn new(dotenv_path: Option<PathBuf>) -> Arc<Self> {
        let env_path = match dotenv_path.clone() {
            Some(path) => Some(path.join(".env")),
            None => None,
        };
        let mgr = Arc::new(Self {
            keys: Arc::new(RwLock::new(HashMap::new())),
            secrets: Arc::new(RwLock::new(HashMap::new())),
            env_path,
        });

        if let Some(path) = dotenv_path {
            let envfile = path.join(".env");
            if path.exists() && envfile.exists() {
                // immediate synchronous load—this is very quick, just one small file
                mgr.load_dotenv(&envfile);

                // now spawn the tokio watcher

                let handler = DotenvHandler {
                    path: envfile.clone(),
                    mgr: Arc::clone(&mgr),
                };
                tokio::spawn(async move {
                    if let Err(e) =
                        DirectoryWatcher::new(path, Arc::new(handler), &[], true, false).await
                    {
                        tracing::error!("dotenv watch_dir failed: {}", e);
                    }
                });
            } else {
                error!(
                    ".env file {} not found, skipping load & watch",
                    envfile.display()
                );
            }
        } else {
            error!(".env watcher disabled (no path provided)");
        }

        mgr
    }

    /// Synchronous helper for programmatic secrets insertion.
    pub fn add_secret_sync(&self, key: &str, secret: &str) {
        {
            let mut keys = self.keys.write().unwrap();
            let mut secrets = self.secrets.write().unwrap();
            let id = rng().next_u32();
            keys.insert(key.to_string(), id);
            secrets.insert(id, secret.to_string());
        }
        self.write_dotenv();
    }
    /// Synchronous helper for programmatic secrets updates.
    pub fn update_secret_sync(&self, key: &str, secret: &str) {
        {
            let mut keys = self.keys.write().unwrap();
            let mut secrets = self.secrets.write().unwrap();
            let handle = match keys.get(key) {
                Some(handle) => *handle,
                None => {
                    let id = rng().next_u32();
                    keys.insert(key.to_string(), id);
                    id
                }
            };
            secrets.insert(handle, secret.to_string());
        }
        self.write_dotenv();
    }
    /// Synchronous helper for programmatic secrets removal.
    pub fn delete_secret_sync(&self, key: &str) {
        {
            let handle_opt = {
                let keys = self.keys.read().unwrap();
                keys.get(key).cloned()
            };
            if let Some(handle) = handle_opt {
                let mut keys = self.keys.write().unwrap();
                let mut secrets = self.secrets.write().unwrap();
                keys.remove(key);
                secrets.remove(&handle);
            }
        }
        self.write_dotenv();
    }
    /// Parse _only_ the given `.env` file, overwrite whatever was there.
    fn load_dotenv(&self, path: &Path) {
        match dotenvy::from_path_iter(path) {
            Ok(iter) => {
                // 1) clear both maps under one write‐lock scope
                {
                    let mut keys = self.keys.write().unwrap();
                    let mut secrets = self.secrets.write().unwrap();
                    keys.clear();
                    secrets.clear();
                }
                // 2) now we're outside of any lock; add_secret_sync will lock/unlock per‐item
                for item in iter {
                    match item {
                        Ok((k, v)) => {
                            self.add_secret_sync(&k, &v);
                        }
                        Err(e) => {
                            error!("Malformed line in {}: {}", path.display(), e);
                        }
                    }
                }
                info!(".env reloaded from {}", path.display());
            }
            Err(DotenvError::Io(io)) if io.kind() == std::io::ErrorKind::NotFound => {
                // no .env → quietly skip
                info!(".env file {} not found, skipping", path.display());
            }
            Err(e) => {
                error!("Failed to read {}: {}", path.display(), e);
            }
        }
    }

    fn write_dotenv(&self) {
        let Some(path) = &self.env_path else { return };

        let keys = self.keys.read().unwrap();
        let secrets = self.secrets.read().unwrap();

        let mut out = String::new();
        for (key, handle) in keys.iter() {
            if let Some(value) = secrets.get(handle) {
                let escaped_value = value.replace('\n', "\\n");
                out.push_str(&format!("{}={}\n", key, escaped_value));
            }
        }

        if let Err(e) = std::fs::write(path, out) {
            error!("Failed to write to .env file {}: {}", path.display(), e);
        } else {
            info!(".env file updated at {}", path.display());
        }
    }
}

#[async_trait::async_trait]
impl SecretsManagerType for EnvSecretsManager {
    fn get(&self, key: &str) -> Option<u32> {
        self.keys.read().unwrap().get(key).copied()
    }
    fn keys(&self) -> Vec<String> {
        let keys_read = self.keys.read().unwrap();
        keys_read.keys().cloned().collect()
    }
    async fn add_secret(&self, key: &str, secret: &str) -> Result<(), SecretsError> {
        self.add_secret_sync(key, secret);
        Ok(())
    }

    async fn update_secret(&self, key: &str, secret: &str) -> Result<(), SecretsError> {
        self.update_secret_sync(key, secret);
        Ok(())
    }

    async fn delete_secret(&self, key: &str) -> Result<(), SecretsError> {
        self.delete_secret_sync(key);
        Ok(())
    }

    async fn reveal(&self, handle: u32) -> Result<Option<String>, SecretsError> {
        Ok(self.secrets.read().unwrap().get(&handle).cloned())
    }

    fn name(&self) -> &'static str {
        "EnvSecrets (watched)"
    }

    fn clone_box(&self) -> Arc<dyn SecretsManagerType> {
        Arc::new(self.clone())
    }

    fn debug_box(&self) -> String {
        let keys = self.keys();
        format!("SecretsManager {{ keys: {:?} }}", keys)
    }
}

pub struct TestSecretsManager {
    // key -> handle
    handles: DashMap<String, u32>,
    // key -> secret
    secrets: DashMap<String, String>,
    next_handle: AtomicU32,
}

impl TestSecretsManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            handles: DashMap::new(),
            secrets: DashMap::new(),
            next_handle: AtomicU32::new(1),
        })
    }
}

impl Clone for TestSecretsManager {
    fn clone(&self) -> Self {
        let cloned_handles = DashMap::new();
        for kv in self.handles.iter() {
            cloned_handles.insert(kv.key().clone(), *kv.value());
        }

        let cloned_secrets = DashMap::new();
        for kv in self.secrets.iter() {
            cloned_secrets.insert(kv.key().clone(), kv.value().clone());
        }

        Self {
            handles: cloned_handles,
            secrets: cloned_secrets,
            next_handle: AtomicU32::new(self.next_handle.load(Ordering::Relaxed)),
        }
    }
}

#[async_trait::async_trait]
impl SecretsManagerType for TestSecretsManager {
    fn get(&self, key: &str) -> Option<u32> {
        self.handles.get(key).map(|v| *v)
    }

    fn keys(&self) -> Vec<String> {
        self.handles.iter().map(|kv| kv.key().clone()).collect()
    }

    async fn add_secret(&self, key: &str, secret: &str) -> Result<(), SecretsError> {
        // If key is new, mint a handle; otherwise keep existing handle
        self.handles
            .entry(key.to_string())
            .or_insert_with(|| self.next_handle.fetch_add(1, Ordering::Relaxed));
        self.secrets.insert(key.to_string(), secret.to_string());
        Ok(())
    }

    async fn update_secret(&self, key: &str, secret: &str) -> Result<(), SecretsError> {
        if !self.handles.contains_key(key) {
            return Err(SecretsError::NotFound);
        }
        self.secrets.insert(key.to_string(), secret.to_string());
        Ok(())
    }

    async fn delete_secret(&self, key: &str) -> Result<(), SecretsError> {
        let existed_h = self.handles.remove(key).is_some();
        let existed_s = self.secrets.remove(key).is_some();
        if existed_h || existed_s {
            Ok(())
        } else {
            Err(SecretsError::NotFound)
        }
    }

    async fn reveal(&self, handle: u32) -> Result<Option<String>, SecretsError> {
        // Find key by handle (linear scan is fine for tests)
        let key_opt = self
            .handles
            .iter()
            .find(|kv| *kv.value() == handle)
            .map(|kv| kv.key().clone());

        if let Some(key) = key_opt {
            Ok(self.secrets.get(&key).map(|v| v.clone()))
        } else {
            Ok(None)
        }
    }

    fn name(&self) -> &'static str {
        "TestSecretsManager"
    }

    fn clone_box(&self) -> Arc<dyn SecretsManagerType> {
        Arc::new(self.clone())
    }

    fn debug_box(&self) -> String {
        format!(
            "TestSecretsManager {{ handles: {}, secrets: {} }}",
            self.handles.len(),
            self.secrets.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_env_manager() -> Arc<EnvSecretsManager> {
        EnvSecretsManager::new(None) // Don't load or watch any file
    }

    #[test]
    fn test_add_secret_sync() {
        let mgr = setup_env_manager();
        mgr.add_secret_sync("API_KEY", "123456");
        let keys = mgr.keys();
        assert_eq!(keys, vec!["API_KEY".to_string()]);
        let handle = mgr.get("API_KEY").unwrap();
        let read = mgr.secrets.read().unwrap();
        let value = read.get(&handle);
        assert_eq!(value, Some(&"123456".to_string()));
    }

    #[test]
    fn test_update_secret_sync_existing_key() {
        let mgr = setup_env_manager();
        mgr.add_secret_sync("TOKEN", "abc");
        mgr.update_secret_sync("TOKEN", "def");

        let handle = mgr.get("TOKEN").unwrap();
        let read = mgr.secrets.read().unwrap();
        let secret = read.get(&handle);
        assert_eq!(secret, Some(&"def".to_string()));
    }

    #[test]
    fn test_update_secret_sync_new_key() {
        let mgr = setup_env_manager();
        mgr.update_secret_sync("NEW_TOKEN", "newvalue");

        let handle = mgr.get("NEW_TOKEN").unwrap();
        let read = mgr.secrets.read().unwrap();
        let secret = read.get(&handle);
        assert_eq!(secret, Some(&"newvalue".to_string()));
    }

    #[test]
    fn test_delete_secret_sync() {
        let mgr = setup_env_manager();
        mgr.add_secret_sync("SECRET", "xyz");

        let handle = mgr.get("SECRET").unwrap();
        mgr.delete_secret_sync("SECRET");

        assert!(mgr.get("SECRET").is_none());
        assert!(mgr.secrets.read().unwrap().get(&handle).is_none());
    }

    #[tokio::test]
    async fn test_reveal_async() {
        let mgr = setup_env_manager();
        mgr.add_secret_sync("PASSWORD", "hunter2");

        let handle = mgr.get("PASSWORD").unwrap();
        let revealed = mgr.reveal(handle).await.unwrap();
        assert_eq!(revealed, Some("hunter2".to_string()));
    }

    #[tokio::test]
    async fn test_async_traits_end_to_end() {
        let mgr = setup_env_manager();

        mgr.add_secret("foo", "bar").await.unwrap();
        let keys = mgr.keys();
        assert_eq!(keys, vec!["foo".to_string()]);

        let handle = mgr.get("foo").unwrap();
        let revealed = mgr.reveal(handle).await.unwrap();
        assert_eq!(revealed, Some("bar".to_string()));

        mgr.update_secret("foo", "baz").await.unwrap();
        let updated = mgr.reveal(handle).await.unwrap();
        assert_eq!(updated, Some("baz".to_string()));

        mgr.delete_secret("foo").await.unwrap();
        assert!(mgr.get("foo").is_none());
    }

    #[tokio::test]
    async fn test_dotenv_write_on_add_update_delete() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let mgr = EnvSecretsManager::new(Some(dir.path().to_path_buf()));

        mgr.add_secret("X", "1").await.unwrap();
        let env_path = dir.path().join(".env");
        let content = fs::read_to_string(&env_path).unwrap();
        assert!(content.contains("X=1"));

        mgr.update_secret("X", "2").await.unwrap();
        let content = fs::read_to_string(&env_path).unwrap();
        assert!(content.contains("X=2"));
        assert!(!content.contains("X=1"));

        mgr.delete_secret("X").await.unwrap();
        let content = fs::read_to_string(&env_path).unwrap();
        assert!(!content.contains("X="));
    }
}
