// aic — minimal interactive chat CLI
//
// 起動シーケンス:
//   1. tracing 初期化（`RUST_LOG` で詳細度調整可、既定は warn）
//   2. clap で引数解析（`aic`, `aic env seal|unseal`, `aic --config <path>`）
//   3. config をホーム + プロジェクトの 2 層浅マージでロード（M1）
//   4. config_dir から secrets をロード（M5: env.json.enc → env.json → 環境変数）
//   5. Settings の `${VAR}` を全フィールドで展開（M5）
//   6. ReplContext を組み立てて REPL ループへ
//
// `env seal|unseal` は REPL を立ち上げずに secrets ファイルを操作して終了する。

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

mod agent;
mod commands;
mod config;
mod llm;
mod mcp;
mod repl;

#[derive(Debug, Parser)]
#[command(name = "aic", about = "minimal interactive chat CLI")]
struct Cli {
    /// 明示的に使うホーム config ファイルパス（既定は `$AIC_CONFIG_DIR` か `~/.config/aic/config.yaml`）
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// secrets ファイル (env.json / env.json.enc) の管理（SPEC §5, M5）
    Env {
        #[command(subcommand)]
        action: EnvAction,
    },
}

#[derive(Debug, Subcommand)]
enum EnvAction {
    /// env.json → env.json.enc（鍵は Keychain へ。既存鍵があれば再利用）
    Seal,
    /// env.json.enc → env.json（編集用に平文を取り出す）
    Unseal,
}

#[tokio::main]
async fn main() -> Result<()> {
    // `RUST_LOG` 未指定時は warn 以上のみ表示。`RUST_LOG=aic=debug` 等で詳細化できる。
    // フィルタが構築失敗するのは想定外なので、unwrap_or_default で fall through。
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

/// `aic env ...` 用に config ファイルではなくその「親ディレクトリ」だけを返す。
/// `aic --config <path>` で明示指定があれば、その親をそのまま使う。
fn resolve_config_dir(explicit_home: Option<&Path>) -> Result<PathBuf> {
    let path = config::home_config_path(explicit_home)?;
    Ok(path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".")))
}

async fn run_repl(config_path: Option<PathBuf>) -> Result<()> {
    let mut settings = config::load(config_path.as_deref())?;
    // secrets は config_dir 配下から拾う（env.json.enc → env.json → 環境変数のフォールバック）
    let secrets = config::secrets::Secrets::load(&settings.config_dir);
    // ここで一度だけ `${VAR}` を全フィールドに適用する。以降の利用箇所は展開済み前提。
    settings.expand_secrets(&secrets);

    let current_model = settings.default_model.clone();
    let http = reqwest::Client::new();

    // MCP 接続。enabled サーバごとに initialize → tools/list を順に試す。
    // 接続失敗は per-server で握りつぶされるので、起動は止まらない（SPEC §14-6）。
    let mcp = mcp::McpManager::connect_all(&settings, http.clone()).await;

    let mut ctx = repl::context::ReplContext {
        settings,
        session: repl::context::Session::new(),
        http,
        secrets,
        current_model,
        mcp,
    };
    repl::run(&mut ctx).await
}
