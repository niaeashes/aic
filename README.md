# aic

A minimal interactive chat CLI for OpenAI-compatible `/chat/completions` endpoints
with MCP (Model Context Protocol, Streamable HTTP) tool calling. Written in Rust,
streaming-only display.

- **No provider abstraction** — talks directly to openai-compatible endpoints
- **MCP is Streamable HTTP only** — no stdio or socket transports
- **Sealed secrets via the system keyring (macOS Keychain / Linux Secret Service) + ChaCha20-Poly1305** — plaintext `env.json` must never be committed

See [SPEC.md](SPEC.md) for the full specification.

---

## From zero to first chat in 5 minutes

### 1. Install

```sh
cargo install --git https://github.com/niaeashes/aic
```

Installs to `~/.cargo/bin/aic` (re-run with `--force` to update). If `~/.cargo/bin`
is not on your `PATH` yet, add it: `export PATH="$HOME/.cargo/bin:$PATH"`.

Or build from a checkout:

```sh
cargo build --release
# Binary is at target/release/aic
```

### 2. Minimal config

For first-time setup, the interactive wizard is the easy path:

```sh
aic
aic [a3f2c1]> /config setup
```

You will be asked, in order:

1. **model group** (group name / `base_url` / `api_key` (`${VAR}` recommended) / models)
2. **default_model**, picked by number from the list
3. (optional) MCP server (name / url / Authorization header)
4. `ui.max_tool_iterations`

The file is written to `~/.config/aic/config.yaml` (override with the
`AIC_CONFIG_DIR` env var or `--config <path>`).

If you'd rather hand-edit instead of running the wizard:

```yaml
# ~/.config/aic/config.yaml
default_model: openai:gpt-4o-mini

# Optional: prepended as the system message of every fresh conversation
# (re-applied after /clear).
system_prompt: You are a concise, helpful assistant.

# Optional: forwarded to /chat/completions. Omitted keys keep the server default.
generation:
  temperature: 0.7
  max_tokens: 2048

model_groups:
  - name: openai
    base_url: https://api.openai.com/v1
    api_key: ${OPENAI_API_KEY}
    models:
      - gpt-4o-mini
      - gpt-4o

  - name: local
    base_url: http://127.0.0.1:11434/v1
    # ollama needs no auth, so api_key is omitted
    models:
      - qwen2.5-coder:32b

mcp_servers:
  - name: tools
    url: https://example.com/mcp
    headers:
      Authorization: Bearer ${MCP_TOKEN}
    enabled: true

  # Or OAuth 2.1 + CIMD instead of a static token (see "MCP OAuth" below)
  - name: oauth-tools
    url: https://mcp.example.com/mcp
    auth:
      client_id: https://<you>.github.io/aic-client.json

ui:
  history_size: 1000
  max_tool_iterations: 10
```

`${VAR}` placeholders are resolved in order: **secrets map → process environment
variables**. Use `$$` for a literal `$`; unresolved placeholders are left as-is.

### 3. Secrets

Create `~/.config/aic/env.json` and put the keys you reference via `${VAR}`:

```json
{
  "OPENAI_API_KEY": "sk-...",
  "MCP_TOKEN": "tskey-..."
}
```

**Never commit the plaintext `env.json`.** It is already in `.gitignore`, but
double-check before pushing.

On macOS and Linux you can seal it into `env.json.enc` and then delete the plaintext:

```sh
aic env seal
# -> generates ~/.config/aic/env.json.enc
# -> stores a 32-byte ChaCha20-Poly1305 key in the system keyring
#    (macOS Keychain / Linux Secret Service; service=aic, account=env-key)
rm ~/.config/aic/env.json
```

On subsequent startup, `env.json.enc` is decrypted with the keyring key. If you
carry the key to another machine you can commit the sealed `env.json.enc` alone.
To edit, run `aic env unseal` to extract the plaintext back.

At startup, secrets are resolved in order: `env.json.enc` (decrypted with the
keyring key) → plaintext `env.json` → process environment variables. Any failure
prints a warning and falls through to the next source — startup never blocks.
On platforms without keyring support (Windows, BSD) only the last two apply.

### 4. Start chatting

```sh
aic
aic [a3f2c1]> Hello
assistant> Hi! How can I help you today?
aic [a3f2c1]> /exit
```

The prompt shows the id of the current **session** (= one conversation history).
`/session new` starts a fresh conversation while keeping the old one around;
`/session use <id>` switches back (a unique id prefix is enough). Sessions are
in-memory only — they are gone when aic exits.

If you have MCP servers configured, tool calls are routed automatically:

```
aic [a3f2c1]> What's the weather today?
· tool call: tools__get_weather({"location":"Tokyo"})
✓ tool ok:   tools__get_weather
assistant> The weather in Tokyo is...
```

Press **Ctrl-C** during a response to interrupt the current turn (the generation
or a stuck tool call) and return to the prompt; the conversation history is left
in a consistent state. **Ctrl-D** quits.

---

## MCP OAuth (CIMD)

For MCP servers that speak the MCP authorization spec, aic supports OAuth 2.1
via **CIMD** (Client ID Metadata Documents) instead of static headers — no
Dynamic Client Registration, no client secret (public client + PKCE).

With CIMD, the OAuth `client_id` is the URL of a small JSON document **you
host** describing the client. Put this on GitHub Pages (raw.githubusercontent.com
serves `text/plain`, which some authorization servers reject — Pages serves
proper `application/json`):

```json
{
  "client_id": "https://<you>.github.io/aic-client.json",
  "client_name": "aic",
  "redirect_uris": ["http://127.0.0.1/callback"],
  "grant_types": ["authorization_code", "refresh_token"],
  "response_types": ["code"],
  "token_endpoint_auth_method": "none"
}
```

`client_id` inside the document must equal the URL it is hosted at, and that
same URL goes into `mcp_servers[].auth.client_id` in your config.

Then, inside the REPL:

```
aic [a3f2c1]> /auth oauth-tools
auth: open this URL to log in (if the browser didn't launch):
  https://as.example/authorize?response_type=code&...
auth: exchanging authorization code ...
Connected to oauth-tools (3 tools): search, fetch, write
```

Notes:

- **Tokens live in memory only.** Nothing is written to disk; after restarting
  aic, run `/auth <name>` again. Within a session, expired tokens are refreshed
  automatically (including one retry when the server answers 401).
- Startup never opens a browser: OAuth servers are skipped with a
  "run `/auth <name>`" notice until you log in.
- `/auth` lists OAuth servers and their token state; `/auth logout <name>`
  drops the tokens and the server's tools.
- The authorization server must advertise `client_id_metadata_document_supported:
  true` and PKCE `S256`; otherwise aic refuses (use static `headers` instead).

---

## REPL commands

| Command | Description |
|---|---|
| `/help` | List all registered commands |
| `/exit` | Quit (Ctrl-D also works) |
| `/clear` | Reset the current session's history (keeps the session id / model selection / MCP connections) |
| `/session` | List in-memory sessions. Current marked with `*` |
| `/session new` | Start a fresh session (new id); the old one stays switchable |
| `/session use <id>` | Switch session (`<id>` may be a unique prefix) |
| `/model` | List configured groups/models. Current model marked with `*` |
| `/model use <group>:<model>` | Switch model (e.g. `/model use local:qwen2.5-coder:32b`) |
| `/config show` | Show current Settings as YAML (api_key / headers redacted) |
| `/config setup` | Interactively generate the home `config.yaml` |
| `/auth` | OAuth login for MCP servers: list state / `/auth <name>` / `/auth logout <name>` |
| `/doctor` | Run environment checks (config / keyring / MCP) with setup hints |

---

## CLI

```sh
aic                       # Start the REPL
aic --config <path>       # Explicit path to config.yaml
aic --version             # Print version
aic env seal              # env.json -> env.json.enc (macOS / Linux)
aic env unseal            # env.json.enc -> env.json (macOS / Linux)
```

### Environment variables

- `RUST_LOG` — `tracing` filter. For verbose logs: `RUST_LOG=aic=debug aic`
- `AIC_CONFIG_DIR` — Directory to use instead of `~/.config/aic`
- `HOME` — Used to resolve the default config directory

---

## File layout

| Path | Purpose |
|---|---|
| `~/.config/aic/config.yaml` | Home config (default) |
| `~/.config/aic/env.json` | Plaintext secrets. **Must not be committed** |
| `~/.config/aic/env.json.enc` | Sealed secrets. Commit-safe |
| `~/.config/aic/history.txt` | rustyline input history |
| `~/.config/aic/trusted_projects.json` | Approved project configs (path + content hash) |
| `./aic.yaml` | Project-level override (top-level shallow merge, **trust-gated**) |

The project-level `aic.yaml` overlays the home config with **complete top-level
key replacement** (no element-wise merging within a key).

### Project config trust

Because a project `aic.yaml` can redirect `base_url`/`headers` and have
`${VAR}` expanded against your sealed secrets, a checked-in config is a
credential-exfiltration vector. So the first time aic sees a project config (and
again whenever its contents change) it shows what the file overrides and asks for
approval — the same model as `direnv`:

```
⚠ Untrusted project config detected: /path/to/aic.yaml
  sets top-level keys: model_groups, ui
  ⚠ model_groups can redirect requests and expand ${SECRETS} into arbitrary URLs/headers
Trust this project config? [y/N]:
```

Approvals are recorded in `trusted_projects.json`. In a non-interactive context
(piped stdin / CI) an unapproved project config is **ignored** with a warning —
run aic interactively in that directory once to approve it.

---

## Internal layout

```
src/
├── main.rs          Entry point: clap, tracing init
├── agent.rs         One-turn chat loop (assistant ↔ tool re-feed)
├── repl/            rustyline loop, dispatch
├── commands/        /exit /clear /session /help /model /config /auth (auto-collected via inventory)
├── config/          Settings types, YAML loading, ${VAR} expansion, secrets / keyring
├── llm/             ChatRequest, SSE parser, ChatClient
└── mcp/             JSON-RPC, Streamable HTTP transport, tool catalog, OAuth (CIMD)
```

To add a new command, drop a file at `src/commands/<name>.rs` and add a single
`mod` line in `commands/mod.rs`. Dispatch / help / registration logic stays
untouched (SPEC §9.3).

---

## Prerequisites

aic itself only needs the Rust toolchain (rustc 1.70+) for `cargo install`.
Sealed secrets (`aic env seal`) need a system keyring; chat and MCP work without one.

### macOS
Nothing extra — the built-in Keychain is used.

### Linux
For sealed secrets, install a Secret Service provider on D-Bus:

- **GNOME / KDE / XFCE**: usually pre-installed (`gnome-keyring` / `kwallet`)
- **Minimal WMs (sway, Hyprland, i3, etc.)**:
  ```sh
  sudo pacman -S gnome-keyring   # or your distro's package manager
  # Then enable PAM auto-unlock, or start it from your session config:
  exec gnome-keyring-daemon --start --components=secrets
  ```
  Quick check it's up: `busctl --user list | grep secret`

Without a Secret Service, aic falls back to plaintext `env.json` or environment
variables. Run `aic` then `/doctor` to see exactly what's wired up.

### Other platforms (Windows, BSD)
Chat and MCP work; sealed secrets do not. Use plaintext `env.json` or env vars.

## License

MIT License — see [LICENSE](LICENSE).
