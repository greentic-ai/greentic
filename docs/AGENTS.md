# Using Ollama Agents in Greentic

This guide explains how to use the built-in `OllamaAgent` node type to interact with local Ollama models in your Greentic flows. Agents support generation, embeddings, and structured assistant prompts with tool calling and state updates.

---

## 🧠 Overview

The `OllamaAgent` wraps a call to your local [Ollama](https://ollama.com) instance using the ollama-rs client. It supports the following modes:

- `chat`: Structured prompting with JSON output (default)
- `generate`: Plain text generation
- `embed`: Generate vector embeddings for text

---

## 🧩 Configuration

Example YAML:

```yaml
generate_reply:
  ollama:
    task: "Summarise the payload"
    model: "llama3"
    mode: generate
    ollama_url: "http://localhost:11434"
```

Fields:

- `task`: Required. Used as part of the input to the LLM.
- `model`: Optional. Model name (e.g., `llama3:instruct`, `mistral`, etc.)
- `mode`: Optional. One of `generate`, `chat`, or `embed`. Default: `chat`
- `ollama_url`: Optional. Default: `http://localhost:11434`
- `tool_names`: Optional list of tools this agent can call
- `model_options`: Optional advanced options (e.g., temperature)

---

## 🤖 Chat Mode (Structured Prompting)

This is the default and most powerful mode. The agent receives:

- `task`: Instruction of what to do
- `payload`: Latest message or extracted data
- `state`: Context memory of the session
- `connections`: Allowed follow-ups
- `tools`: Optional callable tools

The model must return structured JSON with:

```json
{
  "payload": { ... },
  "state": {
    "add": [...],
    "update": [...],
    "delete": [...]
  },
  "tool_call": {
    "name": "weather_api",
    "action": "forecast",
    "input": { "q": "London" }
  },
  "connections": ["next_node"],
  "reply_to_origin": false
}
```

Empty fields should be omitted. Errors in structure will be logged.

---

## ✍️ Generate Mode

For simple completion tasks:

```yaml
generate_summary:
  ollama:
    mode: generate
    task: "Create summary"
    model: "llama3"
```

Payload:

```json
{ "prompt": "Explain why the sky is blue" }
```

Returns:

```json
{ "generated_text": "..." }
```

---

## 🔎 Embed Mode

To compute vector embeddings:

```yaml
vectorise:
  ollama:
    mode: embed
    task: "Embed this"
    model: "llama3-embed"
```

Payload:

```json
{ "text": "The quick brown fox." }
```

Returns:

```json
{ "embeddings": [0.123, -0.456, ...] }
```

---

You're now ready to use AI agents inside Greentic to power dynamic, tool-calling, state-aware flows!

