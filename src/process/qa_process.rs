use std::{collections::HashMap, fmt};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use handlebars::Handlebars;
use regex::Regex;
use schemars::{schema::RootSchema, schema_for, JsonSchema};
use serde_json::{json,  Value as JsonValue};
use serde::{
    de::{self, MapAccess, Visitor},
    ser::SerializeMap,
    Deserialize, Deserializer, Serialize, Serializer,
};

use crate::{agent::manager::BuiltInAgent, message::Message, node::{NodeContext, NodeErr, NodeError, NodeOut, NodeType}, state::StateValue, util::render_handlebars};
/// QAProcessNode
///
/// A “stateful” question‐and‐answer node that guides a user through a series of prompts,
/// validates and stores their answers in flow‐state, and finally routes the completed
/// responses along one of several connections based on configurable rules.
///
/// ## Configuration (`QAProcessConfig`)
///
/// - `welcome_template: String`  
///   A Handlebars template sent only once at the very start of a session.  
///   You can reference any existing `{{state.foo}}` keys here.
///
/// - `questions: Vec<QuestionConfig>`  
///   The ordered list of questions to ask.  Each question has:
///   ```
///   QuestionConfig {
///       id:          String,               // unique identifier for routing
///       prompt:      String,               // what to send as “text”
///       answer_type: AnswerType,           // how to parse user reply
///       state_key:   String,               // where to store in flow‐state
///       validate:    Option<ValidationRule>, // optional regex or numeric range
///   }
///   ```
///   Available `AnswerType`s:
///   ```text
///   Text               → free‐form string (optional regex validation)
///   Number             → parse as f64  (optional Range { min, max })
///   Date               → ISO8601 / RFC3339 dates
///   Choice { options } → user must pick one of the provided strings
///   ```
///   Optional `ValidationRule`s:
///   ```text
///   Regex(String)             // e.g. Regex("^\\d{4}-\\d{2}-\\d{2}$")
///   Range { min: f64, max: f64 } // numeric bounds
///   ```
///
/// - `fallback_agent: Option<BuiltInAgent>`  
///   If the raw reply doesn’t parse or validate, you can hand it off to an LLM agent
///   for interpretation (e.g. spell‐check, free‐text name parsing).  
///
/// - `routing: Vec<RoutingRule>`  
///   After all questions are answered, pick the next connection based on a `Condition`.
///   Each `RoutingRule` is:
///   ```
///   RoutingRule {
///     condition: Condition,    // one of Always, Equals, GreaterThan, LessThan, Custom
///     to:        String,       // node ID or channel name to send the final payload to
///   }
///   ```
///   ```markdown
///   Conditions (all inside a top-level `condition:` key; omit or set to `null` for “always”):
/// 
///   ```yaml
///   # exact equality
///   condition:
///     equals:
///       question_id: "age"
///       value: 21
///   
///   # numeric greater-than
///   condition:
///     greater_than:
///       question_id: "score"
///       threshold: 50.0
///   
///   # numeric less-than
///   condition:
///     less_than:
///       question_id: "score"
///       threshold: 20.0
///   
///   # arbitrary Handlebars boolean expr
///   condition:
///     custom:
///       expr: "state.score >= 75 && state.passed == true"
///   
///   # omit entirely (or explicitly `condition: null`) → always matches
///
/// ## Example YAML Usage
///
/// ```yaml
/// nodes:
///   ask_user:
///     qa:
///       welcome_template: >
///         Welcome! Let's gather a few details first.
///       questions:
///         - id:       "name"
///           prompt:   "👉 What is your full name?"
///           answer_type: Text
///           state_key: "user_name"
///
///         - id:       "age"
///           prompt:   "👉 How old are you?"
///           answer_type: Number
///           state_key: "user_age"
///           validate:
///             Range:
///               min: 0
///               max: 120
///
///         - id:       "birthdate"
///           prompt:   "👉 When is your birthday? (YYYY-MM-DD)"
///           answer_type: Date
///           state_key: "user_birthdate"
///           validate:
///             Regex: "^\\d{4}-\\d{2}-\\d{2}$"
///
///         - id:       "color"
///           prompt:   "👉 Pick a color: red, green or blue."
///           answer_type:
///             Choice:
///               options: ["red","green","blue"]
///           state_key: "favorite_color"
///
///       # if they are under 18, send to "underage" flow; else to "main_process"
///       routing:
///         - condition:
///             Less_than:
///               question_id: "age"
///               threshold: 18
///           to: "underage"
///
///         - to: "main_process"
/// ```
///
/// In the above:
/// 1. **First** the user receives the `welcome_template`.  
/// 2. **Then** each `prompt` is sent in order, and their reply is parsed/validated.  
/// 3. **Finally**, all answers are in `ctx.state` under `"user_name"`, `"user_age"`, etc., and
///    the node emits a single `NodeOut::one(...)` carrying the full answers object to the
///    connection named by the matching `RoutingRule`.
///
/// > **Tip:** Use Handlebars in your prompts or `welcome_template` to show previously‐collected
/// > values:  
/// > ```yaml
/// > prompt: "Nice to meet you, {{state.user_name}}! What’s your favorite number?"
/// > ```  
///
/// This makes `QAProcessNode` a powerful way to build multi‐step, stateful forms or wizards
/// entirely in your flow YAML, without writing any extra Rust!
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct QAProcessNode {
    #[serde(flatten)]
    pub config: QAProcessConfig,
}

#[async_trait]
#[typetag::serde]
impl NodeType for QAProcessNode {
    fn type_name(&self) -> String { "qa".into() }
    fn schema(&self) -> RootSchema { schema_for!(QAProcessConfig) }

    async fn process(&self, msg: Message, ctx: &mut NodeContext)
      -> Result<NodeOut, NodeErr>
    {
        let session = msg.session_id()
                       .expect("channels must set session");

         // read (or default) our little counter
        let idx = ctx
            .get_state("qa.current_question")
            .and_then(StateValue::as_number)
            .map(|n| n as usize)
            .unwrap_or(0);
        

        // 1) first‐time visitor?
        if idx == 0 {
            // clear out any previous answers
            for q in &self.config.questions {
                ctx.delete_state(&q.state_key);
            }
            // prime the counter
            ctx.set_state("qa.current_question", StateValue::Number(0.));
        }

        // if we haven't asked this question yet, ask it
        if idx < self.config.questions.len() {
            // prompt
            let qcfg = &self.config.questions[idx];
            let prompt = render_handlebars(&qcfg.prompt, &ctx.get_all_state());
            let out = Message::new(
                &msg.id(),
                json!({ "text": prompt }),
                Some(session.to_string()),
            );
            return Ok(NodeOut::reply(out));
        }

        // now we have asked all questions, but haven't yet stored the last answer
        // so idx == questions.len()
        // (in practice, you'd store after receiving payload and then bump the counter,
        //  but you can also interleave: ask → receive → store → ask → …)

        // 2) Validate & store answer to question idx-1
        let last_q = &self.config.questions[idx - 1];
        let payload_val = msg.payload();
        let raw = payload_val.as_str().unwrap_or("");
        match parse_and_validate(raw, &last_q.answer_type, last_q.validate.as_ref()) {
            Err(err) => {
                // re‐ask the same question
                let prompt = render_handlebars(&last_q.prompt, &ctx.get_all_state());
                let out = Message::new(
                    &msg.id(),
                    json!({ "text": format!("I didn’t understand: {}\n{}", err, prompt) }),
                    Some(session.to_string()),
                );
                return Ok(NodeOut::all(out));
            }
            Ok(parsed_json) => {
                // first, convert the JSON value into your StateValue enum
                let state_val = StateValue::try_from(parsed_json.clone())
                    .map_err(|e| NodeErr::all(NodeError::ExecutionFailed(
                        format!("Failed to convert answer to StateValue: {:?}", e)
                    )))?;
                
                // store it under the user‐specified state_key
                ctx.set_state(&last_q.state_key, state_val);

                // bump counter to idx+1
                ctx.set_state(
                    "qa.current_question",
                    StateValue::Number((idx + 1) as f64),
                );
            }
        }

        // 3) Are there more questions?
        let new_idx = idx;
        if new_idx < self.config.questions.len() {
            let next_q = &self.config.questions[new_idx];
            let prompt = render_handlebars(&next_q.prompt, &ctx.get_all_state());
            let out = Message::new(
                &msg.id(),
                json!({ "text": prompt }),
                Some(session.to_string()),
            );
            return Ok(NodeOut::all(out));
        }

        // 4) All done! run routing rules against ctx.state
        let answers = ctx.get_all_state();
        let json_answers: HashMap<String, JsonValue> = answers
            .iter()
            .map(|(k, v)| (k.clone(), v.to_json()))
            .collect();
        for rule in &self.config.routing {
            if rule.matches(&json_answers) {
                let payload = JsonValue::Object(
                    answers.into_iter().map(|(k, v)| (k, v.to_json())).collect()
                );
                // wrap that JSON in a Message, preserving the same session:
                let out_msg = Message::new(
                    &msg.id(),
                    payload,
                    msg.session_id(),
                );
                return Ok(NodeOut::one(out_msg, rule.to.clone()));
            }
        }

        Err(NodeErr::all(NodeError::ExecutionFailed(
            "no routing rule matched".into(),
        )))
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}


#[derive(Debug, Clone, Serialize, Deserialize, )]
struct SessionState {
  current_question: usize,
  answers: HashMap<String, JsonValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct QAProcessConfig {
    /// A one‐time “welcome” message (Handlebars template) when a new user/session arrives.
    pub welcome_template: String,

    /// The list of questions to ask, in order.
    pub questions: Vec<QuestionConfig>,

    /// If the user’s free‐text reply doesn’t parse as any of our expected answer types,
    /// you can optionally hand them off to an LLM agent to try to interpret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_agent: Option<BuiltInAgent>,

    /// Once all questions are answered, pick the outgoing connection by matching one of these.
    pub routing: Vec<RoutingRule>,
}

impl PartialEq for QAProcessConfig {
    fn eq(&self, other: &Self) -> bool {
        self.welcome_template == other.welcome_template
            && self.questions == other.questions
            && self.routing == other.routing
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct QuestionConfig {
    /// Unique ID for this question (used in state and in routing conditions).
    pub id: String,

    /// What to send to the user.  Can interpolate `{{state.foo}}`.
    pub prompt: String,

    /// How to parse the user’s reply.
    pub answer_type: AnswerType,

    /// Where in `NodeContext.state` to store the parsed value.
    pub state_key: String,

    /// Optional regexp or range check to validate their answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validate: Option<ValidationRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all="snake_case")]
pub enum AnswerType {
    Text,
    Number,
    Date,                     // ISO8601
    Choice { options: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum ValidationRule {
    /// foo              → free‐form string
    Regex(String),

    ///   range: { min:…, max:… }
    Range { range: RangeParams },
}

/// helper for your `range:` mapping
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RangeParams {
    pub min: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct RoutingRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
    pub to: String,
}

impl RoutingRule {
    pub fn matches(&self, answers: &HashMap<String, JsonValue>) -> bool {
        match &self.condition {
            None => true,
            Some(cond) => cond.matches(answers),
        }
    }
}

impl Condition {
    pub fn matches(&self, answers: &HashMap<String, JsonValue>) -> bool {
        match self {

            Condition::Equals { question_id, value } => {
                answers
                    .get(question_id)
                    .map_or(false, |v| v == value)
            }

            Condition::Custom { expr } => {
                // spin up a fresh Handlebars
                let mut h = Handlebars::new();
                // disable HTML‐escaping so our “true”/“false” come through verbatim
                h.register_escape_fn(handlebars::no_escape);

                // wrap the user’s expr in an #if:
                let tmpl = format!("{{{{#if ({})}}}}true{{{{else}}}}false{{{{/if}}}}", expr);
                match h.render_template(&tmpl, answers) {
                    Ok(s) => s == "true",
                    Err(_) => false,
                }
            }

            Condition::GreaterThan { question_id, threshold } => {
                answers
                    .get(question_id)
                    .and_then(|v| v.as_f64())
                    .map_or(false, |n| n > *threshold)
            }

            Condition::LessThan { question_id, threshold } => {
                answers
                    .get(question_id)
                    .and_then(|v| v.as_f64())
                    .map_or(false, |n| n < *threshold)
            }
        }
    }
}


/// Try to parse the raw string into the given `answer_type`, then optionally
/// apply `validate` (regex or numeric range).  On success you get back a
/// JsonValue (String, Number, or in the date case a String timestamp).  
/// On failure, an Err(message) suitable for re-prompting the user.
pub fn parse_and_validate(
    raw: &str,
    answer_type: &AnswerType,
    validate: Option<&ValidationRule>,
) -> Result<JsonValue, String> {
    match answer_type {
        AnswerType::Text => {
            // first apply regex if present
            if let Some(ValidationRule::Regex(re)) = validate {
                let regex = Regex::new(re)
                    .map_err(|e| format!("internal regex error: {}", e))?;
                if !regex.is_match(raw) {
                    return Err(format!("must match /{}/", re));
                }
            }
            Ok(JsonValue::String(raw.to_owned()))
        }

        AnswerType::Number => {
            let v: f64 = raw.trim().parse().map_err(|_| "please enter a number".to_string())?;
            // range check if given
            if let Some(ValidationRule::Range { range: RangeParams{min, max }}) = validate {
                if v < *min || v > *max {
                    return Err(format!("must be between {} and {}", min, max));
                }
            }
            // use serde_json::json! to produce a Number
            Ok(json!(v))
        }

        AnswerType::Date => {
            // expect ISO8601 / RFC3339
            let dt: DateTime<Utc> = DateTime::parse_from_rfc3339(raw)
                .map_err(|_| "please use YYYY-MM-DD or full ISO8601 timestamp".to_string())?
                .with_timezone(&Utc);
            Ok(JsonValue::String(dt.to_rfc3339()))
        }

        AnswerType::Choice { options } => {
            // find a case‐insensitive match
            let norm = raw.trim();
            // exact match first
            if options.iter().any(|opt| opt == norm) {
                return Ok(JsonValue::String(norm.to_string()));
            }
            // or lowercase match
            if let Some(found) = options
                .iter()
                .find(|opt| opt.to_lowercase() == norm.to_lowercase())
            {
                return Ok(JsonValue::String(found.clone()));
            }
            // not found
            Err(format!(
                "please choose one of: {}",
                options.join(", ")
            ))
        }
    }
}

#[derive(Debug, Clone, PartialEq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Condition {
    Equals { question_id: String, value: JsonValue },
    Custom { expr: String },
    GreaterThan { question_id: String, threshold: f64 },
    LessThan    { question_id: String, threshold: f64 },
}

impl Serialize for Condition {
    fn serialize<S>(&self, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        // we'll emit a map of exactly one entry
        let mut map = ser.serialize_map(Some(1))?;
        match self {
            Condition::Equals { question_id, value } => {
                map.serialize_entry("equals", &json!({
                    "question_id": question_id,
                    "value": value
                }))?;
            }
            Condition::Custom { expr } => {
                map.serialize_entry("custom", &json!({ "expr": expr }))?;
            }
            Condition::GreaterThan { question_id, threshold } => {
                map.serialize_entry("greater_than", &json!({
                    "question_id": question_id,
                    "threshold": threshold
                }))?;
            }
            Condition::LessThan { question_id, threshold } => {
                map.serialize_entry("less_than", &json!({
                    "question_id": question_id,
                    "threshold": threshold
                }))?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Condition {
    fn deserialize<D>(deser: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct CondVisitor;
        impl<'de> Visitor<'de> for CondVisitor {
            type Value = Condition;
            fn expecting(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
                write!(fmt, "a single‐key map with a Condition variant")
            }
            fn visit_map<A>(self, mut map: A) -> Result<Condition, A::Error>
            where
                A: MapAccess<'de>,
            {
                let (key, value): (String, serde_yaml::Value) = map
                    .next_entry()?
                    .ok_or_else(|| de::Error::custom("Expected one entry in Condition map"))?;
                match key.as_ref() {
                    "equals" => {
                        // expect a map mapping question_id→…, value→…
                        let m: HashMap<String, serde_json::Value> =
                            serde_yaml::from_value(value).map_err(de::Error::custom)?;
                        let question_id = m.get("question_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::custom("Equals.question_id must be string"))?
                            .to_string();
                        let value = m.get("value")
                            .cloned()
                            .ok_or_else(|| de::Error::custom("Equals.value missing"))?;
                        Ok(Condition::Equals { question_id, value })
                    }
                    "custom" => {
                        let m: HashMap<String, String> =
                            serde_yaml::from_value(value).map_err(de::Error::custom)?;
                        let expr = m.get("expr")
                            .cloned()
                            .ok_or_else(|| de::Error::custom("Custom.expr missing"))?;
                        Ok(Condition::Custom { expr })
                    }
                    "greater_than" => {
                        let m: HashMap<String, serde_json::Value> =
                            serde_yaml::from_value(value).map_err(de::Error::custom)?;
                        let question_id = m.get("question_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::custom("GreaterThan.question_id must be string"))?
                            .to_string();
                        let threshold = m.get("threshold")
                            .and_then(|v| v.as_f64())
                            .ok_or_else(|| de::Error::custom("GreaterThan.threshold must be number"))?;
                        Ok(Condition::GreaterThan { question_id, threshold })
                    }
                    "less_than" => {
                        let m: HashMap<String, serde_json::Value> =
                            serde_yaml::from_value(value).map_err(de::Error::custom)?;
                        let question_id = m.get("question_id")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| de::Error::custom("LessThan.question_id must be string"))?
                            .to_string();
                        let threshold = m.get("threshold")
                            .and_then(|v| v.as_f64())
                            .ok_or_else(|| de::Error::custom("LessThan.threshold must be number"))?;
                        Ok(Condition::LessThan { question_id, threshold })
                    }
                    other => Err(de::Error::unknown_variant(other, &[
                        "always", "equals", "custom", "greater_than", "less_than",
                    ])),
                }
            }
        }
        deser.deserialize_map(CondVisitor)
    }
}

#[cfg(test)]
mod tests {
    use crate::process::manager::BuiltInProcess;

    use super::*;
    use serde_json::json;
    use chrono::Utc;
    use std::collections::HashMap;
    use serde_yaml::Value as YamlValue;

    #[test]
    fn parse_and_validate_text_no_regex() {
        let v = parse_and_validate("hello", &AnswerType::Text, None).unwrap();
        assert_eq!(v, JsonValue::String("hello".into()));
    }

    #[test]
    fn parse_and_validate_text_with_regex() {
        // only digits
        let vr = parse_and_validate("12345",
            &AnswerType::Text,
            Some(&ValidationRule::Regex(r"^\d+$".into()))
        ).unwrap();
        assert_eq!(vr, JsonValue::String("12345".into()));

        // fail non-digit
        let err = parse_and_validate("abc",
            &AnswerType::Text,
            Some(&ValidationRule::Regex(r"^\d+$".into()))
        ).unwrap_err();
        assert!(err.contains("must match"));
    }

    #[test]
    fn parse_and_validate_number_and_range() {
        // simple
        let v = parse_and_validate("3.14", &AnswerType::Number, None).unwrap();
        assert_eq!(v, json!(3.14));

        // invalid
        assert!(parse_and_validate("foo", &AnswerType::Number, None).is_err());

        // with range
        let rule = ValidationRule::Range {range:RangeParams { min: 0.0, max: 10.0 } };
        assert_eq!(parse_and_validate("5", &AnswerType::Number, Some(&rule)).unwrap(), json!(5.0));
        assert!(parse_and_validate("-1", &AnswerType::Number, Some(&rule)).is_err());
    }

    #[test]
    fn parse_and_validate_date() {
        // valid ISO8601
        let dt = Utc::now().to_rfc3339();
        let v = parse_and_validate(&dt, &AnswerType::Date, None).unwrap();
        // It round-trips as a string
        assert_eq!(v, JsonValue::String(dt));
        // invalid
        assert!(parse_and_validate("not a date", &AnswerType::Date, None).is_err());
    }

    #[test]
    fn parse_and_validate_choice() {
        let opts = vec!["Yes".into(), "No".into()];
        let at = AnswerType::Choice { options: opts.clone() };
        // exact
        assert_eq!(
            parse_and_validate("Yes", &at, None).unwrap(),
            JsonValue::String("Yes".into())
        );
        // case-insensitive
        assert_eq!(
            parse_and_validate("no", &at, None).unwrap(),
            JsonValue::String("No".into())
        );
        // fail
        let err = parse_and_validate("Maybe", &at, None).unwrap_err();
        assert!(err.contains("please choose one of"));
    }

    #[test]
    fn condition_matches_basic() {
        let mut answers = HashMap::new();
        answers.insert("a".to_string(), json!(42));
        answers.insert("b".to_string(), json!("foo"));


        // Equals
        let eq = Condition::Equals {
            question_id: "b".into(),
            value: json!("foo"),
        };
        assert!(eq.matches(&answers));
        assert!(!eq.matches(&HashMap::new()));

        // GreaterThan
        let gt = Condition::GreaterThan { question_id: "a".into(), threshold: 10. };
        assert!(gt.matches(&answers));
        let gt2 = Condition::GreaterThan { question_id: "a".into(), threshold: 100. };
        assert!(!gt2.matches(&answers));

        // LessThan
        let lt = Condition::LessThan { question_id: "a".into(), threshold: 100. };
        assert!(lt.matches(&answers));
        let lt2 = Condition::LessThan { question_id: "a".into(), threshold: 10. };
        assert!(!lt2.matches(&answers));
    }

    #[test]
    fn routing_rule_uses_condition() {
        let mut answers = HashMap::new();
        answers.insert("score".into(), json!(75));

        // route to "pass" if score >= 50, else to "fail"
        let rule_pass = RoutingRule {
            condition: Some(Condition::GreaterThan { question_id: "score".into(), threshold: 50. }),
            to: "pass".into(),
        };
        let rule_fail = RoutingRule {
            condition: Some(Condition::LessThan { question_id: "score".into(), threshold: 50. }),
            to: "fail".into(),
        };
        assert!(rule_pass.matches(&answers));
        assert!(!rule_fail.matches(&answers));
    }



    const QA_YAML: &str = r#"
welcome_template: "Welcome!"
questions:
  - id: "age"
    prompt: "👉 How old are you?"
    answer_type: number
    state_key: "user_age"
    validate:
      range:
        min: 0.0
        max: 120.0

  - id: "name"
    prompt: "👉 What is your name?"
    answer_type: text
    state_key: "user_name"

routing:
  - condition:
        less_than:
            question_id: "age"
            threshold: 18.0
    to: "minor_flow"

  - to: "adult_flow"
"#;

    #[test]
    fn qa_process_config_manual_vs_yaml_value() {
        // 1) Manually construct exactly the same struct
        let cfg_manual = QAProcessConfig {
            welcome_template: "Welcome!".into(),
            questions: vec![
                QuestionConfig {
                    id: "age".into(),
                    prompt: "👉 How old are you?".into(),
                    answer_type: AnswerType::Number,
                    state_key: "user_age".into(),
                    validate: Some(ValidationRule::Range {range: RangeParams{ min: 0.0, max: 120.0 }}),
                },
                QuestionConfig {
                    id: "name".into(),
                    prompt: "👉 What is your name?".into(),
                    answer_type: AnswerType::Text,
                    state_key: "user_name".into(),
                    validate: None,
                },
            ],
            fallback_agent: None,
            routing: vec![
                RoutingRule {
                    condition: Some(Condition::LessThan {
                        question_id: "age".into(),
                        threshold: 18.0,
                    }),
                    to: "minor_flow".into(),
                },
                RoutingRule {
                    condition: None,
                    to: "adult_flow".into(),
                },
            ],
        };

        // 2) Serialize your manual struct → YamlValue
        let val_manual: YamlValue =
            serde_yaml::to_value(&cfg_manual).expect("to_value");

        // 3) Parse the literal YAML → YamlValue
        let val_literal: YamlValue =
            serde_yaml::from_str(QA_YAML).expect("from_str");

        // 4) Compare and, if they differ, print them out in full
        if val_manual != val_literal {
            eprintln!("--- MANUAL YamlValue:\n{:#?}", val_manual);
            eprintln!("--- LITERAL YamlValue:\n{:#?}", val_literal);
        }
        assert_eq!(val_manual, val_literal);
    }

#[derive(serde::Deserialize)]
struct QaWrapper {
    qa: QAProcessConfig,
}

    #[test]
    fn qa_process_config_manual_vs_yaml() {  
        let yaml = r#"
qa:
  welcome_template: "Welcome!"
  questions:
    - id: "age"
      prompt: "👉 How old are you?"
      answer_type: number
      state_key: "user_age"
      validate:
        range:
          min: 0.0
          max: 120.0

    - id: "name"
      prompt: "👉 What is your name?"
      answer_type: text
      state_key: "user_name"

  routing:
    - condition:
        less_than:
          question_id: "age"
          threshold: 18.0
      to: "minor_flow"

    - to: "adult_flow"
"#;
        // build the same config by hand
       let manual = 
        BuiltInProcess::Qa(
            QAProcessNode {
                config: QAProcessConfig {
                    welcome_template: "Welcome!".into(),
                    questions: vec![
                        QuestionConfig {
                            id: "age".into(),
                            prompt: "👉 How old are you?".into(),
                            answer_type: AnswerType::Number,
                            state_key: "user_age".into(),
                            validate: Some(ValidationRule::Range {
                                range: RangeParams { min: 0.0, max: 120.0 },
                            }),
                        },
                        QuestionConfig {
                            id: "name".into(),
                            prompt: "👉 What is your name?".into(),
                            answer_type: AnswerType::Text,
                            state_key: "user_name".into(),
                            validate: None,
                        },
                    ],
                    fallback_agent: None,
                    routing: vec![
                        RoutingRule {
                            condition: Some(Condition::LessThan {
                                question_id: "age".into(),
                                threshold: 18.0,
                            }),
                            to: "minor_flow".into(),
                        },
                        RoutingRule {
                            condition: None,
                            to: "adult_flow".into(),
                        },
                    ],
                },
            }
        );

        // parse YAML
        let w: QaWrapper =
            serde_yaml::from_str(yaml).expect("valid QA yaml");
        let from_yaml = BuiltInProcess::Qa(QAProcessNode { config: w.qa });

        assert_eq!(manual, from_yaml);
    }

}

