# Building a Channel Plugin for Greentic

## Welcome!

First off—thank you for taking the time to read this. You're here because you're curious, excited, or maybe even determined to help shape the future of digital workers. We appreciate that, deeply. The Greentic ecosystem empowers digital agents to operate with context, intelligence, and purpose—and to do that, they need to talk to the outside world.

That’s where **Channel Plugins** come in.

Channel plugins act as the eyes and ears of digital workers. They connect to systems like Telegram, WebSocket, WhatsApp, email, and more. Without these plugins, your digital workers would be isolated, unable to observe or act.

And the best part? These plugins can be written in **any language** that can speak JSON over standard input/output.

- Prefer **Rust**? You’re in good company.
- Love **Node.js**? Go for it.
- Want to try **Go**? Absolutely possible.

Let’s break down what a plugin needs to do and how to build one.

---

## How Communication Works

Channel plugins are standalone processes that:

- Receive instructions via `stdin` (as JSON-RPC requests)
- Send back replies via `stdout` (as JSON-RPC responses or notifications)

You do **not** need any network server or port binding in your plugin.

### The Core Protocol

The plugin talks JSON-RPC 2.0 over STDIO:

- Greentic sends `Request` messages (e.g., `init`, `send_message`)
- Plugin replies with `Response` messages (e.g., `InitResult`, `MessageOutResult`)
- Plugins can also send `messageIn` notifications to alert Greentic of new incoming messages

---

## Trait to Implement (Rust)

The plugin must implement two traits:

### 1. `HasStore`

This gives your plugin access to config and secret key-value stores:

```rust
trait HasStore {
    fn config_store(&self) -> &DashMap<String, String>;
    fn secret_store(&self) -> &DashMap<String, String>;
}
```

### 2. `PluginHandler`

This trait defines the full plugin lifecycle:

```rust
#[async_trait]
trait PluginHandler: HasStore + Clone + Send + Sync + 'static {
    async fn init(&mut self, params: InitParams) -> InitResult;
    fn name(&self) -> NameResult;
    fn capabilities(&self) -> CapabilitiesResult;
    async fn state(&self) -> StateResult;
    async fn drain(&mut self) -> DrainResult;
    async fn stop(&mut self) -> StopResult;
    fn list_config_keys(&self) -> ListKeysResult;
    fn list_secret_keys(&self) -> ListKeysResult;
    async fn send_message(&mut self, params: MessageOutParams) -> MessageOutResult;
    async fn receive_message(&mut self) -> MessageInResult;
}
```

### Example Capabilities:
```rust
fn capabilities(&self) -> CapabilitiesResult {
    CapabilitiesResult {
        capabilities: ChannelCapabilities {
            name: "websocket".to_string(),
            supports_sending: true,
            supports_receiving: true,
            supports_text: true,
            supports_routing: false,
            ..Default::default()
        }
    }
}
```

---

## Running the Plugin

In `main.rs`:
```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    run(MyPlugin::default()).await
}
```

Greentic launches your plugin via `spawn_rpc_plugin("path/to/executable")` and pipes messages via stdio.

---

## Writing a Plugin in Node.js

```js
const readline = require('readline');

process.stdin.on('data', (chunk) => {
  const json = JSON.parse(chunk.toString());
  // handle JSON-RPC request and send reply via stdout
  if (json.method === 'name') {
    process.stdout.write(JSON.stringify({
      jsonrpc: '2.0',
      id: json.id,
      result: { name: 'node_plugin' }
    }) + '\n');
  }
});
```

---

## Writing a Plugin in Go

```go
func main() {
    reader := bufio.NewReader(os.Stdin)
    for {
        line, _ := reader.ReadString('\n')
        var req map[string]interface{}
        json.Unmarshal([]byte(line), &req)
        // handle request and write response
        if req["method"] == "name" {
            response := map[string]interface{}{
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": map[string]string{"name": "go_plugin"},
            }
            res, _ := json.Marshal(response)
            fmt.Println(string(res))
        }
    }
}
```

---

## Deployment

1. Compile or copy your plugin binary to:

```
greentic/plugins/channels/stopped/<plugin_name>
```

2. Greentic will auto-start it when referenced in a flow.

3. Use `spawn_rpc_plugin("./plugins/channels/stopped/websocket")` in tests.

---

## Inspiration: Example Plugins

- ✅ **WebSocket Plugin**: A simple way to expose/send messages over WebSockets
- ✅ **Telegram Plugin**: A full-featured adapter for Telegram Bots

Both plugins implement the full `PluginHandler` trait and communicate via JSON-RPC.

---

## Final Words ❤️

Thanks again for helping make Greentic stronger. Every channel plugin you create is another bridge to a world of action, automation, and possibility. Whether you're enabling conversations from WhatsApp or broadcasting alerts from Slack, you're expanding what our digital workers can see and do.

If you have questions, ideas, or want to showcase your plugin—let us know. Let’s build a world of cooperative AI together.

