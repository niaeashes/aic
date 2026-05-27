// /help — 登録済みコマンドの一覧を出す。
//
// inventory から `&dyn Command` を全部走査し、name の昇順で並べて表示する。
// 並べ替えするのは登録順がリンカ依存で安定しないため。

use anyhow::Result;
use async_trait::async_trait;

use super::{Command, Outcome};
use crate::repl::context::ReplContext;

struct Help;

#[async_trait]
impl Command for Help {
    fn name(&self) -> &'static str {
        "help"
    }

    fn help(&self) -> &'static str {
        "登録済みコマンドの一覧を表示する"
    }

    async fn run(&self, _args: &str, _ctx: &mut ReplContext) -> Result<Outcome> {
        let mut cmds: Vec<&&'static dyn Command> = inventory::iter::<&'static dyn Command>
            .into_iter()
            .collect();
        cmds.sort_by_key(|c| c.name());

        // 整形: 最長 name に合わせて pad
        let width = cmds.iter().map(|c| c.name().len()).max().unwrap_or(0);
        for c in cmds {
            println!("  /{:<width$}  {}", c.name(), c.help(), width = width);
        }
        Ok(Outcome::Continue)
    }
}

inventory::submit! { &Help as &dyn Command }
