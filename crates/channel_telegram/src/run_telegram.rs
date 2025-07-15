use channel_plugin::{
    plugin_helpers::load_env_as_vecs,
    plugin_runtime::{PluginHandler, run},
};
use channel_telegram::TelegramPlugin;

#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    let (config, secrets) = load_env_as_vecs(
        Some("../../greentic/secrets/.env"),
        None, /* default: .env in cwd */
    )
    .expect("failed to read .env");
    let mut plugin = TelegramPlugin::default();
    let params = channel_plugin::message::InitParams {
        version: "0".into(),
        config: config,
        secrets: secrets,
        log_level: channel_plugin::message::LogLevel::Info,
        log_dir: Some("./logs".to_string()),
        otel_endpoint: None,
    };
    plugin.start(params).await;
    run(plugin).await
}
