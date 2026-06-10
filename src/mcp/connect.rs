// mcp/connect — initialize → tools/list connection sequence (SPEC §7.3, §7.5).
//
// Extracted from manager.rs so the OAuth-aware connection logic has a home
// without pushing manager.rs past the 300-line rule. Two callers:
//   - `McpManager::connect_all` (startup; `auth` is always None for
//     OAuth-configured servers because tokens are in-memory only — they get a
//     NeedsAuth outcome and a "run /auth <name>" notice, never a browser)
//   - `/auth <name>` (passes the freshly minted ServerAuth)

use anyhow::{Context, Result};
use serde_json::json;

use crate::config::McpServerCfg;
use crate::mcp::auth::ServerAuth;
use crate::mcp::protocol::{ClientInfo, InitializeParams, McpToolDef, ToolsListResult, PROTOCOL_VERSION};
use crate::mcp::transport::Transport;
use crate::mcp::unauthorized;

/// Result of one connection attempt. `NeedsAuth` is a normal state, not an
/// error — startup reports it and moves on.
pub enum ConnectOutcome {
    Connected {
        transport: Transport,
        tools: Vec<McpToolDef>,
        auth: Option<ServerAuth>,
    },
    NeedsAuth(String),
    Failed(anyhow::Error),
}

/// Connect to one MCP server, attaching the Bearer token when `auth` is given.
/// On a 401 during the handshake, tries one refresh + retry before giving up.
pub async fn connect_server(
    cfg: &McpServerCfg,
    http: reqwest::Client,
    mut auth: Option<ServerAuth>,
) -> ConnectOutcome {
    if cfg.auth.is_some() {
        if auth.is_none() {
            return ConnectOutcome::NeedsAuth(
                "OAuth login required (tokens are in-memory only)".to_string(),
            );
        }
        if cfg.headers.contains_key("Authorization") {
            eprintln!(
                "warning: mcp {} sets both `auth:` and a static Authorization header; \
                 the OAuth Bearer token wins",
                cfg.name
            );
        }
    }

    let mut transport = Transport::new(cfg.url.clone(), cfg.headers.clone(), http);
    if let Some(a) = &mut auth {
        if let Err(e) = a.ensure_fresh().await {
            return ConnectOutcome::NeedsAuth(format!("token refresh failed: {e:#}"));
        }
        transport.set_bearer(Some(a.access_token().to_string()));
    }

    match initialize_and_list(&mut transport).await {
        Ok(tools) => ConnectOutcome::Connected {
            transport,
            tools,
            auth,
        },
        // Reactive 401 during the handshake: refresh once and retry.
        Err(e) if unauthorized(&e).is_some() => {
            let Some(a) = &mut auth else {
                return ConnectOutcome::Failed(e);
            };
            if let Err(re) = a.force_refresh().await {
                return ConnectOutcome::NeedsAuth(format!("{re:#}"));
            }
            transport.set_bearer(Some(a.access_token().to_string()));
            match initialize_and_list(&mut transport).await {
                Ok(tools) => ConnectOutcome::Connected {
                    transport,
                    tools,
                    auth,
                },
                Err(e2) => ConnectOutcome::Failed(e2),
            }
        }
        Err(e) => ConnectOutcome::Failed(e),
    }
}

/// Run initialize → notifications/initialized → tools/list, in order (SPEC §7.3).
pub async fn initialize_and_list(transport: &mut Transport) -> Result<Vec<McpToolDef>> {
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
