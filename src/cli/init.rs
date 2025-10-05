use std::process;

use super::CliContext;
use greentic::apps::cmd_init;
use greentic::logger::FileTelemetry;

pub async fn execute(context: &CliContext) -> anyhow::Result<()> {
    let log_file = context.root.join("logs/greentic-init.log");
    let event_file = context.root.join("logs/greentic-init.json");
    let _ = FileTelemetry::init_files("info", log_file, event_file);
    cmd_init(context.root.clone()).await?;
    println!("Greentic has been initialised. You can start it with 'greentic run'");
    process::exit(0);
}
