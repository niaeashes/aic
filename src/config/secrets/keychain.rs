// keychain — encryption key management via macOS Keychain (SPEC §5.4).
//
// This file owns the **only** cfg gate in the module:
//   - macOS: fetch / create a base64-encoded key at `service=aic, account=env-key`
//     via the keyring crate.
//   - non-macOS: same function signatures, but bail at runtime.
//
// The whole point of this layout is to keep `storage.rs` and `cli.rs` free of
// cfg branches. The user experience on non-macOS is "the build works, but the
// moment you touch env.json.enc, you get a clear error."
//
// The key is 32 bytes (ChaCha20-Poly1305's requirement). In Keychain it lives
// as a base64-encoded string.

use anyhow::Result;

pub(super) const SERVICE: &str = "aic";
pub(super) const ACCOUNT: &str = "env-key";

#[cfg(target_os = "macos")]
pub(super) fn load_key() -> Result<Option<[u8; 32]>> {
    use anyhow::{anyhow, bail, Context};
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let entry = keyring::Entry::new(SERVICE, ACCOUNT)
        .context("failed to initialize the Keychain entry")?;
    match entry.get_password() {
        Ok(b64) => {
            let bytes = B64
                .decode(b64.trim())
                .context("the key stored in Keychain is not valid base64")?;
            if bytes.len() != 32 {
                bail!("Keychain key has wrong length: expected 32 bytes, got {}", bytes.len());
            }
            let mut k = [0u8; 32];
            k.copy_from_slice(&bytes);
            Ok(Some(k))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!("Keychain fetch failed: {e}")),
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn load_key() -> Result<Option<[u8; 32]>> {
    anyhow::bail!(
        "`env.json.enc` encryption/decryption requires macOS Keychain (not supported on this platform)"
    )
}

#[cfg(target_os = "macos")]
pub(super) fn load_or_create_key() -> Result<[u8; 32]> {
    use anyhow::Context;
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    use rand::RngCore;

    if let Some(k) = load_key()? {
        return Ok(k);
    }
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    let entry = keyring::Entry::new(SERVICE, ACCOUNT)
        .context("failed to initialize the Keychain entry")?;
    entry
        .set_password(&B64.encode(k))
        .context("failed to save the key to Keychain")?;
    Ok(k)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn load_or_create_key() -> Result<[u8; 32]> {
    anyhow::bail!("`aic env seal` requires macOS Keychain (not supported on this platform)")
}
