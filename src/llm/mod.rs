// llm — openai-compatible /chat/completions クライアント（SPEC §6, §14-2）。
//
// 「provider 抽象化はしない」方針（SPEC §1, §14）に従い、ChatClient はトレイトでは
// なく具体型 1 つのみ。グループ別の base_url / api_key / headers は呼び出し側が
// 解決して引数で渡す（Settings から引くのは agent.rs の責務）。

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures_util::Stream;

use crate::llm::stream::{parse_stream, StreamEvent};
use crate::llm::types::ChatRequest;

pub mod stream;
pub mod types;

/// HTTP クライアントを共有するための薄いラッパ。
///
/// `reqwest::Client` 自体が Arc 内部状態なので、`ChatClient` は使い捨てで構わない。
#[derive(Debug, Clone)]
pub struct ChatClient {
    http: reqwest::Client,
}

impl ChatClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// `{endpoint}` に `ChatRequest` を POST し、SSE をパースしたストリームを返す。
    ///
    /// 失敗パターン:
    ///   - 接続自体の失敗 → `?` で `Err`（agent 側に伝播）。
    ///   - HTTP 4xx/5xx  → 本文を読み取って `bail!`。LLM プロバイダのエラー本文が
    ///                       そのままユーザに見えるのがデバッグ上一番ありがたい。
    ///
    /// `api_key` が `Some("")` や None の場合は Authorization ヘッダを付けない
    /// （ollama 等、認証なしエンドポイントへの対応）。
    pub async fn stream(
        &self,
        endpoint: &str,
        api_key: Option<&str>,
        extra_headers: &BTreeMap<String, String>,
        req: &ChatRequest,
    ) -> Result<impl Stream<Item = Result<StreamEvent>>> {
        let mut builder = self
            .http
            .post(endpoint)
            .header("Accept", "text/event-stream")
            .json(req);

        if let Some(key) = api_key.filter(|s| !s.is_empty()) {
            builder = builder.bearer_auth(key);
        }
        for (k, v) in extra_headers {
            builder = builder.header(k, v);
        }

        let resp = builder
            .send()
            .await
            .with_context(|| format!("LLM endpoint への POST 失敗: {endpoint}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "LLM {} {}: {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or(""),
                body
            );
        }

        Ok(parse_stream(resp))
    }
}
