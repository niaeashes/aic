// auth — OAuth 2.1 + CIMD authorization for MCP servers (SPEC §7.5).
//
// CIMD (Client ID Metadata Documents): the OAuth client_id is the HTTPS URL of
// a user-hosted JSON metadata document; the authorization server fetches it.
// No dynamic client registration, no client secret (public client + PKCE).
//
// Tokens are held IN MEMORY ONLY (a deliberate scope decision): nothing is
// written to disk and every aic restart requires a fresh `/auth <server>`.
// Within a session, expiry is handled by proactive refresh (`ensure_fresh`)
// plus one reactive refresh on 401 (manager.rs).
//
// Sub-modules:
//   discovery.rs — RFC 9728 / RFC 8414 metadata resolution + CIMD/S256 checks
//   flow.rs      — PKCE, authorize-URL construction, token endpoint requests
//   loopback.rs  — RFC 8252 loopback redirect listener

pub mod discovery;
pub mod flow;
pub mod loopback;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::config::{McpServerCfg, OAuthCfg};

/// How long `/auth` waits for the browser redirect before giving up.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// Safety margin subtracted from `expires_in` (clock skew, in-flight time).
const EXPIRY_SKEW_SECS: u64 = 60;

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// TokenSet
// ---------------------------------------------------------------------------

/// The tokens granted for one MCP server. In-memory only.
#[derive(Debug, Clone)]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds, already skew-adjusted. `None` (no `expires_in` from the
    /// AS) means "never proactively refresh" — the reactive 401 path covers it.
    pub expires_at: Option<u64>,
    /// Scope actually granted; echoed on refresh requests.
    pub scope: Option<String>,
}

impl TokenSet {
    fn from_response(resp: flow::TokenResponse, now: u64) -> Self {
        Self {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
            expires_at: resp.expires_in.map(|s| now + s.saturating_sub(EXPIRY_SKEW_SECS)),
            scope: resp.scope,
        }
    }

    fn is_expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|t| now >= t)
    }

    /// Refresh-token rotation: adopt the new set but keep the old refresh
    /// token when the AS didn't issue a replacement.
    fn apply_refresh(&mut self, resp: flow::TokenResponse, now: u64) {
        let old_refresh = self.refresh_token.take();
        *self = TokenSet::from_response(resp, now);
        if self.refresh_token.is_none() {
            self.refresh_token = old_refresh;
        }
    }
}

// ---------------------------------------------------------------------------
// ServerAuth — per-server in-memory auth state (owned by manager::McpServer)
// ---------------------------------------------------------------------------

pub struct ServerAuth {
    /// RFC 8707 canonical resource URI (the `resource` parameter value).
    resource: String,
    /// CIMD document URL — the OAuth client_id itself.
    client_id: String,
    token_endpoint: String,
    http: reqwest::Client,
    tokens: TokenSet,
}

impl ServerAuth {
    pub fn access_token(&self) -> &str {
        &self.tokens.access_token
    }

    pub fn can_refresh(&self) -> bool {
        self.tokens.refresh_token.is_some()
    }

    /// Human-readable token state for `/auth` listing.
    pub fn describe(&self) -> String {
        match self.tokens.expires_at {
            Some(t) => {
                let now = now_unix();
                if now >= t {
                    let how = if self.can_refresh() {
                        "refreshable"
                    } else {
                        "no refresh token"
                    };
                    format!("token expired ({how})")
                } else {
                    format!("token valid for {}s", t - now)
                }
            }
            None => "token valid (no expiry reported)".to_string(),
        }
    }

    /// Refresh if the access token is past its (skew-adjusted) expiry.
    pub async fn ensure_fresh(&mut self) -> Result<()> {
        if self.tokens.is_expired(now_unix()) {
            self.force_refresh().await?;
        }
        Ok(())
    }

    /// Unconditional refresh — the reactive 401 path.
    pub async fn force_refresh(&mut self) -> Result<()> {
        let Some(refresh_token) = self.tokens.refresh_token.clone() else {
            bail!("access token rejected and no refresh token was granted — run /auth to log in again");
        };
        let resp = flow::refresh(
            &self.http,
            &self.token_endpoint,
            &self.client_id,
            &refresh_token,
            &self.resource,
            self.tokens.scope.as_deref(),
        )
        .await
        .map_err(|e| {
            if format!("{e:#}").contains(flow::INVALID_GRANT) {
                anyhow::anyhow!("refresh token rejected ({e:#}) — run /auth to log in again")
            } else {
                e
            }
        })?;
        self.tokens.apply_refresh(resp, now_unix());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Interactive login (the `/auth <name>` body)
// ---------------------------------------------------------------------------

/// Full browser flow: discovery → PKCE/state → loopback wait → code exchange.
/// Prints progress to the terminal; never called at startup (SPEC §7.5 — only
/// `/auth` may open a browser).
pub async fn interactive_login(
    cfg: &McpServerCfg,
    oauth: &OAuthCfg,
    http: &reqwest::Client,
) -> Result<ServerAuth> {
    validate_client_id(&oauth.client_id)?;
    let resource = canonical_resource(&cfg.url)?;

    eprintln!("auth: discovering authorization server for {} ...", cfg.url);
    let (prm, asm, www) = discovery::discover(http, &cfg.url).await?;

    // Scope precedence: config → WWW-Authenticate hint → PRM scopes_supported.
    let scope: Option<String> = oauth
        .scopes
        .as_ref()
        .filter(|v| !v.is_empty())
        .map(|v| v.join(" "))
        .or(www.scope)
        .or_else(|| prm.scopes_supported.filter(|v| !v.is_empty()).map(|v| v.join(" ")));

    let pkce = flow::generate_pkce();
    let state = flow::generate_state();
    let (listener, port) = loopback::bind().await?;
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let authorize_url = flow::build_authorize_url(
        &asm.authorization_endpoint,
        &oauth.client_id,
        &redirect_uri,
        &state,
        &pkce,
        &resource,
        scope.as_deref(),
    )?;

    eprintln!("auth: open this URL to log in (if the browser didn't launch):");
    eprintln!("  {authorize_url}");
    open_browser(&authorize_url);

    // Commands are NOT raced against Ctrl-C by the REPL loop (only chat turns
    // are), so the race lives here: signal, hard timeout, or callback.
    let callback = tokio::select! {
        r = loopback::wait_for_callback(listener, &state) => r?,
        _ = tokio::signal::ctrl_c() => bail!("login cancelled"),
        _ = tokio::time::sleep(LOGIN_TIMEOUT) => {
            bail!("login timed out after {} seconds", LOGIN_TIMEOUT.as_secs())
        }
    };

    eprintln!("auth: exchanging authorization code ...");
    let resp = flow::exchange_code(
        http,
        &asm.token_endpoint,
        &oauth.client_id,
        &callback.code,
        &redirect_uri,
        &pkce.verifier,
        &resource,
    )
    .await?;

    Ok(ServerAuth {
        resource,
        client_id: oauth.client_id.clone(),
        token_endpoint: asm.token_endpoint,
        http: http.clone(),
        tokens: TokenSet::from_response(resp, now_unix()),
    })
}

/// CIMD requires the client_id to be a fetchable HTTPS URL with a host.
fn validate_client_id(client_id: &str) -> Result<()> {
    let u = url::Url::parse(client_id)
        .with_context(|| format!("auth.client_id is not a valid URL: {client_id}"))?;
    if u.scheme() != "https" || u.host_str().is_none() {
        bail!(
            "auth.client_id must be an https:// URL hosting your CIMD document \
             (got {client_id})"
        );
    }
    Ok(())
}

/// RFC 8707 canonical resource URI: `url::Url` lowercases scheme/host and
/// drops default ports; we additionally strip any fragment. Path and query
/// are preserved exactly.
pub fn canonical_resource(server_url: &str) -> Result<String> {
    let mut u = url::Url::parse(server_url)
        .with_context(|| format!("invalid MCP server URL: {server_url}"))?;
    u.set_fragment(None);
    Ok(u.into())
}

/// Best-effort browser launch; the URL is always printed first so a failure
/// here only costs the user a copy-paste.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = Some("open");
    #[cfg(target_os = "linux")]
    let cmd = Some("xdg-open");
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let cmd: Option<&str> = None;

    if let Some(cmd) = cmd {
        let _ = std::process::Command::new(cmd)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn token_set(expires_at: Option<u64>) -> TokenSet {
        TokenSet {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at,
            scope: None,
        }
    }

    #[test]
    fn token_set_expiry_boundaries() {
        assert!(!token_set(None).is_expired(u64::MAX));
        assert!(!token_set(Some(100)).is_expired(99));
        assert!(token_set(Some(100)).is_expired(100));
        assert!(token_set(Some(100)).is_expired(101));
    }

    #[test]
    fn from_response_applies_skew() {
        let resp: flow::TokenResponse = serde_json::from_str(
            r#"{"access_token":"a","token_type":"Bearer","expires_in":3600}"#,
        )
        .unwrap();
        let t = TokenSet::from_response(resp, 1000);
        assert_eq!(t.expires_at, Some(1000 + 3600 - EXPIRY_SKEW_SECS));
    }

    #[test]
    fn refresh_rotation_keeps_old_refresh_token_when_absent() {
        let mut t = token_set(Some(10));
        let no_rotation: flow::TokenResponse =
            serde_json::from_str(r#"{"access_token":"a2","token_type":"Bearer"}"#).unwrap();
        t.apply_refresh(no_rotation, 50);
        assert_eq!(t.access_token, "a2");
        assert_eq!(t.refresh_token.as_deref(), Some("r"));

        let rotation: flow::TokenResponse = serde_json::from_str(
            r#"{"access_token":"a3","token_type":"Bearer","refresh_token":"r2"}"#,
        )
        .unwrap();
        t.apply_refresh(rotation, 60);
        assert_eq!(t.refresh_token.as_deref(), Some("r2"));
    }

    #[test]
    fn canonical_resource_normalizes_and_strips_fragment() {
        assert_eq!(
            canonical_resource("HTTPS://MCP.Example.COM:443/Path?q=1#frag").unwrap(),
            "https://mcp.example.com/Path?q=1"
        );
        // Non-default port and trailing slash are preserved.
        assert_eq!(
            canonical_resource("http://h:8080/mcp/").unwrap(),
            "http://h:8080/mcp/"
        );
    }

    #[test]
    fn client_id_must_be_https_url() {
        assert!(validate_client_id("https://me.github.io/aic-client.json").is_ok());
        assert!(validate_client_id("http://me.github.io/aic-client.json").is_err());
        assert!(validate_client_id("not a url").is_err());
    }
}
