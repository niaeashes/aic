// dispatch — `/`-始まりの入力を Command トレイト実装に振り分ける（SPEC §9.4）。
//
// 入力規約:
//   - 呼び出し元が先頭の `/` を取り除いた状態で `dispatch` に渡す
//   - 最初の **空白 1 文字** で `name` と `args` を分割（残りはまるごと args）
//   - `name` が一致するコマンドが無ければエラー表示して `Continue`
//
// inventory から `name()` 一致を線形検索する。コマンド数は十数個に収まる前提なので
// HashMap は持たない（毎回イテレートしても十分速い）。

use anyhow::Result;

use crate::commands::{split_first_token, Command, Outcome};
use crate::repl::context::ReplContext;

/// `body` は先頭の `/` を取り除いた残り。
///
/// 未知コマンドや実行中エラーは `eprintln!` で表示し、REPL は継続する想定で
/// 戻り値は常に `Ok(Outcome)`（外側ループからは抜けない）。
pub async fn dispatch(body: &str, ctx: &mut ReplContext) -> Result<Outcome> {
    let (name, args) = split_first_token(body);
    if name.is_empty() {
        eprintln!("コマンド名が空です。`/help` で一覧を確認できます");
        return Ok(Outcome::Continue);
    }

    let found = inventory::iter::<&'static dyn Command>
        .into_iter()
        .find(|c| c.name() == name);

    match found {
        Some(cmd) => match cmd.run(args, ctx).await {
            Ok(outcome) => Ok(outcome),
            Err(e) => {
                eprintln!("/{name} 実行エラー: {e:#}");
                Ok(Outcome::Continue)
            }
        },
        None => {
            eprintln!("未知のコマンド: /{name}（`/help` で一覧）");
            Ok(Outcome::Continue)
        }
    }
}
