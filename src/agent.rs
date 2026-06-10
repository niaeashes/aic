// agent — one turn of the chat loop (SPEC §8).
//
// Loop structure (SPEC §8 steps 1–3):
//
//   1. Push the user message to the session (once per turn, at the top)
//   2. Iterate i = 0..max_tool_iterations:
//      a. Get the wired endpoint info via `ctx.require_active_model()`
//         - tools is `ctx.mcp.as_openai_tools()` (omit the field entirely if empty)
//      b. Run the SSE stream, accumulating content and tool_calls (`accumulate`)
//      c. Push the accumulated assistant message
//      d. tool_calls empty → done, break
//      e. tool_calls non-empty → execute each via `McpManager.call` and push the
//         result as a `tool` message (`tool_call_id` required), then continue the loop
//         - Tool-side / network errors become `"error: ..."` content so the loop
//           can keep going (telling the model so it can self-recover)
//   3. If max_tool_iterations is reached without tool_calls becoming empty, warn
//      and abort (this is the aichat-style "broken tool eats the loop and never
//      returns control" failure mode we want to avoid).
//
// Rendering is not this module's responsibility. We notify a `TurnObserver` of
// what's happening; how it appears on the terminal is up to the impl (see
// `repl::view::TerminalView`). Thanks to this boundary, the accumulation logic
// (`accumulate`) has zero screen side effects and is easy to unit-test.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures_util::{Stream, StreamExt};
use serde_json::Value;

use crate::llm::stream::StreamEvent;
use crate::llm::types::{ChatRequest, FunctionCall, Message, Tool, ToolCall};
use crate::llm::ChatClient;
use crate::repl::context::ReplContext;

/// Vocabulary of "things worth displaying" during a single turn.
///
/// The agent only knows this interface; the actual output (terminal, test capture,
/// future non-TTY backends) is decided by the implementor. `assistant_start` and
/// `assistant_end` have empty default impls so a test fake can just pick up
/// the body and tool events.
pub trait TurnObserver {
    /// Stream start for an assistant message (use to reset per-message state).
    fn assistant_start(&mut self) {}
    /// Delta chunk of the assistant body.
    fn assistant_delta(&mut self, chunk: &str);
    /// Stream end for an assistant message.
    fn assistant_end(&mut self) {}
    /// Tool call begins. `raw_arguments` is unformatted (preview formatting is left to the impl).
    fn tool_call(&mut self, public_name: &str, raw_arguments: &str);
    /// Tool call succeeded.
    fn tool_succeeded(&mut self, public_name: &str);
    /// Tool call failed. `error_text` is the same string that goes into the tool message content.
    fn tool_failed(&mut self, public_name: &str, error_text: &str);
    /// We hit `max_tool_iterations` and aborted.
    fn iteration_limit_reached(&mut self, max: u32);
    /// The turn was interrupted by the user (Ctrl-C) mid-flight.
    fn cancelled(&mut self) {}
}

pub async fn run_turn(
    ctx: &mut ReplContext,
    user_input: String,
    view: &mut dyn TurnObserver,
) -> Result<()> {
    // Inject the configured system prompt as the very first message of a fresh
    // conversation. `session.messages` is empty at startup and right after
    // `/clear`, so this re-seeds the prompt each time without duplicating it.
    if ctx.session.messages.is_empty() {
        if let Some(sp) = &ctx.settings.system_prompt {
            ctx.session.messages.push(Message::system(sp.clone()));
        }
    }
    ctx.session.messages.push(Message::user(user_input));

    // ActiveModel is resolved once when the model is selected. The agent doesn't
    // need to know about Settings. We clone here so we can later re-borrow ctx
    // mutably (e.g. for mcp.call).
    let active = ctx.require_active_model()?;
    let max_iter = ctx.settings.ui.max_tool_iterations;
    let temperature = ctx.settings.generation.temperature;
    let max_tokens = ctx.settings.generation.max_tokens;
    let client = ChatClient::new(ctx.http.clone());

    for _iter in 0..max_iter {
        // Re-fetch the MCP tools on every iteration. (If the list is empty,
        // we omit the `tools` field entirely.)
        let tool_list = ctx.mcp.as_openai_tools();
        let tools: Option<Vec<Tool>> = if tool_list.is_empty() {
            None
        } else {
            Some(tool_list)
        };

        let request = ChatRequest {
            model: active.model.clone(),
            messages: ctx.session.messages.clone(),
            tools,
            stream: true,
            temperature,
            max_tokens,
        };

        let assistant = stream_assistant(
            &client,
            &active.endpoint_url,
            active.api_key.as_deref(),
            &active.headers,
            &request,
            view,
        )
        .await?;
        let tool_calls = assistant.tool_calls().to_vec();
        ctx.session.messages.push(assistant);

        // tool_calls empty → ordinary response, we're done.
        if tool_calls.is_empty() {
            return Ok(());
        }

        // Execute each tool_call and push a tool message.
        for tc in tool_calls {
            let public_name = tc.function.name.clone();
            view.tool_call(&public_name, &tc.function.arguments);

            let content = match parse_tool_arguments(&tc.function.arguments) {
                Ok(args) => match ctx.mcp.call(&public_name, args).await {
                    Ok(text) => {
                        view.tool_succeeded(&public_name);
                        text
                    }
                    Err(e) => {
                        // Tool-side errors and network failures don't break the loop.
                        // Telling the model "this failed" lets it retry or pick a
                        // different tool.
                        let msg = format!("error: {e:#}");
                        view.tool_failed(&public_name, &msg);
                        msg
                    }
                },
                Err(e) => {
                    let msg = format!("error: failed to parse arguments JSON: {e}");
                    view.tool_failed(&public_name, &msg);
                    msg
                }
            };

            ctx.session.messages.push(Message::tool(tc.id, public_name, content));
        }
    }

    // Cap reached. The last assistant turn still carried tool_calls, so from the
    // model's perspective the conversation is incomplete. We just warn the user
    // and return control to the REPL.
    view.iteration_limit_reached(max_iter);
    Ok(())
}

/// Open one stream and build the assistant message from it.
///
/// Network setup is delegated to `ChatClient::stream`; the actual accumulation
/// happens in `accumulate`.
async fn stream_assistant(
    client: &ChatClient,
    endpoint: &str,
    api_key: Option<&str>,
    headers: &BTreeMap<String, String>,
    request: &ChatRequest,
    view: &mut dyn TurnObserver,
) -> Result<Message> {
    let stream = client.stream(endpoint, api_key, headers, request).await?;
    accumulate(stream, view).await
}

/// Consume a stream of `StreamEvent`s, concatenate `content`, merge `tool_calls`
/// per index, and return the resulting `Message::Assistant`. Network-independent
/// (tests can drive it with `stream::iter`), so this is where we verify
/// SPEC §6.1's fragment-joining rules.
///
/// Body chunks are streamed to `view.assistant_delta` as they arrive. Start and
/// end of the stream are signalled via `assistant_start` / `assistant_end`.
async fn accumulate(
    stream: impl Stream<Item = Result<StreamEvent>>,
    view: &mut dyn TurnObserver,
) -> Result<Message> {
    // The unfold-based stream isn't Unpin, so we Box::pin it.
    let mut stream = Box::pin(stream);

    let mut content = String::new();
    // index → in-progress (id, name, arguments)
    let mut tool_calls: BTreeMap<usize, ToolCallAccum> = BTreeMap::new();

    view.assistant_start();
    while let Some(event) = stream.next().await {
        // [DONE] is converted to None at the stream layer, so it never reaches us here.
        let StreamEvent::Chunk(payload) = event?;
        if let Some(c) = payload.content {
            view.assistant_delta(&c);
            content.push_str(&c);
        }
        for delta in payload.tool_calls {
            let entry = tool_calls.entry(delta.index).or_default();
            // SPEC §6.1: id and name only arrive in the first fragment — only
            // overwrite when None.
            if entry.id.is_none() {
                entry.id = delta.id;
            }
            if entry.name.is_none() {
                entry.name = delta.name;
            }
            // arguments arrives fragmented; concatenate every fragment.
            if let Some(frag) = delta.arguments_fragment {
                entry.arguments.push_str(&frag);
            }
        }
    }
    view.assistant_end();

    // Convert accumulators to ToolCall list (BTreeMap guarantees index order).
    // id/name being None at completion is a server bug, not a normal case;
    // unwrap_or_default keeps us from panicking.
    let tool_calls_vec: Vec<ToolCall> = tool_calls
        .into_iter()
        .map(|(_, a)| ToolCall {
            id: a.id.unwrap_or_default(),
            kind: "function".to_string(),
            function: FunctionCall {
                name: a.name.unwrap_or_default(),
                arguments: a.arguments,
            },
        })
        .collect();

    Ok(Message::Assistant {
        content: if content.is_empty() { None } else { Some(content) },
        tool_calls: tool_calls_vec,
    })
}

/// Parse the JSON string in `function.arguments` (returned by the LLM) into a `Value`.
///
/// - Per OpenAI's convention, an empty string means "no arguments" → return `{}`.
/// - On parse failure return Err so the caller can record it as a tool error.
fn parse_tool_arguments(raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(trimmed)
        .with_context(|| format!("failed to parse arguments JSON: {raw}"))
}

#[derive(Default)]
struct ToolCallAccum {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::stream::{ChunkPayload, ToolCallDelta};
    use futures_util::stream;

    // --- parse_tool_arguments -------------------------------------------------

    #[test]
    fn empty_arguments_parses_as_empty_object() {
        let v = parse_tool_arguments("").unwrap();
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn whitespace_only_arguments_parses_as_empty_object() {
        let v = parse_tool_arguments("   \n  ").unwrap();
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn valid_json_arguments_parsed() {
        let v = parse_tool_arguments(r#"{"q":"hello","n":3}"#).unwrap();
        assert_eq!(v["q"], "hello");
        assert_eq!(v["n"], 3);
    }

    #[test]
    fn invalid_json_arguments_errors() {
        let r = parse_tool_arguments("{ not json");
        assert!(r.is_err());
    }

    // --- accumulate -----------------------------------------------------------

    /// Records the events it observes without printing anything.
    #[derive(Default)]
    struct CapturingView {
        events: Vec<String>,
    }
    impl TurnObserver for CapturingView {
        fn assistant_delta(&mut self, chunk: &str) {
            self.events.push(format!("delta:{chunk}"));
        }
        fn tool_call(&mut self, public_name: &str, _raw_arguments: &str) {
            self.events.push(format!("call:{public_name}"));
        }
        fn tool_succeeded(&mut self, public_name: &str) {
            self.events.push(format!("ok:{public_name}"));
        }
        fn tool_failed(&mut self, public_name: &str, _error_text: &str) {
            self.events.push(format!("err:{public_name}"));
        }
        fn iteration_limit_reached(&mut self, max: u32) {
            self.events.push(format!("limit:{max}"));
        }
    }

    fn text_event(s: &str) -> Result<StreamEvent> {
        Ok(StreamEvent::Chunk(ChunkPayload {
            content: Some(s.to_string()),
            tool_calls: vec![],
        }))
    }

    fn tool_event(
        index: usize,
        id: Option<&str>,
        name: Option<&str>,
        args: Option<&str>,
    ) -> Result<StreamEvent> {
        Ok(StreamEvent::Chunk(ChunkPayload {
            content: None,
            tool_calls: vec![ToolCallDelta {
                index,
                id: id.map(str::to_string),
                name: name.map(str::to_string),
                arguments_fragment: args.map(str::to_string),
            }],
        }))
    }

    #[tokio::test]
    async fn accumulate_text_only_concatenates_and_streams_deltas() {
        let events = vec![text_event("Hel"), text_event("lo")];
        let mut view = CapturingView::default();
        let msg = accumulate(stream::iter(events), &mut view).await.unwrap();
        match msg {
            Message::Assistant { content, tool_calls } => {
                assert_eq!(content.as_deref(), Some("Hello"));
                assert!(tool_calls.is_empty());
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
        // Body chunks flow into the view in order (assistant_start/end are no-ops in CapturingView).
        assert_eq!(view.events, vec!["delta:Hel", "delta:lo"]);
    }

    #[tokio::test]
    async fn accumulate_assembles_split_tool_call_fragments() {
        // SPEC §6.1: head fragment has id+name; arguments is split across fragments.
        let events = vec![
            tool_event(0, Some("call_1"), Some("search"), Some("")),
            tool_event(0, None, None, Some(r#"{"q":"#)),
            tool_event(0, None, None, Some(r#""hi"}"#)),
        ];
        let mut view = CapturingView::default();
        let msg = accumulate(stream::iter(events), &mut view).await.unwrap();
        let calls = msg.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "search");
        assert_eq!(calls[0].function.arguments, r#"{"q":"hi"}"#);
        // No body content → no deltas.
        assert!(view.events.is_empty());
    }

    #[tokio::test]
    async fn accumulate_handles_mixed_content_and_tool_call() {
        let events = vec![
            text_event("thinking"),
            tool_event(0, Some("id1"), Some("fetch"), Some("{}")),
        ];
        let mut view = CapturingView::default();
        let msg = accumulate(stream::iter(events), &mut view).await.unwrap();
        match &msg {
            Message::Assistant { content, tool_calls } => {
                assert_eq!(content.as_deref(), Some("thinking"));
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].function.name, "fetch");
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
        assert_eq!(view.events, vec!["delta:thinking"]);
    }

    #[tokio::test]
    async fn accumulate_two_parallel_tool_calls_by_index() {
        // Different indices → separate buckets, ordered by index (BTreeMap).
        let events = vec![
            tool_event(0, Some("a"), Some("first"), Some("{}")),
            tool_event(1, Some("b"), Some("second"), Some("{}")),
        ];
        let mut view = CapturingView::default();
        let msg = accumulate(stream::iter(events), &mut view).await.unwrap();
        let calls = msg.tool_calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "first");
        assert_eq!(calls[1].function.name, "second");
    }
}
