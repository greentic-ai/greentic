use channel_plugin::message::LogLevel;
use clap::{Args, Parser, Subcommand};
use greentic::{
    apps::{cmd_init, download, App}, config::{ConfigManager, EnvConfigManager}, flow_commands::{deploy_flow_file, move_flow_file, validate_flow_file}, logger::init_tracing, schema::write_schema, secret::{EnvSecretsManager, SecretsManager}
};
use std::{env, fs, path::PathBuf, process};
use tracing::{error, info};
use anyhow::bail;

#[derive(Parser, Debug)]
#[command(
    name = "greentic", 
    about = "The Greener Digital Workers Platform", 
    version = "0.2.0-rc4"
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
    let config_dir   = root.join("config");
    let secrets_dir  = root.join("secrets");   
    let env_file = config_dir.join(".env");
    let config_manager = ConfigManager(EnvConfigManager::new(env_file.clone()));
    let secrets_manager = SecretsManager(EnvSecretsManager::new(Some(secrets_dir.clone())));

    let cli = Cli::parse(); 
    match cli.command.unwrap_or(Commands::Run(RunArgs {
        session_timeout: 1800,
        log_level: "info".to_string(),
        otel_logs_endpoint: None,
        otel_events_endpoint: None,
        }))  {
        Commands::Run(args) => {
            run(root,
                args.session_timeout,
                args.log_level,
                args.otel_logs_endpoint,
                args.otel_events_endpoint,
                secrets_manager,
                config_manager,
            ).await?;
            Ok(())
        }
        Commands::Schema(args) => {
            let out_dir = root.join("schemas");
            let log_level     = args.log_level;
            let log_file      = "logs/greentic-schema.log".to_string();
            let event_file      = "logs/greentic-schema.json".to_string();
            let tools_dir    = root.join("plugins").join("tools");
            let channels_dir           = root.join("plugins").join("channels").join("running");
            let _processes_dir= root.join("plugins").join("processes");
            fs::create_dir_all(&out_dir)?;
            write_schema(
                out_dir.clone(),
                root.clone(),
                tools_dir.clone(),
                channels_dir.clone(),
                log_level,
                log_file.clone(),
                event_file.clone(),
            )
            .await?;
            println!("Schemas written to {}", out_dir.display());
            process::exit(0);
        }
        Commands::Init => {
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
            },
            SecretCommands::Update { key, secret } => {
                match secrets_manager.update_secret(&key, &secret).await {
                    Ok(_) => println!("✅ Secret updated."),
                    Err(_) => eprintln!("❌ Secret could not be updated."),
                }
                Ok(())
            },
            SecretCommands::Delete { key } => {
                match secrets_manager.delete_secret(&key).await {
                    Ok(_) => println!("✅ Secret deleted."),
                    Err(_) => eprintln!("❌ Secret could not be deleted."),
                }
                Ok(())
            },
        },

        Commands::Config(config_args) => match config_args.command {
            ConfigCommands::Add { key, value } => {
                match config_manager.0.set(&key, &value).await {
                    Ok(_) => println!("✅ Config added."),
                    Err(_) => eprintln!("❌ Config could not be added."),
                }
                Ok(())
            },
            ConfigCommands::Update { key, value } => {
                match config_manager.0.set(&key, &value).await {
                    Ok(_) => println!("✅ Config updated."),
                    Err(_) => eprintln!("❌ Config could not be updated."),
                }
                Ok(())
            },
            ConfigCommands::Delete { key } => {
                config_manager.0.del(&key).await;
                Ok(())
            },
        }
        Commands::Flow(flow_args) => match flow_args.command {
            FlowCommands::Validate { file } => {
                let log_level     = "info".to_string();
                let log_file      = "logs/greentic-validation.log".to_string();
                let event_file      = "logs/greentic-validation.json".to_string();
                let tools_dir    = root.join("plugins").join("tools");
                validate_flow_file(
                    file, 
                    root, 
                    tools_dir, 
                    log_level, 
                    log_file, 
                    event_file,
                    secrets_manager,
                    config_manager,
                ).await?;
                println!("✅ Flow file is valid.");
                Ok(())
            },
            FlowCommands::Pull { name } => {
                let token_handle = secrets_manager.0.get("GREENTIC_TOKEN");
                if token_handle.is_none() {
                    println!("Please run 'greentic init' one time before calling 'greentic pull ...'");
                    return Ok(());
                }
                let token = secrets_manager.0.reveal(token_handle.unwrap()).await.unwrap().unwrap();
                let out_dir = root.join("flows/running");
                let result = download(&token, "flows", &name, out_dir).await;
                if result.is_err() {
                    println!("Sorry we could not download {} because {:?}",name, result.err().unwrap().to_string());
                }
                
                Ok(())
            },
            FlowCommands::Deploy { file } => {
                let log_level     = "info".to_string();
                let log_file      = "logs/greentic-validation.log".to_string();
                let event_file      = "logs/greentic-validation.json".to_string();
                let tools_dir    = root.join("plugins").join("tools");
                deploy_flow_file(
                    file, 
                    root,
                    tools_dir,
                    log_level,
                    log_file,
                    event_file,
                    secrets_manager,
                    config_manager,
                ).await?;
                Ok(())
            },
            FlowCommands::Start { name } => {
                let from = root.join("flows/stopped");
                let to = root.join("flows/running");
                move_flow_file(&name, &from, &to)?;
                println!("✅ Flow `{}` started. Please make sure greentic is running, i.e. greentic run", name);
                Ok(())
            },
            FlowCommands::Stop { name } => {
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
    session_timeout: u64, 
    log_level: String,
    otel_logs_endpoint: Option<String>,
    otel_events_endpoint: Option<String>,
    secrets_manager: SecretsManager,
    config_manager: ConfigManager,
) -> anyhow::Result<()> {
    let flows_dir    = root.join("flows/running");
    let log_file      = "logs/greentic_logs.log".to_string();
    let log_dir= Some(root.join("logs").to_string_lossy().to_string());
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


    // bootstrap
    let mut app = App::new();
    let result = app.bootstrap(
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
    )
    .await;
    if result.is_err()
    {
            error!("Failed to bootstrap greentic runtime: {:#}", result.err().unwrap());
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

fn convert_level(level: String) -> LogLevel {
    match level.to_lowercase().as_str() {
        "trace" => LogLevel::Trace,
        "debug" => LogLevel::Debug,
        "info"  => LogLevel::Info,
        "warn"  => LogLevel::Warn,
        "error" => LogLevel::Error,
        "critical" => LogLevel::Critical,
        _ => LogLevel::Info,
    }

}