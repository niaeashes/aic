// mcp/manager — Manager that bundles all MCP servers and exposes the public-name catalog (SPEC §7.4, §14-6).
//
// The name exposed to the LLM is **always** the composite `"<server>__<tool>"`,
// held in a `BTreeMap`. `as_openai_tools()` hands these composite names to the
// LLM; on `tools/call`, we look up `(server_idx, real_tool_name)` from the
// map — we never re-parse the string (SPEC §7.4). Using `BTreeMap` guarantees
// sorted order so `as_openai_tools()` doesn't need to re-sort.
//
// Startup flow (called from main.rs):
//   1. `McpManager::connect_all(&settings, http)` runs `initialize →
//      notifications/initialized → tools/list` for every enabled server.
//   2. Per-server failure → log only, skip (aic startup doesn't fail).
//   3. Store the manager into ReplContext; agent.rs uses it from here on.

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

/// One MCP server's connection state plus the fetched tool list.
///
/// The server name is embedded in each public tool name (`<server>__<tool>`) and
/// recoverable via `catalog`, so we don't store it here. `tools` is kept for
/// `as_openai_tools` schema lookup.
struct McpServer {
    tools: Vec<McpToolDef>,
    transport: Transport,
}

pub struct McpManager {
    /// Connected servers. Index is referenced from `catalog`'s value (kept private).
    servers: Vec<McpServer>,
    /// Public name `"<server>__<tool>"` → (server_idx, real tool name). SPEC §7.4.
    /// BTreeMap keeps keys sorted — `as_openai_tools` doesn't need to re-sort.
    catalog: BTreeMap<String, (usize, String)>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self::empty()
    }
}

impl McpManager {
    /// Construct an "empty manager" so the REPL still runs even when no MCP
    /// servers are configured (or all failed).
    pub fn empty() -> Self {
        Self {
            servers: Vec::new(),
            catalog: BTreeMap::new(),
        }
    }

    /// Run initialize → tools/list against every enabled server. Per-server
    /// failures are absorbed.
    pub async fn connect_all(settings: &Settings, http: reqwest::Client) -> Self {
        let mut mgr = Self::empty();
        for cfg in &settings.mcp_servers {
            if !cfg.enabled {
                eprintln!("mcp: {} disabled, skipping", cfg.name);
                continue;
            }
            let mut transport = Transport::new(cfg.url.clone(), cfg.headers.clone(), http.clone());
            match initialize_and_list(&mut transport).await {
                Ok(tools) => {
                    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                    eprintln!(
                        "mcp: connected to {} (tools: {})",
                        cfg.name,
                        names.join(", ")
                    );
                    mgr.push_server(cfg.name.clone(), transport, tools);
                }
                Err(e) => {
                    eprintln!("warning: mcp {} connection failed (skipping): {e:#}", cfg.name);
                }
            }
        }
        // Print the final public-name list (post-collision-resolution) once.
        // The per-server log shows real names; this log shows what the model
        // actually sees in the catalog (SPEC §7.4).
        let names = mgr.public_tool_names();
        if !names.is_empty() {
            eprintln!("mcp: {} public tools: {}", names.len(), names.join(", "));
        }
        mgr
    }

    /// All public tool names (`<server>__<tool>`) in sorted order. Used by the
    /// startup catalog log.
    pub fn public_tool_names(&self) -> Vec<String> {
        self.catalog.keys().cloned().collect()
    }

    /// Build the OpenAI-compatible `tools` array. If empty, callers should
    /// substitute `None` so the `ChatRequest.tools` field can be omitted entirely.
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

    /// Invoke a tool by its public name. Returns concatenated text content
    /// joined by `\n`.
    pub async fn call(&mut self, public_name: &str, arguments: Value) -> Result<String> {
        let (idx, real_name) = self
            .catalog
            .get(public_name)
            .with_context(|| format!("unknown MCP tool: {public_name}"))?
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
            serde_json::from_value(value).context("unexpected tools/call result schema")?;

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

    /// Registration helper used by `connect_all` (and tests via `pub(crate)`).
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

/// Run initialize → notifications/initialized → tools/list, in order.
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
        .context("initialize failed")?;
    transport
        .notify("notifications/initialized", json!({}))
        .await
        .context("notifications/initialized failed")?;
    let list = transport
        .request("tools/list", json!({}))
        .await
        .context("tools/list failed")?;
    let parsed: ToolsListResult =
        serde_json::from_value(list).context("unexpected tools/list result schema")?;
    Ok(parsed.tools)
}

// ---------------------------------------------------------------------------
// Tests
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
