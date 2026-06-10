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

use crate::agent::TurnObserver;
use crate::commands::Outcome;
use crate::llm::types::Message;
use crate::repl::context::{ReplContext, Session};
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

/// Repair the message log after a mid-flight cancel so the next turn starts from
/// a valid boundary. A turn interrupted between "assistant requested tool calls"
/// and "all tool results pushed" would otherwise leave the conversation in a
/// state most OpenAI-compatible servers reject (every `tool_calls` entry must be
/// answered by a `tool` message). We drop any trailing `tool` messages and a
/// trailing assistant-with-tool_calls, leaving the log ending at a clean point.
fn repair_session(session: &mut Session) {
    while matches!(session.messages.last(), Some(Message::Tool { .. })) {
        session.messages.pop();
    }
    if matches!(session.messages.last(), Some(m) if !m.tool_calls().is_empty()) {
        session.messages.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::types::{FunctionCall, ToolCall};

    fn assistant_with_tool_call() -> Message {
        Message::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                kind: "function".into(),
                function: FunctionCall { name: "f".into(), arguments: "{}".into() },
            }],
        }
    }

    #[test]
    fn repair_drops_dangling_assistant_and_partial_tools() {
        // assistant requested a tool call, but the turn was cancelled before the
        // tool result came back: drop both so the log ends on the user message.
        let mut s = Session {
            messages: vec![Message::user("hi"), assistant_with_tool_call()],
        };
        repair_session(&mut s);
        assert!(matches!(s.messages.as_slice(), [Message::User { .. }]));
    }

    #[test]
    fn repair_drops_trailing_tool_then_assistant() {
        let mut s = Session {
            messages: vec![
                Message::user("hi"),
                assistant_with_tool_call(),
                Message::tool("c1", "f", "partial"),
            ],
        };
        repair_session(&mut s);
        assert!(matches!(s.messages.as_slice(), [Message::User { .. }]));
    }

    #[test]
    fn repair_leaves_clean_log_untouched() {
        let mut s = Session {
            messages: vec![Message::user("hi"), Message::assistant_text("done")],
        };
        repair_session(&mut s);
        assert_eq!(s.messages.len(), 2);
    }
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

                // Chat input → agent loop. Race it against Ctrl-C so a long
                // generation or a stuck tool call can be interrupted (readline
                // isn't active during this `await`, so SIGINT wouldn't otherwise
                // reach us). `biased` polls the turn first so a turn that finishes
                // in the same tick isn't reported as cancelled.
                let cancelled = tokio::select! {
                    biased;
                    res = crate::agent::run_turn(ctx, trimmed.to_string(), &mut view) => {
                        if let Err(e) = res {
                            eprintln!("error: {e:#}");
                        }
                        false
                    }
                    _ = tokio::signal::ctrl_c() => true,
                };
                if cancelled {
                    // The run_turn future is dropped here, releasing its borrows.
                    view.cancelled();
                    repair_session(&mut ctx.session);
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
