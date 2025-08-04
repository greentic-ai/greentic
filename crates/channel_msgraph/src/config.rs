use anyhow::{anyhow, Result};
use dashmap::DashMap;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct MsGraphConfig {
    pub domain: String,
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: String,
}

impl MsGraphConfig {
    pub fn from_maps(
        tenant_id: &str,
        domain: &str,
        config_map: &DashMap<String, String>,
        secret_map: &DashMap<String, String>,
    ) -> Result<Self> {
        let key_prefix = format!("msgraph:{tenant_id}:{domain}");

        let client_id = config_map
            .get(&format!("{key_prefix}:client_id"))
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("Missing config: {key_prefix}:client_id"))?;

        let client_secret = secret_map
            .get(&format!("{key_prefix}:client_secret"))
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("Missing secret: {key_prefix}:client_secret"))?;

        Ok(MsGraphConfig {
            domain: domain.to_string(),
            tenant_id: tenant_id.to_string(),
            client_id,
            client_secret,
        })
    }


    pub fn as_env_map(&self) -> HashMap<String, String> {
        let prefix = format!("msgraph:{}:{}", self.tenant_id, self.domain);
        HashMap::from([
            (format!("{prefix}:client_id"), self.client_id.clone()),
            (format!("{prefix}:client_secret"), self.client_secret.clone()),
        ])
    }
}
