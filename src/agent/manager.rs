use async_trait::async_trait;
use schemars::{schema::RootSchema, schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use crate::{
    message::Message,
    node::{NodeContext, NodeErr, NodeOut, NodeType},
};

use super::ollama::OllamaAgent;

/// Every built‐in agent must implement the existing `AgentNode`‐like behavior.
/// Instead of “trait objects,” we enumerate them here.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum BuiltInAgent {
    #[serde(rename = "ollama")]
    Ollama(OllamaAgent),
    // Add more variants as needed…
}

impl BuiltInAgent {
    /// Delegate to the underlying variant’s `agent_name()`.
    pub fn agent_name(&self) -> String {
        match self {
            BuiltInAgent::Ollama(inner) => inner.type_name(),
        }
    }

    /// Delegate to the underlying variant’s `clone_box()` if needed.
    /// But since this enum is `Clone`, you can simply call `.clone()`.
    pub fn boxed_clone(&self) -> BuiltInAgent {
        self.clone()
    }
}

// If you also need to treat `BuiltInAgent` as a `NodeType` so that you can
// serialize flows that mention agent‐nodes, simply forward to each variant:
#[typetag::serde]
#[async_trait]
impl NodeType for BuiltInAgent {
    fn type_name(&self) -> String {
        self.agent_name().to_string()
    }

    fn schema(&self) -> RootSchema {
        schema_for!(BuiltInAgent)
    }

    async fn process(
        &self,
        input: Message,
        context: &mut NodeContext,
    ) -> Result<NodeOut, NodeErr> {
        match self {
            BuiltInAgent::Ollama(inner)  => inner.process(input, context).await,
        }
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}
