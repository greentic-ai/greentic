use crate::config::MsGraphConfig;
use anyhow::Result;
use graph_rs_sdk::{identity::ConfidentialClientApplication, GraphClient};

#[derive(Clone, Debug)]
pub struct GraphState {
    pub config: MsGraphConfig,
    pub client: GraphClient,
}

impl GraphState {
    pub async fn new(config: &MsGraphConfig) -> Result<Self> {
        let confidential_client = ConfidentialClientApplication::builder(config.client_id.clone())
            .with_client_secret(&config.client_secret)
            .with_tenant(&config.tenant_id)
            .build();

        let client = GraphClient::from(&confidential_client);

        Ok(GraphState {
            config: config.clone(),
            client,
        })
    }
}
