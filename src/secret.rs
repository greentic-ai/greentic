use std::fmt::Formatter;
use std::sync::{Arc, RwLock};
use std::collections::HashMap;
use async_trait::async_trait;
use rand::{rng, RngCore};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use std::path::{Path, PathBuf};
use std::fmt::Debug;
use dotenvy::Error as DotenvError;

use crate::watcher::{DirectoryWatcher, WatchedType};

#[async_trait::async_trait]
pub trait SecretsManagerType: Send + Sync {
    async fn as_vec(&self) -> Vec<(String, String)> {
        let mut secrets = vec![];
        for key in self.keys().await {
            if let Some(handle) = self.get(&key) {
                if let Some(secret) = self.reveal(handle).await {
                    secrets.push((key, secret));
                }
            }
        }
        secrets
    }
    fn get(&self, key: &str) -> Option<u32>;
    fn keys(&self) -> Vec<String>;
    async fn add_secret(&mut self, key: &str, secret: &str) -> Result<(),SecretsError>;
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
    where S: serde::Serializer {
        // You could serialize name + keys as a basic fallback
        let mut state = serializer.serialize_struct("SecretsManager", 2)?;
        state.serialize_field("name", self.0.name())?;
        state.serialize_field("keys", &self.0.keys())?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for SecretsManager {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where D: serde::Deserializer<'de> {
        Err(serde::de::Error::custom("SecretsManager cannot be deserialized dynamically"))
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
}

impl EnvSecretsManager {
    pub fn new(dotenv_path: Option<PathBuf>) -> Arc<Self> {
        let mgr = Arc::new(Self {
            keys: Arc::new(RwLock::new(HashMap::new())),
            secrets: Arc::new(RwLock::new(HashMap::new())),
        });

        if let Some(path) = dotenv_path {
            let envfile = path.join(".env");
            if path.exists() && envfile.exists() {
                // immediate synchronous load—this is very quick, just one small file
                mgr.load_dotenv(&envfile);

                // now spawn the tokio watcher

                let handler = DotenvHandler { path: envfile.clone(), mgr: Arc::clone(&mgr) };
                tokio::spawn(async move {
                    if let Err(e) = DirectoryWatcher::new(path, Arc::new(handler), &[], false).await {
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
        let mut keys   = self.keys.write().unwrap();
        let mut secrets= self.secrets.write().unwrap();
        let id = rng().next_u32();
        keys.insert(key.to_string(), id);
        secrets.insert(id, secret.to_string());
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
}

#[async_trait::async_trait]
impl SecretsManagerType for EnvSecretsManager {
    fn get(&self, key: &str) -> Option<u32>{
        self.keys.read().unwrap().get(key).copied()
    }
    fn keys(&self) -> Vec<String> {
        let keys_read = self.keys.read().unwrap();
        keys_read.keys().cloned().collect()
    }
    async fn add_secret(&mut self, key: &str, secret: &str) -> Result<(),SecretsError>{
        self.add_secret_sync(key, secret);
        Ok(())
    }

    async fn reveal(&self, handle: u32) -> Result<Option<String>, SecretsError>{
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


/// Used to reset the secrets member
#[derive(Clone)]
pub struct EmptySecretsManager;

impl EmptySecretsManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

#[async_trait::async_trait]
impl SecretsManagerType for EmptySecretsManager{
    fn get(&self,_key: &str) -> Option<u32>  {
        None
    }

    fn keys(&self) -> Vec<String>  {
        vec![]
    }
    async fn add_secret(&mut self, _key: &str, _secret: &str) -> Result<(),SecretsError>{
       Err(SecretsError::NotFound)
    }

   async fn reveal(&self, _handle: u32) -> Result<Option<String>, SecretsError> {
        Err(SecretsError::NotFound)
    }

    fn name(&self) ->  &'static str {
        ""
    }

    fn clone_box(&self) -> Arc<dyn SecretsManagerType>  {
       Arc::new(self.clone())
    }

    fn debug_box(&self) -> String {
        "".to_string()
    }
}