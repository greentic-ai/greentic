use std::sync::Arc;
use std::time::Duration;
use std::fmt::Debug;
use async_trait::async_trait;
use moka::future::Cache;

use crate::flow::state::{InMemoryState, SessionState};

pub type SessionStore = Arc<dyn SessionStoreType>;

/// Factory and cache for per-session state instances.
#[async_trait]
pub trait SessionStoreType: Send + Sync + Debug {
    /// Returns an existing session or creates a new one with default state.
    async fn get_or_create(&self, session_id: &str) ->  SessionState;

    /// Explicitly removes a session from the store.
    fn remove(&self, session_id: &str);

    /// Clears all sessions (typically for tests or shutdown).
    fn clear(&self);
}



#[derive(Clone, Debug)]
pub struct InMemorySessionStore {
    cache: Cache<String, Arc<InMemoryState>>,
}

impl InMemorySessionStore {
    /// Creates a new SessionStore with given TTL in seconds.
    pub fn new(ttl_secs: u64) -> Self {
        let cache = Cache::builder()
            .time_to_idle(Duration::from_secs(ttl_secs))
            .build();

        Self { cache }
    }
}
#[async_trait]
impl SessionStoreType for InMemorySessionStore {

    /// Forcefully removes a session (e.g. after completion or error).
    fn remove(&self, session_id: &str) {
        self.cache.invalidate(session_id);
    }

    /// Clears all sessions (used in tests or shutdown).
    fn clear(&self) {
        self.cache.invalidate_all();
    }
    
   async fn get_or_create(&self, session_id: &str) -> SessionState {
        let key = session_id.to_string();

        match self.cache.get(&key).await {
            Some(state) => state as SessionState,
            None => {
                let new_state = InMemoryState::new();
                self.cache.insert(key, new_state.clone()).await;
                new_state
            }
        }
    }
} 

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::state::{StateValue};

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

        store.remove(session_id);

        let session2 = store.get_or_create(session_id).await;
        let val = session2.get("foo");

        assert_eq!(val, None); // should be empty after recreation
    }
}
