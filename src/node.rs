use std::{fmt, sync::Arc};
use async_trait::async_trait;
use channel_plugin::message::{MessageDirection, Participant};
use dashmap::{DashMap};
use crate::flow::state::{SessionState, StateValue};
use handlebars::{Handlebars,JsonValue};
use serde::{Deserialize,  Serialize};
use tempfile::TempDir;
use std::fs;
use std::fmt::Debug;
use std::path::PathBuf;
use serde_json::{json, Value};
use crate::{channel::{manager::ChannelManager, node::ChannelNode,}, executor::{exports::wasix::mcp::router::{Content, ResourceContents}, Executor}, flow::{manager::{ChannelNodeConfig, ResolveError, TemplateContext, ValueOrTemplate},}, mapper::Mapper, message::Message, process::manager::ProcessManager, secret::SecretsManager, util::extension_from_mime};
use schemars::{schema::{RootSchema, Schema}, schema_for, JsonSchema, SchemaGenerator};


#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum Routing {
    /// Use all connections defined in the graph (default)
    FollowGraph,
    /// Only use specific named nodes
    ToNodes(Vec<String>),
    ToNode(String),
    /// Send a reply to the original incoming channel
    ReplyToOrigin,
    /// Ends the Flow,
    EndFlow
}

impl Routing {
    pub fn to_node(&self) -> Option<&str> {
        match self {
            Routing::ToNode(node) => Some(node),
            _ => None,
        }
    }

    pub fn to_nodes(&self) -> Option<&[String]> {
        match self {
            Routing::ToNodes(nodes) => Some(nodes),
            _ => None,
        }
    }

    pub fn is_follow_graph(&self) -> bool {
        matches!(self, Routing::FollowGraph)
    }

    pub fn is_end_flow(&self) -> bool {
        matches!(self, Routing::EndFlow)
    }

    pub fn is_reply_to_origin(&self) -> bool {
        matches!(self, Routing::ReplyToOrigin)
    }
}

/// NodeOut enables to send the messages coming out of the Node
/// to either:
/// * all connections (FollowGraph)
/// * some connections (ToNodes)
/// * to the original incoming channel and user (ReplyToOrigin) 
/// * end the flow (EndFlow)
#[derive(Clone,Debug, Serialize, Deserialize,JsonSchema)]
pub struct NodeOut{
    message: Message,
    routing: Routing,
}

impl NodeOut {
    pub fn follow_graph(message: Message) -> Self {
        Self { message, routing: Routing::FollowGraph }
    }

    pub fn to_nodes(message: Message, targets: Vec<String>) -> Self {
        Self { message, routing: Routing::ToNodes(targets) }
    }

    pub fn to_node(message: Message, target: String) -> Self {
        Self { message, routing: Routing::ToNode(target) }
    }

    pub fn all(message: Message) -> Self {
        Self { message, routing: Routing::FollowGraph }
    }

    pub fn next(message: Message, targets: Option<Vec<String>>) -> Self {
        let routing = match targets {
            Some(targets) => if targets.len() == 1 {
                Routing::ToNode(targets.first().unwrap().to_owned())
            } else {
                Routing::ToNodes(targets)
            },
            None => Routing::FollowGraph,
        };
        Self {message, routing}
    }

    pub fn reply(message: Message) -> Self {
        Self { message, routing: Routing::ReplyToOrigin }
    }

    pub fn end_flow(message: Message) -> Self {
        Self { message, routing: Routing::EndFlow }
    }

    pub fn routing(&self) -> &Routing {
        &self.routing
    }

    pub fn message(&self) -> Message {
        self.message.clone()
    }

    pub fn with_routing(message: Message, routing: Routing) -> Self {
        Self { message, routing }
    }
}

/// NodeErr enables to send a NodeError coming out of the Node
/// to either:
/// * all connections (FollowGraph)
/// * some connections (ToNodes)
/// * to the original incoming channel and user (ReplyToOrigin) 
/// * end the flow (EndFlow)
#[derive(Clone,Debug, Serialize, Deserialize,JsonSchema)]
pub struct NodeErr{
    error: NodeError,
    routing: Routing,
}


impl NodeErr {
    pub fn fail(error: NodeError) -> Self {
        Self { error, routing: Routing::EndFlow }
    }
    pub fn follow_graph(error: NodeError) -> Self {
        Self { error, routing: Routing::FollowGraph }
    }

    pub fn all(error: NodeError) -> Self {
        Self { error, routing: Routing::FollowGraph }
    }

    pub fn next(error: NodeError, targets: Option<Vec<String>>) -> Self {
        let routing = match targets {
            Some(targets) => if targets.len() == 1 {
                Routing::ToNode(targets.first().unwrap().to_owned())
            } else {
                Routing::ToNodes(targets)
            },
            None => Routing::FollowGraph,
        };
        Self {error, routing}
    }

    pub fn to_nodes(error: NodeError, targets: Vec<String>) -> Self {
        Self { error, routing: Routing::ToNodes(targets) }
    }
    
    pub fn to_node(error: NodeError, target: String) -> Self {
        Self { error, routing: Routing::ToNode(target) }
    }

    pub fn reply(error: NodeError) -> Self {
        Self { error, routing: Routing::ReplyToOrigin }
    }

    pub fn end_flow(error: NodeError) -> Self {
        Self { error, routing: Routing::EndFlow }
    }

    pub fn routing(&self) -> &Routing {
        &self.routing
    }

    pub fn error(&self) -> NodeError {
        self.error.clone()
    }

    pub fn with_routing(error: NodeError, routing: Routing) -> Self {
        Self { error, routing }
    }
}


#[typetag::serde] 
#[async_trait]
pub trait NodeType: Send + Sync + Debug {
    fn type_name(&self) -> String;
    async fn process(&self, msg: Message, ctx: &mut NodeContext) -> Result<NodeOut, NodeErr>;
    fn clone_box(&self) -> Box<dyn NodeType>;
    /// Return this concrete type’s schema.
    fn schema(&self) -> schemars::schema::RootSchema;
}

#[derive( Serialize, Deserialize )]
pub struct Node(pub Box<dyn NodeType>);

// Optional if you want to access `.0`
impl std::ops::Deref for Node {
    type Target = dyn NodeType;

    fn deref(&self) -> &Self::Target {
        &*self.0
    }
}

impl std::ops::DerefMut for Node {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.0
    }
}

impl Clone for Node {
    fn clone(&self) -> Self {
        Node(self.0.clone_box())
    }
}

impl fmt::Debug for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // This will call the concrete node’s `Debug` impl
        f.debug_tuple("Node").field(&self.0).finish()
    }
}

/// now give it a JsonSchema impl
impl JsonSchema for Node {
    fn schema_name() -> String {
        // Optional: a catch‐all name.
        "node".to_string()
    }

    fn json_schema(generate: &mut SchemaGenerator) -> Schema {
        use schemars::schema::{Schema, SchemaObject, SubschemaValidation};
        use std::default::Default;

        // 1) enumerate every concrete implementor here
        let concrete: &[RootSchema] = &[
            schema_for!(ToolNode),
            schema_for!(ChannelNode),
            // …add others…
        ];

        // 2) merge their definitions into the global generator
        for rs in concrete {
            for (def_name, def_schema) in &rs.definitions {
                generate.definitions_mut().insert(def_name.clone(), def_schema.clone());
            }
        }

        // 3) collect references to each of those definitions
        let any_of = concrete.iter()
            .flat_map(|rs| rs.definitions.keys())
            .map(|def_name| {
                Schema::Object(SchemaObject {
                    reference: Some(format!("#/definitions/{}", def_name).into()),
                    ..Default::default()
                })
            })
            .collect();

        // 4) build a loose `anyOf` schema that references them
        Schema::Object(SchemaObject {
            subschemas: Some(Box::new(SubschemaValidation {
                any_of: Some(any_of),
                ..Default::default()
            })),
            ..Default::default()
        })
    }
}

#[derive(Clone,Debug)]
pub struct ChannelOrigin {
    channel: String,
    reply_to: Option<String>,
    thread_id: Option<String>,
    participant: Participant,
}

impl ChannelOrigin {
    pub fn new(
        channel: String, 
        reply_to: Option<String>,
        thread_id: Option<String>,
        participant: Participant) -> Self
    {
        Self {channel,reply_to,thread_id, participant}
    }

    pub fn channel(&self) -> String {
        self.channel.clone()
    }

    pub fn participant(&self) -> Participant {
        self.participant.clone()
    }


    pub fn reply_to(&self) -> Option<String> {
        self.reply_to.clone()
    }


    pub fn thread_id(&self) -> Option<String> {
        self.thread_id.clone()
    }

    /// Build a ChannelMessage that “replies” with `payload` to the original sender.
    pub fn reply(&self, 
                 node_id: &str,
                 session_id: String,
                 payload: serde_json::Value,
                 ctx: &NodeContext
    ) -> Result<channel_plugin::message::ChannelMessage, ResolveError> {
        // you can re‐use your existing ChannelNodeConfig logic
        // to fill in from/to/content/thread_id/etc.
        let cfg = ChannelNodeConfig {
          channel_name: self.channel.clone(),
          channel_in: false,
          channel_out: true,
          from: None,           // defaults to `greentic` platform
          to: Some(vec![ValueOrTemplate::Value(self.participant.clone())]),
          content: None,        // so that create_out_msg falls back to payload
          thread_id: None,
          reply_to_id: None,
        };

        // build it:
        cfg.create_out_msg(
          ctx,
          node_id.to_string(),
          session_id,
          payload,
          MessageDirection::Outgoing
        )
    }
}

#[warn(dead_code)]
#[derive(Clone,Debug)]
pub struct NodeContext {
    session_id: String,
    state: SessionState,
    config: DashMap<String, String>,
    executor: Arc<Executor>,
    channel_manager: Arc<ChannelManager>,
    process_manager: Arc<ProcessManager>,
    secrets: SecretsManager,
    channel_origin: Option<ChannelOrigin>,
    connections: Option<Vec<String>>,
    pub hb: Arc<Handlebars<'static>>,
}

impl NodeContext
{
    pub fn new(session_id: String, state: SessionState, config: DashMap<String, String>, executor: Arc<Executor>, channel_manager: Arc<ChannelManager>, process_manager: Arc<ProcessManager>, secrets: SecretsManager, channel_origin: Option<ChannelOrigin> ) -> Self {
        let hb = make_handlebars();
        Self {session_id, state, config, executor, channel_manager, process_manager, secrets, connections: None, channel_origin, hb,}
    }

    pub fn get_session_id(&self) -> String {
        self.session_id.clone()
    }

    pub fn channel_origin(&self) -> Option<ChannelOrigin> {
        self.channel_origin.clone()
    }

    pub fn add_nodes(&self, nodes: Vec<String>) {
        for n in nodes {
            self.state.add_node(n);
        }
    }

    pub fn pop_next_node(&mut self) -> Option<String> {
        self.state.pop_node()
    }

    pub fn peek_next_node(&mut self) -> Option<String> {
        self.state.peek_node()
    }

    pub fn add_node(&self, node: String) {
        self.state.add_node(node);
    }

    pub fn set_node(&self, node: String)
    {
        self.state.set_nodes(vec![node.clone()]);
    }

    pub fn set_nodes(&self, nodes: Vec<String>)
    {
        self.state.set_nodes(nodes);
    }

    pub fn nodes(&self) -> Option<Vec<String>> {
        self.state.nodes()
    }

    pub fn set_flow(&self, flow: String) {
        self.state.set_flows(vec![flow.clone()]);
    }

    pub fn add_flow(&self, flow: String) {
        self.state.add_flow(flow);
    }

    pub fn set_flows(&self, flows: Vec<String>)
    {
        self.state.set_flows(flows);
    }

    pub fn flow(&self) -> Option<String> {
        self.state.flows().and_then(|f| f.first().cloned())
    }

    pub fn flows(&self) -> Option<Vec<String>> {
        self.state.flows()
    }

    pub fn get_state(&self, key: &str) -> Option<StateValue> {
        self.state.get(key)
    }

    pub fn save_state<I>(&self, items: I)
    where
        I: IntoIterator<Item = (String, StateValue)> + Send
    {
        for (k, v) in items {
            self.state.set(k, v);
        }
    }

    pub fn state_contains_key(&self, key: &str) -> bool {
        self.state.contains(key)
    }

    pub fn get_all_state(&self) -> Vec<(String, StateValue)> {
        self.state.all()
    }

    pub fn set_state(&mut self, key: &str, value: StateValue) {
        self.state.set(key.to_string(), value);
    }

    pub fn delete_state(&mut self, key: &str) {
        self.state.remove(key);
    }

    pub fn get_config(&self, key: &str) -> Option<String> {
        let value = self.config.get(key).map(|r| r.clone());
        value
    }

    pub fn get_all_config(&self) -> DashMap<String, String> {
        self.config.clone()
    }

    pub fn set_config(&mut self, key: &str, value: String) {
        self.config.insert(key.to_string(), value);
    }

    pub fn delete_config(&mut self, key: &str) {
        self.config.remove(key);
    }

    pub fn executor(&self) -> &Executor {
        self.executor.as_ref()
    }

    pub fn channel_manager(&self) -> &ChannelManager {
        &self.channel_manager.as_ref()
    }

    pub fn process_manager(&self) -> &ProcessManager {
        &self.process_manager.as_ref()
    }

    pub fn set_connections(&mut self, connections: Option<Vec<String>>) {
        self.connections = connections;
    }

    pub fn connections(&self) -> Option<Vec<String>> {
        self.connections.clone()
    }

    pub async fn reveal_secret(&self, key: &str) -> Option<String> {
        match self.secrets.0.get(key){
            Some(handle) => 
                match self.secrets.0.reveal(handle).await {
                    Ok(secret) => secret,
                    Err(_) => None,
                }
            None => None,
        }
    }

}



/// A shared registry you build once at startup:
fn make_handlebars() -> Arc<Handlebars<'static>> {
    let hb = Handlebars::new();

    // If you have partials:
    // hb.register_partial("foo", "{{bar}} world").unwrap();

    // Or helpers:
    // hb.register_helper("upper", Box::new(|h, _, _| {
    //     let v = h.param(0).unwrap().value().as_str().unwrap();
    //     Ok(v.to_uppercase().into())
    // }));

    Arc::new(hb)
}


pub fn state_map_to_json(state: &DashMap<String, StateValue>) -> JsonValue {
    let mut obj = serde_json::Map::new();
    for entry in state.iter() {
        obj.insert(entry.key().clone(), entry.value().to_json());
    }
    JsonValue::Object(obj)
}

impl TemplateContext for NodeContext {
    fn render_template(&self, template: &str) -> Result<String, String> {
        let mut map = serde_json::Map::new();
        for (k, v) in self.state.all() {
            map.insert(k, v.to_json());
        }

        self.hb
            .render_template(template, &Value::Object(map))
            .map_err(|e| format!("handlebars error: {}", e))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum NodeError {
    NotFound (String),
    InvalidInput(String),
    ExecutionFailed(String),
    ConnectionFailed(String),
    Internal(String),
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeError::NotFound(msg) => write!(f, "Node {} not found.",msg),
            NodeError::InvalidInput(msg) => write!(f, "Invalid input: {}", msg),
            NodeError::ExecutionFailed(msg) => write!(f, "Processing error: {}",msg),
            NodeError::ConnectionFailed(msg) => write!(f, "Failed to connect to node: {}",msg),
            NodeError::Internal(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for NodeError {}

/// A ToolNode allows to call a tool either by name, action
/// and the payload will be passed to the action.
/// or dynamically by passing through the payload:
/// "tool_call": {
///     "name": "<name of the tool to call>",
///     "action": "<tool action to call>",
///     "input": "<input parameters to pass to the tool>"
/// }
/// You can use an optional Mapper for transforming input (in_map), 
/// output (out_map) and error (err_map).
/// If you want a specific set of connections to be called when the
/// call is successful, then set the names in the on_ok list.
/// If you want other connections to be called on error, set their names
/// in the on_err list.
/// If no on_ok or on_err are specified then all connections will be called 
/// with the result. 
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "tool")]
pub struct ToolNode {
    name: String,
    action: String,
    in_map: Option<Mapper>,
    out_map: Option<Mapper>,
    err_map: Option<Mapper>,
    on_ok: Option<Vec<String>>,
    on_err: Option<Vec<String>>,
}

impl ToolNode {
    pub fn new(
            name: String,
            action: String,
            in_map: Option<Mapper>,
            out_map: Option<Mapper>,
            err_map: Option<Mapper>,
            on_ok: Option<Vec<String>>,
            on_err: Option<Vec<String>>,
        ) -> Self {
        Self {
            name,
            action,
            in_map,
            out_map,
            err_map,
            on_ok,
            on_err,
        }
    }


    pub fn in_map(&self) -> Option<&Mapper> {
        self.in_map.as_ref()
    }

    pub fn out_map(&self) -> Option<&Mapper> {
        self.out_map.as_ref()
    }

    pub fn err_map(&self) -> Option<&Mapper> {
        self.err_map.as_ref()
    }

    pub fn use_in_map(&self) -> bool {
        self.in_map.is_some()
    }

    pub fn use_out_map(&self) -> bool {
        self.out_map.is_some()
    }

    pub fn use_err_map(&self) -> bool {
        self.err_map.is_some()
    }

    pub fn with_in_map(mut self, mapper: Mapper) -> Self {
        self.in_map = Some(mapper);
        self
    }

    pub fn with_out_map(mut self, mapper: Mapper) -> Self {
        self.out_map = Some(mapper);
        self
    }

    pub fn with_err_map(mut self, mapper: Mapper) -> Self {
        self.err_map = Some(mapper);
        self
    }

    pub fn name(&self) -> String {
        self.name.clone()
    }

    pub fn action(&self) -> String {
        self.action.clone()
    }

    fn build_routing(&self, is_error: bool) -> Routing {
        if is_error {
            match &self.on_err {
                Some(routes) => Routing::ToNodes(routes.clone()),
                None => Routing::FollowGraph,
            }
        } else {
            match &self.on_ok {
                Some(routes) => Routing::ToNodes(routes.clone()),
                None => Routing::FollowGraph,
            }
        }
    }

}
impl Clone for ToolNode {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            action: self.action.clone(),
            in_map: self.in_map.clone(),
            out_map: self.out_map.clone(),
            err_map: self.err_map.clone(),
            on_ok: self.on_ok.clone(),
            on_err: self.on_err.clone(),
        }
    }
}

#[async_trait]
#[typetag::serde]
impl NodeType for ToolNode {
    fn type_name(&self) -> String {
        self.name.clone()
    }

    fn schema(&self) -> RootSchema {
        // this is object-safe because it doesn't mention `Self: Sized`
        schema_for!(ToolNode)
    }

     #[tracing::instrument(name = "tool_node_process", skip(self,context))]
    async fn process(&self, input: Message, context: &mut NodeContext) -> Result<NodeOut, NodeErr> {


        let executor = context.executor();

        // Check for dynamic override and go dynamic, otherwise use static configuration
        let (name, action, params) = match input.payload().get("tool_call") {
            Some(tool_call) => {
                let name = tool_call.get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&self.name)
                    .to_string();

                let action = tool_call.get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&self.action)
                    .to_string();

                let input = tool_call.get("input").cloned().unwrap_or(json!({}));
                (name, action, input)
            },
            None => {
                // Fallback to static configuration
                let payload = if self.use_in_map() {
                    self.in_map
                        .as_ref()
                        .unwrap()
                        .apply_input(&input.payload(), &context.config, context.state.all())
                } else {
                    input.payload()
                };

                (self.name.clone(), self.action.clone(), payload)
            }
        };

        if input.payload().get("tool_call").is_some() {
            tracing::info!("Executing dynamic tool call: {}/{}", name, action);
        } else {
            tracing::info!("Executing static tool call: {}/{}", name, action);
        }

        let result = executor.executor.call_tool(name.clone(), action.clone(), params);

        match result {
            Ok(call_result) => {
                
                let mut outputs = Vec::new();

                for (i, item) in call_result.content.iter().enumerate() {
                    match item {
                        Content::Text(text) => {
                            let parsed: Result<Value, _> = serde_json::from_str(&text.text);
                            let val = match parsed {
                                Ok(v) => v,
                                Err(_) => serde_json::json!({ "text": text.text }),
                            };
                            outputs.push(val);
                        }
                        Content::Image(image) => {
                            let storage_dir = resolve_or_create_storage_dir(&context.config)?;
                            let filename = storage_dir.join(format!("image_{}.{}", i, extension_from_mime(&image.mime_type)));
                            fs::write(&filename, &image.data)
                                .map_err(|e| NodeErr::fail(NodeError::ExecutionFailed(format!("Failed to write image: {}", e))))?;
                            outputs.push(serde_json::json!({ "image": filename.to_string_lossy() }));
                        }
                        Content::Embedded(embedded) => {
                            match &embedded.resource_contents {
                                ResourceContents::Blob(blob) => {
                                    let storage_dir = resolve_or_create_storage_dir(&context.config,)?;
                                    let filename = storage_dir.join(format!("blob_{}.{}", i, extension_from_mime(blob.mime_type.as_deref().unwrap_or("bin"))));
                                    fs::write(&filename, &blob.blob)
                                        .map_err(|e| NodeErr::fail(NodeError::ExecutionFailed(format!("Failed to write blob: {}", e))))?;
                                    outputs.push(serde_json::json!({ "blob": filename.to_string_lossy() }));
                                }
                                ResourceContents::Text(text) => {
                                    let storage_dir = resolve_or_create_storage_dir(&context.config)?;
                                    let filename = storage_dir.join(format!("text_{}.txt", i));
                                    fs::write(&filename, &text.text)
                                        .map_err(|e| NodeErr::fail(NodeError::ExecutionFailed(format!("Failed to write text: {}", e))))?;
                                    outputs.push(serde_json::json!({ "text_file": filename.to_string_lossy() }));
                                }
                            }
                        }
                    }
                }

                let output_json = if outputs.len() == 1 {
                    outputs.into_iter().next().unwrap_or_else(|| json!({}))
                } else {
                    Value::Array(outputs)
                };

                let mapped = if call_result.is_error.unwrap_or(false)  {
                    if self.use_err_map() {
                        Some(self.err_map.as_ref().unwrap().apply_result(&output_json, &context.config, context.state.all()))
                    } else {
                        None
                    }
                } else {
                    if self.use_out_map() {
                        Some(self.out_map.as_ref().unwrap().apply_result(&output_json, &context.config, context.state.all()))
                    } else {
                        None
                    }
                };

                if let Some(mapper_output) = mapped {
                    for (k, v) in mapper_output.state_updates {
                        context.state.set(k, v);
                    }
                    for (k, v) in mapper_output.config_updates {
                        context.config.insert(k, v);
                    }
                    let output_message = Message::new(&input.id(), mapper_output.payload, input.session_id());

                    let routing = self.build_routing(call_result.is_error.unwrap_or(false));
                    Ok(NodeOut::with_routing(output_message, routing))
                } else {
                    let output_message = Message::new(&input.id(), output_json, input.session_id());
                    let routing = self.build_routing(call_result.is_error.unwrap_or(false));
                    Ok(NodeOut::with_routing(output_message, routing))
                }
            }
            Err(e) => {
                Err(NodeErr::fail(
                    NodeError::ExecutionFailed(format!("Tool call failed: {:?}", e))
                ))
            }
        }
    }
    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

fn resolve_or_create_storage_dir(
    config: &DashMap<String, String>,
) -> Result<PathBuf, NodeErr> {
    if let Some(dir_str) = config.get("node_storage_dir") {
        let path = PathBuf::from(dir_str.value());
        if !path.exists() {
            fs::create_dir_all(&path)
            .map_err(|e| {
                NodeErr::fail(
                    NodeError::ExecutionFailed(format!("Failed to create node_storage_dir: {}", e)),
                )
            })?;
        }
        Ok(path)
    } else {
        let tempdir = TempDir::new()
            .map_err(|e| {
                NodeErr::fail(
                    NodeError::ExecutionFailed(format!("Failed to create tempdir: {}", e)),
                )
            })?;
        Ok(tempdir.path().to_path_buf())
    }
}

#[cfg(test)]
pub mod tests {
    use std::path::Path;
    use super::*;
    use crate::channel::manager::HostLogger;
    use crate::config::{ConfigManager, MapConfigManager};
    use crate::executor::exports::wasix::mcp::router::{CallToolResult, TextContent, ToolError};
    use crate::flow::session::InMemorySessionStore;
    use crate::logger::{Logger, OpenTelemetryLogger};
    use crate::message::Message;
    use crate::secret::{EmptySecretsManager, SecretsManager};
    use crate::flow::state::{InMemoryState, StateValue};
    use channel_plugin::plugin::LogLevel;
    use serde_json::json;
    use tempfile::TempDir;

    impl NodeContext {
        pub fn dummy() -> Self {
            let hb = make_handlebars();
            Self { 
                session_id: "123".to_string(),
                state: InMemoryState::new(), 
                config: DashMap::new(), 
                executor: Executor::dummy(), 
                channel_manager: ChannelManager::dummy(), 
                process_manager: ProcessManager::dummy(), 
                secrets: SecretsManager(EmptySecretsManager::new()), 
                channel_origin: None, 
                connections: None,
                hb,
            }
        }

        pub fn mock(result: Result<CallToolResult, ToolError>) -> Self {
            let hb = make_handlebars();
            Self { 
                session_id: "123".to_string(),
                state: InMemoryState::new(), 
                config: DashMap::new(), 
                executor: Arc::new(Executor::mock(result)), 
                channel_manager: ChannelManager::dummy(), 
                process_manager: ProcessManager::dummy(), 
                secrets: SecretsManager(EmptySecretsManager::new()), 
                channel_origin: None, 
                connections: None,
                hb,
            }
        }
    }

    

    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct EchoNode;

    impl Clone for EchoNode {
        fn clone(&self) -> Self {
            EchoNode
        }
    }

    #[async_trait]
    #[typetag::serde]
    impl NodeType for EchoNode {
        fn type_name(&self) -> String {
            "echo".to_string()
        }

        fn schema(&self) -> RootSchema {
            // this is object-safe because it doesn't mention `Self: Sized`
            schema_for!(EchoNode)
        }

        async fn process(&self, input: Message, context: &mut NodeContext) -> Result<NodeOut, NodeErr> {
            context.set_state("echoed", StateValue::String("yes".to_string()));
            Ok(NodeOut::with_routing(input, Routing::FollowGraph))
        }

        fn clone_box(&self) -> Box<dyn NodeType> {
            Box::new(self.clone())
        }
    }

   

    fn make_test_context_with_mock(exec_result: Result<CallToolResult, ToolError>) -> NodeContext {
        NodeContext::mock(exec_result)
    }

    #[tokio::test]
    async fn test_dynamic_tool_call_executes_correctly() {
        let payload = json!({
            "tool_call": {
                "name": "weather_tool",
                "action": "forecast",
                "input": { "location": "Paris" }
            }
        });

        let input = Message::new("test", payload, "sess1".to_string());
        let result = CallToolResult {
            content: vec![Content::Text(TextContent {text:"{\"result\":\"sunny\"}".into(), annotations: None })],
            is_error: Some(false),
        };

        let node = ToolNode::new(
            "fallback_tool".into(),
            "fallback_action".into(),
            None, None, None,
            Some(vec!["next_success".into()]),
            Some(vec!["on_error".into()])
        );

        let mut ctx = make_test_context_with_mock(Ok(result));
        let out = node.process(input, &mut ctx).await.unwrap();
        assert_eq!(out.routing(), &Routing::ToNodes(vec!["next_success".into()]));
    }

    #[tokio::test]
    async fn test_static_tool_config_used_if_no_tool_call() {
        let payload = json!({ "location": "London" });
        let input = Message::new("static_test", payload.clone(), "sess2".to_string());

        let result = CallToolResult {
            content: vec![Content::Text(TextContent {text:"{\"static\":\"ok\"}".into(), annotations:None})],
            is_error: Some(false),
        };

        let node = ToolNode::new("static_tool".into(), "run".into(), None, None, None, None, None);
        let mut ctx = make_test_context_with_mock(Ok(result));
        let out = node.process(input.clone(), &mut ctx).await.unwrap();
        assert_eq!(out.message().payload()["static"], "ok");
    }

    #[tokio::test]
    async fn test_error_routing_goes_to_on_err() {
        let payload = json!({
            "tool_call": {
                "name": "bad_tool",
                "action": "crash",
                "input": {}
            }
        });

        let input = Message::new("err_test", payload, "sess3".to_string());

        let node = ToolNode::new(
            "fallback".into(),
            "noop".into(),
            None, None, None,
            Some(vec!["ok_conn".into()]),
            Some(vec!["err_conn".into()])
        );

        let mut ctx = make_test_context_with_mock(Err(ToolError::ExecutionError("bad call".into())));

        let err = node.process(input, &mut ctx).await.unwrap_err();
        assert_eq!(err.routing(), &Routing::EndFlow);
    }

    #[tokio::test]
    async fn test_missing_dynamic_fields_falls_back_to_static() {
        let payload = json!({
            "tool_call": {
                "input": { "location": "nowhere" }
            }
        });

        let input = Message::new("missing", payload.clone(), "sess4".to_string());
        let result = CallToolResult {
            content: vec![Content::Text(TextContent {text:"{\"fallback\":\"true\"}".into(), annotations: None })],
            is_error: Some(false),
        };

        let node = ToolNode::new("fallback_tool".into(), "fallback_action".into(), None, None, None, None, None);
        let mut ctx = make_test_context_with_mock(Ok(result));
        let out = node.process(input, &mut ctx).await.unwrap();

        assert_eq!(out.message().payload()["fallback"], "true");
    }

    #[tokio::test]
    async fn test_node_context_get_set_delete() {
        let temp_dir = TempDir::new().unwrap();
        let config = DashMap::new();
        config.insert("node_storage_dir".into(), temp_dir.path().to_string_lossy().to_string());

        let secrets =SecretsManager(EmptySecretsManager::new());
        let logging = Logger(Box::new(OpenTelemetryLogger::new()));
        let executor = Executor::new(secrets.clone(),logging);
        let config_mgr = ConfigManager(MapConfigManager::new());
        let host_logger = HostLogger::new(LogLevel::Debug);
        let store =InMemorySessionStore::new(10);
        let channel_manager = ChannelManager::new(config_mgr, secrets.clone(),store.clone(), host_logger).await.expect("could not create channel manager");
        let process_manager = ProcessManager::dummy();
        let mut ctx = NodeContext::new("123".to_string(),InMemoryState::new(), config, executor,channel_manager, process_manager, secrets, None);
        assert!(ctx.get_state("missing").is_none());

        ctx.set_state("key", StateValue::String("value".to_string()));
        assert_eq!(ctx.get_state("key"), Some(StateValue::String("value".to_string())));

        ctx.delete_state("key");
        assert!(ctx.get_state("key").is_none());
    }

    #[test]
    fn test_node_debug_output() {
        let node = Node(Box::new(EchoNode));
        let output = format!("{:?}", node);
        assert_eq!(output, r#"Node(EchoNode)"#);
    }


    #[test]
    fn test_node_error_display() {
        let err = NodeError::InvalidInput("bad".to_string());
        assert_eq!(format!("{}", err), "Invalid input: bad");
    }


    #[tokio::test(flavor = "multi_thread")]
    async fn test_tool_node_with_text_output() {
        let temp_dir = TempDir::new().unwrap();
        let config = DashMap::new();
        config.insert("node_storage_dir".into(), temp_dir.path().to_string_lossy().to_string());

        let secrets =SecretsManager(EmptySecretsManager::new());
        let logging = Logger(Box::new(OpenTelemetryLogger::new()));
        let executor = Executor::new(secrets.clone(),logging);
        let watcher = executor.watch_tool_dir(Path::new("./tests/wasm/mock_tool_watcher").to_path_buf()).await;
        assert!(watcher.is_ok());
        let config_mgr = ConfigManager(MapConfigManager::new());
        let host_logger = HostLogger::new(LogLevel::Debug);
        let store =InMemorySessionStore::new(10);
        let channel_manager = ChannelManager::new(config_mgr, secrets.clone(), store.clone(),host_logger).await.expect("could not create channel manager");
        let process_manager = ProcessManager::dummy();
        let mut context = NodeContext::new("123".to_string(),InMemoryState::new(), config, executor, channel_manager, process_manager, secrets, None);

        let node = ToolNode::new("mock_tool".to_string(), "text_output".to_string(),None, None, None, None, None);
        let msg = Message::new("msg1", json!({"input": "Hello"}),"123".to_string());

        let result = node.process(msg.clone(), &mut context).await.unwrap();
        let output = result.message.payload();
        assert_eq!(output, json!({"input": "Hello"}));
        watcher.unwrap().shutdown();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tool_node_saves_binary_and_text() {
        let temp_dir = TempDir::new().unwrap();
        let config = DashMap::new();
        config.insert("node_storage_dir".into(), temp_dir.path().to_string_lossy().to_string());

        let secrets =SecretsManager(EmptySecretsManager::new());
        let logging = Logger(Box::new(OpenTelemetryLogger::new()));
        let executor = Executor::new(secrets.clone(),logging);
        let watcher = executor.watch_tool_dir(Path::new("./tests/wasm/mock_tool_watcher").to_path_buf()).await;
        assert!(watcher.is_ok());
        let config_mgr = ConfigManager(MapConfigManager::new());
        let host_logger = HostLogger::new(LogLevel::Debug);
        let store =InMemorySessionStore::new(10);
        let channel_manager = ChannelManager::new(config_mgr, secrets.clone(), store.clone(), host_logger).await.expect("could not create channel manager");
        let process_manager = ProcessManager::dummy();
        let mut context = NodeContext::new("123".to_string(),InMemoryState::new(), config, executor, channel_manager, process_manager, secrets, None);

        let node = ToolNode::new("mock_tool".to_string(), "file_output".to_string(), None, None, None, None, None);
        let msg = Message::new("msg2", json!({"input": "data"}),"123".to_string());

        let result = node.process(msg.clone(), &mut context).await.unwrap();
        let output = result.message.payload();

        let obj = output.as_object().expect("Expected object in result");
        let path = obj.values().next().unwrap().as_str().unwrap();
        assert!(
            std::path::Path::new(path).exists(),
            "Expected file path to exist: {}",
            path
        );
        
        watcher.unwrap().shutdown();
    }

}

