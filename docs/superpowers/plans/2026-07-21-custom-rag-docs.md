# Custom RAG Documentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish a `/components/custom-rag/` page on docs.greentic.ai that an external partner can follow to plug their own RAG into Greentic, backed by a reference component that provably compiles and runs.

**Architecture:** A new standalone public repo (`greentic-example-rag`) holds an ABI A WASM component exposing a single `retrieve` operation that calls the partner's HTTP retrieval service. Its CI enforces the five verification gates. The docs page inlines that code and links to a pinned tag, and an aside on the existing knowledge page redirects the misconception that a custom RAG can replace the built-in knowledge tier.

**Tech Stack:** Rust (edition 2024, toolchain 1.95.0), `wasm32-wasip2`, `wit-bindgen`, `greentic-interfaces-guest` (feature `component-v0-6`), `greentic-types`, `greentic-component` CLI, `wasm-tools`, `greentic-pack` CLI (crate `packc`), Astro/Starlight (greentic-docs).

## Global Constraints

- Component ABI is **`greentic:component@0.6.0`**, world `greentic:component/component@0.6.0`. ABI 0.6.1 has no host and must never appear in the example or the page.
- The component must export **both** `greentic:component/node@0.6.0` and `greentic:component/component-descriptor@0.6.0`.
- Rust toolchain `1.95.0`, target `wasm32-wasip2` (`greentic-component/rust-toolchain.toml`).
- Docs are written in **English**. Translations into the seven locales are out of scope.
- Docs must name the pack CLI **`greentic-pack`** (the `[[bin]]` name). `packc` is only the crate/lib name in `greentic-pack/crates/packc`, not an installable command.
- Docs must not present `component-rag`, `component-http`, or `packs/rag-cs` as copyable examples.
- The four limits in the spec's "Limits the page states explicitly" section must all appear on the page.
- No personal names in committed docs — use role labels.
- Commits use conventional format and carry **no** AI attribution or co-author trailer.
- Spec of record: `docs/superpowers/specs/2026-07-21-custom-rag-docs-design.md`.

---

## File Structure

**New repo `greentic-example-rag`** (public, `greenticai` org):

| Path | Responsibility |
|---|---|
| `Cargo.toml` | crate metadata, ABI A world declaration, deps |
| `rust-toolchain.toml` | pins 1.95.0 + `wasm32-wasip2` |
| `src/lib.rs` | the component: `node` Guest (describe + invoke), `component-descriptor` export, `retrieve` dispatch |
| `src/retrieve.rs` | retrieval logic: parse input, call the HTTP service, shape output |
| `component.manifest.json` | operation schemas + `secret_requirements` |
| `.github/workflows/ci.yml` | gates 1–5 |
| `README.md` | what this is, how to fork it, link to the docs page |
| `examples/in.json` | sample input for `greentic-component test` |

`src/lib.rs` holds ABI plumbing only; `src/retrieve.rs` holds the domain logic a partner
actually edits. That split is the point of the example — it shows where their code goes.

**greentic-docs:**

| Path | Change |
|---|---|
| `src/content/docs/components/custom-rag.mdx` | create — the page |
| `src/content/docs/concepts/agent-knowledge.mdx` | modify — add the boundary aside |
| `astro.config.mjs` | modify — sidebar entry in the Components group |

---

## Task 1: Scaffold the example repo and record the export baseline

This task establishes ground truth. Two things the plan cannot assume are settled here by
observation: what the scaffold actually exports, and what `--http-client` does.

**Files:**
- Create: the `greentic-example-rag` working tree (local first; remote creation in Task 6)

**Interfaces:**
- Produces: a compiling ABI A crate named `greentic-example-rag`, and `notes/export-baseline.txt` recording the observed WIT exports — Task 2 depends on that recording.

- [ ] **Step 1: Create the working tree via the scaffold**

Run from a directory outside the monorepo (e.g. `~/src`):

```bash
greentic-component new --name greentic-example-rag --org ai.greentic \
  --template rust-wasi-p2-min \
  --operation retrieve --default-operation retrieve \
  --http-client --secret-key RAG_API_KEY --secret-format text \
  --non-interactive --no-git
```

Expected: a `greentic-example-rag/` directory containing `Cargo.toml`, `src/lib.rs`,
`component.manifest.json`, `Makefile`, `build.rs`, `rust-toolchain.toml`.

If `--http-client` or `--secret-format` is rejected, re-run without the offending flag and note
it — the flags are read from the CLI definition, not from an executed run.

- [ ] **Step 2: Initialise git and commit the untouched scaffold**

Committing the raw scaffold first makes every later diff show exactly what a partner must
change on top of the generator output.

```bash
cd greentic-example-rag
git init -q && git add -A
git commit -q -m "chore: scaffold ABI 0.6.0 component from rust-wasi-p2-min"
```

- [ ] **Step 3: Build for wasm32-wasip2 (gate 1)**

```bash
cargo build --target wasm32-wasip2 --release
```

Expected: builds. Artifact at `target/wasm32-wasip2/release/greentic_example_rag.wasm`.

If it fails on missing `greentic-interfaces-guest` / `greentic-types` versions, resolve the
published versions with `cargo search greentic-interfaces-guest` and pin explicit versions in
`Cargo.toml`. An external partner has no workspace to inherit from, so every dependency must
resolve from crates.io. Record whatever pin you land on — the docs page must show the same one.

- [ ] **Step 4: Record the export baseline (gate 4, first reading)**

```bash
mkdir -p notes
wasm-tools component wit target/wasm32-wasip2/release/greentic_example_rag.wasm \
  | tee notes/export-baseline.txt | grep -n "export" 
```

Expected from reading the template source: `greentic:component/node@0.6.0` is present and
`greentic:component/component-descriptor@0.6.0` is **absent**. Confirm which is true.

If `component-descriptor` is already present, Task 2 reduces to verifying it and its step 3 is
skipped — say so in the Task 2 commit message rather than silently doing nothing.

- [ ] **Step 5: Commit the baseline**

```bash
git add notes/export-baseline.txt
git commit -q -m "chore: record scaffold WIT export baseline"
```

---

## Task 2: Export `component-descriptor@0.6.0`

Without this the runner's describe path returns `Ok(None)` silently (`pack.rs:3186-3189`) and
the designer picker falls back to raw WIT export names instead of logical operations.

**Files:**
- Modify: `src/lib.rs`

**Interfaces:**
- Consumes: `notes/export-baseline.txt` from Task 1.
- Produces: the wasm exports `greentic:component/component-descriptor@0.6.0#describe` returning CBOR-encoded JSON with `operations[]` and `config_schema`.

- [ ] **Step 1: Write the failing check**

The test here is the export inventory itself — a shell assertion, because the property under
test is a property of the built artifact, not of Rust code:

```bash
cat > check-exports.sh <<'EOF'
#!/usr/bin/env bash
# Both exports are required by the runner: node@0.6.0 for invoke (pack.rs:381),
# component-descriptor@0.6.0 for describe (pack.rs:3186).
set -euo pipefail
WASM="${1:-target/wasm32-wasip2/release/greentic_example_rag.wasm}"
WIT="$(wasm-tools component wit "$WASM")"
fail=0
for iface in "greentic:component/node@0.6.0" "greentic:component/component-descriptor@0.6.0"; do
  if grep -q "$iface" <<<"$WIT"; then
    echo "ok: exports $iface"
  else
    echo "MISSING: $iface"; fail=1
  fi
done
exit $fail
EOF
chmod +x check-exports.sh
```

- [ ] **Step 2: Run it to verify it fails**

```bash
./check-exports.sh
```

Expected: `MISSING: greentic:component/component-descriptor@0.6.0`, exit status 1.

- [ ] **Step 3: Add the descriptor export**

Append to `src/lib.rs`. This mirrors the shape the runner's introspection expects, as
implemented by the tested fixture `greentic-runner/tests/assets/component-v0-6-dummy/src/lib.rs`:

```rust
#[cfg(target_arch = "wasm32")]
wit_bindgen::generate!({
    inline: r#"
        package greentic:component@0.6.0;

        interface component-descriptor {
          // CBOR-encoded JSON read by runner-host for contract introspection.
          describe: func() -> list<u8>;
        }

        world component-descriptor-only {
          export component-descriptor;
        }
    "#,
    world: "component-descriptor-only",
    generate_all,
});

#[cfg(target_arch = "wasm32")]
fn component_descriptor_payload_json() -> serde_json::Value {
    serde_json::json!({
        "operations": [
            {
                "name": "retrieve",
                "input": { "schema": retrieve_input_json_schema() },
                "output": { "schema": retrieve_output_json_schema() },
                "input_schema": retrieve_input_json_schema(),
                "output_schema": retrieve_output_json_schema()
            }
        ],
        "config_schema": { "type": "object" }
    })
}

#[cfg(target_arch = "wasm32")]
fn retrieve_input_json_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "query":  { "type": "string",  "description": "The question to retrieve context for" },
            "top_k":  { "type": "integer", "description": "How many passages to return", "default": 5 }
        },
        "required": ["query"]
    })
}

#[cfg(target_arch = "wasm32")]
fn retrieve_output_json_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "context": { "type": "string" },
            "chunks":  { "type": "array", "items": { "type": "object" } }
        },
        "required": ["context"]
    })
}

#[cfg(target_arch = "wasm32")]
struct ComponentDescriptorExports;

#[cfg(target_arch = "wasm32")]
impl exports::greentic::component::component_descriptor::Guest for ComponentDescriptorExports {
    fn describe() -> Vec<u8> {
        greentic_types::cbor::canonical::to_canonical_cbor_allow_floats(
            &component_descriptor_payload_json(),
        )
        .expect("encode descriptor cbor")
    }
}

#[cfg(target_arch = "wasm32")]
export!(ComponentDescriptorExports);
```

If the scaffold's own `wit_bindgen::generate!` macro collides (duplicate `export!` or duplicate
world), reconcile by merging the `component-descriptor` interface into the existing inline WIT
rather than emitting a second `generate!` block. Keep exactly one `export!` per generated world.

- [ ] **Step 4: Rebuild and re-run the check**

```bash
cargo build --target wasm32-wasip2 --release && ./check-exports.sh
```

Expected: both `ok:` lines, exit status 0.

- [ ] **Step 5: Commit**

```bash
git add src/lib.rs check-exports.sh
git commit -q -m "feat: export component-descriptor@0.6.0 for runner introspection"
```

---

## Task 3: Implement `retrieve` against an HTTP retrieval service

**Files:**
- Create: `src/retrieve.rs`
- Modify: `src/lib.rs` (dispatch + module declaration)
- Create: `examples/in.json`

**Interfaces:**
- Consumes: the descriptor schemas from Task 2 — the field names `query`, `top_k`, `context`, `chunks` must match exactly.
- Produces: `retrieve::handle(payload: &serde_json::Value) -> serde_json::Value`, used by the dispatcher in `src/lib.rs`.

- [ ] **Step 1: Write the failing test**

Create `src/retrieve.rs` with the tests first. These run on the host (not wasm), so they need no
runtime:

```rust
//! Retrieval against an external HTTP service.
//!
//! This is the file a partner edits. `lib.rs` is ABI plumbing; the domain logic lives here.

use serde_json::{Value, json};

/// Shapes the request body sent to the partner's retrieval endpoint.
pub fn build_request_body(payload: &Value) -> Result<Value, String> {
    let query = payload
        .get("query")
        .and_then(Value::as_str)
        .filter(|q| !q.trim().is_empty())
        .ok_or_else(|| "missing required field 'query'".to_string())?;
    let top_k = payload.get("top_k").and_then(Value::as_u64).unwrap_or(5);
    Ok(json!({ "query": query, "top_k": top_k }))
}

/// Normalises the service response into the component's output contract.
pub fn shape_response(body: &Value) -> Value {
    let chunks = body
        .get("chunks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let context = body
        .get("context")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            chunks
                .iter()
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n")
        });
    json!({ "context": context, "chunks": chunks })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_body_defaults_top_k_to_five() {
        let body = build_request_body(&json!({ "query": "how do refunds work" })).unwrap();
        assert_eq!(body["query"], "how do refunds work");
        assert_eq!(body["top_k"], 5);
    }

    #[test]
    fn build_request_body_rejects_missing_or_blank_query() {
        assert!(build_request_body(&json!({})).is_err());
        assert!(build_request_body(&json!({ "query": "   " })).is_err());
    }

    #[test]
    fn shape_response_passes_through_explicit_context() {
        let out = shape_response(&json!({ "context": "already joined", "chunks": [] }));
        assert_eq!(out["context"], "already joined");
    }

    #[test]
    fn shape_response_joins_chunk_text_when_context_absent() {
        let out = shape_response(&json!({
            "chunks": [ { "text": "first" }, { "text": "second" } ]
        }));
        assert_eq!(out["context"], "first\n\nsecond");
        assert_eq!(out["chunks"].as_array().unwrap().len(), 2);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test --lib retrieve
```

Expected: FAIL — `src/retrieve.rs` is not yet declared as a module in `src/lib.rs`, so the file
is not compiled. The error names an unresolved module or the tests simply do not run.

- [ ] **Step 3: Declare the module and wire the dispatcher**

In `src/lib.rs`, next to the existing `pub mod qa;`:

```rust
// retrieve: the domain logic — parse input, call the retrieval service, shape output.
pub mod retrieve;
```

Then in the operation dispatcher (`run_component_cbor` in the scaffold), add a `retrieve` branch
ahead of the fallback:

```rust
"retrieve" => match retrieve::build_request_body(&value) {
    Ok(request) => match call_retrieval_service(&request) {
        Ok(body) => retrieve::shape_response(&body),
        Err(err) => serde_json::json!({ "error": err }),
    },
    Err(err) => serde_json::json!({ "error": err }),
},
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cargo test --lib retrieve
```

Expected: 4 tests pass.

- [ ] **Step 5: Enable the host capability features**

These are Cargo features on `greentic-interfaces-guest`, not WIT world edits. `cargo-component`
unions whatever the compiled code actually imports into the final component type, so enabling
the feature and calling the function is sufficient — no `[package.metadata.component.target.dependencies]`
entry is needed.

In `Cargo.toml`:

```toml
greentic-interfaces-guest = { version = ">=1.1.0-dev, <1.2.0-0", default-features = false, features = ["component-v0-6", "secrets", "http-client-v1-1"] }
```

Enable `http-client-v1-1` **only**. Enabling both `http-client` and `http-client-v1-1` fails at
the link step with `failed to upgrade greentic:http/http-client@1.0.0 to @1.1.0 ... different
number of function parameters`. For the same reason, do not enable the catch-all `guest`
feature — it turns on both.

- [ ] **Step 6: Implement the HTTP call**

Add to `src/lib.rs`. This code has been compiled and its import set verified against what the
runner's `register_all` provides (`pack.rs:1598-1626`):

```rust
/// Calls the partner's retrieval endpoint.
///
/// `payload` is the full invocation payload the runner delivers:
/// `{ "config": { ... }, "input": { ... } }`. The runner merges the flow node's
/// `config:` block into the payload before `invoke` (`pack.rs:2390-2422`);
/// components never read host environment variables.
#[cfg(target_arch = "wasm32")]
fn call_retrieval_service(payload: &serde_json::Value) -> Result<serde_json::Value, String> {
    use greentic_interfaces_guest::http_client_v1_1 as client;
    use greentic_interfaces_guest::secrets_store;

    let endpoint = payload
        .get("config")
        .and_then(|c| c.get("rag_endpoint"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "missing config.rag_endpoint".to_string())?;

    // Secrets come from the secrets store only — never from config or the payload.
    let api_key_bytes = secrets_store::get(RAG_API_KEY_SECRET)
        .map_err(|e| format!("secrets get failed: {e:?}"))?
        .ok_or_else(|| format!("missing secret {RAG_API_KEY_SECRET}"))?;
    let api_key = String::from_utf8(api_key_bytes).map_err(|_| "secret is not utf8".to_string())?;

    let body = retrieve::build_request_body(payload.get("input").unwrap_or(payload))?;

    let req = client::Request {
        method: "POST".to_string(),
        url: endpoint.to_string(),
        headers: vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Authorization".to_string(), format!("Bearer {api_key}")),
        ],
        body: serde_json::to_vec(&body).ok(),
    };
    let opts = client::RequestOptions {
        timeout_ms: Some(10_000),
        allow_insecure: Some(false),
        follow_redirects: Some(true),
    };

    let resp = client::send(&req, Some(opts), None)
        .map_err(|e| format!("http error: {} ({})", e.message, e.code))?;
    if !(200..300).contains(&resp.status) {
        return Err(format!("retrieval service returned status {}", resp.status));
    }
    let body_bytes = resp.body.ok_or_else(|| "empty response body".to_string())?;
    serde_json::from_slice(&body_bytes).map_err(|e| format!("invalid JSON response: {e}"))
}

#[cfg(target_arch = "wasm32")]
const RAG_API_KEY_SECRET: &str = "RAG_API_KEY";
```

Adjust the Step 3 dispatcher branch to match this signature — it passes the whole payload, not
just the input half:

```rust
"retrieve" => match call_retrieval_service(&value) {
    Ok(body) => retrieve::shape_response(&body),
    Err(err) => serde_json::json!({ "error": err }),
},
```

Reference notes, verified: `secrets_store::get(key) -> Result<Option<Vec<u8>>, SecretsError>`
binds `greentic:secrets-store@1.0.0` (read-only; the crate's `secrets` feature cannot reach the
write-capable 1.1.0). The runner registers a compat shim that satisfies 1.0.0 imports from its
1.1.0 host state, so this instantiates against the real runner. `component-rag` calls the same
underlying packages but through a locally generated `bindings::` module — its call-site *shape*
is a valid reference, its module paths and world composition are not.

- [ ] **Step 7: Rebuild and re-check exports**

```bash
cargo build --target wasm32-wasip2 --release && ./check-exports.sh
```

Expected: builds, both exports still present. Also confirm with
`wasm-tools component wit` that the component now imports
`greentic:http/http-client@1.1.0` and `greentic:secrets-store/secrets-store@1.0.0` — and that it
does **not** import `greentic:http/http-client@1.0.0`.

- [ ] **Step 8: Add the sample input**

This mirrors the shape the runner actually delivers, so the harness exercises the same parsing
path as production:

```bash
mkdir -p examples
cat > examples/in.json <<'EOF'
{
  "config": { "rag_endpoint": "https://rag.example.com/v1/retrieve" },
  "input":  { "query": "how do refunds work", "top_k": 3 }
}
EOF
```

- [ ] **Step 9: Commit**

```bash
git add src/retrieve.rs src/lib.rs examples/in.json
git commit -q -m "feat: implement retrieve against an external HTTP retrieval service"
```

---

## Task 4: Manifest, secret requirements, and the harness run

**Files:**
- Modify: `component.manifest.json`

**Interfaces:**
- Consumes: the `retrieve` operation and the `RAG_API_KEY` secret name from Task 3.
- Produces: a manifest that `greentic-component doctor` accepts, and a recorded harness output the docs page quotes.

- [ ] **Step 1: Declare the operation schemas and the secret requirement**

Edit `component.manifest.json` so `operations[]` contains `retrieve` with non-empty
`input_schema` / `output_schema` (empty schemas are rejected with `E_OP_SCHEMA_EMPTY`), and add:

```json
"secret_requirements": [
  {
    "key": "RAG_API_KEY",
    "required": true,
    "description": "API key for the retrieval service",
    "format": "text"
  }
]
```

The `input_schema` and `output_schema` here must match the JSON schemas emitted by
`retrieve_input_json_schema()` / `retrieve_output_json_schema()` in Task 2. The runtime reads the
tool's parameter schema from the pack manifest, not from `describe()` — if these two drift, the
model sees a different contract at runtime than the composer previewed.

- [ ] **Step 2: Build through the CLI**

```bash
greentic-component build --manifest ./component.manifest.json --no-flow
```

`--no-flow` is required here: without it the build fails at dev-flow generation with `Required
field query has no default; cannot generate default dev_flow` — `query` is required and has no
sensible default. With `--no-flow` the wasm builds and the blake3 hashes refresh. Note the build
also prints `warning: skipping describe artifacts (... greentic:http/http-client@1.1.0 ... not
found in the linker)` and does **not** emit `dist/*.describe.cbor` — the same linker gap
documented in Step 3. That is expected for an HTTP component and does not block packaging
(Task 5 uses the wasm + manifest, not the describe artifacts). Both gotchas belong in the docs.

- [ ] **Step 3: Run doctor (gate 2) — and record that it cannot instantiate an http component**

```bash
greentic-component doctor target/wasm32-wasip2/release/greentic_example_rag.wasm \
  --manifest ./component.manifest.json
```

**Verified outcome (both CLI 1.1.0 and 1.3.0-research.1):** this FAILS with
`component imports instance greentic:http/http-client@1.1.0, but a matching implementation was
not found in the linker`. This is not a component defect. The `greentic-component` local harness
wires only the legacy `greentic:host@1.0.0` runner-host HTTP surface plus control/state/secrets
(`greentic-component/crates/greentic-component/src/test_harness/linker.rs:114-124`); it does not
wire the blessed `greentic:http/http-client@1.1.0` interface that the production runner provides
(`greentic-runner-host/src/pack.rs` `register_all`, `http_client_v1_1: Some(...)`). The
component targets the runner's ABI on purpose, per the platform's own WIT guidance
(`greentic:host@1.0.0` is marked "LEGACY COMPATIBILITY SURFACE. Prefer dedicated host capability
worlds"), so the harness cannot instantiate it.

Gates 2 and 3 are therefore **not achievable via the CLI harness for an HTTP component**, and
this is recorded as skipped-with-reason rather than papered over. The verification that DOES run:
`cargo test` (the `retrieve.rs` logic, from Task 3), `check-exports.sh` (the ABI shape, Task 2),
and the runner load-test (Task 9) for full instantiation. Save the doctor output as evidence:

```bash
mkdir -p notes
greentic-component doctor target/wasm32-wasip2/release/greentic_example_rag.wasm \
  --manifest ./component.manifest.json > notes/doctor-linker-gap.txt 2>&1 || true
```

- [ ] **Step 4: Re-confirm the logic gate that DOES run**

```bash
cargo test --lib
```

Expected: passes, including the four `retrieve` tests. This is the local behavioural proof;
`greentic-component test` cannot serve that role for this component (Step 3).

- [ ] **Step 5: Commit**

```bash
git add component.manifest.json notes/doctor-linker-gap.txt
git commit -q -m "feat: declare retrieve schemas and the RAG_API_KEY requirement"
```

---

## Task 5: Package as a pack (gate 5)

**Files:**
- Create: `pack/pack.yaml`
- Create: `pack/components/` (the built wasm + manifest)

**Interfaces:**
- Consumes: the built wasm and `component.manifest.json` from Task 4.
- Produces: a `.gtpack`, and the `pack_id` / component id the docs page uses in its flow example.

- [ ] **Step 1: Install the pack CLI**

The crate lives at `greentic-pack/crates/packc`, but the installed binary is named
**`greentic-pack`** (its `[[bin]]` name) — there is no `packc` executable.

```bash
cargo install --path <monorepo>/greentic-pack/crates/packc --locked
greentic-pack --version
```

- [ ] **Step 2: Write pack.yaml**

Follow `PackConfig` (`greentic-pack/crates/packc/src/config.rs:18`) — `pack_id`, `version`,
`kind`, and `publisher` are required. Model it on the working
`packs/chat2data/pack.yaml`, **not** on `packs/rag-cs/pack.yaml`, which is invalid.

```yaml
pack_id: greentic.example.rag
version: 0.1.0
kind: application
publisher: Greentic
components:
  - id: greentic-example-rag
    version: 0.1.0
    world: greentic:component/component@0.6.0
    wasm: components/greentic_example_rag.wasm
    supports: [componentconfig]
```

The `id` here is what a flow node key and an agentic-worker `component_ref` must both use, and
it must equal the `COMPONENT_NAME` the component reports from `describe()`. Nothing validates
this across the boundary — a mismatch drops the tool with only a warning
(`greentic-aw-runtime/src/tools.rs:106-110`). Keeping all three spellings identical is the
example's job to demonstrate.

- [ ] **Step 3: Build the pack**

```bash
mkdir -p pack/components
cp target/wasm32-wasip2/release/greentic_example_rag.wasm pack/components/
cp component.manifest.json pack/components/
greentic-pack build --in ./pack
```

Expected: `pack/dist/manifest.cbor` and a `.gtpack`.

- [ ] **Step 4: Sign and verify**

```bash
openssl genpkey -algorithm ed25519 -out /tmp/example-signing.pem
greentic-pack sign --pack ./pack --key /tmp/example-signing.pem
greentic-pack verify --pack ./pack
```

Expected: verify passes. The throwaway key stays out of the repo — add `*.pem` to
`.gitignore` and never commit it.

- [ ] **Step 5: Commit**

```bash
printf '*.pem\ntarget/\n' >> .gitignore
git add pack/pack.yaml .gitignore
git commit -q -m "feat: package the example component as a gtpack"
```

---

## Task 6: Publish the repo with CI

**Files:**
- Create: `.github/workflows/ci.yml`
- Create: `README.md`

**Interfaces:**
- Consumes: `check-exports.sh` (Task 2), the manifest (Task 4), `pack.yaml` (Task 5).
- Produces: the public repo URL and the pinned tag `v0.1.0` that the docs page links to.

- [ ] **Step 1: Write the CI workflow**

```yaml
name: ci
on: [push, pull_request]

jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.95.0
        with:
          targets: wasm32-wasip2
      - uses: Swatinem/rust-cache@v2
      - name: Unit tests
        run: cargo test --lib
      - name: Build (gate 1)
        run: cargo build --target wasm32-wasip2 --release
      - name: Install wasm-tools
        run: cargo install wasm-tools --locked
      # Gate 4 is the one that matters most: it is exactly the check
      # component-rag fails, and it fails silently at runtime.
      - name: Verify required WIT exports (gate 4)
        run: ./check-exports.sh
```

Gates 2, 3, and 5 need `greentic-component` and `greentic-pack`, which are not published to
crates.io as standalone installables in a form this workflow can assume. (Note gates 2 and 3 are
also blocked for an HTTP component by the harness linker gap — see Task 4.) Add them only once you have
confirmed an install path that works on a clean runner; until then leave them out rather than
adding steps that will red-flag CI for the wrong reason.

- [ ] **Step 2: Write the README**

Cover: what this is (a forkable starting point for a custom RAG), the required exports and why,
how to run the gates locally, and a link to `/components/custom-rag/`. Keep it short — the page
is the tutorial, the README is the orientation.

- [ ] **Step 3: Create the remote and push**

```bash
gh repo create greenticai/greentic-example-rag --public \
  --description "Reference Greentic component: custom RAG over an HTTP retrieval service" \
  --source . --push
```

- [ ] **Step 4: Confirm CI is green**

```bash
gh run watch
```

Expected: green. If red, fix before tagging — the whole point of this repo is that its CI vouches
for the example.

- [ ] **Step 5: Tag**

```bash
git tag v0.1.0 && git push origin v0.1.0
```

---

## Task 7: Write the documentation page

**Files:**
- Create: `greentic-docs/src/content/docs/components/custom-rag.mdx`
- Modify: `greentic-docs/astro.config.mjs`

**Interfaces:**
- Consumes: the tagged repo from Task 6; all code blocks are copied from `v0.1.0`.
- Produces: the page at `/components/custom-rag/`, linked from Task 8's aside.

- [ ] **Step 1: Create a branch in greentic-docs**

```bash
cd greentic-docs
git checkout -b docs/custom-rag
```

- [ ] **Step 2: Write the page**

Frontmatter:

```mdx
---
title: Add a custom RAG
description: Plug your own retrieval stack into Greentic as a WASM component
---

import { Aside, Steps } from '@astrojs/starlight/components';
```

Then the ten sections from the spec, in order. Content requirements per section:

1. **Choose your path** — a three-row table: *you have an HTTP retrieval service* → Path 1;
   *you have retrieval logic in code* → Path 2; *you only have documents* → Path 3.
2. **The component contract** — the two required exports table (from Global Constraints), the
   world, and the three `describe()` fields with their consequences: `operation.id` becomes the
   tool name; an i18n-key-only `display_name.fallback` yields a synthesised generic description;
   a trivial `input.schema` means the model receives `{}`.
3. **Path 1** — the worked example. Quote `src/retrieve.rs` and the dispatcher branch from the
   tagged repo verbatim. Lead with the `greentic-component new` command from Task 1 Step 1, then
   show the descriptor export from Task 2 Step 3 as the thing the scaffold does not give you.
4. **Path 2** — shorter: same skeleton, but `retrieve` embeds the query and scores it against a
   corpus carried in config. Do not present `component-rag` as copyable; mention it only as
   logic reference, with its ABI caveat.
5. **Path 3** — point at Knowledge Base collections in the designer.
6. **Build, test, package** — `greentic-component build --manifest ... --no-flow` (explain why
   `--no-flow`: a required-no-default field breaks dev-flow generation), then `greentic-pack`
   (the pack CLI — `packc` is only the crate name) for the pack. State the harness limitation plainly as an `<Aside type="caution">`:
   as of CLI 1.3.0-research.1, a component that imports `greentic:http/http-client@1.1.0` cannot
   be instantiated by `greentic-component doctor`/`test`, and `greentic-component build` skips
   its describe artifacts — all three fail with `matching implementation was not found in the
   linker` because the local harness wires only the legacy runner-host HTTP surface. So for an
   HTTP component, local verification is `cargo test` (the retrieve logic) plus the export check
   (`wasm-tools component wit`); full instantiation is confirmed only when the pack runs in the
   runner. Do not tell the reader to run `greentic-component test` and expect a `{context,
   chunks}` result — it does not work for this component today.
7. **Use it: as a flow node** — the `schema_version: 2` example with the
   `greentic-example-rag.retrieve` key shape, and the warning that the key's component segment,
   the pack manifest id, and the `describe()` name must all be spelled identically.
8. **Use it: as an agentic-worker tool** — admin registration with role `agentic_worker`,
   `allowed_operations`, registry credentials for private OCI; then the pack requirement stated
   plainly.
9. **Limits you need to know** — all four limits from the spec, as an `<Aside type="caution">`.
10. **Secrets** — `greentic:secrets-store#get`, `secret_requirements` in the manifest,
    `greentic-secrets init --pack <PATH>`.

Every command shown must be one that was actually run in Tasks 1–5. If a command was never
executed, it does not go on the page.

- [ ] **Step 3: Add the sidebar entry**

In `astro.config.mjs`, inside the `Components` sidebar group, add in alphabetical position:

```js
{ label: 'Custom RAG', slug: 'components/custom-rag' },
```

- [ ] **Step 4: Build the docs**

```bash
npm run build
```

Expected: builds clean. Broken internal links fail the build, so this also validates the
cross-link added in Task 8 once that task lands.

- [ ] **Step 5: Commit**

```bash
git add src/content/docs/components/custom-rag.mdx astro.config.mjs
git commit -q -m "docs: how to add a custom RAG as a WASM component"
```

---

## Task 8: Close the knowledge-tier misconception

**Files:**
- Modify: `greentic-docs/src/content/docs/concepts/agent-knowledge.mdx`

**Interfaces:**
- Consumes: the page slug `/components/custom-rag/` from Task 7.

- [ ] **Step 1: Add the aside**

Insert after the intro paragraph, before `## How it works`:

```mdx
<Aside type="note" title="This tier is not pluggable">
The retrieval described on this page is Greentic's built-in knowledge backend. It is selected at
the runner build, not through a capability, so a third-party retrieval stack cannot replace it.
To plug in your own RAG, see [Add a custom RAG](/components/custom-rag/) — it enters as a flow
node or an agentic-worker tool instead.
</Aside>
```

- [ ] **Step 2: Build and verify the link resolves**

```bash
npm run build
```

Expected: builds clean, no broken-link error for `/components/custom-rag/`.

- [ ] **Step 3: Commit and open the PR**

```bash
git add src/content/docs/concepts/agent-knowledge.mdx
git commit -q -m "docs: mark the built-in knowledge tier as not pluggable"
git push -u origin docs/custom-rag
gh pr create --title "docs: how to add a custom RAG" \
  --body "Adds /components/custom-rag/ and marks the built-in knowledge tier as not pluggable. Reference component: https://github.com/greenticai/greentic-example-rag (v0.1.0)."
```

---

## Task 9: Runner load test (gate 6, best effort)

Gate 6 from the spec. Attempt it; if it proves expensive, report it as skipped rather than
quietly dropping it.

**Files:**
- Reference only: `greentic-runner/crates/greentic-runner-host/tests/pack_manifest.rs:1039`

- [ ] **Step 1: Read the existing proof**

Read `agentic_worker_component_invoker_lists_and_invokes` in
`greentic-runner-host/tests/pack_manifest.rs:1039-1090`. It loads a pack, lists operations
through `ComponentToolCatalog`, and invokes one. That is the shape to mirror.

- [ ] **Step 2: Decide and report**

If the example pack from Task 5 can be dropped into that harness without new infrastructure, do
it and record the result. If it needs fixture wiring beyond a copy, stop and report gate 6 as
not run, naming what blocked it.

Do not mark the plan complete with gate 6 silently unaddressed — an explicit "not run, because
X" is a valid outcome; silence is not.

---

## Follow-up issues to file (not part of this plan)

These surfaced during the audit and are real defects, but fixing them is out of scope:

1. `component-rag` and `components-public/crates/component-http` target ABI 0.6.1, which has no
   host. `component-http` is published to GHCR by CI regardless.
2. `packs/rag-cs` is invalid against `PackConfig` and its flow uses a node shape the parser
   rejects.
3. ~~`component-rag/Makefile:41` invokes `greentic-pack build`, a command that does not exist.~~
   **Retracted** — `greentic-pack` IS the correct binary name (`[[bin]]` in
   `greentic-pack/crates/packc/Cargo.toml:102`); `packc` is only the crate/lib name. The earlier
   audit misread the lib name as the binary name. The Makefile line is fine on the command name.
4. The `rust-wasi-p2-min` scaffold does not export `component-descriptor@0.6.0`, so every
   generated component fails runner introspection silently until the author adds it by hand.
5. `ComponentV0V6V0Pre::new` failure maps to `Ok(None)` (`pack.rs:3186-3189`), turning an ABI
   mismatch into "this component has no operations" with no diagnostic.
6. The `greentic-component test`/`doctor` local harness is out of sync with the runner. Its
   linker (`greentic-component/crates/greentic-component/src/test_harness/linker.rs:114-124`)
   wires only the legacy `greentic:host@1.0.0` runner-host HTTP surface, not the blessed
   `greentic:http/http-client@1.1.0` the runner provides. A component that follows the WIT's own
   "prefer dedicated host capability worlds" guidance therefore cannot be instantiated by the
   platform's own test harness — verified against both CLI 1.1.0 and 1.3.0-research.1. The
   harness should wire the dedicated host capability worlds (http-client, secrets 1.1) so
   components built against the current runner ABI can be tested locally.
7. `greentic-pack sign` (v1.3.0-research.2) signs only the standalone `dist/manifest.cbor`, not
   the `.gtpack` archive. `build` always writes a fresh unsigned manifest into the zip, so the
   produced `.gtpack` stays unsigned even after a successful `sign` + `verify` pass (`greentic-pack
   doctor <pack>.gtpack` reports "signature files missing"). `verify` also requires the **public**
   key (`--key <pub.pem>`), not the private signing key. Distribution docs must account for both.
