use crate::graph_client::GraphState;
use crate::subscriptions::register_subscription;
use chrono::{Duration, Utc};
use serde_json::json;

pub fn init() {
    register_subscription("sharepoint-list-items", |state: &GraphState| {
        let mut graph = state.client.clone();
        let domain = state.config.domain.clone();

        // Replace with actual site/list IDs at runtime if possible
        let site_id = "YOUR_SITE_ID";
        let list_id = "YOUR_LIST_ID";

        async move {
            let url = format!("https://{}/notification", domain);
            let expiration = Utc::now() + Duration::minutes(4230);

            let subscription = json!({
                "changeType": "created,updated",
                "notificationUrl": url,
                "resource": format!("/sites/{}/lists/{}/items", site_id, list_id),
                "expirationDateTime": expiration.to_rfc3339(),
                "clientState": "sharepoint_list_state"
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
        "/sites/{site-id}/lists/{list-id}/items".to_string(),
        "sharepoint_state".to_string(),
    )]
}