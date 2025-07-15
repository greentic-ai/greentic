use anyhow::{Context, Result, bail};
use serde_json::Value as JsonValue;
use serde_yaml_bw::Value as YamlValue;
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::info;

use crate::{config::ConfigManager, secret::SecretsManager, validate::validate};

/// Validate that the provided file is a valid YAML or JSON flow definition.
pub async fn validate_flow_file(
    flow_file: PathBuf,
    root_dir: PathBuf,
    tools_dir: PathBuf,
    secrets_maanger: SecretsManager,
    config_manager: ConfigManager,
) -> Result<()> {
    if !flow_file.exists() {
        bail!("File does not exist: {}", flow_file.display());
    }

    let content = fs::read_to_string(&flow_file)
        .with_context(|| format!("Failed to read file: {}", flow_file.display()))?;

    if flow_file.extension().and_then(|s| s.to_str()) == Some("jgtc") {
        serde_json::from_str::<JsonValue>(&content)
            .with_context(|| format!("Invalid JSON in file: {}", flow_file.display()))?;
        info!("✅ Valid JSON flow: {}", flow_file.display());
    } else if flow_file.extension().and_then(|s| s.to_str()) == Some("ygtc") {
        serde_yaml_bw::from_str::<YamlValue>(&content)
            .with_context(|| format!("Invalid YAML in file: {}", flow_file.display()))?;
        info!("✅ Valid YAML flow: {}", flow_file.display());
    } else {
        bail!("Unsupported file extension for: {}", flow_file.display());
    }

    let result = validate(
        flow_file,
        root_dir,
        tools_dir,
        secrets_maanger,
        config_manager,
    )
    .await;
    if result.is_err() {
        bail!("Validation failed");
    }

    Ok(())
}

/// Deploy a flow file after validating and checking dependencies.
pub async fn deploy_flow_file(
    path: PathBuf,
    root: PathBuf,
    tools_dir: PathBuf,
    secrets_maanger: SecretsManager,
    config_manager: ConfigManager,
) -> Result<()> {
    validate_flow_file(
        path.clone(),
        root.clone(),
        tools_dir,
        secrets_maanger,
        config_manager,
    )
    .await?;

    let content = fs::read_to_string(&path)?;
    let json: JsonValue = if path.extension().and_then(|s| s.to_str()) == Some("jgtc") {
        serde_json::from_str(&content)?
    } else {
        let yaml: YamlValue = serde_yaml_bw::from_str(&content)?;
        serde_json::to_value(yaml)?
    };

    let missing = |section: &str, expected: &Vec<String>| -> Result<()> {
        let plugin_path = root.join("plugins").join(section).join("running");
        for name in expected {
            let file = plugin_path.join(format!("{}.wasm", name));
            if !file.exists() {
                bail!("Missing {section}: `{name}`. Please run: greentic {section} pull {name}");
            }
        }
        Ok(())
    };

    if let Some(channels) = json["channels"].as_array() {
        let names = channels
            .iter()
            .filter_map(|c| c["name"].as_str())
            .map(String::from)
            .collect();
        missing("channels", &names)?;
    }

    if let Some(tools) = json["tools"].as_array() {
        let names = tools
            .iter()
            .filter_map(|c| c["name"].as_str())
            .map(String::from)
            .collect();
        missing("tools", &names)?;
    }
    if let Some(processes) = json["processes"].as_array() {
        let names = processes
            .iter()
            .filter_map(|c| c["name"].as_str())
            .map(String::from)
            .collect();
        missing("processes", &names)?;
    }

    let from = path.clone();
    let dest_folder = root.join("flows/stopped");
    let dest = dest_folder.join(from.file_name().unwrap());
    fs::copy(&from, &dest)
        .with_context(|| format!("Failed to copy {} to {}", from.display(), dest.display()))?;

    println!("✅ Flow is ready to be started.");
    info!("✅ Flow is ready to be started.");
    Ok(())
}

/// Move a flow file between directories.
pub fn move_flow_file(name: &str, from: &Path, to: &Path) -> Result<()> {
    let from_yml = from.join(format!("{}.ygtc", name));
    let from_json = from.join(format!("{}.jgtc", name));
    let src = if from_yml.exists() {
        from_yml
    } else if from_json.exists() {
        from_json
    } else {
        if to.join(format!("{}.ygtc", name)).exists() || to.join(format!("{}.ygtc", name)).exists()
        {
            return Ok(());
        } else {
            bail!("No such flow file found for: {}", name);
        }
    };

    let dest = to.join(src.file_name().unwrap());
    fs::rename(&src, &dest)
        .with_context(|| format!("Failed to move {} to {}", src.display(), dest.display()))?;

    info!("✅ Moved {} to {}", src.display(), dest.display());
    Ok(())
}
