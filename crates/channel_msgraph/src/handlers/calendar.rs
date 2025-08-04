use std::sync::Arc;

use crate::handlers::{interpolate_key, SubscriptionField, SubscriptionHandler};
use crate::{graph_client::GraphState, handlers::SubscriptionSpec};
use chrono::{Duration, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::info;


pub struct CalendarHandler;

#[async_trait::async_trait]
impl SubscriptionHandler for CalendarHandler {
    fn spec(&self) -> SubscriptionSpec {
        calendar_spec()
    }

    async fn add(
        &self,
        config: Arc<DashMap<String, String>>,
        graph_state: Arc<RwLock<GraphState>>,
    ) -> anyhow::Result<()> {
        calendar_add(config, graph_state).await
    }

    async fn delete(
        &self,
        config: Arc<DashMap<String, String>>,
        graph_state: Arc<RwLock<GraphState>>,
    ) -> anyhow::Result<()> {
        calendar_delete(config, graph_state).await
    }
}

pub async fn calendar_add(
    config: Arc<DashMap<String, String>>,
    graph_state: Arc<RwLock<GraphState>>,
) -> anyhow::Result<()> {
    let state = graph_state.read().await;
    let domain = state.config.domain.clone();
    let mut graph = state.client.clone();
    drop(state); // release lock early

    let mut payload_map = serde_json::Map::new();
    for entry in config.iter() {
        payload_map.insert(entry.key().clone(), Value::String(entry.value().clone()));
    }
    let payload = Value::Object(payload_map);

    let spec = calendar_spec();

    for field in &spec.fields {
        if field.required && !payload.get(field.key).and_then(|v| v.as_str()).is_some() {
            return Err(anyhow::anyhow!(format!("Missing required field: {}", field.key)));
        }
    }

    let key = interpolate_key(&spec.key_template, &payload)
        .ok_or_else(|| anyhow::anyhow!("Invalid key interpolation"))?;

    info!("Registering calendar subscription with key: {}", key);

    let user_id = payload
        .get("calendar_user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("me");

    let url = format!("https://{}/notification", domain);
    let expiration = Utc::now() + Duration::minutes(4230);
    let resource = format!("/users/{}/events", user_id);

    let subscription = json!({
        "changeType": "created,updated",
        "notificationUrl": url,
        "resource": resource,
        "expirationDateTime": expiration.to_rfc3339(),
        "clientState": "calendar_state"
    });

    graph
        .v1()
        .subscriptions()
        .create_subscription(&subscription)
        .send()
        .await?;

    Ok(())
}


pub async fn calendar_delete(
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
        .get("calendar_user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("me");

    let resource = format!("/users/{}/events", user_id);
    let subs: Value = graph.v1().subscriptions().list_subscription().send().await?.json().await?;
    if let Some(items) = subs.get("value").and_then(|v| v.as_array()) {
        for item in items {
            if item.get("resource").and_then(|v| v.as_str()) == Some(&resource) {
                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                    graph.v1().subscription(id).delete_subscription().send().await?;
                    info!("Deleted subscription with id: {}", id);
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
            if parts.len() == 5 && parts[2] == "calendar" && parts[3] == "user" {
                let user_id = parts[4];
                states.push((format!("/users/{}/events", user_id), "calendar_state".to_string()));
            }
        }
    }

    states
}

pub fn calendar_spec() -> SubscriptionSpec {
    SubscriptionSpec {
        subscription_type: "calendar-user",
        description: "Monitor calendar events for a user.",
        key_template: "msgraph:{tenant_id}:{domain}:calendar:user:{calendar_user_id}",
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
                key: "calendar_user_id",
                required: true,
                description: "User ID for the calendar",
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
        let spec = calendar_spec();
        let payload = json!({
            "tenant_id": "tenant1",
            "domain": "example.com",
            "calendar_user_id": "user123"
        });
        let result = interpolate_key(&spec.key_template, &payload);
        assert_eq!(
            result.unwrap(),
            "msgraph:tenant1:example.com:calendar:user:user123"
        );
    }

    #[test]
    fn test_interpolate_key_missing_field() {
        let spec = calendar_spec();
        let payload = json!({
            "tenant_id": "tenant1",
            "calendar_user_id": "user123"
        });
        let result = interpolate_key(&spec.key_template, &payload);
        assert!(result.is_none());
    }

    #[test]
    fn test_calendar_spec_fields_required() {
        let spec = calendar_spec();
        assert_eq!(spec.subscription_type, "calendar-user");
        assert_eq!(spec.fields.len(), 3);
        for field in &spec.fields {
            assert!(field.required);
        }
    }

    #[test]
    fn test_get_client_states_extraction() {
        let config = DashMap::new();
        config.insert(
            "msgraph:tenant1:example.com:calendar:user:user42".to_string(),
            "some_value".to_string(),
        );
        config.insert(
            "msgraph:tenant1:example.com:email:user42".to_string(), // irrelevant type
            "email_value".to_string(),
        );

        let states = get_client_states(&config);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].0, "/users/user42/events");
        assert_eq!(states[0].1, "calendar_state");
    }
}