# CLAUDE.md

`aic` — minimal interactive chat CLI (Rust) for OpenAI-compatible
`/chat/completions` endpoints with MCP tool calling.

[SPEC.md](SPEC.md) is the authoritative specification; [README.md](README.md) is
the user-facing docs. When you change behavior, update both in the same change.

## Commands

- Build: `cargo build` — release builds use `strip`/`lto`, so prefer debug builds while iterating
- Test: `cargo test` — unit tests live inline in each module under `#[cfg(test)]`
- Verbose logs: `RUST_LOG=aic=debug cargo run`
- `AIC_CONFIG_DIR` overrides `~/.config/aic` — point it at a scratch dir for manual testing so you never touch real config/secrets
- OAuth/CIMD e2e testing without a real server: `python3 dev/mcp-oauth-stub.py` runs a combined AS+MCP stub on 127.0.0.1:8000 (auto-approving `/authorize`, no CIMD-doc fetch); see its module docstring for the matching `aic.yaml` snippet

## Deliberate scope constraints — do NOT "fix" these

- No provider abstraction: talk directly to OpenAI-compatible endpoints. Do not add per-provider adapters.
- MCP is Streamable HTTP only: no stdio or socket transports.
- Streaming-only display: `ChatRequest` always sets `stream: true`.

## Architecture facts you can't see from one file

- `agent.rs` owns the one-turn loop: stream → accumulate SSE deltas → run MCP tool calls → re-feed, capped by `ui.max_tool_iterations` (default 10).
- SSE tool-call fragments: `id`/`name` arrive only in the first chunk; `function.arguments` is split across chunks and must be concatenated by index.
- MCP tools get public names `<server>__<tool>` (double underscore); the catalog in `mcp/manager.rs` maps them back.
- MCP OAuth (CIMD, SPEC §7.5): tokens are **in-memory only** (`ServerAuth` in `mcp/auth/`); startup never opens a browser — `/auth <name>` is the only entry into the interactive flow. The manager's catalog indexes into `servers` by position, so servers are replaced in place and never removed from the Vec.
- Config is two layers — home `config.yaml` overlaid by project `./aic.yaml` — merged by whole top-level key **replacement**, never deep-merged.
- `${VAR}` expansion runs once at startup (`Settings::expand_secrets`); everything downstream assumes already-expanded values. Resolution order: secrets map → process env.
- Secrets fallback chain: `env.json.enc` (ChaCha20-Poly1305, key in system keyring, service=`aic` account=`env-key`) → plaintext `env.json` → env vars. Keyring code is cfg-gated to macOS/Linux in `src/config/secrets/keychain.rs`. Secrets failures warn on stderr and fall through — startup must never block on them.
- REPL errors print to stderr and the loop continues; reserve `?`-bubbling to `main` for startup only.

## Adding a REPL command

Create `src/commands/<name>.rs` implementing the `Command` trait with an
`inventory::submit!`, then add one `pub mod <name>;` line in
`src/commands/mod.rs`. Dispatch, `/help`, and registration pick it up
automatically — do not touch them (SPEC §9.3).

## Security

- NEVER commit plaintext `env.json` or real API keys — not even in tests or fixtures.
- `/config show` must keep redacting `api_key` and header values; preserve that when touching config display code.
