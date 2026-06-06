// repl — rustyline ベースの対話ループ（SPEC §9.1）。
//
// M3 で `/`-始まりはすべて `dispatch::dispatch` に丸投げするようリファクタした。
// `/exit` も Command として登録されており、`Outcome::Exit` を見てループを抜ける。
//
// rustyline の readline は同期ブロッキングだが、`#[tokio::main]` の multi-thread
// ランタイム上で呼ぶので、1 ワーカーをブロックするだけで他のタスクは進める。
// 単一ユーザ対話 CLI として割り切る。
//
// M8: 履歴ファイルを `config_dir/history.txt` に保存する。起動時に load、終了時に
// save。`history_size` で件数上限を引く。読み書き失敗は警告のみで継続。

use std::path::PathBuf;

use anyhow::Result;
use rustyline::error::ReadlineError;
use rustyline::{Config, DefaultEditor};

use crate::commands::Outcome;
use crate::repl::context::ReplContext;
use crate::repl::view::TerminalView;

pub mod context;
pub mod dispatch;
pub mod prompt;
pub mod view;

const PROMPT: &str = "aic> ";
const HISTORY_FILE: &str = "history.txt";

pub async fn run(ctx: &mut ReplContext) -> Result<()> {
    let cfg = Config::builder()
        .max_history_size(ctx.settings.ui.history_size)?
        .auto_add_history(false) // 自分で `add_history_entry` する
        .build();
    let mut rl = DefaultEditor::with_config(cfg)?;

    // 履歴ファイルのパスは config_dir 配下。config_dir が空（テスト等）なら無効化。
    let history_path: Option<PathBuf> = if ctx.settings.config_dir.as_os_str().is_empty() {
        None
    } else {
        Some(ctx.settings.config_dir.join(HISTORY_FILE))
    };
    if let Some(p) = history_path.as_deref() {
        if p.exists() {
            if let Err(e) = rl.load_history(p) {
                tracing::warn!("history 読み込み失敗（{}）: {e:#}", p.display());
            }
        }
    }

    let result = run_loop(ctx, &mut rl).await;

    // 終了経路（正常 / エラー）にかかわらず履歴は保存しておきたい。
    if let Some(p) = history_path.as_deref() {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = rl.save_history(p) {
            tracing::warn!("history 保存失敗（{}）: {e:#}", p.display());
        }
    }
    result
}

async fn run_loop(ctx: &mut ReplContext, rl: &mut DefaultEditor) -> Result<()> {
    // 端末描画は 1 つの TerminalView に集約。per-message 状態は assistant_start で
    // 都度リセットされるため、セッション通しで使い回してよい。
    let mut view = TerminalView::new();
    loop {
        match rl.readline(PROMPT) {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // 直前と同じ入力は履歴に積まない（rustyline の HistoryEntry::Smart 相当）
                let dedup = match rl.history().iter().next_back() {
                    Some(last) => last == trimmed,
                    None => false,
                };
                if !dedup {
                    let _ = rl.add_history_entry(trimmed);
                }

                if let Some(body) = trimmed.strip_prefix('/') {
                    match dispatch::dispatch(body, ctx).await? {
                        Outcome::Continue => continue,
                        Outcome::Exit => break,
                    }
                }

                // チャット入力 → エージェントループ
                if let Err(e) = crate::agent::run_turn(ctx, trimmed.to_string(), &mut view).await {
                    eprintln!("error: {e:#}");
                }
            }
            // Ctrl-C は入力をキャンセルしてプロンプトに戻る
            Err(ReadlineError::Interrupted) => continue,
            // Ctrl-D は終了
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                eprintln!("error: 入力の読み取りに失敗: {e}");
                break;
            }
        }
    }
    Ok(())
}
