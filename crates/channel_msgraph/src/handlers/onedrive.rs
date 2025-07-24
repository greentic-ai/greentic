use crate::graph_client::GraphState;
use crate::subscriptions::register_subscription;
use chrono::{Duration, Utc};
use serde_json::json;

pub fn init() {
    register_subscription("onedrive-root", |state: &GraphState| {
        let mut graph = state.client.clone();
        let domain = state.config.domain.clone();

        async move {
            let url = format!("https://{}/notification", domain);
            let expiration = Utc::now() + Duration::minutes(4230);

            let subscription = json!({
                "changeType": "created,updated",
                "notificationUrl": url,
                "resource": "/users/me/drive/root",
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
    });
}
