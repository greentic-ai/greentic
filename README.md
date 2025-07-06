# Greentic.AI 🚀

Welcome to **Greentic.AI**, the fastest, most extendable, and secure agentic platform for building armies of digital workers. Whether you’re integrating external systems via channels, calling external APIs and other MCP tools, or crafting complex processes, Greentic.AI gives you the building blocks to automate anything.

Now with: **intelligent agents** and **processes**!

---

## Greentic.AI's V1.0.0 Vision

When Greentic.AI reaches version 1.0.0 anybody should be able to:
- Use their favourite messaging system, e.g. Slack, WhatsApp, Teams, Telegram, Messenger,... or other channels like SMS, email, WebChat, voice call and ask Greentic.AI to generate digital workers.
- "Generte a digital assistant who transcribes and summarises meetings; makes calendar bookings accross organisations; reserves mreeting rooms, fligths, hotels, Uber, AirBNB, ..." should be enough to get what you ask for.
- "Generate a digital worker who reads incoming customer emails from sales@acme.com; sorts them between requests for a quote vs after-sales vs others; in case of a quote, opens sales leads in HubSpot and generates a quote via our quoting tool XYZ and asks for approval from sales-manager on Slack before reponding with an email to the customer; in case of after-sales a ServiceNow request is opened and if the request can be done automatically, asks approval from operations-manager on Slack before solving the issue and responding with an email to the customer; in case of others, log the request in ServiceNow but ask operations-manager on Slack what to do and if possible do it or categorise as manual."

The objective is that a simple text command is all Greentic.AI needs to generate a digital worker. Greentic.AI will be able to translate this command into a flow (= yaml text file) which a human can review if necessary to understand if the request has been translated correctly. Given each element of Greentic.AI is self-describing with schemas, Greentic.AI should be able to consistently generate these flows and call agents, tools, processes and integrate with external channels. 

Although Greentic.AI is open source, this does not mean no money can be earned. Developers will be able to create new agents, tools, processes and channels which can be added to Greentic.AI so whoever has a subscription can use them and similar to Spotify the most successful contributions will earn the largest portion of the share of subscriptions assigned to paying developers. Alternatively, developers will be able to create agents, tools, processes and channels which are billed for usage or need to be purchases. Examples would be an AI agent which is billed per 1,000 tokens. More info at [contributor@greentic.ai](mailto:contributor@greentic.ai). Finally anybody can decide to upload their best flows and the most popuplar flows will also earn a revenue share.

Sponsors can pay to make their tools and services available for free to anybody using Greentic. The first sponsor of a category will become the default option for 12 months so "greentic tool pull database" is open for any database vendor to sponsor. Same for "greentic deploy <flow>" is open for any Cloud vendor. "greentic agent use <model>" for any AI model API. Afterwards the highest paying sponsor each month will earn the category. Reach out to [sponsor@greentic.ai](mailto:sponsor@greentic.ai).

If you want to avoid that your tool or service is behind the "subscription wall", then you can sponsor for it to be in the free category. Reach out to [sponsor@greentic.ai](mailto:sponsor@greentic.ai). Greentic.AI users who use sponsored tools and services will share an anonymised identifier (SHA256[sponsor_id+customer@email]) with the sponsor and will be able to read a sponsorship message and agree if they want to be contacted.

If you or your company is interested in a future subscription, please contact [sales@greentic.ai](mailto:sales@greentic.ai).

Anybody needing consulting to transition towards digital workers will be able to book an appointment with any of the Greentic.AI certified consultants. Certified consultants will earn a commissions for reselling Greentic.AI subscriptions and third-party tools and services. More info at [sales@greentic.ai](mailto:sales@greentic.ai).

All MCP tools and processes use WASM, for which they can be digitally signed. If you need industry specific certifications, e.g. regulatory compliance, security standards,... then either industry expert partners will be able to offer you digitally signed versions or offer a curated and certified list via a custom industry-specific store. More info at [sales@greentic.ai](mailto:sales@greentic.ai).

For any press questions, please contact: [press@greentic.ai](mailto:press@greentic.ai). If you found a security issue, please do not open a Github issue, contact: [security@greentic.ai](mailto:security@greentic.ai) instead.  

Greentic.AI is and always will be **open source** so it will be **free** to download. Partners will be able to upsell through Greentic.AI for which joining the Greentic.AI movement enables you to **earn**. If you want to contribute to make Greentic.AI v1.0.0 a reality asap, reach out to [contributor@greentic.ai](mailto:contributor@greentic.ai). All early contributors will gain unfair advantages, e.g. their tools and services will be listed first before others.

## 📋 Table of Contents

1. [Introduction](#introduction)
2. [What is a Digital Worker?](#digital-worker)
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

> Some commands below are coming soon:

```bash
# 🚧 Coming Soon
# greentic flow validate my_flow.ygtc
# greentic channel pull telegram
# greentic tool pull weather_api

greentic flow start my_flow

greentic channel start telegram

greentic tool start weather_api
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
# greentic flow validate <file>.ygtc (🚧 Coming Soon)
greentic flow deploy <file>.ygtc
greentic flow start <flow-id>
greentic flow stop <flow-id>

# Start/Stop channels
greentic channel start <channel-name>
greentic channel stop <channel-name>

# Start/Stop tools
greentic tool start <tool-name>
greentic tool stop <tool-name>
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

