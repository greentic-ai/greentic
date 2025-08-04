use std::sync::Arc;

use crate::handlers::{interpolate_key, SubscriptionField, SubscriptionHandler, SubscriptionSpec};
use crate::graph_client::GraphState;
use chrono::{Duration, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::info;

pub struct OneDriveHandler;

#[async_trait::async_trait]
impl SubscriptionHandler for OneDriveHandler {
    fn spec(&self) -> SubscriptionSpec {
        onedrive_spec()
    }

    async fn add(
        &self,
        config: Arc<DashMap<String, String>>,
        graph_state: Arc<RwLock<GraphState>>,
    ) -> anyhow::Result<()> {
        onedrive_add(config, graph_state).await
    }

    async fn delete(
        &self,
        config: Arc<DashMap<String, String>>,
        graph_state: Arc<RwLock<GraphState>>,
    ) -> anyhow::Result<()> {
        onedrive_delete(config, graph_state).await
    }
}

pub async fn onedrive_add(
    config: Arc<DashMap<String, String>>,
    graph_state: Arc<RwLock<GraphState>>,
) -> anyhow::Result<()> {
    let state = graph_state.read().await;
    let domain = state.config.domain.clone();
    let mut graph = state.client.clone();
    drop(state);

    let mut payload_map = serde_json::Map::new();
    for entry in config.iter() {
        payload_map.insert(entry.key().clone(), Value::String(entry.value().clone()));
    }
    let payload = Value::Object(payload_map);

    let spec = onedrive_spec();
    for field in &spec.fields {
        if field.required && !payload.get(field.key).and_then(|v| v.as_str()).is_some() {
            return Err(anyhow::anyhow!(format!("Missing required field: {}", field.key)));
        }
    }

    let key = interpolate_key(&spec.key_template, &payload)
        .ok_or_else(|| anyhow::anyhow!("Invalid key interpolation"))?;

    info!("Registering OneDrive subscription with key: {}", key);

    let user_id = payload
        .get("onedrive_user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("me");

    let url = format!("https://{}/notification", domain);
    let expiration = Utc::now() + Duration::minutes(4230);
    let resource = format!("/users/{}/drive/root", user_id);

    let subscription = json!({
        "changeType": "created,updated",
        "notificationUrl": url,
        "resource": resource,
        "expirationDateTime": expiration.to_rfc3339(),
        "clientState": "onedrive_root_state"
    });

    graph
        .v1()
        .subscriptions()
        .create_subscription(&subscription)
        .send()
        .await?;

    Ok(())
}

pub async fn onedrive_delete(
    config: Arc<DashMap<String, String>>,
    graph_state: Arc<RwLock<GraphState>>,
) -> anyhow::Result<()> {
    let state = graph_state.read().await;
    let mut graph = state.client.clone();
    drop(state);

    let mut payload_map = serde_json::Map::new();
    for entry in config.iter() {
        payload_map.insert(entry.key().clone(), Value::String(entry.value().clone()));
    }
    let payload = Value::Object(payload_map);

    let user_id = payload
        .get("onedrive_user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("me");

    let resource = format!("/users/{}/drive/root", user_id);
    let subs: Value = graph.v1().subscriptions().list_subscription().send().await?.json().await?;
    if let Some(items) = subs.get("value").and_then(|v| v.as_array()) {
        for item in items {
            if item.get("resource").and_then(|v| v.as_str()) == Some(&resource) {
                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                    graph.v1().subscription(id).delete_subscription().send().await?;
                    info!("Deleted OneDrive subscription with id: {}", id);
                }
            }
        }
    }

    Ok(())
}

pub fn get_client_states(config: &DashMap<String, String>) -> Vec<(String, String)> {
    let mut states = vec![];

    for key in config.iter().map(|kv| kv.key().clone()) {
        if let Some(stripped) = key.strip_prefix("msgraph:") {
            let parts: Vec<&str> = stripped.split(':').collect();
            if parts.len() == 5 && parts[2] == "onedrive" && parts[3] == "user" {
                let user_id = parts[4];
                states.push((format!("/users/{}/drive/root", user_id), "onedrive_root_state".to_string()));
            }
        }
    }

    states
}

pub fn onedrive_spec() -> SubscriptionSpec {
    SubscriptionSpec {
        subscription_type: "onedrive-user",
        description: "Monitor root folder changes in a user's OneDrive.",
        key_template: "msgraph:{tenant_id}:{domain}:onedrive:user:{onedrive_user_id}",
        fields: vec![
            SubscriptionField {
                key: "tenant_id",
                required: true,
                description: "Azure tenant ID",
            },
            SubscriptionField {
                key: "domain",
                required: true,
                description: "Domain (e.g., contoso.com)",
            },
            SubscriptionField {
                key: "onedrive_user_id",
                required: true,
                description: "User ID of the OneDrive account",
            },
        ],
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use dashmap::DashMap;
    use serde_json::json;

    #[test]
    fn test_interpolate_key_success() {
        let spec = onedrive_spec();
        let payload = json!({
            "tenant_id": "tenant1",
            "domain": "example.com",
            "onedrive_user_id": "alice"
        });
        let result = interpolate_key(&spec.key_template, &payload);
        assert_eq!(
            result.unwrap(),
            "msgraph:tenant1:example.com:onedrive:user:alice"
        );
    }

    #[test]
    fn test_interpolate_key_missing() {
        let spec = onedrive_spec();
        let payload = json!({
            "tenant_id": "tenant1",
            "domain": "example.com"
        });
        let result = interpolate_key(&spec.key_template, &payload);
        assert!(result.is_none());
    }

    #[test]
    fn test_get_client_states_extraction() {
        let config = DashMap::new();
        config.insert(
            "msgraph:tenant1:example.com:onedrive:user:alice".to_string(),
            "value".to_string(),
        );
        config.insert(
            "msgraph:tenant1:example.com:calendar:user:alice".to_string(),
            "value".to_string(),
        );

        let states = get_client_states(&config);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].0, "/users/alice/drive/root");
        assert_eq!(states[0].1, "onedrive_root_state");
    }
}
