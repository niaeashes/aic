// protocol — minimal types for JSON-RPC 2.0 and MCP methods (SPEC §7).
//
// `aic` only needs four methods:
//   - initialize / notifications/initialized
//   - tools/list
//   - tools/call
// Anything else the server sends (notifications/*) is dropped on the floor.
//
// On the receive side (`JsonRpcResponse`), `id` / `result` / `error` / `method`
// are all optional. Server-originated notifications (no id) and ordinary
// responses (with id) can both arrive on the same SSE; we tell them apart with
// `result.is_some() || error.is_some()` ("is this my response?").

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The MCP protocol version we declare. SPEC §7.1.
///
/// If the server responds with a different version, we keep going as long as
/// `initialize` succeeded (no capability negotiation in the MVP).
pub const PROTOCOL_VERSION: &str = "2025-03-26";

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 — outbound / inbound
// ---------------------------------------------------------------------------

/// A received JSON-RPC message (response or server→client notification).
///
/// Because MCP's SSE may mix "responses to us" with "server-originated
/// notifications", `method` is kept around so the latter can be filtered out.
///
/// `id` / `method` aren't used for filtering today (`result/error` presence is
/// enough), but we keep them for JSON-RPC compatibility.
#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcResponse {
    #[serde(default)]
    #[allow(dead_code)]
    pub id: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<JsonRpcError>,
    /// Filled when the frame is a server-originated request/notification (aic ignores them).
    #[serde(default)]
    #[allow(dead_code)]
    pub method: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    /// The optional JSON-RPC `data` field. Today we only display the message,
    /// but we keep this around for future detailed display.
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
    /// Empty object in the MVP. We don't negotiate beyond tools, so no capability negotiation is needed.
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
    /// Pagination cursor. In the MVP we only fetch the first page and stop.
    #[serde(default, rename = "nextCursor")]
    #[allow(dead_code)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema. Plumbed through to OpenAI tools' `function.parameters` verbatim.
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
    /// The LLM's `function.arguments` (JSON string) parsed into a `Value`.
    pub arguments: Value,
}

#[derive(Debug, Deserialize)]
pub struct ToolsCallResult {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    /// When `true`, the tool itself reported an error. We don't turn this into
    /// an Err: we let the agent loop continue so the model can read the message.
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

/// An MCP content block. We only pick out `text`; other types are dropped.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    /// image / resource / unknown types are collapsed into a unitless variant.
    /// They produce no extracted text, which is fine for the text-only flow today.
    #[serde(other)]
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tools_list_result() {
        // A typical MCP tools/list response body (the `result` portion).
        let v = serde_json::json!({
            "tools": [
                {
                    "name": "search",
                    "description": "Search.",
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
        assert_eq!(r.tools[0].description.as_deref(), Some("Search."));
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
        // 2 text + 1 image (Other).
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
        // A server-originated notification (no id, has method, no result/error).
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
