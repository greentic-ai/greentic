use std::sync::Arc;

use crate::graph_client::GraphState;
use crate::subscriptions::register_subscription;
use chrono::{Duration, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use tokio::sync::RwLock;

pub fn init(config: Arc<DashMap<String, String>>) {
    register_subscription("sharepoint-list-items", config.clone(), move |graph_state: &Arc<RwLock<GraphState>>, config: Arc<DashMap<String, String>>| {
        let graph_state = Arc::clone(graph_state);

        async move {
            let state = graph_state.read().await;
            let mut graph = state.client.clone();
            let domain = state.config.domain.clone();
            drop(state); // release early

            let hostname = config.get("MS_GRAPH_SHAREPOINT_HOSTNAME")
                .ok_or_else(|| anyhow::anyhow!("Missing MS_GRAPH_SHAREPOINT_HOSTNAME"))?
                .clone();

            let site_path = config.get("MS_GRAPH_SHAREPOINT_SITE_PATH")
                .ok_or_else(|| anyhow::anyhow!("Missing MS_GRAPH_SHAREPOINT_SITE_PATH"))?
                .clone();

            let list_name = config.get("MS_GRAPH_SHAREPOINT_LIST_NAME")
                .ok_or_else(|| anyhow::anyhow!("Missing MS_GRAPH_SHAREPOINT_LIST_NAME"))?
                .clone();

            // Step 1: Get site by path
           let site_resp: Value = graph
                .v1()
                .site(hostname)
                .get_by_path(site_path)
                .send()
                .await?
                .json()
                .await?;

            let site_id = site_resp["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing site id"))?
                .to_string();

            // Step 2: List lists
            let lists_resp: Value = graph
                .v1()
                .site(site_id.clone())
                .lists()
                .list_lists()
                .send()
                .await?
                .json()
                .await?;

            let list_id = lists_resp["value"]
                .as_array()
                .and_then(|lists| {
                    lists.iter().find(|list| list["name"] == list_name)?.get("id")?.as_str().map(|s| s.to_string())
                })
                .ok_or_else(|| anyhow::anyhow!("List '{}' not found", list_name))?;

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

pub fn get_client_states(config: &DashMap<String, String>) -> Vec<(String, String)> {
    let hostname = config.get("MS_GRAPH_SHAREPOINT_HOSTNAME").map(|v| v.clone()).unwrap_or_default();
    let site_path = config.get("MS_GRAPH_SHAREPOINT_SITE_PATH").map(|v| v.clone()).unwrap_or_default();
    let list_name = config.get("MS_GRAPH_SHAREPOINT_LIST_NAME").map(|v| v.clone()).unwrap_or_default();

    vec![(
        format!("/sites/{hostname}:{site_path}/lists/{list_name}/items"),
        "sharepoint_state".to_string(),
    )]
}