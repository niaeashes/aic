// secrets — ${VAR} resolution + env.json / env.json.enc handling (SPEC §5).
//
// Resolution order follows SPEC §5: secrets map → process environment variables.
// The secrets map is built at startup via this fallback chain:
//
//   1. (macOS only) decrypt `<config_dir>/env.json.enc` with the Keychain key
//   2. `<config_dir>/env.json` (plaintext, for local editing)
//   3. Environment variables only
//
// A failure at any stage emits a warning and falls through to the next — startup
// never blocks (SPEC §5.2 end).
//
// Sub-module layout (introduced when secrets.rs was split):
//   crypto.rs    — ChaCha20-Poly1305 AEAD (pure functions)
//   keychain.rs  — macOS Keychain key management (non-macOS bails at runtime)
//   storage.rs   — env.json / env.json.enc file I/O
//   cli.rs       — `aic env seal/unseal` body (called by main.rs)
//
// Public surface is the `Secrets` type plus `seal` / `unseal` — three symbols.
// We re-export `seal` / `unseal` via `pub use` so the external path stays
// `crate::config::secrets::{Secrets, seal, unseal}`.

mod cli;
mod crypto;
mod keychain;
mod storage;

pub use cli::{seal, unseal};

use std::collections::HashMap;
use std::path::Path;

// Constants shared by sub-modules. `Secrets::load` uses these via
// `config_dir.join(...)`, so they live in mod.rs. cli.rs pulls them via
// `super::{ENV_JSON, ENV_JSON_ENC}`.
pub(super) const ENV_JSON: &str = "env.json";
pub(super) const ENV_JSON_ENC: &str = "env.json.enc";

/// Single entry point for `${VAR}` resolution.
///
/// Resolution order is SPEC §5: secrets map → process environment variables.
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    map: HashMap<String, String>,
}

impl Secrets {
    /// Empty `Secrets` that consults only the environment. For tests and emergency fallback.
    #[allow(dead_code)]
    pub fn from_env_only() -> Self {
        Self::default()
    }

    /// Startup load path. Reads env.json(.enc) from `config_dir`.
    ///
    /// Any stage's failure emits an `eprintln!` warning and proceeds to the next
    /// fallback. The returned `Secrets` always has environment-variable fallback,
    /// so the caller doesn't need to worry about partial failures (SPEC §5.2 end).
    ///
    /// On non-macOS when `env.json.enc` exists, `storage::decrypt_env_file` will
    /// bail inside `keychain::load_key()`. That error string is included in the
    /// fallback warning, so we don't need a cfg gate here.
    pub fn load(config_dir: &Path) -> Self {
        let enc_path = config_dir.join(ENV_JSON_ENC);
        let plain_path = config_dir.join(ENV_JSON);

        if enc_path.exists() {
            match storage::decrypt_env_file(&enc_path) {
                Ok(map) => return Self { map },
                Err(e) => {
                    eprintln!(
                        "warning: failed to decrypt {} ({e:#}). Falling back to env.json / environment variables.",
                        enc_path.display()
                    );
                }
            }
        }

        if plain_path.exists() {
            match storage::read_env_file(&plain_path) {
                Ok(map) => return Self { map },
                Err(e) => {
                    eprintln!(
                        "warning: failed to read {} ({e:#}). Starting with environment variables only.",
                        plain_path.display()
                    );
                }
            }
        }
        Self::default()
    }

    /// Resolve one `${VAR}` key. Secrets map first, environment variables as fallback.
    pub fn get(&self, key: &str) -> Option<String> {
        self.map
            .get(key)
            .cloned()
            .or_else(|| std::env::var(key).ok())
    }

    /// Expand every `${VAR}` in the input. Unresolved placeholders are left as-is
    /// (it's better for the config to fail loudly at API-call time than to silently
    /// substitute an empty string).
    ///
    /// `$$` is the literal-`$` escape. Bare `$` and unmatched `${` are passed through.
    pub fn expand(&self, input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        let mut chars = input.char_indices().peekable();
        while let Some((_, c)) = chars.next() {
            if c != '$' {
                out.push(c);
                continue;
            }
            match chars.peek().map(|&(_, ch)| ch) {
                Some('$') => {
                    // $$ → $ (literal escape)
                    chars.next();
                    out.push('$');
                }
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    let mut closed = false;
                    while let Some(&(_, ch)) = chars.peek() {
                        chars.next();
                        if ch == '}' {
                            closed = true;
                            break;
                        }
                        name.push(ch);
                    }
                    if !closed {
                        // `${...` never closed — emit it verbatim.
                        out.push('$');
                        out.push('{');
                        out.push_str(&name);
                        continue;
                    }
                    match self.get(&name) {
                        Some(v) => out.push_str(&v),
                        None => {
                            // Leave the placeholder visible when unresolved.
                            out.push_str("${");
                            out.push_str(&name);
                            out.push('}');
                        }
                    }
                }
                _ => out.push('$'),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_replaces_known_vars_from_map() {
        let mut s = Secrets::default();
        s.map.insert("FOO".into(), "bar".into());
        assert_eq!(s.expand("Bearer ${FOO}"), "Bearer bar");
    }

    #[test]
    fn expand_keeps_unknown_placeholder() {
        let s = Secrets::default();
        // A key we assume is also absent from the environment.
        assert_eq!(
            s.expand("x=${__AIC_TEST_DEFINITELY_UNSET}"),
            "x=${__AIC_TEST_DEFINITELY_UNSET}"
        );
    }

    #[test]
    fn expand_handles_double_dollar_escape() {
        let s = Secrets::default();
        assert_eq!(s.expand("price: $$5"), "price: $5");
    }

    #[test]
    fn expand_passes_through_unclosed_brace() {
        let s = Secrets::default();
        assert_eq!(s.expand("${oops"), "${oops");
    }

    #[test]
    fn map_takes_priority_over_env() {
        let mut s = Secrets::default();
        // Real env var manipulation would be racy in parallel tests, so we only
        // check the map path.
        s.map.insert("HOME".into(), "from-map".into());
        assert_eq!(s.expand("${HOME}"), "from-map");
    }
}
