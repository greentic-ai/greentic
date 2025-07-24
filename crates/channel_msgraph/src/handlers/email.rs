use crate::graph_client::GraphState;
use crate::subscriptions::register_subscription;
use chrono::{Duration, Utc};
use serde_json::json;

pub fn init() {
    register_subscription("email-messages", |state: &GraphState| {
        let mut graph = state.client.clone();
        let domain = state.config.domain.clone();

        async move {
            let url = format!("https://{}/notification", domain);
            let expiration = Utc::now() + Duration::minutes(4230);

            let subscription = json!({
                "changeType": "created,updated",
                "notificationUrl": url,
                "resource": "/users/me/messages",
                "expirationDateTime": expiration.to_rfc3339(),
                "clientState": "email_state"
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
        "/me/messages".to_string(),
        "email_state".to_string(),
    )]
}