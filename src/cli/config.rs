use clap::{Args, Subcommand};

use super::CliContext;

#[derive(Args, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommands,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommands {
    Add { key: String, value: String },
    Update { key: String, value: String },
    Delete { key: String },
}

pub async fn execute(args: ConfigArgs, context: &CliContext) -> anyhow::Result<()> {
    match args.command {
        ConfigCommands::Add { key, value } => {
            match context.config_manager.0.set(&key, &value).await {
                Ok(_) => println!("✅ Config added."),
                Err(_) => eprintln!("❌ Config could not be added."),
            }
            Ok(())
        }
        ConfigCommands::Update { key, value } => {
            match context.config_manager.0.set(&key, &value).await {
                Ok(_) => println!("✅ Config updated."),
                Err(_) => eprintln!("❌ Config could not be updated."),
            }
            Ok(())
        }
        ConfigCommands::Delete { key } => {
            context.config_manager.0.del(&key).await;
            Ok(())
        }
    }
}
