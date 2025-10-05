use clap::{Args, Subcommand};

use super::CliContext;

#[derive(Args, Debug)]
pub struct SecretArgs {
    #[command(subcommand)]
    pub command: SecretCommands,
}

#[derive(Subcommand, Debug)]
pub enum SecretCommands {
    Add { key: String, secret: String },
    Update { key: String, secret: String },
    Delete { key: String },
}

pub async fn execute(args: SecretArgs, context: &CliContext) -> anyhow::Result<()> {
    match args.command {
        SecretCommands::Add { key, secret } => {
            match context.secrets_manager.add_secret(&key, &secret).await {
                Ok(_) => println!("✅ Secret added."),
                Err(_) => eprintln!("❌ Secret could not be added."),
            }
            Ok(())
        }
        SecretCommands::Update { key, secret } => {
            match context.secrets_manager.update_secret(&key, &secret).await {
                Ok(_) => println!("✅ Secret updated."),
                Err(_) => eprintln!("❌ Secret could not be updated."),
            }
            Ok(())
        }
        SecretCommands::Delete { key } => {
            match context.secrets_manager.delete_secret(&key).await {
                Ok(_) => println!("✅ Secret deleted."),
                Err(_) => eprintln!("❌ Secret could not be deleted."),
            }
            Ok(())
        }
    }
}
