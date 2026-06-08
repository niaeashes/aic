// dispatch — route `/`-prefixed input to a Command impl (SPEC §9.4).
//
// Input contract:
//   - The caller has already stripped the leading `/`
//   - The first **whitespace character** splits `name` and `args`
//     (the rest is `args` verbatim)
//   - If no command matches `name`, print an error and continue
//
// We do a linear scan over inventory. Command count stays in the low dozens,
// so we don't bother with a HashMap (iterating is plenty fast).

use anyhow::Result;

use crate::commands::{split_first_token, Command, Outcome};
use crate::repl::context::ReplContext;

/// `body` is whatever follows the leading `/`.
///
/// Unknown commands and runtime errors print to stderr; the REPL keeps going.
/// The return is always `Ok(Outcome)` (we never break the outer loop on error).
pub async fn dispatch(body: &str, ctx: &mut ReplContext) -> Result<Outcome> {
    let (name, args) = split_first_token(body);
    if name.is_empty() {
        eprintln!("command name is empty. Run `/help` to list commands");
        return Ok(Outcome::Continue);
    }

    let found = inventory::iter::<&'static dyn Command>
        .into_iter()
        .find(|c| c.name() == name);

    match found {
        Some(cmd) => match cmd.run(args, ctx).await {
            Ok(outcome) => Ok(outcome),
            Err(e) => {
                eprintln!("/{name} failed: {e:#}");
                Ok(Outcome::Continue)
            }
        },
        None => {
            eprintln!("unknown command: /{name} (run `/help` to list)");
            Ok(Outcome::Continue)
        }
    }
}
