# Built-in Processes in Greentic

This guide explains the built-in process nodes available in Greentic and how to use them in your flows.

---

## 🛠️ Available Built-in Processes

```rust
pub enum BuiltInProcess {
    Debug(DebugProcessNode),
    Script(ScriptProcessNode),
    Template(TemplateProcessNode),
    #[serde(rename = "qa")]
    Qa(QAProcessNode),
    Plugin { name: String, #[serde(skip)] path: PathBuf },
}
```

---

## 🐞 `debug`

A debugging process that lets you inspect message input/state.

### Example
```yaml
debug_process:
  debug:
    print: false  # logs only (no CLI output)
```
- `print: true` → logs + prints to CLI

---

## 📜 `script`

A Rhai scripting process that can transform messages, payloads, and state.

### Supported variables:
- `msg`: full message object
- `payload`: parsed input
- `state`: state context (keys are exposed directly too)

### Output: value is wrapped as `{ "output": ... }`

### Structured Output with Routing (via `__greentic`):
```rhai
return #{
    __greentic: {
        payload: #{ status: "ok" },
        out: ["next_node"]
    }
};
```

### See full documentation in the Rhai section of this file.

---

## 🧩 `template`

A Handlebars-based template processor for rendering string output.

### Supported context:
- `msg`, `payload`, `state`

### Example:
```handlebars
Hello {{state.user_name}}, today is {{payload.date}}.
```

- Control flow (`if`, `each`) supported
- See Handlebars docs for more details

---

## ❓ `qa`

A dynamic, multi-question form-like process with optional validation and routing.

### Supported answer types:
- `text` (with optional `max_words`, `regex`)
- `number` (with optional `range` validation)
- `choice` (predefined options)
- `date` (with `dialect`, `max_words`)
- `llm` (open-ended, always interpreted by LLM)

### Routing based on answers:
```yaml
routing:
  - condition:
      less_than:
        question_id: "age"
        threshold: 18
    to: "minor_flow"

  - to: "adult_flow"
```

### LLM Fallbacks:
Triggered when answers don’t meet constraints (e.g., too many words, invalid number).
```yaml
fallback_agent:
  task: "Reformat answer into a number."
```

### Example Node
```yaml
ask_user:
  qa:
    welcome_template: "Hi! Let's get started."
    questions:
      - id: "name"
        prompt: "What’s your name?"
        answer_type: text
        state_key: "user_name"

      - id: "age"
        prompt: "How old are you?"
        answer_type: number
        state_key: "user_age"
        validate:
          range:
            min: 0
            max: 120

    routing:
      - condition:
          less_than:
            question_id: "age"
            threshold: 18
        to: "underage"
      - to: "main_process"
```

Answers are stored in state and passed on as a unified object.

---

## 🧩 `plugin`

A dynamically loaded process node compiled as `.wasm`, located at `/plugins/<name>.wasm`.

```yaml
my_plugin_step:
  plugin: "my_plugin"
```

Used for extending the platform with custom functionality.

