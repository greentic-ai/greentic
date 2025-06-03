use clap::{Args, Parser, Subcommand};
use greentic::{
    apps::{cmd_init, App}, config::{ConfigManager, EnvConfigManager}, flow_commands::{deploy_flow_file, move_flow_file, validate_flow_file}, logger::init_tracing, schema::write_schema, secret::{EnvSecretsManager, SecretsManager}
};
use std::{env, fs, path::PathBuf, process};
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
}

#[derive(Args, Debug)]
struct FlowArgs {
    #[command(subcommand)]
    command: FlowCommands,
}

#[derive(Args, Debug)]
struct RunArgs {
        
    /// Optional log level override (e.g. error, warn, info, debug, trace)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// OpenTelemetry endpoint (e.g. http://localhost:4317)
    #[arg(long)]
    otel_logs_endpoint: Option<String>,

    /// OpenTelemetry endpoint (e.g. http://localhost:4317)
    #[arg(long)]
    otel_events_endpoint: Option<String>,
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
    Start { name: String },
    Stop { name: String },
}

/// Resolve the greentic root directory from the environment or use default.
pub fn resolve_root_dir() -> PathBuf {
    if let Ok(path) = env::var("GREENTIC_ROOT") {
        PathBuf::from(path)
    } else {
        PathBuf::from("./greentic")
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse(); 
    match cli.command.unwrap_or(Commands::Run(RunArgs {
        log_level: "info".to_string(),
        otel_logs_endpoint: None,
        otel_events_endpoint: None,
        }))  {
        Commands::Run(args) => {
            let root = resolve_root_dir();
            run(root,
                args.log_level,
                args.otel_logs_endpoint,
                args.otel_events_endpoint,
            ).await?;
            Ok(())
        }
        Commands::Schema(args) => {
            let root = resolve_root_dir();
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
        Commands::Init => {
            let root = resolve_root_dir();
            cmd_init(root.clone()).await?;
            println!("Initialized greentic layout at {}", root.display());
            process::exit(0);
        }
        Commands::Flow(flow_args) => match flow_args.command {
            FlowCommands::Validate { file } => {
                validate_flow_file(file)?;
                println!("✅ Flow file is valid.");
                Ok(())
            },
            FlowCommands::Deploy { file } => {
                let root = resolve_root_dir();
                validate_flow_file(file.clone())?;
                deploy_flow_file(file, root)?;
                Ok(())
            },
            FlowCommands::Start { name } => {
                let root = resolve_root_dir();
                let from = root.join("flows/stopped");
                let to = root.join("flows/running");
                move_flow_file(&name, &from, &to)?;
                println!("✅ Flow `{}` started. Please make sure greentic is running, i.e. greentic run", name);
                Ok(())
            },
            FlowCommands::Stop { name } => {
                let root = resolve_root_dir();
                let from = root.join("flows/running");
                let to = root.join("flows/stopped");
                move_flow_file(&name, &from, &to)?;
                println!("✅ Flow `{}` stopped.", name);
                Ok(())
            },
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
    let processes_dir= root.join("plugins").join("processes");
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
        processes_dir.clone(),
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