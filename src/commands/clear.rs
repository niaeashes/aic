// /clear — clear conversation history (SPEC §10).
//
// Only `session.messages` is cleared. The session id, the current model and any
// MCP connections are preserved. To start a separate conversation that you can
// switch back from, use `/session new` instead.

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
        "Clear the current session's history (session id / model selection preserved)"
    }

    async fn run(&self, _args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        ctx.session.messages.clear();
        println!("Conversation history cleared");
        Ok(Outcome::Continue)
    }
}

inventory::submit! { &Clear as &dyn Command }
