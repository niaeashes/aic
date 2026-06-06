// agent — 1 ターン分の chat ループ（SPEC §8）。
//
// ループ構造（SPEC §8 ステップ 1–3）:
//
//   1. user メッセージを session に push（ターン頭、1 回だけ）
//   2. ループ反復 i = 0..max_tool_iterations:
//      a. ctx.require_active_model() で配線済みエンドポイント情報を取得
//         - tools には ctx.mcp.as_openai_tools() を渡す（空なら None でフィールド省略）
//      b. SSE ストリームを回し、content と tool_calls を蓄積（`accumulate`）
//      c. 蓄積を assistant メッセージとして push
//      d. tool_calls が空 → 完了して break
//      e. tool_calls があれば各呼び出しを `McpManager.call` で実行し、結果を
//         `tool` メッセージ（`tool_call_id` 必須）として push してループ継続
//         - tool 側エラー / ネットワーク失敗は "error: ..." を content に詰めてループ継続
//           （モデルに失敗を伝えることで自己回復できるようにする）
//   3. max_tool_iterations に達しても tool_calls が空にならなければ警告を出して打ち切り
//      （aichat 系で見られた「壊れたツールでループ消費して制御戻らない」の回避）
//
// 描画はこのモジュールの責務ではない。`TurnObserver` トレイトに「何が起きたか」を通知
// するだけで、それをどう端末に出すかは実装（`repl::view::TerminalView`）に委ねる。
// この境界のおかげで、蓄積ロジック（`accumulate`）は画面副作用ゼロで単体テストできる。

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use futures_util::{Stream, StreamExt};
use serde_json::Value;

use crate::llm::stream::StreamEvent;
use crate::llm::types::{ChatRequest, FunctionCall, Message, Tool, ToolCall};
use crate::llm::ChatClient;
use crate::repl::context::ReplContext;

/// 1 ターン中に起きる「描画したい出来事」の語彙。
///
/// agent はこのインターフェースだけを知り、実際の出力（端末・テスト捕捉・将来の非TTY）
/// は実装側が決める。`assistant_start` / `assistant_end` はデフォルト空実装にしてあるので、
/// テスト用フェイクは本文・ツールイベントだけ拾えばよい。
pub trait TurnObserver {
    /// assistant メッセージのストリーム開始（per-message 状態のリセット用）。
    fn assistant_start(&mut self) {}
    /// assistant 本文の差分チャンク。
    fn assistant_delta(&mut self, chunk: &str);
    /// assistant メッセージのストリーム終了。
    fn assistant_end(&mut self) {}
    /// ツール呼び出し開始。`raw_arguments` は未整形（プレビュー整形は実装側の判断）。
    fn tool_call(&mut self, public_name: &str, raw_arguments: &str);
    /// ツール成功。
    fn tool_succeeded(&mut self, public_name: &str);
    /// ツール失敗。`error_text` は tool メッセージ content に詰める文面と同じ。
    fn tool_failed(&mut self, public_name: &str, error_text: &str);
    /// max_tool_iterations 到達で打ち切ったとき。
    fn iteration_limit_reached(&mut self, max: u32);
}

pub async fn run_turn(
    ctx: &mut ReplContext,
    user_input: String,
    view: &mut dyn TurnObserver,
) -> Result<()> {
    ctx.session.messages.push(Message::user(user_input));

    // ActiveModel はモデル選択時に 1 度だけ解決済み。agent は Settings を知らなくていい。
    // clone するのは「以降 ctx を &mut で借り直したい（mcp.call 等）」ため。
    let active = ctx.require_active_model()?;
    let max_iter = ctx.settings.ui.max_tool_iterations;
    let client = ChatClient::new(ctx.http.clone());

    for _iter in 0..max_iter {
        // MCP ツールは毎反復で最新化（空配列なら tools フィールドごと省く）。
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

        // tool_calls が空 → 通常応答完了
        if tool_calls.is_empty() {
            return Ok(());
        }

        // それぞれ実行 → tool メッセージとして push
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
                        // tool 側エラーやネットワーク失敗もループは続ける。
                        // モデルが「失敗した」と知れば再試行や別ツールに切り替えられる。
                        let msg = format!("error: {e:#}");
                        view.tool_failed(&public_name, &msg);
                        msg
                    }
                },
                Err(e) => {
                    let msg = format!("error: arguments JSON のパース失敗: {e}");
                    view.tool_failed(&public_name, &msg);
                    msg
                }
            };

            ctx.session.messages.push(Message::tool(tc.id, public_name, content));
        }
    }

    // 上限到達。最後の assistant ターンは tool_calls を持っていた状態なので、
    // モデルから見ると未完了に見える。ユーザには警告だけ出して制御を返す。
    view.iteration_limit_reached(max_iter);
    Ok(())
}

/// 1 回分のストリームを開いて assistant メッセージを組み上げる。
///
/// ネットワーク確立は `ChatClient::stream` に任せ、蓄積本体は `accumulate` に委譲する。
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

/// `StreamEvent` 列を消費し、`content` 連結と `tool_calls` の index ごと結合を行って
/// `Message::Assistant` を返す。ネットワークに依存しないので（`stream::iter` 等で）
/// 単体テストできる — SPEC §6.1 のフラグメント結合をここで検証する。
///
/// 本文チャンクは届くたびに `view.assistant_delta` へ流す。`view.assistant_start` /
/// `assistant_end` でストリームの開始・終了を通知する。
async fn accumulate(
    stream: impl Stream<Item = Result<StreamEvent>>,
    view: &mut dyn TurnObserver,
) -> Result<Message> {
    // unfold ベースのストリームは Unpin ではないので Box::pin で固定する
    let mut stream = Box::pin(stream);

    let mut content = String::new();
    // index → 蓄積中の (id, name, arguments)
    let mut tool_calls: BTreeMap<usize, ToolCallAccum> = BTreeMap::new();

    view.assistant_start();
    while let Some(event) = stream.next().await {
        // [DONE] はストリーム側で None に変換済みなのでここには届かない
        let StreamEvent::Chunk(payload) = event?;
        if let Some(c) = payload.content {
            view.assistant_delta(&c);
            content.push_str(&c);
        }
        for delta in payload.tool_calls {
            let entry = tool_calls.entry(delta.index).or_default();
            // SPEC §6.1: id と name は先頭フラグメントにしか来ない — None のときだけ上書き
            if entry.id.is_none() {
                entry.id = delta.id;
            }
            if entry.name.is_none() {
                entry.name = delta.name;
            }
            // arguments は分割されて届くので全断片を連結
            if let Some(frag) = delta.arguments_fragment {
                entry.arguments.push_str(&frag);
            }
        }
    }
    view.assistant_end();

    // 蓄積を ToolCall 列に変換（index 昇順は BTreeMap が保証）
    // id / name が None のまま完了することは正常系ではあり得ないが、
    // unwrap_or_default で空文字にとどめてパニックを避ける。
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

/// LLM が返した `function.arguments`（JSON 文字列）を `Value` に直す。
///
/// - 空文字は OpenAI 仕様上「引数なし」を意味するので `{}` 扱い。
/// - パース失敗は呼び出し側で tool エラーとして扱えるよう Err にする。
fn parse_tool_arguments(raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(trimmed)
        .with_context(|| format!("arguments JSON のパース失敗: {raw}"))
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

    /// 画面に出さず、観測したイベントを記録するだけのフェイク。
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
        // 本文チャンクが順に view へ流れる（assistant_start/end は CapturingView では no-op）
        assert_eq!(view.events, vec!["delta:Hel", "delta:lo"]);
    }

    #[tokio::test]
    async fn accumulate_assembles_split_tool_call_fragments() {
        // SPEC §6.1: 先頭フラグメントに id+name、arguments は複数断片に分割。
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
        // 本文が無いので delta は流れない
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
        // 異なる index は別バケット。index 昇順で並ぶ（BTreeMap）。
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
