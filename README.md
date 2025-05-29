# Greentic AI 🚀

Welcome to **Greentic AI**, the fastest, most extendable, and secure agentic platform for building autonomous workflows. Whether you’re integrating external systems via channels, calling external APIs and other MCP tools, or crafting complex processes, Greentic AI gives you the building blocks to automate anything. Coming soon: intelligent LLM-powered agents!

---

## 📋 Table of Contents

1. [Introduction](#introduction)  
2. [Key Concepts](#key-concepts)  
   - [Tools (MCP in Wasm)](#tools-mcp-in-wasm)  
   - [Channels](#channels)  
   - [Processes](#processes)  
   - [Agents (Coming Soon)](#agents-coming-soon)  
3. [Getting Started](#getting-started)  
4. [Quick Flow Example](#quick-flow-example)  
5. [Controlling Flows, Channels & Tools](#controlling-flows-channels--tools)  
   - [Start / Stop a Flow](#start--stop-a-flow)  
   - [Start / Stop a Channel](#start--stop-a-channel)  
   - [Start / Stop a Tool](#start--stop-a-tool)  
6. [Coming Soon](#coming-soon)  
7. [Need Custom Agentic Automation?](#need-custom-agentic-automation)  
8. [Contributing](#contributing)  
9. [License](#license)  

---

## 📝 Introduction

Greentic AI is an open-source platform designed to let you build, deploy, and manage agentic workflows at lightning speed.  
- **Fastest** runtime with zero cold-starts for WebAssembly tools.  
- **Extendable** architecture: plug in your own channels, tools, and processes.  
- **Secure** by design: sandboxed Wasm allows to securely run untrusted third-party MCP tools.
- **Observability** via OpenTelemetry integrations

---

## 🔑 Key Concepts

### Tools (MCP in Wasm)

- **MCP** (Micro-Connector Process) modules compile to WebAssembly.  
- Each tool exposes a simple API:  
  ```jsonc
  {
    "name": "weather_api",
    "action": "forecast_weather",
    "parameters": { "q": "London", "days": 3 }
  }
  ```
- Tools run in a sandbox, ensuring resource limits and security.

### Channels

- **Channels** let your flows send and receive messages to the outside world.  
- Examples: Telegram, Slack, HTTP webhooks, databases.  
- Define a channel in your config:
  ```toml
  [[channels]]
  name = "telegram"
  type = "telegram_bot"
  token = "TELEGRAM_TOKEN"
  ```

### Processes - (Coming Soon)

- **Processes** encapsulate business logic and control flow (conditionals, retries, loops).  

### Agents (Coming Soon)

- **Agents** will be LLM-driven “super-processes” that can plan, learn, and adapt.  
- Stay tuned for:
  - Prompt management  
  - Memory & context handling  
  - Autonomous decision loops  

---

## 🚀 Getting Started

1. **Install Greentic CLI**  
   ```bash
   cargo install greentic (coming soon, for now do cargo run)
   ```
2. **Initialize one time **  
   ```bash
   greentic init
   ```
3. **Manage your flows**  (coming soon)
   ```bash
   greentic flow validate my_flow.greentic
   greentic flow deploy my_flow.greentic
   greentic flow start/stop my_flow.greentic
   greentic channel pull telegram
   greentic channel start/stop telegram
   greentic tool pull weather_api
   greentic tool start/stop weather_api 
   ```

---

## 🛠 Quick Flow Example

```json
{
  "id": "sample-weather-flow",
  "description": "Fetch and relay weather forecasts",
  "channels": [
    {
      "name": "telegram",
      "config": {
        "token": "TELEGRAM_TOKEN"
      }
    }
  ],
  "tools": [
    {
      "name": "weather_api",
      "wasm": "weather_api.wasm"
    }
  ],
  "nodes": [
    {
      "id": "weather_in",
      "channel": "telegram",
      "in": true
    },
    {
      "id": "forecast",
      "tool": "weather_api.forecast_weather",
      "parameters": {
        "q": "{{weather_in.payload.location}}",
        "days": 3
      }
    },
    {
      "id": "weather_out",
      "channel": "telegram",
      "out": true
    }
  ],
  "connections": [
    {
      "from": "weather_in",
      "to": "forecast"
    },
    {
      "from": "forecast",
      "to": "weather_out"
    }
  ]
}
```

Start your flow: (coming soon)
```bash
# validate the flow is ok
greentic flow validate ./sample-weather-flow
# deploy the flow (only needs to happen one time)
greentic flow deploy ./sample-weather-flow
# start your flow
greentic flow start sample-weather-flow
```

Stop your flow:
```bash
greentic flow stop sample-weather-flow
```

---

## ⚙️ Controlling Flows, Channels & Tools (Coming soon)

### Start / Stop a Flow

```bash
# Start
greentic flow start <flow-id>

# Stop
greentic flow stop <flow-id>
```

### Start / Stop a Channel

```bash
#
# Start
greentic channel start <channel-name>

# Stop
greentic channel stop <channel-name>
```

---

## 🔭 Coming Soon

- **Agents**: autonomous LLM-backed actors  

If there is demand:
- **Process Library**: pre-built workflows (e.g., PR triage, sales outreach)  
- **UI Dashboard**: visual flow designer and monitor  

---

## 📬 Need Custom Agentic Automation?

Have a specific use-case or need expert help?  
Please fill out our form: [Agentic Automation Inquiry](https://forms.gle/h17SdjoUxozJf6XA6)

---

## 🤝 Contributing

We welcome contributions of all kinds!  
- Bug reports 🐞  
- Feature requests 🎉  
- Code & documentation PRs 📝  

1. Fork the repo  
2. Create a feature branch  
3. Open a PR against `main`  

See [CONTRIBUTING.md](./CONTRIBUTING.md) for full guidelines.

---

## 📄 License

Distributed under the **MIT License**. See [LICENSE](./LICENSE) for details.  

---

Thank you for checking out **Greentic AI**—let’s build the future of automation together! 🚀
