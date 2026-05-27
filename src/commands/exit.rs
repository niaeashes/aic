// /exit — REPL を終了する（SPEC §10）。
//
// 実体は `Outcome::Exit` を返すだけ。REPL ループ側がそれを見て break する。

use anyhow::Result;
use async_trait::async_trait;

use super::{Command, Outcome};
use crate::repl::context::ReplContext;

struct Exit;

#[async_trait]
impl Command for Exit {
    fn name(&self) -> &'static str {
        "exit"
    }

    fn help(&self) -> &'static str {
        "REPL を終了する"
    }

    async fn run(&self, _args: &str, _ctx: &mut ReplContext) -> Result<Outcome> {
        Ok(Outcome::Exit)
    }
}

inventory::submit! { &Exit as &dyn Command }
