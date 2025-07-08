use channel_plugin::plugin_runtime::run;
use channel_telegram::TelegramPlugin;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(TelegramPlugin::default()).await
}
