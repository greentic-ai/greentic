//! Flow + environment validator
//!
//! Inspired by `schema.rs` this module provides one public async entry‐point
//! (`validate`) that is wired up by the CLI sub‑command `greentic validate`.
//!
//! ## Responsibilities
//! 1. **Syntax check** – ensure the provided file is valid YAML **or** JSON.
//! 2. **Against flow.schema.json** – convert to JSON and validate against the
//!    canonical flow schema generated by `schema.rs`.
//! 3. **Tool / Channel existence** – every tool+channel referenced in the flow
//!    must be available in the local installation (i.e. present in the `Executor`
//!    or `ChannelManager`).
//! 4. **Wasm present** – for each tool, a compiled Wasm module must be present
//!    at the expected path (`tools/<tool>.wasm`).
//! 5. **Config / Secret keys** – gather all *required* keys from tools **and**
//!    channels, then make sure they are present in their respective managers.
//!    If something is missing we print actionable instructions.
//!
//! The code purposefully errs on the side of *user friendliness*; we print
//! colourful diagnostics (via `anyhow::Error`) rather than stack traces.
//!
//! Note: this file focuses on core validation logic.  CLI glue‑code lives in
//! `src/main.rs` or wherever the `Commands::Validate` variant is handled.

use std::{collections::HashSet, fs, path::{Path, PathBuf}};

use anyhow::{anyhow, Context, Result};
use channel_plugin::message::LogLevel;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use schemars::schema_for;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_yaml_bw as serde_yaml;
use crate::{
    apps::{detect_host_target, make_executable}, channel::manager::ChannelManager, config::ConfigManager, executor::Executor, flow::{manager::Flow, session::InMemorySessionStore}, logger::{LogConfig, Logger, OpenTelemetryLogger}, secret::SecretsManager
};

/// Diagnostic result – collects *all* warnings & errors instead of failing fast.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Top‑level schema errors (flow.yaml malformed, missing fields, …)
    pub schema_errors:   Vec<String>,
    /// Referenced channels/tools not available locally
    pub missing_plugins: Vec<String>,
    /// Tools that exist but the `.wasm` binary is missing
    pub missing_wasm:    Vec<String>,
    /// Required config keys that are absent in the ConfigManager
    pub missing_config:  Vec<(String, String)>, // (key, description)
    /// Required secret keys that are absent in the SecretsManager
    pub missing_secrets: Vec<(String, String)>, // (key, description)
}

impl ValidationReport {
    pub fn ok(&self) -> bool {
        self.schema_errors.is_empty()
            && self.missing_plugins.is_empty()
            && self.missing_wasm.is_empty()
            && self.missing_config.is_empty()
            && self.missing_secrets.is_empty()
    }
}

/// Public entry‑point used by the CLI.
pub async fn validate(
    flow_file: PathBuf,
    root_dir:  PathBuf,
    tools_dir: PathBuf,
    secrets_manager: SecretsManager,
    config_manager: ConfigManager,
) -> Result<()> {
    // ---------------------------------------------------------------------
    // 0) Read + parse YAML / JSON
    // ---------------------------------------------------------------------
    let text = fs::read_to_string(&flow_file)
        .with_context(|| format!("failed to read {}", flow_file.display()))?;

    // YAML → serde_json::Value (allows handling both YAML & JSON seamlessly)
    let flow_value: Value = if flow_file.extension().and_then(|e| e.to_str()) == Some("jgtc") {
        serde_json::from_str(&text).context("invalid JSON")?
    } else {
        serde_yaml::from_str(&text).context("invalid YAML")?
    };

    // ---------------------------------------------------------------------
    // 1) Load **compiled** schema – or regenerate on the fly
    // ---------------------------------------------------------------------
    let schema_json_path = root_dir.join("schemas/flow.schema.json");
    let schema_json: Value = if schema_json_path.exists() {
        let s = fs::read_to_string(&schema_json_path)?;
        serde_json::from_str(&s)?
    } else {
        // Fallback: derive schema from Flow struct (slower, but handy for dev)
        serde_json::to_value(&schema_for!(Flow)).unwrap()
    };

    let compiled = jsonschema::validator_for(&schema_json)
        .context("failed to compile flow schema")?;

    let mut report = ValidationReport::default();

    //    1a) Schema validation ------------------------------------------------
    for err in compiled.iter_errors(&flow_value) {
        report.schema_errors.push(err.to_string());
    }

    if !report.ok() {
        // Pretty print diagnostics
        if !report.schema_errors.is_empty() {
            eprintln!("\n❌ Flow schema errors:");
            for e in &report.schema_errors {
                eprintln!("  • {e}");
            }
        }

        return Err(anyhow!("validation failed – see diagnostics above"));
    }

    // ---------------------------------------------------------------------
    // 2) Spin up Executor + ChannelManager just like schema.rs  -------------
    // ---------------------------------------------------------------------
    //let _ = FileTelemetry::init_files(&log_level, log_file, event_file);
    let log_config = LogConfig::new(LogLevel::Info, Some(root_dir.join("logs").to_path_buf()), None);
    let logger  = Logger(Box::new(OpenTelemetryLogger::new()));
    let executor = Executor::new(secrets_manager.clone(), logger.clone());
    //let tool_watcher = executor.watch_tool_dir(tools_dir.clone()).await?;
    let sessions = InMemorySessionStore::new(10);
    let channel_mgr = ChannelManager::new(config_manager.clone(), secrets_manager.clone(), sessions, log_config).await?;

    // ---------------------------------------------------------------------
    // 3) Inspect flow JSON to collect all tool & channel IDs ---------------
    // ---------------------------------------------------------------------
    let mut used_tools: HashSet<(String,String)>    = HashSet::new();
    let mut used_channels = HashSet::new();

    collect_ids(&flow_value, &mut used_tools, &mut used_channels);

    let handle = secrets_manager.0.get("GREENTIC_TOKEN").expect("GREENTIC_TOKEN not set, please run 'greentic init' one time before calling 'greentic validate'");
    let token = secrets_manager.0.reveal(handle).await.unwrap().unwrap();

    let running_path = root_dir.join("plugins").join("channels").join("running");
    let stopped_path = root_dir.join("plugins").join("channels").join("stopped");
    // 3a) missing plugins ---------------------------------------------------
    for t in &used_channels {
         let channel_file = match cfg!(target_os = "windows") {
                true => format!("channel_{t}.exe"),
                false => format!("channel_{t}"),
            };

        let running_channel = running_path.join(channel_file.clone());
        let stopped_channel = stopped_path.join(channel_file.clone());

        if !running_channel.exists() && !stopped_channel.exists() {
            println!("Downloading {channel_file}");
            match pull_channel(&token,&channel_file, detect_host_target(),&running_channel).await {
                Ok(_) => {
                    tracing::info!("✅ Pulled and stored missing channel: {t}");
                }
                Err(e) => {
                    tracing::warn!("❌ Could not pull missing channel `{t}`: {e}");
                    report.missing_plugins.push(format!("channel:{t}"));
                    continue;
                }
            }
        }
    }

    // 3b) missing wasm files for tools -------------------------------------
    for (name,_action) in &used_tools {
        let wasm_file = format!("{name}.wasm");
        let wasm_path = tools_dir.join(wasm_file.clone());
        if !wasm_path.exists() {
            println!("Downloading {wasm_file}");
            match pull_wasm(&token,name, &wasm_path).await {
                Ok(_) => {
                    tracing::info!("Pulled missing Wasm for tool: {name}");
                    let _ = executor.add_tool(wasm_path).await;
                }
                Err(e) => {
                    tracing::warn!("Failed to pull Wasm for {name}: {e}");
                    report.missing_wasm.push(wasm_path.display().to_string());
                }
            }

        }
    }

    // 3c) channel required keys --------------------------------------------
    // then start watching
    let plugin_watcher = channel_mgr.clone().start_all(running_path.clone()).await?;
    for c in &used_channels {
        if let Some(wrapper) = channel_mgr.channel(c) {
            let req_cfg  = wrapper.list_config_keys().await.required_keys;
            let req_sec  = wrapper.list_secret_keys().await.required_keys;

            for (k, desc) in req_cfg {
                if config_manager.0.get(&k).await.is_none() {
                    report.missing_config.push((k, desc.unwrap_or_default()));
                }
            }
            for (k, desc) in req_sec {
                if secrets_manager.0.get(&k).is_none() {
                    report.missing_secrets.push((k, desc.unwrap_or_default()));
                }
            }
        } else {
            tracing::warn!("❗ Channel `{c}` still not found in manager after pulling.");
            report.missing_plugins.push(format!("channel:{c}"));
        }
    }

    // 3d) channel required keys --------------------------------------------
    for (name,action) in &used_tools {
        let tool_name = format!("{name}_{action}");
        if let Some(wrapper) = executor.get_tool(tool_name) {
            let req_sec  = wrapper.secrets();

            for secret in req_sec {
                if secret.required && secrets_manager.0.get(&secret.name).is_none() {
                    report.missing_secrets.push((secret.name, secret.description));
                }
            }
        }
    }

    if !report.ok() {
        // Pretty print diagnostics
        if !report.missing_plugins.is_empty() {
            eprintln!("\n❌ Missing channels / tools:");
            for p in &report.missing_plugins {
                if p.starts_with("tool:") {
                    let name = p.strip_prefix("tool:").unwrap();
                    eprintln!("  • {p} → run: greentic tool pull {name}");
                } else {
                    let name = p.strip_prefix("channel:").unwrap();
                    eprintln!("  • {p} → run: greentic channel pull {name}");
                }
            }
        }

        if !report.missing_wasm.is_empty() {
            eprintln!("\n❌ Missing Wasm binaries:");
            for w in &report.missing_wasm {
                eprintln!("  • {w}");
            }
        }

        if !report.missing_config.is_empty() {
            eprintln!("\n❌ Missing config values:");
            for (k, desc) in &report.missing_config {
                eprintln!("  • {k} — {desc}\n    ➜ greentic config add {k} <value>");
            }
        }

        if !report.missing_secrets.is_empty() {
            eprintln!("\n❌ Missing secret values:");
            for (k, desc) in &report.missing_secrets {
                eprintln!("  • {k} — {desc}\n    ➜ greentic secrets add {k} <secret>");
            }
        }
        let _ = channel_mgr.clone().stop_all();
        plugin_watcher.shutdown();

        return Err(anyhow!("validation failed – see diagnostics above"));
    }
    
    // ---------------------------------------------------------------------
    // 4) Output diagnostics -------------------------------------------------
    // ---------------------------------------------------------------------
    println!("✅ Validation passed: flow is ready to deploy");
    let _ = channel_mgr.clone().stop_all();
    plugin_watcher.shutdown();
    return Ok(());
}

// Basic pull function to get a tool. In the future we need login and 
// more advanced version management, ...
async fn pull_channel(token: &str, channel_name: &str, platform: &str, destination: &Path) -> anyhow::Result<()> {
    let url = format!("https://greenticstore.com/channels/{platform}/{channel_name}");
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", token))?)
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("Failed to download channel for channel_{channel_name}: HTTP {}", response.status());
    }

    let bytes = response.bytes().await?;
    fs::create_dir_all(destination.parent().unwrap())?;
    fs::write(destination, &bytes)?;
    make_executable(&destination)?;
    Ok(())
}

// Basic pull function to get a tool. In the future we need login and 
// more advanced version management, ...
async fn pull_wasm(token: &str, tool_name: &str, destination: &Path) -> anyhow::Result<()> {
    let url = format!("https://greenticstore.com/tools/{tool_name}.wasm");
    
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .header(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", token))?)
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!("Failed to download Wasm for {tool_name}: HTTP {}", response.status());
    }

    let bytes = response.bytes().await?;
    fs::create_dir_all(destination.parent().unwrap())?;
    fs::write(destination, &bytes)?;
    Ok(())
}
// -------------------------------------------------------------------------
// Helper: walk the flow JSON structure and collect `tool:` and `channel:`
// ids referenced in nodes.
// -------------------------------------------------------------------------
fn collect_ids(value: &Value, tools: &mut HashSet<(String,String)>, channels: &mut HashSet<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(t)) = map.get("tool") {
                // tool spec can be "weather_api.forecast_weather" → keep full id
                let name = t.get("name");
                let action = t.get("action");
                if name.is_some() && action.is_some() {
                    let tool_name = name.unwrap().as_str().unwrap();
                    let tool_action = action.unwrap().as_str().unwrap();
                    tools.insert((tool_name.to_string(),tool_action.to_string()));
                }
            }
            if let Some(Value::String(c)) = map.get("channel") {
                channels.insert(c.to_string());
            }
            for v in map.values() {
                collect_ids(v, tools, channels);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_ids(v, tools, channels);
            }
        }
        _ => {}
    }
}


#[cfg(test)]
mod tests {
    use crate::{config::MapConfigManager, secret::{EmptySecretsManager, EnvSecretsManager}};

    use super::*;
    use serde_json::json;
    use tempfile::tempdir;
    use std::fs;

    // ---------------------------------------------------------------------
    // helper: build a tiny YAML flow with a single node reference
    // ---------------------------------------------------------------------
    fn sample_flow_yaml(tool: &str, channel: &str) -> String {
        format!(r#"---
id: test
channels: [ {channel} ]
nodes:
  in:
    channel: {channel}
    in: true
  out:
    channel: {channel}
    out: true
  call:
    tool: {tool}
    parameters: {{}}
connections:
  - from: in
    to: call
  - from: call
    to: out
"#)
    }

    // ------------------------------------------------------------------
    // collect_ids helper covers: object recursion, arrays & primitives
    // ------------------------------------------------------------------
    #[test]
    fn test_collect_ids() {
        let doc = json!({
            "nodes": [
                { "tool": { 
                    "name": "weather", 
                    "action":"forecast" 
                }},
                { "channel": "telegram" }
            ]
        });
        let mut tools = std::collections::HashSet::new();
        let mut chs   = std::collections::HashSet::new();
        collect_ids(&doc, &mut tools, &mut chs);
        assert!(tools.contains(&("weather".to_string(),"forecast".to_string())));
        assert!(chs.contains("telegram"));
    }

    // ------------------------------------------------------------------
    // validate() happy‑path: minimal correct flow (no plugins installed)
    // should return an error that mentions missing plugins but *not* crash.
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn test_validate_reports_missing_plugins() {
        let tmp = tempdir().unwrap();
        let root_dir   = tmp.path().to_path_buf();
        let tools_dir  = root_dir.join("tools");
        fs::create_dir_all(&tools_dir).unwrap();

        // write dummy flow file
        let flow_file = root_dir.join("flow.ygtc");
        fs::write(&flow_file, sample_flow_yaml("weather_api.forecast", "telegram")).unwrap();

        let secrets_manager = SecretsManager(EmptySecretsManager::new());
        let config_manager = ConfigManager(MapConfigManager::new());

        // run validator – expect an Err because nothing is installed
        let res = validate(
            flow_file,
            root_dir.clone(),
            tools_dir,
            secrets_manager,
            config_manager,
        ).await;

        assert!(res.is_err(), "validation should fail due to missing plugins");
    }

    // ------------------------------------------------------------------
    // validate() detects YAML syntax errors
    // ------------------------------------------------------------------
    #[tokio::test]
    async fn test_validate_bad_yaml() {
        let tmp = tempdir().unwrap();
        let root_dir   = tmp.path().to_path_buf();
        let tools_dir  = root_dir.join("tools");
        fs::create_dir_all(&tools_dir).unwrap();

        let flow_file = root_dir.join("bad.ygtc");
        fs::write(&flow_file, "this is : not : yaml").unwrap();

        let secrets_manager = SecretsManager(EmptySecretsManager::new());
        let config_manager = ConfigManager(MapConfigManager::new());

        let res = validate(
            flow_file,
            root_dir.clone(),
            tools_dir,
            secrets_manager,
            config_manager,
        ).await;

        // Should surface a serde_yaml parsing error
        assert!(res.is_err());
        let msg = format!("{res:?}");
        assert!(msg.contains("invalid YAML") || msg.contains("while parsing"));
    }

    // --------- Pull a known working channel (ws) ---------------------------
    #[tokio::test]
    async fn test_pull_real_channel_ws() {
        let dir = tempdir().unwrap();
        let dest = dir.path().join("channel_ws");
        let secrets_manager = SecretsManager(EnvSecretsManager::new(Some(Path::new("./greentic/secrets/").to_path_buf())));
        let handle = secrets_manager.0.get("GREENTIC_TOKEN").expect("GREENTIC_TOKEN not set, please run 'greentic init' one time before calling 'greentic validate'");
        let token = secrets_manager.0.reveal(handle).await.unwrap().unwrap();
        let result = pull_channel(&token, "ws",detect_host_target(), &dest).await;
        assert!(result.is_ok(), "expected successful pull: {result:?}");
        assert!(dest.exists(), "destination file not created");
        assert!(fs::metadata(&dest).unwrap().len() > 0, "file is empty");
    }

    // --------- Pull a known working tool (weather_api) ---------------------
    #[tokio::test]
    async fn test_pull_real_tool_weather_api() {
        let dir = tempdir().unwrap();
        let dest = dir.path().join("weather_api.wasm");
        let secrets_manager = SecretsManager(EnvSecretsManager::new(Some(Path::new("./greentic/secrets/").to_path_buf())));
        let handle = secrets_manager.0.get("GREENTIC_TOKEN").expect("GREENTIC_TOKEN not set, please run 'greentic init' one time before calling 'greentic validate'");
        let token = secrets_manager.0.reveal(handle).await.unwrap().unwrap();
        let result = pull_wasm(&token,"weather_api", &dest).await;
        assert!(result.is_ok(), "expected successful pull: {result:?}");
        assert!(dest.exists(), "destination file not created");
        assert!(fs::metadata(&dest).unwrap().len() > 0, "file is empty");
    }

    // --------- Pull a missing tool -----------------------------------------
    #[tokio::test]
    async fn test_pull_missing_tool() {
        let dir = tempdir().unwrap();
        let dest = dir.path().join("nonexistent.wasm");
        let secrets_manager = SecretsManager(EnvSecretsManager::new(Some(Path::new("./greentic/secrets/").to_path_buf())));
        let handle = secrets_manager.0.get("GREENTIC_TOKEN").expect("GREENTIC_TOKEN not set, please run 'greentic init' one time before calling 'greentic validate'");
        let token = secrets_manager.0.reveal(handle).await.unwrap().unwrap();
        let result = pull_wasm(&token, "nonexistent", &dest).await;
        assert!(result.is_err(), "expected error but got success");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("HTTP 404") || err.contains("Failed to download"),
            "unexpected error: {err}"
        );
    }

    // --------- Pull a missing channel --------------------------------------
    #[tokio::test]
    async fn test_pull_missing_channel() {
        let dir = tempdir().unwrap();
        let dest = dir.path().join("channel_fake");
        let secrets_manager = SecretsManager(EnvSecretsManager::new(Some(Path::new("./greentic/secrets/").to_path_buf())));
        let handle = secrets_manager.0.get("GREENTIC_TOKEN").expect("GREENTIC_TOKEN not set, please run 'greentic init' one time before calling 'greentic validate'");
        let token = secrets_manager.0.reveal(handle).await.unwrap().unwrap();
        let result = pull_channel(&token,"fake", detect_host_target(),&dest).await;
        assert!(result.is_err(), "expected error but got success");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("HTTP 404") || err.contains("Failed to download"),
            "unexpected error: {err}"
        );
    }
}