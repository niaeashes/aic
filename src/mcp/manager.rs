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
//      tools/list` for every enabled server (via mcp/connect.rs).
//   2. Per-server failure → log only, skip (aic startup doesn't fail).
//      OAuth-configured servers are skipped with a "run /auth <name>" notice —
//      tokens are in-memory only, so startup can never have one (SPEC §7.5).
//   3. Store the manager into ReplContext; agent.rs uses it from here on.
//
// INVARIANT: catalog values hold indices into `servers`, so entries are never
// removed from the Vec (indices must not shift). `/auth` replaces a server
// in-place at its index; `/auth logout` leaves a dead slot (name kept, catalog
// purged) that a later `/auth <name>` revives via `replace_or_push`.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::llm::types::{Tool, ToolFunction};
use crate::mcp::auth::ServerAuth;
use crate::mcp::connect::{connect_server, ConnectOutcome};
use crate::mcp::protocol::{ContentBlock, McpToolDef, ToolsCallResult};
use crate::mcp::server::McpServer;
use crate::mcp::transport::Transport;
use crate::mcp::unauthorized;

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
    /// failures are absorbed; OAuth servers wait for `/auth` (no token yet).
    pub async fn connect_all(settings: &Settings, http: reqwest::Client) -> Self {
        let mut mgr = Self::empty();
        // Track server names already wired (including auth-pending ones). Two
        // servers sharing a name would produce colliding `<name>__<tool>`
        // catalog keys, silently shadowing the first server's tools.
        let mut seen_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for cfg in &settings.mcp_servers {
            if !cfg.enabled {
                eprintln!("mcp: {} disabled, skipping", cfg.name);
                continue;
            }
            if !seen_names.insert(cfg.name.as_str()) {
                eprintln!(
                    "warning: mcp server name {:?} is duplicated; skipping the later definition",
                    cfg.name
                );
                continue;
            }
            match connect_server(cfg, http.clone(), None).await {
                ConnectOutcome::Connected {
                    transport,
                    tools,
                    auth,
                } => {
                    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
                    eprintln!("mcp: connected to {} (tools: {})", cfg.name, names.join(", "));
                    mgr.push_server(cfg.name.clone(), transport, tools, auth);
                }
                ConnectOutcome::NeedsAuth(reason) => {
                    eprintln!("mcp: {} skipped — {reason}; run /auth {}", cfg.name, cfg.name);
                }
                ConnectOutcome::Failed(e) => {
                    eprintln!("warning: mcp {} connection failed (skipping): {e:#}", cfg.name);
                }
            }
        }
        // Print the final public-name list (post-collision-resolution) once.
        let names = mgr.public_tool_names();
        if !names.is_empty() {
            eprintln!("mcp: {} public tools: {}", names.len(), names.join(", "));
        }
        mgr
    }

    /// All public tool names (`<server>__<tool>`) in sorted order.
    pub fn public_tool_names(&self) -> Vec<String> {
        self.catalog.keys().cloned().collect()
    }

    /// Number of MCP servers currently exposing at least one tool. Used by
    /// `/doctor` to flag the "configured but none connected" case (dead
    /// logged-out slots don't count).
    pub fn connected_server_count(&self) -> usize {
        self.servers.iter().filter(|s| !s.tools.is_empty()).count()
    }

    /// (tool_count, auth state) for the named server, if it has a slot.
    /// Used by the `/auth` listing; `None` means "never connected".
    pub fn server_status(&self, name: &str) -> Option<(usize, Option<&ServerAuth>)> {
        let s = self.servers.iter().find(|s| s.name == name)?;
        Some((s.tools.len(), s.auth.as_ref()))
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
    /// joined by `\n`. OAuth servers get a proactive token refresh first and
    /// one reactive refresh + retry when the server answers 401.
    pub async fn call(&mut self, public_name: &str, arguments: Value) -> Result<String> {
        let (idx, real_name) = self
            .catalog
            .get(public_name)
            .with_context(|| format!("unknown MCP tool: {public_name}"))?
            .clone();
        let server = &mut self.servers[idx];
        server.ensure_bearer().await?;
        let first = server.do_call(&real_name, arguments.clone()).await;
        let value = match first {
            Err(e) if unauthorized(&e).is_some() && server.can_refresh() => {
                server.force_refresh_bearer().await?;
                server.do_call(&real_name, arguments).await?
            }
            other => other?,
        };
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

    /// Replace the same-named server in place (reusing its index — see the
    /// header INVARIANT) or append a new one. Used by `/auth <name>`.
    pub fn replace_or_push(
        &mut self,
        name: String,
        transport: Transport,
        tools: Vec<McpToolDef>,
        auth: Option<ServerAuth>,
    ) {
        match self.servers.iter().position(|s| s.name == name) {
            Some(idx) => {
                self.catalog.retain(|_, (i, _)| *i != idx);
                for t in &tools {
                    self.catalog.insert(format!("{}__{}", name, t.name), (idx, t.name.clone()));
                }
                self.servers[idx] = McpServer {
                    name,
                    tools,
                    transport,
                    auth,
                };
            }
            None => self.push_server(name, transport, tools, auth),
        }
    }

    /// `/auth logout`: purge the server's catalog entries and drop its tokens.
    /// The Vec slot stays (indices must not shift) as a dead entry that a later
    /// `/auth <name>` revives. Returns false when no such server exists.
    pub fn remove_server_tools(&mut self, name: &str) -> bool {
        let Some(idx) = self.servers.iter().position(|s| s.name == name) else {
            return false;
        };
        self.catalog.retain(|_, (i, _)| *i != idx);
        let server = &mut self.servers[idx];
        server.tools.clear();
        server.auth = None;
        server.transport.set_bearer(None);
        true
    }

    /// Registration helper used by `connect_all` / `replace_or_push` (and tests).
    pub(crate) fn push_server(
        &mut self,
        name: String,
        transport: Transport,
        tools: Vec<McpToolDef>,
        auth: Option<ServerAuth>,
    ) {
        let idx = self.servers.len();
        for t in &tools {
            let public = format!("{}__{}", name, t.name);
            self.catalog.insert(public, (idx, t.name.clone()));
        }
        self.servers.push(McpServer {
            name,
            tools,
            transport,
            auth,
        });
    }
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
            None,
        );
        mgr.push_server("b".into(), dummy_transport(), vec![dummy_tool("search")], None);

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

    #[test]
    fn replace_or_push_reuses_index_and_purges_stale_entries() {
        let mut mgr = McpManager::empty();
        mgr.push_server(
            "a".into(),
            dummy_transport(),
            vec![dummy_tool("search"), dummy_tool("fetch")],
            None,
        );
        mgr.push_server("b".into(), dummy_transport(), vec![dummy_tool("search")], None);

        // Replace "a" with a smaller tool set: a__fetch must disappear and
        // b's catalog entries must still point at the right server.
        mgr.replace_or_push("a".into(), dummy_transport(), vec![dummy_tool("search")], None);
        assert_eq!(mgr.public_tool_names(), vec!["a__search", "b__search"]);
        assert_eq!(mgr.servers.len(), 2);
        assert_eq!(mgr.catalog["b__search"].0, 1);

        // Unknown name appends.
        mgr.replace_or_push("c".into(), dummy_transport(), vec![dummy_tool("x")], None);
        assert_eq!(mgr.servers.len(), 3);
        assert_eq!(mgr.catalog["c__x"].0, 2);
    }

    #[test]
    fn remove_server_tools_leaves_dead_slot_and_keeps_indices() {
        let mut mgr = McpManager::empty();
        mgr.push_server("a".into(), dummy_transport(), vec![dummy_tool("search")], None);
        mgr.push_server("b".into(), dummy_transport(), vec![dummy_tool("search")], None);

        assert!(mgr.remove_server_tools("a"));
        assert_eq!(mgr.public_tool_names(), vec!["b__search"]);
        assert_eq!(mgr.servers.len(), 2); // dead slot kept
        assert_eq!(mgr.connected_server_count(), 1);
        let (tool_count, auth) = mgr.server_status("a").unwrap();
        assert_eq!(tool_count, 0);
        assert!(auth.is_none());
        assert!(mgr.server_status("zzz").is_none());

        // Revive via replace_or_push at the same index.
        mgr.replace_or_push("a".into(), dummy_transport(), vec![dummy_tool("fetch")], None);
        assert_eq!(mgr.public_tool_names(), vec!["a__fetch", "b__search"]);
        assert_eq!(mgr.catalog["a__fetch"].0, 0);

        assert!(!mgr.remove_server_tools("nope"));
    }
}
