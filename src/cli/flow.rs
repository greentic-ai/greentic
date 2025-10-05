use std::path::PathBuf;

use clap::{Args, Subcommand};

use super::CliContext;
use greentic::apps::download;
use greentic::flow_commands::{deploy_flow_file, move_flow_file, validate_flow_file};

#[derive(Args, Debug)]
pub struct FlowArgs {
    #[command(subcommand)]
    pub command: FlowCommands,
}

#[derive(Subcommand, Debug)]
pub enum FlowCommands {
    Validate { file: PathBuf },
    Deploy { file: PathBuf },
    Pull { name: String },
    Start { name: String },
    Stop { name: String },
}

pub async fn execute(args: FlowArgs, context: &CliContext) -> anyhow::Result<()> {
    match args.command {
        FlowCommands::Validate { file } => {
            let tools_dir = context.root.join("plugins").join("tools");
            validate_flow_file(
                file,
                context.root.clone(),
                tools_dir,
                context.secrets_manager.clone(),
                context.config_manager.clone(),
            )
            .await?;
            println!("✅ Flow file is valid.");
            Ok(())
        }
        FlowCommands::Pull { name } => {
            let token_handle = context.secrets_manager.0.get("GREENTIC_TOKEN");
            if token_handle.is_none() {
                println!("Please run 'greentic init' one time before calling 'greentic pull ...'");
                return Ok(());
            }
            let token = context
                .secrets_manager
                .0
                .reveal(token_handle.unwrap())
                .await
                .unwrap()
                .unwrap();
            let out_dir = context.root.join("flows/running");
            let result = download(&token, "flows", &name, out_dir.clone()).await;
            if let Err(err) = result {
                println!("Sorry we could not download {} because {}", name, err);
            }
            let tools_dir = context.root.join("plugins").join("tools");
            validate_flow_file(
                out_dir.join(&name),
                context.root.clone(),
                tools_dir,
                context.secrets_manager.clone(),
                context.config_manager.clone(),
            )
            .await?;

            Ok(())
        }
        FlowCommands::Deploy { file } => {
            let tools_dir = context.root.join("plugins").join("tools");
            deploy_flow_file(
                file,
                context.root.clone(),
                tools_dir,
                context.secrets_manager.clone(),
                context.config_manager.clone(),
            )
            .await?;
            Ok(())
        }
        FlowCommands::Start { name } => {
            let from = context.root.join("flows/stopped");
            let to = context.root.join("flows/running");
            move_flow_file(&name, &from, &to)?;
            println!(
                "✅ Flow `{}` started. Please make sure greentic is running, i.e. greentic run",
                name
            );
            Ok(())
        }
        FlowCommands::Stop { name } => {
            let from = context.root.join("flows/running");
            let to = context.root.join("flows/stopped");
            move_flow_file(&name, &from, &to)?;
            println!("✅ Flow `{}` stopped.", name);
            Ok(())
        }
    }
}
