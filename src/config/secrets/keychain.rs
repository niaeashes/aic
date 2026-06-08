// keychain — encryption key management via the system keyring (SPEC §5.4).
//
// This file owns the only cfg gate in the module:
//   - macOS  : Keychain via the `keyring` crate (apple-native backend)
//   - Linux  : Secret Service via D-Bus (sync-secret-service backend)
//   - other  : same function signatures, bail at runtime
//
// The whole point of this layout is to keep `storage.rs` and `cli.rs` free of
// cfg branches. The user experience on unsupported platforms is "the build
// works, but the moment you touch env.json.enc, you get a clear error."
//
// macOS and Linux share the same implementation — the `keyring` crate exposes
// an identical sync API across backends (`Entry::new` / `get_password` /
// `set_password` / `Error::NoEntry`).
//
// The key is 32 bytes (ChaCha20-Poly1305's requirement). In the keyring it
// lives as a base64-encoded string.

use anyhow::Result;

pub(super) const SERVICE: &str = "aic";
pub(super) const ACCOUNT: &str = "env-key";

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn load_key() -> Result<Option<[u8; 32]>> {
    use anyhow::{bail, Context};
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let entry = keyring::Entry::new(SERVICE, ACCOUNT)
        .context("failed to initialize the keyring entry")?;
    match entry.get_password() {
        Ok(b64) => {
            let bytes = B64
                .decode(b64.trim())
                .context("the key stored in the keyring is not valid base64")?;
            if bytes.len() != 32 {
                bail!("keyring key has wrong length: expected 32 bytes, got {}", bytes.len());
            }
            let mut k = [0u8; 32];
            k.copy_from_slice(&bytes);
            Ok(Some(k))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(explain_keyring_error(e)),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn load_key() -> Result<Option<[u8; 32]>> {
    anyhow::bail!(
        "system keyring not supported on this platform. \
         Supported: macOS (Keychain), Linux (Secret Service)."
    )
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
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
        .context("failed to initialize the keyring entry")?;
    entry
        .set_password(&B64.encode(k))
        .map_err(explain_keyring_error)?;
    Ok(k)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn load_or_create_key() -> Result<[u8; 32]> {
    anyhow::bail!(
        "`aic env seal` requires a system keyring. \
         Supported: macOS (Keychain), Linux (Secret Service)."
    )
}

/// Probe the system keyring without creating or modifying any entry.
///
/// Used by `/doctor` to report environment readiness.
///   - `Ok(true)`  : the backend is reachable AND a key is stored
///   - `Ok(false)` : the backend is reachable but no key is stored yet
///   - `Err(_)`    : the backend itself is unreachable (likely missing daemon)
#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn probe() -> Result<bool> {
    Ok(load_key()?.is_some())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn probe() -> Result<bool> {
    anyhow::bail!("no system keyring available on this platform")
}

// ---------------------------------------------------------------------------
// Error explanation — turn opaque keyring errors into "next step" guidance
// ---------------------------------------------------------------------------

/// Wrap a keyring error with platform-specific setup hints.
#[cfg(target_os = "linux")]
fn explain_keyring_error(e: keyring::Error) -> anyhow::Error {
    anyhow::anyhow!(
        "keyring access failed: {e}\n\
         → A Secret Service provider must be running on D-Bus.\n\
         → Quick check: `busctl --user list | grep secret`\n\
         → On sway/Hyprland/i3: install `gnome-keyring`, then either enable PAM\n\
           integration or add `exec gnome-keyring-daemon --start --components=secrets`\n\
           to your session config.\n\
         → On GNOME/KDE: usually starts automatically with the session."
    )
}

#[cfg(target_os = "macos")]
fn explain_keyring_error(e: keyring::Error) -> anyhow::Error {
    // macOS users very rarely hit this path; the system Keychain is always available.
    // Most likely cause is the user denying access in the security prompt.
    anyhow::anyhow!(
        "Keychain access failed: {e}\n\
         → If a permission prompt appeared, choose Allow and retry."
    )
}
