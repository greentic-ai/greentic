// src/flow.rs

use std::{
    collections::{HashMap, HashSet},fmt, fs, path::{Path, PathBuf}, sync::Arc, time::Duration
};
use crate::{
    agent::manager::BuiltInAgent, channel::manager::ChannelManager, executor::Executor, mapper::Mapper, message::Message, node::{ChannelOrigin, NodeContext, NodeErr, NodeError, NodeOut, NodeType}, process::manager::{BuiltInProcess, ProcessManager}, secret::SecretsManager, state::StateStore, watcher::{DirectoryWatcher, WatchedType}
};
use anyhow::Error;
use channel_plugin::message::{ChannelMessage, MessageContent, MessageDirection, Participant};
use thiserror::Error;
use std::fmt::Debug;
use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use dashmap::DashMap;
use petgraph::{graph::NodeIndex, prelude::{StableDiGraph, StableGraph}, visit::{Topo, Walker}, Directed, Direction::Incoming};
use schemars::{schema::{InstanceType, Metadata, Schema, SchemaObject}, JsonSchema, SchemaGenerator};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use tokio::{sync::Mutex,time::sleep};
use tracing::{error, info};
use crate::node::ToolNode;

/// One record per-node, successful or error
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NodeRecord {
    pub node_id: String,
    pub attempt: usize,
    pub started: DateTime<Utc>,
    pub finished: DateTime<Utc>,
    pub result: Result<NodeOut, NodeErr>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionReport {
    pub records: Vec<NodeRecord>,
    /// If we stopped early, this is Some((node_id, error))
    pub error: Option<(String, NodeError)>,
    /// total elapsed wall time
    pub total: TimeDelta,
}

impl JsonSchema for ExecutionReport {
    fn schema_name() -> String {
        "ExecutionReport".to_string()
    }

    fn json_schema(gene: &mut SchemaGenerator) -> Schema {
        // Get subschemas for the obvious fields:
        let mut object = gene.subschema_for::<Vec<NodeRecord>>().into_object();
        let error_schema = gene.subschema_for::<Option<(String, NodeError)>>();

        // Now manually build a property schema for `total` as a number:
        let mut total_meta = Metadata::default();
        total_meta.description = Some("Total elapsed time in seconds".to_string());
        let total_schema = Schema::Object(SchemaObject {
            metadata: Some(Box::new(total_meta)),
            instance_type: Some(InstanceType::Number.into()),
            ..Default::default()
        });

        // Inject the `error` and `total` props:
        object.object().properties.insert("error".to_string(), error_schema);
        object.object().properties.insert("total".to_string(), total_schema);

        // And make sure `records` was already there under its property name:
        Schema::Object(object)
    }
}

/// A declarative flow: id/title/description, map of node-configs, and connections
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Flow {
    id: String,
    title: String,
    description: String,
    
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<String>,

    /// node_id → node configuration
    #[serde(
      deserialize_with = "deserialize_nodes_with_id",
      serialize_with   = "serialize_nodes"    // if you want to hide `id` on serialisation
    )]
    nodes: HashMap<String, NodeConfig>,

    /// adjacency: from node_id → list of to node_ids
    connections: HashMap<String, Vec<String>>,

    #[serde(skip)]
    #[schemars(skip)]
    graph: StableDiGraph<NodeConfig, ()>,
    #[serde(skip)]
    #[schemars(skip)]
    index_of: HashMap<String, NodeIndex>,
    #[serde(skip)]
    #[schemars(skip)]
    plans: HashMap<String, Vec<NodeIndex>>,
}


// this just re‐uses the normal Serde impl for the map but
// after we get it, we fix up each NodeConfig.id
fn deserialize_nodes_with_id<'de, D>(
    deserializer: D
) -> Result<HashMap<String, NodeConfig>, D::Error>
where
    D: Deserializer<'de>
{
    // 1) first let Serde give us a HashMap<String,RawNodeConfig>
    let raw: HashMap<String, RawNodeConfig> = HashMap::deserialize(deserializer)?;
    // 2) now turn it into your real NodeConfig, injecting the key as `id`
    let mut out = HashMap::with_capacity(raw.len());
    for (key, r) in raw {
        out.insert(key.clone(), NodeConfig {
            id: key,
            kind: r.kind,
            config: r.config,
            max_retries: r.max_retries,
            retry_delay_secs: r.retry_delay_secs,
        });
    }
    Ok(out)
}

// if you want to hide `id` when serializing back out, you can
// just re‐serialize the map normally (it won’t include `id`)
fn serialize_nodes<S>(
    nodes: &HashMap<String, NodeConfig>,
    serializer: S
) -> Result<S::Ok, S::Error>
where
    S: Serializer
{
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(Some(nodes.len()))?;
    for (k, v) in nodes {
        // serialize v *without* its `id` field, because RawNodeConfig derived Serialize
        let raw = RawNodeConfig {
            kind: v.kind.clone(),
            config: v.config.clone(),
            max_retries: v.max_retries,
            retry_delay_secs: v.retry_delay_secs,
        };
        map.serialize_entry(k, &raw)?;
    }
    map.end()
}

pub fn json_to_message_content(v: Value) -> MessageContent {
    // First try the “normal” enum‐deserialization:
    match serde_json::from_value::<MessageContent>(v.clone()) {
        Ok(mc) => mc,

        // If that fails, fall back to Text(v.to_string()):
        Err(_) => MessageContent::Text(v.to_string()),
    }
}

impl Flow {
    pub fn id(&self) -> String {
        self.id.clone()
    }
    /// Create a new, empty flow with the given identifiers.
    pub fn new(id: impl Into<String>, title: impl Into<String>, description: impl Into<String>) -> Self {
        Flow {
            id: id.into(),
            title: title.into(),
            description: description.into(),
            channels: Vec::new(),
            nodes: HashMap::new(),
            connections: HashMap::new(),
            graph: StableDiGraph::new(),
            index_of: HashMap::new(),
            plans: HashMap::new(),
        }
    }
    /// Deserialize + build the internal graph
    pub fn build(mut self) -> Self {
        // 0) collect all the to‐be‐split node IDs
        let mut to_split = Vec::new();
        for (nid, cfg) in &self.nodes {
            if let NodeKind::Channel { cfg } = &cfg.kind {
                if cfg.channel_in && cfg.channel_out {
                    to_split.push(nid.clone());
                }
            }
        }

        // 1) for each split_id…
        for node_id in to_split {
            // grab & clone the existing ChannelConfig
            let base = match &self.nodes[&node_id].kind {
                NodeKind::Channel { cfg } => cfg.clone(),
                _ => unreachable!("only splitting Channel nodes"),
            };
            // a) grab its existing children (where it published to next)
            let children = self.connections.remove(&node_id).unwrap_or_default();

            // b) rename the publisher half
            let out_id = format!("{}__out", node_id);
            // clone + tweak your ChannelConfig…
            let mut pub_cfg = base.clone();
            pub_cfg.channel_in  = false;
            pub_cfg.channel_out = true;
            // **this is key**: send somewhere *else*
            pub_cfg.channel_name = format!("{}_out", base.channel_name);

            self.nodes.insert(out_id.clone(), NodeConfig {
                id:   out_id.clone(),
                kind: NodeKind::Channel { cfg: pub_cfg },
                config: None,
                max_retries: None,
                retry_delay_secs: None,
            });

            // mutate the original into a pure receiver…
            if let NodeKind::Channel { cfg: orig_cfg } =
                &mut self.nodes.get_mut(&node_id).unwrap().kind
            {
                orig_cfg.channel_in  = true;
                orig_cfg.channel_out = false;
            }

            // rewire edges:
            self.connections.insert(node_id.clone(), vec![out_id.clone()]);
            if !children.is_empty() {
                self.connections.insert(out_id.clone(), children);
            }
        }

        // now you can carry on as before:
        let mut graph = StableDiGraph::new();
        let mut index_of = HashMap::new();

        // 1) add nodes to graph
        for (nid, cfg) in &self.nodes {
            let idx = graph.add_node(cfg.clone());
            index_of.insert(nid.clone(), idx);
        }

        // 2) add edges from self.connections…
        for (from, tos) in &self.connections {
            if let Some(&i) = index_of.get(from) {
                for to in tos {
                    if let Some(&j) = index_of.get(to) {
                        graph.add_edge(i, j, ());
                    }
                }
            }
        }

        // 3) cycle check, sanity, etc.
        assert!(
            !petgraph::algo::is_cyclic_directed(&graph),
            "flow `{}` has cycles (after splitting channel-nodes!)",
            self.id
        );

        let mut plans: HashMap<String, Vec<NodeIndex>> = HashMap::new();
        let topo = Topo::new(&graph);
        // we’ll get a full topological order once:
        let full_order: Vec<NodeIndex> = topo.iter(&graph).collect();

        for start_name in self.nodes.keys() {
            let start_idx = index_of[start_name];
            // find all reachable from start
            let mut reachable = HashSet::new();
            let mut stack = vec![start_idx];
            while let Some(n) = stack.pop() {
                if reachable.insert(n) {
                    for succ in graph.neighbors_directed(n, petgraph::Direction::Outgoing) {
                        stack.push(succ);
                    }
                }
            }
            // now filter the full topo order down to only those reachable
            let plan = full_order
                .iter()
                .cloned()
                .filter(|ix| reachable.contains(ix))
                .collect::<Vec<_>>();
            plans.insert(start_name.clone(), plan);
        }

        self.plans = plans;
        self.graph    = graph;
        self.index_of = index_of;
        self
    }

    /// Kick off this flow with an initial message
    pub async fn run(&self, msg: Message, start: &str, ctx: &mut NodeContext) -> ExecutionReport {
        let run_start = Utc::now();
        let mut records = Vec::new();
        let mut early_err: Option<(String, NodeError)> = None;
        // outputs holds the “current” Message for each node index
        let mut outputs: HashMap<NodeIndex, Message> = HashMap::new();
        // seed the start node
        let start_idx = *self.index_of
            .get(start)
            .unwrap_or_else(|| panic!("run(): no plan for start `{}`", start));
        outputs.insert(start_idx, msg.clone());

        // look up the linear plan
        let plan = &self.plans[start];

        for &nx in plan {
            let cfg: &NodeConfig = &self.graph[nx];
            let node_id = cfg.id.clone();
            let max_retries = cfg.max_retries.unwrap_or(0);
            let retry_delay = Duration::from_secs(cfg.retry_delay_secs.unwrap_or(1));

            // pull in the single “input” message
            let input = if let Some(m) = outputs.remove(&nx) {
                m
            } else {
                // if multiple upstream preds: merge as before
                let mut ins = Vec::new();
                for pred in self.graph.neighbors_directed(nx, Incoming) {
                    ins.push(
                        outputs.remove(&pred)
                            .expect("missing upstream output")
                    );
                }
                if ins.len() == 1 {
                    ins.into_iter().next().unwrap()
                } else {
                    // copy the session_id from the first upstream message
                    let session_id = ins[0].clone().session_id().clone();
                    let arr = ins.into_iter().map(|m| m.payload()).collect();
                    Message::new(&msg.id(), Value::Array(arr),session_id)

                }
            };

            let mut attempt = 0;
            
            let result = loop {
                let started = Utc::now();
                // dispatch by kind
                let res = match &cfg.kind {
                    NodeKind::Channel { cfg } => {
                        if cfg.channel_out {
                            let cm = cfg.create_out_msg(
                                ctx,
                                input.id().to_string(),
                                input.session_id(),
                                input.payload(),
                                MessageDirection::Outgoing,
                            );
                            
                            match cm {
                                Ok(msg) => {
                                    ctx.channel_manager()
                                    .send_to_channel(&cfg.channel_name, msg)
                                    .map(|_| NodeOut::all(input.clone()))
                                    .map_err(|e| NodeErr::all(NodeError::ConnectionFailed(format!("{:?}", e))))
                                },
                                Err(err) => {
                                    let error = format!("Could not create a channel message because of {:?}",err);
                                    error!(error);
                                    Err(NodeErr::all(NodeError::ExecutionFailed(error)))
                                },
                            }
                            
                        } else {
                            Ok(NodeOut::all(input.clone()))
                        }
                    }

                    NodeKind::Tool { tool } => {
                        // same as before…
                        let mut msg_with_params = input.clone();
                        // Pull out the session ID and payload before we overwrite msg_with_params
                        let session_id = msg_with_params.session_id().clone();
                        let payload = msg_with_params.payload();

                        // Rebuild the message
                        msg_with_params = Message::new(
                            &msg_with_params.id(),
                            payload,
                            session_id,
                        );

                        // fetch secrets…
                        let secrets_keys = ctx.executor().executor.secrets(tool.name.clone());

                        let mut secret_map = HashMap::new();
                        if secrets_keys.is_some() {
                            for s in &secrets_keys.unwrap()
                            {
                                let name = s.name.clone();
                                if let Some(tok) = ctx.reveal_secret(&name).await {
                                    secret_map.insert(name, tok.clone());
                                }
                            }
                        }

                        ToolNode::new(
                            tool.name.clone(),
                            tool.action.clone(),
                            tool.in_map.clone(),
                            tool.out_map.clone(),
                            tool.err_map.clone(),
                            tool.on_ok.clone(),
                            tool.on_err.clone(),
                        ).process(msg_with_params, ctx).await
                    }

                    NodeKind::Agent { agent } => {
                        agent.process(input.clone(), ctx).await
                    }

                    NodeKind::Process { process } => {
                        process.process( input.clone(),ctx).await
                    }
                };

                let finished = Utc::now();
                // record this attempt
                records.push(NodeRecord {
                    node_id: node_id.clone(),
                    attempt,
                    started,
                    finished: finished.clone(),
                    result: res.clone(),
                });

                match res {
                    Ok(output) => break Ok(output),
                    Err(_) if attempt < max_retries => {
                        attempt += 1;
                        sleep(retry_delay).await;
                    }
                    Err(err) => {
                        break Err(err);
                    }
                }
            };

            match result {
                Ok(out_msg) => {
                    outputs.insert(nx, out_msg.message());
                }
                Err(err) => {
                    early_err = Some((node_id.clone(), err.error()));
                    break;
                }
            }
        }

        let total = Utc::now() - run_start;
        ExecutionReport {
            records,
            error: early_err,
            total,
        }
    }


    pub fn channels(&self) -> Vec<String>{
        self.channels.clone()
    }

    pub fn nodes(&self) -> HashMap<String, NodeConfig>{
        self.nodes.clone()
    }

    pub fn connections(&self) -> HashMap<String, Vec<String>>{
        self.connections.clone()
    }

    pub fn add_channel(&mut self, name: impl Into<String>) {
        let name = name.into();
        if !self.channels.contains(&name) {
            self.channels.push(name);
        }
    }

    pub fn add_node(&mut self, name: String, node: NodeConfig) -> Option<NodeConfig> {
        self.nodes.insert(name, node)
    }

    pub fn add_connection(&mut self, name: String, flows: Vec<String>) -> Option<Vec<String>> {
        self.connections.insert(name, flows)
    }

    pub fn graph(&self) -> StableGraph<NodeConfig, (), Directed, u32> {
        self.graph.clone()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ValueOrTemplate<T> {
    Value(T),
    Template(String),
}

impl ValueOrTemplate<Participant> {
    /// Instead of parsing into `Participant` from JSON, just render the template
    /// as a plain string and treat that as the `participant.id`.
    fn resolve_as_string<C>(&self, ctx: &C) -> Result<String, ResolveError>
    where
        C: TemplateContext,
    {
        match self {
            ValueOrTemplate::Value(part) => {
                // if you passed a literal Participant, just grab its `id`:
                Ok(part.id.clone())
            }
            ValueOrTemplate::Template(tmpl) => {
                let rendered = ctx.render_template(tmpl)
                    .map_err(ResolveError::Template)?;
                // rendered is a String like "p1"; return it directly:
                Ok(rendered)
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ChannelNodeConfig {
    /// The channel’s canonical name
    #[serde(rename = "channel")]
    pub channel_name: String,

    /// Whether this node should receive (i.e. subscribe to) this channel
    // Skip if `false`
    #[serde(rename = "in", default, skip_serializing_if = "std::ops::Not::not")]
    pub channel_in: bool,

    /// Whether this node should send (i.e. publish to) this channel
    // Skip if `false`
    #[serde(rename = "out", default, skip_serializing_if = "std::ops::Not::not")]
    pub channel_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<ValueOrTemplate<Participant>>,               // Sender info
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<ValueOrTemplate<Participant>>>,            // Recipient(s)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<ValueOrTemplate<MessageContent>>, // Text or attachment
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<ValueOrTemplate<String>>,       // For threading support
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_id: Option<ValueOrTemplate<String>>, 
}

impl ChannelNodeConfig {
    /// Build a ChannelMessage using this node’s config, a NodeContext, and
    /// the incoming payload/id/session_id/direction.
    pub fn create_out_msg(
        &self,
        ctx: &NodeContext,
        id: String,
        session_id: Option<String>,
        payload: Value,
        direction: MessageDirection,
    ) -> Result<ChannelMessage, ResolveError> {
        // 1) `from`: prefer config, else fallback to channel_origin if you want:
        let from = if let Some(inner) = &self.from {
            inner.resolve(ctx)?
        } else {
            let co = ctx.channel_origin();
            if co.is_none() {
                // no from set, make it the platform
                Participant { 
                    id: id.clone(), 
                    display_name: Some("greentic".to_string()), 
                    channel_specific_id: Some("greentic".to_string()), 
                }
            } else {
                // Optional: fall back to channel_origin for `from` as well
                ctx.channel_origin().map(|co| co.participant()).unwrap()
            }

        };

        // 2) `to`: first try config, else channel_origin, else error:
        let to = if let Some(list) = &self.to {
                list.iter()
                    .map(|tpl| {
                        // 1. Render the template & produce a String:
                        let rendered = tpl.resolve_as_string(ctx)?;
                        // 2. Create a Participant whose only `id` is the rendered string:
                        Ok(Participant {
                            id: rendered,
                            display_name: None,
                            channel_specific_id: None,
                        })
                    })
                    .collect::<Result<Vec<_>, ResolveError>>()?
        } else if let Some(co) = ctx.channel_origin() {
            vec![co.participant()]
        } else {
            let error = format!("no `to` configured and no channel_origin for session_id {:?}", session_id);
            error!(error);
            return Err(ResolveError::Template(
               error.into(),
            ));
        };

        // 3) `content`: prefer config, else convert the raw payload:
        let content = if let Some(inner) = &self.content {
            Some(inner.resolve(ctx)?)
        } else {
            Some(json_to_message_content(payload))
        };

        // 4) `thread_id` and `reply_to_id` are both simple Option<…>:
        let thread_id = self.thread_id.as_ref().map(|tpl| tpl.resolve(ctx)).transpose()?;
        let reply_to_id = self.reply_to_id.as_ref().map(|tpl| tpl.resolve(ctx)).transpose()?;


        // 5) Build the ChannelMessage, pulling channel from the config:
        Ok(ChannelMessage {
            id,
            session_id,
            channel: self.channel_name.clone(),
            from,
            to,
            direction,
            content,
            thread_id,
            reply_to_id,
            ..Default::default()
        })
    }
}

/// Node kinds, flattened by top-level key
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum NodeKind {
    #[serde(rename_all = "camelCase")]
    Channel {
        #[serde(flatten)]
        cfg: ChannelNodeConfig,
    },
    #[serde(rename_all = "camelCase")]
    Tool {
        tool: ToolNodeConfig,
    },
    #[serde(rename_all = "camelCase")]
    Agent {
        #[serde(flatten)]
        agent: BuiltInAgent,
    },
    #[serde(rename_all = "camelCase")]
    Process {
        #[serde(flatten)]
        process: BuiltInProcess,
    },
}


/// A ToolNodeConfig allows to call a tool either by name, action
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
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolNodeConfig {
    pub name: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_map: Option<Mapper>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_map: Option<Mapper>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err_map: Option<Mapper>,
     #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_ok: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_err: Option<Vec<String>>,
}


/// A single node’s config in the flow
#[derive(Debug, Clone, JsonSchema)]
pub struct NodeConfig {
    #[serde(skip)]
    #[schemars(skip)]
    pub id: String,
    #[serde(flatten)]
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    #[serde(default = "NodeConfig::default_max_retries", skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<usize>,
    #[serde(default = "NodeConfig::default_retry_delay", skip_serializing_if = "Option::is_none")]
    pub retry_delay_secs: Option<u64>,
}

// matches the JSON shape of a node, minus the `id` field
#[derive(Deserialize, Serialize)]
struct RawNodeConfig {
    #[serde(flatten)]
    kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_retries: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry_delay_secs: Option<u64>,
}

impl NodeConfig {
    fn default_max_retries() -> Option<usize> { Some(3) }
    fn default_retry_delay() -> Option<u64> { Some(1) }

    pub fn new(id: impl Into<String>, kind: NodeKind, config: Option<Value>) -> Self {
        Self {
            id: id.into(),
            kind,
            config,
            max_retries: Self::default_max_retries(),
            retry_delay_secs: Self::default_retry_delay(),
        }
    }

    pub fn with_retry(mut self, max: usize, delay: u64) -> Self {
        self.max_retries = Some(max);
        self.retry_delay_secs = Some(delay);
        self
    }
}

/// Watches a directory of `.greentic` files
struct FlowWatcher {
    manager: Arc<FlowManager>,
}

#[async_trait]
impl WatchedType for FlowWatcher {
    fn is_relevant(&self, path: &Path) -> bool {
        path.extension().and_then(|e| e.to_str()) == Some("greentic")
    }
    async fn on_create_or_modify(&self, path: &Path) -> anyhow::Result<()> {
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let flow = FlowManager::load_flow_from_file(path.to_str().unwrap())?
            .build();
        self.manager.register_flow(&name, flow);
        Ok(())
    }
    async fn on_remove(&self, path: &Path) -> anyhow::Result<()> {
        info!("Flow file removed: {:?}", path);
        Ok(())
    }
}

pub type FlowAddedHandler = Arc<dyn Fn(&str, &Flow) + Send + Sync >;


/// Manages all loaded flows and dispatches messages
#[derive( Clone)]
pub struct FlowManager {
    flows: DashMap<String, Flow>,
    store: Arc<dyn StateStore>,
    executor: Arc<Executor>,
    channel_manager: Arc<ChannelManager>,
    process_manager: Arc<ProcessManager>,
    secrets: SecretsManager,
    on_added: Arc<Mutex<Vec<FlowAddedHandler>>>,
}

impl FlowManager {
    pub fn new(store: Arc<dyn StateStore>, executor: Arc<Executor>, channel_manager: Arc<ChannelManager>, process_manager: Arc<ProcessManager>, secrets: SecretsManager) -> Arc<Self> {
        let on_added = Arc::new(Mutex::new(Vec::new()));
        Arc::new(FlowManager { flows: DashMap::new(), store, executor, channel_manager, process_manager, secrets, on_added})
    }

    pub async fn subscribe_flow_added(&self, h: FlowAddedHandler) {
        let mut guard = self.on_added.lock().await;
        guard.push(h);
    }

    pub fn register_flow(&self, name: &str, flow: Flow) {
        // insert into the map
        self.flows.insert(name.to_string(), flow.clone());

        info!("Registered flow: {}", name);

        // fire the callbacks without blocking this thread
        let me = self.clone();
        let name = name.to_string();
        let flow = flow.clone();
        tokio::spawn(async move {
            let handlers = me.on_added.lock().await;
            for h in handlers.iter() {
                h(&name, &flow);
            }
        });
    }

    pub fn flows(&self) -> DashMap<String, Flow>{
        self.flows.clone()
    }

    pub fn remove_flow(&self, name: &str) {
        self.flows.remove(name);
        info!("Removed flow: {}", name);
    }

    pub async fn watch_flow_dir(self: Arc<Self>, dir: PathBuf) -> Result<DirectoryWatcher,Error> {
        let watcher = FlowWatcher { manager: self.clone() };
        DirectoryWatcher::new(dir, Arc::new(watcher), &["jgtc","ygtc"], true).await
    }

    pub fn load_flow_from_file(path: &str) -> Result<Flow, FlowError> {
        let contents = fs::read_to_string(path)
            .map_err(|e| FlowError::IoError(format!("read error: {}", e)))?;
        let ext = Path::new(path)
            .extension()
            .and_then(|os| os.to_str())
            .unwrap_or_default()
            .to_lowercase();

        let flow: Flow = match ext.as_str() {
            "jgtc" => {
                // JSON‐based flow file
                serde_json::from_str(&contents)
                    .map_err(|e| FlowError::SerializationError(format!("JSON parse error: {}", e)))?
            }
            "ygtc" => {
                // YAML‐based flow file
                serde_yaml::from_str(&contents)
                    .map_err(|e| FlowError::SerializationError(format!("YAML parse error: {}", e)))?
            }
            other => {
                return Err(FlowError::SerializationError(format!(
                    "unsupported extension “{}” (expected .jgtc or .ygtc)",
                    other
                )));
            }
        };

        Ok(flow.build())
    }

    pub fn save_flow_to_file(path: &str, flow: &Flow) -> Result<(), FlowError> {
        let contents = {
            let ext = Path::new(path)
                .extension()
                .and_then(|os| os.to_str())
                .unwrap_or_default()
                .to_lowercase();

            match ext.as_str() {
                "jgtc" => {
                    // JSON output
                    serde_json::to_string_pretty(flow)
                        .map_err(|e| FlowError::SerializationError(format!("{}", e)))?
                }
                "ygtc" => {
                    // YAML output
                    serde_yaml::to_string(flow)
                        .map_err(|e| FlowError::SerializationError(format!("{}", e)))?
                }
                other => {
                    return Err(FlowError::SerializationError(format!(
                        "unsupported extension “{}” (expected .jgtc or .ygtc)",
                        other
                    )));
                }
            }
        };

        fs::write(path, contents).map_err(|e| FlowError::IoError(format!("{}", e)))?;
        Ok(())
    }

    #[tracing::instrument(skip(self, message))]
    pub async fn process_message(
        &self,
        flow_name: &str,
        node_id: &str,
        message: Message,
        channel_origin: Option<ChannelOrigin>,
    ) -> Option<ExecutionReport> {
        let flow = self.flows.get(flow_name)?;


        let mut ctx = NodeContext::new(
            self.store.load().await,
            HashMap::new(),
            self.executor.clone(),
            self.channel_manager.clone(),
            self.process_manager.clone(),
            self.secrets.clone(),
            channel_origin.clone(),
        );

        let report = flow.run(message, node_id, &mut ctx).await;
        self.store.save(&ctx.get_all_state()).await;
        Some(report)
    }

    pub async fn load_all_flows_from_dir(&self, dir: &Path) -> anyhow::Result<()> {
        if !dir.exists() {
            fs::create_dir_all(dir)?;
        }
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("greentic") {
                let name = path.file_stem().unwrap().to_string_lossy();

                match Self::load_flow_from_file(&path.to_string_lossy()) {
                    Ok(flow) => self.register_flow(&name, flow.build()),
                    Err(e) => error!("Failed to load {}: {}", name, e),
                }
            }
        }
        Ok(())
    }

    pub async fn shutdown_all(&self) {
        let count = self.flows.len();
        self.flows.clear();
        info!("Shut down {} flows", count);
    }
}

#[derive(Debug, Clone)]
pub enum FlowError {
    IoError(String),
    SerializationError(String),
}

impl fmt::Display for FlowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlowError::IoError(msg) => write!(f, "I/O error: {}", msg),
            FlowError::SerializationError(msg) => write!(f, "JSON error: {}", msg),
        }
    }
}

impl std::error::Error for FlowError {}



/// Possible errors when resolving a `ValueOrTemplate`.
#[derive(Debug, Error)]
pub enum ResolveError {
    #[error("template rendering failed: {0}")]
    Template(String),

    #[error("failed to parse rendered value into target type: {0}")]
    Parse(#[from] serde_json::Error),
}


impl<T> ValueOrTemplate<T>
where
    T: DeserializeOwned + Clone,
{
    /// Resolve this into a concrete `T`:
    /// - If `self` is `Value(v)`, just clone and return it.
    /// - If `self` is `Template(s)`, render `s` via `ctx`, then `serde_json::from_str` into `T`.
    pub fn resolve<C>(&self, ctx: &C) -> Result<T, ResolveError>
    where
        C: TemplateContext,
    {
        match self {
            ValueOrTemplate::Value(v) => Ok(v.clone()),
            ValueOrTemplate::Template(tmpl) => {
                // 1) Render the template to a raw JSON string
                let rendered = ctx
                    .render_template(tmpl)
                    .map_err(ResolveError::Template)?;

                // 2) Parse into the target type T
                let parsed: T = serde_json::from_str(&rendered)?;
                Ok(parsed)
            }
        }
    }
}

/// An example trait your `NodeContext` should implement (or adapt to whatever you have).
pub trait TemplateContext {
    /// Render the template with the context’s data, producing a JSON string.
    fn render_template(&self, template: &str) -> Result<String, String>;
}