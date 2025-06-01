use async_trait::async_trait;
use schemars::{schema::RootSchema, schema_for, JsonSchema};
use ::serde::{Deserialize, Serialize};

use crate::{message::Message, node::{NodeContext, NodeErr, NodeOut, NodeType}};


/// A simple debug node for testing that just echoes its input.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "debug")]
pub struct DebugProcessNode;

#[async_trait]
#[typetag::serde]
impl NodeType for DebugProcessNode {
    fn type_name(&self) -> String {
        "debug".to_string()
    }

    fn schema(&self) -> RootSchema {
        schema_for!(DebugProcessNode)
    }

    #[tracing::instrument(name = "debug_node_process", skip(self, context))]
    async fn process(
        &self,
        input: Message,
        context: &mut NodeContext,
    ) -> Result<NodeOut, NodeErr> {
        println!("DEBUG: Got message {:?}, context {:?}", input, context);
        Ok(NodeOut::all(input))
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}