// src/schema.rs

use std::{collections::HashSet, fs, path::PathBuf, sync::Arc};

use anyhow::Error;
use channel_plugin::plugin::LogLevel;
use schemars::schema_for;
use serde_json::{json, Value};

use crate::{
    channel::manager::{ChannelManager, HostLogger}, config::{ConfigManager, MapConfigManager}, executor::Executor, flow::{manager::Flow, session::InMemorySessionStore}, logger::{FileTelemetry, Logger, OpenTelemetryLogger}, secret::{EmptySecretsManager, SecretsManager},
};

/// The entry point invoked by `main.rs` for `Commands::Schema`.
pub async fn write_schema(
    out_dir: PathBuf,
    root_dir: PathBuf,
    tools_dir: PathBuf,
    log_level: String,
    log_dir: String,
    event_dir: String,
) -> Result<(), Error> {
    fs::create_dir_all(&out_dir)?;

    // 2) flow schema
    let flow_schema = schema_for!(Flow);
    let flow_json = serde_json::to_string_pretty(&flow_schema)?;
    fs::write(out_dir.join("flow.schema.json"), flow_json)?;

    // 3) tool schemas
    let _ = FileTelemetry::init_files(
        log_level.as_str(),
        root_dir.join(log_dir),
        root_dir.join(event_dir),
    );

    let logger = Logger(Box::new(OpenTelemetryLogger::new()));
    let secrets = SecretsManager(EmptySecretsManager::new());
    let executor = Executor::new(secrets.clone(), logger);
    executor
        .watch_tool_dir(tools_dir)
        .await
        .expect("Could not load tools");
    write_tools_schema(executor.clone(), &out_dir)?;

    // 4) channel schemas
    let config = ConfigManager(MapConfigManager::new());
    
    let host_logger = HostLogger::new(LogLevel::Warn);
    let store =InMemorySessionStore::new(10);
    let channel_mgr = ChannelManager::new( config, secrets, store.clone(), host_logger)
        .await
        .expect("Could not start channels");
    
    for wrapper in channel_mgr.channels().iter() {
        let (name, schema) = wrapper.wrapper().schema_json()?;
        let filename = format!("channel-{}.schema.json", name.to_lowercase());
        fs::write(out_dir.join(filename), schema)?;
    }

    Ok(())
}

/// Emit each tool’s parameter+secret schema under `out_dir/tool-<tool_id>/`.
fn write_tools_schema(executor: Arc<Executor>, out_dir: &PathBuf) -> anyhow::Result<()> {
    for key in executor.list_tool_keys() {
        let tool = executor
            .get_tool(key.clone())
            .expect("`key` came from `list_tool_keys`");

        // parameters schema
        let params_schema: Value = tool.parameters();

        // build properties object
        let mut props = serde_json::Map::new();
        props.insert("parameters".to_string(), params_schema);

        // secrets subschema, if any
        
        let mut required = Vec::new();
        let secret_keys = tool.secrets();
        if !secret_keys.is_empty() {
            let mut sec_props = serde_json::Map::new();
            for sk in &secret_keys {
                let mut field = serde_json::Map::new();
                field.insert("type".into(), json!("string"));

                field.insert("description".into(), json!(&sk.description));
                
                sec_props.insert(sk.name.clone(), Value::Object(field));
                if sk.required {
                    required.push(Value::String(sk.name.clone()));
                }
            }
            // de-duplicaet secrets
            let mut seen = HashSet::new();
            required.retain(|v| {
                if let Value::String(s) = v {
                    seen.insert(s.clone())
                } else {
                    true
                }
            });

            let mut sec_schema = serde_json::Map::new();
            sec_schema.insert("type".into(), json!("object"));
            sec_schema.insert("properties".into(), Value::Object(sec_props));
            if !required.is_empty() {
                sec_schema.insert("required".into(), Value::Array(required.clone()));
                props.insert("secrets".into(), Value::Object(sec_schema));
                // ensure top‐level requires "secrets"
                required.push(Value::String("secrets".into()));
            }
        }

        // assemble full schema
        let mut root = serde_json::Map::new();
        root.insert("$schema".into(), json!("http://json-schema.org/draft-07/schema#"));
        root.insert("title".into(), json!(tool.name()));
        root.insert("description".into(), json!(tool.description()));
        root.insert("type".into(), json!("object"));
        root.insert("properties".into(), Value::Object(props));
        if !required.is_empty() {
            root.insert("required".into(), Value::Array(required));
        }

        let json_text = serde_json::to_string_pretty(&Value::Object(root))?;

        // write under tool-<id>/tool-<id>.schema.json
        let tool_dir = out_dir.join(format!("tool-{}", key));
        fs::create_dir_all(&tool_dir)?;
        let filename = format!("tool-{}.schema.json", key);
        fs::write(tool_dir.join(filename), json_text)?;
    }

    Ok(())
}
