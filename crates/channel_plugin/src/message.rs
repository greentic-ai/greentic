use std::{collections::HashMap};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct ChannelMessage {
    pub id: String,                      // Unique ID (UUID or channel-provided)
    pub session_id: Option<String>,              // A UUID uses during the same flow execution by the same user
    pub direction: MessageDirection,     // Incoming or Outgoing
    pub timestamp: DateTime<Utc>,        // When it was sent or received
    pub channel: String,                 // Slack, Telegram, Email, etc.
    pub from: Participant,               // Sender info
    pub to: Vec<Participant>,            // Recipient(s)

    pub content: Option<MessageContent>, // Text or attachment
    pub thread_id: Option<String>,       // For threading support
    pub reply_to_id: Option<String>,     // If replying to another message
    pub metadata: HashMap<String, Value>, // Channel-specific or custom data
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub enum MessageDirection {
    #[default]
    Incoming,
    Outgoing
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct Participant {
    pub id: String,                      // Internal or platform-specific ID
    pub display_name: Option<String>,   // Optional for SMS, Email
    pub channel_specific_id: Option<String>, // E.g., phone number, email, handle
}

impl Participant {
    pub fn new(id: String, display_name: Option<String>, channel_specific_id: Option<String>) -> Self {
        Self{id, display_name, channel_specific_id}
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum MessageContent {
    Text(String),
    File(FileMetadata),
    Media(MediaMetadata),
    Event(Event),
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct FileMetadata {
    pub file_name: String,
    pub mime_type: String,
    pub url: String,                  // Direct link or temporary
    pub size_bytes: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct MediaMetadata {
    pub kind: MediaType,             // Image, Video, Audio
    pub file: FileMetadata,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub enum MediaType {
    Image,
    Video,
    Audio,
    Binary
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Event {
    pub event_type: String,           
    pub event_payload: Option<Value> 
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EventType {
    pub event_type: String,             // e.g., "UserJoined", "TypingStarted"
    pub description: String,            // Human-readable description
    pub payload_schema: Option<Value>,  // the json schema for the event_payload  
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct ChannelCapabilities {
    pub name: String,                         // e.g. "Slack", "Email", "SMS"
    pub supports_sending: bool,
    pub supports_receiving: bool,
    pub supports_text: bool,
    pub supports_files: bool,
    pub supports_media: bool,
    pub supports_events: bool,
    pub supports_typing: bool,
    pub supports_threading: bool,
    pub supports_reactions: bool,
    pub supports_call: bool,
    pub supports_buttons: bool,
    pub supports_links: bool,
    pub supports_custom_payloads: bool,

    pub supported_events: Vec<EventType>, // List of events with descriptions
}