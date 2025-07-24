use axum::{extract::{Path, Query}, http::StatusCode, routing::post, Extension, Router};
use dashmap::DashMap;
use reqwest::get;
use tracing::info;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tower_http::trace::TraceLayer;
use tokio::sync::{mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender}, Mutex, RwLock};
use channel_plugin::{message::{CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, DrainResult, HealthResult, InitParams, InitResult, ListKeysResult, MessageContent, MessageInResult, MessageOutParams, MessageOutResult, NameResult, StateResult, StopResult}, plugin_runtime::{run, HasStore, PluginHandler}};
mod oauth;
mod auth;
mod certs;
use crate::{certs::maybe_run_tls_server, oauth::{oauth_callback, start_oauth, OAuthCallback}};
use axum_server::Server;

#[derive(Clone)]
struct AppState {
    routes: Arc<RwLock<HashMap<String, RouteConfig>>>,
    inbound_tx: UnboundedSender<ChannelMessage>,
    plugin: WebhookPlugin,
}

#[derive(Clone)]
pub struct RouteConfig {
    pub target_node: String,
    pub auth: Option<AuthConfig>,
    pub oauth_redirect: bool,
}

#[derive(Clone)]
pub enum AuthConfig {
    Github { secret: String },
    Stripe { secret: String },
    Google { jwks_url: String },
    Microsoft { jwks_url: String },
    Slack { signing_secret: String },
    Zoom { verification_token: String },
    Twilio { auth_token: String },
    CustomHmac { secret: String, header: String },
    JwtWithJwks { jwks_url: String, expected_issuer: Option<String>, expected_audience: Option<String> },
    Auto, 
}


#[derive(Default, Clone)]
pub struct WebhookPlugin {
    cfg: DashMap<String, String>,
    secrets: DashMap<String, String>,
    state: Arc<Mutex<ChannelState>>,
    inbound_tx: Option<UnboundedSender<ChannelMessage>>,
    inbound_rx: Option<Arc<Mutex<UnboundedReceiver<ChannelMessage>>>>,
}

impl HasStore for WebhookPlugin {
    fn config_store(&self) -> &DashMap<String, String> {
        &self.cfg
    }
    fn secret_store(&self) -> &DashMap<String, String> {
        &self.secrets
    }
}

#[async_trait::async_trait]
impl PluginHandler for WebhookPlugin {
    async fn init(&mut self, _p: InitParams) -> InitResult {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (inbound_tx, inbound_rx) = unbounded_channel::<ChannelMessage>();
        self.inbound_tx = Some(inbound_tx.clone());
        self.inbound_rx = Some(Arc::new(Mutex::new(inbound_rx)));
        *self.state.lock().await = ChannelState::RUNNING;

        let routes = Arc::new(RwLock::new(HashMap::new()));
        let plugin = self.clone();

        let app_state = AppState {
            routes: routes.clone(),
            inbound_tx: inbound_tx.clone(),
            plugin,
        };

        let addr = match self.get_config("WEBHOOK_PORT") {
            Some(port_str) => {
                match port_str.parse::<u16>() {
                    Ok(port_num) => SocketAddr::from(([0, 0, 0, 0], port_num)),
                    Err(e) => {
                        return InitResult {
                            success: false,
                            error: Some(format!("Invalid port '{:?}': {}", port_str, e)),
                        };
                    }
                }
            }
            None => {
                return InitResult {
                    success: false,
                    error: Some("Set config 'WEBHOOK_PORT' before using the webhook channel.".to_string()),
                };
            }
        };

        let cert_opt = self.get_secret("TLS_CERT");
        let key_opt = self.get_secret("TLS_KEY");

        tokio::spawn(async move {
            let app = Router::new()
                .route("/oauth/:provider", get(start_oauth.into_service()))
                .route("/oauth/:provider/callback", get(oauth_callback.into_service()))
                .route("/:route", post(handle_webhook))
                .layer(Extension(app_state))
                .layer(TraceLayer::new_for_http());

            let result = async {
                if maybe_run_tls_server(app.clone(), addr, cert_opt, key_opt).await? {
                    info!("Started HTTPS server on https://{}", addr);
                    Ok(())
                } else {
                    info!("Started HTTP server on http://{}", addr);
                    Server::bind(addr)
                        .serve(app.into_make_service())
                        .await
                        .map_err(anyhow::Error::from)
                }
            }.await;
            let _ = tx.send(result);
        });

        match rx.await {
            Ok(Ok(())) => InitResult { success: true, error: None },
            Ok(Err(e)) => InitResult { success: false, error: Some(format!("Server error: {e}")) },
            Err(_) => InitResult { success: false, error: Some("Failed to receive server result".into()) },
        }
    }

    async fn send_message(&mut self, _p: MessageOutParams) -> MessageOutResult {
        MessageOutResult { success: false, error: Some("webhook does not support sending".to_string()) }
    }

    async fn receive_message(&mut self) -> MessageInResult {
        if let Some(rx) = &self.inbound_rx {
            let mut lock = rx.lock().await;
            match lock.recv().await {
                Some(msg) => MessageInResult { message: msg, error: false },
                None => MessageInResult { message: ChannelMessage::default(), error: true },
            }
        } else {
            MessageInResult { message: ChannelMessage::default(), error: true }
        }
    }

    async fn drain(&mut self) -> DrainResult {
        *self.state.lock().await = ChannelState::DRAINING;
        DrainResult { success: true, error: None }
    }

    async fn stop(&mut self) -> StopResult {
        *self.state.lock().await = ChannelState::STOPPED;
        StopResult { success: true, error: None }
    }

    async fn health(&self) -> HealthResult {
        HealthResult { healthy: true, reason: None }
    }

    async fn state(&self) -> StateResult {
        StateResult { state: self.state.lock().await.clone() }
    }

    fn name(&self) -> NameResult {
        NameResult { name: "webhook".into() }
    }

    fn list_config_keys(&self) -> ListKeysResult {
        ListKeysResult { 
            required_keys: vec![], 
            optional_keys: vec![
                ("TLS_CERT".into(),Some("The TLS certificate to run the https server.".to_string())),
                ("TLS_KEY".into(),Some("The TLS key to run the https server.".to_string())),
        ] }
    }

    fn list_secret_keys(&self) -> ListKeysResult {
        ListKeysResult { required_keys: vec![
            ("WEBHOOK_PORT".into(),Some("The port to listen to for the webhook server.".to_string())),
        ], optional_keys: vec![] }
    }

    fn capabilities(&self) -> CapabilitiesResult {
        CapabilitiesResult {
            capabilities: ChannelCapabilities {
                name: "webhook".into(),
                supports_sending: false,
                supports_receiving: true,
                supports_text: true,
                supports_routing: true,
                ..Default::default()
            },
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(WebhookPlugin::default()).await
}


async fn handle_webhook(
    Path(route): Path<String>,
    Extension(state): Extension<AppState>,
    Query(query): Query<OAuthCallback>,
    body: String,
) -> Result<StatusCode, StatusCode> {
    let routes = state.routes.read().await;
    if let Some(config) = routes.get(&route) {
        if let Some(auth) = &config.auth {
            if !auth::verify(auth, &Default::default(), &body).await {
                return Err(StatusCode::UNAUTHORIZED);
            }
        }

        if config.oauth_redirect {
           let _ = oauth_callback(Query(query), route.clone(), state.plugin.clone(), &route).await;
           return Ok(StatusCode::OK);
        }

        let msg = ChannelMessage {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: Some(route.clone()),
            content: vec![MessageContent::Text { text: body }],
            ..Default::default()
        };
        let _ = state.inbound_tx.send(msg);
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::{Request, StatusCode}, Router};
    use tower::ServiceExt; // for `app.oneshot()`
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::RwLock;
    use super::*;

    #[tokio::test]
    async fn test_webhook_route_not_found() {
        let (inbound_tx, _inbound_rx) = unbounded_channel::<ChannelMessage>();
        let plugin = WebhookPlugin::default();
        let app = Router::new()
            .route("/:route", axum::routing::post(handle_webhook))
            .layer(axum::Extension(AppState {
                routes: Arc::new(RwLock::new(HashMap::new())),
                inbound_tx,
                plugin,
            }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/unknown")
                    .method("POST")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_webhook_route_with_auth_fail() {
        let (inbound_tx, _inbound_rx) = unbounded_channel::<ChannelMessage>();

        let mut routes = HashMap::new();
        routes.insert(
            "secured".to_string(),
            RouteConfig {
                target_node: "test_node".to_string(),
                auth: Some(AuthConfig::CustomHmac {
                    secret: "wrong_secret".to_string(),
                    header: "X-Custom-HMAC".to_string(),
                }),
                oauth_redirect: false,
            },
        );
        let plugin = WebhookPlugin::default();
        let app = Router::new()
            .route("/:route", axum::routing::post(handle_webhook))
            .layer(axum::Extension(AppState {
                routes: Arc::new(RwLock::new(routes)),
                inbound_tx,
                plugin,
            }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/secured")
                    .method("POST")
                    .body(Body::from("payload"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_webhook_route_oauth_redirect() {
        let (inbound_tx, _inbound_rx) = unbounded_channel::<ChannelMessage>();

        let mut routes = HashMap::new();
        routes.insert(
            "oauth".to_string(),
            RouteConfig {
                target_node: "noop".to_string(),
                auth: None,
                oauth_redirect: true,
            },
        );
        let plugin = WebhookPlugin::default();
        let app = Router::new()
            .route("/:route", axum::routing::post(handle_webhook))
            .layer(axum::Extension(AppState {
                routes: Arc::new(RwLock::new(routes)),
                inbound_tx,
                plugin,
            }));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/oauth")
                    .method("POST")
                    .body(Body::from("code=abc&state=xyz"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

}