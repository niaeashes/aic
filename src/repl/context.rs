// context — Session / ReplContext（SPEC §11）。
//
// グローバル状態を持たず、REPL ループから `&mut ReplContext` で受け渡す方針。
// ライフタイムを構造体に持たせない（保持は `String` / 所有型のみ）。
//
// `Secrets` は main.rs で `expand_secrets` を呼び出した後は不要になるため、
// `ReplContext` には含めない。展開済みの Settings だけを保持する。

use crate::config::{ModelRef, Settings};
use crate::llm::types::Message;
use crate::mcp::McpManager;

/// 会話履歴。OpenAI 互換のメッセージ列をそのまま保持する。
///
/// `/clear` で消されるのはここ（モデル選択や MCP 接続は維持される）。
#[derive(Debug, Default)]
pub struct Session {
    pub messages: Vec<Message>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }
}

/// REPL/コマンド/エージェントが共通に触る状態の束。
///
/// 全フィールドは `&mut ReplContext` 経由で受け渡し、グローバル共有しない（SPEC §11）。
pub struct ReplContext {
    pub settings: Settings,
    pub session: Session,
    pub http: reqwest::Client,
    /// 現在使用中のモデル。config に `default_model` が無ければ None で起動し、
    /// `/model use` で確定する想定（M4）。`agent::run_turn` が None だとエラーで弾く。
    pub current_model: Option<ModelRef>,
    /// MCP サーバ群 + 公開ツールカタログ（M6）。
    /// 起動時の接続失敗は per-server で握りつぶし、空でも REPL は回る。
    pub mcp: McpManager,
}
