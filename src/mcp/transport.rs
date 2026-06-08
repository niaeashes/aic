// transport — MCP Streamable HTTP (SPEC §7.1, §7.2, §7.3).
//
// One POST endpoint serves two response shapes, distinguished by Content-Type:
//   - `application/json`     … one JSON-RPC response in one body
//   - `text/event-stream`    … SSE frames carrying one or more JSON-RPC messages
//
// The client always sends `Accept: application/json, text/event-stream`.
// `Mcp-Session-Id`, if the server returned one in the initialize response, is
// echoed back on every subsequent request (SPEC §7.3). `MCP-Protocol-Version`
// is sent on every request.
//
// The SSE parser is intentionally minimal — "extract `data:` lines → parse each
// message via serde_json into JSON-RPC" — because MCP responses are mostly
// single-roundtrip and we can read the whole body first (unlike the LLM SSE
// path, no `eventsource-stream` needed here).

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::Serialize;
use serde_json::{json, Value};

use crate::mcp::protocol::{JsonRpcResponse, PROTOCOL_VERSION};

/// One MCP server's transport state.
///
/// `session_id` and `next_id` mutate, hence `&mut self`. At the app level
/// the transport is wired through `ReplContext`, never as a global (SPEC §11).
pub struct Transport {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    http: reqwest::Client,
    /// Value of `Mcp-Session-Id` from the initialize response (echoed on later requests).
    session_id: Option<String>,
    /// Monotonically increasing JSON-RPC request id.
    next_id: i64,
}

impl Transport {
    pub fn new(url: String, headers: BTreeMap<String, String>, http: reqwest::Client) -> Self {
        Self {
            url,
            headers,
            http,
            session_id: None,
            next_id: 0,
        }
    }

    /// Debugging hook — peek at the session id (used by tests and a future
    /// `/config show` extension).
    #[allow(dead_code)]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn next_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    // -----------------------------------------------------------------------
    // Public RPC API
    // -----------------------------------------------------------------------

    /// Standard request/response. Returns the raw `result` as `Value`.
    /// JSON-RPC errors `bail!` here; server-originated notification frames are skipped.
    pub async fn request<P: Serialize>(&mut self, method: &str, params: P) -> Result<Value> {
        let id = self.next_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let (status, ct, text) = self.post_raw(&body).await?;
        if status == reqwest::StatusCode::ACCEPTED {
            // 202 on a regular request is not expected per spec.
            bail!("MCP {method}: got 202 but no body");
        }
        let resp = parse_response_payload(&ct, &text)
            .with_context(|| format!("failed to parse MCP {method} response"))?;
        if let Some(e) = resp.error {
            bail!("MCP {method} error {}: {}", e.code, e.message);
        }
        resp.result
            .ok_or_else(|| anyhow!("MCP {method}: frame has neither result nor error"))
    }

    /// Notification (no id, no response expected). HTTP 200/202 both count as success.
    pub async fn notify<P: Serialize>(&mut self, method: &str, params: P) -> Result<()> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        // For notifications we discard the response body either way (any
        // server-originated notification mixed in is ignored at the MVP).
        self.post_raw(&body).await.map(|_| ())
    }

    // -----------------------------------------------------------------------
    // Internal: POST + header management
    // -----------------------------------------------------------------------

    /// Single POST. Returns (status, content-type, body).
    /// If `Mcp-Session-Id` appears in the response headers, we cache it.
    async fn post_raw(&mut self, body: &Value) -> Result<(reqwest::StatusCode, String, String)> {
        let mut req = self
            .http
            .post(&self.url)
            // Accept both MIME types (SPEC §7.2).
            .header(ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(body);
        if let Some(sid) = &self.session_id {
            req = req.header("Mcp-Session-Id", sid);
        }
        // User-configured headers (`${VAR}` already expanded at startup). If a
        // collision happens these win.
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("MCP POST failed: {}", self.url))?;
        let status = resp.status();
        // Pick up Mcp-Session-Id if present (no-op if absent).
        if let Some(v) = resp
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|h| h.to_str().ok())
        {
            self.session_id = Some(v.to_string());
        }
        let ct = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("MCP HTTP {}: {}", status.as_u16(), body);
        }
        let text = resp
            .text()
            .await
            .with_context(|| "failed to read MCP response body")?;
        Ok((status, ct, text))
    }
}

// ---------------------------------------------------------------------------
// Response parsing — Content-Type branching, test hook
// ---------------------------------------------------------------------------

/// Branch on Content-Type and parse the body as either a single JSON-RPC
/// response or an SSE stream. Returns the first frame that carries `result`
/// or `error`. Has no IO side effects, so it's unit-testable.
pub fn parse_response_payload(content_type: &str, body: &str) -> Result<JsonRpcResponse> {
    if body.is_empty() {
        bail!("empty response body");
    }
    if content_type.contains("text/event-stream") {
        parse_sse_jsonrpc(body)
    } else {
        // application/json or unknown (last-resort: treat as a single JSON).
        let resp: JsonRpcResponse = serde_json::from_str(body)
            .with_context(|| format!("failed to parse JSON-RPC single-shot: {body}"))?;
        Ok(resp)
    }
}

/// From an SSE body, return the first message carrying `result` or `error`.
/// Server-originated notifications (no id, has method) are skipped.
pub fn parse_sse_jsonrpc(text: &str) -> Result<JsonRpcResponse> {
    for msg in extract_sse_data(text) {
        let resp: JsonRpcResponse = match serde_json::from_str(&msg) {
            Ok(r) => r,
            Err(e) => {
                // One broken frame shouldn't prevent picking up a later valid frame.
                eprintln!("warning: failed to parse MCP SSE frame: {e} (skipping)");
                continue;
            }
        };
        if resp.result.is_some() || resp.error.is_some() {
            return Ok(resp);
        }
    }
    bail!("MCP SSE: no message with result/error")
}

/// Split a raw SSE body into one string per event, joining repeated `data:`
/// lines with `\n`.
///
/// Rules:
///   - Events are separated by a blank line (`\n\n`)
///   - Multiple `data:` lines within one event are joined by `\n`
///   - `event:` / `id:` / `retry:` are ignored (MCP doesn't use them)
///   - At most one literal space right after `data:` is stripped (SSE spec)
pub fn extract_sse_data(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Normalize CRLF before splitting.
    let normalized = text.replace("\r\n", "\n");
    for block in normalized.split("\n\n") {
        let mut buf = String::new();
        for line in block.split('\n') {
            if let Some(rest) = line.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if !buf.is_empty() {
                    buf.push('\n');
                }
                buf.push_str(rest);
            }
        }
        if !buf.is_empty() {
            out.push(buf);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests — parser only (the HTTP layer is verified by manual REPL runs at MVP)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_sse_data_joins_multi_data_lines() {
        let text = "data: {\"a\":\ndata: 1}\n\n";
        let v = extract_sse_data(text);
        assert_eq!(v, vec!["{\"a\":\n1}".to_string()]);
    }

    #[test]
    fn extract_sse_data_splits_on_blank_lines() {
        let text = "event: message\ndata: one\n\ndata: two\n\n";
        let v = extract_sse_data(text);
        assert_eq!(v, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn parse_response_handles_single_json_tools_list() {
        // application/json single-shot: `result.tools` is visible.
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"search","inputSchema":{"type":"object"}}]}}"#;
        let r = parse_response_payload("application/json", body).unwrap();
        let result = r.result.unwrap();
        assert_eq!(result["tools"][0]["name"], "search");
    }

    #[test]
    fn parse_response_handles_sse_tools_list_after_notification() {
        // SSE: even with a leading server notification, the result frame is found.
        let body = "event: message\n\
                    data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/log\",\"params\":{}}\n\n\
                    event: message\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[{\"name\":\"fetch\",\"inputSchema\":{\"type\":\"object\"}}]}}\n\n";
        let r = parse_response_payload("text/event-stream; charset=utf-8", body).unwrap();
        let result = r.result.unwrap();
        assert_eq!(result["tools"][0]["name"], "fetch");
    }

    #[test]
    fn parse_response_propagates_jsonrpc_error() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"Method not found"}}"#;
        let r = parse_response_payload("application/json", body).unwrap();
        assert!(r.error.is_some());
        assert!(r.result.is_none());
    }

    #[test]
    fn parse_sse_jsonrpc_fails_when_only_notifications() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/log\",\"params\":{}}\n\n";
        assert!(parse_sse_jsonrpc(body).is_err());
    }
}
