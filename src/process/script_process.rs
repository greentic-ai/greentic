use async_trait::async_trait;
use schemars::{schema::RootSchema, schema_for, JsonSchema};
use ::serde::{Deserialize, Serialize};

use crate::{message::Message, node::{NodeContext, NodeErr, NodeOut, NodeType}};

/// A Rhia script process node
/// TODO add examples
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "script")]
pub struct ScriptProcessNode;

#[async_trait]
#[typetag::serde]
impl NodeType for ScriptProcessNode {
    fn type_name(&self) -> String {
        "script".to_string()
    }

    fn schema(&self) -> RootSchema {
        schema_for!(ScriptProcessNode)
    }

    #[tracing::instrument(name = "script_node_process", skip(self, context))]
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