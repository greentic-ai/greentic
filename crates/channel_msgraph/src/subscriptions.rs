use std::sync::Arc;
use anyhow::Result;
use chrono::{Duration, Utc};
use dashmap::DashMap;
use serde_json::json;
use tokio::sync::RwLock;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use tokio::sync::RwLock as TokioRwLock;

use crate::graph_client::GraphState;

pub type SubscriptionFn = Arc<dyn Fn(&Arc<RwLock<GraphState>>) -> BoxFuture<'static, Result<()>> + Send + Sync>;

static SUBSCRIPTION_REGISTRARS: Lazy<TokioRwLock<Vec<(&'static str, SubscriptionFn)>>> =
    Lazy::new(|| TokioRwLock::new(vec![]));

/// Low-level reusable registration to Graph API
pub async fn register_graph_subscription(
    resource: &str,
    client_state: &str,
    change_type: &str,
    graph: &Arc<RwLock<GraphState>>,
) -> Result<()> {
    let domain = graph.read().await.config.domain.clone();
    let url = format!("https://{}/notification", domain);
    let expiration = Utc::now() + Duration::minutes(4230);

    let subscription = json!({
        "changeType": change_type,
        "notificationUrl": url,
        "resource": resource,
        "expirationDateTime": expiration.to_rfc3339(),
        "clientState": client_state,
    });

    let mut graph_guard = graph.write().await;
    graph_guard
        .client
        .v1()
        .subscriptions()
        .create_subscription(&subscription)
        .send()
        .await?;

    Ok(())
}

/// Register a named subscription function (e.g., "calendar-events", "teams-chat")
pub async fn register_subscription<F, Fut>(name: &'static str, config: Arc<DashMap<String, String>>, f: F)
where
    F: Fn(&Arc<RwLock<GraphState>>, Arc<DashMap<String, String>>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let wrapper: SubscriptionFn = Arc::new(move |state| {
        let config = Arc::clone(&config);
        Box::pin(f(state, config))
    });
    tokio::spawn(async move {
        let mut lock = SUBSCRIPTION_REGISTRARS.write().await;
        lock.push((name, wrapper));
    });
}

/// Run all registered subscription closures at once (e.g., during init)
pub async fn register_all_subscriptions(graph_state: Arc<RwLock<GraphState>>) -> Result<()> {
    let handlers = SUBSCRIPTION_REGISTRARS.read().await;
    for (name, func) in handlers.iter() {
        tracing::info!("📡 Registering MS Graph subscription: {name}");
        func(&graph_state).await?;
    }
    Ok(())
}


/// Renew all active subscription handlers (called in `.poll()`)
pub async fn renew_all(state: &Arc<RwLock<GraphState>>) -> Result<()> {
    let registrars = SUBSCRIPTION_REGISTRARS.read().await;

    for (name, reg_fn) in registrars.iter() {
        tracing::info!("🔄 Registering: {}", name);
        if let Err(e) = reg_fn(state).await {
            tracing::error!("❌ Failed to register {}: {:?}", name, e);
        }
    }

    Ok(())
}
