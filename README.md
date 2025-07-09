<table>
  <tr>
    <td><img src="assets/greentic-logo-very-small.png" alt="Greentic.AI Logo" width="120"></td>
    <td>
      <h1>Greentic.AI 🚀</h1>
      <p><strong>Build armies of digital workers:</strong> fast, secure, and extendable. Automate anything using Wasm tools, channels, agents, and flows.</p>
    </td>
  </tr>
</table>
Now with: **intelligent agents** and **processes**!

---

Greentic.ai is currently at v0.2.0. You will have to create your own flows, plugins, tools,... however a basic store with some free flows, plugins and tools is available. You can use tools that connect to APIs which use no authentication or API keys. oAuth will be supported in v0.3.0. You will have to run Greentic.ai from your local computer. Deploying onto the Cloud is coming in v0.4.0. The [vision for v1.0.0](./docs/VISION.md) however foresees a world where you just ask via WhatsApp, Teams, Slack, Telegram,... for a digital worker to be generated and automatically Greentic.ai will create it for you based on a simple request. Also learn how Greentic.ai is able to generate revenues for partners. 

---

## 📋 Table of Contents

1. [Introduction](#introduction)
2. [What is a Digital Worker?](#wat-is-a-digital-worker)
3. [Key Concepts](#key-concepts)
   - [Tools (MCP in Wasm)](#tools-mcp-in-wasm)
   - [Channels](#channels)
   - [Processes](#processes)
   - [Agents](#agents)
4. [Getting Started](#getting-started)
5. [Quick Flow Example (YAML)](#quick-flow-example-yaml)
6. [Controlling Flows, Channels & Tools](#controlling-flows-channels--tools)
7. [Coming Soon](#coming-soon)
8. [Need Custom Agentic Automation?](#need-custom-agentic-automation)
9. [Contributing](#contributing)
10. [License](#license)

---

## 📝 Introduction

Greentic.AI is an open-source platform designed to let you build, deploy, and manage digital workers at lightning speed.

- **Fastest** runtime with zero cold-starts for WebAssembly tools.
- **Extendable** architecture: plug in your own channels, tools, agents and processes, all defined in an easy to understand text-based flow.
- **Secure** by design: tools are sandboxed inside Wasm allowing securely running untrusted third-party MCP tools.
- **Observability** via OpenTelemetry integrations

---

## 🤖 What is a Digital Worker?

A **Digital Worker** is a flow that acts autonomously and intelligently to handle a complete task, from end to end.

It:

- Listens for messages (via **Channels** like Telegram or Slack)
- Extracts meaning or decisions (via **Agents**, powered by LLMs)
- Calls APIs or executes functions (via **Tools** written in Wasm)
- Handles control logic (via **Processes** like retries, conditionals, loops)

Flows link these components into one cohesive automation. Your digital workers are secure, modular, and language-agnostic.

---

## 🔑 Key Concepts

### Tools (MCP in Wasm)

- **MCP** (Model-Context Protocol) modules compile to WebAssembly.
- Each tool can define its own actions, inputs, outputs, and run logic securely.
- Tools live in `tools/` and are called by the flows.

👉 [Learn how to build MCP Tools (Coming Soon)](./docs/TOOL_PLUGIN.md)

### Channels

- **Channels** allow flows to send/receive messages to/from the outside world.
- Examples: Telegram, Slack, Email, HTTP Webhooks.

👉 [How to build Channel Plugins](./docs/PLUGIN.md)

### Processes

- **Processes** are logic blocks (decisions, branches, loops, retries).
- Defined declaratively in YAML.

### Agents

- **Agents** are LLM-powered nodes capable of autonomous decision-making.
- Agents understand context, use memory, trigger tools, and follow goals.

---

## 🚀 Getting Started

Install Greentic.AI via:
```bash
cargo install greentic
```
The first time around initialise everything:
```bash
greentic init
```
Before running a flow, you can validate that the yaml/json
format is valid, all channels and tools are present as well 
as all required config keys and secrets are set up.
If a channel or tool are not present, they will be pulled automatically. 
Afterwards you can deploy the flow. You only need to validate/deploy
one time. Afterwards you can start/stop the flow.
```bash
greentic flow validate <file>.ygtc
greentic flow deploy <file>.ygtc
```

When you are ready to start a flow you can call:
```bash
greentic flow start <flow_id>
```

---

## 🛠 Quick Flow Example (YAML)

```yaml
id: weather_bot
title: Get your weather prediction
description: >
  This flow shows how you can combine either a fixed question and answer process
  with an AI fallback if the user is not answering the questions correctly.
channels:
  - telegram  
nodes:
  # 1) Messages come in via Telegram
  telegram_in:
    channel: telegram
    in: true

   # 2) QA node: ask for the city and fallback to the OllamaAgent if more than 3 words are used
  extract_city:
    qa:
      welcome_template: "Hi there! Let's get your weather forecast."
      questions:
        - id: q_location
          prompt: "👉 What location would you like a forecast for?"
          answer_type: text
          state_key: q
          max_words: 3
      fallback_agent:
        type: ollama
        task: |
          You are a weather bot and have asked the location for a weather forecast.
          The user responded in free text.  Extract exactly the location they want the weather for.
          Return **exactly** a JSON object like:
          {
            "q": "<location text>",
          }
      routing:
        - to: forecast_weather
  # 3) “forecast_weather”: the Weather API tool, using the JSON from parse_request.
  forecast_weather:
    tool:
      name: weather_api
      action: forecast_weather
    parameters:
      q: "{{extract_city.payload.city}}"
      days: 3

  # 4) “weather_template”: format the weather API’s JSON into a friendly sentence.
  weather_out_template:
    template: |
        Here’s your forecast for {{ location.name }}:

        • High: {{ forecast.forecastday.[0].day.maxtemp_c }}°C
        • Low: {{ forecast.forecastday.[0].day.mintemp_c }}°C
        • Condition: {{ forecast.forecastday.[0].day.condition.text }}
        • Rain Today? {{#if (eq (forecast.forecastday.[0].day.daily_will_it_rain) 1)}}Yes{{else}}No{{/if}}

  # 5) “telegram_out”: send the forecast back to Telegram.
  telegram_out:
    channel: telegram
    out: true

connections:
  telegram_in:
    - extract_city

  extract_city:
    - forecast_weather

  forecast_weather:
    - weather_out_template

  weather_out_template:
    - telegram_out 
```

---

## ⚙️ Controlling Flows, Channels & Tools

```bash
# Start/Stop flows
greentic flow validate <file>.ygtc 
greentic flow deploy <file>.ygtc
greentic flow start <flow-id>
greentic flow stop <flow-id>
```

---

## 🔭 Coming Soon

- v0.3.0 oAuth MCP Tools - connect to any SaaS
- v0.4.0 Serverless Cloud deployment of flows - greentic deploy <flow>

Roadmap:
- More Agentic: memory persistence, vector databases, A2A,...
- AI Flow Designer
- Flow, Tools, Channels & Processes marketplace

---

## 📬 Need Custom Agentic Automation?

Have a specific use-case or need expert help?\
Please fill out our form: [Agentic Automation Inquiry](https://forms.gle/h17SdjoUxozJf6XA6)

---

## 🤝 Contributing

We are actively looking for contributors and welcome contributions of all kinds!

- Bug reports 🐞
- Feature requests 🎉
- Code & documentation PRs 📝

1. Fork the repo
2. Create a feature branch
3. Open a PR against `main`

See [CONTRIBUTING.md](./docs/CONTRIBUTING.md) for full guidelines.

---

## 📄 License

Distributed under the **MIT License**. See [LICENSE](./LICENSE) for details.

---

Thank you for checking out **Greentic.AI**—let’s build the future of automation together! 🚀

