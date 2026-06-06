// /config — 現在の設定の確認 + 初期化ウィザード起動（SPEC §10, MILESTONES.md M4, M8）。
//
// サブコマンド:
//   - `show`  : 現在の Settings を YAML で表示。api_key / headers の値はマスク。
//   - `setup` : `config::wizard` を起動し、結果を確認の上 config.yaml に書き出す。
//
// このファイルは「コマンドとしての配線」と「ファイル書き出しのフロー」だけを持つ:
//   - 機密マスクのロジック  → `Settings::redacted()`（`config/types.rs`）
//   - 対話 IO プリミティブ → `repl::prompt`
//   - 収集ドメイン        → `config::wizard`
//
// rustyline の readline 待機中ではないので、wizard 内は `std::io::stdin().lock()` を
// `BufRead` として渡せる（外側の rustyline と衝突しない）。

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
        "`show` で現在の設定を表示 / `setup` で対話的にホーム config.yaml を生成"
    }

    async fn run(&self, args: &str, ctx: &mut ReplContext) -> Result<Outcome> {
        let (sub, _rest) = split_first_token(args.trim());
        match sub {
            "" | "show" => show(&ctx.settings),
            "setup" => setup(ctx),
            other => bail!("不明なサブコマンド: {other}（使い方: /config show | /config setup）"),
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
    // 書き出し先は ctx.settings.config_dir（main.rs でロード済み）配下の config.yaml。
    // `--config <path>` 指定時も config_dir はそのパスの親ディレクトリになっているため、
    // ここで再評価する必要はない。home_config_path(None) を使うと --config 指定が無視される。
    let target = if ctx.settings.config_dir.as_os_str().is_empty() {
        home_config_path(None).context("ホーム config パスの解決に失敗")?
    } else {
        ctx.settings.config_dir.join("config.yaml")
    };

    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();

    println!("aic config setup — 対話的にホーム config.yaml を生成します。");
    println!("書き出し先: {}", target.display());

    if target.exists()
        && !prompt_bool(&mut stdin, "既存ファイルがあります。上書きしますか？", false)?
    {
        println!("setup を中断しました。");
        return Ok(Outcome::Continue);
    }

    let new_settings = wizard::run(&mut stdin, &ctx.settings)?;

    // プレビュー（redact 済み）
    println!("\n=== 生成される config.yaml（秘匿値はマスク表示）===");
    print!("{}", serde_yml::to_string(&new_settings.redacted())?);
    println!("===");

    if !prompt_bool(&mut stdin, "\nこの内容で書き出しますか？", true)? {
        println!("setup を中断しました。");
        return Ok(Outcome::Continue);
    }

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("ディレクトリ作成失敗: {}", parent.display()))?;
    }
    let yaml = serde_yml::to_string(&new_settings)?;
    std::fs::write(&target, yaml)
        .with_context(|| format!("書き出し失敗: {}", target.display()))?;
    println!("wrote: {}", target.display());
    println!("注意: 反映には `aic` の再起動が必要です。secrets は別途 `env.json` を");
    println!(
        "      作成し、必要なら `aic env seal` で `env.json.enc` に封印してください。"
    );

    Ok(Outcome::Continue)
}

inventory::submit! { &Config as &dyn Command }
