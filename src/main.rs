use clap::{Args, Parser, Subcommand};
use greentic::{
    apps::{cmd_init, App}, config::{ConfigManager, EnvConfigManager}, logger::init_tracing, schema::write_schema, secret::{EnvSecretsManager, SecretsManager}
};
use std::{fs, path::{Path, PathBuf}, process};
use tracing::{error, info};
use anyhow::bail;

#[derive(Parser, Debug)]
#[command(
    name = "greentic", 
    about = "The Greener Agentic AI", 
    version = "0.1.0"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Args, Debug)]
pub struct RootArg {
    /// where to look for flows, plugins, etc.
    #[arg(
        value_name = "GREENTIC_DIR",
        default_value = "./greentic",
    )]
    pub root: PathBuf,
}

#[derive(Subcommand, Debug)]
#[command(subcommand = "run")]
enum Commands {
    /// Run the runtime
    Run(RunArgs),

    /// Emit JSON‐Schema
    Schema(SchemaArgs),

    /// Initialize a fresh layout
    Init(InitArgs),
}


#[derive(Args, Debug)]
struct RunArgs {
    #[command(flatten)]
    root: RootArg,
        
    /// Optional log level override (e.g. error, warn, info, debug, trace)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// OpenTelemetry endpoint (e.g. http://localhost:4317)
    #[arg(long)]
    otel_logs_endpoint: String,

    /// OpenTelemetry endpoint (e.g. http://localhost:4317)
    #[arg(long)]
    otel_events_endpoint: String,
}

    /// Emit JSON‐Schema for flows, tools and channels into `<root>/schemas`

#[derive(Args, Debug)]
struct SchemaArgs {
    #[command(flatten)]
    root: RootArg,

    /// Optional log level override for the internal tracer
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Write logs to file enabled. Default: true
    #[arg(long, default_value_t = true)]
    logs_enabled: bool,
}

#[derive(Args, Debug)]
struct InitArgs {
    #[command(flatten)]
    root: RootArg,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse(); 
    match cli.command {
        Some(Commands::Run(args)) => {
            let root = args.root.root;
            run(root,
                args.log_level,
                Some(args.otel_logs_endpoint),
                Some(args.otel_events_endpoint),
            ).await?;
            Ok(())
        }
        Some(Commands::Schema(args)) => {
            let root = args.root.root;
            let out_dir = root.join("schemas");
            let log_level     = args.log_level;
            let log_file      = "logs/greentic-schema.log".to_string();
            let event_file      = "logs/greentic-schema.json".to_string();
            let tools_dir    = root.join("plugins").join("tools");
            let _processes_dir= root.join("plugins").join("processes");
            
            fs::create_dir_all(&out_dir)?;
            write_schema(
                out_dir.clone(),
                root.clone(),
                tools_dir.clone(),
                log_level,
                log_file.clone(),
                event_file.clone(),
            )
            .await?;
            println!("Schemas written to {}", out_dir.display());
            process::exit(0);
        }
        Some(Commands::Init(args)) => {
            let root = args.root.root;
            cmd_init(root.clone()).await?;
            println!("Initialized greentic layout at {}", root.display());
            process::exit(0);
        }
        None => {
            run(Path::new("./greentic").to_path_buf(), 
                "info".to_string(),
                None,
                None,
            ).await?;
            Ok(())
        }
    }
}

async fn run(root: PathBuf, 
    log_level: String,
    otel_logs_endpoint: Option<String>,
    otel_events_endpoint: Option<String>,
) -> anyhow::Result<()> {

    let flows_dir    = root.join("flows/running");
    let config_dir   = root.join("config");
    let secrets_dir  = root.join("secrets");
    let log_file      = "logs/greentic_logs.log".to_string();
    let event_file    = "logs/greentic_events.log".to_string();
    let tools_dir    = root.join("plugins").join("tools");
    let _processes_dir= root.join("plugins").join("processes");
    let channels_dir = root.join("plugins").join("channels/running");       
    // tracing / logger
    let logger = init_tracing(
        root.clone(),
        log_file.clone(),
        event_file.clone(),
        log_level.clone(),
        otel_logs_endpoint.clone(),
        otel_events_endpoint.clone(),
    ).expect("could not create logger");


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

    // config & secrets
    let env_file = config_dir.join(".env");
    let config_mgr = ConfigManager(EnvConfigManager::new(env_file.clone()));
    let secrets_mgr = SecretsManager(EnvSecretsManager::new(Some(secrets_dir.clone())));

    // bootstrap
    let mut app = App::new();
    let result = app.bootstrap(
        flows_dir.clone(),
        channels_dir.clone(),
        tools_dir.clone(),
        config_mgr,
        logger,
        secrets_mgr,
    )
    .await;
    if result.is_err()
    {
            error!("Failed to bootstrap greentic runtime: {:#}", result.err().unwrap());
            process::exit(1);
    };

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