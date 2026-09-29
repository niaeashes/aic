// repl/view — terminal rendering (the production impl of `agent::TurnObserver`).
//
// This is the only place we turn the agent's "events to display" vocabulary
// into actual print!/eprintln!.
//   - assistant body → stdout (flush per chunk)
//   - thinking (reasoning) → stderr, gray, under a `thinking> ` label
//   - tool indicators / cap warnings → stderr
//
// While nothing is being printed (waiting for the first token, running a tool)
// a stderr spinner animates; every hook that prints stops it first, and the
// REPL calls `stop_spinner` after each turn so errors never land on its line.
//
// `section` is the per-message state we use to "print each label once" and
// "add a trailing newline only when we actually printed something". Switching
// between thinking and body closes the previous line first. `assistant_start`
// resets it for every new message, so one `TerminalView` instance can be
// reused for the whole session.

use std::io::{IsTerminal, Write};
use std::time::Duration;

use crate::agent::{ResponseStats, TurnObserver};
use crate::repl::spinner::Spinner;
use crate::llm::stream::Usage;

const ASSISTANT_LABEL: &str = "assistant> ";
const THINKING_LABEL: &str = "thinking> ";
/// ANSI bright black (gray) and reset.
const GRAY: &str = "\x1b[90m";
const RESET: &str = "\x1b[0m";
const TOOL_ARG_PREVIEW_MAX: usize = 80;

/// Terminal (TTY) rendering. Behavior is bit-for-bit identical to before the View was split out.
#[derive(Default)]
pub struct TerminalView {
    /// Which labelled line is open (label printed, line not yet terminated).
    section: Section,
    spinner: Spinner,
    /// Gray thinking text only on a color-capable stderr (TTY, no `NO_COLOR`).
    color: bool,
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Section {
    #[default]
    None,
    Thinking,
    Assistant,
}

impl TerminalView {
    pub fn new() -> Self {
        Self {
            color: std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
            ..Self::default()
        }
    }

    /// Terminate the open line (if any) on the stream it was written to.
    fn close_section(&mut self) {
        match self.section {
            Section::None => {}
            Section::Thinking => eprintln!(),
            Section::Assistant => println!(),
        }
        self.section = Section::None;
    }

    /// Clear any running spinner. Called by the REPL when a turn ends (by any
    /// path, including errors that bypass the observer hooks).
    pub fn stop_spinner(&mut self) {
        self.spinner.stop();
    }
}

impl TurnObserver for TerminalView {
    fn waiting(&mut self) {
        self.spinner.start("waiting");
    }

    fn assistant_start(&mut self) {
        self.section = Section::None;
    }

    fn reasoning_delta(&mut self, chunk: &str) {
        self.spinner.stop();
        if self.section != Section::Thinking {
            self.close_section();
            eprint!("{THINKING_LABEL}");
            self.section = Section::Thinking;
        }
        // Color each chunk separately so an interrupt mid-thought can't leave
        // the terminal gray.
        if self.color {
            eprint!("{GRAY}{chunk}{RESET}");
        } else {
            eprint!("{chunk}");
        }
    }

    fn assistant_delta(&mut self, chunk: &str) {
        self.spinner.stop();
        if self.section != Section::Assistant {
            // Print the assistant label once, at the head of the body.
            self.close_section();
            print!("{ASSISTANT_LABEL}");
            self.section = Section::Assistant;
        }
        print!("{chunk}");
        // Flush so long responses don't pile up at the end.
        std::io::stdout().flush().ok();
    }

    fn assistant_end(&mut self) {
        self.spinner.stop();
        self.close_section();
    }

    fn tool_call(&mut self, public_name: &str, raw_arguments: &str) {
        eprintln!("· tool call: {public_name}({})", arg_preview(raw_arguments));
        self.spinner.start(format!("running {public_name}"));
    }

    fn tool_succeeded(&mut self, public_name: &str) {
        self.spinner.stop();
        eprintln!("✓ tool ok:   {public_name}");
    }

    fn tool_failed(&mut self, public_name: &str, error_text: &str) {
        self.spinner.stop();
        eprintln!("✗ tool err:  {public_name}: {error_text}");
    }

    fn iteration_limit_reached(&mut self, max: u32) {
        eprintln!(
            "warning: tool calls reached ui.max_tool_iterations ({max}); aborting"
        );
    }

    fn cancelled(&mut self) {
        self.spinner.stop();
        // End the in-progress line cleanly, then note the interrupt.
        self.close_section();
        eprintln!("^C (interrupted)");
    }

    fn empty_response(&mut self) {
        eprintln!("warning: model returned an empty response (not added to history)");
    }

    fn truncated(&mut self) {
        eprintln!(
            "warning: response cut off by the server (finish_reason=length) — \
             hit generation.max_tokens or the model's context window"
        );
    }

    fn response_stats(&mut self, stats: &ResponseStats) {
        if let Some(u) = &stats.usage {
            eprintln!("{}", usage_line(u, stats.context_window));
        }
        eprintln!("{}", time_line(stats.first_token, stats.total));
    }
}

/// `· time: first token 3.2s, total 12.4s` (`first token -` if nothing came).
fn time_line(first_token: Option<Duration>, total: Duration) -> String {
    let first = match first_token {
        Some(d) => format!("{:.1}s", d.as_secs_f32()),
        None => "-".to_string(),
    };
    format!("· time: first token {first}, total {:.1}s", total.as_secs_f32())
}

/// `· context: 12,345 / 32,768 tokens (38%) (history 11,000 + reply 1,345)`.
/// Without a configured window the ` / N (P%)` part is omitted.
fn usage_line(usage: &Usage, context_window: Option<u32>) -> String {
    let total = group_digits(usage.total_tokens);
    let used = match context_window.filter(|w| *w > 0) {
        Some(w) => {
            let pct = usage.total_tokens * 100 / u64::from(w);
            format!("{total} / {} tokens ({pct}%)", group_digits(u64::from(w)))
        }
        None => format!("{total} tokens"),
    };
    format!(
        "· context: {used} (history {} + reply {})",
        group_digits(usage.prompt_tokens),
        group_digits(usage.completion_tokens),
    )
}

/// `12345` → `"12,345"`.
fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
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
    fn usage_line_with_and_without_window() {
        let u = Usage { prompt_tokens: 11000, completion_tokens: 1345, total_tokens: 12345 };
        assert_eq!(
            usage_line(&u, Some(32768)),
            "· context: 12,345 / 32,768 tokens (37%) (history 11,000 + reply 1,345)"
        );
        assert_eq!(
            usage_line(&u, None),
            "· context: 12,345 tokens (history 11,000 + reply 1,345)"
        );
    }

    #[test]
    fn time_line_formats_durations() {
        assert_eq!(
            time_line(Some(Duration::from_millis(3240)), Duration::from_millis(12400)),
            "· time: first token 3.2s, total 12.4s"
        );
        assert_eq!(time_line(None, Duration::from_millis(500)), "· time: first token -, total 0.5s");
    }

    #[test]
    fn group_digits_inserts_commas() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1000), "1,000");
        assert_eq!(group_digits(1234567), "1,234,567");
    }

    #[test]
    fn arg_preview_empty_returns_empty() {
        assert_eq!(arg_preview(""), "");
        assert_eq!(arg_preview("   \n  "), "");
    }
}
