use async_trait::async_trait;
use dashmap::DashMap;
use async_nats::{Client, ConnectOptions};
use futures_util::StreamExt;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc::UnboundedReceiver, RwLock};
use tracing::{error, info, warn};
use nkeys::KeyPair;
use std::{fs, path::Path};
use crate::{message::{CapabilitiesResult, ChannelCapabilities, ChannelMessage, ChannelState, DrainResult, EventType, HealthResult, InitParams, InitResult, ListKeysResult, MessageContent, MessageInResult, MessageOutParams, MessageOutResult, NameResult, StateResult, StopResult}, plugin_runtime::{HasStore, PluginHandler}};

#[derive(Clone, Debug)]
pub struct PubSubPlugin {
    nats: Option<Arc<Client>>,
    router_id: String,
    greentic_id: String,
    filter: Option<String>,
    state: Arc<RwLock<ChannelState>>,
    cfg: DashMap<String, String>,
    secrets: DashMap<String, String>,
    subscriber_rx: Arc<RwLock<Option<UnboundedReceiver<ChannelMessage>>>>,
    stop_tx: Arc<RwLock<Option<tokio::sync::watch::Sender<()>>>>,
}

impl PubSubPlugin {
    pub fn new(router_id: String, greentic_id: String) -> Self {
        Self {
            nats: None,
            router_id,
            greentic_id,
            filter: None,
            state: Arc::new(RwLock::new(ChannelState::STOPPED)),
            cfg: DashMap::new(),
            secrets: DashMap::new(),
            subscriber_rx: Arc::new(RwLock::new(None)),
            stop_tx: Arc::new(RwLock::new(None)),
        }
    }

    fn subject_in(&self) -> String {
        let filter = match self.filter.clone() {
            Some(filter) => filter,
            None => ">".to_string(), // don't filter, include all
        };
        format!("router.{}.in.{}.{}", 
            self.router_id, 
            self.greentic_id, 
            filter)
    }

    fn subject_out(&self) -> String {
        let filter = match self.filter.clone() {
            Some(filter) => filter,
            None => ">".to_string(), // don't filter, include all
        };
        format!("router.{}.out.{}.{}", 
            self.router_id, 
            self.greentic_id, 
            filter)
    }

    fn subject_adm(&self) -> String {
        let filter = match self.filter.clone() {
            Some(filter) => filter,
            None => ">".to_string(), // don't filter, include all
        };
        format!("router.{}.adm.{}.{}", 
            self.router_id, 
            self.greentic_id, 
            filter)
    }

    async fn start_subscriber(&self) -> anyhow::Result<()> {
        let subject = self.subject_in();
        let mut subscription = self.nats.as_ref().expect("nats was not set").subscribe(subject).await?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *self.subscriber_rx.write().await = Some(rx); // store the receiver
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(());
        *self.stop_tx.write().await = Some(stop_tx);

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = async {
                        match subscription.next().await {
                            Some(message) => { 
                                match serde_json::from_slice::<ChannelMessage>(&message.payload) {
                                    Ok(channel_msg) => {
                                        let _ = tx.send(channel_msg);
                                    }
                                    Err(err) => {
                                        warn!("❌ Failed to parse incoming message: {:#?}", err);
                                    }
                                }
                            },
                            None => {
                                warn!("🔌 NATS subscription ended");
                            }
                        }
                    } => {},
                    _ = stop_rx.changed() => {
                        info!("🛑 NATS subscriber received stop signal");
                        break;
                    }
                }
            }
        });
        *self.state.write().await = ChannelState::STOPPED;

        Ok(())
    }
}

#[async_trait]
impl PluginHandler for PubSubPlugin {
    async fn init(&mut self, _: InitParams) -> InitResult {
        let creds_path: PathBuf = match self.get_secret("GREENTIC_SECRETS_DIR") {
            Some(secret_dir) => {
                let secrets = Path::new(&secret_dir);
                if !secrets.exists() {
                    return InitResult {
                        success: false,
                        error: Some("The directory mentioned in GREENTIC_SECRETS_DIR does not exist.".to_string()),
                    };
                }
                let creds_path = secrets.join("greentic.creds");
                if !creds_path.exists()
                {
                    let nats_jwt = secrets.join("greentic_presigned.jwt");
                    let jwt = match std::fs::read_to_string(&nats_jwt) {
                        Ok(j) => j,
                        Err(e) => {
                            return InitResult {
                                success: false,
                                error: Some(format!("Could not read JWT file: {}, please rerun greentic init.", e)),
                            };
                        }
                    };
                    let keypair = match self.get_secret("GREENTIC_NATS_SEED"){
                        Some(seed) => {
                            match KeyPair::from_seed(&seed) {
                                Ok(keypair) => keypair,
                                Err(_) => {
                                    return InitResult {
                                        success: false,
                                        error: Some("Could not create keypair from seed. Please review GREENTIC_NATS_SEED from secrets. Please add it via greentic init".into()),
                                    };
                                },
                            }
                        },
                        None => {
                            return InitResult {
                                success: false,
                                error: Some("Could not get GREENTIC_NATS_SEED from secrets. Please add it via greentic init".into()),
                            };
                        },
                    };
                    let creds = generate_creds_file(&jwt, &keypair);
                    if creds.is_err(){
                        return InitResult {
                            success: false,
                            error: Some(format!("Could not generate the greentic.creds for PubSub because {}.", creds.unwrap_err().to_string())),
                        };
                    }
                    
                    if std::fs::write(creds_path.clone(), creds.unwrap()).is_err(){
                        return InitResult {
                            success: false,
                            error: Some("Could not write the greentic.nk for PubSub.".to_string()),
                        };
                    }
                }
                creds_path
            }
            None => {
                return InitResult {
                    success: false,
                    error: Some("Missing required secret key: GREENTIC_SECRETS_DIR".to_string()),
                };
            }
        };
        
        let nats_url = self
            .get_config("PUBSUB_NATS_URL")
            .unwrap_or_else(|| "nats.greentic.ai".to_string());
        match connect_with_creds(&nats_url, creds_path).await {
            Ok(conn) => {
                self.nats = Some(Arc::new(conn));
            }
            Err(e) => {
                return InitResult {
                    success: false,
                    error: Some(format!("Failed to connect to NATS: {}", e)),
                };
            }
        }
        match self.start_subscriber().await {
            Ok(_) => {
                *self.state.write().await = ChannelState::RUNNING;
                InitResult {
                    success: true,
                    error: None,
                }
            }
            Err(e) => InitResult {
                success: false,
                error: Some(format!("Failed to start subscriber: {}", e)),
            },
        }
    }

    async fn send_message(&mut self, p: MessageOutParams) -> MessageOutResult {
        let is_admin = p.message.content.iter().any(|c| {
            matches!(
                c,
                MessageContent::Event { event }
                if matches!(
                    event.event_type.as_str(),
                    "subscription_add" | "subscription_update" | "subscription_remove"
                )
            )
        });

        let subject = if is_admin {
            self.subject_adm()
        } else {
            self.subject_out()
        };

        match serde_json::to_vec(&p.message) {
            Ok(payload) => {
                if let Err(e) = self.nats.as_ref().expect("NATS connection not set").publish(subject, payload.into()).await {
                    return MessageOutResult {
                        success: false,
                        error: Some(format!("Failed to publish message: {}", e)),
                    };
                }
                info!("📤 Published message via PubSubPlugin");
                MessageOutResult {
                    success: true,
                    error: None,
                }
            }
            Err(e) => MessageOutResult {
                success: false,
                error: Some(format!("Failed to serialize message: {}", e)),
            },
        }
    }

    async fn receive_message(&mut self) -> MessageInResult {
        let mut lock = self.subscriber_rx.write().await;

        if let Some(rx) = lock.as_mut() {
            match rx.recv().await {
                Some(msg) => MessageInResult {
                    message: msg,
                    error: false,
                },
                None => MessageInResult {
                    message: ChannelMessage::default(),
                    error: true,
                },
            }
        } else {
            error!("Cannot lock subscriber queue");
            MessageInResult {
                message: ChannelMessage::default(),
                error: true,
            }
        }
    }



    async fn stop(&mut self) -> StopResult {
         if let Some(tx) = self.stop_tx.write().await.take() {
            let _ = tx.send(()); // signal cancellation
        }
        StopResult {
            success: true,
            error: None,
        }
    }

    async fn drain(&mut self) -> DrainResult {
        *self.state.write().await = ChannelState::DRAINING;
        if let Some(tx) = self.stop_tx.write().await.take() {
            let _ = tx.send(()); // signal cancellation
        }
        DrainResult {
            success: true,
            error: None,
        }
    }

    async fn health(&self) -> HealthResult {
        HealthResult {
            healthy: true,
            reason: None,
        }
    }

    async fn state(&self) -> StateResult {
        StateResult {
            state: self.state.read().await.clone(),
        }
    }

    fn name(&self) -> NameResult {
        NameResult {
            name: "pubsub".into(),
        }
    }

    fn list_config_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![],
            optional_keys: vec![
                ("PUBSUB_NATS_URL".into(),Some("The URL were NATS is listening.".into()),),
            ],
            dynamic_keys: vec![],
        }
    }

    fn list_secret_keys(&self) -> ListKeysResult {
        ListKeysResult {
            required_keys: vec![
                ("GREENTIC_NATS_SEED".into(), Some("The seed for the NATS secret.".into())),
                ("GREENTIC_SECRETS_DIR".into(), Some("The directory to read the pre-signed JWT and store the keys.".into())),
            ],
            optional_keys: vec![],
            dynamic_keys: vec![],
        }
    }

    fn capabilities(&self) -> CapabilitiesResult {
        let subscription_schema = json!({
            "type": "object",
            "properties": {
                "tenant_id": { "type": "string" },
                "entity_id": { "type": "string" },
                "params": {}
            },
            "required": ["entity_id"],
            "additionalProperties": false
        });
        CapabilitiesResult {
            capabilities: ChannelCapabilities {
                name: "pubsub".into(),
                supports_sending: true,
                supports_receiving: true,
                supports_text: true,
                supports_routing: true,
                supported_events: vec![
                    EventType::new(
                        "subscription_add".to_string(),
                        "Request to add a new subscription".to_string(),
                        Some(subscription_schema.clone()),
                    ),
                    EventType::new(
                        "subscription_update".to_string(),
                        "Request to update an existing subscription".to_string(),
                        Some(subscription_schema.clone()),
                    ),
                    EventType::new(
                        "subscription_remove".to_string(),
                        "Request to remove an existing subscription".to_string(),
                        Some(subscription_schema),
                    ),
                ],
                ..Default::default()
            },
        }
    }
}
/// connect with jwt to NATS
async fn connect_with_creds(nats_url: &str, creds_path: PathBuf) -> anyhow::Result<async_nats::Client> {
    let creds_data = fs::read_to_string(creds_path)?;
    let opts = ConnectOptions::new()
        .credentials(&creds_data)
        .expect("Could not read credentials");

    let client = opts.connect(nats_url).await?;
    Ok(client)
}

/// Package JWT and seed into a .creds-compatible string
fn generate_creds_file(jwt: &str, kp: &KeyPair) -> anyhow::Result<String> {
    let seed = kp.seed()?;
    Ok(format!(
        "-----BEGIN NATS USER JWT-----\n{}\n------END NATS USER JWT------\n\n************************* IMPORTANT *************************\nNKEY Seed printed below can be used to sign and prove identity.\nNKEYs are sensitive and should be treated as secrets.\n\n-----BEGIN USER NKEY SEED-----\n{}\n------END USER NKEY SEED------\n",
        jwt.trim(),
        seed.trim(),
    ))
}

impl HasStore for PubSubPlugin {
    fn config_store(&self) -> &DashMap<String, String> {
        &self.cfg
    }

    fn secret_store(&self) -> &DashMap<String, String> {
        &self.secrets
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;
    use duct::{cmd, Handle};
    use std::thread;

    use crate::{plugin_runtime::PluginHandler, pubsub_client::PubSubPlugin};

    fn start_local_nats() -> Handle {
        let child = cmd!("nats-server", "-p", "4222")
            .stderr_to_stdout()
            .stdout_capture()
            .start()
            .expect("Failed to start NATS server");
        
        // Give the server time to start
        thread::sleep(Duration::from_secs(1));
        child
    }

    #[tokio::test]
    async fn test_plugin_initializes() {
        let nats_process = start_local_nats();

        // Set required config
        let mut plugin = PubSubPlugin::new("testrouter".into(),"testgreentic".into(),);
        plugin.cfg.insert("PUBSUB_NATS_URL".into(), "localhost:4222".into());
        plugin.secrets.insert("GREENTIC_SECRETS_DIR".into(),"./test".into());
        plugin.secrets.insert("GREENTIC_NATS_JWT".into(),"jwt".into());

        let result = plugin.init(Default::default()).await;
        assert!(result.success, "Init should succeed: {:?}", result.error);

        // Clean up NATS process
        let _ = nats_process.kill();
    }
}
