// /clear — clear conversation history (SPEC §10).
//
// Only `session.messages` is cleared. The current model and any MCP connections
// are preserved.

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
        "Clear the conversation history (model selection is preserved)"
    }

    async fn run(&self, _args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        ctx.session.messages.clear();
        println!("Conversation history cleared");
        Ok(Outcome::Continue)
    }
}

inventory::submit! { &Clear as &dyn Command }
