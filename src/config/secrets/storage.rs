// storage — file I/O for env.json / env.json.enc.
//
// This file holds the "read a file and produce a HashMap<String,String>" logic.
// Encryption is delegated to `crypto`; key management to `keychain`. Because
// `keychain::load_key()` bails immediately on non-macOS, `decrypt_env_file` has
// no need for a cfg gate of its own — the caller just sees an Err and falls
// through (SPEC §5.2 end).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use super::{crypto, keychain};

pub(super) fn read_env_file(path: &Path) -> Result<HashMap<String, String>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read: {}", path.display()))?;
    let map: HashMap<String, String> = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse JSON: {}", path.display()))?;
    Ok(map)
}

pub(super) fn decrypt_env_file(path: &Path) -> Result<HashMap<String, String>> {
    let b64 = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read: {}", path.display()))?;
    let key = keychain::load_key()?.ok_or_else(|| {
        anyhow!(
            "no key found in Keychain (service={}, account={}). \
             Run `aic env seal` first",
            keychain::SERVICE,
            keychain::ACCOUNT
        )
    })?;
    let plaintext = crypto::decrypt(b64.trim(), &key)?;
    let map: HashMap<String, String> =
        serde_json::from_slice(&plaintext).context("failed to parse JSON after decryption")?;
    Ok(map)
}
