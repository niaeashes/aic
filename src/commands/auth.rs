// /auth — OAuth (CIMD) login for MCP servers (SPEC §7.5, §10).
//
//   /auth                 List OAuth-configured servers and their token state
//   /auth <name>          Run the browser login flow and (re)connect the server
//   /auth logout <name>   Drop the in-memory tokens and the server's tools
//
// Tokens are in-memory only: startup never opens a browser, so this command is
// the single entry point into the interactive flow. Errors `bail!` and the
// REPL loop continues (existing convention).

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use super::{split_first_token, Command, Outcome};
use crate::mcp::auth::interactive_login;
use crate::mcp::connect::{connect_server, ConnectOutcome};
use crate::repl::context::ReplContext;

struct Auth;

#[async_trait]
impl Command for Auth {
    fn name(&self) -> &'static str {
        "auth"
    }

    fn help(&self) -> &'static str {
        "OAuth login for MCP servers: /auth | /auth <name> | /auth logout <name>"
    }

    async fn run(&self, args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        let (sub, rest) = split_first_token(args.trim());
        match sub {
            "" => list_status(ctx),
            "logout" => logout(rest, ctx),
            name => login(name, ctx).await,
        }?;
        Ok(Outcome::Continue)
    }
}

inventory::submit! { &Auth as &dyn Command }

// ---------------------------------------------------------------------------
// Subcommand bodies
// ---------------------------------------------------------------------------

fn list_status(ctx: &ReplContext) -> Result<()> {
    let oauth_servers: Vec<_> = ctx
        .settings
        .mcp_servers
        .iter()
        .filter(|s| s.auth.is_some())
        .collect();
    if oauth_servers.is_empty() {
        println!("No MCP servers with `auth:` configured. Add an `auth.client_id` in config.yaml.");
        return Ok(());
    }
    for cfg in oauth_servers {
        let state = match ctx.mcp.server_status(&cfg.name) {
            Some((n, Some(auth))) if n > 0 => {
                format!("connected — {n} tool{}, {}", if n == 1 { "" } else { "s" }, auth.describe())
            }
            Some((_, _)) => format!("logged out — run /auth {}", cfg.name),
            None => format!("not connected — run /auth {}", cfg.name),
        };
        let enabled = if cfg.enabled { "" } else { " (disabled)" };
        println!("{}{enabled}: {state}", cfg.name);
    }
    Ok(())
}

async fn login(name: &str, ctx: &mut ReplContext) -> Result<()> {
    // Clone the config so &ctx.settings isn't held across the awaits below
    // (ctx.mcp needs &mut at the end).
    let cfg = ctx
        .settings
        .mcp_servers
        .iter()
        .find(|s| s.name == name)
        .with_context(|| format!("unknown MCP server: {name:?} (see /config show)"))?
        .clone();
    let oauth = cfg
        .auth
        .clone()
        .with_context(|| format!("server {name:?} has no `auth:` block; it uses static headers"))?;
    if !cfg.enabled {
        bail!("server {name:?} is disabled in config");
    }

    let server_auth = interactive_login(&cfg, &oauth, &ctx.http).await?;
    eprintln!("auth: logged in; connecting to {} ...", cfg.url);

    match connect_server(&cfg, ctx.http.clone(), Some(server_auth)).await {
        ConnectOutcome::Connected {
            transport,
            tools,
            auth,
        } => {
            let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
            println!(
                "Connected to {} ({} tool{}): {}",
                cfg.name,
                names.len(),
                if names.len() == 1 { "" } else { "s" },
                names.join(", ")
            );
            ctx.mcp.replace_or_push(cfg.name.clone(), transport, tools, auth);
            Ok(())
        }
        ConnectOutcome::NeedsAuth(reason) => bail!("connection still unauthorized: {reason}"),
        ConnectOutcome::Failed(e) => Err(e.context(format!("connecting to {} failed", cfg.name))),
    }
}

fn logout(rest: &str, ctx: &mut ReplContext) -> Result<()> {
    let (name, _) = split_first_token(rest);
    if name.is_empty() {
        bail!("usage: /auth logout <name>");
    }
    if ctx.mcp.remove_server_tools(name) {
        println!("Logged out of {name}: tokens dropped, tools removed.");
    } else {
        println!("{name} was not connected; nothing to do.");
    }
    Ok(())
}
