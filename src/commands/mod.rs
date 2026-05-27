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

// inventory に積むのは静的参照。各コマンドのファイル末尾で `inventory::submit!` する。
inventory::collect!(&'static dyn Command);

pub mod clear;
pub mod config;
pub mod exit;
pub mod help;
pub mod model;
