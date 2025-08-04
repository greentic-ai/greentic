// channel_msgraph/src/main.rs

mod config;
mod certs;
mod subscriptions;
mod webhook;
mod handlers;
mod graph_client;
mod models;
mod utils;

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::routing::post;
use axum::Router;
use channel_plugin::message::{CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, DrainResult, EventType, InitParams, InitResult, ListKeysResult, MessageInResult, MessageOutParams, MessageOutResult, NameResult, StateResult, StopResult};
use channel_plugin::plugin_runtime::{run, HasStore, PluginHandler};
use dashmap::DashMap;
use serde_json::json;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Mutex, RwLock};

use handlers::send_message;
use webhook::poll_incoming;

use crate::config::MsGraphConfig;
use crate::graph_client::GraphState;
use crate::handlers::{get_client_states, HANDLERS};
use crate::utils::extract_event_type;
use crate::webhook::{handle_notification, handle_subscribe, WebhookState};

#[derive(Clone)]
struct MsGraphPlugin {
    state: Arc<Mutex<ChannelState>>,
    config: Arc<DashMap<String, String>>,
    secrets: Arc<DashMap<String, String>>,
    sender: Sender<ChannelMessage>,
    receiver: Arc<Mutex<Receiver<ChannelMessage>>>,
}

impl MsGraphPlugin {
    pub async fn add_subscription(&self, msg: &ChannelMessage) -> anyhow::Result<()> {
        let payload = msg.get_event_payload()?; // You should implement this helper

        let tenant_id = payload.get("tenant_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Missing tenant_id"))?;
        let domain = payload.get("domain").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Missing domain"))?;

        let msgraph_config = MsGraphConfig::from_maps(tenant_id, domain, &self.config, &self.secrets)?;
        let graph_state = Arc::new(RwLock::new(GraphState::new(&msgraph_config).await?));

        let config = Arc::new(DashMap::new());
        for (k, v) in payload.iter() {
            config.insert(k.clone(), v.as_str().unwrap_or_default().to_string());
        }

        for handler in HANDLERS.values() {
            if handler.spec().subscription_type == msg.event_type()? {
                return handler.add(config.clone(), graph_state.clone()).await;
            }
        }

        Err(anyhow::anyhow!("No handler found for event_type: {:?}", msg.event_type()))
    }

    pub async fn update_subscription(&self, msg: &ChannelMessage) -> anyhow::Result<()> {
        let payload = msg.get_event_payload()?;

        let tenant_id = payload.get("tenant_id").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing tenant_id"))?;
        let domain = payload.get("domain").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing domain"))?;

        let msgraph_config = MsGraphConfig::from_maps(tenant_id, domain, &self.config, &self.secrets)?;
        let graph_state = Arc::new(RwLock::new(GraphState::new(&msgraph_config).await?));

        let config = Arc::new(DashMap::new());
        for (k, v) in payload {
            config.insert(k.clone(), v.as_str().unwrap_or_default().to_string());
        }

        for handler in HANDLERS.values() {
            if handler.spec().subscription_type == msg.event_type()? {
                // Try delete, then add again
                handler.delete(config.clone(), graph_state.clone()).await.ok();
                return handler.add(config.clone(), graph_state.clone()).await;
            }
        }

        Err(anyhow::anyhow!("No handler found for event_type: {:?}", msg.event_type()))
    }

    pub async fn delete_subscription(&self, msg: &ChannelMessage) -> anyhow::Result<()> {
        let payload = msg.get_event_payload()?;

        let tenant_id = payload.get("tenant_id").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing tenant_id"))?;
        let domain = payload.get("domain").and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing domain"))?;

        let msgraph_config = MsGraphConfig::from_maps(tenant_id, domain, &self.config, &self.secrets)?;
        let graph_state = Arc::new(RwLock::new(GraphState::new(&msgraph_config).await?));

        let config = Arc::new(DashMap::new());
        for (k, v) in payload {
            config.insert(k.clone(), v.as_str().unwrap_or_default().to_string());
        }

        for handler in HANDLERS.values() {
            if handler.spec().subscription_type == msg.event_type()? {
                return handler.delete(config.clone(), graph_state.clone()).await;
            }
        }

        Err(anyhow::anyhow!("No handler found for event_type: {:?}", msg.event_type()))
    }
}

impl HasStore for MsGraphPlugin {
    fn config_store(&self) -> &DashMap<String, String> {
        &self.config
    }

    fn secret_store(&self) -> &DashMap<String, String> {
        &self.secrets
    }
}

#[async_trait]
impl PluginHandler for MsGraphPlugin {
    async fn init(&mut self, _params: InitParams) -> InitResult {
        *self.state.lock().await = ChannelState::RUNNING;
        InitResult {
            success: true,
            error: None,
        }
    }

    async fn drain(&mut self) -> DrainResult {
        *self.state.lock().await = ChannelState::STOPPED;
        DrainResult { success: true, error: None }
    }

    async fn stop(&mut self) -> StopResult {
        *self.state.lock().await = ChannelState::STOPPED;
        StopResult { success: true, error: None }
    }

    async fn send_message(&mut self, params: MessageOutParams) -> MessageOutResult {
        match extract_event_type(&params.message) {
            Some("AddSubscription") => {
                match self.add_subscription(&params.message).await {
                    Ok(_) => MessageOutResult {
                        success: true,
                        error: None,
                    },
                    Err(err) => MessageOutResult {
                        success: false,
                        error: Some(format!("Failed to add subscription: {}", err)),
                    },
                }
            }

            Some("UpdateSubscription") => {
                match self.update_subscription(&params.message).await {
                    Ok(_) => MessageOutResult {
                        success: true,
                        error: None,
                    },
                    Err(err) => MessageOutResult {
                        success: false,
                        error: Some(format!("Failed to update subscription: {}", err)),
                    },
                }
            }

            Some("DeleteSubscription") => {
                match self.delete_subscription(&params.message).await {
                    Ok(_) => MessageOutResult {
                        success: true,
                        error: None,
                    },
                    Err(err) => MessageOutResult {
                        success: false,
                        error: Some(format!("Failed to delete subscription: {}", err)),
                    },
                }
            }

            _ => {
                // Default behavior: forward the message
                match send_message(params, self).await {
                    Ok(_) => MessageOutResult {
                        success: true,
                        error: None,
                    },
                    Err(err) => MessageOutResult {
                        success: false,
                        error: Some(err.to_string()),
                    },
                }
            }
        }
    }


    async fn receive_message(&mut self) -> MessageInResult {
        poll_incoming(&self.receiver).await
    }

    async fn state(&self) -> StateResult {
        StateResult { state: self.state.lock().await.clone() }
    }

    fn name(&self) -> NameResult {
        NameResult {
            name: "channel_msgraph".to_string(),
        }
    }

    fn list_config_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![
                ("MS_GRAPH_CLIENT_ID".to_string(), Some("Azure app client ID should be set via MS_GRAPH_CLIENT_ID".to_string())),
            ],
            optional_keys: vec![],
            dynamic_keys: vec![
                ("msgraph:{tenant_id}:{domain}:email:{email_user_id}".to_string(), Some("Define which email accounts need to be tracked".to_string())),
                ("msgraph:{tenant_id}:{domain}:sharepoint:host:{sharepoint_hostname}".to_string(), Some("The SharePoint hostname like contoso.sharepoint.com to use".to_string())),
                ("msgraph:{tenant_id}:{domain}:sharepoint:site:{sharepoint_site_path}".to_string(), Some("The SharePoint path like /sites/engineering to use".to_string())),
                ("msgraph:{tenant_id}:{domain}:sharepoint:list:{sharepoint_list_name}".to_string(), Some("The SharePoint name of the list like 'Tasks' to use".to_string())),
                ("msgraph:{tenant_id}:{domain}:teams:team:{teams_team_name}".to_string(), Some("The Teams team name to use".to_string())),
                ("msgraph:{tenant_id}:{domain}:teams:channel:{teams_channel_name}".to_string(), Some("The Teams channel name to use".to_string())),
                ("msgraph:{tenant_id}:{domain}:onedrive:user:{onedrive_user_id}".to_string(), Some("The specific user for onedrive instead of 'me' to use".to_string())),
                ("msgraph:{tenant_id}:{domain}:calendar:user:{calendar_user_id}".to_string(), Some("The specific user for calendar instead of 'me' to use".to_string())),
            ],
        }
    }

    fn list_secret_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![("MS_GRAPH_CLIENT_SECRET".to_string(), Some("Azure app client secret should be set via MS_GRAPH_CLIENT_SECRET".to_string()))],
            optional_keys: vec![],
            dynamic_keys: vec![("msgraph:{tenant_id}:{user_id}:refresh_token".to_string(), Some("The refresh token for a specific user of a specific tenant".to_string())),],
        }
    }

    fn capabilities(&self) -> CapabilitiesResult {
        CapabilitiesResult {
            capabilities: ChannelCapabilities{
                name: "channel_msgraph".to_string(),
                supports_sending: true,
                supports_receiving: true,
                supports_text: true,
                supports_media: true,
                supported_events: event_types(),
                ..Default::default()
            }
        }
    }
}


fn event_types() -> Vec<EventType> {
    vec![
        EventType {
            event_type: "EmailNotification".to_string(),
            description: "Receive emails for a specific user in a tenant/domain.".to_string(),
            payload_schema: Some(json!({
                "type": "object",
                "required": ["tenant_id", "domain", "email_user_id"],
                "properties": {
                    "tenant_id": { "type": "string" },
                    "domain": { "type": "string" },
                    "email_user_id": { "type": "string" }
                }
            })),
        },
        EventType {
            event_type: "SharePointNotification".to_string(),
            description: "Track SharePoint list changes for a site.".to_string(),
            payload_schema: Some(json!({
                "type": "object",
                "required": ["tenant_id", "domain", "sharepoint_hostname", "sharepoint_site_path", "sharepoint_list_name"],
                "properties": {
                    "tenant_id": { "type": "string" },
                    "domain": { "type": "string" },
                    "sharepoint_hostname": { "type": "string" },
                    "sharepoint_site_path": { "type": "string" },
                    "sharepoint_list_name": { "type": "string" }
                }
            })),
        },
        EventType {
            event_type: "TeamsNotification".to_string(),
            description: "Receive messages from a Teams channel.".to_string(),
            payload_schema: Some(json!({
                "type": "object",
                "required": ["tenant_id", "domain", "teams_team_name", "teams_channel_name"],
                "properties": {
                    "tenant_id": { "type": "string" },
                    "domain": { "type": "string" },
                    "teams_team_name": { "type": "string" },
                    "teams_channel_name": { "type": "string" }
                }
            })),
        },
        EventType {
            event_type: "OneDriveUserNotification".to_string(),
            description: "Track OneDrive changes for a user.".to_string(),
            payload_schema: Some(json!({
                "type": "object",
                "required": ["tenant_id", "domain", "onedrive_user_id"],
                "properties": {
                    "tenant_id": { "type": "string" },
                    "domain": { "type": "string" },
                    "onedrive_user_id": { "type": "string" }
                }
            })),
        },
        EventType {
            event_type: "CalendarUserNotification".to_string(),
            description: "Monitor calendar events for a user.".to_string(),
            payload_schema: Some(json!({
                "type": "object",
                "required": ["tenant_id", "domain", "calendar_user_id"],
                "properties": {
                    "tenant_id": { "type": "string" },
                    "domain": { "type": "string" },
                    "calendar_user_id": { "type": "string" }
                }
            })),
        },
    ]
}


#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    // plugin state
    let state = Arc::new(Mutex::new(ChannelState::STOPPED));
    let config = Arc::new(DashMap::new());
    let secrets = Arc::new(DashMap::new());

    // channel between webhook and plugin
    let (tx, rx) = tokio::sync::mpsc::channel(100);

    // plugin
    let plugin = MsGraphPlugin {
        state: state.clone(),
        config: config.clone(),
        secrets: secrets.clone(),
        sender: tx.clone(),
        receiver: Arc::new(Mutex::new(rx)),
    };

    // Webhook state shared with HTTP handler
    let client_states = get_client_states();
    let webhook_state = WebhookState::new(graph_client::GraphState::new(&config::MsGraphConfig::from_env()?).await?, client_states);
    webhook_state.sender = tx;

    // Axum HTTP server
    let app = Router::new()
        .route("/graph/subscribe", post(handle_subscribe))
        .route("/notification", post(handle_notification))
        .with_state(webhook_state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    let server = axum::Server::bind(&addr)
        .serve(app.into_make_service());

    // Run both plugin and webhook server concurrently
    tokio::select! {
        res = run(plugin) => res?,
        res = server => res?,
    }

    Ok(())
}
