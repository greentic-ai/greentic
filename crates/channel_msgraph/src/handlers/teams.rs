use crate::graph_client::GraphState;
use crate::subscriptions::register_subscription;
use chrono::{Duration, Utc};
use serde_json::json;

pub fn init() {
    register_subscription("teams-channel-messages", |state: &GraphState| {
        let mut graph = state.client.clone();
        let domain = state.config.domain.clone();

        // Replace with actual team and channel IDs
        let team_id = "YOUR_TEAM_ID";
        let channel_id = "YOUR_CHANNEL_ID";

        async move {
            let url = format!("https://{}/notification", domain);
            let expiration = Utc::now() + Duration::minutes(59); // Teams = 1 hour max

            let resource = format!("/teams/{}/channels/{}/messages", team_id, channel_id);

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

pub fn get_client_states() -> Vec<(String, String)> {
    vec![(
        "/teams/{team-id}/channels/{channel-id}/messages".to_string(),
        "teams_state".to_string(),
    )]
}