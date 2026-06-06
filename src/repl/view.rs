// repl/view — 端末への描画（`agent::TurnObserver` の本番実装）。
//
// agent が定義する「描画イベントの語彙」を、実際の print!/eprintln! に落とす唯一の場所。
//   - assistant 本文 → stdout（逐次 flush）
//   - tool インジケータ / 上限警告 → stderr
//
// per-message 状態 `mid_line` で「assistant ラベルを 1 度だけ出す」「本文があった
// ときだけ末尾改行する」を管理する。`assistant_start` で毎メッセージ頭にリセットする
// ので、1 つの `TerminalView` をセッション通しで使い回せる。

use std::io::Write;

use crate::agent::TurnObserver;

const ASSISTANT_LABEL: &str = "assistant> ";
const TOOL_ARG_PREVIEW_MAX: usize = 80;

/// 端末（TTY）向けの描画。挙動は View 分離前と完全に同一。
#[derive(Default)]
pub struct TerminalView {
    /// assistant 本文を出力中（ラベル済み・未改行）か。
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
            // 先頭で 1 度だけ assistant ラベルを出す
            print!("{ASSISTANT_LABEL}");
            self.mid_line = true;
        }
        print!("{chunk}");
        // flush しないと長い応答が後ろにまとめて出てしまう
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
            "warning: tool 呼び出しが ui.max_tool_iterations ({max}) に達したため打ち切りました"
        );
    }
}

/// ツール呼び出しの可視化用に引数を 1 行プレビュー化。
///
/// - 改行は `\n` のエスケープに置換
/// - `TOOL_ARG_PREVIEW_MAX` を超えたら末尾を `…` で省略
/// - 空 / 空白のみは `""` を返してカッコの中身ゼロを明示
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
        // 切り詰め後の文字数 = TOOL_ARG_PREVIEW_MAX + 1（'…' ぶん）
        assert_eq!(p.chars().count(), TOOL_ARG_PREVIEW_MAX + 1);
    }

    #[test]
    fn arg_preview_empty_returns_empty() {
        assert_eq!(arg_preview(""), "");
        assert_eq!(arg_preview("   \n  "), "");
    }
}
