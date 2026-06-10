// mcp/server — one MCP server's connection state (SPEC §7.4, §7.5).
//
// Extracted from manager.rs (300-line rule). `McpServer` bundles the live
// transport, the fetched tool list, and the in-memory OAuth state; the
// catalog that maps public tool names onto these stays in McpManager.
//
// The auth helpers live HERE (not on McpManager) on purpose: manager's
// `call()` holds `&mut self.servers[idx]` across awaits, so everything it
// needs must be reachable through that one borrow.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::mcp::auth::ServerAuth;
use crate::mcp::protocol::{McpToolDef, ToolsCallParams};
use crate::mcp::transport::Transport;

pub(crate) struct McpServer {
    pub(crate) name: String,
    pub(crate) tools: Vec<McpToolDef>,
    pub(crate) transport: Transport,
    /// In-memory OAuth state; `None` for static-header servers.
    pub(crate) auth: Option<ServerAuth>,
}

impl McpServer {
    /// Proactively refresh the access token if expired, then (re)attach it.
    pub(crate) async fn ensure_bearer(&mut self) -> Result<()> {
        if let Some(a) = &mut self.auth {
            a.ensure_fresh().await?;
            self.transport.set_bearer(Some(a.access_token().to_string()));
        }
        Ok(())
    }

    /// Unconditional refresh — the reactive 401 path.
    pub(crate) async fn force_refresh_bearer(&mut self) -> Result<()> {
        let a = self.auth.as_mut().context("server has no OAuth state")?;
        a.force_refresh().await?;
        self.transport.set_bearer(Some(a.access_token().to_string()));
        Ok(())
    }

    pub(crate) fn can_refresh(&self) -> bool {
        self.auth.as_ref().is_some_and(|a| a.can_refresh())
    }

    pub(crate) async fn do_call(&mut self, real_name: &str, arguments: Value) -> Result<Value> {
        self.transport
            .request(
                "tools/call",
                ToolsCallParams {
                    name: real_name,
                    arguments,
                },
            )
            .await
    }
}
