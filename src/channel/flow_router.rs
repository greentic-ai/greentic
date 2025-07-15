// flow_router.rs

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use channel_plugin::message::ChannelMessage;
use handlebars::Handlebars;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// A FlowRouter is responsible for mapping *every* incoming
/// `ChannelMessage` to zero or more `(flow_id, start_node_id)` pairs.
///
/// You can swap in a new router at runtime (e.g. on config-file change).
#[async_trait]
#[typetag::serde]
pub trait FlowRouter: Send + Sync {
    /// Given an incoming ChannelMessage, return all the flows
    /// that should be kicked off *and* the ID of the node
    /// inside each flow at which to begin.
    async fn resolve(&self, msg: &ChannelMessage) -> Vec<(String, String)>;
}

///
/// ChannelFlowRouter
///
/// A simple static mapping: “all messages on channel X go to flows A, B, C”
///
/// ```json
/// {
///   "type": "channel",
///   "map": {
///     "telegram": ["welcome_flow","audit_flow"],
///     "slack":    ["slack_notifications"]
///   }
/// }
/// ```
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "channel")]
pub struct ChannelFlowRouter {
    /// channel → list of flows
    pub map: HashMap<String, Vec<String>>,
}

impl ChannelFlowRouter {
    /// Construct an empty router.
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Add a mapping from `channel` to `flow_name`.
    pub fn add_mapping(&mut self, channel: &str, flow_name: &str) {
        self.map
            .entry(channel.into())
            .or_default()
            .push(flow_name.into());
    }
}

#[async_trait]
#[typetag::serde]
impl FlowRouter for ChannelFlowRouter {
    async fn resolve(&self, msg: &ChannelMessage) -> Vec<(String, String)> {
        // if there's no mapping, return empty Vec
        self.map
            .get(&msg.channel)
            .into_iter()
            .flat_map(|flows| {
                flows
                    .iter()
                    .map(move |flow| (flow.clone(), msg.channel.clone()))
            })
            .collect()
    }
}

///
/// ScriptFlowRouter
///
/// A templated mapping via Handlebars on the serialized message JSON:
///
/// ```json
/// {
///   "type": "script",
///   "templates": {
///     "sms": "{{#if payload.urgent}}urgent_flow{{else}}normal_flow{{/if}}"
///   }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename = "script")]
pub struct ScriptFlowRouter {
    /// channel → template string
    pub templates: HashMap<String, String>,

    // Handlebars registry, not serialized
    #[serde(skip)]
    #[schemars(skip)]
    #[schemars(default = "default_registry")]
    registry: Arc<Handlebars<'static>>,
}

fn default_registry() -> Arc<Handlebars<'static>> {
    Arc::new(Handlebars::new())
}

impl ScriptFlowRouter {
    /// Construct an empty script router.
    pub fn new() -> Self {
        Self {
            templates: HashMap::new(),
            registry: default_registry(),
        }
    }

    /// Install a new template for a channel.
    pub fn set_template(&mut self, channel: &str, template: &str) {
        self.templates.insert(channel.into(), template.into());
    }

    /// Render the message into a JSON value for templating.
    fn to_context(msg: &ChannelMessage) -> serde_json::Value {
        json!(msg)
    }
}

#[async_trait]
#[typetag::serde]
impl FlowRouter for ScriptFlowRouter {
    async fn resolve(&self, msg: &ChannelMessage) -> Vec<(String, String)> {
        // find the template for this channel
        let tmpl = match self.templates.get(&msg.channel) {
            Some(t) => t,
            None => return Vec::new(),
        };

        // render
        let ctx = Self::to_context(msg);
        match self.registry.render_template(tmpl, &ctx) {
            Ok(rendered) => {
                // split comma-separated flow names
                rendered
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|flow| (flow.to_string(), msg.channel.clone()))
                    .collect()
            }
            Err(e) => {
                tracing::error!("Flow template error for channel {}: {:?}", msg.channel, e);
                Vec::new()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use channel_plugin::message::ChannelMessage;

    fn make_msg(channel: &str) -> ChannelMessage {
        ChannelMessage {
            channel: channel.into(),
            ..ChannelMessage::default()
        }
    }

    #[tokio::test]
    async fn channel_router_empty() {
        let router = ChannelFlowRouter::new();
        let out = router.resolve(&make_msg("foo")).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn channel_router_mapped() {
        let mut router = ChannelFlowRouter::new();
        router.add_mapping("email", "flow1");
        router.add_mapping("email", "flow2");

        let out = router.resolve(&make_msg("email")).await;
        assert_eq!(
            out,
            vec![
                ("flow1".to_string(), "email".to_string()),
                ("flow2".to_string(), "email".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn script_router_empty() {
        let router = ScriptFlowRouter::new();
        let out = router.resolve(&make_msg("sms")).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn script_router_map_list() {
        let mut router = ScriptFlowRouter::new();
        router.set_template("sms", "a,b , c");
        let out = router.resolve(&make_msg("sms")).await;
        assert_eq!(
            out,
            vec![
                ("a".into(), "sms".into()),
                ("b".into(), "sms".into()),
                ("c".into(), "sms".into()),
            ]
        );
    }

    #[tokio::test]
    async fn script_router_bad_template() {
        let mut router = ScriptFlowRouter::new();
        router.set_template("sms", "{{#if}}"); // invalid
        let out = router.resolve(&make_msg("sms")).await;
        assert!(out.is_empty());
    }
}
