# aic — Specification (v0.1)

A minimal interactive chat CLI in Rust, inspired by aichat.

The LLM side speaks OpenAI-compatible APIs; tools come from MCP servers over
Streamable HTTP.

> `aic` is a working name. The binary name and config directory name can be
> renamed freely later.

---

## 1. Scope

**In scope**

- Connecting to OpenAI-compatible `/chat/completions` (SSE streaming only)

- Connecting to MCP servers over Streamable HTTP (static-header auth, or
  OAuth 2.1 with CIMD — §7.5)

- Bridging LLM `tool_call` ↔ MCP `tools/call` (the agent loop)

- Interactive REPL with slash commands

- Model switching via model groups

- Layered config (home + project working directory)

- Encrypted secrets on macOS (`env.json.enc` + Keychain)

**Out of scope**

- MCP stdio transport (HTTP only)

- OAuth Dynamic Client Registration (CIMD only — §7.5; an AS without CIMD
  support is a hard error)

- OAuth token persistence (tokens are in-memory only; every aic restart
  requires `/auth <server>` again)

- Non-streaming LLM paths (streaming-only)

- RAG / serve mode / web playground (the aichat features we don't carry over)

- Native provider implementations (Claude/Gemini-specific paths; openai-compatible only)

---

## 2. Terminology

| Term | Definition |

|---|---|

| model group | A single OpenAI-compatible endpoint configuration (base_url + api_key + headers) |

| model | A model ID exposed by a group (e.g. `gpt-4o`, `qwen2.5-coder:32b`) |

| model ref | A reference of the form `<group>:<model>`, e.g. `local:qwen2.5-coder:32b` |

| MCP server | A self-hosted tool server we connect to over Streamable HTTP |

| secrets | The map of secret values derived from `env.json`; expanded into `${VAR}` placeholders in the config |

---

## 3. Project layout

Keep each file at roughly 300 lines or fewer (so the LLM can hold a whole file
in context and so borrow-checker errors stay local).

```

src/

  main.rs              Entry point. Parses args, starts the REPL or env subcommand

  config/

    mod.rs             Settings, config.yaml loading, layer merging

    secrets.rs         env.json.enc decryption, Keychain access, ${VAR} expansion

  llm/

    mod.rs             ChatClient (request construction)

    stream.rs          SSE parsing, accumulating delta and tool_call fragments

    types.rs           Message / Role / ToolCall / ChatRequest etc.

  mcp/

    mod.rs             Module wiring + typed HttpError (401 detection)

    manager.rs         McpManager (tool catalog + dispatch + connect_all)

    server.rs          McpServer (one connection: transport + tools + OAuth state)

    connect.rs         initialize → tools/list sequence (OAuth-aware)

    transport.rs       Streamable HTTP transport (POST + response handling)

    protocol.rs        JSON-RPC types, initialize / tools-list / tools-call

    auth/

      mod.rs           TokenSet / ServerAuth / interactive_login (§7.5)

      discovery.rs     RFC 9728 / RFC 8414 metadata resolution

      flow.rs          PKCE, authorize-URL build, token exchange

      loopback.rs      RFC 8252 loopback redirect listener

  repl/

    mod.rs             REPL loop, line editing (rustyline)

    context.rs         ReplContext

    dispatch.rs        Command auto-collection and dispatch

  commands/

    mod.rs             Command trait, Outcome, `mod` declarations

    model.rs           /model

    config.rs          /config

    clear.rs           /clear

    exit.rs            /exit

  agent.rs             One-turn chat ↔ tool loop

build.rs               (optional) Scan commands/ and generate `mod` declarations

```

---

## 4. Configuration

### 4.1 File locations and layering

A two-layer setup where the later layer overrides the earlier:

1. **Home (global)**: `$AIC_CONFIG_DIR` || `~/.config/aic/config.yaml`

2. **Project working directory**: `./aic.yaml` in the current directory (if present)

Merge rule: **shallow replacement keyed on top-level keys**. Any key that exists
in the project layer wholesale replaces the home layer's value (no element-wise
merging of lists). Predictable and trivial to implement.

`/config setup` writes to the home layer. The project layer must be created by hand.

#### Project config trust (§4.3)

The project layer is **trust-gated** (`config/trust.rs`). A checked-in `aic.yaml`
can point `base_url`/`headers` at an arbitrary endpoint and have `${VAR}` expanded
against the sealed secrets map — i.e. running `aic` inside a hostile repo could
exfiltrate keyring secrets. To prevent this, a project config is applied only
after the user approves it, keyed by **absolute path + content hash** (so editing
the file forces re-approval), recorded in `<config_dir>/trusted_projects.json`.

- Trusted (path+hash match): applied silently.
- Untrusted, interactive stdin: prompt showing the overridden top-level keys
  (flagging `model_groups`/`mcp_servers` as sensitive); on `y` record + apply,
  otherwise ignore.
- Untrusted, non-interactive stdin (CI/pipe): ignored with a warning (fail-safe).

The hash is a non-cryptographic change detector (SipHash via `DefaultHasher`),
not an integrity guarantee — the trust file lives in the user's own config dir,
and the real safeguard is the human review at approval time.

### 4.2 config.yaml schema

```yaml

default_model: openai:gpt-4o-mini   # model ref used at startup

system_prompt: You are helpful.     # optional; first message of every fresh turn

generation:                          # optional; omitted keys keep server defaults

  temperature: 0.7

  max_tokens: 2048

model_groups:

  - name: openai

    base_url: https://api.openai.com/v1

    api_key: ${OPENAI_API_KEY}      # ${VAR} is resolved from secrets / environment

    headers: {}                      # Arbitrary extra headers

    models: [gpt-4o, gpt-4o-mini]

  - name: local

    base_url: http://ollama.ts.net:11434/v1

    models: [qwen2.5-coder:32b]      # api_key is optional

mcp_servers:

  - name: tools

    url: https://mcp.internal.ts.net/mcp

    headers:

      Authorization: Bearer ${MCP_TOKEN}

    enabled: true

  - name: oauth-tools                  # OAuth 2.1 + CIMD instead of static headers (§7.5)

    url: https://mcp.example.com/mcp

    auth:

      client_id: https://<user>.github.io/aic-client.json   # hosted CIMD document URL

      scopes: [mcp.read]               # optional; discovery defaults otherwise

ui:

  history_size: 1000

  max_tool_iterations: 10            # Upper bound on the agent loop

```

Streaming is always on (`ChatRequest.stream: true`); it is not a config key.

YAML deserialization uses `serde_yml` (since `serde_yaml` is archived; we use the
maintained fork).

---

## 5. Secrets management

`${VAR}` placeholders in the config are resolved in the order **secrets map →
process environment variables**.

### 5.1 Files

- `env.json` (plaintext, gitignored) — edited by the user

  ```json

  { "OPENAI_API_KEY": "sk-...", "MCP_TOKEN": "tskey-..." }

  ```

- `env.json.enc` (encrypted, commit-safe) — read at runtime

Both files live in the home config directory.

### 5.2 macOS flow

1. When `aic env seal` runs:

   - Generate a 32-byte random key and **store it in Keychain** (`keyring` crate;
     service=`aic`, account=`env-key`). Reuse an existing key if present.

   - Encrypt `env.json` with ChaCha20-Poly1305 and write `env.json.enc`.

2. When `aic` starts:

   - Fetch the key from Keychain → decrypt `env.json.enc` → hold the secrets map in memory.

   - Decryption failure (no key etc.) emits a warning and falls back to environment variables.

### 5.3 File format

`env.json.enc` = `base64( nonce(12B) || ciphertext_with_tag )`.

### 5.4 Linux / other platforms

**Linux** uses Secret Service over D-Bus as the keyring backend (via the
`sync-secret-service` feature of the `keyring` crate). Any Secret Service
provider works: `gnome-keyring`, `kwallet`, KeePassXC, etc. The on-disk format
and the 32-byte key length match macOS exactly. Once a provider is running,
`aic env seal/unseal` behaves identically to macOS.

If no Secret Service is reachable at startup, aic emits a warning that includes
setup hints and falls back to plaintext `env.json` (if present) or environment
variables.

**Other platforms (Windows, BSD)**: the system keyring path is not wired up.
aic still loads `env.json` plaintext or process environment variables, and the
REPL / MCP paths work normally. `aic env seal/unseal` returns a clear error
asking for a supported platform.

Across all platforms, `/doctor` (a REPL command) reports the keyring state,
config presence, MCP connections, and what to do next when something is off.

---

## 6. LLM connection (openai-compatible)

- `POST {base_url}/chat/completions`, `stream: true` fixed.

- Headers: `Authorization: Bearer {api_key}` (when api_key is set) + the group's `headers`.

- Request body: `model`, `messages`, `tools` (from MCP; omitted if empty), `stream: true`.

### 6.1 SSE parsing (`llm/stream.rs`)

Use `eventsource-stream` over reqwest's byte stream to split lines, then parse
each `data:` line's JSON incrementally:

- `choices[0].delta.content` → write to terminal incrementally and accumulate.

- `choices[0].delta.tool_calls` → **accumulate per `index`**. `id` and
  `function.name` arrive in the first fragment only; `function.arguments`
  (a JSON string) is split across multiple fragments and must be concatenated.
  This is a common source of bugs — implement it explicitly.

- `data: [DONE]` ends the stream.

### 6.2 Message types

OpenAI-compatible. `role` is one of `system` / `user` / `assistant` / `tool`.
`assistant` carries `tool_calls`; `tool` carries `tool_call_id` and `content`.

---

## 7. MCP connection (Streamable HTTP)

JSON-RPC 2.0. The MVP uses POST only — no server-originated GET streams or
server-originated requests.

### 7.1 Common request shape

`POST {server.url}` with a JSON-RPC payload. Headers:

- `Content-Type: application/json`

- `Accept: application/json, text/event-stream`

- The config's `headers` (static tokens etc.)

- `MCP-Protocol-Version: <version we target>` (on all requests after initialize)

- `Mcp-Session-Id: <value>` (only if the server issued one in its initialize response)

### 7.2 Response handling

Branch on `Content-Type`:

- `application/json` → parse as a single JSON-RPC response.

- `text/event-stream` → parse SSE and extract the JSON-RPC response with the
  matching `id` (the §6.1 parser is reused).

### 7.3 Lifecycle

Once per connection:

1. `initialize` → remember the `Mcp-Session-Id` from the response headers and the protocol version.

2. `notifications/initialized` (notification; no response).

3. `tools/list` → cache the tool definitions.

When invoking a tool: `tools/call` with `name` and `arguments`.

### 7.4 Tool catalog and naming

To avoid name collisions across servers, the name exposed to the LLM is
`<server>__<tool>`. Reverse lookup is via `HashMap<public_name, (server_idx,
real_tool_name)>` — never parse the string back.

`McpManager` owns this catalog. `as_openai_tools()` builds the `tools` array
for LLM requests; `call(public_name, args)` routes to the right server.

### 7.5 OAuth authorization (CIMD)

Servers with an `auth:` block authenticate via OAuth 2.1 using **CIMD**
(Client ID Metadata Documents, draft-ietf-oauth-client-id-metadata-document,
as adopted by the MCP authorization spec): `auth.client_id` is the HTTPS URL
of a **user-hosted JSON metadata document** (GitHub Pages works), and that URL
*is* the OAuth `client_id` — the authorization server fetches it. There is no
Dynamic Client Registration and no client secret (public client + PKCE S256).

**Tokens are in-memory only.** Nothing is persisted; startup never opens a
browser. An `auth:`-configured server is skipped at startup with a
"run `/auth <name>`" notice, and `/auth <name>` is the single entry point into
the interactive flow.

Flow (`mcp/auth/`):

1. **Discovery** — unauthenticated POST to the server harvests the 401
   `WWW-Authenticate: Bearer resource_metadata="…"` pointer (RFC 9728); the
   well-known locations (path-inserted first) are the fallback. The PRM's
   `authorization_servers[0]` then resolves to AS metadata via RFC 8414
   (`oauth-authorization-server{path}`, then OIDC discovery forms).

2. **Hard requirements** — the AS must advertise `S256` in
   `code_challenge_methods_supported` and
   `client_id_metadata_document_supported: true`. Either missing → hard error
   (there is no DCR fallback; the message says to use static `headers` instead).

3. **Authorization** — PKCE S256 + random `state`; redirect URI is a loopback
   listener `http://127.0.0.1:<ephemeral port>/callback` (RFC 8252 — literal
   `127.0.0.1`, never `localhost`; the AS must ignore the port when matching).
   The URL is always printed before the browser is launched (`open`/`xdg-open`,
   best-effort). The callback wait is raced against Ctrl-C and a 300 s timeout
   (commands are not raced by the REPL loop, so the race lives in
   `interactive_login`). The accept loop 404s anything that isn't
   `GET /callback` (favicon, speculative connections). A `state` mismatch or an
   `error=` callback aborts without exchanging the code.

4. **Tokens** — the RFC 8707 `resource` parameter (the canonicalized server
   URL: lowercase scheme/host, default port dropped, fragment stripped) is sent
   on the authorization, exchange, and refresh requests. Scope precedence:
   config `scopes` → `WWW-Authenticate` `scope` hint → PRM `scopes_supported` →
   omitted. `expires_at` is skew-adjusted by −60 s; a missing `expires_in`
   disables proactive refresh.

5. **At runtime** — the access token rides as `Authorization: Bearer …`,
   applied *after* the config `headers` so a stale static `Authorization`
   header can't clobber it (configuring both warns once). Before each
   `tools/call` the token is refreshed if expired; a 401 mid-session triggers
   one forced refresh + retry (typed `HttpError` in `mcp/mod.rs` makes the 401
   detectable through anyhow). A rejected refresh (`invalid_grant`) tells the
   user to run `/auth <name>` again.

The hosted CIMD document must list `token_endpoint_auth_method: "none"`,
`redirect_uris` containing `http://127.0.0.1/callback` (no port), and a
`client_id` equal to its own URL. README carries a copy-paste example.

---

## 8. Agent loop (`agent.rs`)

`run_turn(ctx, user_input)`:

1. If the session is empty (startup / after `/clear`) and `system_prompt` is set,
   push it as the leading `system` message.

2. Push the `user` message to the session.

3. Loop (up to `ui.max_tool_iterations` times):

   1. Build the request (messages + tools + current_model + `generation.*`).

   2. Run the stream. Display assistant text incrementally; accumulate tool_calls.

   3. Push the `assistant` message (with tool_calls) to the session.

   4. If tool_calls is empty → done, exit the loop.

   5. Execute each tool_call via `McpManager.call` → push the result as a `tool` message.

   (numbered 1–5 as above)

4. If the cap is reached, warn the user and abort (avoid aichat's unbounded recursion).

---

## 9. REPL and command system

### 9.1 REPL loop

- Line editing and history come from `rustyline`. The history file lives in the config directory.

- If input starts with `/` → dispatch as a command. Otherwise → `run_turn` for chat.

- A chat turn is raced against `Ctrl-C` (`tokio::select!`); interrupting drops the
  in-flight turn and returns to the prompt. Because a cancel can land between an
  `assistant` tool-call request and its `tool` results, the message log is then
  repaired (drop trailing `tool` messages and a dangling assistant-with-tool_calls)
  so the next turn starts from a server-valid boundary. `Ctrl-D` exits.

- The shared `reqwest::Client` sets a **connect timeout** but no whole-request
  timeout — a total deadline would truncate long streamed responses.

### 9.2 Command trait (`commands/mod.rs`)

```rust

#[async_trait::async_trait]

pub trait Command: Sync + Send {

    fn name(&self) -> &'static str;   // "model" → invoked as "/model"

    fn help(&self) -> &'static str;   // One-line help

    async fn run(&self, args: &str, ctx: &mut ReplContext)

        -> anyhow::Result<Outcome>;   // args is whatever follows "/<name> "

}

pub enum Outcome { Continue, Exit }

inventory::collect!(&'static dyn Command);

```

- `async-trait` enables using async trait methods as trait objects.

- `inventory` auto-collects commands at compile time. No central `match` block.

### 9.3 Adding a new command

Drop a single file:

```rust

// src/commands/clear.rs

use super::*;

struct Clear;

#[async_trait::async_trait]

impl Command for Clear {

    fn name(&self) -> &'static str { "clear" }

    fn help(&self) -> &'static str { "Clear the conversation history" }

    async fn run(&self, _args: &str, ctx: &mut ReplContext)

        -> anyhow::Result<Outcome>

    {

        ctx.session.messages.clear();

        println!("Conversation history cleared");

        Ok(Outcome::Continue)

    }

}

inventory::submit! { &Clear as &dyn Command }

```

The only central edit left is one `mod clear;` line in `commands/mod.rs`. If you
hate even that, a `build.rs` can scan `commands/*.rs` and generate the `mod`
lines for you (optional). The dispatch / help / registration logic never has
to be touched.

### 9.4 Dispatch (`repl/dispatch.rs`)

Split input on the first whitespace into `name` and `args`. Look up `name()`
across the inventory-collected commands and call `run`. Unknown commands print
an error and return `Continue`.

---

## 10. Command behavior

| Command | Behavior |

|---|---|

| `/model use <group>:<model>` | Switch the current model. `<group>` and `<model>` are split **at the first `:`, exactly once** (the model name itself may contain `:`, so use `splitn(2, ':')`). Unknown group/model is an error. |

| `/config show` | Print the merged effective config. Secrets inside `api_key` / `headers` are redacted to `***`. |

| `/config setup` | Interactive wizard. Asks about model groups / MCP servers and writes the result to the home `config.yaml`. |

| `/auth` | List OAuth-configured MCP servers and their in-memory token state. `/auth <name>` runs the §7.5 browser flow and (re)connects the server. `/auth logout <name>` drops the tokens and the server's tools. |

| `/clear` | Clear the session message history (model selection and MCP connections are preserved). |

| `/exit` | Return `Outcome::Exit` to end the REPL. |

As helpers (optional): `/model` with no arguments lists groups and models;
`/help` lists every command's help text.

---

## 11. Runtime state model

No global `Arc<RwLock<…>>`. The REPL is single-threaded; state is passed
explicitly via ownership and `&mut`.

```rust

struct Settings {        // Loaded at startup, effectively immutable

    default_model: ModelRef,

    model_groups: Vec<ModelGroup>,

    mcp_servers:  Vec<McpServerCfg>,

    ui: UiConfig,

    config_dir: PathBuf,

}

struct Session {         // Mutated via &mut

    messages: Vec<Message>,

}

struct ReplContext {

    settings: Settings,

    session:  Session,

    http:     reqwest::Client,

    mcp:      McpManager,      // Live connections + tool catalog

    secrets:  Secrets,

    current_model: ModelRef,

}

```

Commands and `agent.rs` receive `&mut ReplContext`. Structs do not carry
lifetimes (own `String` instead of holding `&str`). Errors are uniformly
`anyhow::Result`.

---

## 12. CLI subcommands (non-REPL)

A minimal `clap` surface:

| Invocation | Behavior |

|---|---|

| `aic` | Start the REPL |

| `aic env seal` | Encrypt `env.json` → `env.json.enc`; store the key in Keychain |

| `aic env unseal` | Decrypt `env.json.enc` back to `env.json` (for editing; optional) |

| `aic --config <path>` | Explicit config path (optional) |

---

## 13. Crate list

| Crate | Purpose |

|---|---|

| `tokio` | Async runtime |

| `reqwest` (`stream`, `json`, `rustls-tls`) | HTTP client |

| `eventsource-stream` | SSE parsing (reused by both LLM and MCP paths) |

| `serde`, `serde_json`, `serde_yml` | Serialization (config is YAML) |

| `clap` (`derive`) | Argument parsing |

| `rustyline` | REPL line editing and history |

| `anyhow` | Error handling |

| `async-trait` | Object safety for the Command trait |

| `inventory` | Compile-time command registration |

| `chacha20poly1305` | AEAD encryption for `env.json` |

| `keyring` | macOS Keychain access |

| `base64`, `rand` | Encoding and nonce/key bytes (also PKCE verifier/state) |

| `sha2` | PKCE S256 challenge hashing |

| `url` | Authorize-URL construction, resource canonicalization (already in the tree via reqwest) |

| `directories` (or hand-written) | Resolving home/config directories |

| `tracing`, `tracing-subscriber` | Logging |

---

## 14. Implementation phases (for vibe coding)

Every phase must end with a passing `cargo check`. The compiler is the test suite.

1. **Skeleton** — Module layout, `Settings`/`Session`/`ReplContext` types,
   config.yaml loading and merging. `aic` starts an empty REPL (`/exit` is the
   only working command).

2. **LLM chat** — SSE connection to an openai-compatible endpoint. `run_turn`
   works without tools. A single hard-coded model is fine.

3. **Command infrastructure** — `Command` trait + `inventory` dispatch.
   Implement `/clear`, `/exit`, `/help`.

4. **Model switching** — Multi-group support via `model_groups`, `/model use`,
   `/model` listing, `/config show`.

5. **Secrets** — `env.json` / `env.json.enc` / Keychain, `aic env seal`,
   `${VAR}` expansion.

6. **MCP** — Streamable HTTP transport, initialize → tools/list, `McpManager`,
   tool catalog.

7. **Agent loop** — Bridging `tool_call` ↔ `tools/call`, the
   `max_tool_iterations` cap.

8. **Polish** — `/config setup` wizard, streaming display formatting,
   error message uniformity.

Until a second provider shows up, the macro-style abstraction in §9 isn't
needed. Commands can use `inventory` from the start (it's a requirement).

---

## 15. Future extensions (out of scope but the design accommodates them)

- Native providers (Claude/Gemini) — Turn `ChatClient` into a trait when this is needed.

- MCP stdio transport — Add another implementation in `mcp/transport.rs`.

- Session persistence (save/restore JSON files) — Make `Session` serde-aware.

- OAuth token persistence — Encrypt a token file with the existing
  `config/secrets/crypto.rs` machinery (today tokens are in-memory only, §7.5).
