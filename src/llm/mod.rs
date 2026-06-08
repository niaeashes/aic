// llm — openai-compatible /chat/completions client (SPEC §6, §14-2).
//
// Per the "no provider abstraction" policy (SPEC §1, §14), `ChatClient` is a
// single concrete type, not a trait. Per-group base_url / api_key / headers are
// resolved by the caller and passed in — pulling them from Settings is the
// agent's job, not ours.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures_util::Stream;

use crate::llm::stream::{parse_stream, StreamEvent};
use crate::llm::types::ChatRequest;

pub mod stream;
pub mod types;

/// Thin wrapper that shares the underlying HTTP client.
///
/// `reqwest::Client` is already Arc-internal, so `ChatClient` can be created
/// per call without performance worry.
#[derive(Debug, Clone)]
pub struct ChatClient {
    http: reqwest::Client,
}

impl ChatClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// POST a `ChatRequest` to `{endpoint}` and return the SSE-parsed stream.
    ///
    /// Failure modes:
    ///   - Connection-level failure → `?` propagates Err to the agent.
    ///   - HTTP 4xx/5xx           → read the body and `bail!`. Showing the
    ///                                provider's raw error body is the most
    ///                                useful thing for debugging.
    ///
    /// If `api_key` is `Some("")` or `None`, no Authorization header is added
    /// (handles ollama and other auth-less endpoints).
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
            .with_context(|| format!("failed to POST to LLM endpoint: {endpoint}"))?;

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
