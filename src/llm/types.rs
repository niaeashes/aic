// types — OpenAI 互換 /chat/completions のメッセージ / ツール型（SPEC §6.2）。
//
// 厳密スキーマではなく実用で過不足のないシリアライズを優先。
// `serde(skip_serializing_if = ...)` を多用してリクエストペイロードを小さく保つ。
//
// Message は internally-tagged enum にして、role ごとに必須フィールドをコンパイル時に保証する。
//   - Tool variant: content / name / tool_call_id が全て必須（型で強制）
//   - Assistant variant: content は省略可、tool_calls は省略可
//   - User / System variant: content が必須

use serde::{Deserialize, Serialize};

/// `/chat/completions` の 1 メッセージ。`role` 値で variant を discriminate する。
///
/// `#[serde(tag = "role", rename_all = "lowercase")]` により、JSON は OpenAI 互換の
/// `{"role": "user", "content": "..."}` のようなフラット形式になる。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    /// LLM の応答。`content` と `tool_calls` はどちらか片方だけの場合もある。
    Assistant {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    /// ツール実行結果。3 フィールドすべて必須（SPEC §6.2）。
    Tool {
        content: String,
        /// 呼び出し元 assistant.tool_calls[i].id と一致させる。
        tool_call_id: String,
        /// MCP 公開名（`<server>__<tool>` 形式）。OpenAI 仕様では optional だが常に詰める。
        name: String,
    },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Message::System { content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Message::User { content: content.into() }
    }
    pub fn assistant_text(content: impl Into<String>) -> Self {
        Message::Assistant { content: Some(content.into()), tool_calls: Vec::new() }
    }
    pub fn tool(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Message::Tool {
            tool_call_id: tool_call_id.into(),
            name: name.into(),
            content: content.into(),
        }
    }

    /// `Assistant` variant の tool_calls を返す。他の variant は空スライス。
    ///
    /// agent ループが「tool_calls があるか」を判定するために使う。
    pub fn tool_calls(&self) -> &[ToolCall] {
        match self {
            Message::Assistant { tool_calls, .. } => tool_calls,
            _ => &[],
        }
    }
}

/// assistant が要求するツール呼び出し 1 件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    /// 現状 OpenAI 互換 API では常に `"function"`。
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON 文字列。SSE では分割されて届くので連結したもの（SPEC §6.1）。
    pub arguments: String,
}

/// LLM へ公開するツール定義（MCP 由来。M6 で `as_openai_tools()` から生成）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    /// 現状常に `"function"`。
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema。MCP 側のスキーマをそのまま流し込む想定。
    pub parameters: serde_json::Value,
}

/// `/chat/completions` リクエストボディ。`stream: true` 固定（SPEC §1, §6）。
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    pub stream: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_serializes_minimal_fields() {
        let m = Message::user("hi");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "user");
        assert_eq!(v["content"], "hi");
        // user には tool_calls / tool_call_id / name は出ない
        assert!(v.get("tool_calls").is_none());
        assert!(v.get("tool_call_id").is_none());
        assert!(v.get("name").is_none());
    }

    #[test]
    fn assistant_with_tool_calls_serializes_array() {
        let m = Message::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "do_it".into(),
                    arguments: r#"{"x":1}"#.into(),
                },
            }],
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "assistant");
        assert!(v.get("content").is_none());
        assert_eq!(v["tool_calls"][0]["id"], "call_1");
        assert_eq!(v["tool_calls"][0]["type"], "function");
        assert_eq!(v["tool_calls"][0]["function"]["name"], "do_it");
    }

    #[test]
    fn assistant_text_only_omits_tool_calls() {
        let m = Message::assistant_text("hello");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "assistant");
        assert_eq!(v["content"], "hello");
        // tool_calls が空なのでフィールド自体が消える
        assert!(v.get("tool_calls").is_none());
    }

    #[test]
    fn chat_request_omits_tools_when_none() {
        let req = ChatRequest {
            model: "gpt-4o-mini".into(),
            messages: vec![Message::user("ping")],
            tools: None,
            stream: true,
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["stream"], true);
        assert!(v.get("tools").is_none());
    }

    #[test]
    fn tool_message_has_required_fields() {
        let m = Message::tool("call_42", "my_tool", "result text");
        assert!(matches!(m, Message::Tool { .. }));
        if let Message::Tool { tool_call_id, name, content } = &m {
            assert_eq!(tool_call_id, "call_42");
            assert_eq!(name, "my_tool");
            assert_eq!(content, "result text");
        }
    }

    #[test]
    fn tool_message_serializes_correctly() {
        let m = Message::tool("cid", "fn_name", "ok");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "tool");
        assert_eq!(v["tool_call_id"], "cid");
        assert_eq!(v["name"], "fn_name");
        assert_eq!(v["content"], "ok");
        // tool_calls は tool variant に存在しないのでペイロードに出ない
        assert!(v.get("tool_calls").is_none());
    }

    #[test]
    fn tool_message_missing_tool_call_id_fails_to_deserialize() {
        // tool_call_id が必須フィールドであることをデシリアライズで検証
        let json = r#"{"role":"tool","name":"fn","content":"ok"}"#;
        let r: Result<Message, _> = serde_json::from_str(json);
        assert!(r.is_err(), "tool_call_id が欠けていたらデシリアライズ失敗するべき");
    }

    #[test]
    fn tool_calls_helper_returns_assistant_tool_calls() {
        let m = Message::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "x".into(),
                kind: "function".into(),
                function: FunctionCall { name: "f".into(), arguments: "{}".into() },
            }],
        };
        assert_eq!(m.tool_calls().len(), 1);
        // 他 variant は空
        assert!(Message::user("hi").tool_calls().is_empty());
        assert!(Message::tool("id", "n", "c").tool_calls().is_empty());
    }
}
