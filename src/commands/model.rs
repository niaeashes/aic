// /model — list models and switch the current one (SPEC §10).
//
// Subcommands:
//   - no args: list all configured groups/models as `<group>:<model>`. The
//              current `current_model` is marked with `*`.
//   - `use <group>:<model>`: switch the current model. If the group/model is
//              not in config, error out (the loop keeps running).
//
// Argument parsing must always go through `ModelRef::parse` (which uses
// `splitn(2, ':')`) so model names containing `:` (e.g.
// `local:qwen2.5-coder:32b`) are handled correctly.

use anyhow::{bail, Result};
use async_trait::async_trait;

use super::{split_first_token, Command, Outcome};
use crate::active_model::ActiveModel;
use crate::config::ModelRef;
use crate::repl::context::ReplContext;

struct Model;

#[async_trait]
impl Command for Model {
    fn name(&self) -> &'static str {
        "model"
    }

    fn help(&self) -> &'static str {
        "List models / switch via `use <group>:<model>`"
    }

    async fn run(&self, args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            print_list(ctx);
            return Ok(Outcome::Continue);
        }

        // Only `use <ref>` is supported today. Add more subcommands here via a
        // match if we grow the surface.
        let (sub, rest) = split_first_token(trimmed);
        match sub {
            "use" => use_model(rest, ctx),
            other => {
                bail!("unknown subcommand: {other} (usage: /model | /model use <group>:<model>)")
            }
        }
    }
}

fn print_list(ctx: &ReplContext) {
    let current_label = ctx.current_model.as_ref().map(|a| a.label());
    if ctx.settings.model_groups.is_empty() {
        println!("(no model_groups configured)");
        return;
    }
    for group in &ctx.settings.model_groups {
        if group.models.is_empty() {
            println!("{}: (no models registered)", group.name);
            continue;
        }
        for model in &group.models {
            let label = format!("{}:{}", group.name, model);
            let mark = if current_label.as_deref() == Some(&label) { "*" } else { " " };
            println!("{mark} {label}");
        }
    }
}

fn use_model(arg: &str, ctx: &mut ReplContext) -> Result<Outcome> {
    if arg.is_empty() {
        bail!("usage: /model use <group>:<model>");
    }
    let model_ref = ModelRef::parse(arg)?;
    // `resolve` validates both group presence and model registration.
    let active = ActiveModel::resolve(&ctx.settings, &model_ref)?;
    println!("Switched model: {}", active.label());
    ctx.current_model = Some(active);
    Ok(Outcome::Continue)
}

inventory::submit! { &Model as &dyn Command }
