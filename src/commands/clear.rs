// /clear — 会話履歴を消去する（SPEC §10）。
//
// session.messages のみクリア。current_model や MCP 接続（M6 以降）は維持する。

use anyhow::Result;
use async_trait::async_trait;

use super::{Command, Outcome};
use crate::repl::context::ReplContext;

struct Clear;

#[async_trait]
impl Command for Clear {
    fn name(&self) -> &'static str {
        "clear"
    }

    fn help(&self) -> &'static str {
        "会話履歴を消去する（モデル選択は維持）"
    }

    async fn run(&self, _args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        ctx.session.messages.clear();
        println!("会話履歴を消去しました");
        Ok(Outcome::Continue)
    }
}

inventory::submit! { &Clear as &dyn Command }
