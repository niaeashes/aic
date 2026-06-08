// /help — list every registered command.
//
// Walk `inventory` for `&dyn Command`, sort by name, and print. Sorting is
// necessary because linker-defined registration order isn't stable.

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
        "List all registered commands"
    }

    async fn run(&self, _args: &str, _ctx: &mut ReplContext) -> Result<Outcome> {
        let mut cmds: Vec<&&'static dyn Command> = inventory::iter::<&'static dyn Command>
            .into_iter()
            .collect();
        cmds.sort_by_key(|c| c.name());

        // Pad column width to the longest name.
        let width = cmds.iter().map(|c| c.name().len()).max().unwrap_or(0);
        for c in cmds {
            println!("  /{:<width$}  {}", c.name(), c.help(), width = width);
        }
        Ok(Outcome::Continue)
    }
}

inventory::submit! { &Help as &dyn Command }
