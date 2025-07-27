use std::sync::Arc;

use crate::graph_client::GraphState;
use crate::subscriptions::register_subscription;
use chrono::{Duration, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::sync::RwLock;

pub fn init(config: Arc<DashMap<String, String>>) {
    register_subscription("teams-channel-messages", config.clone(), move |graph_state: &Arc<RwLock<GraphState>>, config: Arc<DashMap<String, String>>| {
        let graph_state = Arc::clone(graph_state);

        async move {
            let state = graph_state.read().await;
            let mut graph = state.client.clone();
            let domain = state.config.domain.clone();
            drop(state); // release lock early

            let team_name = config.get("MS_GRAPH_TEAMS_TEAM_NAME")
                .ok_or_else(|| anyhow::anyhow!("Missing MS_GRAPH_TEAMS_TEAM_NAME"))?
                .to_string();

            let channel_name = config.get("MS_GRAPH_TEAMS_CHANNEL_NAME")
                .ok_or_else(|| anyhow::anyhow!("Missing MS_GRAPH_TEAMS_CHANNEL_NAME"))?
                .to_string();

            // Step 1: Find team ID
            let teams_resp: Value = graph
                .v1()
                .me()
                .joined_teams()
                .list_joined_teams()
                .send()
                .await?
                .json()
                .await?;

            let team_id = teams_resp["value"]
                .as_array()
                .and_then(|teams| {
                    teams.iter()
                        .find(|team| team["displayName"] == team_name)?
                        .get("id")?
                        .as_str()
                        .map(|s| s.to_string())
                })
                .ok_or_else(|| anyhow::anyhow!("Team '{}' not found", team_name))?;

            // Step 2: Find channel ID
            let channels_resp: Value = graph
                .v1()
                .team(&team_id)
                .channels()
                .list_channels()
                .send()
                .await?
                .json()
                .await?;

            let channel_id = channels_resp["value"]
                .as_array()
                .and_then(|channels| {
                    channels.iter()
                        .find(|ch| ch["displayName"] == channel_name)?
                        .get("id")?
                        .as_str()
                        .map(|s| s.to_string())
                })
                .ok_or_else(|| anyhow::anyhow!("Channel '{}' not found in team '{}'", channel_name, team_name))?;

            let resource = format!("/teams/{}/channels/{}/messages", team_id, channel_id);
            let url = format!("https://{}/notification", domain);
            let expiration = Utc::now() + Duration::minutes(59); // Teams max duration is 1 hour

            let subscription = json!({
                "changeType": "created",
                "notificationUrl": url,
                "resource": resource,
                "expirationDateTime": expiration.to_rfc3339(),
                "clientState": "teams_state"
            });

            graph
                .v1()
                .subscriptions()
                .create_subscription(&subscription)
                .send()
                .await?;

            Ok(())
        }
    });
}


pub fn get_client_states(config: &DashMap<String, String>) -> Vec<(String, String)> {
    let team_name = config.get("MS_GRAPH_TEAMS_TEAM_NAME").map(|v| v.clone()).unwrap_or_default();
    let channel_name = config.get("MS_GRAPH_TEAMS_CHANNEL_NAME").map(|v| v.clone()).unwrap_or_default();

    vec![(
        format!("/teams/{team_name}/channels/{channel_name}/messages"),
        "teams_state".to_string(),
    )]
}
