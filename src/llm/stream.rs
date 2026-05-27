// stream — `/chat/completions` の SSE パース（SPEC §6.1）。
//
// `eventsource-stream` で `data:` 行を抜き出し、各行の JSON を逐次パースして
// `StreamEvent` を yield する。蓄積（content 結合、tool_calls の index ごと連結）は
// 上位（agent.rs）の責務。ここは「1 チャンク = 1 イベント」のままに留める。
//
// SPEC §6.1 の罠を実装にも明記:
//   - `choices[0].delta.tool_calls[i].id` と `function.name` は **先頭フラグメントのみ** に来る
//   - `function.arguments` は **複数フラグメントに分割** されて届く
//   蓄積側は「最初の id/name を採用、arguments は全フラグメントを連結」が正解。

use anyhow::{anyhow, Result};
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use serde::Deserialize;

/// SSE 1 件分のチャンク。`Done` は `data: [DONE]` 受信時。
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Chunk(ChunkPayload),
    Done,
}

/// 1 チャンクから取り出した有意なデルタ。空チャンクは `parse_stream` 内でスキップ。
#[derive(Debug, Clone, Default)]
pub struct ChunkPayload {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
}

/// `choices[0].delta.tool_calls[i]` 1 件。`index` で蓄積バケットを引く。
#[derive(Debug, Clone)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    /// `function.arguments` の断片（JSON 文字列の途中で切れている）。
    pub arguments_fragment: Option<String>,
}

// ---------------------------------------------------------------------------
// バイトストリーム → StreamEvent ストリーム
// ---------------------------------------------------------------------------

/// `reqwest::Response` を消費し、SSE をパースして `StreamEvent` を流す。
///
/// `Done` を yield した後にさらに `next()` が呼ばれた場合、内部でストリームを使い切るまで
/// 空読みするだけで、何も yield しない。呼び出し側は `Done` で break すべき。
pub fn parse_stream(resp: reqwest::Response) -> impl Stream<Item = Result<StreamEvent>> {
    let es = resp.bytes_stream().eventsource();
    futures_util::stream::unfold(Some(es), |state| async move {
        let mut es = state?;
        loop {
            match es.next().await {
                None => return None,
                Some(Err(e)) => {
                    return Some((Err(anyhow!("SSE 受信エラー: {e}")), Some(es)));
                }
                Some(Ok(event)) => {
                    // SSE の event-type 行は使わない（OpenAI 互換は `data:` のみ送る）。
                    if event.data == "[DONE]" {
                        return Some((Ok(StreamEvent::Done), Some(es)));
                    }
                    if event.data.is_empty() {
                        continue;
                    }
                    match parse_chunk(&event.data) {
                        Ok(None) => continue, // finish_reason のみ等の空チャンク
                        Ok(Some(payload)) => {
                            return Some((Ok(StreamEvent::Chunk(payload)), Some(es)));
                        }
                        Err(e) => {
                            return Some((Err(e), Some(es)));
                        }
                    }
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// JSON チャンクの構造
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawChunk {
    #[serde(default)]
    choices: Vec<RawChoice>,
}

#[derive(Debug, Deserialize, Default)]
struct RawChoice {
    #[serde(default)]
    delta: RawDelta,
    // finish_reason 等は今は使わない（蓄積は上位で done で確定する）。
}

#[derive(Debug, Deserialize, Default)]
struct RawDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<RawToolCall>,
}

#[derive(Debug, Deserialize)]
struct RawToolCall {
    /// SPEC §6.1: 同じツール呼び出しのフラグメントは同じ index を持つ。
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<RawFunction>,
}

#[derive(Debug, Deserialize)]
struct RawFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

fn parse_chunk(data: &str) -> Result<Option<ChunkPayload>> {
    let raw: RawChunk = serde_json::from_str(data)
        .map_err(|e| anyhow!("chunk JSON パース失敗: {e}; data={data}"))?;
    let Some(choice) = raw.choices.into_iter().next() else {
        return Ok(None);
    };

    let tool_calls: Vec<ToolCallDelta> = choice
        .delta
        .tool_calls
        .into_iter()
        .map(|tc| {
            let (name, arguments_fragment) = match tc.function {
                Some(f) => (f.name, f.arguments),
                None => (None, None),
            };
            ToolCallDelta {
                index: tc.index,
                id: tc.id,
                name,
                arguments_fragment,
            }
        })
        .collect();

    if choice.delta.content.is_none() && tool_calls.is_empty() {
        return Ok(None);
    }

    Ok(Some(ChunkPayload {
        content: choice.delta.content,
        tool_calls,
    }))
}

// ---------------------------------------------------------------------------
// 単体テスト — JSON パーサ部分のみ
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_content_chunk() {
        let data = r#"{"choices":[{"delta":{"content":"hello"}}]}"#;
        let p = parse_chunk(data).unwrap().unwrap();
        assert_eq!(p.content.as_deref(), Some("hello"));
        assert!(p.tool_calls.is_empty());
    }

    #[test]
    fn empty_delta_chunk_returns_none() {
        // finish_reason だけが届く最終チャンク等。Yield 対象外。
        let data = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        assert!(parse_chunk(data).unwrap().is_none());
    }

    #[test]
    fn no_choices_returns_none() {
        // ストリーム開始時の usage チャンク等、choices が空でもパニックしない。
        let data = r#"{"choices":[]}"#;
        assert!(parse_chunk(data).unwrap().is_none());
    }

    #[test]
    fn parses_tool_call_head_fragment() {
        // SPEC §6.1: 先頭フラグメントには id + name、arguments は空文字 or 部分のみ。
        let data = r#"{"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"call_1","function":{"name":"do_it","arguments":""}}
        ]}}]}"#;
        let p = parse_chunk(data).unwrap().unwrap();
        assert_eq!(p.tool_calls.len(), 1);
        let t = &p.tool_calls[0];
        assert_eq!(t.index, 0);
        assert_eq!(t.id.as_deref(), Some("call_1"));
        assert_eq!(t.name.as_deref(), Some("do_it"));
        assert_eq!(t.arguments_fragment.as_deref(), Some(""));
    }

    #[test]
    fn parses_tool_call_arguments_continuation() {
        // 継続フラグメントは id/name なし、arguments のみ。
        let data = r#"{"choices":[{"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":"{\"x\":"}}
        ]}}]}"#;
        let p = parse_chunk(data).unwrap().unwrap();
        let t = &p.tool_calls[0];
        assert!(t.id.is_none());
        assert!(t.name.is_none());
        assert_eq!(t.arguments_fragment.as_deref(), Some("{\"x\":"));
    }
}
