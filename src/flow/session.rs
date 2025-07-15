use crate::flow::state::{InMemoryState, SessionState};
use async_trait::async_trait;
use moka::future::Cache;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

pub type SessionStore = Arc<dyn SessionStoreType>;

/// Factory and cache for per-session state instances.
#[async_trait]
pub trait SessionStoreType: Send + Sync + Debug {
    /// Returns an existing session if it exists.
    async fn get(&self, session_id: &str) -> Option<SessionState>;
    /// Returns an existing session or creates a new one with default state.
    async fn get_or_create(&self, session_id: &str) -> SessionState;

    /// Try to get session by unique plugin identifier and and run route matching if no session is available
    async fn get_channel(&self, channel_key: &str) -> Option<String>;

    /// Try to get or create session by plugin name + key and rely on session routing
    async fn get_or_create_channel(&self, channel_key: &str) -> String;

    /// Explicitly removes a session from the store.
    async fn remove(&self, session_id: &str);

    /// Clears all sessions (typically for tests or shutdown).
    fn clear(&self);
}

#[derive(Clone, Debug)]
pub struct InMemorySessionStore {
    cache: Cache<String, Arc<InMemoryState>>, // session_id → session state
    by_channel: Cache<String, String>,        // (plugin_name|key) = channel_key → session_id
    reverse_map: Cache<String, String>,       // session_id → (plugin_name | key) = channel_key
}

impl InMemorySessionStore {
    /// Creates a new SessionStore with given TTL in seconds.
    pub fn new(ttl_secs: u64) -> Arc<Self> {
        let cache = Cache::builder()
            .time_to_idle(Duration::from_secs(ttl_secs))
            .eviction_listener(|key: Arc<String>, _value: Arc<InMemoryState>, cause| {
                info!("Session expired: key={}, cause={:?}", key, cause,);
                // Optionally trigger cleanup logic or emit metrics here.
            })
            .build();
        let by_channel = Cache::builder()
            .time_to_idle(Duration::from_secs(ttl_secs))
            .build();

        let reverse_map = Cache::builder()
            .time_to_idle(Duration::from_secs(ttl_secs))
            .build();

        Arc::new(Self {
            cache,
            by_channel,
            reverse_map,
        })
    }
}
#[async_trait]
impl SessionStoreType for InMemorySessionStore {
    /// Forcefully removes a session (e.g. after completion or error).
    async fn remove(&self, session_id: &str) {
        self.cache.invalidate(session_id).await;

        if let Some(channle_pair) = self.reverse_map.get(session_id).await {
            self.by_channel.invalidate(&channle_pair).await;
            self.reverse_map.invalidate(session_id).await;
        }
    }

    /// Clears all sessions (used in tests or shutdown).
    fn clear(&self) {
        self.cache.invalidate_all();
        self.by_channel.invalidate_all();
    }

    /// Translates a channel plugin and unique key per channel into a session
    async fn get_channel(&self, channel_key: &str) -> Option<String> {
        self.by_channel.get(channel_key).await
    }

    /// Same as get_channel but if a session does not exist, it gets created.
    async fn get_or_create_channel(&self, channel_key: &str) -> String {
        // 1. Try from cache
        if let Some(entry) = self.get_channel(channel_key).await {
            return entry;
        } else {
            let session_id = uuid::Uuid::new_v4().to_string();
            self.by_channel
                .insert(channel_key.to_string(), session_id.clone())
                .await;
            self.reverse_map
                .insert(session_id.clone(), channel_key.to_string())
                .await;
            session_id
        }
    }

    async fn get(&self, session_id: &str) -> Option<SessionState> {
        let key = session_id.to_string();

        let result = match self.cache.get(&key).await {
            Some(state) => {
                let state: Option<SessionState> = Some(state);
                state
            }
            None => None,
        };
        result
    }

    async fn get_or_create(&self, session_id: &str) -> SessionState {
        let key = session_id.to_string();

        let result = match self.cache.get(&key).await {
            Some(state) => state as SessionState,
            None => {
                let new_state = InMemoryState::new();
                self.cache.insert(key, new_state.clone()).await;
                new_state
            }
        };
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::state::StateValue;

    #[tokio::test]
    async fn test_session_store_create_and_retrieve() {
        let store = InMemorySessionStore::new(60);
        let session_id = "abc123";

        let session = store.get_or_create(session_id).await;
        session.set("foo".to_string(), StateValue::String("bar".into()));

        let session2 = store.get_or_create(session_id).await;
        let val = session2.get("foo");

        assert_eq!(val, Some(StateValue::String("bar".into())));
    }

    #[tokio::test]
    async fn test_session_store_removal() {
        let store = InMemorySessionStore::new(60);
        let session_id = "abc123";

        let session = store.get_or_create(session_id).await;
        session.set("foo".to_string(), StateValue::String("bar".into()));

        store.remove(session_id).await;

        let session2 = store.get_or_create(session_id).await;
        let val = session2.get("foo");

        assert_eq!(val, None); // should be empty after recreation
    }

    #[tokio::test]
    async fn test_get_channel_none() {
        let store = InMemorySessionStore::new(60);
        let result = store.get_channel("telegram|chat_123").await;
        assert!(result.is_none(), "No session should exist yet");
    }

    #[tokio::test]
    async fn test_get_or_create_channel_creates() {
        let store = InMemorySessionStore::new(60);
        let sid1 = store.get_or_create_channel("telegram|chat_123").await;
        let sid2 = store.get_channel("telegram|chat_123").await;

        assert_eq!(Some(sid1.clone()), sid2);

        store.remove(&sid1).await;

        assert!(store.get_channel("telegram|chat_123").await.is_none());
    }

    #[tokio::test]
    async fn test_clear_sessions() {
        let store = InMemorySessionStore::new(60);
        let session1 = store.get_or_create("session1").await;
        session1.set("foo".to_string(), StateValue::String("bar".into()));

        store.clear();

        let new1 = store.get_or_create("session1").await;
        assert_eq!(new1.get("foo"), None); // Confirm it was reset
    }

    #[tokio::test]
    async fn test_reverse_index_cleanup() {
        let store = InMemorySessionStore::new(60);
        let sid = store.get_or_create_channel("telegram|chat_999").await;

        // Remove via session_id
        store.remove(&sid).await;

        // Ensure cleanup
        assert!(store.get_channel("telegram|chat_999").await.is_none());
        let expected_none = store.reverse_map.get::<str>(sid.as_ref()).await;
        assert!(expected_none.is_none());
    }
}
