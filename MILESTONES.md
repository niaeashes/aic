# aic — Milestones

Implementation plan for SPEC.md v0.1. Each milestone lists **deliverables**,
**definition of done (DoD)**, and **dependencies**, based on the phases in SPEC §14.

`cargo check` must pass at the end of every milestone (SPEC §14). Testing is
mostly "compiler + manual REPL run"; `cargo test` is reserved for type/parser
logic and other pure code.

Progress tracking: tick the checkboxes as you go. Done = `[x]`, in progress =
`[~]`, not started = `[ ]`.

---

## M0. Project initialization

Just the bare scaffold: SPEC §3 directory layout and SPEC §13 dependencies physically present.

**Deliverables**

- [x] `Cargo.toml` (package name `aic`, edition 2021, one binary)
- [x] SPEC §13 crates listed under `[dependencies]` (versions = current stable)
- [x] `.gitignore` (`/target`, `env.json`, `*.swp`, etc.)
- [x] `src/main.rs` — stub with just `fn main() { println!("aic"); }`
- [x] SPEC §3 directory skeleton created (`src/config/`, `src/llm/`, `src/mcp/`, `src/repl/`, `src/commands/`, `src/agent.rs`) with empty `mod.rs` files

**DoD**

- [x] `cargo build` succeeds
- [x] `cargo run` prints `aic` and exits

---

## M1. Skeleton — config loading + empty REPL

SPEC §14-1. Types, config loading, and the outermost REPL shell.

**Deliverables**

- [x] `config/mod.rs` — `Settings`, `ModelGroup`, `ModelRef`, `McpServerCfg`, `UiConfig` types (SPEC §4.2, §11)
- [x] `config/mod.rs` — YAML loading via `serde_yml`; two-layer shallow merge of home + project (SPEC §4.1)
- [x] `config/secrets.rs` — stub `Secrets` type (env-only version) and `${VAR}` expansion helper
- [x] `repl/context.rs` — `Session` / `ReplContext` (SPEC §11)
- [x] `repl/mod.rs` — Minimal `rustyline`-based loop. `/exit` exits, everything else echoes
- [x] `main.rs` — load config → build `ReplContext` → start the REPL
- [x] `clap` accepts `aic` / `aic --config <path>` (reserve `env seal/unseal` as subcommand names, `unimplemented!()` is fine)

**DoD**

- [x] Starts with default settings even when no home config exists
- [x] An `aic.yaml` in the working directory is merged on top (manual check)
- [x] `/exit` terminates cleanly

**Depends on**: M0

---

## M2. LLM chat — SSE streaming

SPEC §14-2, §6. Round-trip chat without tools.

**Deliverables**

- [x] `llm/types.rs` — `Message`, `Role`, `ToolCall`, `ChatRequest`, `Tool` (SPEC §6.2). `serde` serialization
- [x] `llm/stream.rs` — Iterator that SSE-parses with `eventsource-stream` and accumulates `delta.content` and `delta.tool_calls` (per-index concatenation)
  - The SPEC §6.1 gotchas (id/name only in the head fragment, arguments fragmented) are noted explicitly in the implementation comments
- [x] `llm/mod.rs` — `ChatClient::stream(request) -> impl Stream<Item = StreamEvent>` (or equivalent)
- [x] `agent.rs` — Skeleton `run_turn(ctx, input)`. Empty tools, just stream → push assistant message
- [x] REPL: input not starting with `/` goes to `run_turn`
- [x] Load `default_model` at startup and reflect it into `current_model`

**DoD**

- [x] Against a real endpoint (OpenAI or local ollama), user input → streaming response is shown incrementally
- [x] History (`session.messages`) accumulates across turns
- [x] Streams without any `delta.content` (pure tool_call responses) don't panic

**Depends on**: M1

---

## M3. Command infrastructure

SPEC §14-3, §9. Set up `inventory`-based auto-collection from the start (per the note at the end of SPEC §14).

**Deliverables**

- [x] `commands/mod.rs` — `Command` trait, `Outcome` enum, `inventory::collect!`
- [x] `repl/dispatch.rs` — Split `name`/`args` on the first whitespace; dispatch via `inventory` walk; unknown commands print an error and continue
- [x] `commands/exit.rs` — `/exit` reimplemented via `Command`
- [x] `commands/clear.rs` — `/clear`
- [x] `commands/help.rs` — Lists every registered command's `name()` / `help()`
- [x] REPL loop is refactored to forward `/`-prefixed input straight to `dispatch`

**DoD**

- [x] `/help` shows `/exit /clear /help` together
- [x] After `/clear`, history is reset (model selection is preserved)
- [x] Adding a new command takes "one file + one `mod` line in `commands/mod.rs`" — nothing more

**Depends on**: M2

---

## M4. Model switching

SPEC §14-4, §10. Support for multiple groups / models.

**Deliverables**

- [x] `ModelRef` uses `splitn(2, ':')` (so model names containing `:` work; SPEC §10)
- [x] `commands/model.rs` — `/model` (list), `/model use <group>:<model>`
- [x] `commands/config.rs` — `/config show` (API key / headers masked; SPEC §10)
- [x] `ChatClient` request construction pulls `base_url` / `api_key` / `headers` from the group of `current_model`
- [x] `Settings` lookup helpers: `group_by_name`, `model_exists(group, model)`

**DoD**

- [x] Multiple groups can be listed via `/model` after configuration
- [x] `/model use local:qwen2.5-coder:32b` and other model names containing `:` work
- [x] `/config show` redacts the API key to `***`
- [x] Unknown group/model surfaces with a clear error

**Depends on**: M3

---

## M5. Secrets

SPEC §14-5, §5. macOS Keychain + ChaCha20-Poly1305.

**Deliverables**

- [x] `config/secrets.rs` — `Secrets` resolves via the "secrets map → env vars" fallback (SPEC §5)
- [x] `${VAR}` expansion is applied to all relevant fields after config loading (`api_key`, `headers` values, `mcp_servers[].headers` values)
- [x] ChaCha20-Poly1305 seal/unseal (SPEC §5.3 `base64(nonce(12) || ciphertext_with_tag)` format)
- [x] `keyring` fetches/creates the 32-byte key at `service=aic, account=env-key`
- [x] `aic env seal` subcommand: `env.json` → `env.json.enc`, store key in Keychain (reuse if present)
- [x] `aic env unseal` subcommand: `env.json.enc` → `env.json`
- [x] On startup, attempt `env.json.enc` → on failure, warn and fall back to env vars (SPEC §5.2, §5.4)

**DoD**

- [x] On macOS, after `aic env seal`, deleting `env.json` still results in a successful decrypt + API key resolution on next startup
- [x] Manually deleting the Keychain key prints a warning and falls back
- [x] On non-macOS the resolution order is `env.json` plaintext → env vars

**Depends on**: M4

---

## M6. MCP — Streamable HTTP

SPEC §14-6, §7. Up to the tool catalog. The agent loop isn't wired yet.

**Deliverables**

- [x] `mcp/protocol.rs` — JSON-RPC 2.0 types; request/response types for `initialize` / `notifications/initialized` / `tools/list` / `tools/call`
- [x] `mcp/transport.rs` — One POST endpoint per request, branching on `Content-Type` (`application/json` is single-shot; `text/event-stream` reuses a thin SSE data extractor; SPEC §7.2)
  - `Accept: application/json, text/event-stream` header
  - `MCP-Protocol-Version`, `Mcp-Session-Id` handling (SPEC §7.1, §7.3)
- [x] `mcp/mod.rs` — `McpManager::connect_all(&Settings)` runs initialize → tools/list against every enabled server
- [x] Tool catalog: `HashMap<public name "<server>__<tool>", (server_idx, real tool name)>` (SPEC §7.4 — no string re-parsing)
- [x] `as_openai_tools()` — Generates the `tools` array for LLM requests
- [x] `call(public_name, args)` — Routes `tools/call` to the right server and returns the response
- [x] On startup, MCP connection failures are logged but startup continues

**DoD**

- [ ] Against a real MCP server (Streamable HTTP, static-header auth), `initialize` → `tools/list` succeeds (**awaiting manual verification** — once a real server is available)
- [x] Startup log lists the public tool names
- [x] Unit tests: `tools/list` responses parse in both SSE and single-JSON forms
- [x] Two servers with name-colliding tools are disambiguated by `<server>__<tool>`

**Depends on**: M5 (for `${MCP_TOKEN}` expansion)

---

## M7. Agent loop

SPEC §14-7, §8. Finally a "chat that calls tools".

**Deliverables**

- [x] `agent.rs::run_turn` full implementation (SPEC §8 steps 1–3)
  - Accumulate the assistant's `tool_calls` → push to the session
  - Execute each tool_call via `McpManager.call` → push as a `tool` message (`tool_call_id` required)
  - Done when tool_calls is empty
- [x] On hitting `ui.max_tool_iterations`, warn and abort (SPEC §8 end; avoid aichat-style runaway)
- [x] Tool result formatting: concatenate the MCP `content` array's `text` entries into the `tool` message's `content` (the concatenation is done by `McpManager.call` in M6; the agent just plugs the returned string in)
- [x] Tool execution errors (network / server) get an `"error: ..."` string in the tool result and the loop continues (so the model can see the failure)

**DoD**

- [ ] A prompt that triggers a single tool call walks the full LLM → tool call → result re-feed → final answer round trip (**awaiting manual verification** — real MCP server + LLM integration)
- [ ] Setting the loop cap to 2 and using a broken tool aborts with a warning and returns control to the REPL (**awaiting manual verification**)
- [x] Disabled servers (`enabled: false`) don't show up in `as_openai_tools()` (already filtered in M6's `connect_all`)

**Depends on**: M6

---

## M8. Polish

SPEC §14-8. Formatting and the last command for everyday use.

**Deliverables**

- [x] `/config setup` wizard added to `commands/config.rs`. Interactively asks about model groups / MCP servers and writes to the **home** `config.yaml` (SPEC §10, §4.1)
- [x] Streaming display polish (assistant label, tool-call start/end indicators)
- [x] Uniform error messages (a sweep over user-facing wording — `error:` / `warning:` prefixes consistent)
- [x] `tracing-subscriber` env filter (`RUST_LOG=aic=debug` for verbose logs; default `warn,aic=info`)
- [x] `rustyline` history file saved under the config directory (SPEC §9.1 — `config_dir/history.txt`, respecting `history_size`)
- [x] README.md (basic startup steps, `aic env seal` usage, sample config)

**DoD**

- [ ] In a clean environment, you can follow the README and get the first chat back in under 5 minutes (**awaiting manual verification**)
- [ ] Running `/config setup` on first boot generates `config.yaml` and the next startup picks it up (**awaiting manual verification**)
- [x] Non-zero exit is reserved for serious errors (normal REPL termination returns 0 — `/exit` / Ctrl-D / Ctrl-C all exit 0)

**Depends on**: M7

---

## Cross-cutting principles

- **File length**: Aim for ≤ 300 lines per file (SPEC §3). Split when you'd exceed it
- **Errors**: `anyhow::Result` everywhere (SPEC §11)
- **State**: No global `Arc<RwLock<_>>`. Pass `&mut ReplContext` instead (SPEC §11)
- **Lifetimes**: Don't carry lifetimes in structs. Hold owned `String`s (SPEC §11)
- **Streaming**: Don't write a non-streaming path (SPEC §1)
- **Provider abstraction**: openai-compatible only. Don't turn `ChatClient` into a trait (SPEC §1, §14)
