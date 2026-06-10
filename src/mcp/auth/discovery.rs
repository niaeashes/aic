// auth/discovery — OAuth server discovery for an MCP resource (SPEC §7.5).
//
// Chain (MCP authorization spec):
//   1. Unauthenticated POST to the MCP server → 401 with
//      `WWW-Authenticate: Bearer resource_metadata="…"` (RFC 9728). If the
//      header is missing, fall back to the well-known PRM locations.
//   2. GET Protected Resource Metadata → `authorization_servers[0]`.
//   3. GET Authorization Server Metadata (RFC 8414; OIDC discovery fallback).
//   4. Hard requirements: PKCE S256 advertised, and
//      `client_id_metadata_document_supported: true` (CIMD) — aic has no DCR
//      fallback, so we refuse early with a clear message instead of letting the
//      AS show an opaque error page.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::json;

// ---------------------------------------------------------------------------
// Metadata shapes
// ---------------------------------------------------------------------------

/// RFC 9728 Protected Resource Metadata (the fields we use).
#[derive(Debug, Deserialize)]
pub struct PrmMetadata {
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
}

/// RFC 8414 Authorization Server Metadata (the fields we use).
#[derive(Debug, Deserialize)]
pub struct AsMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub code_challenge_methods_supported: Option<Vec<String>>,
    /// CIMD support flag (draft-ietf-oauth-client-id-metadata-document).
    #[serde(default)]
    pub client_id_metadata_document_supported: Option<bool>,
}

/// Parsed `WWW-Authenticate: Bearer …` parameters we care about.
#[derive(Debug, Default, PartialEq)]
pub struct WwwAuth {
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
}

// ---------------------------------------------------------------------------
// Discovery driver
// ---------------------------------------------------------------------------

/// Probe the MCP server and resolve its authorization server's metadata.
pub async fn discover(
    http: &reqwest::Client,
    server_url: &str,
) -> Result<(PrmMetadata, AsMetadata, WwwAuth)> {
    let www = probe_www_authenticate(http, server_url).await;

    // PRM candidates: the server-supplied pointer first, then well-known guesses.
    let mut prm_urls: Vec<String> = Vec::new();
    if let Some(u) = &www.resource_metadata {
        prm_urls.push(u.clone());
    }
    prm_urls.extend(well_known_prm_urls(server_url)?);

    let prm: PrmMetadata = fetch_first_json(http, &prm_urls)
        .await
        .context("could not fetch OAuth protected resource metadata (RFC 9728)")?;
    if prm.authorization_servers.is_empty() {
        bail!("protected resource metadata lists no authorization_servers");
    }
    if prm.authorization_servers.len() > 1 {
        eprintln!(
            "warning: multiple authorization servers advertised; using the first: {}",
            prm.authorization_servers[0]
        );
    }
    let issuer = prm.authorization_servers[0].clone();

    let asm: AsMetadata = fetch_first_json(http, &well_known_as_urls(&issuer)?)
        .await
        .with_context(|| format!("could not fetch authorization server metadata for {issuer}"))?;

    // MCP auth spec: the client MUST refuse if S256 isn't advertised.
    let s256_ok = asm
        .code_challenge_methods_supported
        .as_ref()
        .is_some_and(|m| m.iter().any(|v| v == "S256"));
    if !s256_ok {
        bail!(
            "authorization server {issuer} does not advertise PKCE S256 \
             (code_challenge_methods_supported); refusing to continue"
        );
    }
    // CIMD: a URL client_id may only be used against an AS that opted in.
    if asm.client_id_metadata_document_supported != Some(true) {
        bail!(
            "authorization server {issuer} does not advertise CIMD support \
             (client_id_metadata_document_supported: true). aic has no dynamic \
             client registration fallback — use static `headers` auth for this \
             server instead"
        );
    }

    Ok((prm, asm, www))
}

/// Fire one unauthenticated JSON-RPC POST at the server purely to harvest the
/// 401 `WWW-Authenticate` header. Any other outcome (success, network error)
/// yields an empty WwwAuth — the caller falls back to well-known URLs.
async fn probe_www_authenticate(http: &reqwest::Client, server_url: &str) -> WwwAuth {
    let body = json!({"jsonrpc": "2.0", "id": 0, "method": "ping"});
    let resp = http
        .post(server_url)
        .header("Accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await;
    match resp {
        Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => r
            .headers()
            .get("WWW-Authenticate")
            .and_then(|h| h.to_str().ok())
            .map(parse_www_authenticate)
            .unwrap_or_default(),
        _ => WwwAuth::default(),
    }
}

/// GET each candidate in order; first 200 with valid JSON wins.
async fn fetch_first_json<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    urls: &[String],
) -> Result<T> {
    let mut last_err = None;
    for u in urls {
        match http.get(u).header("Accept", "application/json").send().await {
            Ok(r) if r.status().is_success() => match r.json::<T>().await {
                Ok(v) => return Ok(v),
                Err(e) => last_err = Some(format!("{u}: invalid JSON ({e})")),
            },
            Ok(r) => last_err = Some(format!("{u}: HTTP {}", r.status().as_u16())),
            Err(e) => last_err = Some(format!("{u}: {e}")),
        }
    }
    bail!(
        "no candidate URL answered with valid metadata (last: {})",
        last_err.unwrap_or_else(|| "no candidates".into())
    )
}

// ---------------------------------------------------------------------------
// URL candidates (pure, unit-tested)
// ---------------------------------------------------------------------------

/// RFC 9728 §3: path-inserted well-known first, then host root.
pub fn well_known_prm_urls(server_url: &str) -> Result<Vec<String>> {
    let u = url::Url::parse(server_url).with_context(|| format!("invalid URL: {server_url}"))?;
    let origin = origin_of(&u)?;
    let mut out = Vec::new();
    if u.path() != "/" && !u.path().is_empty() {
        out.push(format!(
            "{origin}/.well-known/oauth-protected-resource{}",
            u.path()
        ));
    }
    out.push(format!("{origin}/.well-known/oauth-protected-resource"));
    Ok(out)
}

/// RFC 8414 §3 + OIDC discovery fallbacks, order pinned:
///   1. `{origin}/.well-known/oauth-authorization-server{path}`
///   2. `{origin}/.well-known/openid-configuration{path}`
///   3. `{issuer}/.well-known/openid-configuration`
/// (for a path-less issuer, 1 and 2 collapse to the path-less forms and 3 == 2).
pub fn well_known_as_urls(issuer: &str) -> Result<Vec<String>> {
    let u = url::Url::parse(issuer).with_context(|| format!("invalid issuer URL: {issuer}"))?;
    let origin = origin_of(&u)?;
    let path = u.path().trim_end_matches('/');
    let mut out = Vec::new();
    if path.is_empty() {
        out.push(format!("{origin}/.well-known/oauth-authorization-server"));
        out.push(format!("{origin}/.well-known/openid-configuration"));
    } else {
        out.push(format!(
            "{origin}/.well-known/oauth-authorization-server{path}"
        ));
        out.push(format!("{origin}/.well-known/openid-configuration{path}"));
        out.push(format!("{origin}{path}/.well-known/openid-configuration"));
    }
    Ok(out)
}

fn origin_of(u: &url::Url) -> Result<String> {
    let host = u.host_str().context("URL has no host")?;
    Ok(match u.port() {
        Some(p) => format!("{}://{}:{}", u.scheme(), host, p),
        None => format!("{}://{}", u.scheme(), host),
    })
}

/// Parse `Bearer k1="v1", k2=v2` parameters (quoted and bare forms).
/// Forgiving by design: unknown parameters are skipped, scheme prefix optional.
pub fn parse_www_authenticate(header: &str) -> WwwAuth {
    let rest = header
        .trim()
        .strip_prefix("Bearer ")
        .or_else(|| header.trim().strip_prefix("bearer "))
        .unwrap_or(header);
    let mut out = WwwAuth::default();
    for part in rest.split(',') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim().trim_matches('"').to_string();
        match key.as_str() {
            "resource_metadata" => out.resource_metadata = Some(val),
            "scope" => out.scope = Some(val),
            _ => {}
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn www_authenticate_parses_quoted_and_bare_values() {
        let w = parse_www_authenticate(
            r#"Bearer resource_metadata="https://h/.well-known/oauth-protected-resource/mcp", scope=mcp.read"#,
        );
        assert_eq!(
            w.resource_metadata.as_deref(),
            Some("https://h/.well-known/oauth-protected-resource/mcp")
        );
        assert_eq!(w.scope.as_deref(), Some("mcp.read"));
    }

    #[test]
    fn www_authenticate_ignores_unknown_params_and_missing_fields() {
        let w = parse_www_authenticate(r#"Bearer realm="x", error="invalid_token""#);
        assert_eq!(w, WwwAuth::default());
    }

    #[test]
    fn prm_candidates_are_path_inserted_then_root() {
        let v = well_known_prm_urls("https://h.example/mcp").unwrap();
        assert_eq!(
            v,
            vec![
                "https://h.example/.well-known/oauth-protected-resource/mcp".to_string(),
                "https://h.example/.well-known/oauth-protected-resource".to_string(),
            ]
        );
    }

    #[test]
    fn prm_candidates_for_root_url_skip_path_form() {
        let v = well_known_prm_urls("https://h.example/").unwrap();
        assert_eq!(
            v,
            vec!["https://h.example/.well-known/oauth-protected-resource".to_string()]
        );
    }

    #[test]
    fn as_candidates_for_pathless_issuer() {
        let v = well_known_as_urls("https://as.example").unwrap();
        assert_eq!(
            v,
            vec![
                "https://as.example/.well-known/oauth-authorization-server".to_string(),
                "https://as.example/.well-known/openid-configuration".to_string(),
            ]
        );
    }

    #[test]
    fn as_candidates_for_issuer_with_path_keep_pinned_order() {
        let v = well_known_as_urls("https://as.example/tenant1").unwrap();
        assert_eq!(
            v,
            vec![
                "https://as.example/.well-known/oauth-authorization-server/tenant1".to_string(),
                "https://as.example/.well-known/openid-configuration/tenant1".to_string(),
                "https://as.example/tenant1/.well-known/openid-configuration".to_string(),
            ]
        );
    }

    #[test]
    fn as_metadata_deserializes_with_and_without_cimd_flag() {
        let with: AsMetadata = serde_json::from_str(
            r#"{"authorization_endpoint":"https://a/auth","token_endpoint":"https://a/token",
                "code_challenge_methods_supported":["S256"],
                "client_id_metadata_document_supported":true}"#,
        )
        .unwrap();
        assert_eq!(with.client_id_metadata_document_supported, Some(true));

        let without: AsMetadata = serde_json::from_str(
            r#"{"authorization_endpoint":"https://a/auth","token_endpoint":"https://a/token"}"#,
        )
        .unwrap();
        assert_eq!(without.client_id_metadata_document_supported, None);
        assert!(without.code_challenge_methods_supported.is_none());
    }

    #[test]
    fn non_default_port_is_kept_in_candidates() {
        let v = well_known_prm_urls("http://127.0.0.1:8000/mcp").unwrap();
        assert_eq!(
            v[0],
            "http://127.0.0.1:8000/.well-known/oauth-protected-resource/mcp"
        );
    }
}
