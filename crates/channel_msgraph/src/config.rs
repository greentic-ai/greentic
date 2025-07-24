use anyhow::{anyhow, Result};
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
        config_map: &dashmap::DashMap<String, String>,
        secret_map: &dashmap::DashMap<String, String>,
    ) -> Result<Self> {
        let domain = config_map
            .get("MS_GRAPH_DOMAIN")
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("Missing config key: MS_GRAPH_DOMAIN"))?;

        let tenant_id = config_map
            .get("MS_GRAPH_TENANT_ID")
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("Missing config key: MS_GRAPH_TENANT_ID"))?;

        let client_id = config_map
            .get("MS_GRAPH_CLIENT_ID")
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("Missing config key: MS_GRAPH_CLIENT_ID"))?;

        let client_secret = secret_map
            .get("MS_GRAPH_CLIENT_SECRET")
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("Missing secret key: MS_GRAPH_CLIENT_SECRET"))?;

        Ok(MsGraphConfig {
            domain,
            tenant_id,
            client_id,
            client_secret,
        })
    }

    pub fn as_env_map(&self) -> HashMap<String, String> {
        HashMap::from([
            ("MS_GRAPH_DOMAIN".into(), self.domain.clone()),
            ("MS_GRAPH_TENANT_ID".into(), self.tenant_id.clone()),
            ("MS_GRAPH_CLIENT_ID".into(), self.client_id.clone()),
            ("MS_GRAPH_CLIENT_SECRET".into(), self.client_secret.clone()),
        ])
    }
}
