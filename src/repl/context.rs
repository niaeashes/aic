// context — Session / ReplContext (SPEC §11).
//
// No global state: the REPL loop passes `&mut ReplContext` through. Structs
// don't carry lifetimes (everything held is owned: `String` etc.).
//
// `Secrets` becomes irrelevant after `main.rs` calls `expand_secrets`, so it's
// not part of `ReplContext`. We carry only the already-expanded Settings.

use anyhow::{Context, Result};

use crate::active_model::ActiveModel;
use crate::config::Settings;
use crate::llm::types::Message;
use crate::mcp::McpManager;

/// Conversation history. An OpenAI-compatible list of messages.
///
/// Each session carries a short `id`, issued at creation and shown in the REPL
/// prompt (`aic [a3f2c1]>`). `/session` lists/creates/switches sessions;
/// `/clear` empties `messages` but keeps the `id` (the model selection and MCP
/// connections are preserved either way).
#[derive(Debug)]
pub struct Session {
    /// Six lowercase base-36 chars. `/session new` re-rolls on (astronomically
    /// unlikely) collision with an existing session.
    pub id: String,
    pub messages: Vec<Message>,
}

impl Session {
    pub fn new() -> Self {
        Self { id: new_session_id(), messages: Vec::new() }
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// 36^6 ≈ 2×10⁹ ids — plenty for the handful of in-memory sessions a process
/// ever holds.
fn new_session_id() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut rng = rand::thread_rng();
    (0..6).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
}

/// Shared state that the REPL, commands, and agent all touch.
///
/// Always passed by `&mut ReplContext` — never global (SPEC §11).
pub struct ReplContext {
    pub settings: Settings,
    pub session: Session,
    /// Inactive sessions, switchable via `/session use <id>`. In-memory only —
    /// gone on exit (persistence is future work, SPEC §15).
    pub stash: Vec<Session>,
    pub http: reqwest::Client,
    /// The model currently in use. Resolved and cached at `/model use` time via
    /// `ActiveModel::resolve`. Stays `None` if config has no `default_model`;
    /// `agent::run_turn` returns an error in that case (no per-turn re-resolution
    /// needed).
    pub current_model: Option<ActiveModel>,
    /// MCP servers + public tool catalog (M6).
    /// Per-server failures at startup are absorbed; the REPL still runs even
    /// when this is empty.
    pub mcp: McpManager,
}

impl ReplContext {
    /// Return a **clone** of the current `ActiveModel`. The `agent::run_turn`
    /// path needs to re-borrow `ctx` as `&mut` (e.g. for mcp.call), so we
    /// avoid holding an `&ActiveModel` across that boundary.
    ///
    /// When unselected, return a user-facing error (callers can just `?` it).
    pub fn require_active_model(&self) -> Result<ActiveModel> {
        self.current_model.clone().context(
            "no model selected. Set default_model in config, or use `/model use <group>:<model>`",
        )
    }
}
