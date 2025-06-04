use async_trait::async_trait;
use dashmap::DashMap;
use ollama_rs::generation::embeddings::request::{EmbeddingsInput, GenerateEmbeddingsRequest};
use ollama_rs::models::ModelOptions;
use ollama_rs::generation::completion::request::GenerationRequest;
use ollama_rs::generation::chat::{ChatMessage, request::ChatMessageRequest};
use ollama_rs::Ollama;
use schemars::{schema::RootSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use tracing::error;
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaAgent {
    /// The Ollama model to invoke (e.g. "llama2:latest", "vicuna:v1", etc.).
    #[serde(default)]
    pub model: Option<String>,

    /// If not set the default localhost with port 11434 will be used
    #[serde(default)]
    pub ollama_host: Option<Url>,
    #[serde(default)]
    pub ollama_port: Option<u16>,

    /// Set optional options for the model
    #[serde(default)]
    pub model_options: Option<ModelOptions>,

    /// Map of `tool.name -> ToolNode` for any external tool calls (`ToolCall` variant).
    /// This field is not serialized in JSON nor included in the schema.
    #[serde(skip)]
    pub tool_nodes: Option<DashMap<String, Box<ToolNode>>>,
}

impl OllamaAgent {
    /// Construct a new `OllamaAgent` with explicit fields.
    pub fn new(
        model: Option<String>,
        ollama_host: Option<Url>,
        ollama_port: Option<u16>,
        model_options: Option<ModelOptions>,
        tool_nodes: Option<DashMap<String, Box<ToolNode>>>,
    ) -> Self {
        OllamaAgent {
            model,
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
        handle_request(input, req, self, context).await
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

/// Each kind of Ollama request—tool call, embed, chat, or generate.
#[derive(Serialize, Deserialize, Debug, Clone)]
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
    msg: Message,
    req: OllamaRequest,
    agent: &OllamaAgent,
    context: &mut NodeContext,
) -> Result<NodeOut, NodeErr> {
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
            tool_node.process(dummy, context).await
        }

        OllamaRequest::Embed { model, text } => {
            let req = GenerateEmbeddingsRequest::new(model.clone(), EmbeddingsInput::Single(text));
            let resp = client.generate_embeddings(req).await;
            match resp {
                Ok(resp) => {
                    let json = json!({"embeddings":resp.embeddings});
                    let msg = Message::new(&msg.id(), json, msg.session_id());
                    Ok(NodeOut::all(msg))
                },
                Err(error) => {
                    let error = format!("Could not generate embedding with ollama. Got error: {}",error);
                    error!(error);
                    Err(NodeErr::all(NodeError::ExecutionFailed(error)))
                },
            }
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
            
            let resp = client.send_chat_messages_with_history(&mut vec![], req).await;
            match resp {
                Ok(resp) => {
                    let reply_text = resp.message.content.clone();
                    let json = json!({ "reply": reply_text });
                    let msg = Message::new(&msg.id(), json, msg.session_id());
                    Ok(NodeOut::all(msg))
                },
                Err(error) => {
                    let error = format!("Chat API error: {}",error);
                    error!(error);
                    Err(NodeErr::all(NodeError::ExecutionFailed(error)))
                },
            }
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
            let resp = client.generate(gen_req).await;

            match resp {
                Ok(resp) => {
                    let json = json!({ "generated_text": resp.response });
                    let msg = Message::new(&msg.id(), json, msg.session_id());
                    Ok(NodeOut::all(msg))
                },
                Err(error) => {
                    let error = format!("Completion error: {}",error);
                    error!(error);
                    Err(NodeErr::all(NodeError::ExecutionFailed(error)))
                },
            }
        }
    }
}



#[cfg(test)]
mod tests {
    use crate::{channel::manager::ChannelManager, executor::Executor, process::manager::ProcessManager, secret::{EmptySecretsManager, SecretsManager}};

    use super::*;
    use ollama_rs::generation::chat::MessageRole;
    use schemars::schema::SchemaObject;
    use serde_json::json;

    /// Helper: extract a subschema object by key from a `RootSchema`.
    /*fn get_definition<'a>(root: &'a RootSchema, name: &str) -> Option<&'a SchemaObject> {
        root.definitions.get(name).and_then(|s| {
            if let schemars::schema::Schema::Object(obj) = s {
                Some(obj)
            } else {
                None
            }
        })
    }*/

    #[test]
    fn ollamaagent_serde_roundtrip_and_schema() {
        // 1) Create a sample OllamaAgent
        let agent = OllamaAgent {
            model: Some("llama2:latest".to_string()),
            ollama_host: Some(Url::parse("http://localhost").unwrap()),
            ollama_port: Some(11434),
            model_options: Some(ModelOptions::default()),
            tool_nodes: None,
        };

        // 2) Round‐trip serialization
        let serialized = serde_json::to_string_pretty(&agent).expect("serialize failed");
        let deserialized: OllamaAgent =
            serde_json::from_str(&serialized).expect("deserialize failed");
        assert_eq!(deserialized.model, Some("llama2:latest".to_string()));
        assert_eq!(deserialized.ollama_port, Some(11434));
        assert_eq!(deserialized.ollama_host.unwrap().as_str(), "http://localhost/");

        // 3) Generate the JSON‐Schema for `OllamaAgent`
        let schema: RootSchema = schema_for!(OllamaAgent);
        // Look up the “OllamaAgent” definition in the `definitions` map
        let root_obj: &SchemaObject = &schema.schema;
        let props = root_obj
            .object
            .as_ref()
            .expect("root must be an object")
            .properties
            .clone();

        // Now you can assert that "model", "ollama_host", and "ollama_port" are present:
        assert!(props.contains_key("model"));
        assert!(props.contains_key("ollama_host"));
        assert!(props.contains_key("ollama_port"));

        // Check that “model” is a string:
        if let Some(schemars::schema::Schema::Object(model_schema_obj)) = props.get("model") {
            let ty = model_schema_obj
                .instance_type
                .as_ref()
                .and_then(|single_or_vec| {
                    // single_or_vec could be Single(InstanceType::String) or Vec([String,…])
                    if let schemars::schema::SingleOrVec::Single(boxed) = single_or_vec {
                        Some((**boxed).clone())
                    } else {
                        None
                    }
                });
            assert_eq!(ty, Some(schemars::schema::InstanceType::String));
        } else {
            panic!("`model` property was not an object‐schema");
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
            "msg": { "id": "m1", "payload": {"bar": "baz"} }
        });

        let req_tool: OllamaRequest =
            serde_json::from_value(json_toolcall.clone()).expect("ToolCall JSON failed");
        match req_tool {
            OllamaRequest::ToolCall { input, msg, tool_node } => {
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
                model_options: _model_options,
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
            Some("llama2:latest".to_string()),
            None,
            None,
            None,
            None,
        );

        // Create a dummy NodeContext (not used in this test).
        let mut ctx = NodeContext::new(/* state */ Default::default(),
                /* config */ Default::default(),
                /* executor */ Executor::dummy(),
                /* channel_mgr */ ChannelManager::dummy(),
                                /* process */ ProcessManager::dummy(),
                /* secrets */ SecretsManager(EmptySecretsManager::new()),
                /* channel_origin */ None);

        // Here, we call `handle_request` directly with an impossible variant. For example:
        // Since `handle_request` takes an `OllamaRequest`, there is no invalid-mode here.
        // Instead, we can simulate a request that will cause the client to fail, e.g.:
        // a) Chat without a reachable server. We expect an error containing "Chat API error".
        let bad_req = OllamaRequest::Chat {
            model: "nonexistent-model".to_string(),
            history: vec![ChatMessage { role: MessageRole::User, content: "Hello".into(), tool_calls: vec![], images: None }],
            model_options: None,
        };

        let msg = Message::new("test", json!({}), Some("123".to_string()));
        let result = handle_request(msg, bad_req, &agent, &mut ctx).await;
        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        // We expect the error to mention "Chat API error"
        assert!(
            err_msg.contains("Chat API error"),
            "Unexpected error: {}",
            err_msg
        );
    }
}
