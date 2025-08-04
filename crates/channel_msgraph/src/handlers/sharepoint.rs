use std::sync::Arc;

use crate::graph_client::GraphState;
use crate::handlers::{interpolate_key, SubscriptionField, SubscriptionHandler, SubscriptionSpec};
use chrono::{Duration, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::info;

pub struct SharePointHandler;

#[async_trait::async_trait]
impl SubscriptionHandler for SharePointHandler {
    fn spec(&self) -> SubscriptionSpec {
        sharepoint_spec()
    }

    async fn add(&self, config: Arc<DashMap<String, String>>, graph_state: Arc<RwLock<GraphState>>) -> anyhow::Result<()> {
        let state = graph_state.read().await;
        let domain = state.config.domain.clone();
        let mut graph = state.client.clone();
        drop(state);

        let mut payload_map = serde_json::Map::new();
        for entry in config.iter() {
            payload_map.insert(entry.key().clone(), Value::String(entry.value().clone()));
        }
        let payload = Value::Object(payload_map);

        for field in &self.spec().fields {
            if field.required && !payload.get(field.key).and_then(|v| v.as_str()).is_some() {
                return Err(anyhow::anyhow!("Missing required field: {}", field.key));
            }
        }

        let key = interpolate_key(&self.spec().key_template, &payload)
            .ok_or_else(|| anyhow::anyhow!("Invalid key interpolation"))?;
        info!("Registering SharePoint subscription with key: {}", key);

        let hostname = payload.get("sharepoint_hostname").and_then(|v| v.as_str()).unwrap();
        let site_path = payload.get("sharepoint_site_path").and_then(|v| v.as_str()).unwrap();
        let list_name = payload.get("sharepoint_list_name").and_then(|v| v.as_str()).unwrap();

        let url = format!("https://{}/notification", domain);
        let expiration = Utc::now() + Duration::minutes(4230);
        let resource = format!("https://{}/_api/web/GetList('{}')", hostname, site_path.to_owned() + "/lists/" + list_name);

        let subscription = json!({
            "changeType": "updated",
            "notificationUrl": url,
            "resource": resource,
            "expirationDateTime": expiration.to_rfc3339(),
            "clientState": "sharepoint_state"
        });

        graph
            .v1()
            .subscriptions()
            .create_subscription(&subscription)
            .send()
            .await?;

        Ok(())
    }

    async fn delete(&self, config: Arc<DashMap<String, String>>, graph_state: Arc<RwLock<GraphState>>) -> anyhow::Result<()> {
        let state = graph_state.read().await;
        let mut graph = state.client.clone();
        drop(state);

        let mut payload_map = serde_json::Map::new();
        for entry in config.iter() {
            payload_map.insert(entry.key().clone(), Value::String(entry.value().clone()));
        }
        let payload = Value::Object(payload_map);

        let hostname = payload.get("sharepoint_hostname").and_then(|v| v.as_str()).unwrap();
        let site_path = payload.get("sharepoint_site_path").and_then(|v| v.as_str()).unwrap();
        let list_name = payload.get("sharepoint_list_name").and_then(|v| v.as_str()).unwrap();
        let resource = format!("https://{}/_api/web/GetList('{}')", hostname, site_path.to_owned() + "/lists/" + list_name);

        let subs: Value = graph.v1().subscriptions().list_subscription().send().await?.json().await?;
        if let Some(items) = subs.get("value").and_then(|v| v.as_array()) {
            for item in items {
                if item.get("resource").and_then(|v| v.as_str()) == Some(&resource) {
                    if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                        graph.v1().subscription(id).delete_subscription().send().await?;
                        info!("Deleted SharePoint subscription with id: {}", id);
                    }
                }
            }
        }

        Ok(())
    }
}

pub fn get_client_states(config: &DashMap<String, String>) -> Vec<(String, String)> {
    let mut states = vec![];

    for key in config.iter().map(|kv| kv.key().clone()) {
        if let Some(stripped) = key.strip_prefix("msgraph:") {
            let parts: Vec<&str> = stripped.split(':').collect();
            if parts.len() == 6 && parts[2] == "sharepoint" && parts[3] == "list" {
                let list_name = parts[4];
                states.push((format!("sharepoint:{}", list_name), "sharepoint_state".to_string()));
            }
        }
    }

    states
}

pub fn sharepoint_spec() -> SubscriptionSpec {
    SubscriptionSpec {
        subscription_type: "sharepoint-list",
        description: "Monitor changes to a SharePoint list.",
        key_template: "msgraph:{tenant_id}:{domain}:sharepoint:host:{sharepoint_hostname}:site:{sharepoint_site_path}:list:{sharepoint_list_name}",
        fields: vec![
            SubscriptionField {
                key: "tenant_id",
                required: true,
                description: "Azure tenant ID",
            },
            SubscriptionField {
                key: "domain",
                required: true,
                description: "Your public domain",
            },
            SubscriptionField {
                key: "sharepoint_hostname",
                required: true,
                description: "The SharePoint host (e.g. contoso.sharepoint.com)",
            },
            SubscriptionField {
                key: "sharepoint_site_path",
                required: true,
                description: "Path to the SharePoint site (e.g. /sites/engineering)",
            },
            SubscriptionField {
                key: "sharepoint_list_name",
                required: true,
                description: "Name of the SharePoint list (e.g. Tasks)",
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use dashmap::DashMap;

    #[test]
    fn test_interpolate_key_success() {
        let spec = sharepoint_spec();
        let payload = json!({
            "tenant_id": "tenant1",
            "domain": "example.com",
            "sharepoint_hostname": "contoso.sharepoint.com",
            "sharepoint_site_path": "/sites/engineering",
            "sharepoint_list_name": "Tasks"
        });
        let result = interpolate_key(&spec.key_template, &payload);
        assert_eq!(
            result.unwrap(),
            "msgraph:tenant1:example.com:sharepoint:host:contoso.sharepoint.com:site:/sites/engineering:list:Tasks"
        );
    }

    #[test]
    fn test_interpolate_key_missing_field() {
        let spec = sharepoint_spec();
        let payload = json!({
            "tenant_id": "tenant1",
            "domain": "example.com",
            "sharepoint_hostname": "contoso.sharepoint.com"
        });
        let result = interpolate_key(&spec.key_template, &payload);
        assert!(result.is_none());
    }

    #[test]
    fn test_sharepoint_spec_fields_required() {
        let spec = sharepoint_spec();
        assert_eq!(spec.subscription_type, "sharepoint-list");
        assert_eq!(spec.fields.len(), 5);
        for field in &spec.fields {
            assert!(field.required);
        }
    }

    #[test]
    fn test_get_client_states_extraction() {
        let config = DashMap::new();
        config.insert(
            "msgraph:tenant1:example.com:sharepoint:list:Tasks".to_string(),
            "some_value".to_string(),
        );
        config.insert(
            "msgraph:tenant1:example.com:email:user42".to_string(),
            "irrelevant".to_string(),
        );

        let states = get_client_states(&config);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].0, "sharepoint:Tasks");
        assert_eq!(states[0].1, "sharepoint_state");
    }
}
