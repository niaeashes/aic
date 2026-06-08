# aic

A minimal interactive chat CLI for OpenAI-compatible `/chat/completions` endpoints
with MCP (Model Context Protocol, Streamable HTTP) tool calling. Written in Rust,
streaming-only display.

- **No provider abstraction** — talks directly to openai-compatible endpoints
- **MCP is Streamable HTTP only** — no stdio or socket transports
- **secrets via macOS Keychain + ChaCha20-Poly1305** — plaintext `env.json` must never be committed

See [SPEC.md](SPEC.md) for the full specification and [MILESTONES.md](MILESTONES.md)
for the milestone breakdown.

---

## From zero to first chat in 5 minutes

### 1. Build

```sh
cargo build --release
# Binary is at target/release/aic
```

### 2. Minimal config

For first-time setup, the interactive wizard is the easy path:

```sh
aic
aic> /config setup
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

ui:
  stream: true
  history_size: 1000
  max_tool_iterations: 10
```

`${VAR}` placeholders are resolved in order: **secrets map → process environment variables**.

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

On macOS you can seal it into `env.json.enc` and then delete the plaintext:

```sh
aic env seal
# -> generates ~/.config/aic/env.json.enc
# -> stores a 32-byte ChaCha20-Poly1305 key in macOS Keychain (service=aic, account=env-key)
rm ~/.config/aic/env.json
```

On subsequent startup, `env.json.enc` is decrypted with the Keychain key. If you
carry the key to another machine you can commit the sealed `env.json.enc` alone.
To edit, run `aic env unseal` to extract the plaintext back.

On non-macOS platforms (Linux etc.) the fallback chain is: `env.json` plaintext →
process environment variables. Keychain support is macOS-only.

### 4. Start chatting

```sh
aic
aic> Hello
assistant> Hi! How can I help you today?
aic> /exit
```

If you have MCP servers configured, tool calls are routed automatically:

```
aic> What's the weather today?
· tool call: tools__get_weather({"location":"Tokyo"})
✓ tool ok:   tools__get_weather
assistant> The weather in Tokyo is...
```

---

## REPL commands

| Command | Description |
|---|---|
| `/help` | List all registered commands |
| `/exit` | Quit (Ctrl-D also works) |
| `/clear` | Reset conversation history (keeps model selection / MCP connections) |
| `/model` | List configured groups/models. Current model marked with `*` |
| `/model use <group>:<model>` | Switch model (e.g. `/model use local:qwen2.5-coder:32b`) |
| `/config show` | Show current Settings as YAML (api_key / headers redacted) |
| `/config setup` | Interactively generate the home `config.yaml` |

---

## CLI

```sh
aic                       # Start the REPL
aic --config <path>       # Explicit path to config.yaml
aic env seal              # env.json -> env.json.enc (macOS)
aic env unseal            # env.json.enc -> env.json (macOS)
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
| `./aic.yaml` | Project-level override (top-level shallow merge) |

The project-level `aic.yaml` overlays the home config with **complete top-level
key replacement** (no element-wise merging within a key).

---

## Internal layout

```
src/
├── main.rs          Entry point: clap, tracing init
├── agent.rs         One-turn chat loop (assistant ↔ tool re-feed)
├── repl/            rustyline loop, dispatch
├── commands/        /exit /clear /help /model /config (auto-collected via inventory)
├── config/          Settings types, YAML loading, ${VAR} expansion, secrets / Keychain
├── llm/             ChatRequest, SSE parser, ChatClient
└── mcp/             JSON-RPC, Streamable HTTP transport, tool catalog
```

To add a new command, drop a file at `src/commands/<name>.rs` and add a single
`mod` line in `commands/mod.rs`. Dispatch / help / registration logic stays
untouched (SPEC §9.3).

---

## Installation

```sh
cargo install --git https://github.com/niaeashes/aic
```

Installs to `~/.cargo/bin/aic`. Requires a Rust toolchain (rustc 1.70+).
Because it uses macOS Keychain, `aic env seal/unseal` only works on macOS today.
Chat and MCP work on any platform — resolve `${VAR}` via plaintext `env.json` or
environment variables.

If `~/.cargo/bin` is not on your `PATH` yet (i.e. you didn't install Rust via
rustup), add it manually:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

Updating:

```sh
cargo install --git https://github.com/niaeashes/aic --force
```

## License

MIT License — see [LICENSE](LICENSE).
