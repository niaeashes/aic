// commands — Command トレイトと inventory による自動収集（SPEC §9.2）。
//
// 新コマンド追加は対応するファイルを置き、ここに `mod` 行を 1 つ足すだけ。
// ディスパッチ／ヘルプ／登録ロジックには触らない（SPEC §9.3 の方針）。
//
// `async-trait` を使うのはトレイトオブジェクト (`&dyn Command`) を inventory に積むため。
// inventory は `Send + Sync + 'static` を要求するので、コマンド型は基本 ZST にする。

use anyhow::Result;
use async_trait::async_trait;

use crate::repl::context::ReplContext;

/// コマンド実行後の REPL 制御フロー。
///
/// `/exit` のような終了系だけが `Exit` を返し、それ以外は `Continue`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Exit,
}

#[async_trait]
pub trait Command: Sync + Send {
    /// `/model` → `"model"`。先頭の `/` は含めない。
    fn name(&self) -> &'static str;

    /// 1 行ヘルプ。`/help` で並べる文面。
    fn help(&self) -> &'static str;

    /// `args` は `/<name>` とそれに続く 1 個の空白を除いた残り。空文字列も来る。
    async fn run(&self, args: &str, ctx: &mut ReplContext) -> Result<Outcome>;
}

/// 先頭の空白で `(first_token, rest)` に分割する。空白が無ければ rest は空文字列。
///
/// 用途は2つあり、どちらも同じ「最初のトークンとそれ以降」という形:
///   - REPL の dispatch: `body` を `<コマンド名> <引数...>` に割る
///   - 各コマンド内:     `args` を `<サブコマンド> <残り...>` に割る
///
/// `rest` は `trim_start` のみ（先頭空白を食い、内部・末尾の空白は保つ）。
/// コマンド名・サブコマンド名は ASCII 想定だが、`char::is_whitespace` で全角空白等にも
/// 一応耐える。
pub fn split_first_token(s: &str) -> (&str, &str) {
    match s.find(char::is_whitespace) {
        Some(idx) => (&s[..idx], s[idx + 1..].trim_start()),
        None => (s, ""),
    }
}

// inventory に積むのは静的参照。各コマンドのファイル末尾で `inventory::submit!` する。
inventory::collect!(&'static dyn Command);

pub mod clear;
pub mod config;
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
