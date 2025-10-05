use anyhow::Result;
use assert_cmd::Command;
use greentic::runtime_snapshot::RuntimeSnapshot;
use greentic::snapshot::{
    ConfigReq, EnvironmentState, GreenticSnapshot, Installed, Req, RequirementKind,
    RequirementOwner, ScopedKey, SecretReq, SnapshotManifest, load_snapshot, plan_from,
};
use std::fs::File;
use std::path::PathBuf;
use tempfile::{NamedTempFile, TempDir, tempdir};

fn write_snapshot_file(manifest: &SnapshotManifest) -> Result<(TempDir, PathBuf)> {
    let dir = tempdir()?;
    let path = dir.path().join("snapshot.gtc");
    let snapshot = GreenticSnapshot {
        manifest: manifest.clone(),
        payload: serde_cbor::to_vec(&RuntimeSnapshot::empty())?,
    };
    let mut file = File::create(&path)?;
    serde_cbor::to_writer(&mut file, &snapshot)?;
    Ok((dir, path))
}

#[test]
fn manifest_round_trip_cbor() -> Result<()> {
    let manifest = SnapshotManifest {
        format_version: "1".into(),
        created_at: "2024-01-01T00:00:00Z".into(),
        greentic_version: "0.0-test".into(),
        required_tools: vec![Req {
            id: "demo_tool".into(),
            version_req: None,
        }],
        required_channels: Vec::new(),
        required_processes: Vec::new(),
        required_agents: Vec::new(),
        required_configs: Vec::new(),
        optional_configs: Vec::new(),
        required_secrets: Vec::new(),
        optional_secrets: Vec::new(),
    };

    let file = NamedTempFile::new()?;
    let path = file.path().to_path_buf();
    let snapshot = GreenticSnapshot {
        manifest: manifest.clone(),
        payload: serde_cbor::to_vec(&RuntimeSnapshot::empty())?,
    };
    let mut handle = File::create(&path)?;
    serde_cbor::to_writer(&mut handle, &snapshot)?;

    let loaded = load_snapshot(path.as_path())?;
    assert_eq!(loaded.manifest, manifest);
    Ok(())
}

#[test]
fn plan_ok_when_everything_present() {
    let manifest = SnapshotManifest {
        format_version: "1".into(),
        created_at: "2024-01-01T00:00:00Z".into(),
        greentic_version: "0.0-test".into(),
        required_tools: vec![Req {
            id: "demo_tool".into(),
            version_req: None,
        }],
        required_channels: Vec::new(),
        required_processes: Vec::new(),
        required_agents: Vec::new(),
        required_configs: vec![ConfigReq {
            key: "DEMO_CONFIG".into(),
            owner: RequirementOwner::new(RequirementKind::Tool, "demo_tool"),
            description: None,
        }],
        optional_configs: Vec::new(),
        required_secrets: vec![SecretReq {
            key: "DEMO_SECRET".into(),
            owner: RequirementOwner::new(RequirementKind::Tool, "demo_tool"),
            description: None,
        }],
        optional_secrets: Vec::new(),
    };

    let state = EnvironmentState {
        tools: vec![Installed {
            id: "demo_tool".into(),
            version: None,
        }],
        channels: Vec::new(),
        processes: Vec::new(),
        configs: vec![ScopedKey {
            key: "DEMO_CONFIG".into(),
            owner: Some(RequirementOwner::new(RequirementKind::Tool, "demo_tool")),
        }],
        secrets: vec![ScopedKey {
            key: "DEMO_SECRET".into(),
            owner: Some(RequirementOwner::new(RequirementKind::Tool, "demo_tool")),
        }],
    };

    let plan = plan_from(&manifest, &state);
    assert!(plan.ok);
    assert!(plan.missing_tools.is_empty());
    assert!(plan.missing_secrets.is_empty());
    assert!(plan.suggested_commands.install.is_empty());
    assert!(plan.suggested_commands.secret.is_empty());
}

#[test]
fn plan_missing_reports_and_commands() {
    let manifest = SnapshotManifest {
        format_version: "1".into(),
        created_at: "2024-01-01T00:00:00Z".into(),
        greentic_version: "0.0-test".into(),
        required_tools: vec![Req {
            id: "missing_tool".into(),
            version_req: None,
        }],
        required_channels: Vec::new(),
        required_processes: Vec::new(),
        required_agents: Vec::new(),
        required_configs: Vec::new(),
        optional_configs: Vec::new(),
        required_secrets: vec![SecretReq {
            key: "IMPORTANT_SECRET".into(),
            owner: RequirementOwner::new(RequirementKind::Tool, "missing_tool"),
            description: None,
        }],
        optional_secrets: Vec::new(),
    };

    let plan = plan_from(&manifest, &EnvironmentState::default());
    assert!(!plan.ok);
    assert_eq!(plan.missing_tools.len(), 1);
    assert_eq!(plan.missing_secrets.len(), 1);
    assert!(
        plan.suggested_commands
            .install
            .contains(&"greentic store install tool missing_tool@latest".to_string())
    );
    assert!(plan.suggested_commands.secret.contains(
        &"greentic secrets add IMPORTANT_SECRET <VALUE>  # tool:missing_tool".to_string()
    ));
}

#[test]
fn cli_validate_exit_codes() -> Result<()> {
    let ok_manifest = SnapshotManifest {
        format_version: "1".into(),
        created_at: "2024-01-01T00:00:00Z".into(),
        greentic_version: "0.0-test".into(),
        required_tools: Vec::new(),
        required_channels: Vec::new(),
        required_processes: Vec::new(),
        required_agents: Vec::new(),
        required_configs: Vec::new(),
        optional_configs: Vec::new(),
        required_secrets: Vec::new(),
        optional_secrets: Vec::new(),
    };

    let missing_manifest = SnapshotManifest {
        format_version: "1".into(),
        created_at: "2024-01-01T00:00:00Z".into(),
        greentic_version: "0.0-test".into(),
        required_tools: vec![Req {
            id: "cli_missing_tool".into(),
            version_req: None,
        }],
        required_channels: Vec::new(),
        required_processes: Vec::new(),
        required_agents: Vec::new(),
        required_configs: Vec::new(),
        optional_configs: Vec::new(),
        required_secrets: Vec::new(),
        optional_secrets: Vec::new(),
    };

    let (ok_dir, ok_path) = write_snapshot_file(&ok_manifest)?;
    let (missing_dir, missing_path) = write_snapshot_file(&missing_manifest)?;

    let greentic_root_ok = tempdir()?;
    std::fs::create_dir_all(greentic_root_ok.path().join("greentic"))?;

    Command::cargo_bin("greentic")?
        .env("GREENTIC_ROOT", greentic_root_ok.path())
        .args([
            "snapshot",
            "validate",
            "--file",
            ok_path.to_string_lossy().as_ref(),
        ])
        .assert()
        .success();

    let greentic_root_missing = tempdir()?;
    std::fs::create_dir_all(greentic_root_missing.path().join("greentic"))?;

    Command::cargo_bin("greentic")?
        .env("GREENTIC_ROOT", greentic_root_missing.path())
        .args([
            "snapshot",
            "validate",
            "--file",
            missing_path.to_string_lossy().as_ref(),
        ])
        .assert()
        .code(2);

    // keep directories alive until after commands run
    drop(ok_dir);
    drop(missing_dir);
    drop(greentic_root_ok);
    drop(greentic_root_missing);

    Ok(())
}
