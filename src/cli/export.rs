use std::{fs::File, path::PathBuf, process, time::Duration};

use anyhow::{Context, bail};
use clap::Args;
use tokio::time::sleep;
use tracing::dispatcher;
use tracing::error;

use super::CliContext;
use channel_plugin::message::LogLevel;
use greentic::apps::App;
use greentic::config::ConfigManager;
use greentic::logger::{Logger, init_tracing};
use greentic::secret::SecretsManager;
use greentic::snapshot::{self, GreenticSnapshot};
use std::panic::catch_unwind;

#[derive(Args, Debug)]
pub struct ExportArgs {
    /// Optional output filename
    #[arg(long)]
    pub name: Option<String>,

    /// Optional output directory (defaults to ./greentic/imports)
    #[arg(long)]
    pub dir: Option<PathBuf>,
}

pub async fn execute(args: ExportArgs, context: &CliContext) -> anyhow::Result<()> {
    match export_runtime(
        context.root.clone(),
        context.secrets_manager.clone(),
        context.config_manager.clone(),
        args.name,
        args.dir,
    )
    .await
    {
        Ok(final_path) => {
            println!("✅ Exported runtime snapshot to {}", final_path.display());
            process::exit(0);
        }
        Err(err) => {
            error!("Export failed: {err:#}");
            println!("❌ Export failed: {err:#}");
            process::exit(-1);
        }
    }
}

pub async fn export_runtime(
    root: PathBuf,
    secrets_manager: SecretsManager,
    config_manager: ConfigManager,
    export_name: Option<String>,
    export_dir: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    const EXPORT_SESSION_TIMEOUT: u64 = 1800;
    const EXPORT_DELAY_MS: u64 = 500;

    if !root.exists() {
        bail!(
            "Root directory `{}` does not exist. Please run `greentic init` first.",
            root.display()
        );
    }

    let flows_dir = root.join("flows/running");
    let tools_dir = root.join("plugins").join("tools");
    let processes_dir = root.join("plugins").join("processes");
    let channels_dir = root.join("plugins").join("channels/running");

    let log_dir = Some(root.join("logs"));
    let log_file = "logs/greentic_export.log".to_string();
    let event_file = "logs/greentic_export_events.log".to_string();

    let logger = if dispatcher::has_been_set() {
        Logger(Box::new(greentic::logger::OpenTelemetryLogger::new()))
    } else {
        match catch_unwind(|| {
            init_tracing(
                root.clone(),
                log_file,
                event_file,
                "info".to_string(),
                None,
                None,
            )
        }) {
            Ok(Ok(logger)) => logger,
            Ok(Err(err)) => {
                println!(
                    "Tracing initialization failed ({}); falling back to basic logger",
                    err
                );
                Logger(Box::new(greentic::logger::OpenTelemetryLogger::new()))
            }
            Err(_) => {
                println!("Tracing initialization panicked; falling back to basic logger");
                Logger(Box::new(greentic::logger::OpenTelemetryLogger::new()))
            }
        }
    };

    let mut app = App::new();
    app.bootstrap(
        EXPORT_SESSION_TIMEOUT,
        flows_dir,
        channels_dir,
        tools_dir,
        processes_dir,
        config_manager,
        logger,
        LogLevel::Info,
        log_dir,
        None,
        secrets_manager,
        None,
    )
    .await
    .context("failed to bootstrap runtime for export")?;

    sleep(Duration::from_millis(EXPORT_DELAY_MS)).await;

    let snapshot = match app.snapshot() {
        Ok(snapshot) => snapshot,
        Err(err) => {
            app.shutdown().await;
            return Err(err);
        }
    };

    let base_dir = export_dir.unwrap_or_else(|| root.join("imports"));
    std::fs::create_dir_all(&base_dir).with_context(|| {
        format!(
            "failed to create exports directory at {}",
            base_dir.display()
        )
    })?;

    let file_name = export_name.unwrap_or_else(|| {
        let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        format!("snapshot-{}.gtc", timestamp)
    });

    let destination = base_dir.join(file_name);

    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create export destination directory at {}",
                parent.display()
            )
        })?;
    }

    let manifest = snapshot::manifest_from_runtime(&snapshot);
    let payload =
        serde_cbor::to_vec(&snapshot).context("failed to serialize runtime snapshot payload")?;
    let wrapped = GreenticSnapshot { manifest, payload };

    {
        let mut file = File::create(&destination)
            .with_context(|| format!("failed to create export file {}", destination.display()))?;
        serde_cbor::to_writer(&mut file, &wrapped).with_context(|| {
            format!("failed to serialize snapshot to {}", destination.display())
        })?;
    }

    app.shutdown().await;

    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use greentic::config::MapConfigManager;
    use greentic::logger::OpenTelemetryLogger;
    use greentic::runtime_snapshot::RuntimeSnapshot;
    use greentic::secret::TestSecretsManager;
    use greentic::snapshot::GreenticSnapshot;
    use std::fs::{self, File};
    use std::io::BufReader;
    use tempfile::TempDir;

    fn create_test_layout(root: &PathBuf) {
        let dirs = [
            "config",
            "secrets",
            "logs",
            "flows/running",
            "flows/stopped",
            "plugins/tools",
            "plugins/channels/running",
            "plugins/channels/stopped",
            "plugins/processes",
        ];
        for d in &dirs {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        fs::write(root.join("config/.env"), "").unwrap();
    }

    async fn make_test_secrets() -> SecretsManager {
        let secrets = SecretsManager(TestSecretsManager::new());
        secrets
            .add_secret("GREENTIC_ID", "test-greentic-id")
            .await
            .unwrap();
        secrets
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn export_writes_snapshot() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("greentic");
        create_test_layout(&root);

        let secrets = make_test_secrets().await;
        let config = ConfigManager(MapConfigManager::new());

        let export_path = export_runtime(
            root.clone(),
            secrets,
            config,
            Some("test-snapshot.gtc".into()),
            None,
        )
        .await
        .expect("export should succeed");

        assert!(export_path.exists());

        let file = File::open(export_path).unwrap();
        let wrapper: GreenticSnapshot =
            serde_cbor::from_reader(BufReader::new(file)).expect("snapshot should deserialize");
        let snapshot: RuntimeSnapshot =
            serde_cbor::from_slice(&wrapper.payload).expect("payload should deserialize");
        assert_eq!(snapshot.version, RuntimeSnapshot::CURRENT_VERSION);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bootstrap_from_snapshot() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("greentic");
        create_test_layout(&root);

        let secrets = make_test_secrets().await;
        let config = ConfigManager(MapConfigManager::new());

        let snapshot_path = export_runtime(
            root.clone(),
            secrets.clone(),
            config.clone(),
            Some("preload.gtc".into()),
            None,
        )
        .await
        .expect("export should succeed");

        let file = File::open(snapshot_path).unwrap();
        let wrapper: GreenticSnapshot =
            serde_cbor::from_reader(BufReader::new(file)).expect("snapshot should deserialize");
        let snapshot: RuntimeSnapshot =
            serde_cbor::from_slice(&wrapper.payload).expect("payload should deserialize");

        let flows_dir = root.join("flows/running");
        let channels_dir = root.join("plugins/channels/running");
        let tools_dir = root.join("plugins/tools");
        let processes_dir = root.join("plugins/processes");
        let logger = Logger(Box::new(OpenTelemetryLogger::new()));

        let mut app = App::new();
        app.bootstrap(
            60,
            flows_dir,
            channels_dir,
            tools_dir,
            processes_dir,
            config,
            logger,
            LogLevel::Info,
            Some(root.join("logs")),
            None,
            secrets,
            Some(&snapshot),
        )
        .await
        .expect("bootstrap with snapshot should succeed");

        let captured = app.snapshot().expect("snapshot capture should succeed");
        assert_eq!(captured.version, RuntimeSnapshot::CURRENT_VERSION);
        app.shutdown().await;
    }
}
