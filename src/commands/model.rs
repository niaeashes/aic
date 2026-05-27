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

use super::{Command, Outcome};
use crate::config::ModelRef;  // ユーザー入力のパースに引き続き必要
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
        let (sub, rest) = match trimmed.find(char::is_whitespace) {
            Some(idx) => (&trimmed[..idx], trimmed[idx + 1..].trim()),
            None => (trimmed, ""),
        };
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
    if !ctx.settings.model_exists(&model_ref.group, &model_ref.model) {
        // group の有無で文面を分け、ユーザに次の一手を分かりやすく示す
        if ctx.settings.group_by_name(&model_ref.group).is_none() {
            bail!(
                "model group '{}' が config に存在しません（`/model` で一覧）",
                model_ref.group
            );
        }
        bail!(
            "モデル '{}' は group '{}' に登録されていません（`/model` で一覧）",
            model_ref.model,
            model_ref.group
        );
    }
    // Settings から ActiveModel を解決してキャッシュ。以降の agent はこれを直接使う。
    let active = ctx.settings.activate_model(&model_ref)?;
    println!("モデルを切り替えました: {}", active.label());
    ctx.current_model = Some(active);
    Ok(Outcome::Continue)
}

inventory::submit! { &Model as &dyn Command }
