//! Registry for first-party agents shipped with Greentic.
//!
//! The [`BuiltInAgent`] enum enables flows to reference agents in a uniform
//! fashion when deserialising from YAML. Each variant simply delegates to the
//! concrete [`NodeType`] implementation.

use crate::{
    agent::openai::OpenAiAgent,
    message::Message,
    node::{NodeContext, NodeErr, NodeOut, NodeType},
};
use async_trait::async_trait;
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use super::ollama::OllamaAgent;

/// Deserialisable wrapper around each agent implementation that ships with the
/// binary.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum BuiltInAgent {
    /// Open-source friendly agent backed by a local Ollama runtime.
    #[serde(rename = "ollama")]
    Ollama(OllamaAgent),
    /// SaaS-backed agent that calls the OpenAI Chat Completions API.
    #[serde(rename = "openai")]
    OpenAi(OpenAiAgent),
    // Add more variants as needed…
}

impl BuiltInAgent {
    /// Returns a human-friendly identifier for the underlying agent.
    ///
    /// ```rust
    /// use greentic::agent::manager::BuiltInAgent;
    /// use greentic::agent::ollama::{OllamaAgent, OllamaMode};
    ///
    /// let agent = BuiltInAgent::Ollama(OllamaAgent {
    ///     task: "Collect context".into(),
    ///     system_prompt: None,
    ///     model: None,
    ///     mode: Some(OllamaMode::Chat),
    ///     ollama_url: None,
    ///     model_options: None,
    ///     tool_names: None,
    ///     use_payload: None,
    ///     tool_nodes: None,
    /// });
    /// assert_eq!(agent.agent_name(), "ollama");
    /// ```
    pub fn agent_name(&self) -> String {
        match self {
            BuiltInAgent::Ollama(inner) => inner.type_name(),
            BuiltInAgent::OpenAi(inner) => inner.type_name(),
        }
    }

    /// Convenience method for cloning the enum without allocating a box.
    pub fn boxed_clone(&self) -> BuiltInAgent {
        self.clone()
    }
}

/// Allow treating [`BuiltInAgent`] values as [`NodeType`]s when evaluating a
/// flow.
#[typetag::serde]
#[async_trait]
impl NodeType for BuiltInAgent {
    fn type_name(&self) -> String {
        self.agent_name().to_string()
    }

    fn schema(&self) -> Schema {
        schema_for!(BuiltInAgent)
    }

    async fn process(&self, input: Message, context: &mut NodeContext) -> Result<NodeOut, NodeErr> {
        match self {
            BuiltInAgent::Ollama(inner) => inner.process(input, context).await,
            BuiltInAgent::OpenAi(inner) => inner.process(input, context).await,
        }
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}
