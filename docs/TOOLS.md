# Creating MCP Tools in Greentic

This guide explains how to build your own MCP-compatible WebAssembly tools using the WIT interface defined by `mcp-secrets`. We'll walk through the requirements and give you a working example using **The Cat API**.

---

## 🧱 Tool Structure

Each MCP tool is a Rust crate that exports a `cdylib` and implements the `router` and optionally `secrets-list` interfaces as defined in WIT.

### Required:

- `wit-bindgen` usage with the `mcp-secrets` world.
- Export via `wit_bindgen::generate!` and `export!(YourToolStruct)`.
- `list_tools()` that returns tools this plugin supports.
- `call_tool()` that implements the logic.
- If your tool uses API keys or credentials, implement `secrets_list::Guest` and list the secrets you require.

---

## 🧪 Example: Cat API Tool

This tool fetches a random cat image from [thecatapi.com](https://developers.thecatapi.com/) and returns it as an embedded image.

### Dependencies

```toml
[dependencies]
anyhow = "1.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
wit-bindgen = "0.43"
reqwest = { version = "0.12", default-features = false, features = ["json"] }
base64 = "0.22"
```

### Required Secrets

Add your API key to:

```env
# File: greentic/secrets/.env
CAT_API_KEY=your_api_key_here
```

Declare it in your `list_secrets()` implementation:

```rust
fn list_secrets() -> Vec<SecretsDescription> {
    vec![SecretsDescription {
        name: "CAT_API_KEY".to_string(),
        description: "API key for The Cat API".to_string(),
        required: true,
    }]
}
```

### Tool Implementation Skeleton

```rust
impl Guest for CatTool {
    fn name() -> String { "cat_tool".into() }

    fn instructions() -> String {
        "Fetches a random cat image from The Cat API.".into()
    }

    fn capabilities() -> ServerCapabilities {
        ServerCapabilities {
            tools: Some(ToolsCapability { list_changed: Some(true) }),
            ..Default::default()
        }
    }

    fn list_tools() -> Vec<Tool> {
        vec![Tool {
            name: "cat_image".into(),
            description: "Returns a random cat image".into(),
            input_schema: Value { json: "{}".into() },
            output_schema: None,
        }]
    }

    async fn call_tool(tool_name: String, args: Value) -> Result<CallToolResult, ToolError> {
        if tool_name != "cat_image" {
            return Err(ToolError::NotFound(tool_name));
        }

        let key = std::env::var("CAT_API_KEY").map_err(|_| ToolError::ExecutionError("Missing CAT_API_KEY".into()))?;
        let client = reqwest::Client::new();
        let res: serde_json::Value = client
            .get("https://api.thecatapi.com/v1/images/search")
            .header("x-api-key", key)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?
            .json()
            .await
            .map_err(|e| ToolError::ExecutionError(e.to_string()))?;

        let url = res[0]["url"].as_str().ok_or_else(|| ToolError::ExecutionError("No URL in response".into()))?;
        let bytes = client.get(url).send().await.map_err(|e| ToolError::ExecutionError(e.to_string()))?.bytes().await.map_err(|e| ToolError::ExecutionError(e.to_string()))?;
        let mime_type = if url.ends_with(".png") { "image/png" } else { "image/jpeg" };
        let base64_str = base64::engine::general_purpose::STANDARD.encode(bytes);
        let data_uri = format!("data:{};base64,{}", mime_type, base64_str);

        Ok(CallToolResult {
            content: vec![Content::Image(ImageContent {
                data: data_uri,
                mime_type: mime_type.to_string(),
                annotations: None,
            })],
            is_error: Some(false),
        })
    }

    // Other functions (optional) can be left unimplemented or return empty
}
```

---

## 📦 Building

Run:

```bash
cargo build --release --target wasm32-wasi
```

Output will be at:

```sh
target/wasm32-wasi/release/cat_tool.wasm
```

Move it to the tools folder and validate:

```bash
greentic tool add cat_tool target/wasm32-wasi/release/cat_tool.wasm
```

---

## ✅ Summary Checklist

- [ ] Create a Rust crate with `crate-type = ["cdylib"]`
- [ ] Use `wit_bindgen` with the `mcp-secrets` world
- [ ] Implement and export a struct for:
  - `router::Guest`
  - `secrets_list::Guest` (if using secrets)
- [ ] Implement:
  - `name()`
  - `instructions()`
  - `capabilities()`
  - `list_tools()`
  - `call_tool()`
- [ ] Add tool metadata (`Tool`) with input/output schemas
- [ ] Use `reqwest` (with `json` feature) for external API calls
- [ ] Add API keys or secrets to `greentic/secrets/.env`
- [ ] Declare required secrets in `list_secrets()`
- [ ] Build with `cargo build --release --target wasm32-wasi`
- [ ] Add tool with:  
  `greentic tool add your_tool target/wasm32-wasi/release/your_tool.wasm`
- [ ] Test tool via your Greentic flows

You're now ready to ship your MCP tools to the Greentic platform!

