use crate::flow::manager::Flow;
use crate::process::manager::ProcessManager;
use crate::process::process::ProcessWrapper;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// Serializable representation of the loaded runtime components.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSnapshot {
    pub version: u32,
    pub flows: Vec<Flow>,
    pub tools: Vec<ToolSnapshot>,
    pub channels: Vec<ChannelSnapshot>,
    pub processes: Vec<ProcessSnapshot>,
}

impl RuntimeSnapshot {
    pub const CURRENT_VERSION: u32 = 1;

    /// Create an empty snapshot placeholder.
    pub fn empty() -> Self {
        RuntimeSnapshot {
            version: Self::CURRENT_VERSION,
            flows: Vec::new(),
            tools: Vec::new(),
            channels: Vec::new(),
            processes: Vec::new(),
        }
    }

    /// Helper for callers that already have all the components ready.
    pub fn from_parts(
        flows: Vec<Flow>,
        tools: Vec<ToolSnapshot>,
        channels: Vec<ChannelSnapshot>,
        processes: Vec<ProcessSnapshot>,
    ) -> Self {
        RuntimeSnapshot {
            version: Self::CURRENT_VERSION,
            flows,
            tools,
            channels,
            processes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSnapshot {
    pub name: String,
    pub tool_id: String,
    pub action: String,
    pub wasm_path: PathBuf,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    pub parameters_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSnapshot {
    pub name: String,
    pub remote: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wasm_path: Option<PathBuf>,
    pub parameters_json: String,
}

impl From<&ProcessWrapper> for ProcessSnapshot {
    fn from(wrapper: &ProcessWrapper) -> Self {
        let instance = wrapper.instance();
        ProcessSnapshot {
            name: wrapper.name().to_string(),
            description: wrapper.description(),
            wasm_path: Some(instance.wasm_path),
            parameters_json: serde_json::to_string(&wrapper.parameters())
                .unwrap_or_else(|_| "{}".into()),
        }
    }
}

impl ProcessSnapshot {
    pub fn for_built_in(name: String, description: String, parameters: Value) -> Self {
        ProcessSnapshot {
            name,
            description,
            wasm_path: None,
            parameters_json: serde_json::to_string(&parameters).unwrap_or_else(|_| "{}".into()),
        }
    }

    pub fn from_manager(manager: &ProcessManager) -> Vec<ProcessSnapshot> {
        manager
            .watcher
            .as_ref()
            .map(|watcher| {
                watcher
                    .loaded_processes()
                    .iter()
                    .map(|entry| {
                        let wrapper = entry.value().clone();
                        ProcessSnapshot::from(wrapper.as_ref())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
}
