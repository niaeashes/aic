// cli — implementation of `aic env seal` / `aic env unseal`.
//
// Called from `main.rs` as `config::secrets::seal` / `unseal`. The path stays
// the same as before the secrets module was split into a directory, thanks to
// `pub use` in mod.rs.
//
// Role: file read → key retrieval → encrypt/decrypt → file write. Encryption
// goes through `crypto`, key handling through `keychain` — no cfg gate here.
// On non-macOS, `keychain::load_or_create_key` / `load_key` bail immediately,
// surfacing a clear error to the user.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

use super::{crypto, keychain, ENV_JSON, ENV_JSON_ENC};

/// `<config_dir>/env.json` → `<config_dir>/env.json.enc`.
/// If Keychain has no key, generate one and store it (DoD: reuse if already present).
pub fn seal(config_dir: &Path) -> Result<()> {
    let plain_path = config_dir.join(ENV_JSON);
    let enc_path = config_dir.join(ENV_JSON_ENC);

    let raw = std::fs::read(&plain_path).with_context(|| {
        format!(
            "env.json not found at {} (edit the plaintext first)",
            plain_path.display()
        )
    })?;
    // Validate: must parse as a JSON object. Value types are not constrained.
    let _: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&raw).with_context(|| {
            format!(
                "env.json must be a JSON object: {}",
                plain_path.display()
            )
        })?;

    let key = keychain::load_or_create_key()?;
    let b64 = crypto::encrypt(&raw, &key)?;
    if let Some(parent) = enc_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&enc_path, &b64)
        .with_context(|| format!("failed to write: {}", enc_path.display()))?;
    println!("sealed: {}", enc_path.display());
    Ok(())
}

/// `<config_dir>/env.json.enc` → `<config_dir>/env.json`.
/// Extract the plaintext for editing. Errors if there's no Keychain key.
pub fn unseal(config_dir: &Path) -> Result<()> {
    let enc_path = config_dir.join(ENV_JSON_ENC);
    let plain_path = config_dir.join(ENV_JSON);

    let b64 = std::fs::read_to_string(&enc_path)
        .with_context(|| format!("failed to read: {}", enc_path.display()))?;
    let key = keychain::load_key()?.ok_or_else(|| {
        anyhow!(
            "no key in Keychain (service={}, account={}). \
             env.json.enc from another machine can't be decrypted here",
            keychain::SERVICE,
            keychain::ACCOUNT
        )
    })?;
    let plaintext = crypto::decrypt(b64.trim(), &key)?;
    if let Some(parent) = plain_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&plain_path, &plaintext)
        .with_context(|| format!("failed to write: {}", plain_path.display()))?;
    println!("unsealed: {}", plain_path.display());
    Ok(())
}
