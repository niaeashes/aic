// repl — rustyline-based interactive loop (SPEC §9.1).
//
// Any `/`-prefixed input goes straight through to `dispatch::dispatch`. `/exit`
// itself is a Command — we exit the loop when it returns `Outcome::Exit`.
//
// `rustyline`'s readline is synchronous and blocking, but we call it on the
// `#[tokio::main]` multi-threaded runtime, so blocking one worker leaves other
// tasks free. As a single-user interactive CLI, that's a fine trade-off.
//
// History persists at `config_dir/history.txt`. Loaded at startup, saved at
// shutdown. `history_size` caps the entry count. Read/write failures only emit
// a warning — they never block the loop.

use std::path::PathBuf;

use anyhow::Result;
use rustyline::error::ReadlineError;
use rustyline::{Config, DefaultEditor};

use crate::commands::Outcome;
use crate::repl::context::ReplContext;
use crate::repl::view::TerminalView;

pub mod context;
pub mod dispatch;
pub mod prompt;
pub mod view;

const PROMPT: &str = "aic> ";
const HISTORY_FILE: &str = "history.txt";

pub async fn run(ctx: &mut ReplContext) -> Result<()> {
    let cfg = Config::builder()
        .max_history_size(ctx.settings.ui.history_size)?
        .auto_add_history(false) // We add entries ourselves.
        .build();
    let mut rl = DefaultEditor::with_config(cfg)?;

    // The history file lives under config_dir. Empty config_dir (e.g. in tests)
    // disables persistence.
    let history_path: Option<PathBuf> = if ctx.settings.config_dir.as_os_str().is_empty() {
        None
    } else {
        Some(ctx.settings.config_dir.join(HISTORY_FILE))
    };
    if let Some(p) = history_path.as_deref() {
        if p.exists() {
            if let Err(e) = rl.load_history(p) {
                tracing::warn!("failed to load history ({}): {e:#}", p.display());
            }
        }
    }

    let result = run_loop(ctx, &mut rl).await;

    // Save history regardless of the exit path (clean or erroring).
    if let Some(p) = history_path.as_deref() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = rl.save_history(p) {
            tracing::warn!("failed to save history ({}): {e:#}", p.display());
        }
    }
    result
}

async fn run_loop(ctx: &mut ReplContext, rl: &mut DefaultEditor) -> Result<()> {
    // One TerminalView for the whole session. Per-message state is reset by
    // `assistant_start`, so reusing it across turns is fine.
    let mut view = TerminalView::new();
    loop {
        match rl.readline(PROMPT) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // Don't add the line to history if it's identical to the previous
                // entry (rustyline's HistoryEntry::Smart equivalent).
                let dedup = match rl.history().iter().next_back() {
                    Some(last) => last == trimmed,
                    None => false,
                };
                if !dedup {
                    let _ = rl.add_history_entry(trimmed);
                }

                if let Some(body) = trimmed.strip_prefix('/') {
                    match dispatch::dispatch(body, ctx).await? {
                        Outcome::Continue => continue,
                        Outcome::Exit => break,
                    }
                }

                // Chat input → agent loop
                if let Err(e) = crate::agent::run_turn(ctx, trimmed.to_string(), &mut view).await {
                    eprintln!("error: {e:#}");
                }
            }
            // Ctrl-C cancels the current input and returns to the prompt.
            Err(ReadlineError::Interrupted) => continue,
            // Ctrl-D exits.
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("error: failed to read input: {e}");
                break;
            }
        }
    }
    Ok(())
}
