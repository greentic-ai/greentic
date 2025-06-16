use std::sync::Mutex;
use std::{sync::Arc};
use dashmap::DashMap;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::broadcast;
use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;

pub type SessionState = Arc<dyn SessionStateType + Send + Sync + 'static>;

/// Represents a per-session key-value store with async access.
#[async_trait]
pub trait SessionStateType: Send + Sync + Debug {
    /// The session tracks which flows and nodes should get to handle the next message
    fn flows(&self) -> Option<Vec<String>>;
    fn add_flow(&self, flow: String);
    fn set_flows(&self, flows: Vec<String>);
    fn nodes(&self) -> Option<Vec<String>>;
    fn add_node(&self, node: String);
    fn pop_node(&self) -> Option<String>;
    fn set_nodes(&self, nodes: Vec<String>);

    /// Gets the value associated with a key, if present.
    fn get(&self, key: &str) -> Option<StateValue>;

    /// Sets or replaces the value for a key.
    fn set(&self, key: String, value: StateValue);

    /// Save the updated state
    fn save(&self, state: Vec<(String, StateValue)>);

    /// Returns true if the session contains a StateValue for the key
    fn contains(&self, key: &str) -> bool;

    /// Removes the value for a key.
    fn remove(&self, key: &str);

    /// Clears all keys from the session.
    fn clear(&self);

    /// Returns all key-value pairs in the session.
    fn all(&self) -> Vec<(String, StateValue)>;
}


#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StateValue {
    String(String),
    Number(f64),
    Boolean(bool),
    List(Vec<StateValue>),
    #[schemars(with = "HashMap<String, StateValue>")]
    Map(DashMap<String, StateValue>),
    Null,
}

impl PartialEq for StateValue {
    fn eq(&self, other: &Self) -> bool {
        use StateValue::*;
        match (self, other) {
            (String(a), String(b)) => a == b,
            (Number(a), Number(b)) => a == b,
            (Boolean(a), Boolean(b)) => a == b,
            (List(a), List(b)) => a == b,
            (Null, Null) => true,
            (Map(a), Map(b)) => {
                let a_map: HashMap<_, _> = a.iter().map(|r| (r.key().clone(), r.value().clone())).collect();
                let b_map: HashMap<_, _> = b.iter().map(|r| (r.key().clone(), r.value().clone())).collect();
                a_map == b_map
            }
            _ => false,
        }
    }
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

    pub fn as_map(&self) -> Option<&DashMap<String, StateValue>> {
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
                let mut map = serde_json::Map::new();
                for r in m.iter() {
                    map.insert(r.key().clone(), r.value().to_json());
                }
                Value::Object(map)
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


#[derive(Clone, Debug)]
pub struct InMemoryState {
    store: Arc<DashMap<String, StateValue>>,
    nodes: Arc<Mutex<VecDeque<String>>>,
    flows: Arc<Mutex<VecDeque<String>>>,
}

impl InMemoryState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { 
            store: Arc::new(DashMap::new()),
            nodes: Arc::new(Mutex::new(VecDeque::new())),
            flows: Arc::new(Mutex::new(VecDeque::new())),
        })
    }
}
#[async_trait]
impl SessionStateType for InMemoryState {
    fn get(&self, key: &str) -> Option<StateValue> {
        self.store.get(key).map(|v| v.clone())
    }

    fn set(&self, key: String, value: StateValue) {
        self.store.insert(key.to_string(), value);
    }

    fn contains(&self, key: &str) -> bool {
        self.store.contains_key(key)
    }

    fn remove(&self, key: &str) {
        self.store.remove(key);
    }

    fn clear(&self) {
        self.store.clear();
    }

    fn save(&self, state: Vec<(String, StateValue)>) {
        self.store.clear();
        for (key, value) in state {
            self.store.insert(key, value);
        }
    }

    fn all(&self) -> Vec<(String, StateValue)> {
        self.store
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect()
    }

    fn flows(&self) -> Option<Vec<String>> {
        Some(self.flows.lock().unwrap().iter().cloned().collect())
    }

    fn add_flow(&self, flow: String) {
        let mut q = self.flows.lock().unwrap();
        if !q.contains(&flow) {
            q.push_back(flow);
        }
    }

    fn set_flows(&self, flows: Vec<String>) {
        let mut q = self.flows.lock().unwrap();
        q.clear();
        q.extend(flows);
    }

    fn pop_node(&self) -> Option<String> {
        self.nodes.lock().unwrap().pop_front()
    }

    fn nodes(&self) -> Option<Vec<String>> {
        Some(self.nodes.lock().unwrap().iter().cloned().collect())
    }

    fn add_node(&self, node: String) {
        let mut q = self.nodes.lock().unwrap();
        if !q.contains(&node) {
            q.push_back(node);
        }
    }

    fn set_nodes(&self, nodes: Vec<String>) {
        let mut q = self.nodes.lock().unwrap();
        q.clear();
        q.extend(nodes);
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
    use std::collections::HashMap;

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

        let expected = {
            let mut m = HashMap::new();
            m.insert("k".into(), StateValue::Null);
            m
        };

        let map_data: DashMap<String, StateValue> = DashMap::new();
        map_data.insert("k".into(), StateValue::Null);

        let map: HashMap<_, _> = map_data.iter().map(|r| (r.key().clone(), r.value().clone())).collect();
        assert_eq!(map, expected);

        assert_eq!(StateValue::Null.as_str(), None);
    }

    #[tokio::test]
    async fn test_in_memory_state_store() {
        let store = InMemoryState::new();

        // Prepare a Vec of (String, StateValue)
        let state = vec![
            ("test".to_string(), StateValue::Boolean(true))
        ];

        // Save the state using StateSaver
        store.save(state);

        // Load and check the state using StateStore
        let loaded = store.all();

        // Convert to HashMap for easy lookup
        let map: std::collections::HashMap<_, _> = loaded.into_iter().collect();

        assert_eq!(map.get("test"), Some(&StateValue::Boolean(true)));
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
        let store = InMemoryState::new();

        let state = vec![
            ("x".to_string(), StateValue::Number(3.14))
        ];

        store.save(state);

        let loaded = store.all();
        let map: std::collections::HashMap<_, _> = loaded.into_iter().collect();

        assert_eq!(map.get("x"), Some(&StateValue::Number(3.14)));
    }
}
