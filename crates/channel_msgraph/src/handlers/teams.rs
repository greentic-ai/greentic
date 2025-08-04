use std::sync::Arc;

use crate::handlers::{interpolate_key, SubscriptionField, SubscriptionHandler, SubscriptionSpec};
use crate::graph_client::GraphState;
use chrono::{Duration, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::sync::RwLock;
use tracing::info;

pub struct TeamsHandler;

#[async_trait::async_trait]
impl SubscriptionHandler for TeamsHandler {
    fn spec(&self) -> SubscriptionSpec {
        teams_spec()
    }

    async fn add(&self, config: Arc<DashMap<String, String>>, graph_state: Arc<RwLock<GraphState>>) -> anyhow::Result<()> {
        teams_add(config, graph_state).await
    }

    async fn delete(&self, config: Arc<DashMap<String, String>>, graph_state: Arc<RwLock<GraphState>>) -> anyhow::Result<()> {
        teams_delete(config, graph_state).await
    }
}

pub async fn teams_add(
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

    let spec = teams_spec();
    for field in &spec.fields {
        if field.required && !payload.get(field.key).and_then(|v| v.as_str()).is_some() {
            return Err(anyhow::anyhow!(format!("Missing required field: {}", field.key)));
        }
    }

    let key = interpolate_key(&spec.key_template, &payload)
        .ok_or_else(|| anyhow::anyhow!("Invalid key interpolation"))?;

    info!("Registering Teams subscription with key: {}", key);

    let team_name = payload.get("teams_team_name").and_then(|v| v.as_str()).unwrap_or("general");
    let channel_name = payload.get("teams_channel_name").and_then(|v| v.as_str()).unwrap_or("general");

    let resource = format!("/teams/{}/channels/{}/messages", team_name, channel_name);
    let url = format!("https://{}/notification", domain);
    let expiration = Utc::now() + Duration::minutes(4230);

    let subscription = json!({
        "changeType": "created,updated",
        "notificationUrl": url,
        "resource": resource,
        "expirationDateTime": expiration.to_rfc3339(),
        "clientState": "teams_channel_state"
    });

    graph
        .v1()
        .subscriptions()
        .create_subscription(&subscription)
        .send()
        .await?;

    Ok(())
}

pub async fn teams_delete(
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

    let team_name = payload.get("teams_team_name").and_then(|v| v.as_str()).unwrap_or("general");
    let channel_name = payload.get("teams_channel_name").and_then(|v| v.as_str()).unwrap_or("general");
    let resource = format!("/teams/{}/channels/{}/messages", team_name, channel_name);

    let subs: Value = graph.v1().subscriptions().list_subscription().send().await?.json().await?;
    if let Some(items) = subs.get("value").and_then(|v| v.as_array()) {
        for item in items {
            if item.get("resource").and_then(|v| v.as_str()) == Some(&resource) {
                if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                    graph.v1().subscription(id).delete_subscription().send().await?;
                    info!("Deleted Teams subscription with id: {}", id);
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
            if parts.len() == 6 && parts[2] == "teams" && parts[3] == "team" && parts[4] == "channel" {
                let channel = parts[5];
                let team = parts[4];
                states.push((
                    format!("/teams/{}/channels/{}/messages", team, channel),
                    "teams_channel_state".to_string(),
                ));
            }
        }
    }
    states
}

pub fn teams_spec() -> SubscriptionSpec {
    SubscriptionSpec {
        subscription_type: "teams-channel",
        description: "Monitor messages in a specific Teams channel.",
        key_template: "msgraph:{tenant_id}:{domain}:teams:team:{teams_team_name}:channel:{teams_channel_name}",
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
                key: "teams_team_name",
                required: true,
                description: "Microsoft Teams team name",
            },
            SubscriptionField {
                key: "teams_channel_name",
                required: true,
                description: "Microsoft Teams channel name",
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
    fn test_interpolate_key_teams() {
        let spec = teams_spec();
        let payload = json!({
            "tenant_id": "tenant1",
            "domain": "example.com",
            "teams_team_name": "Alpha",
            "teams_channel_name": "General"
        });
        let result = interpolate_key(&spec.key_template, &payload);
        assert_eq!(
            result.unwrap(),
            "msgraph:tenant1:example.com:teams:team:Alpha:channel:General"
        );
    }

    #[test]
    fn test_missing_fields_teams() {
        let spec = teams_spec();
        let payload = json!({
            "tenant_id": "tenant1",
            "teams_team_name": "Alpha"
        });
        let result = interpolate_key(&spec.key_template, &payload);
        assert!(result.is_none());
    }

    #[test]
    fn test_get_client_states_teams() {
        let config = DashMap::new();
        config.insert(
            "msgraph:tenant1:example.com:teams:team:Alpha:channel:General".to_string(),
            "value".to_string(),
        );
        config.insert(
            "msgraph:tenant1:example.com:calendar:user:user1".to_string(),
            "other".to_string(),
        );

        let states = get_client_states(&config);
        assert_eq!(states.len(), 1);
        assert_eq!(
            states[0].0,
            "/teams/Alpha/channels/General/messages"
        );
        assert_eq!(states[0].1, "teams_channel_state");
    }
}
