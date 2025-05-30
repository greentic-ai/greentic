use std::{fmt, sync::Arc};
use std::collections::HashMap;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{broadcast, RwLock};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum StateValue {
    String(String),
    Number(f64),
    Boolean(bool),
    List(Vec<StateValue>),
    Map(HashMap<String, StateValue>),
    Null,
}

impl StateValue {
    pub fn as_str(&self) -> Option<&str> {
        if let StateValue::String(s) = self {
            Some(s)
        } else {
            None
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        if let StateValue::Number(n) = self {
            Some(*n)
        } else {
            None
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        if let StateValue::Boolean(b) = self {
            Some(*b)
        } else {
            None
        }
    }

    pub fn as_list(&self) -> Option<&Vec<StateValue>> {
        if let StateValue::List(l) = self {
            Some(l)
        } else {
            None
        }
    }

    pub fn as_map(&self) -> Option<&HashMap<String, StateValue>> {
        if let StateValue::Map(m) = self {
            Some(m)
        } else {
            None
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            StateValue::String(s) => json!(s),
            StateValue::Number(n) => json!(n),
            StateValue::Boolean(b) => json!(b),
            StateValue::List(l) => json!(l.iter().map(|v| v.to_json()).collect::<Vec<_>>()),
            StateValue::Map(m) => {
                json!(m.iter().map(|(k, v)| (k.clone(), v.to_json())).collect::<HashMap<_, _>>())
            }
            StateValue::Null => Value::Null,
        }
    }
}

impl TryFrom<Value> for StateValue {
    type Error = ();

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::String(s) => Ok(StateValue::String(s)),
            Value::Number(n) => Ok(StateValue::Number(n.as_f64().ok_or(())?)),
            Value::Bool(b) => Ok(StateValue::Boolean(b)),
            Value::Array(a) => Ok(StateValue::List(
                a.into_iter().filter_map(|v| StateValue::try_from(v).ok()).collect(),
            )),
            Value::Object(o) => Ok(StateValue::Map(
                o.into_iter()
                    .filter_map(|(k, v)| Some((k, StateValue::try_from(v).ok()?)))
                    .collect(),
            )),
            Value::Null => Ok(StateValue::Null),
        }
    }
}

#[async_trait]
pub trait StateStore: Send + Sync {
    async fn load(&self) -> HashMap<String, StateValue>;
    async fn save(&self, state: &HashMap<String, StateValue>);

    fn name(&self) -> &'static str;
}

impl fmt::Debug for dyn StateStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StateStore")
         .field("impl", &self.name())
         .finish()
    }
}

pub struct InMemoryState {
    store: Arc<RwLock<HashMap<String, StateValue>>>,
}

impl InMemoryState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { store: Arc::new(RwLock::new(HashMap::new())),})
    }
}
#[async_trait]
impl StateStore for InMemoryState {
        async fn load(&self) -> HashMap<String, StateValue> {
        self.store.read().await.clone()
    }

    async fn save(&self, state: &HashMap<String, StateValue>) {
        let mut w = self.store.write().await;
        *w = state.clone();
    }

    fn name(&self) -> &'static str {
        "InMemoryStateStore"
    }
}

pub struct StateChannel {
    sender: broadcast::Sender<String>,
}

impl StateChannel {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(100);
        Self { sender }
    }

    pub fn publish(&self, message: String) {
        let _ = self.sender.send(message);
    }

    pub async fn subscribe(&self) -> broadcast::Receiver<String> {
        self.sender.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[test]
    fn test_state_value_accessors() {
        let string = StateValue::String("hello".into());
        assert_eq!(string.as_str(), Some("hello"));
        assert_eq!(string.as_number(), None);

        let number = StateValue::Number(42.0);
        assert_eq!(number.as_number(), Some(42.0));
        assert_eq!(number.as_str(), None);

        let boolean = StateValue::Boolean(true);
        assert_eq!(boolean.as_bool(), Some(true));

        let list = StateValue::List(vec![StateValue::Null]);
        assert!(list.as_list().is_some());

        let mut map_data = HashMap::new();
        map_data.insert("k".into(), StateValue::Null);
        let map = StateValue::Map(map_data.clone());
        assert_eq!(map.as_map(), Some(&map_data));

        assert_eq!(StateValue::Null.as_str(), None);
    }

    #[tokio::test]
    async fn test_in_memory_state_store() {
        let store = InMemoryState::new();
        let mut state = HashMap::new();
        state.insert("test".to_string(), StateValue::Boolean(true));

        store.save(&state).await;
        let loaded = store.load().await;

        assert_eq!(loaded.get("test"), Some(&StateValue::Boolean(true)));
        assert_eq!(store.name(), "InMemoryStateStore");
    }

    #[tokio::test]
    async fn test_state_channel_pub_sub() {
        let channel = StateChannel::new();
        let mut rx = channel.subscribe().await;

        channel.publish("hello".into());

        // Use timeout to avoid hanging if pub/sub fails
        let received = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("timeout")
            .expect("recv failed");

        assert_eq!(received, "hello");
    }

    #[tokio::test]
    async fn test_state_store_trait_object_usage() {
        let store: Arc<dyn StateStore> = InMemoryState::new();
        let mut state = HashMap::new();
        state.insert("x".into(), StateValue::Number(3.14));

        store.save(&state).await;
        let loaded = store.load().await;

        assert_eq!(loaded.get("x"), Some(&StateValue::Number(3.14)));
    }
}
