// repl/view — terminal rendering (the production impl of `agent::TurnObserver`).
//
// This is the only place we turn the agent's "events to display" vocabulary
// into actual print!/eprintln!.
//   - assistant body → stdout (flush per chunk)
//   - tool indicators / cap warnings → stderr
//
// `mid_line` is the per-message state we use to "print the assistant label once"
// and "add a trailing newline only when we actually printed body content".
// `assistant_start` resets it for every new message, so one `TerminalView`
// instance can be reused for the whole session.

use std::io::Write;

use crate::agent::TurnObserver;

const ASSISTANT_LABEL: &str = "assistant> ";
const TOOL_ARG_PREVIEW_MAX: usize = 80;

/// Terminal (TTY) rendering. Behavior is bit-for-bit identical to before the View was split out.
#[derive(Default)]
pub struct TerminalView {
    /// True if we've already printed the label and haven't terminated the line.
    mid_line: bool,
}

impl TerminalView {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TurnObserver for TerminalView {
    fn assistant_start(&mut self) {
        self.mid_line = false;
    }

    fn assistant_delta(&mut self, chunk: &str) {
        if !self.mid_line {
            // Print the assistant label once, at the head of the message.
            print!("{ASSISTANT_LABEL}");
            self.mid_line = true;
        }
        print!("{chunk}");
        // Flush so long responses don't pile up at the end.
        std::io::stdout().flush().ok();
    }

    fn assistant_end(&mut self) {
        if self.mid_line {
            println!();
            self.mid_line = false;
        }
    }

    fn tool_call(&mut self, public_name: &str, raw_arguments: &str) {
        eprintln!("· tool call: {public_name}({})", arg_preview(raw_arguments));
    }

    fn tool_succeeded(&mut self, public_name: &str) {
        eprintln!("✓ tool ok:   {public_name}");
    }

    fn tool_failed(&mut self, public_name: &str, error_text: &str) {
        eprintln!("✗ tool err:  {public_name}: {error_text}");
    }

    fn iteration_limit_reached(&mut self, max: u32) {
        eprintln!(
            "warning: tool calls reached ui.max_tool_iterations ({max}); aborting"
        );
    }
}

/// Single-line preview of tool arguments for the inline indicator.
///
/// - Newlines are escaped as `\n`
/// - Anything past `TOOL_ARG_PREVIEW_MAX` is truncated with `…`
/// - Empty / whitespace-only input returns `""` so the empty parens are explicit
fn arg_preview(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "".to_string();
    }
    let single_line: String = trimmed
        .chars()
        .map(|c| match c {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => " ".to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("");
    if single_line.chars().count() > TOOL_ARG_PREVIEW_MAX {
        let truncated: String = single_line.chars().take(TOOL_ARG_PREVIEW_MAX).collect();
        format!("{truncated}…")
    } else {
        single_line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arg_preview_collapses_newlines() {
        assert_eq!(arg_preview("{\n  \"x\": 1\n}"), "{\\n  \"x\": 1\\n}");
    }

    #[test]
    fn arg_preview_truncates_long_strings() {
        let long: String = "a".repeat(200);
        let p = arg_preview(&long);
        assert!(p.ends_with('…'));
        // After truncation = TOOL_ARG_PREVIEW_MAX + 1 chars (the '…' marker).
        assert_eq!(p.chars().count(), TOOL_ARG_PREVIEW_MAX + 1);
    }

    #[test]
    fn arg_preview_empty_returns_empty() {
        assert_eq!(arg_preview(""), "");
        assert_eq!(arg_preview("   \n  "), "");
    }
}
