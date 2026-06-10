// mcp — MCP Streamable HTTP client (SPEC §7, §14-6).
//
// Sub-modules:
//   auth/        — OAuth 2.1 + CIMD authorization (SPEC §7.5)
//   connect.rs   — initialize → tools/list connection sequence
//   manager.rs   — McpManager (tool catalog, dispatch, connect_all)
//   server.rs    — McpServer (one connection's transport + tools + OAuth state)
//   protocol.rs  — JSON-RPC 2.0 types and MCP message types
//   transport.rs — POST-based Streamable HTTP transport

pub mod auth;
pub mod connect;
pub mod manager;
pub mod protocol;
pub(crate) mod server;
pub mod transport;

pub use manager::McpManager;

/// Typed HTTP-level error from the MCP transport, so callers can detect
/// status codes (notably 401 for the OAuth refresh-and-retry path) by
/// downcasting through anyhow.
///
/// Lives here rather than transport.rs to keep that file under the 300-line
/// rule (SPEC §3).
#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    pub body: String,
    /// `WWW-Authenticate` response header, when present. Carries the RFC 9728
    /// `resource_metadata` pointer. OAuth discovery currently harvests it via
    /// its own probe (`auth/discovery.rs`), so this is a debugging hook today.
    #[allow(dead_code)]
    pub www_authenticate: Option<String>,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Same wording as the pre-typed bail!, so user-visible errors don't change.
        write!(f, "MCP HTTP {}: {}", self.status, self.body)
    }
}

impl std::error::Error for HttpError {}

/// If `e` is (or wraps) an `HttpError` with status 401, return it.
/// Downcasting sees through any number of `.context()` layers.
pub fn unauthorized(e: &anyhow::Error) -> Option<&HttpError> {
    e.downcast_ref::<HttpError>().filter(|h| h.status == 401)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    fn err_401() -> anyhow::Error {
        anyhow::Error::new(HttpError {
            status: 401,
            body: "unauthorized".into(),
            www_authenticate: Some("Bearer resource_metadata=\"https://x/.well-known/oauth-protected-resource\"".into()),
        })
    }

    #[test]
    fn display_matches_legacy_bail_wording() {
        let e = HttpError {
            status: 500,
            body: "boom".into(),
            www_authenticate: None,
        };
        assert_eq!(e.to_string(), "MCP HTTP 500: boom");
    }

    #[test]
    fn unauthorized_downcasts_through_context_layers() {
        let plain = err_401();
        assert!(unauthorized(&plain).is_some());

        let one: anyhow::Error = Err::<(), _>(err_401())
            .context("initialize failed")
            .unwrap_err();
        assert!(unauthorized(&one).is_some());

        let two: anyhow::Error = Err::<(), _>(err_401())
            .context("inner")
            .context("outer")
            .unwrap_err();
        assert_eq!(unauthorized(&two).map(|h| h.status), Some(401));
    }

    #[test]
    fn unauthorized_ignores_other_statuses_and_error_kinds() {
        let forbidden = anyhow::Error::new(HttpError {
            status: 403,
            body: "no".into(),
            www_authenticate: None,
        });
        assert!(unauthorized(&forbidden).is_none());
        assert!(unauthorized(&anyhow::anyhow!("plain error")).is_none());
    }
}
