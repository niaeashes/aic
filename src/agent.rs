// agent — 1 ターン分の chat ループ（SPEC §8）。
//
// M7 で「tool 呼べるチャット」になる。ループ構造（SPEC §8 ステップ 1–3）:
//
//   1. user メッセージを session に push（ターン頭、1 回だけ）
//   2. ループ反復 i = 0..max_tool_iterations:
//      a. current_model のグループから base_url/api_key/headers を引いて ChatRequest 構築
//         - tools には ctx.mcp.as_openai_tools() を渡す（空なら None でフィールド省略）
//      b. SSE ストリームを回し、content と tool_calls を蓄積
//      c. 蓄積を assistant メッセージとして push
//      d. tool_calls が空 → 完了して break
//      e. tool_calls があれば各呼び出しを `McpManager.call` で実行し、結果を
//         `tool` メッセージ（`tool_call_id` 必須）として push してループ継続
//         - tool 側エラー / ネットワーク失敗は "error: ..." を content に詰めてループ継続
//           （モデルに失敗を伝えることで自己回復できるようにする）
//   3. max_tool_iterations に達しても tool_calls が空にならなければ警告を出して打ち切り
//      （aichat 系で見られた「壊れたツールでループ消費して制御戻らない」の回避）
//
// M8: 表示整形。assistant ラベル、tool 開始/終了のインジケータを統一する。
//   - assistant 本文が空でストレートに tool_call へ進むケースでも「思考だけで応答無し」
//     と区別が付くようインジケータを必ず出す。
//   - tool 引数は短くプレビュー表示（長すぎる JSON はターミナルを汚す）。

use std::collections::BTreeMap;
use std::io::Write;

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde_json::Value;

use crate::llm::stream::StreamEvent;
use crate::llm::types::{ChatRequest, FunctionCall, Message, Role, Tool, ToolCall};
use crate::llm::ChatClient;
use crate::repl::context::ReplContext;

const ASSISTANT_LABEL: &str = "assistant> ";
const TOOL_ARG_PREVIEW_MAX: usize = 80;

pub async fn run_turn(ctx: &mut ReplContext, user_input: String) -> Result<()> {
    ctx.session.messages.push(Message::user(user_input));

    // モデル / グループ解決（ターンを通して固定）。
    let model_ref = ctx.current_model.clone().context(
        "モデルが未選択です。config の default_model か、/model use <group>:<model> で選択してください",
    )?;
    let group = ctx
        .settings
        .group_by_name(&model_ref.group)
        .with_context(|| format!("model group '{}' が config に存在しません", model_ref.group))?;

    // `${VAR}` 展開は起動時に `Settings::expand_secrets` で済んでいる（M5）。
    let api_key = group.api_key.clone();
    let headers: BTreeMap<String, String> = group.headers.clone();
    let endpoint = format!(
        "{}/chat/completions",
        group.base_url.trim_end_matches('/')
    );

    let max_iter = ctx.settings.ui.max_tool_iterations;
    let client = ChatClient::new(ctx.http.clone());

    for _iter in 0..max_iter {
        // MCP ツールは毎反復で最新化（disabled トグル等は M7 段階では起動時固定だが、
        // 「空配列なら tools フィールドごと省く」だけここで担保しておく）。
        let tool_list = ctx.mcp.as_openai_tools();
        let tools: Option<Vec<Tool>> = if tool_list.is_empty() {
            None
        } else {
            Some(tool_list)
        };

        let request = ChatRequest {
            model: model_ref.model.clone(),
            messages: ctx.session.messages.clone(),
            tools,
            stream: true,
        };

        let assistant = stream_assistant(&client, &endpoint, api_key.as_deref(), &headers, &request)
            .await?;
        let tool_calls = assistant.tool_calls.clone();
        ctx.session.messages.push(assistant);

        // tool_calls が空 → 通常応答完了
        if tool_calls.is_empty() {
            return Ok(());
        }

        // それぞれ実行 → tool メッセージとして push
        for tc in tool_calls {
            let public_name = tc.function.name.clone();
            let arg_preview = arg_preview(&tc.function.arguments);
            eprintln!("· tool call: {public_name}({arg_preview})");

            let arguments = parse_tool_arguments(&tc.function.arguments);
            let content = match arguments {
                Ok(args) => match ctx.mcp.call(&public_name, args).await {
                    Ok(text) => {
                        eprintln!("✓ tool ok:   {public_name}");
                        text
                    }
                    Err(e) => {
                        // tool 側エラーやネットワーク失敗もループは続ける。
                        // モデルが「失敗した」と知れば再試行や別ツールに切り替えられる。
                        let msg = format!("error: {e:#}");
                        eprintln!("✗ tool err:  {public_name}: {msg}");
                        msg
                    }
                },
                Err(e) => {
                    let msg = format!("error: arguments JSON のパース失敗: {e}");
                    eprintln!("✗ tool err:  {public_name}: {msg}");
                    msg
                }
            };

            ctx.session.messages.push(Message {
                role: Role::Tool,
                content: Some(content),
                name: Some(public_name),
                tool_calls: Vec::new(),
                tool_call_id: Some(tc.id.clone()),
            });
        }
    }

    // 上限到達。最後の assistant ターンは tool_calls を持っていた状態なので、
    // モデルから見ると未完了に見える。ユーザには警告だけ出して制御を返す。
    eprintln!(
        "warning: tool 呼び出しが ui.max_tool_iterations ({max_iter}) に達したため打ち切りました"
    );
    Ok(())
}

/// 1 回分のストリームを回して assistant メッセージを組み上げる。
///
/// `content` と `tool_calls` のどちらか / 両方を持つ可能性がある。
/// 呼び出し側は戻り値を `session.messages` に push してから tool_calls を見て分岐する。
async fn stream_assistant(
    client: &ChatClient,
    endpoint: &str,
    api_key: Option<&str>,
    headers: &BTreeMap<String, String>,
    request: &ChatRequest,
) -> Result<Message> {
    // unfold ベースのストリームは Unpin ではないので Box::pin で固定する
    let mut stream = Box::pin(client.stream(endpoint, api_key, headers, request).await?);

    let mut content = String::new();
    // index → 蓄積中の (id, name, arguments)
    let mut tool_calls: BTreeMap<usize, ToolCallAccum> = BTreeMap::new();
    let mut printed_anything = false;

    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::Chunk(payload) => {
                if let Some(c) = payload.content {
                    if !printed_anything {
                        // 先頭で 1 度だけ assistant ラベルを出す
                        print!("{ASSISTANT_LABEL}");
                    }
                    print!("{c}");
                    // flush しないと長い応答が後ろにまとめて出てしまう
                    std::io::stdout().flush().ok();
                    content.push_str(&c);
                    printed_anything = true;
                }
                for delta in payload.tool_calls {
                    let entry = tool_calls.entry(delta.index).or_default();
                    // SPEC §6.1: id と name は先頭フラグメントにしか来ない
                    if let Some(id) = delta.id {
                        if entry.id.is_empty() {
                            entry.id = id;
                        }
                    }
                    if let Some(name) = delta.name {
                        if entry.name.is_empty() {
                            entry.name = name;
                        }
                    }
                    // arguments は分割されて届くので全断片を連結
                    if let Some(frag) = delta.arguments_fragment {
                        entry.arguments.push_str(&frag);
                    }
                }
            }
            StreamEvent::Done => break,
        }
    }
    if printed_anything {
        println!();
    }

    // 蓄積を ToolCall 列に変換（index 昇順は BTreeMap が保証）
    let tool_calls_vec: Vec<ToolCall> = tool_calls
        .into_iter()
        .map(|(_, a)| ToolCall {
            id: a.id,
            kind: "function".to_string(),
            function: FunctionCall {
                name: a.name,
                arguments: a.arguments,
            },
        })
        .collect();

    Ok(Message {
        role: Role::Assistant,
        content: if content.is_empty() { None } else { Some(content) },
        name: None,
        tool_calls: tool_calls_vec,
        tool_call_id: None,
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

/// ツール呼び出しの可視化用に引数を 1 行プレビュー化。
///
/// - 改行は `\n` のエスケープに置換
/// - `TOOL_ARG_PREVIEW_MAX` を超えたら末尾を `…` で省略
/// - 空 / 空白のみは `""` を返してカッコの中身ゼロを明示
fn arg_preview(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "".to_string();
    }
    let single_line: String = trimmed
        .chars()
        .map(|c| match c {
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => " ".to_string(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("");
    if single_line.chars().count() > TOOL_ARG_PREVIEW_MAX {
        let truncated: String = single_line.chars().take(TOOL_ARG_PREVIEW_MAX).collect();
        format!("{truncated}…")
    } else {
        single_line
    }
}

#[derive(Default)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn arg_preview_collapses_newlines() {
        assert_eq!(arg_preview("{\n  \"x\": 1\n}"), "{\\n  \"x\": 1\\n}");
    }

    #[test]
    fn arg_preview_truncates_long_strings() {
        let long: String = "a".repeat(200);
        let p = arg_preview(&long);
        assert!(p.ends_with('…'));
        // 切り詰め後の文字数 = TOOL_ARG_PREVIEW_MAX + 1（'…' ぶん）
        assert_eq!(p.chars().count(), TOOL_ARG_PREVIEW_MAX + 1);
    }

    #[test]
    fn arg_preview_empty_returns_empty() {
        assert_eq!(arg_preview(""), "");
        assert_eq!(arg_preview("   \n  "), "");
    }
}
