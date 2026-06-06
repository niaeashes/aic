// mcp/manager — MCP 全サーバを束ねるマネージャと公開名カタログ（SPEC §7.4, §14-6）。
//
// 公開ツール名は **必ず** `"<server>__<tool>"` の合成キーで `BTreeMap` に保持し、
// LLM へは `as_openai_tools()` がこの合成キーをそのまま渡す。`tools/call` 時は
// マップから (server_idx, 実ツール名) を引き直して投げる — 名前文字列をパースし直さ
// ない（SPEC §7.4）。BTreeMap を使うことで挿入のたびにソート済みを保証し、
// `as_openai_tools()` での再ソートが不要になる。
//
// 起動シーケンス（main.rs から呼ぶ）:
//   1. `McpManager::connect_all(&settings, http)` で enabled な全サーバへ:
//        initialize → notifications/initialized → tools/list
//   2. 失敗したサーバはログだけ残してスキップ（aic 起動は止めない）
//   3. ReplContext に格納し、以降 agent.rs から触る

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::llm::types::{Tool, ToolFunction};
use crate::mcp::protocol::{
    ClientInfo, ContentBlock, InitializeParams, McpToolDef, ToolsCallParams, ToolsCallResult,
    ToolsListResult, PROTOCOL_VERSION,
};
use crate::mcp::transport::Transport;

/// 1 つの MCP サーバの稼働状態 + 取得済みツール一覧。
///
/// サーバ名は公開ツール名（`<server>__<tool>`）に埋め込み済みで `catalog` から復元
/// できるため、ここでは保持しない（`tools` は `as_openai_tools` の schema 引き当て用）。
struct McpServer {
    tools: Vec<McpToolDef>,
    transport: Transport,
}

pub struct McpManager {
    /// 接続済みサーバ群。index は `catalog` の値から参照される（内部表現なので非公開）。
    servers: Vec<McpServer>,
    /// 公開名 `"<server>__<tool>"` → (server_idx, 実ツール名)。SPEC §7.4。
    /// BTreeMap なのでキー順が常にソート済み — `as_openai_tools` で再ソート不要。
    catalog: BTreeMap<String, (usize, String)>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::empty()
    }
}

impl McpManager {
    /// MCP サーバ無し（config に書かれていない or 全失敗）でも REPL を回せるよう、
    /// 「空のマネージャ」を作れるようにしておく。
    pub fn empty() -> Self {
        Self {
            servers: Vec::new(),
            catalog: BTreeMap::new(),
        }
    }

    /// 設定上の全 enabled サーバへ初期化 → tools/list。失敗は per-server で握りつぶす。
    pub async fn connect_all(settings: &Settings, http: reqwest::Client) -> Self {
        let mut mgr = Self::empty();
        for cfg in &settings.mcp_servers {
            if !cfg.enabled {
                eprintln!("mcp: {} は disabled のためスキップ", cfg.name);
                continue;
            }
            let mut transport = Transport::new(cfg.url.clone(), cfg.headers.clone(), http.clone());
            match initialize_and_list(&mut transport).await {
                Ok(tools) => {
                    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                    eprintln!(
                        "mcp: {} に接続成功（tools: {}）",
                        cfg.name,
                        names.join(", ")
                    );
                    mgr.push_server(cfg.name.clone(), transport, tools);
                }
                Err(e) => {
                    eprintln!("warning: mcp {} 接続失敗（スキップ）: {e:#}", cfg.name);
                }
            }
        }
        // LLM に渡る公開名（`<server>__<tool>`）の最終一覧を 1 度だけ出す。
        // per-server ログは実ツール名だが、ここは衝突回避済みの公開名なので
        // 「モデルから実際に見えるカタログ」を確認できる（SPEC §7.4, MILESTONES M6 DoD）。
        let names = mgr.public_tool_names();
        if !names.is_empty() {
            eprintln!("mcp: 公開ツール {} 件: {}", names.len(), names.join(", "));
        }
        mgr
    }

    /// 全公開ツール名（`<server>__<tool>`）を文字列ソート順で返す。
    /// 起動時のカタログ要約ログで使う。
    pub fn public_tool_names(&self) -> Vec<String> {
        self.catalog.keys().cloned().collect()
    }

    /// OpenAI 互換 `tools` 配列を生成。空なら呼び出し側で `None` 化することで
    /// `ChatRequest.tools` フィールド自体を省略できる。
    pub fn as_openai_tools(&self) -> Vec<Tool> {
        let mut out = Vec::with_capacity(self.catalog.len());
        for (public_name, (idx, real_name)) in &self.catalog {
            let def = self.servers[*idx]
                .tools
                .iter()
                .find(|t| &t.name == real_name);
            let (description, parameters) = match def {
                Some(d) => (d.description.clone(), d.input_schema.clone()),
                None => (None, json!({"type": "object"})),
            };
            out.push(Tool {
                kind: "function".to_string(),
                function: ToolFunction {
                    name: public_name.clone(),
                    description,
                    parameters,
                },
            });
        }
        out
    }

    /// 公開名でツールを呼ぶ。テキストコンテンツを `\n` 連結で返す。
    pub async fn call(&mut self, public_name: &str, arguments: Value) -> Result<String> {
        let (idx, real_name) = self
            .catalog
            .get(public_name)
            .with_context(|| format!("未知の MCP ツール: {public_name}"))?
            .clone();
        let server = &mut self.servers[idx];
        let value = server
            .transport
            .request(
                "tools/call",
                ToolsCallParams {
                    name: &real_name,
                    arguments,
                },
            )
            .await?;
        let parsed: ToolsCallResult =
            serde_json::from_value(value).context("tools/call result スキーマが想定外")?;

        let mut text = String::new();
        for block in parsed.content {
            if let ContentBlock::Text { text: t } = block {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&t);
            }
        }
        if parsed.is_error {
            anyhow::bail!("MCP tool {public_name} returned error: {text}");
        }
        Ok(text)
    }

    /// `connect_all` 内部で使う登録ヘルパ。`pub(crate)` でテストからも叩ける。
    pub(crate) fn push_server(
        &mut self,
        name: String,
        transport: Transport,
        tools: Vec<McpToolDef>,
    ) {
        let idx = self.servers.len();
        for t in &tools {
            let public = format!("{}__{}", name, t.name);
            self.catalog.insert(public, (idx, t.name.clone()));
        }
        self.servers.push(McpServer { tools, transport });
    }
}

/// initialize → notifications/initialized → tools/list を順に叩く。
async fn initialize_and_list(transport: &mut Transport) -> Result<Vec<McpToolDef>> {
    let init = InitializeParams {
        protocol_version: PROTOCOL_VERSION.to_string(),
        capabilities: json!({}),
        client_info: ClientInfo {
            name: "aic".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
    };
    transport
        .request("initialize", init)
        .await
        .context("initialize 失敗")?;
    transport
        .notify("notifications/initialized", json!({}))
        .await
        .context("notifications/initialized 失敗")?;
    let list = transport
        .request("tools/list", json!({}))
        .await
        .context("tools/list 失敗")?;
    let parsed: ToolsListResult =
        serde_json::from_value(list).context("tools/list result スキーマが想定外")?;
    Ok(parsed.tools)
}

// ---------------------------------------------------------------------------
// テスト
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn dummy_tool(name: &str) -> McpToolDef {
        McpToolDef {
            name: name.to_string(),
            description: None,
            input_schema: json!({"type": "object"}),
        }
    }

    fn dummy_transport() -> Transport {
        Transport::new(
            "http://invalid.invalid".to_string(),
            BTreeMap::new(),
            reqwest::Client::new(),
        )
    }

    #[test]
    fn catalog_disambiguates_collisions_across_servers() {
        let mut mgr = McpManager::empty();
        mgr.push_server(
            "a".into(),
            dummy_transport(),
            vec![dummy_tool("search"), dummy_tool("fetch")],
        );
        mgr.push_server(
            "b".into(),
            dummy_transport(),
            vec![dummy_tool("search")],
        );

        let names = mgr.public_tool_names();
        assert_eq!(names, vec!["a__fetch", "a__search", "b__search"]);

        let tools = mgr.as_openai_tools();
        assert_eq!(tools.len(), 3);
        assert!(tools.iter().all(|t| t.kind == "function"));
        let openai_names: Vec<&str> = tools.iter().map(|t| t.function.name.as_str()).collect();
        assert_eq!(openai_names, vec!["a__fetch", "a__search", "b__search"]);
    }

    #[test]
    fn empty_manager_yields_no_openai_tools() {
        let mgr = McpManager::empty();
        assert!(mgr.as_openai_tools().is_empty());
        assert!(mgr.public_tool_names().is_empty());
    }
}
