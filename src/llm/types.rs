// types — OpenAI 互換 /chat/completions のメッセージ / ツール型（SPEC §6.2）。
//
// 厳密スキーマではなく実用で過不足のないシリアライズを優先。
// `serde(skip_serializing_if = ...)` を多用してリクエストペイロードを小さく保つ。

use serde::{Deserialize, Serialize};

/// メッセージのロール。OpenAI に準拠（system / user / assistant / tool）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 1 メッセージ。`assistant` 行は `tool_calls` を、`tool` 行は `tool_call_id` を持つ。
///
/// `content` は `tool_calls` を返す assistant ターンでは欠落することがあるため Option。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// tool ロールでは送信先の MCP ツール名（任意）。assistant では未使用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// assistant が呼び出すツール群。空なら送信時にフィールド自体を省く。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// tool ロール時、対応する assistant.tool_calls[i].id を必ず指定する（SPEC §6.2）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self::new(Role::System, Some(content.into()))
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::new(Role::User, Some(content.into()))
    }
    pub fn assistant_text(content: impl Into<String>) -> Self {
        Self::new(Role::Assistant, Some(content.into()))
    }
    fn new(role: Role, content: Option<String>) -> Self {
        Self {
            role,
            content,
            name: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
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
        // 空フィールドはペイロードに出さない
        assert!(v.get("tool_calls").is_none());
        assert!(v.get("tool_call_id").is_none());
        assert!(v.get("name").is_none());
    }

    #[test]
    fn assistant_with_tool_calls_serializes_array() {
        let m = Message {
            role: Role::Assistant,
            content: None,
            name: None,
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: FunctionCall {
                    name: "do_it".into(),
                    arguments: r#"{"x":1}"#.into(),
                },
            }],
            tool_call_id: None,
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "assistant");
        assert!(v.get("content").is_none());
        assert_eq!(v["tool_calls"][0]["id"], "call_1");
        assert_eq!(v["tool_calls"][0]["type"], "function");
        assert_eq!(v["tool_calls"][0]["function"]["name"], "do_it");
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
    fn role_deserializes_case_insensitive_via_lowercase() {
        let r: Role = serde_json::from_str(r#""tool""#).unwrap();
        assert_eq!(r, Role::Tool);
    }
}
