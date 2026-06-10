// auth/flow — PKCE, authorization-URL construction, token exchange (SPEC §7.5).
//
// Pure OAuth 2.1 mechanics for a public client (no client secret):
//   - PKCE S256 (RFC 7636): verifier = 32 random bytes b64url-nopad (43 chars),
//     challenge = b64url-nopad(sha256(verifier))
//   - Authorization request built with url::Url so percent-encoding is correct
//     (the CIMD client_id is itself a URL and must survive encoding)
//   - Token endpoint POSTs are form-urlencoded with NO client authentication
//     (token_endpoint_auth_method: none); the RFC 8707 `resource` parameter is
//     sent on every request per the MCP authorization spec.

use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// PKCE / state
// ---------------------------------------------------------------------------

pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn generate_pkce() -> Pkce {
    let verifier = random_b64url_32();
    Pkce {
        challenge: challenge_for(&verifier),
        verifier,
    }
}

pub fn generate_state() -> String {
    random_b64url_32()
}

fn random_b64url_32() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// S256: b64url-nopad(sha256(ascii(verifier))). Split out so the RFC 7636
/// Appendix B test vector can pin it.
fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

// ---------------------------------------------------------------------------
// Authorization request URL
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn build_authorize_url(
    authorization_endpoint: &str,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    pkce: &Pkce,
    resource: &str,
    scope: Option<&str>,
) -> Result<String> {
    let mut u = url::Url::parse(authorization_endpoint)
        .with_context(|| format!("invalid authorization_endpoint: {authorization_endpoint}"))?;
    u.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("state", state)
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("resource", resource);
    if let Some(s) = scope {
        u.query_pairs_mut().append_pair("scope", s);
    }
    Ok(u.into())
}

// ---------------------------------------------------------------------------
// Token endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[allow(dead_code)]
    pub token_type: String,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// OAuth error body (RFC 6749 §5.2). Parsed from non-2xx token responses so
/// `invalid_grant` (expired/revoked refresh token) is detectable upstream.
#[derive(Debug, Deserialize)]
struct OAuthErrorBody {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

/// Marker substring used by ServerAuth to map refresh failures to a
/// "run /auth <name>" hint.
pub const INVALID_GRANT: &str = "invalid_grant";

pub async fn exchange_code(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
    resource: &str,
) -> Result<TokenResponse> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
        ("resource", resource),
    ];
    post_token(http, token_endpoint, &params)
        .await
        .context("authorization code exchange failed")
}

pub async fn refresh(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &str,
    resource: &str,
    scope: Option<&str>,
) -> Result<TokenResponse> {
    let mut params = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
        ("resource", resource),
    ];
    if let Some(s) = scope {
        params.push(("scope", s));
    }
    post_token(http, token_endpoint, &params)
        .await
        .context("token refresh failed")
}

async fn post_token(
    http: &reqwest::Client,
    token_endpoint: &str,
    params: &[(&str, &str)],
) -> Result<TokenResponse> {
    let resp = http
        .post(token_endpoint)
        .form(params)
        .send()
        .await
        .with_context(|| format!("POST {token_endpoint} failed"))?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        // Prefer the structured OAuth error; fall back to the raw body.
        let msg = match serde_json::from_str::<OAuthErrorBody>(&body) {
            Ok(e) => match e.error_description {
                Some(d) => format!("{} ({d})", e.error),
                None => e.error,
            },
            Err(_) => body,
        };
        anyhow::bail!("token endpoint returned HTTP {}: {msg}", status.as_u16());
    }
    serde_json::from_str(&body)
        .with_context(|| format!("unexpected token endpoint response: {body}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_appendix_b_vector() {
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn pkce_verifier_is_43_unreserved_chars() {
        let p = generate_pkce();
        assert_eq!(p.verifier.len(), 43); // 32 bytes b64url-nopad
        assert!(p
            .verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_eq!(p.challenge, challenge_for(&p.verifier));
    }

    #[test]
    fn state_values_differ() {
        assert_ne!(generate_state(), generate_state());
    }

    #[test]
    fn authorize_url_carries_all_required_params_encoded() {
        let pkce = Pkce {
            verifier: "v".into(),
            challenge: "c-hallenge".into(),
        };
        let u = build_authorize_url(
            "https://as.example/authorize?audience=x",
            "https://me.github.io/aic-client.json",
            "http://127.0.0.1:49152/callback",
            "st4te",
            &pkce,
            "https://mcp.example/mcp?v=1",
            Some("mcp.read mcp.write"),
        )
        .unwrap();
        // Pre-existing query params survive.
        assert!(u.contains("audience=x"));
        // client_id / resource / redirect_uri are percent-encoded URLs.
        assert!(u.contains("client_id=https%3A%2F%2Fme.github.io%2Faic-client.json"));
        assert!(u.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A49152%2Fcallback"));
        assert!(u.contains("resource=https%3A%2F%2Fmcp.example%2Fmcp%3Fv%3D1"));
        assert!(u.contains("response_type=code"));
        assert!(u.contains("state=st4te"));
        assert!(u.contains("code_challenge=c-hallenge"));
        assert!(u.contains("code_challenge_method=S256"));
        assert!(u.contains("scope=mcp.read+mcp.write"));
    }

    #[test]
    fn authorize_url_omits_scope_when_none() {
        let pkce = generate_pkce();
        let u = build_authorize_url(
            "https://as.example/authorize",
            "https://me.github.io/c.json",
            "http://127.0.0.1:1/callback",
            "s",
            &pkce,
            "https://mcp.example/mcp",
            None,
        )
        .unwrap();
        assert!(!u.contains("scope="));
    }

    #[test]
    fn token_response_deserializes_with_and_without_optionals() {
        let full: TokenResponse = serde_json::from_str(
            r#"{"access_token":"a","token_type":"Bearer","expires_in":3600,
                "refresh_token":"r","scope":"mcp.read"}"#,
        )
        .unwrap();
        assert_eq!(full.expires_in, Some(3600));
        assert_eq!(full.refresh_token.as_deref(), Some("r"));

        let minimal: TokenResponse =
            serde_json::from_str(r#"{"access_token":"a","token_type":"Bearer"}"#).unwrap();
        assert!(minimal.expires_in.is_none());
        assert!(minimal.refresh_token.is_none());
        assert!(minimal.scope.is_none());
    }
}
