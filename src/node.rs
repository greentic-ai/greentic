use std::{collections::HashMap, fmt, sync::Arc};
use channel_plugin::message::Participant;
use serde::{Deserialize,  Serialize};
use tempfile::TempDir;
use std::fs;
use std::fmt::Debug;
use std::path::PathBuf;
use serde_json::{json, Value};
use crate::{channel::{manager::ChannelManager, node::ChannelNode,}, executor::{exports::wasix::mcp::router::{Content, ResourceContents}, Executor}, mapper::Mapper, message::Message, secret::SecretsManager, state::StateValue, util::extension_from_mime};
use schemars::{schema::{RootSchema, Schema}, schema_for, JsonSchema, SchemaGenerator};
#[typetag::serde] 
pub trait NodeType: Send + Sync + Debug {
    fn type_name(&self) -> String;
    fn process(&self, msg: Message, ctx: &mut NodeContext) -> Result<Message, NodeError>;
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
    participant: Participant,
}

impl ChannelOrigin {
    pub fn new(channel: String, participant: Participant) -> Self
    {
        Self {channel,participant}
    }

    pub fn channel(&self) -> String {
        self.channel.clone()
    }

    pub fn participant(&self) -> Participant {
        self.participant.clone()
    }
}

#[warn(dead_code)]
#[derive(Clone,)]
pub struct NodeContext {
    state: HashMap<String, StateValue>,
    config: HashMap<String, String>,
    executor: Arc<Executor>,
    channel_manager: Arc<ChannelManager>,
    secrets: SecretsManager,
    channel_origin: Option<ChannelOrigin>,
}

impl NodeContext
{
    pub fn new(state: HashMap<String, StateValue>, config: HashMap<String, String>, executor: Arc<Executor>, channel_manager: Arc<ChannelManager>, secrets: SecretsManager, channel_origin: Option<ChannelOrigin> ) -> Self {
        Self { state, config, executor, channel_manager, secrets, channel_origin }
    }

    pub fn channel_origin(&self) -> Option<ChannelOrigin> {
        self.channel_origin.clone()
    }

    pub fn get(&self, key: &str) -> Option<&StateValue> {
        self.state.get(key)
    }

    pub fn get_all(&self) -> HashMap<String, StateValue> {
        self.state.clone()
    }

    pub fn set(&mut self, key: &str, value: StateValue) {
        self.state.insert(key.to_string(), value);
    }

    pub fn delete(&mut self, key: &str) {
        self.state.remove(key);
    }

    pub fn executor(&self) -> &Executor {
        self.executor.as_ref()
    }

    pub fn channel_manager(&self) -> &ChannelManager {
        &self.channel_manager.as_ref()
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


#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum NodeError {
    NotFound,
    InvalidInput(String),
    ExecutionFailed(String),
    ConnectionFailed(String),
    Internal(String),
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeError::NotFound => write!(f, "Node not found"),
            NodeError::InvalidInput(msg) => write!(f, "Invalid input: {}", msg),
            NodeError::ExecutionFailed(msg) => write!(f, "Processing error: {}",msg),
            NodeError::ConnectionFailed(msg) => write!(f, "Failed to connect to node: {}",msg),
            NodeError::Internal(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for NodeError {}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "tool")]
pub struct ToolNode {
    name: String,
    action: String,
    parameters: Option<Value>,
    secrets: Option<HashMap<String, String>>,
    in_map: Option<Mapper>,
    out_map: Option<Mapper>,
    err_map: Option<Mapper>,
}

impl ToolNode {
    pub fn new(
            name: String,
            action: String,
            parameters: Option<Value>,
            secrets: Option<HashMap<String, String>>,
            in_map: Option<Mapper>,
            out_map: Option<Mapper>,
            err_map: Option<Mapper>,
        ) -> Self {
        Self {
            name,
            action,
            parameters,
            secrets,
            in_map,
            out_map,
            err_map,
        }
    }
    /// Supply static parameters to the tool.
    pub fn with_parameters(mut self, params: Value) -> Self {
        self.parameters = Some(params);
        self
    }

    /// Supply secret keys to the tool; these will be fetched from the SecretsManager.
    pub fn with_secrets(mut self, secrets: HashMap<String, String>) -> Self {
        self.secrets = Some(secrets);
        self
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

}
impl Clone for ToolNode {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            parameters: self.parameters.clone(),
            secrets: self.secrets.clone(),
            action: self.action.clone(),
            in_map: self.in_map.clone(),
            out_map: self.out_map.clone(),
            err_map: self.err_map.clone(),
        }
    }
}

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
    fn process(&self, input: Message, context: &mut NodeContext) -> Result<Message, NodeError> {


        let executor = context.executor();

        // apply the in_map if available
        let effective_payload = if self.use_in_map() {
            self.in_map
                .as_ref()
                .unwrap()
                .apply_input(&input.payload(), &context.config, &context.state)
        } else {
            input.payload()
        };

        let result = executor.call_tool(
            self.name.clone(),
            self.action.clone(),
            effective_payload,
        );

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
                                .map_err(|e| NodeError::ExecutionFailed(format!("Failed to write image: {}", e)))?;
                            outputs.push(serde_json::json!({ "image": filename.to_string_lossy() }));
                        }
                        Content::Embedded(embedded) => {
                            match &embedded.resource_contents {
                                ResourceContents::Blob(blob) => {
                                    let storage_dir = resolve_or_create_storage_dir(&context.config)?;
                                    let filename = storage_dir.join(format!("blob_{}.{}", i, extension_from_mime(blob.mime_type.as_deref().unwrap_or("bin"))));
                                    fs::write(&filename, &blob.blob)
                                        .map_err(|e| NodeError::ExecutionFailed(format!("Failed to write blob: {}", e)))?;
                                    outputs.push(serde_json::json!({ "blob": filename.to_string_lossy() }));
                                }
                                ResourceContents::Text(text) => {
                                    let storage_dir = resolve_or_create_storage_dir(&context.config)?;
                                    let filename = storage_dir.join(format!("text_{}.txt", i));
                                    fs::write(&filename, &text.text)
                                        .map_err(|e| NodeError::ExecutionFailed(format!("Failed to write text: {}", e)))?;
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
                        Some(self.err_map.as_ref().unwrap().apply_result(&output_json, &context.config, &context.state))
                    } else {
                        None
                    }
                } else {
                    if self.use_out_map() {
                        Some(self.out_map.as_ref().unwrap().apply_result(&output_json, &context.config, &context.state))
                    } else {
                        None
                    }
                };

                if let Some(mapper_output) = mapped {
                    for (k, v) in mapper_output.state_updates {
                        context.state.insert(k, v);
                    }
                    for (k, v) in mapper_output.config_updates {
                        context.config.insert(k, v);
                    }
                    Ok(Message::new(&input.id(), mapper_output.payload,input.session_id()))
                } else {
                    Ok(Message::new(&input.id(), output_json,input.session_id()))
                }
            }
            Err(e) => Err(NodeError::ExecutionFailed(format!("Tool call failed: {:?}", e))),
        }
    }
    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

fn resolve_or_create_storage_dir(
    config: &HashMap<String, String>,
) -> Result<PathBuf, NodeError> {
    if let Some(dir_str) = config.get("node_storage_dir") {
        let path = PathBuf::from(dir_str);
        if !path.exists() {
            fs::create_dir_all(&path)
                .map_err(|e| NodeError::ExecutionFailed(format!("Failed to create node_storage_dir: {}", e)))?;
        }
        Ok(path)
    } else {
        let tempdir = TempDir::new()
            .map_err(|e| NodeError::ExecutionFailed(format!("Failed to create tempdir: {}", e)))?;
        let path = tempdir.path().to_path_buf();
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::manager::HostLogger;
    use crate::config::{ConfigManager, MapConfigManager};
    use crate::logger::{Logger, OpenTelemetryLogger};
    use crate::message::Message;
    use crate::secret::{EmptySecretsManager, SecretsManager};
    use crate::state::{StateValue};
    use serde_json::json;
    use tempfile::TempDir;

    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct EchoNode;

    impl Clone for EchoNode {
        fn clone(&self) -> Self {
            EchoNode
        }
    }

    #[typetag::serde]
    impl NodeType for EchoNode {
        fn type_name(&self) -> String {
            "echo".to_string()
        }

        fn schema(&self) -> RootSchema {
            // this is object-safe because it doesn't mention `Self: Sized`
            schema_for!(EchoNode)
        }

        fn process(&self, input: Message, context: &mut NodeContext) -> Result<Message, NodeError> {
            context.set("echoed", StateValue::String("yes".to_string()));
            Ok(input)
        }

        fn clone_box(&self) -> Box<dyn NodeType> {
            Box::new(self.clone())
        }
    }

    #[tokio::test]
    async fn test_node_context_get_set_delete() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = HashMap::new();
        config.insert("node_storage_dir".into(), temp_dir.path().to_string_lossy().to_string());

        let secrets =SecretsManager(EmptySecretsManager::new());
        let logging = Logger(Box::new(OpenTelemetryLogger::new()));
        let executor = Executor::new(secrets.clone(),logging);
        let config_mgr = ConfigManager(MapConfigManager::new());
        let host_logger = HostLogger::new();
        let channel_manager = ChannelManager::new(config_mgr, secrets.clone(), host_logger).await.expect("could not create channel manager");
        let mut ctx = NodeContext::new(HashMap::new(), config, executor,channel_manager, secrets, None);
        assert!(ctx.get("missing").is_none());

        ctx.set("key", StateValue::String("value".to_string()));
        assert_eq!(ctx.get("key"), Some(&StateValue::String("value".to_string())));

        ctx.delete("key");
        assert!(ctx.get("key").is_none());
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


    #[tokio::test]
    async fn test_tool_node_with_text_output() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = HashMap::new();
        config.insert("node_storage_dir".into(), temp_dir.path().to_string_lossy().to_string());

        let secrets =SecretsManager(EmptySecretsManager::new());
        let logging = Logger(Box::new(OpenTelemetryLogger::new()));
        let executor = Executor::new(secrets.clone(),logging);
        let config_mgr = ConfigManager(MapConfigManager::new());
        let host_logger = HostLogger::new();
        let channel_manager = ChannelManager::new(config_mgr, secrets.clone(), host_logger).await.expect("could not create channel manager");
        let mut context = NodeContext::new(HashMap::new(), config, executor, channel_manager, secrets, None);

        let node = ToolNode::new("mock_tool".to_string(), "text_output".to_string(),None, None, None, None, None);
        let msg = Message::new("msg1", json!({"input": "Hello"}),None);

        let result = node.process(msg.clone(), &mut context).unwrap();
        let output = result.payload();

        assert!(output.is_array());
        let arr = output.as_array().unwrap();
        assert!(arr.iter().all(|v| v.is_object()));
    }

    #[tokio::test]
    async fn test_tool_node_saves_binary_and_text() {
        let temp_dir = TempDir::new().unwrap();
        let mut config = HashMap::new();
        config.insert("node_storage_dir".into(), temp_dir.path().to_string_lossy().to_string());

        let secrets =SecretsManager(EmptySecretsManager::new());
        let logging = Logger(Box::new(OpenTelemetryLogger::new()));
        let executor = Executor::new(secrets.clone(),logging);
        let config_mgr = ConfigManager(MapConfigManager::new());
        let host_logger = HostLogger::new();
        let channel_manager = ChannelManager::new(config_mgr, secrets.clone(), host_logger).await.expect("could not create channel manager");
        let mut context = NodeContext::new(HashMap::new(), config, executor, channel_manager, secrets, None);

        let node = ToolNode::new("mock_tool".to_string(), "file_output".to_string(), None, None, None, None, None);
        let msg = Message::new("msg2", json!({"input": "data"}),None);

        let result = node.process(msg.clone(), &mut context).unwrap();
        let output = result.payload();
        assert!(output.is_array());

        for item in output.as_array().unwrap() {
            let obj = item.as_object().expect("Expected object in array");
            let path = obj.values().next().unwrap().as_str().unwrap();
            assert!(
                std::path::Path::new(path).exists(),
                "Expected file path to exist: {}",
                path
            );
        }
    }

}

