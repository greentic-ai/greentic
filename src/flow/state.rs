use async_trait::async_trait;
use dashmap::DashMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::sync::broadcast;

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
    fn peek_node(&self) -> Option<String>;
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
    Integer(i64),
    Float(f64),
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
            (Integer(a), Integer(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (Boolean(a), Boolean(b)) => a == b,
            (List(a), List(b)) => a == b,
            (Null, Null) => true,
            (Map(a), Map(b)) => {
                let a_map: HashMap<_, _> = a
                    .iter()
                    .map(|r| (r.key().clone(), r.value().clone()))
                    .collect();
                let b_map: HashMap<_, _> = b
                    .iter()
                    .map(|r| (r.key().clone(), r.value().clone()))
                    .collect();
                a_map == b_map
            }
            _ => false,
        }
    }
}

impl StateValue {
    pub fn as_string(&self) -> Option<String> {
        if let StateValue::String(s) = self {
            Some(s.clone())
        } else {
            None
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        if let StateValue::Integer(n) = self {
            Some(*n)
        } else {
            None
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        if let StateValue::Float(n) = self {
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
            StateValue::Integer(n) => json!(n),
            StateValue::Float(n) => json!(n),
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
            Value::Number(n) => {
                if n.is_i64() {
                    Ok(StateValue::Integer(n.as_i64().ok_or(())?))
                } else if n.is_f64() {
                    Ok(StateValue::Float(n.as_f64().ok_or(())?))
                } else {
                    Err(()) // unlikely unless it's u64 and you don’t support it
                }
            }
            Value::Bool(b) => Ok(StateValue::Boolean(b)),
            Value::Array(a) => Ok(StateValue::List(
                a.into_iter()
                    .filter_map(|v| StateValue::try_from(v).ok())
                    .collect(),
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
        let flows = self
            .flows
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<String>>();
        if flows.is_empty() { None } else { Some(flows) }
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

    fn peek_node(&self) -> Option<String> {
        self.nodes().and_then(|nodes| nodes.first().cloned())
    }

    fn nodes(&self) -> Option<Vec<String>> {
        let nodes = self
            .nodes
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<String>>();
        let all_nodes = if nodes.is_empty() { None } else { Some(nodes) };
        all_nodes
    }

    fn add_node(&self, node: String) {
        let mut q = self.nodes.lock().unwrap();
        if !q.contains(&node) {
            q.push_back(node.clone());
        }
    }

    fn set_nodes(&self, nodes: Vec<String>) {
        let mut q = self.nodes.lock().unwrap();
        q.clear();
        q.extend(nodes.clone());
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
    use tokio::time::{Duration, timeout};

    #[test]
    fn test_state_value_accessors() {
        let string = StateValue::String("hello".into());
        assert_eq!(string.as_string(), Some("hello".to_string()));
        assert_eq!(string.as_int(), None);

        let number = StateValue::Float(42.0);
        assert_eq!(number.as_float(), Some(42.0));
        assert_eq!(number.as_int(), None);
        assert_eq!(number.as_string(), None);

        let number = StateValue::Integer(42);
        assert_eq!(number.as_int(), Some(42));
        assert_eq!(number.as_float(), None);
        assert_eq!(number.as_string(), None);

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

        let map: HashMap<_, _> = map_data
            .iter()
            .map(|r| (r.key().clone(), r.value().clone()))
            .collect();
        assert_eq!(map, expected);

        assert_eq!(StateValue::Null.as_string(), None);
    }

    #[tokio::test]
    async fn test_in_memory_state_store() {
        let store = InMemoryState::new();

        // Prepare a Vec of (String, StateValue)
        let state = vec![("test".to_string(), StateValue::Boolean(true))];

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

        let state = vec![("x".to_string(), StateValue::Float(3.14))];

        store.save(state);

        let loaded = store.all();
        let map: std::collections::HashMap<_, _> = loaded.into_iter().collect();

        assert_eq!(map.get("x"), Some(&StateValue::Float(3.14)));
    }
}
