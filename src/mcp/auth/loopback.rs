// auth/loopback — RFC 8252 loopback redirect listener (SPEC §7.5).
//
// Binds 127.0.0.1 on an ephemeral port and waits for the authorization
// server's browser redirect to `/callback`. Deliberately not a one-shot
// accept: browsers open speculative connections and request /favicon.ico,
// so anything that isn't `GET /callback…` gets a 404 and the loop continues.
// The overall deadline (timeout + Ctrl-C) is raced by the caller
// (`interactive_login`), not here.
//
// HTTP handling is hand-rolled on purpose — one request line is all we need;
// pulling in a server crate for this would violate the project's scope.

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Successful authorization redirect parameters.
pub struct Callback {
    pub code: String,
}

/// Bind 127.0.0.1:0 (ephemeral). Returns the listener and the chosen port.
/// We always use the literal `127.0.0.1`, never `localhost` (RFC 8252 §8.3).
pub async fn bind() -> Result<(TcpListener, u16)> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("failed to bind loopback listener on 127.0.0.1")?;
    let port = listener.local_addr().context("no local addr")?.port();
    Ok((listener, port))
}

/// Accept-loop until a `/callback` request arrives, then validate it.
/// The browser always gets a small HTML page (success or failure) before we
/// return. State mismatch / AS-reported errors abort the login — the code is
/// never exchanged in those cases.
pub async fn wait_for_callback(listener: TcpListener, expected_state: &str) -> Result<Callback> {
    loop {
        let (mut stream, _) = listener.accept().await.context("accept failed")?;
        let Some(target) = read_request_target(&mut stream).await else {
            continue; // unreadable / speculative connection
        };
        let (path, query) = match target.split_once('?') {
            Some((p, q)) => (p, q),
            None => (target.as_str(), ""),
        };
        if path != "/callback" {
            respond(&mut stream, 404, "Not Found", "No such page.").await;
            continue;
        }
        match parse_callback_query(query) {
            Ok(cb) => {
                if cb.state != expected_state {
                    respond(&mut stream, 400, "Bad Request", "State mismatch.").await;
                    bail!("authorization callback state mismatch — possible CSRF; aborting login");
                }
                respond(
                    &mut stream,
                    200,
                    "OK",
                    "aic: login complete. You can close this tab.",
                )
                .await;
                return Ok(Callback { code: cb.code });
            }
            Err(CallbackError::Denied { error, description }) => {
                respond(&mut stream, 200, "OK", "aic: login failed. See the terminal.").await;
                match description {
                    Some(d) => bail!("authorization server denied the request: {error} ({d})"),
                    None => bail!("authorization server denied the request: {error}"),
                }
            }
            Err(CallbackError::Missing(field)) => {
                respond(&mut stream, 400, "Bad Request", "Malformed callback.").await;
                bail!("authorization callback is missing `{field}`");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Request parsing (pure parts are unit-tested)
// ---------------------------------------------------------------------------

/// Read enough of the request to extract the target from the request line.
async fn read_request_target(stream: &mut TcpStream) -> Option<String> {
    // 8 KiB is far beyond any sane redirect URL; read once, best-effort.
    let mut buf = vec![0u8; 8192];
    let n = stream.read(&mut buf).await.ok()?;
    let head = String::from_utf8_lossy(&buf[..n]);
    parse_request_target(head.lines().next()?)
}

/// `"GET /callback?code=x HTTP/1.1"` → `Some("/callback?code=x")`.
fn parse_request_target(request_line: &str) -> Option<String> {
    let mut parts = request_line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    target.starts_with('/').then(|| target.to_string())
}

struct RawCallback {
    code: String,
    state: String,
}

enum CallbackError {
    /// AS sent `error=` (e.g. access_denied) instead of a code.
    Denied {
        error: String,
        description: Option<String>,
    },
    Missing(&'static str),
}

fn parse_callback_query(query: &str) -> Result<RawCallback, CallbackError> {
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut error_description = None;
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            "error_description" => error_description = Some(v.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = error {
        return Err(CallbackError::Denied {
            error,
            description: error_description,
        });
    }
    let code = code.ok_or(CallbackError::Missing("code"))?;
    let state = state.ok_or(CallbackError::Missing("state"))?;
    Ok(RawCallback { code, state })
}

/// Minimal hand-rolled HTTP response; errors are ignored (the browser side
/// is cosmetic — the flow outcome is decided by the return value).
async fn respond(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let html = format!(
        "<!doctype html><html><body><p>{body}</p></body></html>"
    );
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_target_parses_get_line() {
        assert_eq!(
            parse_request_target("GET /callback?code=x&state=y HTTP/1.1"),
            Some("/callback?code=x&state=y".to_string())
        );
    }

    #[test]
    fn request_target_rejects_garbage_and_non_get() {
        assert_eq!(parse_request_target("POST /callback HTTP/1.1"), None);
        assert_eq!(parse_request_target("garbage"), None);
        assert_eq!(parse_request_target(""), None);
    }

    #[test]
    fn callback_query_happy_path_decodes_percent_encoding() {
        let cb = parse_callback_query("code=ab%2Fcd&state=s1").ok().unwrap();
        assert_eq!(cb.code, "ab/cd");
        assert_eq!(cb.state, "s1");
    }

    #[test]
    fn callback_query_surfaces_as_error_params() {
        match parse_callback_query("error=access_denied&error_description=nope&state=s") {
            Err(CallbackError::Denied { error, description }) => {
                assert_eq!(error, "access_denied");
                assert_eq!(description.as_deref(), Some("nope"));
            }
            _ => panic!("expected Denied"),
        }
    }

    #[test]
    fn callback_query_flags_missing_fields() {
        assert!(matches!(
            parse_callback_query("state=only"),
            Err(CallbackError::Missing("code"))
        ));
        assert!(matches!(
            parse_callback_query("code=only"),
            Err(CallbackError::Missing("state"))
        ));
    }

    #[tokio::test]
    async fn non_callback_paths_get_404_and_loop_continues() {
        let (listener, port) = bind().await.unwrap();
        let wait = tokio::spawn(async move { wait_for_callback(listener, "S").await });

        // Speculative request first — must not terminate the wait.
        let mut s1 = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s1.write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n").await.unwrap();
        let mut buf = String::new();
        s1.read_to_string(&mut buf).await.unwrap();
        assert!(buf.starts_with("HTTP/1.1 404"));

        // Real callback completes the flow.
        let mut s2 = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        s2.write_all(b"GET /callback?code=C&state=S HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let cb = wait.await.unwrap().unwrap();
        assert_eq!(cb.code, "C");
    }
}
