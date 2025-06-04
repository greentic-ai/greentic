// channel_node.rs

use crate::flow::manager::{Flow, FlowManager, NodeKind};
use crate::message::Message;
use crate::node::{ChannelOrigin, NodeContext, NodeErr, NodeError, NodeOut, NodeType};
use async_trait::async_trait;
use channel_plugin::message::{ChannelMessage, MessageContent, MessageDirection};
use channel_plugin::plugin::ChannelPlugin;
use dashmap::DashMap;
use schemars::{schema_for, JsonSchema};
use schemars::schema::RootSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{error, warn};
use super::flow_router::{ChannelFlowRouter, ScriptFlowRouter};
use super::manager::{ChannelManager, IncomingHandler};

/// A channel‐node in your flow graph can either inject messages *into* a flow
/// (via the `process` method) or be registered in the registry to receive
/// incoming messages *from* the real channel plugin.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "channel")]
pub struct ChannelNode {
    /// which plugin channel to send to (e.g. "telegram")
    pub channel_name: String,
    /// logical flow name (for bookkeeping, not used by process itself)
    pub flow_name:    String,
    /// the node inside the flow who should be notified
    pub node_id:      String,
    /// poll for new messages
    pub poll_messages: bool,
    /// allow sending messages
    pub send_messages: bool,
    /// how to route incoming messages into flows
    #[serde(rename = "router")]
    pub router_config: FlowRouterConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename = "router")]
pub enum FlowRouterConfig {
    #[serde(rename = "channel")]
    Channel(ChannelFlowRouter),
    #[serde(rename = "script")]
    Script(ScriptFlowRouter),
}


impl ChannelNode {
    /// When an actual plugin yields an incoming ChannelMessage,
    /// this helper will route it into one or more flows.
    pub async fn handle_message(&self, msg: &ChannelMessage, fm: &Arc<FlowManager>) {
        let input = Message::new_uuid(&msg.channel, serde_json::to_value(msg).unwrap());
        let channel_origin = ChannelOrigin::new(msg.channel.clone(), msg.from.clone());
        if let Some(report) = fm
            .process_message(&self.flow_name, &self.node_id, input, Some(channel_origin))
            .await
        {
            let payload_json = serde_json::to_string(&report)
                .expect("cannot serialize report");

            tracing::event!(
                target: "request",                    // picked up by your JSON‐file “reports” layer
                tracing::Level::INFO,
                flow = %self.flow_name,
                node = %self.node_id,
                report = %payload_json,
                "flow run completed"
            );
            
        }
        
    }
}

/// A registry of *incoming* channel nodes.  When a plugin polls you get
/// a ChannelMessage→ call `handle_incoming`, and it fans out into all
/// matching `ChannelNode`s (by channel name) and pushes into the flows.
#[derive(Clone)]
pub struct ChannelsRegistry {
    map:          Arc<DashMap<String, Vec<ChannelNode>>>,
    flow_manager: Arc<FlowManager>,
}

impl ChannelsRegistry {
    pub async fn new(flow_manager: Arc<FlowManager>, _channel_manager: Arc<ChannelManager>) -> Arc<Self> {
        let me = Arc::new(Self {
            map: Arc::new(DashMap::new()),
            flow_manager: flow_manager.clone(),
        });

        // subscribe to *all* new flows and auto‐register their Channel nodes
        let registry = me.clone();
        flow_manager
            .subscribe_flow_added(Arc::new(move |flow_id: &str, flow: &Flow| {
                // find all nodes of kind Channel in this flow
                for (node_name, cfg) in flow.nodes().iter() {
                    if let NodeKind::Channel { cfg} = &cfg.kind {
                        //ensure_channel_running(channel_manager.clone(), &cfg.channel_name).expect("Could not find the channel or it was not started, i.e. is it in plugins/channels/running?");
                        registry.register(ChannelNode {
                            channel_name: cfg.channel_name.clone(),
                            flow_name:    flow_id.to_string(),
                            node_id:      node_name.clone(),
                            poll_messages: cfg.channel_in.clone(),
                            send_messages: cfg.channel_out.clone(),
                            router_config: FlowRouterConfig::Channel(ChannelFlowRouter::new()),
                        });
                    }
                }
            }))
            .await;

        me
    }

    pub fn subscribe(&self) {
        
    }

    /// Register a channel‐node in your flow:
    pub fn register(&self, node: ChannelNode) {
        self.map
            .entry(node.channel_name.clone())
            .or_default()
            .push(node);
    }
}

#[async_trait]
impl IncomingHandler for ChannelsRegistry {
    async fn handle_incoming(&self, msg: ChannelMessage) {
        // exactly your old `handle_incoming` logic:
        if let Some(nodes) = self.map.get(&msg.channel) {
            if nodes.is_empty() {
                error!(
                    channel = %msg.channel,
                    "received message but channel has no nodes configured"
                );
            } else {
                for node in nodes.iter().cloned() {
                    node.handle_message(&msg, &self.flow_manager).await;
                }
            }
        } else {
            error!(
                channel = %msg.channel,
                "received message but no flows bound for this channel"
            );
        }
    }
}

/// So that you can `#[typetag::serde]` your `ChannelNode` inside a flow graph:
#[async_trait]
#[typetag::serde]
impl NodeType for ChannelNode {
    fn type_name(&self) -> String {
        self.channel_name.clone()
    }

    fn schema(&self) -> RootSchema {
        schema_for!(ChannelNode)
    }

    /// Invoked when *inside* a flow you explicitly `ChannelNode.process(...)`.
    /// Serializes your internal `Message` payload into a `ChannelMessage`
    /// and sends it back out on the plugin’s send‐loop via your manager.
    #[tracing::instrument(name = "channel_node_process", skip(self,ctx))]
    async fn process(&self, input: Message, ctx: &mut NodeContext) -> Result<NodeOut, NodeErr> {
        let mut plugin = ctx
            .channel_manager()
            .channel(&self.channel_name)
            .ok_or_else(|| NodeErr::all(NodeError::Internal(format!("no such channel: {}", self.channel_name))))?;

        // try to deserialize a full ChannelMessage
        let send_result = if let Ok(mut cm) = serde_json::from_value::<ChannelMessage>(input.payload().clone())
        {
            // it was already a ChannelMessage
            cm.channel   = self.channel_name.clone();
            cm.direction = MessageDirection::Outgoing;
            if cm.to.is_empty() {
                // assuming we are returning to the channel the message came from
                if let Some(channel_origin) = ctx.channel_origin() {
                    cm.to = vec![channel_origin.participant()];
                } else {
                    let error = format!("No to field was specified so don't know where to send the message to in channel {} with session id {:?}",cm.channel, input.session_id());
                    error!(error);
                    return Err(NodeErr::all(NodeError::InvalidInput(error)));
                }
                
            }
            plugin.send(cm)
        } else {
            // fallback: wrap raw JSON as text
            let text = input.payload().to_string();
            // assuming we are returning to the channel the message came from
            let to = if let Some(channel_origin) = ctx.channel_origin() {
                vec![channel_origin.participant()]
            } else {
                let error = format!("No to field was specified so don't know where to send the message to in channel {} with session id {:?}",plugin.name(), input.session_id());
                error!(error);
                return Err(NodeErr::all(NodeError::InvalidInput(error)));
            };
            let cm = ChannelMessage {
                to: to.clone(),
                channel:   self.channel_name.clone(),
                session_id: input.session_id().clone(),
                direction: MessageDirection::Outgoing,
                content:   Some(MessageContent::Text(text)),
                ..Default::default()
            };
            plugin.send(cm)
        };

        if let Err(e) = send_result {
            warn!(error = ?e, "failed to send to channel {}", self.channel_name);
        }
        Ok(NodeOut::all(input))
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::manager::{ChannelManager, HostLogger};
    use crate::config::{ConfigManager, MapConfigManager};
    use crate::process::manager::ProcessManager;
    use crate::{executor::Executor, flow::manager::FlowManager, logger::OpenTelemetryLogger,
                secret::EmptySecretsManager, state::InMemoryState,};
    use crate::secret::SecretsManager;
    use crate::logger::Logger;
    use channel_plugin::message::{ChannelMessage, MessageDirection};

    #[tokio::test]
    async fn test_registry_dispatches_safely() {
        let store = InMemoryState::new();
        let secrets = SecretsManager(EmptySecretsManager::new());
        let logger = Logger(Box::new(OpenTelemetryLogger::new()));
        let exec = Executor::new(secrets.clone(), logger);
        let config = ConfigManager(MapConfigManager::new());
        let host_logger = HostLogger::new();
        let cm = ChannelManager::new(config, secrets.clone(), host_logger).await.expect("could not create channel manager");
        let pm = ProcessManager::dummy();
        let fm = FlowManager::new(store, exec, cm.clone(), pm.clone(), secrets);
        let reg = ChannelsRegistry::new(fm,cm).await;

        // no panic if nothing registered
        let mut msg = ChannelMessage::default();
        msg.channel = "foo".into();
        msg.direction = MessageDirection::Incoming;
        reg.handle_incoming(msg.clone()).await;

        // register one node
        let node = ChannelNode {
            channel_name: "foo".into(),
            flow_name:    "flow_x".into(),
            node_id:      "node_id".into(),
            poll_messages: true,
            send_messages: false,
            router_config: FlowRouterConfig::Channel(ChannelFlowRouter::default()),
        };
        reg.register(node);
        // still no panic, (router map empty)
        reg.handle_incoming(msg).await;
    }
}
