use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{error, info};
use std::{collections::HashMap, env, path::PathBuf};

#[async_trait::async_trait]
#[typetag::serde] 
pub trait ConfigManagerType: Send + Sync  {
    async fn as_vec(&self) -> Vec<(String, String)> {
        let mut config = vec![];
        for key in self.keys().await {
            if let Some(value) = self.get(&key).await {
                config.push((key, value));
            }
        }
        config
    }
    async fn keys(&self) -> Vec<String>;
    async fn get(&self, key: &str) -> Option<String>;
    async fn set(&mut self, key: &str, value: &str) -> Result<(), String>;
    fn clone_box(&self) -> Box<dyn ConfigManagerType>;
    fn debug_box(&self) -> String;
}

#[derive(Serialize, Deserialize )]
pub struct ConfigManager(pub Box<dyn ConfigManagerType>);

impl ConfigManager {
    pub fn into_inner(self) -> Box<dyn ConfigManagerType> {
        self.0
    }
}

impl Clone for ConfigManager {
    fn clone(&self) -> Self {
        ConfigManager(self.0.clone_box())
    }
}

impl std::fmt::Debug for ConfigManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.debug_box())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnvConfigManager;

impl EnvConfigManager {
    pub fn new(env_file: PathBuf) -> Box<Self> {
        if env_file.exists() {
            dotenvy::from_path(env_file.clone()).ok();
            info!("Loaded .env from {}", env_file.display());
        } else {
            error!("could not load .env from {}",env_file.display())
        }

        Box::new(Self)
    }
}

#[typetag::serde]
#[async_trait]
impl ConfigManagerType for EnvConfigManager {

    async fn keys(&self) -> Vec<String> {
        env::vars().map(|(k, _)| k).collect()
    }
    async fn get(&self, key: &str) -> Option<String> {
        env::var(key).ok()
    }

    async fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        unsafe { env::set_var(key, value); };
        Ok(())
    }

    fn clone_box(&self) -> Box<dyn ConfigManagerType> {
        Box::new(self.clone())
    }

    fn debug_box(&self) -> String {
        "EnvConfigManager".to_string()
    }
}



#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct MapConfigManager {
    // simple HashMap → serde will derive Serialize + Deserialize for us
    map: HashMap<String, String>,
}

impl MapConfigManager {
    pub fn new() -> Box<Self> {
        Box::new(Self{map:HashMap::new()})
    }
}

#[typetag::serde]
#[async_trait]
impl ConfigManagerType for MapConfigManager {
    async fn keys(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    async fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }

    async fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        self.map.insert(key.to_string(), value.to_string());
        Ok(())
    }

    fn clone_box(&self) -> Box<dyn ConfigManagerType> {
        Box::new(self.clone())
    }
    fn debug_box(&self) -> String {
        format!("MapConfigManager({} entries)", self.map.len())
    }
}