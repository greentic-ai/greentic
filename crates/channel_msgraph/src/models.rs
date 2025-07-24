use serde::{Deserialize, Serialize};

/// Full payload POSTed by Microsoft Graph webhook
#[derive(Debug, Deserialize, Serialize)]
pub struct GraphWebhookPayload {
    pub value: Vec<GraphNotification>,
}

/// Microsoft Graph individual notification item
#[derive(Debug, Deserialize, Serialize)]
pub struct GraphNotification {
    #[serde(rename = "subscriptionId")]
    pub subscription_id: String,

    #[serde(rename = "changeType")]
    pub change_type: String,

    #[serde(rename = "resource")]
    pub resource: String,

    #[serde(rename = "clientState")]
    pub client_state: Option<String>,

    #[serde(rename = "tenantId")]
    pub tenant_id: Option<String>,

    #[serde(rename = "id")]
    pub id: String,

    #[serde(rename = "eventTime")]
    pub event_time: String,

    // Only for Teams/Mail (not always present)
    #[serde(rename = "resourceData", skip_serializing_if = "Option::is_none")]
    pub resource_data: Option<ResourceData>,
}

/// Optional embedded data about the resource
#[derive(Debug, Deserialize, Serialize)]
pub struct ResourceData {
    pub id: Option<String>,
    #[serde(rename = "@odata.type")]
    pub odata_type: Option<String>,
    #[serde(rename = "@odata.id")]
    pub odata_id: Option<String>,

    // For Teams messages
    #[serde(rename = "replyToId")]
    pub reply_to_id: Option<String>,
    #[serde(rename = "from")]
    pub from: Option<Participant>,
    #[serde(rename = "body")]
    pub body: Option<ItemBody>,

    // For SharePoint or OneDrive
    #[serde(rename = "name")]
    pub name: Option<String>,
    #[serde(rename = "parentReference")]
    pub parent_reference: Option<ParentReference>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ItemBody {
    #[serde(rename = "content")]
    pub content: String,
    #[serde(rename = "contentType")]
    pub content_type: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Participant {
    #[serde(rename = "user")]
    pub user: Option<UserIdentity>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UserIdentity {
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(rename = "id")]
    pub id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ParentReference {
    pub drive_id: Option<String>,
    pub id: Option<String>,
    pub path: Option<String>,
}
