use std::{fs, process};

use clap::Args;

use super::CliContext;
use greentic::schema::write_schema;

#[derive(Args, Debug)]
pub struct SchemaArgs {
    /// Optional log level override for the internal tracer
    #[arg(long, default_value = "info")]
    pub log_level: String,

    /// Write logs to file enabled. Default: true
    #[arg(long, default_value_t = true)]
    pub logs_enabled: bool,
}

pub async fn execute(args: SchemaArgs, context: &CliContext) -> anyhow::Result<()> {
    let out_dir = context.root.join("schemas");
    let log_level = args.log_level;
    let _ = args.logs_enabled;
    let log_file = context.root.join("logs/greentic-schema.log");
    let event_file = context.root.join("logs/greentic-schema.json");
    let tools_dir = context.root.join("plugins").join("tools");
    let channels_dir = context
        .root
        .join("plugins")
        .join("channels")
        .join("running");
    let _processes_dir = context.root.join("plugins").join("processes");
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
