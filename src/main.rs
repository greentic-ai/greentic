use anyhow::{Context, bail};
use channel_plugin::message::LogLevel;
use clap::{Args, Parser, Subcommand};
use greentic::{
    apps::{App, cmd_init, download},
    config::{ConfigManager, EnvConfigManager},
    flow_commands::{deploy_flow_file, move_flow_file, validate_flow_file},
    logger::{FileTelemetry, Logger, init_tracing},
    runtime_snapshot::RuntimeSnapshot,
    schema::write_schema,
    secret::{EnvSecretsManager, SecretsManager},
};
use std::panic::catch_unwind;
use std::{env, fs, fs::File, io::BufReader, path::PathBuf, process};
use tokio::time::{Duration, sleep};
use tracing::dispatcher;
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(
    name = "greentic",
    about = "The Greener Digital Workers Platform",
    version = "0.2.3"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the runtime
    Run(RunArgs),

    /// Emit JSON‐Schema
    Schema(SchemaArgs),

    /// Initialize a fresh layout
    Init,

    /// Manage flows
    Flow(FlowArgs),

    /// Handle secrets
    Secrets(SecretArgs),

    /// Handle secrets
    Config(ConfigArgs),

    /// Handle export
    Export(ExportArgs),
}

#[derive(Args, Debug)]
struct SecretArgs {
    #[command(subcommand)]
    command: SecretCommands,
}

#[derive(Args, Debug)]
struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommands,
}

#[derive(Args, Debug)]
struct FlowArgs {
    #[command(subcommand)]
    command: FlowCommands,
}

#[derive(Args, Debug)]
struct ExportArgs {
    /// Optional output filename
    #[arg(long)]
    name: Option<String>,

    /// Optional output directory (defaults to ./greentic/imports)
    #[arg(long)]
    dir: Option<PathBuf>,
}

#[derive(Args, Debug)]
struct RunArgs {
    #[arg(long, default_value = "1800")]
    session_timeout: u64,
    /// Optional log level override (e.g. error, warn, info, debug, trace)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// OpenTelemetry endpoint (e.g. http://localhost:4317)
    #[arg(long)]
    otel_logs_endpoint: Option<String>,

    /// OpenTelemetry endpoint (e.g. http://localhost:4317)
    #[arg(long)]
    otel_events_endpoint: Option<String>,

    /// Allow to pass a saved state for ultra fast boot
    #[arg(long)]
    snapshot: Option<String>,
}

/// Emit JSON‐Schema for flows, tools and channels into `<root>/schemas`

#[derive(Args, Debug)]
struct SchemaArgs {
    /// Optional log level override for the internal tracer
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Write logs to file enabled. Default: true
    #[arg(long, default_value_t = true)]
    logs_enabled: bool,
}

#[derive(Subcommand, Debug)]
enum FlowCommands {
    Validate { file: PathBuf },
    Deploy { file: PathBuf },
    Pull { name: String },
    Start { name: String },
    Stop { name: String },
}

#[derive(Subcommand, Debug)]
enum SecretCommands {
    Add { key: String, secret: String },
    Update { key: String, secret: String },
    Delete { key: String },
}

#[derive(Subcommand, Debug)]
enum ConfigCommands {
    Add { key: String, value: String },
    Update { key: String, value: String },
    Delete { key: String },
}

/// Resolve the greentic root directory from the environment or use default.
pub fn resolve_root_dir() -> PathBuf {
    if let Ok(path) = env::var("GREENTIC_ROOT") {
        PathBuf::from(path)
    } else {
        PathBuf::from("./")
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    // config & secrets
    let root = resolve_root_dir().join("greentic");
    let config_dir = root.join("config");
    let secrets_dir = root.join("secrets");
    let env_file = config_dir.join(".env");
    let config_manager = ConfigManager(EnvConfigManager::new(env_file.clone()));
    let secrets_manager = SecretsManager(EnvSecretsManager::new(Some(secrets_dir.clone())));

    let cli = Cli::parse();
    match cli.command.unwrap_or(Commands::Run(RunArgs {
        session_timeout: 1800,
        log_level: "info".to_string(),
        otel_logs_endpoint: None,
        otel_events_endpoint: None,
        snapshot: None,
    })) {
        Commands::Run(args) => {
            let state: Option<PathBuf> = match args.snapshot {
                Some(state_string) => {
                    // does the state exist
                    let state = PathBuf::from(state_string.clone());
                    match state.exists() {
                        true => Some(state),
                        false => {
                            let error = format!(
                                "Greentic state does not exist. Please make sure {} points to a valid greentic state. Use greentic export to get a valid state.",
                                state_string.clone()
                            );
                            error!(error);
                            println!("{}", error);
                            process::exit(-1);
                        }
                    }
                }
                None => None,
            };
            run(
                root,
                args.session_timeout,
                args.log_level,
                args.otel_logs_endpoint,
                args.otel_events_endpoint,
                secrets_manager,
                config_manager,
                state,
            )
            .await?;
            Ok(())
        }
        Commands::Export(args) => {
            match export_runtime(
                root.clone(),
                secrets_manager,
                config_manager,
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
        Commands::Schema(args) => {
            let out_dir = root.join("schemas");
            let log_level = args.log_level;
            let log_file = root.join("logs/greentic-schema.log");
            let event_file = root.join("logs/greentic-schema.json");
            let tools_dir = root.join("plugins").join("tools");
            let channels_dir = root.join("plugins").join("channels").join("running");
            let _processes_dir = root.join("plugins").join("processes");
            fs::create_dir_all(&out_dir)?;
            write_schema(
                out_dir.clone(),
                tools_dir.clone(),
                channels_dir.clone(),
                vec![],
                log_level,
                log_file.clone(),
                event_file.clone(),
            )
            .await?;
            println!("Schemas written to {}", out_dir.display());
            process::exit(0);
        }
        Commands::Init => {
            let log_file = root.join("logs/greentic-init.log");
            let event_file = root.join("logs/greentic-init.json");
            let _ = FileTelemetry::init_files("info", log_file, event_file);
            cmd_init(root.clone()).await?;
            println!("Greentic has been initialised. You can start it with 'greentic run'");
            process::exit(0);
        }
        Commands::Secrets(secret_args) => match secret_args.command {
            SecretCommands::Add { key, secret } => {
                match secrets_manager.add_secret(&key, &secret).await {
                    Ok(_) => println!("✅ Secret added."),
                    Err(_) => eprintln!("❌ Secret could not be added."),
                }
                Ok(())
            }
            SecretCommands::Update { key, secret } => {
                match secrets_manager.update_secret(&key, &secret).await {
                    Ok(_) => println!("✅ Secret updated."),
                    Err(_) => eprintln!("❌ Secret could not be updated."),
                }
                Ok(())
            }
            SecretCommands::Delete { key } => {
                match secrets_manager.delete_secret(&key).await {
                    Ok(_) => println!("✅ Secret deleted."),
                    Err(_) => eprintln!("❌ Secret could not be deleted."),
                }
                Ok(())
            }
        },

        Commands::Config(config_args) => match config_args.command {
            ConfigCommands::Add { key, value } => {
                match config_manager.0.set(&key, &value).await {
                    Ok(_) => println!("✅ Config added."),
                    Err(_) => eprintln!("❌ Config could not be added."),
                }
                Ok(())
            }
            ConfigCommands::Update { key, value } => {
                match config_manager.0.set(&key, &value).await {
                    Ok(_) => println!("✅ Config updated."),
                    Err(_) => eprintln!("❌ Config could not be updated."),
                }
                Ok(())
            }
            ConfigCommands::Delete { key } => {
                config_manager.0.del(&key).await;
                Ok(())
            }
        },
        Commands::Flow(flow_args) => match flow_args.command {
            FlowCommands::Validate { file } => {
                let tools_dir = root.join("plugins").join("tools");
                validate_flow_file(file, root, tools_dir, secrets_manager, config_manager).await?;
                println!("✅ Flow file is valid.");
                Ok(())
            }
            FlowCommands::Pull { name } => {
                let token_handle = secrets_manager.0.get("GREENTIC_TOKEN");
                if token_handle.is_none() {
                    println!(
                        "Please run 'greentic init' one time before calling 'greentic pull ...'"
                    );
                    return Ok(());
                }
                let token = secrets_manager
                    .0
                    .reveal(token_handle.unwrap())
                    .await
                    .unwrap()
                    .unwrap();
                let out_dir = root.join("flows/running");
                let result = download(&token, "flows", &name, out_dir.clone()).await;
                if result.is_err() {
                    println!(
                        "Sorry we could not download {} because {:?}",
                        name,
                        result.err().unwrap().to_string()
                    );
                }
                let tools_dir = root.join("plugins").join("tools");
                validate_flow_file(
                    out_dir.join(name),
                    root,
                    tools_dir,
                    secrets_manager,
                    config_manager,
                )
                .await?;

                Ok(())
            }
            FlowCommands::Deploy { file } => {
                let tools_dir = root.join("plugins").join("tools");
                deploy_flow_file(file, root, tools_dir, secrets_manager, config_manager).await?;
                Ok(())
            }
            FlowCommands::Start { name } => {
                let from = root.join("flows/stopped");
                let to = root.join("flows/running");
                move_flow_file(&name, &from, &to)?;
                println!(
                    "✅ Flow `{}` started. Please make sure greentic is running, i.e. greentic run",
                    name
                );
                Ok(())
            }
            FlowCommands::Stop { name } => {
                let from = root.join("flows/running");
                let to = root.join("flows/stopped");
                move_flow_file(&name, &from, &to)?;
                println!("✅ Flow `{}` stopped.", name);
                Ok(())
            }
        },
    }
}

async fn run(
    root: PathBuf,
    session_timeout: u64,
    log_level: String,
    otel_logs_endpoint: Option<String>,
    otel_events_endpoint: Option<String>,
    secrets_manager: SecretsManager,
    config_manager: ConfigManager,
    snapshot: Option<PathBuf>,
) -> anyhow::Result<()> {
    let snapshot_data = if let Some(ref path) = snapshot {
        info!("Loading runtime snapshot from {}", path.display());
        let file = File::open(path)
            .with_context(|| format!("Failed to open snapshot at {}", path.display()))?;
        Some(
            serde_cbor::from_reader::<RuntimeSnapshot, _>(BufReader::new(file))
                .with_context(|| format!("Failed to deserialize snapshot at {}", path.display()))?,
        )
    } else {
        None
    };
    let flows_dir = root.join("flows/running");
    let log_file = "logs/greentic_logs.log".to_string();
    let log_dir = Some(root.join("logs"));
    let event_file = "logs/greentic_events.log".to_string();
    let tools_dir = root.join("plugins").join("tools");
    let processes_dir = root.join("plugins").join("processes");
    let channels_dir = root.join("plugins").join("channels/running");
    // tracing / logger
    let logger = init_tracing(
        root.clone(),
        log_file.clone(),
        event_file.clone(),
        log_level.clone(),
        otel_logs_endpoint.clone(),
        otel_events_endpoint.clone(),
    )
    .expect("could not create logger");

    info!("Greentic runtime starting up…");
    println!("Greentic runtime starting up…");

    if !root.exists() {
        let err = format!(
            "Root directory `{}` does not exist. \
                Please run `greentic init -r <your-dir>` first.",
            root.display()
        );
        error!("{}", err);
        bail!(err);
    }

    // bootstrap
    let mut app = App::new();
    let result = app
        .bootstrap(
            session_timeout.clone(),
            flows_dir.clone(),
            channels_dir.clone(),
            tools_dir.clone(),
            processes_dir.clone(),
            config_manager,
            logger,
            convert_level(log_level),
            log_dir,
            otel_logs_endpoint,
            secrets_manager,
            snapshot_data.as_ref(),
        )
        .await;
    if result.is_err() {
        error!(
            "Failed to bootstrap greentic runtime: {:#}",
            result.err().unwrap()
        );
        process::exit(1);
    };

    println!(
        r#"                       
             @%@@     @@@@         @@@@@@@  
            @%*#%##%@@%++%@   @@%##*+++*%@@ 
           @#========+++#@  @@#***++++*%@   
  @%%%%@@@@#*+===+=====#@ @%#*#*++++++*%    
 @***===++++**+=++*=====#@@#*@%######**%@   
@#=======++%============*@@@#%%#+++++*#%@@  
%*================+#+===+@ @###+##*+*@@     
@#===============*%=====+%%%*%*+++**%@      
 @@@@@@#+=====+#@#======#*#*#*+*#%@@@@      
   @%+#@#***#%%#===+===*+===+*%%@      @@@  
    @#==++*++===+%+=============#@  @@%*%@  
      @@%%###%%#+================+%@@%*=#@  
      @@++++================+=====*@@*+%@@  
      @@+=================+*======+#==#@    
       @+==============++=%=======+##@@     
       @*==#*===*======+#*%=======+%@       
       @*===*#@@#======*#*%*======#@        
      @@+===+#@@#======%@@@%*====+%@        
     @@+*#*+*%@@##+#*=+@@@%**#*+=#@         
    @@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@                             
        G R E E N T I C   A I 🦕

       Fast | Secure | Extendable
"#
    );

    info!("Greentic runtime running; press Ctrl‐C to exit");
    println!("Greentic runtime running; press Ctrl‐C to exit");

    // wait for CTRL‐C
    tokio::signal::ctrl_c().await?;

    println!("\nShutting down…");
    info!("Greentic runtime shutting down");

    app.shutdown().await;

    println!("Goodbye!");

    process::exit(0);
}

async fn export_runtime(
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
    fs::create_dir_all(&base_dir).with_context(|| {
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
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create export destination directory at {}",
                parent.display()
            )
        })?;
    }

    {
        let mut file = File::create(&destination)
            .with_context(|| format!("failed to create export file {}", destination.display()))?;
        serde_cbor::to_writer(&mut file, &snapshot).with_context(|| {
            format!("failed to serialize snapshot to {}", destination.display())
        })?;
    }

    app.shutdown().await;

    Ok(destination)
}

fn convert_level(level: String) -> LogLevel {
    match level.to_lowercase().as_str() {
        "trace" => LogLevel::Trace,
        "debug" => LogLevel::Debug,
        "info" => LogLevel::Info,
        "warn" => LogLevel::Warn,
        "error" => LogLevel::Error,
        "critical" => LogLevel::Critical,
        _ => LogLevel::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use greentic::apps::App;
    use greentic::logger::Logger;
    use greentic::logger::OpenTelemetryLogger;
    use greentic::secret::TestSecretsManager;
    use tempfile::TempDir;
    use greentic::config::MapConfigManager;

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
        let snapshot: RuntimeSnapshot =
            serde_cbor::from_reader(BufReader::new(file)).expect("snapshot should deserialize");
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
        let snapshot: RuntimeSnapshot =
            serde_cbor::from_reader(BufReader::new(file)).expect("snapshot should deserialize");

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
