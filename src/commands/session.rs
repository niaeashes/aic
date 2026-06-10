// /session — list / create / switch in-memory sessions (SPEC §10).
//
// A session is one conversation history; the active one's id is shown in the
// REPL prompt (`aic [a3f2c1]>`). Inactive sessions live in `ctx.stash` and are
// in-memory only — they vanish on exit.
//
// Subcommands:
//   - no args:    list sessions, active marked `*`
//   - `new`:      stash the active session and start a fresh one (new id).
//                 An empty active session is discarded, not stashed.
//   - `use <id>`: switch to a stashed session. `<id>` may be a unique prefix.
//
// Note the contrast with `/clear`, which empties the active session's
// messages but keeps its id.

use anyhow::{bail, Result};
use async_trait::async_trait;

use super::{split_first_token, Command, Outcome};
use crate::llm::types::Message;
use crate::repl::context::{ReplContext, Session};

struct SessionCmd;

#[async_trait]
impl Command for SessionCmd {
    fn name(&self) -> &'static str {
        "session"
    }

    fn help(&self) -> &'static str {
        "List sessions / `new` starts a fresh one / `use <id>` switches"
    }

    async fn run(&self, args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            print_list(&ctx.session, &ctx.stash);
            return Ok(Outcome::Continue);
        }

        let (sub, rest) = split_first_token(trimmed);
        match sub {
            "new" => {
                new_session(&mut ctx.session, &mut ctx.stash);
                println!("Started session {}", ctx.session.id);
                Ok(Outcome::Continue)
            }
            "use" => {
                if rest.is_empty() {
                    bail!("usage: /session use <id>");
                }
                if switch_session(&mut ctx.session, &mut ctx.stash, rest)? {
                    println!("Switched to session {}", ctx.session.id);
                } else {
                    println!("Already on session {}", ctx.session.id);
                }
                Ok(Outcome::Continue)
            }
            other => {
                bail!("unknown subcommand: {other} (usage: /session | /session new | /session use <id>)")
            }
        }
    }
}

fn print_list(active: &Session, stash: &[Session]) {
    println!("* {}  ({} messages)  {}", active.id, active.messages.len(), title(active));
    for s in stash {
        println!("  {}  ({} messages)  {}", s.id, s.messages.len(), title(s));
    }
}

/// List label: the first user message, whitespace-collapsed and truncated.
fn title(s: &Session) -> String {
    const MAX_CHARS: usize = 40;
    let first = s.messages.iter().find_map(|m| match m {
        Message::User { content } => Some(content.as_str()),
        _ => None,
    });
    let Some(text) = first else {
        return "(empty)".to_string();
    };
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > MAX_CHARS {
        let cut: String = flat.chars().take(MAX_CHARS).collect();
        format!("{cut}…")
    } else {
        flat
    }
}

/// Stash `active` and replace it with a fresh session. An empty active session
/// is dropped instead of stashed (nothing to switch back to). The fresh id is
/// re-rolled until it collides with no live session.
fn new_session(active: &mut Session, stash: &mut Vec<Session>) {
    let mut fresh = Session::new();
    while fresh.id == active.id || stash.iter().any(|s| s.id == fresh.id) {
        fresh = Session::new();
    }
    let old = std::mem::replace(active, fresh);
    if !old.messages.is_empty() {
        stash.push(old);
    }
}

/// Switch the active session to the one matching `query` (exact id first, then
/// unique prefix; the active session participates so `use <current>` is a
/// no-op). Returns `true` if a switch happened, `false` if the match was the
/// already-active session.
fn switch_session(active: &mut Session, stash: &mut Vec<Session>, query: &str) -> Result<bool> {
    let (on_active, in_stash) = find_matches(&active.id, stash, query);
    match (on_active, in_stash.as_slice()) {
        (true, []) => Ok(false),
        (false, [i]) => {
            let target = stash.remove(*i);
            stash.push(std::mem::replace(active, target));
            Ok(true)
        }
        (false, []) => bail!("no session matches `{query}` (run /session to list)"),
        _ => bail!("`{query}` matches multiple sessions (run /session to list)"),
    }
}

/// (active matches, stash indices that match). Exact id matches win; only when
/// there is none do prefix matches count.
fn find_matches(active_id: &str, stash: &[Session], query: &str) -> (bool, Vec<usize>) {
    let exact: Vec<usize> =
        stash.iter().enumerate().filter(|(_, s)| s.id == query).map(|(i, _)| i).collect();
    if active_id == query || !exact.is_empty() {
        return (active_id == query, exact);
    }
    let prefix: Vec<usize> = stash
        .iter()
        .enumerate()
        .filter(|(_, s)| s.id.starts_with(query))
        .map(|(i, _)| i)
        .collect();
    (active_id.starts_with(query), prefix)
}

inventory::submit! { &SessionCmd as &dyn Command }

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, messages: Vec<Message>) -> Session {
        Session { id: id.into(), messages }
    }

    #[test]
    fn new_session_stashes_nonempty_and_drops_empty() {
        let mut active = session("aaaaaa", vec![Message::user("hi")]);
        let mut stash = Vec::new();
        new_session(&mut active, &mut stash);
        assert_eq!(stash.len(), 1);
        assert_eq!(stash[0].id, "aaaaaa");
        assert!(active.messages.is_empty());
        assert_ne!(active.id, "aaaaaa");

        // The (empty) session just created is discarded by the next `new`.
        new_session(&mut active, &mut stash);
        assert_eq!(stash.len(), 1);
    }

    #[test]
    fn switch_by_exact_id_swaps_active_into_stash() {
        let mut active = session("aaaaaa", vec![Message::user("a")]);
        let mut stash = vec![session("bbbbbb", vec![Message::user("b")])];
        let switched = switch_session(&mut active, &mut stash, "bbbbbb").unwrap();
        assert!(switched);
        assert_eq!(active.id, "bbbbbb");
        assert_eq!(stash.len(), 1);
        assert_eq!(stash[0].id, "aaaaaa");
    }

    #[test]
    fn switch_by_unique_prefix() {
        let mut active = session("aaaaaa", vec![]);
        let mut stash = vec![session("bbbbbb", vec![]), session("bccccc", vec![])];
        assert!(switch_session(&mut active, &mut stash, "bc").unwrap());
        assert_eq!(active.id, "bccccc");
    }

    #[test]
    fn switch_to_current_is_a_noop() {
        let mut active = session("aaaaaa", vec![]);
        let mut stash = vec![session("bbbbbb", vec![])];
        assert!(!switch_session(&mut active, &mut stash, "aaaaaa").unwrap());
        assert_eq!(active.id, "aaaaaa");
        assert_eq!(stash.len(), 1);
    }

    #[test]
    fn ambiguous_prefix_errors() {
        let mut active = session("aaaaaa", vec![]);
        let mut stash = vec![session("bbbbbb", vec![]), session("bbcccc", vec![])];
        assert!(switch_session(&mut active, &mut stash, "bb").is_err());
    }

    #[test]
    fn exact_id_wins_over_prefix_collision() {
        // `bbb` is itself an id AND a prefix of `bbbbbb` — exact must win.
        let mut active = session("aaaaaa", vec![]);
        let mut stash = vec![session("bbb", vec![]), session("bbbbbb", vec![])];
        assert!(switch_session(&mut active, &mut stash, "bbb").unwrap());
        assert_eq!(active.id, "bbb");
    }

    #[test]
    fn unknown_id_errors() {
        let mut active = session("aaaaaa", vec![]);
        let mut stash = Vec::new();
        assert!(switch_session(&mut active, &mut stash, "zzz").is_err());
    }

    #[test]
    fn title_truncates_and_collapses_whitespace() {
        let s = session("aaaaaa", vec![Message::user("hello\n  world")]);
        assert_eq!(title(&s), "hello world");
        let long = "x".repeat(100);
        let s = session("aaaaaa", vec![Message::user(long)]);
        assert_eq!(title(&s).chars().count(), 41); // 40 + ellipsis
        let s = session("aaaaaa", vec![]);
        assert_eq!(title(&s), "(empty)");
    }
}
