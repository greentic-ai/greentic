mod cli;

use clap::Parser;
use cli::Cli;
use greentic::config::{ConfigManager, EnvConfigManager};
use greentic::secret::{EnvSecretsManager, SecretsManager};
use std::env;
use std::path::PathBuf;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let root = resolve_root_dir().join("greentic");
    let config_dir = root.join("config");
    let secrets_dir = root.join("secrets");
    let env_file = config_dir.join(".env");

    let config_manager = ConfigManager(EnvConfigManager::new(env_file));
    let secrets_manager = SecretsManager(EnvSecretsManager::new(Some(secrets_dir)));

    let context = cli::CliContext::new(root, config_manager, secrets_manager);
    let command = cli.command.unwrap_or_else(cli::default_command);

    cli::execute(&context, command).await
}

/// Resolve the greentic root directory from the environment or use default.
fn resolve_root_dir() -> PathBuf {
    if let Ok(path) = env::var("GREENTIC_ROOT") {
        PathBuf::from(path)
    } else {
        PathBuf::from("./")
    }
}
