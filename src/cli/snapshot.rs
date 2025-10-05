use std::{path::PathBuf, process};

use anyhow::{Context, bail};
use clap::{Args, Subcommand};
use tempfile::{TempDir, tempdir};

use super::CliContext;
use greentic::snapshot::{EnvironmentState, load_snapshot, plan_from};

use super::export::export_runtime;

#[derive(Args, Debug)]
pub struct SnapshotArgs {
    #[command(subcommand)]
    pub command: SnapshotCommands,
}

#[derive(Subcommand, Debug)]
pub enum SnapshotCommands {
    /// Validate a snapshot manifest against the local environment
    Validate(SnapshotValidateArgs),

    /// Print the manifest embedded in a snapshot file
    Manifest(SnapshotManifestArgs),
}

#[derive(Args, Debug)]
pub struct SnapshotValidateArgs {
    /// Optional path to a .gtc snapshot; mutually exclusive with --take
    #[arg(long)]
    pub file: Option<PathBuf>,

    /// Output machine-readable JSON only
    #[arg(long)]
    pub json: bool,

    /// Export a fresh snapshot before validating
    #[arg(long)]
    pub take: bool,
}

#[derive(Args, Debug)]
pub struct SnapshotManifestArgs {
    /// Path to a .gtc snapshot file
    #[arg(long)]
    pub file: PathBuf,

    /// Emit compact JSON (defaults to pretty JSON)
    #[arg(long)]
    pub json: bool,
}

pub async fn execute(args: SnapshotArgs, context: &CliContext) -> anyhow::Result<()> {
    match args.command {
        SnapshotCommands::Validate(validate_args) => {
            handle_snapshot_validate(context, validate_args).await?;
            Ok(())
        }
        SnapshotCommands::Manifest(manifest_args) => {
            handle_snapshot_manifest(manifest_args)?;
            Ok(())
        }
    }
}

async fn handle_snapshot_validate(
    context: &CliContext,
    args: SnapshotValidateArgs,
) -> anyhow::Result<()> {
    let SnapshotValidateArgs { file, json, take } = args;

    if file.is_some() && take {
        bail!("--file and --take cannot be used together");
    }

    let mut temp_dir_holder: Option<TempDir> = None;

    let snapshot_path = if let Some(path) = file {
        path
    } else if take {
        let temp_dir =
            tempdir().context("failed to create temporary directory for snapshot export")?;
        let export_path = export_runtime(
            context.root.clone(),
            context.secrets_manager.clone(),
            context.config_manager.clone(),
            None,
            Some(temp_dir.path().to_path_buf()),
        )
        .await
        .context("failed to export runtime before validation")?;
        temp_dir_holder = Some(temp_dir);
        export_path
    } else {
        bail!("Provide --file <path> or use --take to export a fresh snapshot before validation");
    };

    let snapshot = load_snapshot(&snapshot_path)
        .with_context(|| format!("Failed to load snapshot at {}", snapshot_path.display()))?;

    let env_state = EnvironmentState::discover();
    let plan = plan_from(&snapshot.manifest, &env_state);

    let output = if json {
        serde_json::to_string(&plan)?
    } else {
        serde_json::to_string_pretty(&plan)?
    };

    println!("{}", output);

    drop(temp_dir_holder);

    if plan.ok {
        Ok(())
    } else {
        process::exit(2);
    }
}

fn handle_snapshot_manifest(args: SnapshotManifestArgs) -> anyhow::Result<()> {
    let snapshot = load_snapshot(&args.file)
        .with_context(|| format!("Failed to load snapshot at {}", args.file.display()))?;

    let output = if args.json {
        serde_json::to_string(&snapshot.manifest)?
    } else {
        serde_json::to_string_pretty(&snapshot.manifest)?
    };

    println!("{}", output);
    Ok(())
}
