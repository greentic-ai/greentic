use async_trait::async_trait;
use dashmap::DashMap;
use ollama_rs::generation::embeddings::request::{EmbeddingsInput, GenerateEmbeddingsRequest};
use ollama_rs::models::ModelOptions;
use ollama_rs::generation::completion::request::GenerationRequest;
use ollama_rs::generation::chat::{ChatMessage, request::ChatMessageRequest};
use ollama_rs::Ollama;
use schemars::{schema::RootSchema, schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use url::Url;

use crate::{
    message::Message,
    node::{NodeContext, NodeErr, NodeError, NodeOut, NodeType, ToolNode},
};

// --------------------------------------------------------------------------------
// 1) Define OllamaAgent, which wraps ollama-rs usage
// --------------------------------------------------------------------------------

/// `OllamaAgent` invokes a local Ollama server via `ollama_rs`.  
/// It supports plain generation (`Generate`), embeddings (`Embed`), chat mode (`Chat`), and tool calls (`ToolCall`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct OllamaAgent {
    /// The Ollama model to invoke (e.g. "llama2:latest", "vicuna:v1", etc.).
    pub model: String,

    /// If not set the default localhost with port 11434 will be used
    pub ollama_host: Option<Url>,
    pub ollama_port: Option<u16>,

    /// Set optional options for the model
    #[schemars(default)]
    pub model_options: Option<ModelOptions>,

    /// Map of `tool.name -> ToolNode` for any external tool calls (`ToolCall` variant).
    /// This field is not serialized in JSON nor included in the schema.
    #[schemars(skip)]
    #[serde(skip)]
    pub tool_nodes: Option<DashMap<String, Box<ToolNode>>>,
}

impl OllamaAgent {
    /// Construct a new `OllamaAgent` with explicit fields.
    pub fn new(
        model: impl Into<String>,
        ollama_host: Option<Url>,
        ollama_port: Option<u16>,
        model_options: Option<ModelOptions>,
        tool_nodes: Option<DashMap<String, Box<ToolNode>>>,
    ) -> Self {
        OllamaAgent {
            model: model.into(),
            ollama_host,
            ollama_port,
            model_options,
            tool_nodes,
        }
    }
}

#[async_trait]
#[typetag::serde]
impl NodeType for OllamaAgent {
    fn type_name(&self) -> String {
        "ollama".to_string()
    }

    fn schema(&self) -> RootSchema {
        schema_for!(OllamaAgent)
    }

    #[tracing::instrument(name = "ollama_agent_node_process", skip(self, context))]
    async fn process(
        &self,
        input: Message,
        context: &mut NodeContext,
    ) -> Result<NodeOut, NodeErr> {
        // 1) Parse the incoming payload as an `OllamaRequest`
        let req: OllamaRequest = serde_json::from_value(input.payload().clone())
            .map_err(|e| NodeErr::all(NodeError::ExecutionFailed(format!(
                "Invalid OllamaRequest JSON: {}",
                e
            ))))?;

        // 2) Delegate to our handler
        let result_json = handle_request(req, self, context)
            .await
            .map_err(|e| NodeErr::all(NodeError::ExecutionFailed(format!(
                "OllamaRequest handling failed: {}",
                e
            ))))?;

        // 3) Wrap into a new `Message` and return
        let out_msg = Message::new(&input.id(), result_json, input.session_id().clone());
        Ok(NodeOut::all(out_msg))
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

/// Each kind of Ollama request—tool call, embed, chat, or generate.
#[derive(Serialize, Deserialize, Debug, Clone, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum OllamaRequest {
    /// 1) Call an external “tool” (e.g. HTTP fetch, calculator, etc.).
    ///    The `tool_name` identifies which `ToolNode` to invoke, and `input` is arbitrary JSON.
    ToolCall {
        tool_node: ToolNode,
        input: JsonValue,
        msg: Message,
    },

    /// 2) Generate embeddings for a given text.
    ///    `model` is the embedding model identifier; `text` is the raw string.
    Embed {
        model: String,
        text: String,
    },

    /// 3) Engage in “chat mode”: a sequence of role-tagged messages.
    ///    `history` is a Vec<ChatMessage>.
    Chat {
        model: String,
        history: Vec<ChatMessage>,
        model_options: Option<ModelOptions>,
    },

    /// 4) Plain generation from a single prompt.
    Generate {
        model: String,
        prompt: String,
        model_options: Option<ModelOptions>,
    },
}


/// Given an `OllamaRequest`, perform the appropriate call via `ollama_rs` and return the JSON result.
async fn handle_request(
    req: OllamaRequest,
    agent: &OllamaAgent,
    context: &mut NodeContext,
) -> Result<JsonValue, anyhow::Error> {
    let mut client = if agent.ollama_host.is_some() && agent.ollama_port.is_some() {
        // You might want to clone or take references instead of unwrap directly:
        let host = agent.ollama_host.as_ref().unwrap().clone();
        let port = agent.ollama_port.unwrap();
        Ollama::new(host, port)
    } else {
        Ollama::default()
    };
    
    match req {
        OllamaRequest::ToolCall { tool_node, input, msg } => {
            // Wrap `input` + `msg.session_id()` into a dummy Message and invoke the ToolNode
            let dummy = Message::new("tool_call_msg", input.clone(), msg.session_id().clone());
            match tool_node.process(dummy, context).await {
                Ok(node_out) => Ok(node_out.message().payload().clone()),
                Err(err) => Err(anyhow::anyhow!(
                    "Tool `{}` failed (session {:?}): {:?}",
                    tool_node.name(),
                    msg.session_id(),
                    err
                )),
            }
        }

        OllamaRequest::Embed { model, text } => {
            let req = GenerateEmbeddingsRequest::new(model.clone(), EmbeddingsInput::Single(text));
            let resp = client.generate_embeddings(req).await
                .map_err(|e| anyhow::anyhow!("Embedding error: {}", e))?;
            // Response is something like { "embedding": Vec<f32> }
            Ok(json!({ "embedding": resp.embeddings }))
        }

        OllamaRequest::Chat {
            model,
            history,
            model_options,
        } => {
            // Convert our `ChatMessage` → `ors::ChatMessage` (role + content)
            let mut ors_history: Vec<ChatMessage> = Vec::with_capacity(history.len());
            for msg in history.into_iter() {
                ors_history.push(ChatMessage::new(msg.role, msg.content.clone()));
            }

            let req = match model_options {
                Some(opts) => {
                    ChatMessageRequest::new(model.clone(), ors_history).options(opts)
                },
                None => {
                    ChatMessageRequest::new(model.clone(), ors_history)
                },
            };
            
            let resp = client.send_chat_messages_with_history(&mut vec![], req).await
                .map_err(|e| anyhow::anyhow!("Chat API error: {}", e))?;

            let reply_text = resp.message.content.clone();
            Ok(json!({ "reply": reply_text }))
        }

        OllamaRequest::Generate {
            model,
            prompt,
            model_options,
        } => {
            let gen_req = match model_options {
                Some(opts) => {
                    GenerationRequest::new(model.clone(), prompt.clone()).options(opts)
                },
                None => {
                    GenerationRequest::new(model.clone(), prompt.clone())
                },
            };
            let resp = client.generate(gen_req).await
                .map_err(|e| anyhow::anyhow!("Completion error: {}", e))?;

            Ok(json!({ "generated_text": resp.response }))
        }
    }
}


// Add these tests at the bottom of your `src/agents.rs` (or in a separate `tests/` file).

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use ollama_rs::generation::chat::MessageRole;
    use schemars::schema::SchemaObject;
    use serde_json::json;

    /// Helper: extract a subschema object by key from a `RootSchema`.
    fn get_definition<'a>(root: &'a RootSchema, name: &str) -> Option<&'a SchemaObject> {
        root.definitions.get(name).and_then(|s| {
            if let schemars::schema::Schema::Object(obj) = s {
                Some(obj)
            } else {
                None
            }
        })
    }

    #[test]
    fn ollamaagent_serde_roundtrip_and_schema() {
        // 1) Create a sample OllamaAgent
        let agent = OllamaAgent::new(
            "llama2:latest",
            Some(Url::parse("http://localhost").unwrap()),
            Some(11434),
            Some(ModelOptions::default().temperature(0.5).max_tokens(128)),
            None,
        );

        // 2) Serialize to JSON and back
        let serialized = serde_json::to_string_pretty(&agent).expect("serialize failed");
        let deserialized: OllamaAgent =
            serde_json::from_str(&serialized).expect("deserialize failed");
        assert_eq!(deserialized.model, "llama2:latest");
        assert_eq!(deserialized.ollama_port, Some(11434));
        assert_eq!(deserialized.ollama_host.unwrap().as_str(), "http://localhost/");

        // 3) Generate the JSON-Schema for OllamaAgent
        let schema = schema_for!(OllamaAgent);
        // There should be a definition named "OllamaAgent"
        let def = get_definition(&schema, "OllamaAgent")
            .expect("OllamaAgent definition missing");
        // The "properties" map inside should contain "model", "ollama_host", "ollama_port"
        let props = def.properties.as_ref().expect("no properties in schema");
        assert!(props.contains_key("model"));
        assert!(props.contains_key("ollama_host"));
        assert!(props.contains_key("ollama_port"));
        // "model" must be of type string
        if let Some(schemars::schema::Schema::Object(obj)) = props.get("model") {
            let ty = obj
                .instance_type
                .as_ref()
                .and_then(|t| t.as_ref().first().cloned());
            assert_eq!(ty, Some(schemars::schema::InstanceType::String));
        } else {
            panic!("`model` schema is not an object");
        }
    }

    #[test]
    fn ollamarequest_deserialize_variants() {
        // ToolCall variant
        let json_toolcall = json!({
            "mode": "tool_call",
            "tool_node": {
                // Minimal ToolNode stub: only `name` and `action` matter for deserialization
                "name": "dummy_tool",
                "action": "do_something"
            },
            "input": { "foo": 42 },
            "msg": { "id": "m1", "payload": {"bar": "baz"}, "session_id": null }
        });
        let req_tool: OllamaRequest =
            serde_json::from_value(json_toolcall.clone()).expect("ToolCall JSON failed");
        match req_tool {
            OllamaRequest::ToolCall { input, msg, mut tool_node } => {
                assert_eq!(input, json!({ "foo": 42 }));
                // message roundtrip
                assert_eq!(msg.id(), "m1");
                assert_eq!(tool_node.name(), "dummy_tool");
            }
            _ => panic!("Expected ToolCall variant"),
        }

        // Embed variant
        let json_embed = json!({
            "mode": "embed",
            "model": "llama2:latest",
            "text": "Hello, world!"
        });
        let req_embed: OllamaRequest =
            serde_json::from_value(json_embed.clone()).expect("Embed JSON failed");
        match req_embed {
            OllamaRequest::Embed { model, text } => {
                assert_eq!(model, "llama2:latest");
                assert_eq!(text, "Hello, world!");
            }
            _ => panic!("Expected Embed variant"),
        }

        // Chat variant
        let json_chat = json!({
            "mode": "chat",
            "model": "llama2:latest",
            "history": [
                { "role": "user", "content": "Hi" },
                { "role": "assistant", "content": "Hello" }
            ],
            "model_options": {
                "temperature": 0.3,
                "max_tokens": 64
            }
        });
        let req_chat: OllamaRequest =
            serde_json::from_value(json_chat.clone()).expect("Chat JSON failed");
        match req_chat {
            OllamaRequest::Chat {
                model,
                history,
                model_options,
            } => {
                assert_eq!(model, "llama2:latest");
                assert_eq!(history.len(), 2);
                assert_eq!(history[0].role, MessageRole::User);
                assert_eq!(history[1].role, MessageRole::Assistant);
            }
            _ => panic!("Expected Chat variant"),
        }

        // Generate variant
        let json_gen = json!({
            "mode": "generate",
            "model": "llama2:latest",
            "prompt": "Why is the sky blue?",
            "model_options": null
        });
        let req_gen: OllamaRequest =
            serde_json::from_value(json_gen.clone()).expect("Generate JSON failed");
        match req_gen {
            OllamaRequest::Generate {
                model,
                prompt,
                model_options,
            } => {
                assert_eq!(model, "llama2:latest");
                assert_eq!(prompt, "Why is the sky blue?");
                assert!(model_options.is_none());
            }
            _ => panic!("Expected Generate variant"),
        }
    }

    #[tokio::test]
    async fn handle_request_invalid_json_error() {
        // We give a bogus JSON so that `process()` will error out early.
        // Use a dummy OllamaAgent (host/port irrelevant here).
        let agent = OllamaAgent::new(
            "llama2:latest",
            None,
            None,
            None,
            None,
        );

        // Create a dummy NodeContext (not used in this test).
        let mut ctx = NodeContext::new(/* state */ Default::default(),
                /* config */ Default::default(),
                /* executor */ Arc::new(crate::executor::Executor::dummy()),
                /* channel_mgr */ Arc::new(crate::channel::manager::ChannelManager::dummy()),
                /* secrets */ crate::secret::SecretsManager::dummy(),
                /* channel_origin */ None);

        // Here, we call `handle_request` directly with an impossible variant. For example:
        // Since `handle_request` takes an `OllamaRequest`, there is no invalid-mode here.
        // Instead, we can simulate a request that will cause the client to fail, e.g.:
        // a) Chat without a reachable server. We expect an error containing "Chat API error".
        let bad_req = OllamaRequest::Chat {
            model: "nonexistent-model".to_string(),
            history: vec![ChatMessage { role: MessageRole::User, content: "Hello".into(), tool_calls: None, images: None }],
            model_options: None,
        };

        let result = handle_request(bad_req, &agent, &mut ctx).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        // We expect the error to mention "Chat API error"
        assert!(
            err_msg.contains("Chat API error"),
            "Unexpected error: {}",
            err_msg
        );
    }
}
