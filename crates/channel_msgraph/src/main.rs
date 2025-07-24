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
use channel_plugin::message::{CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, DrainResult, InitParams, InitResult, ListKeysResult, MessageInResult, MessageOutParams, MessageOutResult, NameResult, StateResult, StopResult};
use channel_plugin::plugin_runtime::{run, HasStore, PluginHandler};
use dashmap::DashMap;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::Mutex;

use handlers::send_message;
use webhook::poll_incoming;

use crate::handlers::get_client_states;
use crate::webhook::{handle_notification, handle_subscribe, WebhookState};

#[derive(Clone)]
struct MsGraphPlugin {
    state: Arc<Mutex<ChannelState>>,
    config: Arc<DashMap<String, String>>,
    secrets: Arc<DashMap<String, String>>,
    sender: Sender<ChannelMessage>,
    receiver: Arc<Mutex<Receiver<ChannelMessage>>>,
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
        match send_message(params, self).await {
            Ok(res) => MessageOutResult {
                success: true,
                error: None,
            },
            Err(err) => MessageOutResult {
                success: false,
                error: Some(err.to_string()),
            },
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
                ("MS_GRAPH_DOMAIN".to_string(), Some("public base domain should be set via MS_GRAPH_DOMAIN".to_string())),
                ("MS_GRAPH_TENANT_ID".to_string(), Some("Azure tenant ID should be set via MS_GRAPH_TENANT_ID".to_string())),
                ("MS_GRAPH_CLIENT_ID".to_string(), Some("Azure app client ID should be set via MS_GRAPH_CLIENT_ID".to_string())),
            ],
            optional_keys: vec![],
        }
    }

    fn list_secret_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![("MS_GRAPH_CLIENT_SECRET".to_string(), Some("Azure app client secret should be set via MS_GRAPH_CLIENT_SECRET".to_string()))],
            optional_keys: vec![],
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
                ..Default::default()
            }
        }
    }
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
