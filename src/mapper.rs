use std::collections::HashMap;

use dashmap::DashMap;

use handlebars::Handlebars;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::flow::state::StateValue;

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema, PartialEq )]
#[serde(tag = "type", rename = "map", rename_all = "snake_case")]
pub enum Mapper {
    Copy(CopyMapper),
    Rename(RenameMapper),
    Script(ScriptMapper),
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema )]
#[serde(rename = "map_out")]
pub enum MapperOutput {
    Simple(Value),                        // used for input mapping
    Result {
        payload: Value,
        #[schemars(with = "std::collections::HashMap<String, StateValue>")]
        state_updates: DashMap<String, StateValue>,
        #[schemars(with = "std::collections::HashMap<String, String>")]
        config_updates: DashMap<String, String>,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone, JsonSchema )]
#[serde(rename = "map_res")]
pub struct MapperResult {
    pub payload: Value,
    #[schemars(with = "std::collections::HashMap<String, StateValue>")]
    pub state_updates: DashMap<String, StateValue>,
    #[schemars(with = "std::collections::HashMap<String, String>")]
    pub config_updates: DashMap<String, String>,
}

impl MapperResult {
    pub fn from_payload(payload: Value) -> Self {
        Self {
            payload,
            state_updates: DashMap::new(),
            config_updates: DashMap::new(),
        }
    }
}

impl Mapper {
    pub fn apply_input(
        &self,
        payload: &Value,
        config: &DashMap<String, String>,
        state: Vec<(String, StateValue)>,
    ) -> Value {
        match self {
            Mapper::Copy(mapper) => mapper.apply(payload, config, state),
            Mapper::Rename(mapper) => mapper.apply(payload, config, state),
            Mapper::Script(mapper) => mapper.apply(payload, config, state),
        }
    }

    pub fn apply_result(
        &self,
        tool_output: &Value,
        config: &DashMap<String, String>,
        state: Vec<(String, StateValue)>,
    ) -> MapperResult {
        match self {
            Mapper::Script(m) => m.apply_result(tool_output, config, state), // new method
            _ => MapperResult::from_payload(self.apply_input(tool_output, config, state)),
        }
    }
}

/// A mapper that copies selected fields from payload, config, and state,
/// with an optional default value in case the field is not found.
/// 
/// Config and state only accept strings as keys. Payload also accepts
/// JSON pointers so you can look up a value inside the payload, e.g.
/// /parameters/days
/// 
/// # Example
/// ```json
/// {
///   "type": "copy",
///   "payload": ["a", "b", { "c": 4 }],
///   "config": ["env", { "region": "eu-west-1" }],
///   "state": ["done", { "tries": 0 }]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename = "copy")]
pub struct CopyMapper {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Vec<CopyKey>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Vec<CopyKey>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<Vec<CopyKey>>,
}
/// Describes a key to copy from a source, with an optional default if missing.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum CopyKey {
    Key(String),
    #[schemars(with = "std::collections::HashMap<String, Value>")]
    WithDefault(DashMap<String, Value>),
}

impl PartialEq for CopyKey {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (CopyKey::Key(a), CopyKey::Key(b)) => a == b,
            (CopyKey::WithDefault(a), CopyKey::WithDefault(b)) => {
                // Convert DashMap to regular HashMap for comparison
                let a_map: HashMap<_, _> = a.iter().map(|entry| (entry.key().clone(), entry.value().clone())).collect();
                let b_map: HashMap<_, _> = b.iter().map(|entry| (entry.key().clone(), entry.value().clone())).collect();
                a_map == b_map
            }
            _ => false,
        }
    }
}
impl CopyKey {
    pub fn extract(&self) -> (String, Option<Value>) {
        match self {
            CopyKey::Key(k) => (k.clone(), None),
            CopyKey::WithDefault(map) => {
                let entry = map.iter().next().expect("default map should have one entry");
                let k = entry.key();
                let v = entry.value();
                (k.clone(), Some(v.clone()))
            }
        }
    }
}

impl CopyMapper {
    pub fn apply(
        &self,
        payload: &Value,
        config: &DashMap<String, String>,
        state: Vec<(String, StateValue)>,
    ) -> Value {
        let mut out = serde_json::Map::new();

        if let Some(keys) = &self.payload {
            for key in keys {
                let (mut k, default) = key.extract();
                if k.contains('/') {
                    if !k.starts_with('/') {
                        k.insert(0, '/');
                    }
                    let val = payload.pointer(&k).cloned().or(default);
                    if let Some(v) = val {
                        let field = k.rsplitn(2, '/').next().unwrap().to_string();
                        out.insert(field, v);
                    }
                } else {
                    let val = payload.get(&k).map(|s| json!(s)).or(default);
                    if let Some(v) = val {
                        out.insert(k, v);
                    }
                }
            }
        }

        if let Some(keys) = &self.config {
            for key in keys {
                let (k, default) = key.extract();
                let val = config.get(&k).map(|s| json!(s)).or(default);
                if let Some(v) = val {
                    out.insert(k, v);
                }
            }
        }

        if let Some(keys) = &self.state {
            for key in keys {
                let (k, default) = key.extract();
                let val = state
                    .iter()
                    .find(|(key, _)| key == &k)
                    .map(|(_, v)| v.to_json())
                    .or(default);
                if let Some(v) = val {
                    out.insert(k, v);
                }
            }
        }

        Value::Object(out)
    }
}

// ---------------------------------------------
// Tier 2: RenameMapper
// ---------------------------------------------
/// A mapper that renames fields and optionally provides defaults.
///
/// # Example
/// ```json
/// {
///   "type": "rename",
///   "x": { "from": "payload", "key": "user_id" },
///   "y": { "from": "config", "key": "region", "default": "us-west" },
///   "z": { "from": "state", "key": "stage", "default": "init" }
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename = "rename")]
pub struct RenameMapper(
    #[schemars(with = "std::collections::HashMap<String, StateValue>")]
    pub DashMap<String, SourceField>
);

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "from", rename="source")]
pub enum SourceField {
    #[serde(rename = "payload")]
    Payload { key: String, #[serde(default)] default: Option<Value> },
    #[serde(rename = "config")]
    Config { key: String, #[serde(default)] default: Option<Value> },
    #[serde(rename = "state")]
    State { key: String, #[serde(default)] default: Option<Value> },
}
impl PartialEq for RenameMapper {
    fn eq(&self, other: &Self) -> bool {
        let self_map: HashMap<_, _> = self.0.iter().map(|kv| (kv.key().clone(), kv.value().clone())).collect();
        let other_map: HashMap<_, _> = other.0.iter().map(|kv| (kv.key().clone(), kv.value().clone())).collect();
        self_map == other_map
    }
}
impl RenameMapper {
    pub fn apply(
        &self,
        payload: &Value,
        config: &DashMap<String, String>,
        state: Vec<(String, StateValue)>,
    ) -> Value {
       let mut out = serde_json::Map::new();

        for entry in self.0.iter() {
            let out_key = entry.key();
            let source = entry.value();

            let val = match source {
                SourceField::Payload { key, default } => {
                    payload.get(key).cloned().or_else(|| default.clone())
                }
                SourceField::Config { key, default } => {
                    config.get(key).map(|v| json!(v.clone())).or_else(|| default.clone())
                }
                SourceField::State { key, default } => {
                    state
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.to_json())
                    .or_else(|| default.clone())
                }
            };

            if let Some(v) = val {
                out.insert(out_key.clone(), v);
            }
        }


        Value::Object(out)
    }
    
}

// ---------------------------------------------
// Tier 3: ScriptMapper (stub)
// ---------------------------------------------
/// A mapper using Handlebars templating to build structured outputs.
///
/// # Example
/// ```json
/// {
///   "type": "script",
///   "template": "{ \"payload\": { \"temperature\": \"{{weather.temp_c}}\" } }"
/// }
/// ```
/// Variables from `payload`, `config`, and `state` are flattened and available
/// to the template context.
/// 
/// If the payload variable is an array coming from a tool, then automatically it will
/// be converted into an object with key root: { ... the array ... }.
/// You can then get information out in the following way:
/// ```json
/// {"payload": { "temperature": {{root.[0].current.temp_c}} }}
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename = "script")]
pub struct ScriptMapper {
    pub template: String,
}

impl ScriptMapper {
    pub fn apply(
        &self,
        payload: &Value,
        config: &DashMap<String, String>,
        state: Vec<(String, StateValue)>,
    ) -> Value {
        let mut merged = serde_json::Map::new();

        if let Some(obj) = payload.as_object() {
            for (k, v) in obj {
                merged.insert(k.clone(), v.clone());
            }
        }

        for entry in config.iter() {
            merged.insert(entry.key().clone(), Value::String(entry.value().clone()));
        }

        for (key,value) in state.iter() {
            merged.insert(key.clone(), value.to_json());
        }

        let hbs = Handlebars::new();

        match hbs.render_template(&self.template, &merged) {
            Ok(rendered) => json!({ "result": rendered }),
            Err(e) => json!({ "error": format!("render error: {}", e) }),
        }
    }

    pub fn apply_result(
        &self,
        output: &Value,
        config: &DashMap<String, String>,
        state: Vec<(String, StateValue)>,
    ) -> MapperResult {
        let mut merged = serde_json::Map::new();

        let normalized_output = if output.is_array() {
            json!({ "root": output })
        } else {
            output.clone()
        };

        if let Some(obj) = normalized_output.as_object() {
            for (k, v) in obj {
                merged.insert(k.clone(), v.clone());
            }
        }

        for entry in config.iter() {
            merged.insert(entry.key().clone(), Value::String(entry.value().clone()));
        }

        for (key,value) in state.iter() {
            merged.insert(key.clone(), value.to_json());
        }

        let hbs = Handlebars::new();
        match hbs.render_template(&self.template, &merged) {
            Ok(rendered) => match serde_json::from_str::<Value>(&rendered) {
                Ok(Value::Object(mut obj)) => {
                    let payload = obj.remove("payload").unwrap_or(json!({}));
                    let state_updates = obj
                        .remove("state")
                        .and_then(|v| v.as_object().cloned())
                        .map(|m| {
                            m.into_iter()
                                .filter_map(|(k, v)| Some((k, StateValue::try_from(v).ok()?)))
                                .collect()
                        })
                        .unwrap_or_default();

                    let config_updates = obj
                        .remove("config")
                        .and_then(|v| v.as_object().cloned())
                        .map(|m| {
                            m.into_iter()
                                .filter_map(|(k, v)| Some((k, v.as_str()?.to_string())))
                                .collect()
                        })
                        .unwrap_or_default();

                    MapperResult {
                        payload,
                        state_updates,
                        config_updates,
                    }
                }
                _ => MapperResult::from_payload(json!({ "error": "invalid json structure" })),
            },
            Err(e) => MapperResult::from_payload(json!({ "error": format!("template error: {}", e) })),
        }
    
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use crate::flow::state::StateValue;

    #[test]
    fn test_copy_mapper_with_slash_pointer_and_simple_keys() {
        use super::*;
        use serde_json::json;
        use crate::flow::state::StateValue;

        // Mapper that pulls q and days out of a nested "parameters" object,
        // plus simple keys for config and state.
        let mapper = CopyMapper {
            payload: Some(vec![
                CopyKey::Key("/parameters/q".into()),
                CopyKey::Key("/parameters/days".into()),
            ]),
            config: Some(vec![
                CopyKey::Key("env".into()),
            ]),
            state: Some(vec![
                CopyKey::Key("done".into()),
            ]),
        };

        // Nested payload, plus flat config and state maps
        let payload = json!({
            "parameters": {
                "q": "nested_q",
                "days": 42
            }
        });
        let config: DashMap<String, String> = [("env".to_string(), "production".to_string())]
            .into_iter()
            .collect();
        let state: Vec<(String, StateValue)> = vec![("done".to_string(), StateValue::String("yes".into()))];

        let output = mapper.apply(&payload, &config, state);

        // Should have unwrapped both pointer keys
        assert_eq!(output["q"], "nested_q");
        assert_eq!(output["days"], 42);

        // And also picked up config/state keys as usual
        assert_eq!(output["env"], "production");
        assert_eq!(output["done"], "yes");
    }

    #[test]
    fn test_copy_mapper() {
        let mapper = CopyMapper {
            payload: Some(vec![
                CopyKey::WithDefault({
                    let map = DashMap::new();
                    map.insert("a".into(), json!("fallback_a"));
                    map
                }),
                CopyKey::Key("b".into()),
            ]),
            config: Some(vec![
                CopyKey::WithDefault({
                    let map = DashMap::new();
                    map.insert("env".into(), json!("prod"));
                    map
                }),
            ]),
            state: Some(vec![
                CopyKey::WithDefault({
                    let map = DashMap::new();
                    map.insert("done".into(), json!("yes"));
                    map
                }),
            ]),
        };

        let payload = json!({ "a": "from_payload", "b": "from_payload_b" });
        let config = [("env".into(), "from_config".into())].iter().cloned().collect();
        let state = vec![("done".into(), StateValue::String("from_state".into()))];

        let output = mapper.apply(&payload, &config, state);
        assert_eq!(output["a"], "from_payload");
        assert_eq!(output["b"], "from_payload_b");
        assert_eq!(output["env"], "from_config");
        assert_eq!(output["done"], "from_state");
    }

    #[test]
    fn test_rename_mapper() {
        let mapping = DashMap::new();
        mapping.insert("x".into(), SourceField::Payload {
            key: "a".into(),
            default: Some(json!("default_val")),
        });
        mapping.insert("y".into(), SourceField::Config {
            key: "b".into(),
            default: None,
        });
        mapping.insert("z".into(), SourceField::State {
            key: "c".into(),
            default: Some(json!("fallback_state")),
        });

        let mapper = RenameMapper(mapping);

        let payload = json!({ "a": "payload_val" });
        let config = [("b".into(), "config_val".into())].iter().cloned().collect();
        let state = vec![("c".into(), StateValue::String("state_val".into()))];

        let output = mapper.apply(&payload, &config, state);
        assert_eq!(output["x"], "payload_val");
        assert_eq!(output["y"], "config_val");
        assert_eq!(output["z"], "state_val");
    }

    #[test]
    fn test_script_mapper() {
        let mapper = ScriptMapper {
            template: "Hello {{name}}, config says {{env}} and state token is {{{token}}}.".into(),
        };

        let payload = json!({ "name": "Alice" });
        let config = [("env".into(), "prod".into())].iter().cloned().collect();
        let state = vec![("token".into(), StateValue::String("xyz123".into()))];

        let output = mapper.apply(&payload, &config, state);
        assert_eq!(
            output["result"],
            "Hello Alice, config says prod and state token is xyz123."
        );
    }

    #[test]
    fn test_script_mapper_apply_result_basic_payload() {
        let mapper = ScriptMapper {
            template: r#"{"payload": {"result": "{{value}}"}}"#.into(),
        };

        let output = json!({ "value": "42" });
        let config = DashMap::new();
        let state = vec![];

        let result = mapper.apply_result(&output, &config, state);
        assert_eq!(result.payload["result"], "42");
        assert!(result.state_updates.is_empty());
        assert!(result.config_updates.is_empty());
    }

    #[test]
    fn test_script_mapper_apply_result_state_and_config() {
        let mapper = ScriptMapper {
            template: r#"{
                "payload": { "message": "{{msg}}" },
                "state": { "done": "true" },
                "config": { "step": "post" }
            }"#.into(),
        };

        let output = json!({ "msg": "Completed" });
        let config = DashMap::new();
        let state = vec![];

        let result = mapper.apply_result(&output, &config, state);

        assert_eq!(result.payload["message"], "Completed");
        assert_eq!(result.state_updates.get("done").as_deref(), Some(&StateValue::String("true".into())));
        assert_eq!(result.config_updates.get("step").as_deref(), Some(&"post".to_string()));
    }

    #[test]
    fn test_script_mapper_apply_result_missing_fields() {
        let mapper = ScriptMapper {
            template: r#"{
                "payload": { "info": "{{data}}" }
            }"#.into(),
        };

        let output = json!({ "data": "X" });
        let config = DashMap::new();
        let state = vec![];

        let result = mapper.apply_result(&output, &config, state);

        assert_eq!(result.payload["info"], "X");
        assert!(result.state_updates.is_empty());
        assert!(result.config_updates.is_empty());
    }

    #[test]
    fn test_script_mapper_apply_result_invalid_json() {
        let mapper = ScriptMapper {
            template: "this is not json {{oops}}".into(),
        };

        let output = json!({ "oops": "!!" });
        let config = DashMap::new();
        let state = vec![];

        let result = mapper.apply_result(&output, &config, state);

        assert!(result.payload.get("error").is_some());
        assert!(result.state_updates.is_empty());
    }

    #[test]
    fn test_script_mapper_apply_result_template_error() {
        let mapper = ScriptMapper {
            template: "{{#unclosed".into(), // malformed mustache
        };

        let output = json!({ "any": "value" });
        let config = DashMap::new();
        let state = vec![];

        let result = mapper.apply_result(&output, &config, state);

        assert!(result.payload.get("error").is_some());
    }

    #[test]
    fn test_script_mapper_extract_temperature() {
        use crate::mapper::{Mapper, ScriptMapper};
        use serde_json::json;

        let input_json = serde_json::from_str::<serde_json::Value>(r#"
        [
        {
            "current": {
            "cloud": 0.0,
            "condition": {
                "code": 1009,
                "icon": "//cdn.weatherapi.com/weather/64x64/day/122.png",
                "text": "Overcast"
            },
            "feelslike_c": 24.2,
            "feelslike_f": 75.6,
            "gust_kph": 17.4,
            "gust_mph": 10.8,
            "humidity": 31.0,
            "is_day": 1,
            "last_updated": "2025-05-14 16:30",
            "last_updated_epoch": 1747236600,
            "precip_in": 0.0,
            "precip_mm": 0.0,
            "pressure_in": 30.12,
            "pressure_mb": 1020.0,
            "temp_c": 23.1,
            "temp_f": 73.6,
            "uv": 3.3,
            "vis_km": 10.0,
            "vis_miles": 6.0,
            "wind_degree": 58.0,
            "wind_dir": "ENE",
            "wind_kph": 15.1,
            "wind_mph": 9.4
            },
            "location": {
            "country": "United Kingdom",
            "lat": 51.5171,
            "localtime": "2025-05-14 16:38",
            "localtime_epoch": 1747237094,
            "lon": -0.1062,
            "name": "London",
            "region": "City of London, Greater London",
            "tz_id": "Europe/London"
            }
        }
        ]
        "#).unwrap();

        // Extract temperature from input[0].current.temp_c
        let script = r#"{"payload": { "temperature": {{root.[0].current.temp_c}} }}"#;

        let mapper = ScriptMapper { template: script.into() };
        let mapper = Mapper::Script(mapper);

        let result = mapper.apply_result(&input_json, &DashMap::new(), vec![]);

        assert_eq!(
            result.payload["temperature"],
            json!(23.1),
            "Expected temperature to be extracted correctly"
        );
    }


}
