use std::path::PathBuf;

use clap::{Parser, Subcommand};

pub mod config;
pub mod export;
pub mod flow;
pub mod init;
pub mod run;
pub mod schema;
pub mod secrets;
pub mod snapshot;

use config::ConfigArgs;
use export::ExportArgs;
use flow::FlowArgs;
use run::RunArgs;
use schema::SchemaArgs;
use secrets::SecretArgs;
use snapshot::SnapshotArgs;

use greentic::{config::ConfigManager, secret::SecretsManager};

#[derive(Parser, Debug)]
#[command(
    name = "greentic",
    about = "The Greener Digital Workers Platform",
    version = "0.2.3"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
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

    /// Handle configuration
    Config(ConfigArgs),

    /// Handle export
    Export(ExportArgs),

    /// Validate snapshot requirements against the current environment
    Snapshot(SnapshotArgs),
}

#[derive(Clone)]
pub struct CliContext {
    pub root: PathBuf,
    pub config_manager: ConfigManager,
    pub secrets_manager: SecretsManager,
}

impl CliContext {
    pub fn new(
        root: PathBuf,
        config_manager: ConfigManager,
        secrets_manager: SecretsManager,
    ) -> Self {
        Self {
            root,
            config_manager,
            secrets_manager,
        }
    }
}

pub async fn execute(context: &CliContext, command: Commands) -> anyhow::Result<()> {
    match command {
        Commands::Run(args) => run::execute(args, context).await,
        Commands::Schema(args) => schema::execute(args, context).await,
        Commands::Init => init::execute(context).await,
        Commands::Flow(args) => flow::execute(args, context).await,
        Commands::Secrets(args) => secrets::execute(args, context).await,
        Commands::Config(args) => config::execute(args, context).await,
        Commands::Export(args) => export::execute(args, context).await,
        Commands::Snapshot(args) => snapshot::execute(args, context).await,
    }
}

pub fn default_command() -> Commands {
    Commands::Run(RunArgs::default_runtime())
}
