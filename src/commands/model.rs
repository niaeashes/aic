// /model — モデル一覧と切り替え（SPEC §10, MILESTONES.md M4）。
//
// サブコマンド:
//   - 引数なし: 設定済みグループ／モデルを `<group>:<model>` 形式で列挙。
//                現在の `current_model` には `*` を付与する。
//   - `use <group>:<model>`: `current_model` を切り替える。
//                config に該当グループ／モデルが無ければエラー（ループは継続）。
//
// `:` を含むモデル名（例 `local:qwen2.5-coder:32b`）を扱うため、引数のパースは
// 必ず `ModelRef::parse`（内部で `splitn(2, ':')`）に委譲する。

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
        "モデル一覧 / `use <group>:<model>` で切り替え"
    }

    async fn run(&self, args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        let trimmed = args.trim();
        if trimmed.is_empty() {
            print_list(ctx);
            return Ok(Outcome::Continue);
        }

        // `use <ref>` の形のみサポート。将来サブコマンドが増えたら match に展開する。
        let (sub, rest) = split_first_token(trimmed);
        match sub {
            "use" => use_model(rest, ctx),
            other => {
                bail!("不明なサブコマンド: {other}（使い方: /model | /model use <group>:<model>）")
            }
        }
    }
}

fn print_list(ctx: &ReplContext) {
    let current_label = ctx.current_model.as_ref().map(|a| a.label());
    if ctx.settings.model_groups.is_empty() {
        println!("（model_groups が未設定）");
        return;
    }
    for group in &ctx.settings.model_groups {
        if group.models.is_empty() {
            println!("{}: （モデル未登録）", group.name);
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
        bail!("使い方: /model use <group>:<model>");
    }
    let model_ref = ModelRef::parse(arg)?;
    // resolve が group 存在・model リスト登録の両方を検証してエラーを返す。
    let active = ActiveModel::resolve(&ctx.settings, &model_ref)?;
    println!("モデルを切り替えました: {}", active.label());
    ctx.current_model = Some(active);
    Ok(Outcome::Continue)
}

inventory::submit! { &Model as &dyn Command }
