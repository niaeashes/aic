// protocol — JSON-RPC 2.0 と MCP メソッドの最小型（SPEC §7）。
//
// `aic` が必要なのは:
//   - initialize / notifications/initialized
//   - tools/list
//   - tools/call
// の 4 つだけ。サーバが余計に送ってくる notifications/* は無視する。
//
// 受信側 (`JsonRpcResponse`) は `id` / `result` / `error` / `method` を全部 Optional に
// しておく。サーバ起動オリジン通知（id 無し）と通常応答（id 有り）の双方が同じ SSE
// 上に流れてきても、`result.is_some() || error.is_some()` で「自分の応答かどうか」を
// 判定できるようにするため。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// クライアントが宣言する MCP プロトコルバージョン。SPEC §7.1。
///
/// サーバが別バージョンで応答してきても、`initialize` が成功する限りそのまま続行する
/// （細かな capabilities 交渉は MVP では行わない）。
pub const PROTOCOL_VERSION: &str = "2025-03-26";

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 — 送信用 / 受信用
// ---------------------------------------------------------------------------

/// 受信側 JSON-RPC メッセージ（response / server→client notification 兼用）。
///
/// MCP の SSE では同じストリームに「自分への応答」と「サーバ起点 notification」が
/// 混ざる可能性があるため、`method` が入っているフレームは notification として
/// スキップ判定できるようにしておく。
///
/// `id` / `method` は今は filter 判定に使っていない（`result/error` の有無で済む）が、
/// JSON-RPC 互換のため保持する。
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    #[serde(default)]
    #[allow(dead_code)]
    pub id: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
    /// サーバ起点の request / notification ではここが入る（aic は無視する）。
    #[serde(default)]
    #[allow(dead_code)]
    pub method: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    /// JSON-RPC 仕様の任意フィールド。今はメッセージしか表示していないが、
    /// 将来詳細表示に使う想定で残しておく。
    #[serde(default)]
    #[allow(dead_code)]
    pub data: Option<Value>,
}

// ---------------------------------------------------------------------------
// initialize
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// MVP では空オブジェクト固定。tools 以外の機能を使わないので capabilities 交渉は無い。
    pub capabilities: Value,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

#[derive(Debug, Serialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

// ---------------------------------------------------------------------------
// tools/list
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ToolsListResult {
    #[serde(default)]
    pub tools: Vec<McpToolDef>,
    /// ページング指示。MVP では 1 ページ目だけ取って打ち切る。
    #[serde(default, rename = "nextCursor")]
    #[allow(dead_code)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema。OpenAI tools の `function.parameters` にそのまま流し込む。
    #[serde(default = "default_schema", rename = "inputSchema")]
    pub input_schema: Value,
}

fn default_schema() -> Value {
    serde_json::json!({ "type": "object" })
}

// ---------------------------------------------------------------------------
// tools/call
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct ToolsCallParams<'a> {
    pub name: &'a str,
    /// LLM が返してくる `function.arguments`（JSON 文字列）を `Value` にパースしたもの。
    pub arguments: Value,
}

#[derive(Debug, Deserialize)]
pub struct ToolsCallResult {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    /// `true` ならツール側エラー扱い。ループ継続のため Err にはせず文面だけ拾う設計（M7 で参照）。
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

/// MCP の content block。`text` のみ取り出し、他種は捨てる。
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    /// image / resource / 未知のタイプは情報を持たないユニットに落とす。
    /// 取り出し側で空欄になるが、M6 段階ではテキスト用途で十分。
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tools_list_result() {
        // 典型的な MCP tools/list 応答（result 部分のみ）。
        let v = serde_json::json!({
            "tools": [
                {
                    "name": "search",
                    "description": "検索する",
                    "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}
                },
                {
                    "name": "fetch",
                    "inputSchema": {"type": "object"}
                }
            ]
        });
        let r: ToolsListResult = serde_json::from_value(v).unwrap();
        assert_eq!(r.tools.len(), 2);
        assert_eq!(r.tools[0].name, "search");
        assert_eq!(r.tools[0].description.as_deref(), Some("検索する"));
        assert!(r.tools[1].description.is_none());
    }

    #[test]
    fn parses_tools_call_text_content() {
        let v = serde_json::json!({
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "image", "data": "..."},
                {"type": "text", "text": "world"}
            ],
            "isError": false
        });
        let r: ToolsCallResult = serde_json::from_value(v).unwrap();
        // text 2 件、image 1 件（Other に落ちる）
        assert_eq!(r.content.len(), 3);
        let texts: Vec<&str> = r
            .content
            .iter()
            .filter_map(|c| match c {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::Other => None,
            })
            .collect();
        assert_eq!(texts, vec!["hello", "world"]);
        assert!(!r.is_error);
    }

    #[test]
    fn jsonrpc_response_parses_server_notification() {
        // サーバ起点 notification（id 無し、method あり、result/error 無し）。
        let v = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {"level": "info", "data": "hi"}
        });
        let r: JsonRpcResponse = serde_json::from_value(v).unwrap();
        assert!(r.id.is_none());
        assert!(r.result.is_none());
        assert!(r.error.is_none());
        assert_eq!(r.method.as_deref(), Some("notifications/message"));
    }

    #[test]
    fn jsonrpc_response_parses_error_envelope() {
        let v = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "error": {"code": -32601, "message": "Method not found"}
        });
        let r: JsonRpcResponse = serde_json::from_value(v).unwrap();
        let e = r.error.unwrap();
        assert_eq!(e.code, -32601);
        assert_eq!(e.message, "Method not found");
    }
}
