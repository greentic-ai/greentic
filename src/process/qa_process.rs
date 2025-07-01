use std::{collections::HashMap, fmt};

use async_trait::async_trait;
use channel_plugin::message::MessageContent;
use chrono::{DateTime, Utc};
use chrono_english::parse_date_string;
use handlebars::Handlebars;
use regex::Regex;
use schemars::{Schema, schema_for, JsonSchema};
use serde_json::{json,  Value as JsonValue};
use serde::{
    de::{self, MapAccess, Visitor},
    ser::SerializeMap,
    Deserialize, Deserializer, Serialize, Serializer,
};

use crate::{agent::manager::BuiltInAgent, flow::state::StateValue, message::Message, node::{NodeContext, NodeErr, NodeError, NodeOut, NodeType, Routing}, util::render_handlebars};
/// A QA process allows asking users a series of questions and storing their answers.
/// It supports different types of questions like text, number, choice, date, and LLM (AI-powered).
/// Each answer type can define validation rules (like number range or regex), and some types can specify constraints (like `max_words`).
/// If an answer is missing or doesn’t match the expectations, the fallback mechanism can route the input to an LLM to interpret or correct it.
/// Based on the collected answers, different follow-up paths (routing) can be taken.
///
/// ### Example: Ask a user's name and age
/// ```yaml
/// welcome_template: "Hi! Let's get started."
/// questions:
///   - id: "name"
///     prompt: "What's your name?"
///     answer_type: text
///     state_key: "user_name"
///
///   - id: "age"
///     prompt: "How old are you?"
///     answer_type: number
///     state_key: "user_age"
///     validate:
///       range:
///         min: 0
///         max: 120
///
/// routing:
///   - condition:
///       less_than:
///         question_id: "age"
///         threshold: 18
///     to: "minor_flow"
///   - to: "adult_flow"
/// ```
///
/// ### Answer types:
///
/// **Text:** Free-form input like a name, place, or sentence.
/// ```yaml
/// answer_type: text
/// ```
/// Optional constraints:
/// ```yaml
/// answer_type:
///   text:
///     max_words: 3       # If longer, LLM fallback is used
///     regex: "^[A-Za-z]+$"  # If fails, LLM fallback is used
/// ```
/// **Fallback for text:** `max_words` or `regex` don't reject the answer outright. Instead, if they don't match, the fallback LLM can attempt to extract or clean the correct response.
///
/// **Number:** Numerical input like age, count, or temperature.
/// ```yaml
/// answer_type: number
/// ```
/// Optional validation:
/// ```yaml
/// validate:
///   range:
///     min: 0
///     max: 100
/// ```
/// **Fallback for number:** If not a valid number, or out of the `range`, fallback LLM is triggered to correct or interpret it.
///
/// **Choice:** User must select from a predefined list.
/// ```yaml
/// answer_type:
///   choice:
///     options: ["Yes", "No", "Maybe"]
///     max_words: 1
/// ```
/// - Matching is case-insensitive.
/// - If the answer doesn’t match any option or exceeds `max_words`, fallback LLM is used.
///
/// **Date:** Natural date input like "next Friday" or "1st June".
/// ```yaml
/// answer_type:
///   date:
///     dialect: uk  # or "us" for MM/DD/YYYY format
///     max_words: 5
/// ```
/// - If parsing fails or `max_words` is exceeded, fallback LLM is used to extract a valid date.
///
/// **LLM:** Always sends the answer directly to the LLM.
/// Useful for open-ended questions or when user intent needs interpretation.
/// ```yaml
/// answer_type: llm
/// ```
/// Example with LLM task:
/// ```yaml
/// prompt: "What do you want to achieve this week?"
/// answer_type: llm
/// state_key: "user_goal"
/// fallback_agent:
///   task: "Extract the user's main goal from their message."
/// ```
/// The `task` should be written in natural language to instruct the LLM clearly.
///
/// ### Fallback mechanism
/// For **text**, **number**, **choice**, and **date**, the fallback is triggered *only* when validation or constraints fail (like regex mismatch, out-of-range number, unmatched choice, etc.).
/// 
/// The fallback agent does **not** reject the answer. Instead, it attempts to:
/// - Clean or extract the intended meaning
/// - Reformat the user’s response to match expected format
/// - Or prompt the user to clarify
///
/// Example fallback:
/// ```yaml
/// fallback_agent:
///   task: "Please rephrase the user's answer so it matches the expected format."
/// ```
///
/// For **LLM** type questions, the answer *always* goes to the fallback agent (LLM), regardless of constraints.
///
/// ### Routing:
/// Use previous answers to decide what comes next.
/// ```yaml
/// routing:
///   - condition:
///       less_than:
///         question_id: "age"
///         threshold: 18
///     to: "minor_flow"
///
///   - to: "adult_flow"
/// ```
///
/// This makes it easy to guide users through different paths based on their responses.
///
/// ### Summary:
/// - `answer_type` defines the kind of expected input.
/// - Constraints like `max_words` or `regex` don’t reject answers—they trigger the fallback LLM to help.
/// - `validate` rules apply additional checks (especially for numbers).
/// - Fallback agents enhance robustness by letting the LLM assist when user input is unclear.
/// - Routing makes flows dynamic and context-aware.
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
///           answer_type: text
///           state_key: "user_name"
///
///         - id:       "age"
///           prompt:   "👉 How old are you?"
///           answer_type: number
///           state_key: "user_age"
///           validate:
///             range:
///               min: 0
///               max: 120
///
///         - id:       "birthdate"
///           prompt:   "👉 When is your birthday? (YYYY-MM-DD)"
///           answer_type: date
///           state_key: "user_birthdate"
///           validate:
///             regex: "^\\d{4}-\\d{2}-\\d{2}$"
///
///         - id:       "color"
///           prompt:   "👉 Pick a color: red, green or blue."
///           answer_type:
///             choice:
///               options: ["red","green","blue"]
///           state_key: "favorite_color"
///
///       # if they are under 18, send to "underage" flow; else to "main_process"
///       routing:
///         - condition:
///             less_than:
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
    fn schema(&self) -> Schema { schema_for!(QAProcessConfig) }

    async fn process(&self, msg: Message, ctx: &mut NodeContext)
      -> Result<NodeOut, NodeErr>
    {
        let session = msg.session_id();

         // read (or default) our little counter
        let idx_val = ctx
            .get_state("qa.current_question")
            .unwrap_or(StateValue::Number(0.0));
        let idx = idx_val.as_number().unwrap() as usize;

        // 1) first‐time visitor?
        let mut first_time = false;
        if ctx.get_state("qa.current_question").is_none() || idx > self.config.questions.len() {
            for q in &self.config.questions {
                ctx.delete_state(&q.state_key);
            }
            ctx.set_state("qa.current_question", StateValue::Number(1.));
            first_time = true;
        }


        // 🧠 If this is *not* the first message, we should handle the answer from last_q
        if !first_time && idx > 0 && idx <= self.config.questions.len() {
            let last_q = &self.config.questions[idx - 1];
            let payload_val = msg.payload();
            let text = extract_raw_text(&payload_val);
            match parse_and_validate(&text, &last_q.answer_type, last_q.validate.clone()) {
                Ok(parsed_json) => {
                    let state_val = StateValue::try_from(parsed_json.clone())
                        .map_err(|e| NodeErr::fail(NodeError::ExecutionFailed(
                            format!("Failed to convert answer to StateValue: {:?}", e)
                        )))?;
                    ctx.set_state(&last_q.state_key, state_val);
                }
                Err(e) if e == "fallback_trigger" => {
                    if let Some(agent) = &self.config.fallback_agent {
                        // Prepare new message for the fallback agent
                        let input_msg = Message::new(
                            &msg.id(),
                            msg.payload().clone(), // or wrap it in `{ "input": ... }` if needed
                            msg.session_id(),
                        );
                        // Call the agent node
                        let output = agent.process(input_msg, ctx).await.map_err(|e| 
                            NodeErr::fail(NodeError::ExecutionFailed(format!("Fallback agent error: {:?}", e)))
                        )?;

                        // Extract payload from NodeOut (expecting single reply)
                        let interpreted = output.message().payload();

                        // Convert and store
                        let state_val = StateValue::try_from(interpreted.clone()).map_err(|e| 
                            NodeErr::fail(NodeError::ExecutionFailed(
                                format!("Fallback value conversion failed: {:?}", e)
                            ))
                        )?;
                        ctx.set_state(&last_q.state_key, state_val);
                        ctx.set_state("qa.current_question", StateValue::Number((idx + 1) as f64));

                    } else {
                        let prompt = render_handlebars(&last_q.prompt, &ctx.get_all_state());
                        let out = Message::new(
                            &msg.id(),
                            json!({ "text": format!("I couldn’t understand. Please try again.\n{}", prompt) }),
                            session.to_string(),
                        );
                        return Ok(NodeOut::with_routing(out, Routing::FollowGraph));
                    }
                }
                Err(err) => {
                    // re‐ask the same question
                    let prompt = render_handlebars(&last_q.prompt, &ctx.get_all_state());
                    let out = Message::new(
                        &msg.id(),
                        json!({ "text": format!("I didn’t understand: {}\n{}", err, prompt) }),
                        session.to_string(),
                    );
                    return Ok(NodeOut::with_routing(out, Routing::FollowGraph));
                }
                
            }
        }


        // 🔁 Ask next question (if any)
        
        if idx < self.config.questions.len() {
            // prompt
            
            let qcfg = self.config.questions.get(idx).unwrap();
            let welcome = render_handlebars(&self.config.welcome_template, &ctx.get_all_state());
            let prompt = render_handlebars(&qcfg.prompt, &ctx.get_all_state());
            let msg_text = match first_time {
                true => format!("{}\n{}",welcome,prompt),
                false => prompt,
            };
            let out = Message::new(
                &msg.id(),
                json!({ "text": msg_text }),
                session.to_string(),
            );
            let next = (idx + 1) as f64;
            ctx.set_state("qa.current_question", StateValue::Number(next));

            return Ok(NodeOut::reply(out));
        }


        // 4) All done! run routing rules against ctx.state
        let answers = ctx.get_all_state();

        let mut json_answers = serde_json::Map::new();
        for (k, v) in &answers {
            json_answers.insert(k.clone(), v.to_json());
        }

        for rule in &self.config.routing {
            if rule.matches(&json_answers) {
                let payload = JsonValue::Object(json_answers.clone());

                let out_msg = Message::new(
                    &msg.id(),
                    payload,
                    msg.session_id(),
                );
                // reset
                ctx.delete_state("qa.current_question");

                return Ok(NodeOut::with_routing(out_msg, Routing::ToNode(rule.to.clone())));
            }
        }

        // reset
        ctx.delete_state("qa.current_question");
        Err(NodeErr::fail(NodeError::ExecutionFailed(
            "no routing rule matched".into())))
    }

    fn clone_box(&self) -> Box<dyn NodeType> {
        Box::new(self.clone())
    }
}


fn extract_raw_text(value: &serde_json::Value) -> String {
    // 1. Try to parse as one or more MessageContent
    if let Ok(mc) = serde_json::from_value::<MessageContent>(value.clone()) {
        if let MessageContent::Text{text} = mc {
            return text;
        }
    }
    if let Ok(vec) = serde_json::from_value::<Vec<MessageContent>>(value.clone()) {
        for mc in vec {
            if let MessageContent::Text{text} = mc {
                return text;
            }
        }
    }


    // 2. Check value["text"]
    if let Some(text_val) = value.get("text").and_then(|v| v.as_str()) {
        return text_val.to_string();
    }

    // 3. Check value["content"]["Text"]
    if let Some(text_val) = value.get("content")
                                 .and_then(|c| c.get("Text"))
                                 .and_then(|v| v.as_str()) {
        return text_val.to_string();
    }

    // 4. Fallback: if it's a JSON string, extract it
    if let Some(s) = value.as_str() {
        return s.to_string();  // <- clean "Alice"
    }

    // 5. Fallback: stringify the full JSON
    value.to_string()
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
    #[serde(flatten)]
    pub answer_type: AnswerType,

    /// Where in `NodeContext.state` to store the parsed value.
    pub state_key: String,

    /// Optional regexp or range check to validate their answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validate: Option<ValidationRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "answer_type", rename_all = "lowercase")]
pub enum AnswerType {
    Text {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_words: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        regex: Option<String>,
    },
    Number {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_words: Option<usize>,
    },
    Date {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_words: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dialect: Option<Dialect> 
    },
    Choice {
        options: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_words: Option<usize>,
    },
    #[serde(rename = "llm")]
    Llm,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub enum Dialect {
    Uk,
    Us,
}
impl Dialect {
    pub fn to_chrono(&self) -> chrono_english::Dialect {
        match self {
            Dialect::Uk => chrono_english::Dialect::Uk,
            Dialect::Us => chrono_english::Dialect::Us,
        }
    }
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
    pub fn matches(&self, answers: &serde_json::Map<String, JsonValue>) -> bool {
        match &self.condition {
            None => true,
            Some(cond) => cond.matches(answers),
        }
    }
}

impl Condition {
    pub fn matches(&self, answers: &serde_json::Map<String, JsonValue>) -> bool {
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
    validate: Option<ValidationRule>,
) -> Result<JsonValue, String> {
    let word_count = raw.split_whitespace().count();
    match answer_type {
        AnswerType::Text { max_words, regex } => {
            // Too many words will trigger the LLM
            if let Some(limit) = max_words {
                if word_count > *limit {
                    return Err("fallback_trigger".to_string());
                }
            }
            // If the answer does not match the regex, fallback to LLM
            if let Some(re) = regex {
                let regex = Regex::new(re)
                    .map_err(|e| format!("internal regex error: {}", e))?;
                if !regex.is_match(raw) {
                    return Err("fallback_trigger".into());
                }
            }
            Ok(JsonValue::String(raw.to_owned()))
        }

        AnswerType::Number { max_words } => {
            if let Some(limit) = max_words {
                if word_count > *limit {
                    return Err("fallback_trigger".into());
                }
            }
            let v: f64 = raw.trim().parse().map_err(|_| "please enter a number".to_string())?;
            // range check if given
            if let Some(ValidationRule::Range { range: RangeParams{min, max }}) = validate {
                if v < min || v > max {
                    return Err(format!("must be between {} and {}", min, max));
                }
            }
            // use serde_json::json! to produce a Number
            Ok(json!(v))
        }

        AnswerType::Date { max_words, dialect } => {
            if let Some(limit) = max_words {
                if word_count > *limit {
                    return Err("fallback_trigger".into());
                }
            }
            let chrono_dialect = dialect
                .as_ref()
                .map(|d| d.to_chrono())
                .unwrap_or(chrono_english::Dialect::Uk);

            let dt: DateTime<Utc> = parse_date_string(raw, Utc::now(), chrono_dialect)
                .map_err(|_| {
                    "sorry, I couldn’t understand the date. Try something like 'tomorrow', 'Monday at 9am', or 'June 17'".to_string()
                })?;
            Ok(JsonValue::String(dt.to_rfc3339()))
        }

        AnswerType::Llm => Err("fallback_trigger".into()),

        AnswerType::Choice { options, max_words } => {
            if let Some(limit) = max_words {
                if word_count > *limit {
                    return Err("fallback_trigger".into());
                }
            }
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
             // ❌ If no match, return user-friendly error instead of fallback_trigger
            let choices = options.join(", ");
            Err(format!("please choose one of: {}", choices))
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
                let (key, value): (String, serde_yaml_bw::Value) = map
                    .next_entry()?
                    .ok_or_else(|| de::Error::custom("Expected one entry in Condition map"))?;
                match key.as_ref() {
                    "equals" => {
                        // expect a map mapping question_id→…, value→…
                        let m: HashMap<String, serde_json::Value> =
                            serde_yaml_bw::from_value(value).map_err(de::Error::custom)?;
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
                            serde_yaml_bw::from_value(value).map_err(de::Error::custom)?;
                        let expr = m.get("expr")
                            .cloned()
                            .ok_or_else(|| de::Error::custom("Custom.expr missing"))?;
                        Ok(Condition::Custom { expr })
                    }
                    "greater_than" => {
                        let m: HashMap<String, serde_json::Value> =
                            serde_yaml_bw::from_value(value).map_err(de::Error::custom)?;
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
                            serde_yaml_bw::from_value(value).map_err(de::Error::custom)?;
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
    use crate::{agent::ollama::OllamaAgent, channel::{manager::{ChannelManager, ManagedChannel}, PluginWrapper}, config::{ConfigManager, MapConfigManager}, executor::Executor, flow::{manager::{ChannelNodeConfig, Flow, NodeConfig, NodeKind}, session::InMemorySessionStore, state::InMemoryState}, logger::{LogConfig, Logger, OpenTelemetryLogger}, node::ChannelOrigin, process::{debug_process::DebugProcessNode, manager::{BuiltInProcess, ProcessManager}}, secret::{EmptySecretsManager, EnvSecretsManager, SecretsManager}};

    use super::*;
    use channel_plugin::{message::{LogLevel, Participant}, plugin_actor::{spawn_rpc_plugin,}};
    use dashmap::DashMap;
    use serde_json::json;
    use chrono::Utc;
    use std::{path::Path, sync::Arc};
    use serde_yaml_bw::Value as YamlValue;

    #[test]
    fn parse_and_validate_llm() {
        let at = AnswerType::Llm;

        let result = parse_and_validate("anything at all", &at, None);
        assert_eq!(result.unwrap_err(), "fallback_trigger".to_string());

        let result = parse_and_validate("", &at, None);
        assert_eq!(result.unwrap_err(), "fallback_trigger".to_string());
    }

    #[tokio::test]
    async fn qa_node_captures_answers_and_routes_correctly() {
        let config = QAProcessConfig {
            welcome_template: "Hi!".into(),
            questions: vec![
                QuestionConfig {
                    id: "q1".into(),
                    prompt: "What's your name?".into(),
                    answer_type: AnswerType::Text {max_words: None, regex: None},
                    state_key: "name".into(),
                    validate: None,
                },
                QuestionConfig {
                    id: "q2".into(),
                    prompt: "How old are you?".into(),
                    answer_type: AnswerType::Number{max_words: None,},
                    state_key: "age".into(),
                    validate: Some(ValidationRule::Range { range: RangeParams { min: 0.0, max: 130.0 }}),
                },
            ],
            fallback_agent: None,
            routing: vec![
                RoutingRule {
                    condition: Some(Condition::LessThan { question_id: "age".into(), threshold: 18.0 }),
                    to: "underage_node".into(),
                },
                RoutingRule {
                    condition: None,
                    to: "default_node".into(),
                },
            ],
        };

        let qa_node = QAProcessNode { config };
        let state = InMemoryState::new();
        let ctx_config = DashMap::new();
        let secrets = SecretsManager(EmptySecretsManager::new());
        let logger = Logger(Box::new(OpenTelemetryLogger::new()));
        let executor = Executor::new(secrets.clone(), logger.clone());
        let config_manager = ConfigManager(MapConfigManager::new());
        let store = InMemorySessionStore::new(10);
        let channel_origin = ChannelOrigin::new("test_channel".into(), None, None, Participant::new("user".into(), None, None));
        let process_manager = ProcessManager::new(Path::new("./plugins/processes")).unwrap();
        let channel_manager = ChannelManager::new(config_manager, secrets.clone(), store.clone(), LogConfig::default()).await.unwrap();

        let mut ctx = NodeContext::new(
            "sess1".into(),
            state,
            ctx_config,
            executor,
            channel_manager,
            Arc::new(process_manager),
            secrets,
            Some(channel_origin),
        );

        // First interaction — welcome + q1
        let incoming1 = Message::new_uuid("test", json!({}));
        let result1 = qa_node.process(incoming1.clone(), &mut ctx).await.unwrap();
        let msg1 = result1.message();
        assert!(msg1.payload()["text"].as_str().unwrap().contains("What's your name?"));

        // Answer to q1
        let incoming2 = Message::new_uuid("test", json!("Alice"));
        let result2 = qa_node.process(incoming2.clone(), &mut ctx).await.unwrap();
        let msg2 = result2.message();
        assert!(msg2.payload()["text"].as_str().unwrap().contains("How old are you?"));

        // Answer to q2
        let incoming3 = Message::new_uuid("test", json!(25));
        let result3 = qa_node.process(incoming3.clone(), &mut ctx).await.unwrap();
        let route3 = result3.routing();
        assert_eq!(route3.to_node().unwrap(), "default_node");

        // Check state
        let all_vec = ctx.get_all_state();
        let all: HashMap<_, _> = all_vec.into_iter().collect();

        let name = all.get("name").expect("expected name").as_string();
        assert_eq!(name, Some("Alice".to_string()));
        assert_eq!(all.get("age").and_then(|v| v.as_number()), Some(25.0));
    }

    #[test]
    fn parse_and_validate_text_no_regex() {
        let answer_type = AnswerType::Text {
            regex: None,
            max_words: None,
        };
        // Valid non-empty string
        let val = "hello";
        let v = parse_and_validate(val, &answer_type, None).unwrap();
        assert_eq!(v, JsonValue::String(val.into()));

        // Valid empty string (if allowed)
        let empty = parse_and_validate("", &answer_type, None).unwrap();
        assert_eq!(empty, JsonValue::String("".into()));

        // Invalid if regex provided (should still be covered in separate test)
    }

    #[test]
    fn parse_and_validate_text_with_regex_and_max_words() {
        // ✅ regex: only digits
        let answer_type = AnswerType::Text {
            regex: Some(r"^\d+$".into()),
            max_words: None,
        };

        let vr = parse_and_validate("12345", &answer_type, None).unwrap();
        assert_eq!(vr, JsonValue::String("12345".into()));

        // ❌ regex fail: non-digit
        let err = parse_and_validate("abc", &answer_type, None).unwrap_err();
        assert_eq!(err, "fallback_trigger");

        // ✅ max_words OK
        let answer_type_max = AnswerType::Text {
            regex: None,
            max_words: Some(3),
        };
        let short = parse_and_validate("one two three", &answer_type_max, None).unwrap();
        assert_eq!(short, JsonValue::String("one two three".into()));

        // ❌ max_words exceeded
        let err = parse_and_validate("one two three four", &answer_type_max, None).unwrap_err();
        assert_eq!(err, "fallback_trigger");

        // ✅ regex + max_words combined
        let both = AnswerType::Text {
            regex: Some(r"^\w+( \w+)*$".into()), // allow words separated by single spaces
            max_words: Some(2),
        };
        let ok = parse_and_validate("hello world", &both, None).unwrap();
        assert_eq!(ok, JsonValue::String("hello world".into()));

        // ❌ regex + max_words: fails both
        let err = parse_and_validate("hello 123 !@#", &both, None).unwrap_err();
        assert_eq!(err, "fallback_trigger");
    }


    #[test]
    fn parse_and_validate_number_and_range() {
        // ✅ Basic valid number
        let number_type = AnswerType::Number {max_words: None,};
        let v = parse_and_validate("3.14", &number_type, None).unwrap();
        assert_eq!(v, json!(3.14));

        // ❌ Invalid number input
        let err = parse_and_validate("foo", &number_type, None).unwrap_err();
        assert_eq!(err, "please enter a number");

        // ✅ Number within range
        let number_type = AnswerType::Number {max_words: None,};
        let rule = ValidationRule::Range {
            range: RangeParams { min: 0.0, max: 10.0 },
        };
        let v = parse_and_validate("5", &number_type, Some(rule.clone())).unwrap();
        assert_eq!(v, json!(5.0));

        // ❌ Number below range
        let err = parse_and_validate("-1", &number_type, Some(rule.clone())).unwrap_err();
        assert_eq!(err, "must be between 0 and 10");

        // ❌ Number above range
        let err = parse_and_validate("42", &number_type, Some(rule)).unwrap_err();
        assert_eq!(err, "must be between 0 and 10");

        // ❌ Too many words should trigger fallback
        let number_type_limited = AnswerType::Number { max_words: Some(1) };
        let err = parse_and_validate("123 456", &number_type_limited, None).unwrap_err();
        assert_eq!(err, "fallback_trigger");
    }


    #[test]
    fn parse_and_validate_date() {
        // ✅ Valid ISO8601 date-time string
        let dt = Utc::now().to_rfc3339();
        let date_type = AnswerType::Date {max_words: None, dialect: Some(Dialect::Uk)};
        let v = parse_and_validate(&dt, &date_type, None).unwrap();
        assert_eq!(v, JsonValue::String(dt));

        // ✅ Natural language date (e.g. "tomorrow")
        let v2 = parse_and_validate("tomorrow", &date_type, None).unwrap();
        assert!(
            v2.as_str().unwrap().contains('T'),
            "Expected a valid RFC3339 date string from natural language"
        );

        // ❌ Invalid date format
        let invalid_input = "not a date";
        let err = parse_and_validate(invalid_input, &date_type, None).unwrap_err();
        assert_eq!(
            err,
            "sorry, I couldn’t understand the date. Try something like 'tomorrow', 'Monday at 9am', or 'June 17'".to_string(),
        );

        // ❌ Incorrect but realistic-looking input
        let badly_formatted = "12-25-2025";
        let err = parse_and_validate(badly_formatted, &date_type, None).unwrap_err();
        assert_eq!(
            err,
            "sorry, I couldn’t understand the date. Try something like 'tomorrow', 'Monday at 9am', or 'June 17'".to_string(),
        );
    }


    #[test]
    fn parse_and_validate_choice() {
        let opts = vec!["Yes".into(), "No".into()];
        let at = AnswerType::Choice { max_words:Some(1), options: opts.clone() };

        // ✅ Exact match
        assert_eq!(
            parse_and_validate("Yes", &at, None).unwrap(),
            JsonValue::String("Yes".into())
        );

        // ✅ Case-insensitive match
        assert_eq!(
            parse_and_validate("no", &at, None).unwrap(),
            JsonValue::String("No".into())
        );

        // ❌ Invalid option
        let err = parse_and_validate("Maybe", &at, None).unwrap_err();
        assert!(
            err.contains("please choose one of"),
            "Unexpected error: {}",
            err
        );

        // ❌ Empty string input
        let err = parse_and_validate("", &at, None).unwrap_err();
        assert!(
            err.contains("please choose one of"),
            "Unexpected error: {}",
            err
        );

        // ❌ Too many words
        let err = parse_and_validate("yes definitely", &at, None).unwrap_err();
        assert_eq!(err, "fallback_trigger");
    }


    #[test]
    fn condition_matches_basic() {
        let mut answers = serde_json::Map::new();
        answers.insert("a".to_string(), json!(42));
        answers.insert("b".to_string(), json!("foo"));


        // Equals
        let eq = Condition::Equals {
            question_id: "b".into(),
            value: json!("foo"),
        };
        assert!(eq.matches(&answers));
        assert!(!eq.matches(&serde_json::Map::new()));

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
        let mut answers = serde_json::Map::new();
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
        // 1) Manually construct exactly the same config
        let cfg_manual = QAProcessConfig {
            welcome_template: "Welcome!".into(),
            questions: vec![
                QuestionConfig {
                    id: "age".into(),
                    prompt: "👉 How old are you?".into(),
                    answer_type: AnswerType::Number {max_words: None},
                    state_key: "user_age".into(),
                    validate: Some(ValidationRule::Range {
                        range: RangeParams { min: 0.0, max: 120.0 },
                    }),
                },
                QuestionConfig {
                    id: "name".into(),
                    prompt: "👉 What is your name?".into(),
                    answer_type: AnswerType::Text {
                        max_words: None,
                        regex: None,
                    },
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

        // 2) Serialize manual struct → YamlValue
        let val_manual: YamlValue = serde_yaml_bw::to_value(&cfg_manual).expect("manual to_value failed");

        // 3) Parse literal YAML → YamlValue
        let val_literal: YamlValue = serde_yaml_bw::from_str(QA_YAML).expect("literal from_str failed");

        // 4) Show full diff if mismatch
        if val_manual != val_literal {
            eprintln!("❌ YAML values differ:");
            eprintln!("--- Manual YamlValue:\n{:#?}", val_manual);
            eprintln!("--- Literal YamlValue:\n{:#?}", val_literal);
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

        let manual = BuiltInProcess::Qa(QAProcessNode {
            config: QAProcessConfig {
                welcome_template: "Welcome!".into(),
                questions: vec![
                    QuestionConfig {
                        id: "age".into(),
                        prompt: "👉 How old are you?".into(),
                        answer_type: AnswerType::Number {max_words:None},
                        state_key: "user_age".into(),
                        validate: Some(ValidationRule::Range {
                            range: RangeParams { min: 0.0, max: 120.0 },
                        }),
                    },
                    QuestionConfig {
                        id: "name".into(),
                        prompt: "👉 What is your name?".into(),
                        answer_type: AnswerType::Text {
                            max_words: None,
                            regex: None,
                        },
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
        });

        // Deserialize YAML into QA config
        let wrapper: QaWrapper = serde_yaml_bw::from_str(yaml).expect("invalid QA yaml");
        let from_yaml = BuiltInProcess::Qa(QAProcessNode { config: wrapper.qa });

        if manual != from_yaml {
            eprintln!("❌ Mismatch between manual and parsed QA config");
            eprintln!("--- Manual:\n{:#?}", manual);
            eprintln!("--- Parsed:\n{:#?}", from_yaml);
        }

        assert_eq!(manual, from_yaml);
    }


   #[tokio::test]
    async fn test_qa_via_mock_yaml() {
    let yaml = r#"
id: test-qa-mock
title: Test QA
description: Test QA via Mock
channels:
  - mock_inout
nodes:
  mock_in:
    channel: mock_inout
    in: true
    max_retries: 3
    retry_delay_secs: 1
  qa_ask:
    qa:
      welcome_template: Hi there! Let's get your forecast.
      questions:
        - id: q_location
          prompt: 👉 What location would you like a forecast for?
          answer_type: text
          state_key: q
        - id: q_days
          prompt: 👉 Over how many days? (enter a number)
          answer_type: number
          state_key: days
          validate:
            range:
              min: 0.0
              max: 7.0
      fallback_agent:
        task: task
      routing: []
    max_retries: 3
    retry_delay_secs: 1
  debug_node:
    debug:
      print: true
    max_retries: 3
    retry_delay_secs: 1
connections:
  mock_in:
    - qa_ask
  qa_ask:
    - debug_node
"#;

       

        let expected = Flow::new(
            "test-qa-mock".to_string(),
            "Test QA".to_string(),
            "Test QA via Mock".to_string(),
        )
        .add_channel("mock_inout")
        .add_node(
            "mock_in".to_string(),
            NodeConfig::new(
                "mock_in",
                NodeKind::Channel {
                    cfg: ChannelNodeConfig {
                        channel_name: "mock_inout".to_string(),
                        channel_in: true,
                        channel_out: false,
                        from: None,
                        to: None,
                        content: None,
                        thread_id: None,
                        reply_to_id: None,
                    },
                },
                None,
            ),
        )
        .add_node(
            "qa_ask".to_string(),
            NodeConfig::new(
                "qa_ask",
                NodeKind::Process {
                    process: BuiltInProcess::Qa(QAProcessNode {
                        config: QAProcessConfig {
                            welcome_template: "Hi there! Let's get your forecast.".into(),
                            questions: vec![
                                QuestionConfig {
                                    id: "q_location".into(),
                                    prompt: "👉 What location would you like a forecast for?".into(),
                                    answer_type: AnswerType::Text { max_words: None, regex: None },
                                    state_key: "q".into(),
                                    validate: None,
                                },
                                QuestionConfig {
                                    id: "q_days".into(),
                                    prompt: "👉 Over how many days? (enter a number)".into(),
                                    answer_type: AnswerType::Number {max_words: None},
                                    state_key: "days".into(),
                                    validate: Some(ValidationRule::Range {
                                        range: RangeParams {
                                            min: 0.0,
                                            max: 7.0,
                                        },
                                    }),
                                },
                            ],
                            fallback_agent: Some(BuiltInAgent::Ollama(OllamaAgent::new(
                                None, "task".into(), None, None, None, None, None, None,
                            ))),
                            routing: vec![],
                        },
                    }),
                },
                None,
            ),
        )
        .add_node(
            "debug_node".to_string(),
            NodeConfig::new(
                "debug_node",
                NodeKind::Process {
                    process: BuiltInProcess::Debug(DebugProcessNode { print: true }),
                },
                None,
            ),
        )
        .add_connection("mock_in".to_string(), vec!["qa_ask".to_string()])
        .add_connection("qa_ask".to_string(), vec!["debug_node".to_string()])
        .build();
        //println!("{}",serde_yaml_bw::to_string(&expected).expect("could not generate yaml"));

        let parsed_flow: Flow = serde_yaml_bw::from_str(yaml).expect("invalid flow YAML");
        let built_flow = parsed_flow.clone().build();
        assert_eq!(built_flow, expected);

        // Setup runtime context
        let store = InMemorySessionStore::new(10);
        let logger = Logger(Box::new(OpenTelemetryLogger::new()));
        let secrets = SecretsManager(EnvSecretsManager::new(Some(Path::new("./greentic/secrets/").to_path_buf())));
        let executor = Executor::new(secrets.clone(), logger.clone());
        let state = InMemoryState::new();
        let config = DashMap::<String, String>::new();
        let config_manager = ConfigManager(MapConfigManager::new());
        let process_manager = ProcessManager::new(Path::new("./greentic/plugins/processes/").to_path_buf()).unwrap();
        let channel_origin = ChannelOrigin::new(
            "mock".to_string(),
            None,
            None,
            Participant::new("id".to_string(), None, None),
        );
        let channel_manager = ChannelManager::new(config_manager, secrets.clone(), store.clone(), LogConfig::default())
            .await
            .expect("could not make channel manager");

        let path = Path::new("./greentic/plugins/channels/stopped/channel_mock_inout");

        let plugin = spawn_rpc_plugin(path).await.expect("Could not load plugin");

        let log_config = LogConfig::new(LogLevel::Info, Some("./greentic/logs".to_string()), None);
        
        let mock = ManagedChannel::new(PluginWrapper::new(plugin.channel_client(),plugin.control_client(), store.clone(), log_config).await, None, None);
        channel_manager
            .register_channel("mock_inout".to_string(), mock)
            .await
            .expect("could not register channel");

        let mut ctx = NodeContext::new(
            "123".to_string(),
            state,
            config,
            executor,
            channel_manager,
            Arc::new(process_manager),
            secrets,
            Some(channel_origin),
        );

        let payload = json!({"q": "London", "days": 5});
        let incoming = Message::new_uuid("test", payload);
        let report = built_flow.run(incoming, "mock_in", &mut ctx).await;

        assert_eq!(report.records.len(), 2);
        assert_eq!(report.records[0].node_id, "mock_in");
        assert_eq!(report.records[1].node_id, "qa_ask");

        assert!(report.error.is_some());
        assert!(format!("{:?}", report.error).contains("channel `mock` not loaded"));
    }

}

