// aic — minimal interactive chat CLI
//
// Startup sequence:
//   1. Initialize tracing (filterable via `RUST_LOG`; default is warn)
//   2. Parse args with clap (`aic`, `aic env seal|unseal`, `aic --config <path>`)
//   3. Load config as a two-layer shallow merge of home + project
//   4. Load secrets from config_dir (env.json.enc → env.json → env vars)
//   5. Expand `${VAR}` in every Settings field
//   6. Build the ReplContext and start the REPL loop
//
// `env seal|unseal` operates on the secrets files and exits without starting the REPL.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod active_model;
mod agent;
mod commands;
mod config;
mod llm;
mod mcp;
mod repl;

use active_model::ActiveModel;

#[derive(Debug, Parser)]
#[command(name = "aic", about = "minimal interactive chat CLI", version)]
struct Cli {
    /// Explicit path to the home config file (default: `$AIC_CONFIG_DIR` or `~/.config/aic/config.yaml`)
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Manage the secrets files (env.json / env.json.enc) (SPEC §5)
    Env {
        #[command(subcommand)]
        action: EnvAction,
    },
}

#[derive(Debug, Subcommand)]
enum EnvAction {
    /// env.json → env.json.enc (key goes into Keychain; reuse if one already exists)
    Seal,
    /// env.json.enc → env.json (extracts the plaintext for editing)
    Unseal,
}

#[tokio::main]
async fn main() -> Result<()> {
    // When `RUST_LOG` is unset, only warn-level and above are shown. Override with
    // e.g. `RUST_LOG=aic=debug` for verbose logs. Filter construction is not
    // expected to fail; fall through to the default instead of erroring out.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,aic=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.cmd {
        Some(Cmd::Env { action }) => {
            let dir = resolve_config_dir(cli.config.as_deref())?;
            match action {
                EnvAction::Seal => config::secrets::seal(&dir),
                EnvAction::Unseal => config::secrets::unseal(&dir),
            }
        }
        None => run_repl(cli.config).await,
    }
}

/// For `aic env ...`, return the *parent directory* of the config file rather
/// than the file itself. If `--config <path>` is explicit, use its parent as-is.
fn resolve_config_dir(explicit_home: Option<&Path>) -> Result<PathBuf> {
    let path = config::home_config_path(explicit_home)?;
    Ok(path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".")))
}

async fn run_repl(config_path: Option<PathBuf>) -> Result<()> {
    let mut settings = config::load(config_path.as_deref())?;
    // Load secrets from under config_dir (env.json.enc → env.json → env vars fallback).
    let secrets = config::secrets::Secrets::load(&settings.config_dir);
    // Apply `${VAR}` substitution once, here. All later code assumes the expansion is done.
    settings.expand_secrets(&secrets);

    // If default_model is set, resolve it once and cache. Failure (e.g. group missing
    // from config) doesn't block startup — we leave `None` and let the agent surface
    // a clear error when the first turn runs.
    let current_model = settings
        .default_model
        .as_ref()
        .and_then(|r| ActiveModel::resolve(&settings, r).ok());

    let http = reqwest::Client::new();

    // Connect to MCP servers. For each enabled server we run initialize → tools/list.
    // Per-server connection failures are absorbed so startup never blocks (SPEC §14-6).
    let mcp = mcp::McpManager::connect_all(&settings, http.clone()).await;

    // `secrets` has done its job in expand_secrets. Drop it so only the expanded
    // settings remain.
    drop(secrets);

    let mut ctx = repl::context::ReplContext {
        settings,
        session: repl::context::Session::new(),
        http,
        current_model,
        mcp,
    };
    repl::run(&mut ctx).await
}
