// /config — view current configuration and launch the setup wizard (SPEC §10).
//
// Subcommands:
//   - `show`  : print the current Settings as YAML, with api_key / headers masked.
//   - `setup` : start the `config::wizard` and write the result to config.yaml after confirmation.
//
// This file only handles "command wiring" and the file-write flow:
//   - Redaction logic           → `Settings::redacted()` (`config/types.rs`)
//   - Interactive IO primitives → `repl::prompt`
//   - Collection domain         → `config::wizard`
//
// We're not inside rustyline's readline when this command runs, so we can grab
// `std::io::stdin().lock()` as a `BufRead` to feed the wizard (no conflict with
// the outer rustyline editor).

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use super::{split_first_token, Command, Outcome};
use crate::config::{home_config_path, wizard, Settings};
use crate::repl::context::ReplContext;
use crate::repl::prompt::prompt_bool;

struct Config;

#[async_trait]
impl Command for Config {
    fn name(&self) -> &'static str {
        "config"
    }

    fn help(&self) -> &'static str {
        "`show` prints the current config / `setup` runs the interactive wizard"
    }

    async fn run(&self, args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        let (sub, _rest) = split_first_token(args.trim());
        match sub {
            "" | "show" => show(&ctx.settings),
            "setup" => setup(ctx),
            other => bail!("unknown subcommand: {other} (usage: /config show | /config setup)"),
        }
    }
}

fn show(settings: &Settings) -> Result<Outcome> {
    let yaml = serde_yml::to_string(&settings.redacted())?;
    if !settings.config_dir.as_os_str().is_empty() {
        println!("# config_dir: {}", settings.config_dir.display());
    }
    print!("{yaml}");
    Ok(Outcome::Continue)
}

fn setup(ctx: &mut ReplContext) -> Result<Outcome> {
    // The write target is `ctx.settings.config_dir/config.yaml` (loaded in main.rs).
    // When `--config <path>` is explicit, `config_dir` is already the parent of
    // that path, so no recalculation is needed here. Using `home_config_path(None)`
    // would ignore the explicit override.
    let target = if ctx.settings.config_dir.as_os_str().is_empty() {
        home_config_path(None).context("failed to resolve home config path")?
    } else {
        ctx.settings.config_dir.join("config.yaml")
    };

    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();

    println!("aic config setup — interactively generate the home config.yaml.");
    println!("write target: {}", target.display());

    if target.exists()
        && !prompt_bool(&mut stdin, "File already exists. Overwrite?", false)?
    {
        println!("setup aborted.");
        return Ok(Outcome::Continue);
    }

    let new_settings = wizard::run(&mut stdin, &ctx.settings)?;

    // Preview (redacted)
    println!("\n=== resulting config.yaml (secrets masked) ===");
    print!("{}", serde_yml::to_string(&new_settings.redacted())?);
    println!("===");

    if !prompt_bool(&mut stdin, "\nWrite this to disk?", true)? {
        println!("setup aborted.");
        return Ok(Outcome::Continue);
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create directory: {}", parent.display()))?;
    }
    let yaml = serde_yml::to_string(&new_settings)?;
    std::fs::write(&target, yaml)
        .with_context(|| format!("failed to write: {}", target.display()))?;
    println!("wrote: {}", target.display());
    println!("Note: restart `aic` for changes to take effect. Set up secrets separately");
    println!(
        "      via `env.json` and, if you want, `aic env seal` to seal them into `env.json.enc`."
    );

    Ok(Outcome::Continue)
}

inventory::submit! { &Config as &dyn Command }
