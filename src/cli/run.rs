use std::{fs::File, io::BufReader, path::PathBuf, process};

use anyhow::{Context, bail};
use clap::Args;
use tokio::signal;
use tracing::{error, info};

use super::CliContext;
use channel_plugin::message::LogLevel;
use greentic::apps::App;
use greentic::config::ConfigManager;
use greentic::logger::init_tracing;
use greentic::runtime_snapshot::RuntimeSnapshot;
use greentic::secret::SecretsManager;

#[derive(Args, Debug)]
pub struct RunArgs {
    #[arg(long, default_value = "1800")]
    pub session_timeout: u64,
    /// Optional log level override (e.g. error, warn, info, debug, trace)
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// OpenTelemetry endpoint (e.g. http://localhost:4317)
    #[arg(long)]
    pub otel_logs_endpoint: Option<String>,

    /// OpenTelemetry endpoint (e.g. http://localhost:4317)
    #[arg(long)]
    pub otel_events_endpoint: Option<String>,

    /// Allow to pass a saved state for ultra fast boot
    #[arg(long)]
    pub snapshot: Option<String>,
}

impl RunArgs {
    pub fn default_runtime() -> Self {
        Self {
            session_timeout: 1800,
            log_level: "info".to_string(),
            otel_logs_endpoint: None,
            otel_events_endpoint: None,
            snapshot: None,
        }
    }
}

pub async fn execute(args: RunArgs, context: &CliContext) -> anyhow::Result<()> {
    let state: Option<PathBuf> = match args.snapshot {
        Some(state_string) => {
            let state = PathBuf::from(state_string.clone());
            if state.exists() {
                Some(state)
            } else {
                let error = format!(
                    "Greentic state does not exist. Please make sure {} points to a valid greentic state. Use greentic export to get a valid state.",
                    state_string
                );
                error!(error);
                println!("{}", error);
                process::exit(-1);
            }
        }
        None => None,
    };

    run_runtime(
        context.root.clone(),
        args.session_timeout,
        args.log_level,
        args.otel_logs_endpoint,
        args.otel_events_endpoint,
        context.secrets_manager.clone(),
        context.config_manager.clone(),
        state,
    )
    .await
    .map(|_| ())
}

async fn run_runtime(
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

    let mut app = App::new();
    let result = app
        .bootstrap(
            session_timeout,
            flows_dir,
            channels_dir,
            tools_dir,
            processes_dir,
            config_manager,
            logger,
            convert_level(log_level),
            log_dir,
            otel_logs_endpoint,
            secrets_manager,
            snapshot_data.as_ref(),
        )
        .await;

    if let Err(err) = result {
        error!("Failed to bootstrap greentic runtime: {:#}", err);
        process::exit(1);
    }

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

    signal::ctrl_c().await?;

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
        "info" => LogLevel::Info,
        "warn" => LogLevel::Warn,
        "error" => LogLevel::Error,
        "critical" => LogLevel::Critical,
        _ => LogLevel::Info,
    }
}
