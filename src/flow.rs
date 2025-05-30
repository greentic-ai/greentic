// src/flow.rs

use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
    sync::Arc, time::Duration,
};
use crate::{
    agent::AgentNode, channel::manager::ChannelManager, executor::Executor, mapper::Mapper, message::Message, node::{ChannelOrigin, NodeContext, NodeError, NodeType}, process::ProcessNode, secret::SecretsManager, state::StateStore, watcher::{watch_dir, WatchedType}
};
use anyhow::Error;
use channel_plugin::message::{ChannelMessage, MessageContent, MessageDirection, Participant};
use std::fmt::Debug;
use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use dashmap::DashMap;
use petgraph::{graph::NodeIndex, prelude::{StableDiGraph, StableGraph}, visit::{Topo, Walker}, Directed, Direction::Incoming};
use schemars::{schema::{InstanceType, Metadata, Schema, SchemaObject}, JsonSchema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json,Value};
use tokio::{sync::Mutex, task::JoinHandle, time::sleep};
use tracing::{error, info};
use crate::node::ToolNode;

/// One record per-node, successful or error
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct NodeRecord {
    pub node_id: String,
    pub attempt: usize,
    pub started: DateTime<Utc>,
    pub finished: DateTime<Utc>,
    pub result: Result<Message, NodeError>,
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
                            //let cm = get_message(cfg,&input);
                            let cm = ChannelMessage {
                                id:        input.id().to_string(),
                                session_id: input.session_id(),
                                channel:   cfg.channel_name.clone(),
                                direction: MessageDirection::Outgoing,
                                content:   Some(json_to_message_content(input.payload().clone())),
                                ..Default::default()
                            };
                            ctx.channel_manager()
                                .send_to_channel(&cfg.channel_name, cm)
                                .map(|_| input.clone())
                                .map_err(|e| NodeError::ConnectionFailed(format!("{:?}", e)))
                        } else {
                            Ok(input.clone())
                        }
                    }

                    NodeKind::Tool { tool } => {
                        // same as before…
                        let mut msg_with_params = input.clone();
                        if let Some(params) = &tool.parameters {
                            // Pull out the session ID and payload before we overwrite msg_with_params
                            let session_id = msg_with_params.session_id().clone();
                            let payload = msg_with_params.payload();

                            // Rebuild the message
                            msg_with_params = Message::new(
                                &msg_with_params.id(),
                                json!({
                                    "parameters": params,
                                    "payload": payload,
                                }),
                                session_id,
                            );
                        }
                        // fetch secrets…
                        let mut secret_map = HashMap::new();
                        for s in &tool.secrets {
                            if let Some(tok) = ctx.reveal_secret(s).await {
                                secret_map.insert(s.clone(), tok.clone());
                            }
                        }
                        ToolNode::new(
                            tool.name.clone(),
                            tool.action.clone(),
                            tool.parameters.clone(),
                            Some(secret_map),
                            tool.in_map.clone(),
                            tool.out_map.clone(),
                            tool.err_map.clone(),
                        ).process(msg_with_params, ctx)
                    }

                    NodeKind::Agent { agent } => {
                        AgentNode::new(agent.agent.clone(), agent.task.clone())
                            .process(input.clone(), ctx)
                    }

                    NodeKind::Process { process } => {
                        ProcessNode::new(process.name.clone(), process.args.clone())
                            .process(input.clone(), ctx)
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
                    outputs.insert(nx, out_msg);
                }
                Err(err) => {
                    early_err = Some((node_id.clone(), err));
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
/* 
fn get_message(cfg: &ChannelNodeConfig,input: &Message) -> ChannelMessage {


    
    let mut cm = ChannelMessage {
        id:        input.id().to_string(),
        channel:   cfg.channel_name.clone(),
        direction: MessageDirection::Outgoing,
        content:   Some(json_to_message_content(input.payload().clone())),
        ..Default::default()
    };

    // Prepare Handlebars + context once
    let mut hb = Handlebars::new();
    hb.register_helper(
        "json",
        Box::new(
            move |h: &Helper<'_>,
                _: &Handlebars<'_>,
                _: &Context,
                _: &mut RenderContext<'_,'_>,
                out: &mut dyn Output| -> HelperResult
            {
                // Grab the first positional parameter
                let param = h
                    .param(0)
                    .ok_or_else(|| RenderErrorReason::MissingVariable(Some("Helper `json` got no argument".to_string())))?;
                // Serialize it
                let s = serde_json::to_string(param.value())
                    .map_err(|err| RenderErrorReason::SerdeError(err))?;
                // Write it into the output buffer
                out.write(&s)?;
                Ok(())
            }
        ) as Box<dyn HelperDef + Send + Sync>
    );


    let ctx: Value = serde_json::to_value(&input.payload()).unwrap();

    println!("payload {:?}",to_string_pretty(&input.payload()));

    // 2) from
    if let Some(ref entry) = cfg.from {
        cm.from = match entry {
            ValueOrTemplate::Value(p)    => p.clone(),
            ValueOrTemplate::Template(t) => {
                let rendered = hb.render_template(t, &ctx).unwrap();
                serde_json::from_str(&rendered).unwrap()
            }
        };
    }

    // 3) to
    if let Some(ref list) = cfg.to {
        cm.to = list.iter().map(|entry| {
            match entry {
                ValueOrTemplate::Value(p)    => p.clone(),
                ValueOrTemplate::Template(t) => {
                    let rendered = hb.render_template(t, &ctx).unwrap();
                    Participant{id:serde_json::from_str(&rendered).unwrap(), display_name: None, channel_specific_id: None}
                }
            }
        }).collect();
    }

    // 4) content
    if let Some(ref entry) = cfg.content {
        cm.content = Some(match entry {
            ValueOrTemplate::Value(c)    => c.clone(),
            ValueOrTemplate::Template(t) => {
                // render to a JSON string like `"Hello"` or `{...}` then parse
                let rendered = hb.render_template(t, &ctx).unwrap();
                serde_json::from_str(&rendered).unwrap()
            }
        });
    }

    // 5) thread_id
    if let Some(ref entry) = cfg.thread_id {
        cm.thread_id = Some(match entry {
            ValueOrTemplate::Value(s)    => s.clone(),
            ValueOrTemplate::Template(t) => hb.render_template(t, &ctx).unwrap()
        });
    }

    // 6) reply_to_id
    if let Some(ref entry) = cfg.reply_to_id {
        cm.reply_to_id = Some(match entry {
            ValueOrTemplate::Value(s)    => s.clone(),
            ValueOrTemplate::Template(t) => hb.render_template(t, &ctx).unwrap()
        });
    }

    cm
}
*/
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ValueOrTemplate<T> {
    Value(T),
    Template(String),
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
        agent: AgentNodeConfig,
    },
    #[serde(rename_all = "camelCase")]
    Process {
        process: ProcessNodeConfig,
    },
}


/// Internal tool-node config
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolNodeConfig {
    pub name: String,
    pub action: String,
    /// literal parameters to the operation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
    /// secret *names* whose values we must fetch at runtime
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_map: Option<Mapper>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out_map: Option<Mapper>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err_map: Option<Mapper>,
}

/// Internal agent-node config
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentNodeConfig {
    pub agent: String,
    pub task: String,
}

/// Internal process-node config
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ProcessNodeConfig {
    pub name: String,
    pub args: Value,
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
    secrets: SecretsManager,
    on_added: Arc<Mutex<Vec<FlowAddedHandler>>>,
}

impl FlowManager {
    pub fn new(store: Arc<dyn StateStore>, executor: Arc<Executor>, channel_manager: Arc<ChannelManager>, secrets: SecretsManager) -> Arc<Self> {
        let on_added = Arc::new(Mutex::new(Vec::new()));
        Arc::new(FlowManager { flows: DashMap::new(), store, executor, channel_manager, secrets, on_added})
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

    pub async fn watch_flow_dir(self: Arc<Self>, dir: PathBuf) -> Result<JoinHandle<()>,Error> {
        let watcher = FlowWatcher { manager: self.clone() };
        watch_dir(dir, Arc::new(watcher), &["greentic"], true).await
    }

    pub fn load_flow_from_file(path: &str) -> Result<Flow, FlowError> {
        let json = fs::read_to_string(path)
            .map_err(|e| FlowError::IoError(format!("read error: {}", e)))?;
        let flow: Flow = serde_json::from_str(&json)
            .map_err(|e| FlowError::SerializationError(format!("parse error: {}", e)))?;
        Ok(flow.build())
    }

    pub fn save_flow_to_file(path: &str, flow: &Flow) -> Result<(), FlowError> {
        let json = serde_json::to_string_pretty(flow)
            .map_err(|e| FlowError::SerializationError(format!("{}", e)))?;
        fs::write(path, json).map_err(|e| FlowError::IoError(format!("{}", e)))?;
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
            self.secrets.clone(),
            channel_origin.clone(),
        );

        let report = flow.run(message, node_id, &mut ctx).await;
        self.store.save(&ctx.get_all()).await;
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
