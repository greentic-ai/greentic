use async_trait::async_trait;
use schemars::{schema::RootSchema, schema_for, JsonSchema};
use ::serde::{Deserialize, Serialize};

use crate::{message::Message, node::{NodeContext, NodeErr, NodeOut, NodeType}};

/// A Handlebars template process node
/// TODO add examples
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "template")]
pub struct TemplateProcessNode;

#[async_trait]
#[typetag::serde]
impl NodeType for TemplateProcessNode {
    fn type_name(&self) -> String {
        "template".to_string()
    }

    fn schema(&self) -> RootSchema {
        schema_for!(TemplateProcessNode)
    }

    #[tracing::instrument(name = "template_node_process", skip(self, context))]
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