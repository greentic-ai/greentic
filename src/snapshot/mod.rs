//! Snapshot manifest and validation utilities for Greentic runtime exports.
//!
//! The snapshot format wraps the existing CBOR payload (`payload`) with a
//! [`SnapshotManifest`] that records logical requirements. Consumers can load a
//! snapshot, inspect the local environment, and produce a [`ValidationPlan`]
//! describing the work needed to satisfy the manifest.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_yaml_bw as serde_yaml;
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
};
use tracing::warn;

use crate::{
    agent::manager::BuiltInAgent,
    process::manager::BuiltInProcess,
    runtime_snapshot::{RequirementEntry, RuntimeSnapshot},
};

/// What a snapshot requires at runtime to "just start".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotManifest {
    pub format_version: String,
    pub created_at: String,
    pub greentic_version: String,
    pub required_tools: Vec<Req>,
    pub required_channels: Vec<Req>,
    #[serde(alias = "required_plugins")]
    pub required_processes: Vec<Req>,
    pub required_agents: Vec<Req>,
    pub required_configs: Vec<ConfigReq>,
    pub optional_configs: Vec<ConfigReq>,
    pub required_secrets: Vec<SecretReq>,
    pub optional_secrets: Vec<SecretReq>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Req {
    pub id: String,
    pub version_req: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConfigReq {
    pub key: String,
    pub owner: RequirementOwner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct SecretReq {
    pub key: String,
    pub owner: RequirementOwner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RequirementKind {
    Tool,
    Channel,
    Process,
    Agent,
}

impl RequirementKind {
    fn as_str(&self) -> &'static str {
        match self {
            RequirementKind::Tool => "tool",
            RequirementKind::Channel => "channel",
            RequirementKind::Process => "process",
            RequirementKind::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequirementOwner {
    pub kind: RequirementKind,
    pub id: String,
}

impl RequirementOwner {
    pub fn new(kind: RequirementKind, id: impl Into<String>) -> Self {
        RequirementOwner {
            kind,
            id: id.into(),
        }
    }
}

/// The exported `.gtc` CBOR payload wraps this manifest alongside binary data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GreenticSnapshot {
    pub manifest: SnapshotManifest,
    pub payload: Vec<u8>,
}

/// Load a snapshot from disk and deserialize it from CBOR.
pub fn load_snapshot(path: &Path) -> Result<GreenticSnapshot> {
    let bytes =
        fs::read(path).with_context(|| format!("Failed to read snapshot at {}", path.display()))?;

    match serde_cbor::from_slice::<GreenticSnapshot>(&bytes) {
        Ok(snapshot) => Ok(snapshot),
        Err(err) => {
            if let Ok(legacy) = serde_cbor::from_slice::<RuntimeSnapshot>(&bytes) {
                let manifest = manifest_from_runtime(&legacy);
                let payload = serde_cbor::to_vec(&legacy)
                    .context("Failed to serialize legacy snapshot payload")?;
                Ok(GreenticSnapshot { manifest, payload })
            } else {
                Err(err).context("Snapshot payload is not valid CBOR")
            }
        }
    }
}

/// Simple descriptor for locally installed components.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Installed {
    pub id: String,
    pub version: Option<String>,
}

/// Represents a key within an optional scope (e.g. `tool:mcp.weather`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopedKey {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<RequirementOwner>,
}

/// Snapshot of the local Greentic environment used for validation.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentState {
    pub tools: Vec<Installed>,
    pub channels: Vec<Installed>,
    pub processes: Vec<Installed>,
    pub configs: Vec<ScopedKey>,
    pub secrets: Vec<ScopedKey>,
}

impl EnvironmentState {
    /// Best-effort discovery based on the default Greentic directory layout.
    ///
    /// The directory root is resolved from `GREENTIC_ROOT` or defaults to the
    /// current working directory.
    pub fn discover() -> Self {
        let root = resolve_root_dir();
        let tools = discover_tools(&root);
        let channels = discover_channels(&root);
        let processes = discover_processes(&root);
        let configs = present_configs_in(&root.join("config/.env"));
        let secrets = present_secrets_in(&root.join("secrets/.env"));

        EnvironmentState {
            tools,
            channels,
            processes,
            configs,
            secrets,
        }
    }
}

/// Human-readable remediation plan for satisfying a snapshot manifest.
#[derive(Debug, Clone, Serialize)]
pub struct ValidationPlan {
    pub ok: bool,
    pub summary: String,
    pub missing_tools: Vec<Req>,
    pub missing_channels: Vec<Req>,
    pub missing_processes: Vec<Req>,
    pub missing_agents: Vec<Req>,
    pub missing_configs: Vec<ConfigReq>,
    pub missing_secrets: Vec<SecretReq>,
    pub suggested_commands: SuggestedCommands,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SuggestedCommands {
    pub install: Vec<String>,
    pub config: Vec<String>,
    pub secret: Vec<String>,
}

/// Build a validation plan by comparing a manifest to the local environment.
pub fn plan_from(manifest: &SnapshotManifest, state: &EnvironmentState) -> ValidationPlan {
    let missing_tools = missing_requirements(&manifest.required_tools, &state.tools);
    let missing_channels = missing_requirements(&manifest.required_channels, &state.channels);
    let missing_processes = missing_requirements(&manifest.required_processes, &state.processes);
    let missing_configs = missing_config_reqs(&manifest.required_configs, &state.configs);
    let missing_secrets = missing_secret_reqs(&manifest.required_secrets, &state.secrets);
    let missing_agents: Vec<Req> = Vec::new();

    let ok = missing_tools.is_empty()
        && missing_channels.is_empty()
        && missing_processes.is_empty()
        && missing_agents.is_empty()
        && missing_configs.is_empty()
        && missing_secrets.is_empty();

    let summary = build_summary(
        &missing_tools,
        &missing_channels,
        &missing_processes,
        &missing_agents,
        &missing_configs,
        &missing_secrets,
    );
    let suggested_commands = build_commands(
        &missing_tools,
        &missing_channels,
        &missing_processes,
        &missing_configs,
        &missing_secrets,
    );

    ValidationPlan {
        ok,
        summary,
        missing_tools,
        missing_channels,
        missing_processes,
        missing_agents,
        missing_configs,
        missing_secrets,
        suggested_commands,
    }
}

fn missing_requirements(required: &[Req], installed: &[Installed]) -> Vec<Req> {
    required
        .iter()
        .filter(|req| {
            if matches!(req.version_req.as_deref(), Some("builtin")) {
                return false;
            }
            match installed.iter().find(|item| item.id == req.id) {
                None => true,
                Some(inst) => match (&req.version_req, &inst.version) {
                    (None, _) => false,
                    (Some(_), None) => true,
                    (Some(expected), Some(actual)) => expected != actual,
                },
            }
        })
        .cloned()
        .collect()
}

fn missing_config_reqs(required: &[ConfigReq], present: &[ScopedKey]) -> Vec<ConfigReq> {
    let mut present_exact: BTreeSet<(String, RequirementOwner)> = BTreeSet::new();
    let mut present_keys: BTreeSet<String> = BTreeSet::new();

    for entry in present {
        if let Some(owner) = &entry.owner {
            present_exact.insert((entry.key.clone(), owner.clone()));
        }
        present_keys.insert(entry.key.clone());
    }

    required
        .iter()
        .filter(|req| {
            let owned = (req.key.clone(), req.owner.clone());
            !present_exact.contains(&owned) && !present_keys.contains(&req.key)
        })
        .cloned()
        .collect()
}

fn missing_secret_reqs(required: &[SecretReq], present: &[ScopedKey]) -> Vec<SecretReq> {
    let mut present_exact: BTreeSet<(String, RequirementOwner)> = BTreeSet::new();
    let mut present_keys: BTreeSet<String> = BTreeSet::new();

    for entry in present {
        if let Some(owner) = &entry.owner {
            present_exact.insert((entry.key.clone(), owner.clone()));
        }
        present_keys.insert(entry.key.clone());
    }

    required
        .iter()
        .filter(|req| {
            let owned = (req.key.clone(), req.owner.clone());
            !present_exact.contains(&owned) && !present_keys.contains(&req.key)
        })
        .cloned()
        .collect()
}

fn build_summary(
    missing_tools: &[Req],
    missing_channels: &[Req],
    missing_processes: &[Req],
    missing_agents: &[Req],
    missing_configs: &[ConfigReq],
    missing_secrets: &[SecretReq],
) -> String {
    let counts = [
        (missing_tools.len(), "tool"),
        (missing_channels.len(), "channel"),
        (missing_processes.len(), "process"),
        (missing_agents.len(), "agent"),
        (missing_configs.len(), "config"),
        (missing_secrets.len(), "secret"),
    ];

    let total_missing: usize = counts.iter().map(|(count, _)| *count).sum();
    if total_missing == 0 {
        return "All requirements satisfied".to_string();
    }

    let mut parts = Vec::new();
    for (count, label) in counts.iter().filter(|(count, _)| *count > 0) {
        let plural = if *count == 1 {
            (*label).to_string()
        } else {
            format!("{}s", label)
        };
        parts.push(format!("{} {}", count, plural));
    }

    let detail = parts.join(", ");
    format!("{} items missing ({})", total_missing, detail)
}

fn build_commands(
    missing_tools: &[Req],
    missing_channels: &[Req],
    missing_processes: &[Req],
    missing_configs: &[ConfigReq],
    missing_secrets: &[SecretReq],
) -> SuggestedCommands {
    let mut cmd = SuggestedCommands::default();

    for req in missing_tools {
        if let Some(value) = store_cmd("tool", req) {
            cmd.install.push(value);
        }
    }
    for req in missing_channels {
        if let Some(value) = store_cmd("channel", req) {
            cmd.install.push(value);
        }
    }
    for req in missing_processes {
        if let Some(value) = store_cmd("process", req) {
            cmd.install.push(value);
        }
    }
    for req in missing_configs {
        cmd.config.push(config_cmd(req));
    }
    for req in missing_secrets {
        cmd.secret.push(secret_cmd(req));
    }

    cmd
}

fn owner_scope(owner: &RequirementOwner) -> String {
    format!("{}:{}", owner.kind.as_str(), owner.id)
}

fn store_cmd(kind: &str, req: &Req) -> Option<String> {
    if matches!(req.version_req.as_deref(), Some("builtin")) {
        return None;
    }
    let version = req.version_req.as_deref().unwrap_or("latest");
    Some(format!(
        "greentic store install {} {}@{}",
        kind, req.id, version
    ))
}

fn config_cmd(req: &ConfigReq) -> String {
    format!(
        "greentic config set {}::{}=\"<VALUE>\"",
        owner_scope(&req.owner),
        req.key
    )
}

fn secret_cmd(req: &SecretReq) -> String {
    format!(
        "greentic secrets add {} <VALUE>  # {}",
        req.key,
        owner_scope(&req.owner)
    )
}

fn push_config_entry(
    owner: &RequirementOwner,
    entry: &RequirementEntry,
    required: &mut Vec<ConfigReq>,
    optional: &mut Vec<ConfigReq>,
) {
    let req = ConfigReq {
        key: entry.key.clone(),
        owner: owner.clone(),
        description: entry.description.clone(),
    };
    if entry.required {
        required.push(req);
    } else {
        optional.push(req);
    }
}

fn push_secret_entry(
    owner: &RequirementOwner,
    entry: &RequirementEntry,
    required: &mut Vec<SecretReq>,
    optional: &mut Vec<SecretReq>,
) {
    let req = SecretReq {
        key: entry.key.clone(),
        owner: owner.clone(),
        description: entry.description.clone(),
    };
    if entry.required {
        required.push(req);
    } else {
        optional.push(req);
    }
}

fn dedup_requirements<T: Ord>(items: &mut Vec<T>) {
    items.sort();
    items.dedup();
}

#[derive(Default, Clone)]
struct ComponentRequirements {
    configs: Vec<RequirementEntry>,
    secrets: Vec<RequirementEntry>,
}

fn merge_agent_requirements(
    map: &mut BTreeMap<String, ComponentRequirements>,
    agent_name: String,
    agent: &BuiltInAgent,
) {
    let reqs = agent_component_requirements(agent);
    map.entry(agent_name)
        .and_modify(|existing| {
            existing.configs.extend(reqs.configs.clone());
            existing.secrets.extend(reqs.secrets.clone());
        })
        .or_insert(reqs);
}

fn agent_component_requirements(agent: &BuiltInAgent) -> ComponentRequirements {
    match agent {
        BuiltInAgent::Ollama(_) => ComponentRequirements {
            configs: vec![optional_entry("OLLAMA_URL")],
            secrets: vec![optional_entry("OLLAMA_KEY")],
        },
        BuiltInAgent::OpenAi(_) => ComponentRequirements {
            configs: vec![optional_entry("OPENAI_URL")],
            secrets: vec![required_entry("OPENAI_KEY")],
        },
    }
}

fn required_entry(key: &str) -> RequirementEntry {
    required_entry_with_desc(key, None)
}

fn optional_entry(key: &str) -> RequirementEntry {
    optional_entry_with_desc(key, None)
}

fn required_entry_with_desc(key: &str, description: Option<&str>) -> RequirementEntry {
    RequirementEntry {
        key: key.to_string(),
        description: description.map(|s| s.to_string()),
        required: true,
    }
}

fn optional_entry_with_desc(key: &str, description: Option<&str>) -> RequirementEntry {
    RequirementEntry {
        key: key.to_string(),
        description: description.map(|s| s.to_string()),
        required: false,
    }
}

/// Build a manifest from a runtime snapshot, populating requirement lists with
/// best-effort data.
pub fn manifest_from_runtime(runtime: &RuntimeSnapshot) -> SnapshotManifest {
    let mut tool_ids = BTreeSet::new();
    let mut channel_ids = BTreeSet::new();
    let mut process_sources: BTreeMap<String, bool> = BTreeMap::new();
    let mut agent_ids = BTreeSet::new();
    let mut agent_requirements: BTreeMap<String, ComponentRequirements> = BTreeMap::new();

    let mut required_configs: Vec<ConfigReq> = Vec::new();
    let mut optional_configs: Vec<ConfigReq> = Vec::new();
    let mut required_secrets: Vec<SecretReq> = Vec::new();
    let mut optional_secrets: Vec<SecretReq> = Vec::new();

    for tool in &runtime.tools {
        tool_ids.insert(tool.tool_id.clone());
        let owner = RequirementOwner::new(RequirementKind::Tool, tool.tool_id.clone());
        for entry in &tool.secrets {
            push_secret_entry(&owner, entry, &mut required_secrets, &mut optional_secrets);
        }
        for entry in &tool.configs {
            push_config_entry(&owner, entry, &mut required_configs, &mut optional_configs);
        }

        let defaults = tool_default_requirements(&tool.tool_id);
        for entry in &defaults.secrets {
            push_secret_entry(&owner, entry, &mut required_secrets, &mut optional_secrets);
        }
        for entry in &defaults.configs {
            push_config_entry(&owner, entry, &mut required_configs, &mut optional_configs);
        }
    }

    for channel in &runtime.channels {
        channel_ids.insert(channel.name.clone());
        let owner = RequirementOwner::new(RequirementKind::Channel, channel.name.clone());
        for entry in &channel.configs {
            push_config_entry(&owner, entry, &mut required_configs, &mut optional_configs);
        }
        for entry in &channel.secrets {
            push_secret_entry(&owner, entry, &mut required_secrets, &mut optional_secrets);
        }
    }

    for process in &runtime.processes {
        let is_builtin = process.wasm_path.is_none();
        process_sources
            .entry(process.name.clone())
            .and_modify(|existing| *existing = *existing && is_builtin)
            .or_insert(is_builtin);
        let owner = RequirementOwner::new(RequirementKind::Process, process.name.clone());
        for entry in &process.configs {
            push_config_entry(&owner, entry, &mut required_configs, &mut optional_configs);
        }
        for entry in &process.secrets {
            push_secret_entry(&owner, entry, &mut required_secrets, &mut optional_secrets);
        }
    }

    for flow in &runtime.flows {
        for channel in flow.channels() {
            channel_ids.insert(channel);
        }
        for channel in flow.remote_channels() {
            channel_ids.insert(channel);
        }
        for node in flow.nodes().into_values() {
            match node.kind {
                crate::flow::manager::NodeKind::Tool { tool } => {
                    let tool_id = tool.name;
                    tool_ids.insert(tool_id.clone());
                    let owner = RequirementOwner::new(RequirementKind::Tool, tool_id.clone());
                    let defaults = tool_default_requirements(&tool_id);
                    for entry in &defaults.secrets {
                        push_secret_entry(
                            &owner,
                            entry,
                            &mut required_secrets,
                            &mut optional_secrets,
                        );
                    }
                    for entry in &defaults.configs {
                        push_config_entry(
                            &owner,
                            entry,
                            &mut required_configs,
                            &mut optional_configs,
                        );
                    }
                }
                crate::flow::manager::NodeKind::Channel { cfg } => {
                    channel_ids.insert(cfg.channel_name);
                }
                crate::flow::manager::NodeKind::Process { process } => {
                    let (name, is_builtin) = match &process {
                        BuiltInProcess::Plugin { name, .. } => (name.clone(), false),
                        _ => (process.process_name(), true),
                    };
                    process_sources
                        .entry(name.clone())
                        .and_modify(|existing| *existing = *existing && is_builtin)
                        .or_insert(is_builtin);

                    if let BuiltInProcess::Qa(node) = &process {
                        if let Some(agent) = &node.config.fallback_agent {
                            let agent_name = agent.agent_name();
                            agent_ids.insert(agent_name.clone());
                            merge_agent_requirements(&mut agent_requirements, agent_name, agent);
                        }
                    }
                }
                crate::flow::manager::NodeKind::Agent { agent } => {
                    let agent_name = agent.agent_name();
                    agent_ids.insert(agent_name.clone());
                    merge_agent_requirements(&mut agent_requirements, agent_name, &agent);
                }
            }
        }
    }

    for (agent_name, reqs) in agent_requirements {
        let owner = RequirementOwner::new(RequirementKind::Agent, agent_name.clone());
        for entry in &reqs.configs {
            push_config_entry(&owner, entry, &mut required_configs, &mut optional_configs);
        }
        for entry in &reqs.secrets {
            push_secret_entry(&owner, entry, &mut required_secrets, &mut optional_secrets);
        }
    }

    dedup_requirements(&mut required_configs);
    dedup_requirements(&mut optional_configs);
    dedup_requirements(&mut required_secrets);
    dedup_requirements(&mut optional_secrets);

    let required_tools = tool_ids
        .into_iter()
        .map(|id| Req {
            id,
            version_req: None,
        })
        .collect();

    let required_channels = channel_ids
        .into_iter()
        .map(|id| Req {
            id,
            version_req: None,
        })
        .collect();

    let required_processes = process_sources
        .into_iter()
        .map(|(id, is_builtin)| Req {
            id,
            version_req: if is_builtin {
                Some("builtin".into())
            } else {
                None
            },
        })
        .collect();

    let required_agents = agent_ids
        .into_iter()
        .map(|id| Req {
            id,
            version_req: Some("builtin".into()),
        })
        .collect();

    SnapshotManifest {
        format_version: "1".into(),
        created_at: Utc::now().to_rfc3339(),
        greentic_version: env!("CARGO_PKG_VERSION").into(),
        required_tools,
        required_channels,
        required_processes,
        required_agents,
        required_configs,
        optional_configs,
        required_secrets,
        optional_secrets,
    }
}

fn resolve_root_dir() -> PathBuf {
    let base = env::var("GREENTIC_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join("greentic")
}

fn installed_tools_in(dir: &Path) -> Vec<Installed> {
    registry_entries(dir).unwrap_or_else(|| collect_tool_files(dir))
}

fn installed_channels_in(dir: &Path) -> Vec<Installed> {
    if let Some(entries) = registry_entries(dir) {
        return entries;
    }

    let mut all = Vec::new();
    all.extend(collect_channel_files(&dir.join("running")));
    all.extend(collect_channel_files(&dir.join("stopped")));
    if all.is_empty() {
        all.extend(collect_channel_files(dir));
    }
    dedup_installed(all)
}

fn installed_processes_in(dir: &Path) -> Vec<Installed> {
    registry_entries(dir).unwrap_or_else(|| collect_process_files(dir))
}

fn discover_tools(root: &Path) -> Vec<Installed> {
    let mut entries = installed_tools_in(&root.join("plugins/tools"));
    entries.extend(
        registry_entries(&root.join("tools"))
            .unwrap_or_else(|| collect_tool_files(&root.join("tools"))),
    );
    if let Some(parent) = root.parent() {
        entries.extend(
            registry_entries(&parent.join("tools"))
                .unwrap_or_else(|| collect_tool_files(&parent.join("tools"))),
        );
    }
    dedup_installed(entries)
}

fn discover_channels(root: &Path) -> Vec<Installed> {
    let mut entries = installed_channels_in(&root.join("plugins/channels"));
    entries.extend(
        registry_entries(&root.join("channels"))
            .unwrap_or_else(|| collect_channel_files(&root.join("channels"))),
    );
    if let Some(parent) = root.parent() {
        entries.extend(
            registry_entries(&parent.join("channels"))
                .unwrap_or_else(|| collect_channel_files(&parent.join("channels"))),
        );
    }
    dedup_installed(entries)
}

fn discover_processes(root: &Path) -> Vec<Installed> {
    let mut entries = installed_processes_in(&root.join("plugins/processes"));
    entries.extend(
        registry_entries(&root.join("processes"))
            .unwrap_or_else(|| collect_process_files(&root.join("processes"))),
    );
    if let Some(parent) = root.parent() {
        entries.extend(
            registry_entries(&parent.join("processes"))
                .unwrap_or_else(|| collect_process_files(&parent.join("processes"))),
        );
    }
    dedup_installed(entries)
}

fn registry_entries(dir: &Path) -> Option<Vec<Installed>> {
    for candidate in ["registry.json", "registry.yaml", "registry.yml"] {
        let path = dir.join(candidate);
        if !path.exists() {
            continue;
        }
        if let Ok(entries) = load_registry_entries(&path) {
            if !entries.is_empty() {
                return Some(entries);
            }
        }
    }
    None
}

fn load_registry_entries(path: &Path) -> Result<Vec<Installed>> {
    let bytes = fs::read(path)?;
    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
        return Ok(parse_registry_value(value));
    }
    if let Ok(value) = serde_yaml::from_slice::<Value>(&bytes) {
        return Ok(parse_registry_value(value));
    }
    Ok(Vec::new())
}

fn parse_registry_value(value: Value) -> Vec<Installed> {
    match value {
        Value::Array(items) => items.into_iter().filter_map(parse_registry_entry).collect(),
        Value::Object(map) => {
            if let Some(array) = map
                .get("entries")
                .or_else(|| map.get("items"))
                .or_else(|| map.get("tools"))
                .and_then(|v| v.as_array())
            {
                return array
                    .iter()
                    .cloned()
                    .filter_map(parse_registry_entry)
                    .collect();
            }

            map.into_iter()
                .filter_map(|(id, value)| match value {
                    Value::String(version) => Some(Installed {
                        id,
                        version: Some(version),
                    }),
                    Value::Object(obj) => {
                        let version = obj
                            .get("version")
                            .or_else(|| obj.get("version_req"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        Some(Installed { id, version })
                    }
                    Value::Null => Some(Installed { id, version: None }),
                    _ => None,
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn parse_registry_entry(value: Value) -> Option<Installed> {
    match value {
        Value::Object(mut obj) => {
            let id = obj
                .remove("id")
                .or_else(|| obj.remove("name"))?
                .as_str()
                .map(|s| s.to_string())?;
            let version = obj
                .remove("version")
                .or_else(|| obj.remove("version_req"))
                .and_then(|v| v.as_str().map(|s| s.to_string()));
            Some(Installed { id, version })
        }
        Value::String(id) => Some(Installed { id, version: None }),
        _ => None,
    }
}

fn collect_tool_files(dir: &Path) -> Vec<Installed> {
    collect_from_dir(dir, &["wasm"], |stem| vec![parse_named_identifier(stem)])
}

fn collect_process_files(dir: &Path) -> Vec<Installed> {
    collect_from_dir(dir, &["wasm"], |stem| vec![parse_named_identifier(stem)])
}

fn collect_channel_files(dir: &Path) -> Vec<Installed> {
    collect_from_dir(
        dir,
        &["", "exe", "sh", "bat", "ps1", "cmd", "dll", "so", "dylib"],
        parse_channel_identifiers,
    )
}

fn collect_from_dir<F>(dir: &Path, allowed_exts: &[&str], mut transform: F) -> Vec<Installed>
where
    F: FnMut(&str) -> Vec<Installed>,
{
    let mut results = BTreeSet::new();
    let mut stack = vec![dir.to_path_buf()];

    while let Some(current) = stack.pop() {
        if !current.exists() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&current) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }

                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !allowed_exts.is_empty() && !allowed_exts.contains(&ext) {
                    continue;
                }

                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if stem.starts_with('.') {
                        continue;
                    }
                    for installed in transform(stem) {
                        results.insert(installed);
                    }
                }
            }
        }
    }

    results.into_iter().collect()
}

fn parse_channel_identifiers(stem: &str) -> Vec<Installed> {
    if stem.is_empty() {
        return Vec::new();
    }

    let mut candidates = BTreeSet::new();
    candidates.insert(stem.to_string());

    let trimmed = stem
        .trim_start_matches("channel_")
        .trim_start_matches("channel-")
        .trim_start_matches("channel.");

    if trimmed != stem && !trimmed.is_empty() {
        candidates.insert(format!("channel_{}", trimmed));
        candidates.insert(format!("channel.{}", trimmed));
        candidates.insert(trimmed.to_string());
    }

    candidates
        .into_iter()
        .map(|id| parse_named_identifier(&id))
        .collect()
}

fn parse_named_identifier(raw: &str) -> Installed {
    let (name, version) = split_name_version(raw);
    Installed {
        id: name.to_string(),
        version: version.map(|v| v.to_string()),
    }
}

fn split_name_version(raw: &str) -> (&str, Option<&str>) {
    if let Some(idx) = raw.rfind('@') {
        let (name, version) = raw.split_at(idx);
        let version = &version[1..];
        if !name.is_empty() && !version.is_empty() {
            return (name, Some(version));
        }
    }
    (raw, None)
}

fn dedup_installed(values: Vec<Installed>) -> Vec<Installed> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter(|item| seen.insert((item.id.clone(), item.version.clone())))
        .collect()
}

fn present_configs_in(path: &Path) -> Vec<ScopedKey> {
    read_env_file(path)
}

fn present_secrets_in(path: &Path) -> Vec<ScopedKey> {
    read_env_file(path)
}

fn read_env_file(path: &Path) -> Vec<ScopedKey> {
    match dotenvy::from_path_iter(path) {
        Ok(iter) => iter
            .filter_map(|res| match res {
                Ok((key, _)) => parse_scoped_key(&key),
                Err(err) => {
                    warn!("Skipping malformed line in {}: {}", path.display(), err);
                    None
                }
            })
            .collect(),
        Err(dotenvy::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Vec::new()
        }
        Err(err) => {
            warn!("Failed to read {}: {}", path.display(), err);
            Vec::new()
        }
    }
}

fn parse_scoped_key(raw: &str) -> Option<ScopedKey> {
    if raw.is_empty() {
        return None;
    }

    if let Some((scope, key)) = raw.split_once("::") {
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        return Some(ScopedKey {
            key: key.to_string(),
            owner: parse_owner(scope.trim()),
        });
    }

    Some(ScopedKey {
        key: raw.to_string(),
        owner: None,
    })
}

fn parse_owner(raw: &str) -> Option<RequirementOwner> {
    if raw.is_empty() {
        return None;
    }

    let (kind_str, id) = raw.split_once(':').or_else(|| raw.split_once('.'))?;
    let id = id.trim();
    if id.is_empty() {
        return None;
    }

    let kind = match kind_str.trim() {
        "tool" => RequirementKind::Tool,
        "channel" => RequirementKind::Channel,
        "process" => RequirementKind::Process,
        "agent" => RequirementKind::Agent,
        _ => return None,
    };

    Some(RequirementOwner::new(kind, id.to_string()))
}

fn tool_default_requirements(tool_id: &str) -> ComponentRequirements {
    match tool_id {
        "weather_api" => ComponentRequirements {
            configs: Vec::new(),
            secrets: vec![required_entry_with_desc(
                "WEATHERAPI_KEY",
                Some("WeatherAPI.com API key for weather_api tool"),
            )],
        },
        _ => ComponentRequirements::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manifest() -> SnapshotManifest {
        SnapshotManifest {
            format_version: "1".into(),
            created_at: "2024-01-01T00:00:00Z".into(),
            greentic_version: "0.2.2".into(),
            required_tools: vec![Req {
                id: "mcp.weather".into(),
                version_req: Some(">=1.2,<2".into()),
            }],
            required_channels: vec![Req {
                id: "channel.telegram".into(),
                version_req: None,
            }],
            required_processes: Vec::new(),
            required_agents: Vec::new(),
            required_configs: vec![ConfigReq {
                key: "WEATHER_API_KEY".into(),
                owner: RequirementOwner::new(RequirementKind::Tool, "mcp.weather"),
                description: None,
            }],
            optional_configs: Vec::new(),
            required_secrets: vec![SecretReq {
                key: "TELEGRAM_TOKEN".into(),
                owner: RequirementOwner::new(RequirementKind::Channel, "channel.telegram"),
                description: None,
            }],
            optional_secrets: Vec::new(),
        }
    }

    #[test]
    fn plan_reports_missing_items() {
        let manifest = make_manifest();
        let state = EnvironmentState::default();
        let plan = plan_from(&manifest, &state);

        assert!(!plan.ok);
        assert_eq!(plan.missing_tools.len(), 1);
        assert_eq!(plan.missing_channels.len(), 1);
        assert_eq!(plan.missing_configs.len(), 1);
        assert_eq!(plan.missing_secrets.len(), 1);
        assert!(plan.summary.starts_with("4 items missing"));
        assert!(!plan.suggested_commands.install.is_empty());
    }

    #[test]
    fn plan_ok_when_all_present() {
        let manifest = make_manifest();
        let state = EnvironmentState {
            tools: vec![Installed {
                id: "mcp.weather".into(),
                version: Some(">=1.2,<2".into()),
            }],
            channels: vec![Installed {
                id: "channel.telegram".into(),
                version: None,
            }],
            processes: Vec::new(),
            configs: vec![ScopedKey {
                key: "WEATHER_API_KEY".into(),
                owner: Some(RequirementOwner::new(RequirementKind::Tool, "mcp.weather")),
            }],
            secrets: vec![ScopedKey {
                key: "TELEGRAM_TOKEN".into(),
                owner: Some(RequirementOwner::new(
                    RequirementKind::Channel,
                    "channel.telegram",
                )),
            }],
        };

        let plan = plan_from(&manifest, &state);
        assert!(plan.ok);
        assert_eq!(plan.summary, "All requirements satisfied");
        assert!(plan.suggested_commands.install.is_empty());
    }
}
