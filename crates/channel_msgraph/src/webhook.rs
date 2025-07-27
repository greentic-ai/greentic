use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use channel_plugin::message::{ChannelMessage, Event, MessageContent, MessageDirection, Participant};
use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::Receiver;
use crate::{graph_client::GraphState, subscriptions::register_subscription, utils::to_channel_message};
use crate::models::GraphWebhookPayload;

#[derive(Debug)]
pub struct WebhookState {
    pub graph_state: Arc<RwLock<GraphState>>,
    pub sender: Sender<ChannelMessage>,
    pub receiver: Receiver<ChannelMessage>,
    pub expected_client_states: DashMap<String, String>,
}

impl WebhookState {
    pub fn new(graph_state: GraphState, client_states: Vec<(String,String)>) -> Arc<Self> {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let expected_client_states = DashMap::new();
        for (key,value) in client_states {
            expected_client_states.insert(key, value);
        }
        
        let webhook_state = Arc::new(WebhookState {
            graph_state: Arc::new(RwLock::new(graph_state)),
            sender: tx,
            receiver: rx,
            expected_client_states,
        });
        return webhook_state;
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ValidationQuery {
    validation_token: Option<String>,
}

pub async fn handle_notification(
    Query(query): Query<ValidationQuery>,
    headers: HeaderMap,
    State(state): State<Arc<WebhookState>>,
    Json(payload): Json<GraphWebhookPayload>,
) -> Response {
    // ✅ Step 1: Validate Microsoft Graph subscription handshake
    if let Some(token) = query.validation_token {
        return token.into_response();
    }

    // ✅ Step 2: (Optional) inspect headers
    for (name, value) in headers.iter() {
        tracing::debug!("🔎 Header: {}: {:?}", name, value);
    }

    // ✅ Step 3: Handle actual notifications
    for notif in &payload.value {
        if let Some(ref state_check) = notif.client_state {
            let resource = notif.resource.clone();
            if let Some(expected_ref)  = state.expected_client_states.get(&resource) {
                let expected: String = expected_ref.clone();
                if expected != *state_check {
                    tracing::warn!("⚠️ Skipping notification: client_state mismatch for resource {resource}");
                    continue;
                }
            }
                
        }

        if let Some(msg) = to_channel_message(notif) {
            if let Err(e) = state.sender.send(msg).await {
                tracing::error!("❌ Failed to send ChannelMessage: {e:?}");
            }
        } else {
            tracing::warn!("⚠️ Skipping empty MS Graph notification");
        }
    }

    StatusCode::ACCEPTED.into_response()
}


#[axum::debug_handler]
pub async fn poll_incoming(
    State(runtime): State<Arc<ChannelPluginRuntime>>,
    Json(payload): Json<Value>,
) -> StatusCode {
    tracing::info!(payload = ?payload, "received graph webhook");

    let msg = ChannelMessage {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now().to_rfc3339(),
        direction: MessageDirection::Incoming,
        channel: "msgraph".to_string(),
        from: Participant::new("microsoft-graph".to_string(), None, None),
        to: vec![],
        content: vec![MessageContent::Event {
            event: Event{
                event_type: "graph_notification".to_string(),
                event_payload: payload,
            }
        }],
        ..Default::default()
    };

    if let Err(err) = runtime.receive_message(msg).await {
        tracing::error!(?err, "error while handling webhook");
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::OK
    }
}

#[derive(Debug, Deserialize)]
pub struct SubscribeRequest {
    pub resource: String,
    pub client_state: Option<String>,
    pub change_type: Option<String>, // default: "created,updated"
}

pub async fn handle_subscribe(
    State(state): State<Arc<WebhookState>>,
    Json(req): Json<SubscribeRequest>,
) -> impl IntoResponse {
    let graph = state.graph_state.clone();
    let resource = req.resource.clone();
    let change_type = req.change_type.unwrap_or_else(|| "created,updated".to_string());
    let client_state = req
        .client_state
        .unwrap_or_else(|| format!("client_state_{}", uuid::Uuid::new_v4()));

    state
        .expected_client_states
        .insert(resource.clone(), client_state.clone());

    register_subscription(&resource, &client_state, &change_type, &graph).await
        .map(|_| StatusCode::OK)
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}