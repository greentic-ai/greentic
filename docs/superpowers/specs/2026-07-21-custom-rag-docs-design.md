# Custom RAG documentation for docs.greentic.ai — design

**Date:** 2026-07-21
**Status:** design approved, implementation not started
**Deliverable:** a public documentation page teaching an external partner how to plug their own
RAG into Greentic, plus a compiling reference component.

## Why

A partner needs to bring their own retrieval stack to Greentic. The commitment made to them is
"we give you a WASM capability, you make it work". Before writing that page we audited what the
platform actually supports today. Three findings changed the shape of this document, and all
three must be reflected in what we publish.

### Finding 1 — the built-in knowledge tier is not pluggable

`AgentConfig.knowledge` (`greentic-runner/crates/greentic-aw-runtime/src/config.rs:80`) drives
automatic per-turn retrieval, but its backend is closed:

- `KnowledgeVariant` is a single-variant enum — `{ Chronicle }`
  (`greentic-dw-providers/crates/greentic-dw-providers-common/src/knowledge.rs:26`)
- the backend is selected by `#[cfg(feature = "knowledge-chronicle")]` plus environment
  variables (`greentic-runner/crates/greentic-runner-host/src/runner/knowledge_mount.rs`)
- the seam is a Rust trait `Knowledge { ingest, search }`
  (`greentic-aw-runtime/src/knowledge.rs:92`), not a WIT interface

There is no `greentic:rag/*`, `greentic.cap.rag.*`, or `cap://dw.rag` capability anywhere in the
workspace. A third-party RAG therefore **cannot** replace the knowledge tier. It enters as a
flow node or as an agentic-worker tool.

### Finding 2 — there are two component ABIs, and only one has a host

| | ABI A | ABI B |
|---|---|---|
| Package / world | `greentic:component@0.6.0`, world `component` | `greentic:component@0.6.1`, world `component-v0-v6-v0` |
| Exports | `node` (`invoke(op, envelope)`), `component-descriptor` (`describe() -> list<u8>`) | `descriptor`, `runtime`, `qa`, `component-i18n` |
| Host in workspace | yes | **none** |

The runner instantiates `greentic:component/node@0.6.0`
(`greentic-runner-host/src/pack.rs:381`) and
`greentic:component/component-descriptor@0.6.0#describe` (`pack.rs:3186`). ABI B exports
`descriptor@0.6.1` — the instance names can never match. A repo-wide grep finds no host
importing the 0.6.1 interfaces.

Failure is silent on the describe path: `ComponentV0V6V0Pre::new` returning `Err` is mapped to
`Ok(None)` (`pack.rs:3186-3189`), so an ABI B component simply looks like it has no operations.
The invoke path fails loudly only at the end of the 0.6 → 0.5 → 0.4 → `component-runtime`
fallback chain (`pack.rs:454`).

Consequences for our own repos, which are **out of scope for this document but must be filed
separately**:

- `component-rag` and `components-public/crates/component-http` are on ABI B. `component-http`
  is published to GHCR by CI (`components-public/.github/workflows/ci.yml:41`) despite having no
  runtime.
- Messaging and event providers also declare ABI B, but they survive because they additionally
  export `schema-core-api@1.0.0`, which is what the host actually instantiates. Their 0.6.1
  exports are dead weight.
- `packs/rag-cs` is aspirational: its `pack.yaml` lacks the required `pack_id`, `kind`, and
  `publisher` fields of `PackConfig` (`greentic-pack/crates/packc/src/config.rs:18`), it has no
  `components/`, no `dist/`, no lockfile, and its flow uses a node shape the parser rejects
  (`greentic-flow/src/error.rs:140` requires `<component>.<operation>` keys).

Working references instead: `greentic-runner/tests/assets/component-v0-6-dummy/` (exercised by
`greentic-runner-host/tests/component_v06_introspection.rs`) and
`components-public/crates/component-pack2flow`.

### Finding 3 — admin registration does not reach the runtime

The Component tools tab (`tenant_component_tools`) is enforced in the designer only. The runner
builds its component-tool catalogue from the loaded pack
(`greentic-runner-host/src/runner/component_invoker.rs:114`) and never reads the admin feed —
`grep component-tools greentic-runner/crates` returns zero hits. The runner also holds no
registry credentials and performs no OCI pull for component tools.

Three further gaps on that rail:

- `component_ref` in the designer and the component id in the pack manifest must match exactly;
  nothing validates this, and a mismatch drops the tool with a warning (`tools.rs:106-110`)
- at runtime the tool's JSON schema comes from the pack manifest, not from the `describe()` the
  composer introspected; the composer's stored `ToolRef.input_schema` is ignored
- the designer test-chat cannot execute `component:` tools at all — it can only select them

No test covers the full registration → pull → runtime-dispatch chain.

## What we build

### Page

`greentic-docs/src/content/docs/components/custom-rag.mdx` → `/components/custom-rag/`, added to
the existing **Components** sidebar group in `astro.config.mjs`.

Sections:

1. **Choose your path** — decision table: you have an HTTP retrieval service / you have
   retrieval logic in code / you only have documents
2. **The component contract** — ABI A, the two required exports, the CBOR descriptor
3. **Path 1: wrap your HTTP service** — the primary worked example
4. **Path 2: retrieval inside the WASM** — shorter, embedding + similarity sketch
5. **Path 3: no code** — points at Knowledge Base collections in the designer
6. **Build, test, package**
7. **Use it: as a flow node**
8. **Use it: as an agentic-worker tool**
9. **Limits you need to know**
10. **Secrets**

### Edit to an existing page

`concepts/agent-knowledge.mdx` gains an `<Aside>` near the top stating that the tier it
describes is the built-in Chronicle backend and is not pluggable, linking to
`/components/custom-rag/`. Without this, a partner reads that page and reasonably concludes
their RAG can slot into it.

### Reference component

A new standalone public repository under the `greenticai` org — working name
**`greentic-example-rag`** — holding a complete, compiling ABI A component: `Cargo.toml`,
`wit/`, `src/lib.rs`, `component.manifest.json`, `README.md`. A partner forks it as their
starting point rather than copying code out of a web page.

It carries its own CI: build for `wasm32-wasip2`, then the export check and the harness run from
the Verification section. That CI is what keeps the example honest — if a future ABI change
breaks it, the build goes red instead of the example silently rotting.

Drift tradeoff, accepted knowingly: because the repository is separate, the MDX cannot quote its
files at build time. The page therefore inlines the code and links to the repository at a pinned
tag. The mitigation is that the repo's CI, not the page, is the source of truth for
"does this compile" — and the page is updated from a tag rather than from a moving branch.

Whether it is also added to this monorepo as a submodule is left to the implementation plan; it
is not required for the partner-facing goal.

## Contract details the page must state

The component exports **both**:

| Export | Purpose | Consumer |
|---|---|---|
| `greentic:component/component-descriptor@0.6.0#describe` → `list<u8>` CBOR | operation introspection | `pack.rs:3186`, designer picker |
| `greentic:component/node@0.6.0#invoke(op, envelope)` | execution | `pack.rs:381` |

World: `greentic:component/component@0.6.0`.

The CBOR payload follows `ComponentDescribe`
(`greentic-types/src/schemas/component/v0_6_0/describe.rs:34`). The designer reads three fields,
and each has a quality consequence worth stating as a requirement rather than a footnote:

- `operation.id` becomes the tool name
- `display_name.fallback` becomes the tool description; an i18n-key-only value yields a
  synthesised generic sentence
- `operation.input.schema` (a `SchemaIr`, not JSON Schema) becomes the tool parameters; a
  trivial schema means the model receives `{}`

### Commands, as verified

```
greentic-component new --name rag-partner --org <org> --operation retrieve \
  --http-client --secret-key RAG_API_KEY
greentic-component build --manifest ./component.manifest.json
greentic-component test --wasm ./component.wasm --op retrieve --input ./in.json \
  --secrets ./secrets.env
greentic-component doctor <wasm> --manifest ./component.manifest.json
greentic-pack build --in <DIR>
greentic-pack sign --pack <DIR> --key <ed25519.pem>
greentic-pack verify
```

**Correction (verified 2026-07-22 by building the crate):** the pack binary is **`greentic-pack`**,
not `packc`. `greentic-pack/crates/packc/Cargo.toml` sets `[package] name = "greentic-pack"`,
`[lib] name = "packc"`, and `[[bin]] name = "greentic-pack"` — so `packc` is only the crate/lib
name. The earlier draft's claim that the binary is `packc` (and that `component-rag/Makefile`'s
`greentic-pack build` is a non-existent command) was wrong and is retracted.

The test harness sandbox refuses HTTP by default. Separately — and more importantly for an HTTP
component — the `greentic-component` harness cannot instantiate a component importing
`greentic:http/http-client@1.1.0` at all (its linker wires only the legacy runner-host HTTP
surface), so `doctor`/`test` and the `build` describe step do not work for this component. Local
verification is `cargo test` + `wasm-tools component wit`; full instantiation is confirmed only
in the runner. See the plan's Task 4 and follow-up #6.

### Flow node shape

```yaml
schema_version: 2
nodes:
  rag_retrieve:
    rag-partner.retrieve:
      input: { query: "{{in.text}}", top_k: 5 }
```

### Agentic-worker tool

Register in the admin Component tools tab with role `agentic_worker`, optionally constraining
`allowed_operations`; add per-host registry credentials for a private OCI ref. Then, stated
plainly: the component must also be inside the `.gtpack` the runner loads, and `component_ref`
must equal the component id in that pack's manifest.

### Secrets

`greentic:secrets-store#get(key)`, declared as `secret_requirements` in
`component.manifest.json` (`greentic-types/src/secrets.rs:200`), populated by the operator via
`greentic-secrets init --pack <PATH>`.

## Limits the page states explicitly

1. A custom RAG cannot replace the agentic worker's built-in knowledge tier.
2. Admin registration affects the designer only; the component must ship inside the pack to be
   callable at runtime.
3. The schema the model sees at runtime comes from the pack manifest, not from `describe()`.
4. The designer test-chat can select but not execute `component:` tools.

## Verification

The worked example is verified before the page is finalised, in order. Gates 1–5 also become the
example repository's CI, so they keep running after the page ships:

1. `cargo build --target wasm32-wasip2 --release` passes
2. `greentic-component doctor` confirms the world and the embedded manifest
3. `greentic-component test --op retrieve` returns the expected output — this proves `invoke`
   runs, not merely that the code compiles
4. `wasm-tools component wit` literally lists `greentic:component/component-descriptor@0.6.0`
   **and** `greentic:component/node@0.6.0`. This is the gate `component-rag` fails, and it fails
   silently, so it is checked explicitly.
5. `greentic-pack build --in <DIR>` produces a `.gtpack`
6. If affordable without heavy infrastructure: load that pack in the runner and invoke the
   operation, mirroring `greentic-runner-host/tests/pack_manifest.rs:1039`
   (`agentic_worker_component_invoker_lists_and_invokes`)

Gates 1–5 run locally. If gate 6 proves expensive it is reported as skipped rather than quietly
dropped, and the page ships on gates 1–5.

Docs build: `npm run build` in `greentic-docs`, which also checks internal links.

## Out of scope

- Translations into the seven locales. English first; translations follow via `greentic-i18n`.
- Fixing `component-rag` / `component-http` to a live ABI.
- Fixing `packs/rag-cs`.
- Making the runner pull component tools from OCI, or making the knowledge tier pluggable.

The first three are follow-up issues to file; the fourth is a platform project, not a docs task.
