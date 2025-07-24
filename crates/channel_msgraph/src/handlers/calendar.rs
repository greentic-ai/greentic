use crate::graph_client::GraphState;
use crate::subscriptions::register_subscription;
use chrono::{Duration, Utc};
use serde_json::json;

pub fn init() {
    register_subscription("calendar-events", |state: &GraphState| {
        // ✅ Clone what's needed *before* the async block
        let domain = state.config.domain.clone();
        let mut graph = state.client.clone();
        async move {
            let url = format!("https://{}/notification", domain);
            let expiration = Utc::now() + Duration::minutes(4230);

            let subscription = json!({
                "changeType": "created,updated",
                "notificationUrl": url,
                "resource": "/users/me/events",
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
    });
}

pub fn get_client_states() -> Vec<(String, String)> {
    vec![(
        "/users/me/events".to_string(),
        "calendar_state".to_string(),
    )]
}