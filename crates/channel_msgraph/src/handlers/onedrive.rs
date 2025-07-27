use std::sync::Arc;

use crate::graph_client::GraphState;
use crate::subscriptions::register_subscription;
use chrono::{Duration, Utc};
use dashmap::DashMap;
use serde_json::json;
use tokio::sync::RwLock;

pub fn init(config: Arc<DashMap<String, String>>) {
    register_subscription("onedrive-root", config.clone(), move |graph_state: &Arc<RwLock<GraphState>>, config: Arc<DashMap<String, String>>| {
        let graph_state = Arc::clone(graph_state);

        async move {
            let state = graph_state.read().await;
            let mut graph = state.client.clone();
            let domain = state.config.domain.clone();
            drop(state); // Release read lock

            let url = format!("https://{}/notification", domain);
            let expiration = Utc::now() + Duration::minutes(4230);

            // Allow for custom user ID or fallback to "me"
            let user_id = config.get("MS_GRAPH_ONEDRIVE_USER_ID")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "me".to_string());

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
    });
}


pub fn get_client_states(config: &DashMap<String, String>) -> Vec<(String, String)> {
    let user_id = config
        .get("MS_GRAPH_ONEDRIVE_USER_ID")
        .map(|v| v.clone())
        .unwrap_or_else(|| "me".to_string());

    vec![(
        format!("/users/{}/drive/root", user_id),
        "onedrive_root_state".to_string(),
    )]
}