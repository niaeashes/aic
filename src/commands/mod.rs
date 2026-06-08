// commands — Command trait and inventory-based auto-collection (SPEC §9.2).
//
// To add a new command, drop a file in this directory and add one `mod` line
// here. Dispatch / help / registration logic stays untouched (SPEC §9.3).
//
// We use `async-trait` so trait objects (`&dyn Command`) can be stored in
// inventory. Inventory requires `Send + Sync + 'static`; command types should
// generally be ZSTs.

use anyhow::Result;
use async_trait::async_trait;

use crate::repl::context::ReplContext;

/// REPL flow control after a command runs.
///
/// Only the terminator commands (e.g. `/exit`) return `Exit`; everything else returns `Continue`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Exit,
}

#[async_trait]
pub trait Command: Sync + Send {
    /// `/model` → `"model"`. Don't include the leading `/`.
    fn name(&self) -> &'static str;

    /// One-line help, used in the `/help` listing.
    fn help(&self) -> &'static str;

    /// `args` is whatever comes after `/<name>` and one whitespace character. Empty string is allowed.
    async fn run(&self, args: &str, ctx: &mut ReplContext) -> Result<Outcome>;
}

/// Split `s` into `(first_token, rest)` on the first whitespace. Without
/// whitespace, `rest` is the empty string.
///
/// Two use cases share the same shape — "the first token and everything after":
///   - REPL dispatch: split `body` into `<command name> <args...>`
///   - inside a command: split `args` into `<subcommand> <rest...>`
///
/// `rest` is `trim_start`-ed (we eat leading whitespace; interior and trailing
/// whitespace is preserved). Command names and subcommands are ASCII in practice,
/// but `char::is_whitespace` also accepts full-width whitespace etc.
pub fn split_first_token(s: &str) -> (&str, &str) {
    match s.find(char::is_whitespace) {
        Some(idx) => (&s[..idx], s[idx + 1..].trim_start()),
        None => (s, ""),
    }
}

// Inventory holds static references. Each command file appends with `inventory::submit!`.
inventory::collect!(&'static dyn Command);

pub mod clear;
pub mod config;
pub mod doctor;
pub mod exit;
pub mod help;
pub mod model;

#[cfg(test)]
mod tests {
    use super::split_first_token;

    #[test]
    fn splits_token_only() {
        assert_eq!(split_first_token("exit"), ("exit", ""));
    }

    #[test]
    fn splits_token_and_rest() {
        assert_eq!(
            split_first_token("model use local:qwen2.5-coder:32b"),
            ("model", "use local:qwen2.5-coder:32b")
        );
    }

    #[test]
    fn empty_input_yields_empty_token() {
        assert_eq!(split_first_token(""), ("", ""));
    }

    #[test]
    fn trims_leading_but_keeps_internal_whitespace_in_rest() {
        assert_eq!(split_first_token("help   foo  bar"), ("help", "foo  bar"));
    }
}
