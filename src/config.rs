use async_trait::async_trait;
use dashmap::DashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{env, fs, path::PathBuf};
use tracing::{error, info};

#[async_trait::async_trait]
#[typetag::serde]
pub trait ConfigManagerType: Send + Sync {
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
    async fn del(&self, key: &str);
    async fn set(&self, key: &str, value: &str) -> Result<(), String>;
    fn clone_box(&self) -> Box<dyn ConfigManagerType>;
    fn debug_box(&self) -> String;
}

#[derive(Serialize, Deserialize)]
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
pub struct EnvConfigManager {
    env_file: PathBuf,
}

impl EnvConfigManager {
    pub fn new(env_file: PathBuf) -> Box<Self> {
        if env_file.exists() {
            dotenvy::from_path(env_file.clone()).ok();
            info!("Loaded .env from {}", env_file.display());
        } else {
            error!("could not load .env from {}", env_file.display())
        }

        Box::new(Self { env_file })
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

    async fn set(&self, key: &str, value: &str) -> Result<(), String> {
        unsafe {
            env::set_var(key, value);
        };
        // Update .env file
        let env_path = &self.env_file;
        let content = fs::read_to_string(env_path).unwrap_or_default();
        let mut lines: Vec<String> = Vec::new();
        let mut found = false;

        for line in content.lines() {
            if let Some((k, _)) = line.split_once('=') {
                if k.trim() == key {
                    lines.push(format!("{key}={value}"));
                    found = true;
                } else {
                    lines.push(line.to_string());
                }
            } else {
                lines.push(line.to_string());
            }
        }

        if !found {
            lines.push(format!("{key}={value}"));
        }

        fs::write(env_path, lines.join("\n")).map_err(|e| e.to_string())?;

        Ok(())
    }

    async fn del(&self, key: &str) {
        unsafe {
            env::remove_var(key);
        };
        // Remove from file
        let env_path = &self.env_file;
        if let Ok(content) = fs::read_to_string(env_path) {
            let lines: Vec<String> = content
                .lines()
                .filter(|line| {
                    if let Some((k, _)) = line.split_once('=') {
                        k.trim() != key
                    } else {
                        true
                    }
                })
                .map(|l| l.to_string())
                .collect();

            let _ = fs::write(env_path, lines.join("\n"));
        }
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
    #[schemars(with = "std::collections::HashMap<String, String>")]
    map: DashMap<String, String>,
}

impl MapConfigManager {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            map: DashMap::new(),
        })
    }
}

#[typetag::serde]
#[async_trait]
impl ConfigManagerType for MapConfigManager {
    async fn keys(&self) -> Vec<String> {
        self.map.iter().map(|entry| entry.key().clone()).collect()
    }

    async fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).map(|v| v.clone())
    }

    async fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.map.insert(key.to_string(), value.to_string());
        Ok(())
    }

    async fn del(&self, key: &str) {
        self.map.remove(key);
    }

    fn clone_box(&self) -> Box<dyn ConfigManagerType> {
        Box::new(self.clone())
    }
    fn debug_box(&self) -> String {
        format!("MapConfigManager({} entries)", self.map.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::write;
    use tempfile::{TempDir, tempdir};

    #[tokio::test]
    async fn test_map_config_manager_basic() {
        let mgr = MapConfigManager::new();

        // Set a config value
        mgr.set("foo", "bar").await.unwrap();
        assert_eq!(mgr.get("foo").await, Some("bar".to_string()));

        // Overwrite it
        mgr.set("foo", "baz").await.unwrap();
        assert_eq!(mgr.get("foo").await, Some("baz".to_string()));

        // Get keys
        let keys = mgr.keys().await;
        assert_eq!(keys, vec!["foo".to_string()]);

        // Delete it
        mgr.del("foo").await;
        assert_eq!(mgr.get("foo").await, None);
    }

    #[tokio::test]
    async fn test_map_config_manager_as_vec() {
        let mgr = MapConfigManager::new();
        mgr.set("a", "1").await.unwrap();
        mgr.set("b", "2").await.unwrap();

        let mut config = mgr.as_vec().await;
        config.sort(); // ensure deterministic order for test

        assert_eq!(
            config,
            vec![
                ("a".to_string(), "1".to_string()),
                ("b".to_string(), "2".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn test_env_config_manager_read_only() {
        // Set an env var temporarily using `set_var` and remove it after the test
        let key = "TEMP_TEST_ENV_VAR";
        let value = "test_value";

        // Save existing value (if any)
        let old_value = std::env::var(key).ok();

        unsafe { std::env::set_var(key, value) };

        let mgr = EnvConfigManager::new(PathBuf::from("/nonexistent.env")); // No load

        assert_eq!(mgr.get(key).await, Some(value.to_string()));
        assert!(mgr.keys().await.contains(&key.to_string()));

        // Clean up
        if let Some(v) = old_value {
            unsafe { std::env::set_var(key, v) };
        } else {
            unsafe { std::env::remove_var(key) };
        }
    }

    #[tokio::test]
    async fn test_env_config_manager_set_and_delete_safely() {
        let key = "TEMP_ENV_VAR_FOR_TEST";
        let value = "secret";

        // Save previous value
        let backup = std::env::var(key).ok();
        let tmp = TempDir::new().unwrap();
        let env = tmp.path().join(".env");

        let mgr = EnvConfigManager::new(PathBuf::from(env));

        // Unsafe block for set/remove
        mgr.set(key, value).await.unwrap();
        assert_eq!(std::env::var(key).ok(), Some(value.to_string()));
        assert_eq!(mgr.get(key).await, Some(value.to_string()));

        mgr.del(key).await;
        assert_eq!(std::env::var(key).ok(), None);

        // Restore original value if it existed
        if let Some(v) = backup {
            unsafe { std::env::set_var(key, v) };
        }
    }

    #[tokio::test]
    async fn test_env_config_manager_with_temp_env_file() {
        let dir = tempdir().unwrap();
        let env_path = dir.path().join(".env");

        let content = "API_KEY=abc123\nLOG_LEVEL=debug\n";
        write(&env_path, content).unwrap();

        let mgr = EnvConfigManager::new(env_path.clone());

        assert_eq!(mgr.get("API_KEY").await, Some("abc123".to_string()));
        assert_eq!(mgr.get("LOG_LEVEL").await, Some("debug".to_string()));
    }
}
