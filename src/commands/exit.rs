// /exit — terminate the REPL (SPEC §10).
//
// Just returns `Outcome::Exit`. The REPL loop sees that and breaks.

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
        "Exit the REPL"
    }

    async fn run(&self, _args: &str, _ctx: &mut ReplContext) -> Result<Outcome> {
        Ok(Outcome::Exit)
    }
}

inventory::submit! { &Exit as &dyn Command }
