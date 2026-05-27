// transport — MCP Streamable HTTP（SPEC §7.1, §7.2, §7.3）。
//
// 1 つの POST エンドポイントに対し、Content-Type で 2 種類の応答を区別する:
//   - `application/json`     … JSON-RPC レスポンスを 1 件丸ごと返す
//   - `text/event-stream`    … SSE フレームで 1 件以上の JSON-RPC メッセージを流す
//
// クライアントは常に `Accept: application/json, text/event-stream` を送る。
// `Mcp-Session-Id` はサーバが initialize 応答ヘッダで返した値を以降の全リクエストで
// echo back する（SPEC §7.3）。`MCP-Protocol-Version` も毎回付ける。
//
// SSE パーサは「`data:` 行抽出 → 各メッセージごとに `serde_json` で JSON-RPC へ」と
// いう薄い実装で済ませる（M2 の LLM SSE と違い、`eventsource-stream` を持ち込まなく
// てもよい — MCP の応答は 1 ロード分を読み切ってから処理する単発系が中心）。

use std::collections::BTreeMap;

use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use serde::Serialize;
use serde_json::{json, Value};

use crate::mcp::protocol::{JsonRpcResponse, PROTOCOL_VERSION};

/// 1 つの MCP サーバ向け接続状態。
///
/// `session_id` と `next_id` を変えるので `&mut self` で受ける。アプリ全体としては
/// `ReplContext` 経由で配線するため、グローバル共有状態にはならない（SPEC §11）。
pub struct Transport {
    pub url: String,
    pub headers: BTreeMap<String, String>,
    http: reqwest::Client,
    /// initialize 応答で `Mcp-Session-Id` が返った場合の値（以降のリクエストで echo back）。
    session_id: Option<String>,
    /// 単調増加 JSON-RPC リクエスト ID。
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

    /// デバッグ用にセッション ID を覗ける（テストや /config show の将来拡張で使う想定）。
    #[allow(dead_code)]
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn next_id(&mut self) -> i64 {
        self.next_id += 1;
        self.next_id
    }

    // -----------------------------------------------------------------------
    // パブリック RPC API
    // -----------------------------------------------------------------------

    /// 通常の request/response。`result` を生 `Value` で返す。
    /// JSON-RPC error はここで `bail!` し、サーバ側通知のフレームは読み飛ばす。
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
            // 通常の request に 202 が返ってくることは仕様上想定外。
            bail!("MCP {method}: 応答に 202 が返ったが、body が無い");
        }
        let resp = parse_response_payload(&ct, &text)
            .with_context(|| format!("MCP {method} 応答のパースに失敗"))?;
        if let Some(e) = resp.error {
            bail!("MCP {method} error {}: {}", e.code, e.message);
        }
        resp.result
            .ok_or_else(|| anyhow!("MCP {method}: result も error も無いフレーム"))
    }

    /// 通知（id 無し、応答不要）。HTTP 上は 200/202 のどちらでも成功扱い。
    pub async fn notify<P: Serialize>(&mut self, method: &str, params: P) -> Result<()> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        // 通知では body の有無を問わずレスポンス内容は捨てる（サーバ起点 notification が
        // 混ざる可能性があるが、MVP では拾わない）。
        self.post_raw(&body).await.map(|_| ())
    }

    // -----------------------------------------------------------------------
    // 内部: POST + ヘッダ管理
    // -----------------------------------------------------------------------

    /// POST 1 発、(status, content-type, body) を返す。
    /// `Mcp-Session-Id` がレスポンスヘッダに付いていれば自身に保存する。
    async fn post_raw(&mut self, body: &Value) -> Result<(reqwest::StatusCode, String, String)> {
        let mut req = self
            .http
            .post(&self.url)
            // 両方の MIME を受け入れる宣言（SPEC §7.2）
            .header(ACCEPT, "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(body);
        if let Some(sid) = &self.session_id {
            req = req.header("Mcp-Session-Id", sid);
        }
        // ユーザ設定ヘッダ（${VAR} は起動時展開済み）。同名キーがあればこちらで上書き。
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }

        let resp = req
            .send()
            .await
            .with_context(|| format!("MCP POST 失敗: {}", self.url))?;
        let status = resp.status();
        // セッション ID 取得（無ければそのまま）。
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
            .with_context(|| "MCP レスポンス本文の読み取り失敗")?;
        Ok((status, ct, text))
    }
}

// ---------------------------------------------------------------------------
// 応答パース — Content-Type 分岐とテストフック
// ---------------------------------------------------------------------------

/// Content-Type を見て JSON 単発 / SSE のどちらかでパースし、
/// `result` か `error` を持つ最初のフレームを返す。
/// 通信副作用が無いので単体テストから直接叩ける。
pub fn parse_response_payload(content_type: &str, body: &str) -> Result<JsonRpcResponse> {
    if body.is_empty() {
        bail!("レスポンス本文が空");
    }
    if content_type.contains("text/event-stream") {
        parse_sse_jsonrpc(body)
    } else {
        // application/json または不明（最後の砦として単発 JSON 扱い）
        let resp: JsonRpcResponse = serde_json::from_str(body)
            .with_context(|| format!("JSON-RPC 単発パース失敗: {body}"))?;
        Ok(resp)
    }
}

/// SSE 本文から最初の `result` または `error` を持つメッセージを返す。
/// サーバ起点 notification（id 無し、method あり）はスキップする。
pub fn parse_sse_jsonrpc(text: &str) -> Result<JsonRpcResponse> {
    for msg in extract_sse_data(text) {
        let resp: JsonRpcResponse = match serde_json::from_str(&msg) {
            Ok(r) => r,
            Err(e) => {
                // 1 フレーム壊れていても他フレームで救えるよう続行
                eprintln!("warning: MCP SSE フレームのパースに失敗: {e}（スキップ）");
                continue;
            }
        };
        if resp.result.is_some() || resp.error.is_some() {
            return Ok(resp);
        }
    }
    bail!("MCP SSE: result/error を含むメッセージが無い");
}

/// 生 SSE 本文を `data:` 行ベースに分解する。
///
/// 仕様:
///   - イベントは空行 (`\n\n`) で区切る
///   - 1 イベント内の複数 `data:` 行は `\n` で連結
///   - `event:` `id:` `retry:` は無視（MCP では使わない）
///   - `data:` の直後の半角スペース 1 個だけは食わせる（SSE 仕様）
pub fn extract_sse_data(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // CRLF を LF に正規化してから分割
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
// テスト — パーサ部のみ（HTTP 部はモック無しで MVP 段階は手動 REPL 確認）
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
        // application/json 単発: `result.tools` が見える
        let body = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"search","inputSchema":{"type":"object"}}]}}"#;
        let r = parse_response_payload("application/json", body).unwrap();
        let result = r.result.unwrap();
        assert_eq!(result["tools"][0]["name"], "search");
    }

    #[test]
    fn parse_response_handles_sse_tools_list_after_notification() {
        // SSE: 先頭にサーバ通知が来ても、result/error フレームを拾える
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
