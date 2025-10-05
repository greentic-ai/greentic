use crate::agent::agent_reply_schema::{AgentReply, parse_state_value};
use crate::node::Routing;
use crate::{
    message::Message,
    node::{NodeContext, NodeErr, NodeError, NodeOut, NodeType, ToolNode},
};
use async_trait::async_trait;
use dashmap::DashMap;
use ollama_rs::Ollama;
use ollama_rs::generation::chat::{ChatMessage, request::ChatMessageRequest};
use ollama_rs::generation::completion::request::GenerationRequest;
use ollama_rs::generation::embeddings::request::{EmbeddingsInput, GenerateEmbeddingsRequest};
use ollama_rs::generation::parameters::{FormatType, JsonStructure};
use ollama_rs::models::ModelOptions;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use tracing::{error, info};
use url::Url;

// --------------------------------------------------------------------------------
// 1) Define OllamaAgent, which wraps ollama-rs usage
// --------------------------------------------------------------------------------

/// `OllamaAgent` invokes a local Ollama server via `ollama_rs`.  
/// If OLLAMA_URL is set in config then another url is used.
/// If OLLAMA_KEY is used then a Bearer token is used and https is assumed.
/// It supports plain generation (`Generate`), embeddings (`Embed`), chat mode (`Chat`) and tool calls (`ToolCall`).

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OllamaAgent {
    pub task: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<OllamaMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ollama_url: Option<url::Url>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_options: Option<ollama_rs::models::ModelOptions>,

    /// If no tools are included then tool calling will not be possible
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_names: Option<Vec<String>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_payload: Option<bool>,

    /// Map of `tool.name -> ToolNode` for any external tool calls (`ToolCall` variant).
    /// This field is not serialized in JSON nor included in the schema.
    #[serde(skip)]
    pub tool_nodes: Option<DashMap<String, Box<ToolNode>>>,
}

impl OllamaAgent {
    fn build_ollama_client(&self, context: &NodeContext) -> Ollama {
        let chosen_url: Option<Url> = self.ollama_url.clone().or_else(|| {
            // Works whether get_config returns Option<String> or Option<&str>
            context
                .get_config("OLLAMA_URL")
                .as_deref() // Option<&str>
                .and_then(|s| Url::parse(s).ok())
        });

        let mut headers = HeaderMap::new();
        if let Some(key) = context.get_config("OLLAMA_KEY") {
            let val = format!("Bearer {}", key);
            if let Ok(hv) = HeaderValue::from_str(&val) {
                headers.insert(AUTHORIZATION, hv);
            }
        }

        match chosen_url {
            Some(url) => {
                // If the URL has a port, use it. Otherwise fall back to default constructor.
                if let Some(port) = url.port() {
                    if headers.is_empty() {
                        Ollama::new(url, port)
                    } else {
                        let client = reqwest::Client::builder()
                            .default_headers(headers)
                            .timeout(std::time::Duration::from_secs(60))
                            .build()
                            .map_err(|e| {
                                NodeErr::fail(NodeError::ExecutionFailed(format!(
                                    "reqwest client: {e}"
                                )))
                            })
                            .expect("Could not create client for ollama");

                        ollama_rs::Ollama::new_with_client(url, port, client)
                    }
                } else {
                    // Many users will pass e.g. http://host:11434 — if no port, use default()
                    Ollama::default()
                }
            }
            None => Ollama::default(),
        }
    }
}

impl PartialEq for OllamaAgent {
    fn eq(&self, other: &Self) -> bool {
        self.task == other.task
            && self.system_prompt == other.system_prompt
            && self.model == other.model
            && self.mode == other.mode
            && self.ollama_url == other.ollama_url
            && self.use_payload == other.use_payload
            && self.tool_names == other.tool_names
        // tool_nodes and model_config are intentionally skipped from equality check
    }
}

#[async_trait]
#[typetag::serde]
impl NodeType for OllamaAgent {
    fn type_name(&self) -> String {
        "ollama".to_string()
    }

    fn schema(&self) -> schemars::Schema {
        schemars::schema_for!(OllamaAgent)
    }

    #[tracing::instrument(name = "ollama_agent_node_process", skip(self, context))]
    async fn process(&self, input: Message, context: &mut NodeContext) -> Result<NodeOut, NodeErr> {
        // 1) build our client
        let mut client = self.build_ollama_client(context);

        match self.mode.unwrap_or_default() {
            OllamaMode::Embed => return self.do_embed(client, &input).await,
            OllamaMode::Generate => return self.do_generate(client, &input).await,
            OllamaMode::Chat => {
                let task = &self.task;
                let payload = input.payload();
                let state_json = json!(context.get_all_state());
                let conns = context.connections().unwrap_or_default();
                //let tools      = self
                //    .tool_names
                //    .clone()
                //    .unwrap_or_default();

                let system_prompt = match &self.system_prompt {
                    Some(prompt) => prompt,
                    None => &format!(
                        r#"You are a structured AI agent inside an automation platform.

You are given:
- A free-text `task` written by a non-technical user.
- A `payload` representing the latest user message.
- An existing `state` of previously known variables.
- A list of allowed `connections` (APIs or services you can call).

Your job is to extract the **structured values** needed to complete the task and return an `AgentReply` in valid JSON.

### Instructions

1. **If the task can be completed confidently:**
   - Use the `Success` variant.
   - Fill `state_add` with new values you extract from the payload.
     - Each entry must include: `key`, `value`, and `value_type` (`string`, `integer`, `number`, `boolean`, `array`).
   - Use `state_update` if you're changing a known value.
   - Use `state_delete` if a previously stored key is no longer needed.
   - Copy the `connections` list **exactly as provided**, unless the task clearly says to filter or choose among them.

2. **If the task is missing required info:**
   - Use the `NeedMoreInfo` variant.
   - Ask a clear, concise question to get the missing input.
   - If any required value is uncertain or ambiguous, do not insert a placeholder (e.g., "value": "What days?").
   - Instead, return the NeedMoreInfo variant and ask for the specific missing input clearly.

### Output Format
Return JSON that matches the Rust enum `AgentReply`, either:
- `Success`: with `payload`, optional state changes, and the copied `connections`.
- `NeedMoreInfo`: with a question in the `payload.text` field.

⚠️ Never guess or hallucinate values. Only extract what you're confident about.
"#
                    ),
                };

                let user_msg = format!(
                    "task: {}\npayload: {}\nstate: {}\nconnections: {:?}\n", //tools: {:?}",
                    task,
                    payload,
                    state_json,
                    conns, //tools
                );

                let history = vec![
                    ChatMessage::system(system_prompt.to_string()),
                    ChatMessage::user(user_msg),
                ];

                // 3) call the LLM
                let model = self.model.clone().unwrap_or("llama3:latest".into());
                let schema: schemars::Schema = schemars::schema_for!(AgentReply);
                let format = FormatType::StructuredJson(Box::new(JsonStructure::new_for_schema(
                    schema.clone(),
                )));
                let mut req = ChatMessageRequest::new(model, history).format(format.clone());
                if let Some(opts) = &self.model_options {
                    req = req.options(opts.clone());
                }

                let resp = client
                    .send_chat_messages_with_history(&mut vec![], req)
                    .await;
                if resp.is_err() {
                    let err = resp.err();
                    error!("LLM gave error: {:?}", err);
                    return Err(NodeErr::fail(NodeError::ExecutionFailed(format!(
                        "LLM error: {:?}",
                        err
                    ))));
                }
                info!("agent response: {:?}", resp);
                let reply: AgentReply = serde_json::from_str(&resp.unwrap().message.content)
                    .expect("ollama should respond with structured reply");

                match reply {
                    AgentReply::Success {
                        payload,
                        state_add,
                        state_update,
                        state_delete,
                        connections,
                    } => {
                        if let Some(states_add) = state_add {
                            for state in states_add.iter() {
                                context.set_state(
                                    &state.key,
                                    parse_state_value(&state.value_type, &state.value)
                                        .expect("invalid state value"),
                                );
                            }
                        }
                        if let Some(states_update) = state_update {
                            for state in states_update.iter() {
                                context.set_state(
                                    &state.key,
                                    parse_state_value(&state.value_type, &state.value)
                                        .expect("invalid state value"),
                                );
                            }
                        }
                        if let Some(states_delete) = state_delete {
                            for state in states_delete.iter() {
                                context.delete_state(state);
                            }
                        }

                        let next_conn = match connections.is_empty() {
                            true => None,
                            false => Some(connections),
                        };

                        let payload_json: JsonValue = match serde_json::from_str(&payload) {
                            Ok(json) => json,
                            Err(_) => json!({"text":payload.clone()}),
                        };
                        let main_msg =
                            Message::new(&input.id(), payload_json, input.session_id().clone());
                        return Ok(NodeOut::next(main_msg, next_conn));
                    }
                    AgentReply::NeedMoreInfo { payload } => {
                        let next_payload = json!({"text":payload.text});
                        let main_msg =
                            Message::new(&input.id(), next_payload, input.session_id().clone());
                        return Ok(NodeOut::reply(main_msg));
                    }
                }

                /*

                // 1) Process “tool_calls” if present:
                if let Some(call) = result.get("tool_call") {

                    let name = call
                        .get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            NodeErr::fail(NodeError::ExecutionFailed(
                                "Missing `name` in tool_call".into()),)
                        })?;
                    let action = call
                        .get("action")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            NodeErr::fail(NodeError::ExecutionFailed(
                                "Missing `action` in tool_call".into()))
                        })?;
                    let args = call.get("input").cloned().unwrap_or(json!({}));
                    match context.executor().executor.call_tool(
                        name.to_string(),
                        action.to_string(),
                        args.clone(),
                    ) {
                        Ok(tool_res) => {
                            let result_json = call_result_to_json(tool_res);

                            let tool_msg = Message::new(
                                &input.id(),
                                result_json,
                                input.session_id().clone(),
                            );
                            return Ok(NodeOut::next(tool_msg,next_conn));
                        }
                        Err(e) => {
                            // route to your on_err connection
                            return Err(NodeErr::next(
                                NodeError::ExecutionFailed(format!("tool `{}` errored: {:?}", name, e)),
                                next_conn,
                            ));
                        }
                    }

                }
                */
            }
        }
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}

impl OllamaAgent {
    /// Construct a new `OllamaAgent` with explicit fields.
    pub fn new(
        mode: Option<OllamaMode>,
        task: String,
        system_prompt: Option<String>,
        model: Option<String>,
        ollama_url: Option<Url>,
        model_options: Option<ModelOptions>,
        tool_names: Option<Vec<String>>,
        tool_nodes: Option<DashMap<String, Box<ToolNode>>>,
        use_payload: Option<bool>,
    ) -> Self {
        OllamaAgent {
            mode,
            task,
            system_prompt,
            model,
            ollama_url,
            model_options,
            tool_names,
            tool_nodes,
            use_payload,
        }
    }

    /// Embed branch: returns a JSON payload of embeddings.
    async fn do_embed(&self, client: Ollama, input: &Message) -> Result<NodeOut, NodeErr> {
        // assume `input.payload()` is { "model": "...", "text": "..." }
        let model = input
            .payload()
            .get("model")
            .and_then(JsonValue::as_str)
            .unwrap_or(&self.model.as_deref().unwrap_or("default"))
            .to_string();
        let text = input
            .payload()
            .get("text")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();

        let req = GenerateEmbeddingsRequest::new(model, EmbeddingsInput::Single(text));
        let resp = client.generate_embeddings(req).await.map_err(|e| {
            NodeErr::fail(NodeError::ExecutionFailed(format!("Embed error: {}", e)))
        })?;

        let out = json!({ "embeddings": resp.embeddings });
        let msg = Message::new(input.id().as_str(), out, input.session_id().clone());
        Ok(NodeOut::with_routing(msg, Routing::FollowGraph))
    }

    /// Generate branch: returns a JSON payload with `"generated_text"`.
    async fn do_generate(&self, client: Ollama, input: &Message) -> Result<NodeOut, NodeErr> {
        // assume `input.payload()` is { "model": "...", "prompt": "..." }
        let model = input
            .payload()
            .get("model")
            .and_then(JsonValue::as_str)
            .unwrap_or(&self.model.as_deref().unwrap_or("default"))
            .to_string();
        let prompt = input
            .payload()
            .get("prompt")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();

        let mut req = GenerationRequest::new(model, prompt);
        if let Some(opts) = &self.model_options {
            req = req.options(opts.clone());
        }
        let resp = client.generate(req).await.map_err(|e| {
            NodeErr::fail(NodeError::ExecutionFailed(format!("Generate error: {}", e)))
        })?;

        let out = json!({ "generated_text": resp.response });
        let msg = Message::new(input.id().as_str(), out, input.session_id().clone());
        Ok(NodeOut::with_routing(msg, Routing::FollowGraph))
    }
}

/// Each kind of Ollama request—tool call, embed, chat, or generate.
#[derive(Debug, Copy, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum OllamaMode {
    /// 1) Generate embeddings for a given text.
    ///    `model` is the embedding model identifier; `text` is the raw string.
    Embed,

    /// 2) Engage in “chat mode”: a sequence of role-tagged messages.
    ///    `history` is a Vec<ChatMessage>.
    Chat,

    /// 3) Plain generation from a single prompt.
    Generate,
}

impl Default for OllamaMode {
    fn default() -> Self {
        OllamaMode::Chat
    }
}

#[cfg(test)]
mod ollama_agent_tests {
    use super::*;
    use schemars::schema_for;
    use serde_json::json;
    use tokio;
    use url::Url;

    fn make_agent(mode: Option<OllamaMode>) -> OllamaAgent {
        OllamaAgent {
            mode,
            task: "dummy".into(),
            system_prompt: None,
            model: Some("llama3.2:1b".into()),
            ollama_url: None,
            model_options: None,
            tool_names: None,
            tool_nodes: None,
            use_payload: None,
        }
    }

    #[test]
    fn serde_roundtrip_and_schema_contains_all_fields() {
        let agent = OllamaAgent {
            mode: Some(OllamaMode::Generate),
            task: "t".into(),
            system_prompt: None,
            model: Some("m".into()),
            ollama_url: Some(Url::parse("http://x/").unwrap()),
            model_options: None,
            tool_names: Some(vec!["foo".into()]),
            tool_nodes: None,
            use_payload: None,
        };

        let s = serde_json::to_string(&agent).unwrap();
        let de: OllamaAgent = serde_json::from_str(&s).unwrap();
        assert_eq!(de.mode, Some(OllamaMode::Generate));
        assert_eq!(de.task, "t");
        assert_eq!(de.model, Some("m".into()));

        // schema
        let schema = schema_for!(OllamaAgent);
        let schema_json: JsonValue = serde_json::to_value(&schema).unwrap();
        // Navigate to the properties map
        let props = schema_json
            .get("properties")
            .expect("no properties in schema");

        for key in &["mode", "task", "model", "ollama_url", "tool_names"] {
            assert!(props.get(*key).is_some(), "missing `{}` in schema", key);
        }
    }

    #[tokio::test]
    async fn default_mode_is_chat_and_bad_endpoint_errors() {
        // no mode => Chat
        let mut agent = make_agent(None);
        agent.ollama_url = Some(Url::parse("http://localhost:123").unwrap());
        // this will attempt Chat against localhost:11434 and fail
        let msg = Message::new("id", json!({}), "123".to_string());
        let mut ctx = NodeContext::dummy();
        let err = agent.process(msg, &mut ctx).await.unwrap_err();
        let e = format!("{:?}", err);
        assert!(
            e.contains("LLM error"),
            "expected Chat path to produce an LLM error"
        );
    }

    #[tokio::test]
    async fn embed_mode_goes_to_do_embed_and_parses_payload() {
        let mut agent = make_agent(Some(OllamaMode::Embed));
        // override host to bogus so it errors *after* embedding arguments
        agent.ollama_url = Some(Url::parse("http://127.0.0.1:1/").unwrap());
        let payload = json!({
            "model": "custom-model",
            "text": "hello embed"
        });
        let msg = Message::new("id", payload.clone(), "123".to_string());
        let mut ctx = NodeContext::dummy();
        let res = agent.process(msg, &mut ctx).await;
        // On embedding the *client* will error connecting to host:1,
        // but we at least confirmed we hit do_embed and built the right request
        let err = res.unwrap_err();
        let s = format!("{:?}", err);
        assert!(s.contains("Embed error"), "expected Embed branch");
    }

    #[tokio::test]
    async fn generate_mode_goes_to_do_generate_and_parses_payload() {
        let mut agent = make_agent(Some(OllamaMode::Generate));
        agent.ollama_url = Some(Url::parse("http://127.0.0.1:1/").unwrap());
        let payload = json!({
            "model": "custom-model",
            "prompt": "why?"
        });
        let msg = Message::new("id", payload.clone(), "123".to_string());
        let mut ctx = NodeContext::dummy();
        let res = agent.process(msg, &mut ctx).await;
        let err = res.unwrap_err();
        let s = format!("{:?}", err);
        assert!(s.contains("Generate error"), "expected Generate branch");
    }
}
