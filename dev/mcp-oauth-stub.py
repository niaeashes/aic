#!/usr/bin/env python3
"""Local OAuth (CIMD) + MCP stub for testing aic's /auth flow offline.

One process plays both roles on 127.0.0.1 (SPEC §7.5 test harness):

  resource server   POST /mcp                    401 without a valid Bearer,
                                                 minimal JSON-RPC with one
                                                 `echo` tool when authorized
  RFC 9728          GET  /.well-known/oauth-protected-resource[/mcp]
  RFC 8414          GET  /.well-known/oauth-authorization-server
                                                 advertises CIMD + S256
  authorization     GET  /authorize              auto-approves (no login UI),
                                                 302 straight back to the
                                                 client's loopback callback
  token             POST /token                  code (PKCE-verified) and
                                                 refresh_token grants; refresh
                                                 tokens rotate on every use

Deliberately lenient where a real AS must not be: the CIMD document is NOT
fetched (any https:// client_id passes), every authorization is approved, and
tokens are random strings held in memory. Never expose this beyond localhost.

Usage:
  python3 dev/mcp-oauth-stub.py [--port 8000] [--expires-in 3600]

  # --expires-in 30 makes every minted access token already "expired" under
  # aic's 60s skew, forcing the proactive-refresh path on each connection.

aic config to point at it (any https URL works as client_id):
  mcp_servers:
    - name: test
      url: http://127.0.0.1:8000/mcp
      auth:
        client_id: https://example.com/aic-client.json
"""

import argparse
import base64
import hashlib
import json
import secrets
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlencode, urlparse

ARGS = None  # set in main()

# In-memory grant state.
CODES = {}    # code -> {"challenge": str, "redirect_uri": str, "client_id": str}
ACCESS = {}   # access token -> expiry (unix secs)
REFRESH = {}  # refresh token -> client_id


def b64url_sha256(text: str) -> str:
    digest = hashlib.sha256(text.encode("ascii")).digest()
    return base64.urlsafe_b64encode(digest).rstrip(b"=").decode("ascii")


def issuer() -> str:
    return f"http://127.0.0.1:{ARGS.port}"


def mint_tokens(client_id: str) -> dict:
    access = secrets.token_urlsafe(24)
    refresh = secrets.token_urlsafe(24)
    ACCESS[access] = time.time() + ARGS.expires_in
    REFRESH[refresh] = client_id
    return {
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ARGS.expires_in,
        "refresh_token": refresh,
        "scope": "mcp.test",
    }


class Handler(BaseHTTPRequestHandler):
    server_version = "aic-oauth-stub/0.1"

    # ------------------------------------------------------------------ utils

    def send_json(self, obj, status=200, extra_headers=()):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for k, v in extra_headers:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def read_body(self) -> bytes:
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length)

    # ------------------------------------------------------------------- GET

    def do_GET(self):
        url = urlparse(self.path)
        if url.path in (
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
        ):
            self.send_json(
                {
                    "resource": f"{issuer()}/mcp",
                    "authorization_servers": [issuer()],
                    "scopes_supported": ["mcp.test"],
                }
            )
        elif url.path == "/.well-known/oauth-authorization-server":
            self.send_json(
                {
                    "issuer": issuer(),
                    "authorization_endpoint": f"{issuer()}/authorize",
                    "token_endpoint": f"{issuer()}/token",
                    "response_types_supported": ["code"],
                    "grant_types_supported": ["authorization_code", "refresh_token"],
                    "code_challenge_methods_supported": ["S256"],
                    "token_endpoint_auth_methods_supported": ["none"],
                    "client_id_metadata_document_supported": True,
                }
            )
        elif url.path == "/authorize":
            self.handle_authorize(parse_qs(url.query))
        else:
            self.send_json({"error": "not_found"}, status=404)

    def handle_authorize(self, q):
        def one(key):
            return q.get(key, [None])[0]

        client_id = one("client_id") or ""
        redirect_uri = one("redirect_uri") or ""
        state = one("state")
        challenge = one("code_challenge")
        problems = []
        if not client_id.startswith("https://"):
            problems.append("client_id must be an https:// CIMD URL")
        if urlparse(redirect_uri).hostname != "127.0.0.1":
            problems.append("redirect_uri must be a 127.0.0.1 loopback URL")
        if one("response_type") != "code":
            problems.append("response_type must be code")
        if one("code_challenge_method") != "S256" or not challenge:
            problems.append("PKCE S256 code_challenge required")
        if not state:
            problems.append("state required")
        if problems:
            self.send_json({"error": "invalid_request", "detail": problems}, status=400)
            return

        code = secrets.token_urlsafe(16)
        CODES[code] = {
            "challenge": challenge,
            "redirect_uri": redirect_uri,
            "client_id": client_id,
        }
        sep = "&" if "?" in redirect_uri else "?"
        location = f"{redirect_uri}{sep}{urlencode({'code': code, 'state': state})}"
        self.send_response(302)
        self.send_header("Location", location)
        self.send_header("Content-Length", "0")
        self.end_headers()

    # ------------------------------------------------------------------ POST

    def do_POST(self):
        url = urlparse(self.path)
        if url.path == "/token":
            self.handle_token()
        elif url.path == "/mcp":
            self.handle_mcp()
        else:
            self.send_json({"error": "not_found"}, status=404)

    def handle_token(self):
        form = parse_qs(self.read_body().decode())

        def one(key):
            return form.get(key, [None])[0]

        grant = one("grant_type")
        if grant == "authorization_code":
            entry = CODES.pop(one("code") or "", None)
            verifier = one("code_verifier") or ""
            if (
                entry is None
                or b64url_sha256(verifier) != entry["challenge"]
                or one("redirect_uri") != entry["redirect_uri"]
                or one("client_id") != entry["client_id"]
            ):
                self.send_json(
                    {"error": "invalid_grant", "error_description": "bad code/PKCE"},
                    status=400,
                )
                return
            self.send_json(mint_tokens(entry["client_id"]))
        elif grant == "refresh_token":
            client_id = REFRESH.pop(one("refresh_token") or "", None)  # rotation
            if client_id is None or one("client_id") != client_id:
                self.send_json(
                    {"error": "invalid_grant", "error_description": "unknown refresh token"},
                    status=400,
                )
                return
            self.send_json(mint_tokens(client_id))
        else:
            self.send_json({"error": "unsupported_grant_type"}, status=400)

    def handle_mcp(self):
        auth = self.headers.get("Authorization") or ""
        token = auth.removeprefix("Bearer ").strip()
        if not token or ACCESS.get(token, 0) < time.time():
            prm = f"{issuer()}/.well-known/oauth-protected-resource/mcp"
            self.send_json(
                {"error": "unauthorized"},
                status=401,
                extra_headers=[
                    ("WWW-Authenticate", f'Bearer resource_metadata="{prm}", scope="mcp.test"')
                ],
            )
            return

        msg = json.loads(self.read_body() or b"{}")
        method, msg_id = msg.get("method"), msg.get("id")
        if msg_id is None:  # notification (e.g. notifications/initialized)
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return

        if method == "initialize":
            result = {
                "protocolVersion": msg.get("params", {}).get("protocolVersion", "2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "aic-oauth-stub", "version": "0.1"},
            }
        elif method == "tools/list":
            result = {
                "tools": [
                    {
                        "name": "echo",
                        "description": "Echo the input text back.",
                        "inputSchema": {
                            "type": "object",
                            "properties": {"text": {"type": "string"}},
                            "required": ["text"],
                        },
                    }
                ]
            }
        elif method == "tools/call":
            text = msg.get("params", {}).get("arguments", {}).get("text", "")
            result = {"content": [{"type": "text", "text": f"echo: {text}"}], "isError": False}
        elif method == "ping":
            result = {}
        else:
            self.send_json(
                {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "error": {"code": -32601, "message": f"method not found: {method}"},
                }
            )
            return
        self.send_json({"jsonrpc": "2.0", "id": msg_id, "result": result})

    def log_message(self, fmt, *args):  # noqa: A003 - keep default signature
        sys.stderr.write("stub: %s\n" % (fmt % args))


def main():
    global ARGS
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, default=8000)
    parser.add_argument("--expires-in", type=int, default=3600)
    ARGS = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", ARGS.port), Handler)
    sys.stderr.write(f"stub: serving on {issuer()} (MCP at {issuer()}/mcp)\n")
    server.serve_forever()


if __name__ == "__main__":
    main()
