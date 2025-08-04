use std::sync::Arc;

use channel_plugin::{message::{ChannelMessage, MessageOutParams}, plugin_runtime::HasStore};
use dashmap::DashMap;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::{config::MsGraphConfig, graph_client::GraphState};

pub mod calendar;
pub mod email;
pub mod onedrive;
pub mod teams;
pub mod sharepoint;

pub static HANDLERS: Lazy<DashMap<&'static str, Arc<dyn SubscriptionHandler>>> = Lazy::new(|| {
    let map: DashMap<&'static str, Arc<dyn SubscriptionHandler>> = DashMap::new();
    map.insert("calendar-user", Arc::new(calendar::CalendarHandler));
    map.insert("email-user", Arc::new(email::EmailHandler));
    map.insert("teams-channel", Arc::new(teams::TeamsHandler));
    map.insert("onedrive-user", Arc::new(onedrive::OneDriveHandler));
    map.insert("sharepoint-list", Arc::new(sharepoint::SharePointHandler));
    map
});


pub async fn init_all(config: Arc<DashMap<String, String>>, graph_state: Arc<RwLock<GraphState>>) {
    for handler in HANDLERS.iter() {
        // This assumes you pass in config that's filtered per handler, or all keys are namespaced
        if let Err(err) = handler.add(config.clone(), graph_state.clone()).await {
            tracing::error!("Error initializing {}: {:?}", handler.spec().subscription_type, err);
        }
    }
}

pub fn get_client_states(config: &DashMap<String, String>) -> Vec<(String, String)> {
    let mut all = vec![];
    all.extend(calendar::get_client_states(config));
    all.extend(email::get_client_states(config));
    all.extend(teams::get_client_states(config));
    all.extend(onedrive::get_client_states(config));
    all.extend(sharepoint::get_client_states(config));
    all
}

#[derive(Clone, Debug)]
pub struct SubscriptionField {
    pub key: &'static str,
    pub required: bool,
    pub description: &'static str,
}
#[derive(Clone, Debug)]
pub struct SubscriptionSpec {
    pub subscription_type: &'static str,
    pub description: &'static str,
    pub fields: Vec<SubscriptionField>,
    pub key_template: &'static str, // e.g., "msgraph:{tenant_id}:{domain}:calendar:user:{calendar_user_id}"
}

pub fn generate_json_schema(spec: &SubscriptionSpec) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = vec![];

    for field in &spec.fields {
        properties.insert(
            field.key.to_string(),
            json!({
                "type": "string",
                "description": field.description,
            }),
        );
        if field.required {
            required.push(field.key.to_string());
        }
    }

    json!({
        "type": "object",
        "required": required,
        "properties": properties,
    })
}

pub fn interpolate_key(template: &str, payload: &Value) -> Option<String> {
    let mut key = template.to_string();
    for caps in template.match_indices('{') {
        let end = key[caps.0..].find('}').map(|i| i + caps.0)?;
        let var = &key[caps.0 + 1..end];
        let val = payload.get(var)?.as_str()?;
        key = key.replacen(&format!("{{{}}}", var), val, 1);
    }
    Some(key)
}

#[async_trait::async_trait]
pub trait SubscriptionHandler: Send + Sync {
    fn spec(&self) -> SubscriptionSpec;

    async fn add(&self, config: Arc<DashMap<String, String>>, graph_state: Arc<RwLock<GraphState>>) -> anyhow::Result<()>;

    async fn update(&self, config: Arc<DashMap<String, String>>, graph_state: Arc<RwLock<GraphState>>) -> anyhow::Result<()> {
        self.delete(config.clone(), graph_state.clone()).await?;
        self.add(config, graph_state).await
    }

    async fn delete(&self, config: Arc<DashMap<String, String>>, graph_state: Arc<RwLock<GraphState>>) -> anyhow::Result<()>;

    // Optional: implement only if outbound messaging is needed
    async fn send_message(
        &self,
        _msg: &ChannelMessage,
        _config: Arc<DashMap<String, String>>,
        _graph_state: Arc<RwLock<GraphState>>,
    ) -> Option<anyhow::Result<()>> {
        None
    }
}

pub async fn send_message(
    params: MessageOutParams,
    plugin: &mut dyn HasStore,
) -> Result<(), anyhow::Error> {
    let msg = params.message.clone();
    let payload = msg.get_event_payload()?;
    let event_type = msg.event_type()?;

    let tenant_id = payload.get("tenant_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing tenant_id"))?;

    let domain = payload.get("domain")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing domain"))?;

    let msgraph_config = MsGraphConfig::from_maps(tenant_id, domain, plugin.config_store(), plugin.secret_store())?;
    let graph_state = Arc::new(RwLock::new(GraphState::new(&msgraph_config).await?));

    let config = Arc::new(DashMap::new());
    for (k, v) in payload.iter() {
        config.insert(k.clone(), v.as_str().unwrap_or_default().to_string());
    }

    if let Some(handler) = HANDLERS.get(event_type) {
        match handler.send_message(&msg, config, graph_state).await {
            Some(Ok(())) => Ok(()),
            Some(Err(e)) => Err(e),
            None => Err(anyhow::anyhow!("Handler does not support sending messages")),
        }
    } else {
        Err(anyhow::anyhow!("No handler for event type {}", event_type))
    }
}
