// stream — SSE parsing for `/chat/completions` (SPEC §6.1).
//
// We use `eventsource-stream` to extract `data:` lines, then parse each line's
// JSON incrementally into a `StreamEvent`. Accumulation (concatenating content,
// joining tool_calls per index) is the caller's responsibility (agent.rs). Here
// we keep it as "one chunk = one event".
//
// SPEC §6.1 gotchas, called out explicitly in the implementation:
//   - `choices[0].delta.tool_calls[i].id` and `function.name` arrive **only in
//     the first fragment**
//   - `function.arguments` is **fragmented across multiple chunks**
//   The accumulator must "take the first id/name, concatenate every arguments fragment."

use anyhow::{anyhow, Result};
use eventsource_stream::Eventsource;
use futures_util::{Stream, StreamExt};
use serde::Deserialize;

/// One SSE chunk.
///
/// When `[DONE]` arrives, we end the stream immediately (returning `None`);
/// hence no separate `Done` variant. Callers terminate via the stream's natural
/// end (`next()` returns `None`).
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Chunk(ChunkPayload),
}

/// The meaningful delta extracted from one chunk. Empty chunks are skipped inside `parse_stream`.
#[derive(Debug, Clone, Default)]
pub struct ChunkPayload {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
}

/// One `choices[0].delta.tool_calls[i]`. `index` selects the accumulator bucket.
#[derive(Debug, Clone)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    /// A fragment of `function.arguments` (the JSON string may be cut mid-character).
    pub arguments_fragment: Option<String>,
}

// ---------------------------------------------------------------------------
// Byte stream → StreamEvent stream
// ---------------------------------------------------------------------------

/// Consume a `reqwest::Response`, SSE-parse it, and yield `StreamEvent`s.
///
/// `[DONE]` returns `None` immediately so the stream ends — dropping `es` also
/// releases the underlying HTTP connection. Callers just `while let Some(...)`.
pub fn parse_stream(resp: reqwest::Response) -> impl Stream<Item = Result<StreamEvent>> {
    let es = resp.bytes_stream().eventsource();
    futures_util::stream::unfold(Some(es), |state| async move {
        let mut es = state?;
        loop {
            match es.next().await {
                None => return None,
                Some(Err(e)) => {
                    return Some((Err(anyhow!("SSE receive error: {e}")), Some(es)));
                }
                Some(Ok(event)) => {
                    // The SSE `event:` line is unused (openai-compatible sends `data:` only).
                    if event.data == "[DONE]" {
                        // Drop es → release HTTP connection. End the stream naturally.
                        return None;
                    }
                    if event.data.is_empty() {
                        continue;
                    }
                    match parse_chunk(&event.data) {
                        Ok(None) => continue, // Empty chunk (e.g. finish_reason only)
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
// JSON chunk shape
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
    // finish_reason etc. is unused (done is signaled by stream end above).
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
    /// SPEC §6.1: fragments of the same tool call share the same index.
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
        .map_err(|e| anyhow!("failed to parse chunk JSON: {e}; data={data}"))?;
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
// Unit tests — JSON parser only
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
        // The "finish_reason only" final chunk etc. Not yielded.
        let data = r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        assert!(parse_chunk(data).unwrap().is_none());
    }

    #[test]
    fn no_choices_returns_none() {
        // Startup `usage` chunks etc. — empty `choices` mustn't panic.
        let data = r#"{"choices":[]}"#;
        assert!(parse_chunk(data).unwrap().is_none());
    }

    #[test]
    fn parses_tool_call_head_fragment() {
        // SPEC §6.1: the head fragment carries id + name; arguments is empty or partial.
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
        // Continuation fragments have no id/name, only arguments.
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
