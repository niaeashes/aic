// config — configuration management module (SPEC §4, §11).
//
// Sub-modules:
//   types.rs   — Configuration types and impls (Settings / ModelGroup / ModelRef / UiConfig etc.)
//   loader.rs  — config.yaml loading and the two-layer merge
//   secrets.rs — Secrets decryption and ${VAR} expansion (macOS Keychain + ChaCha20-Poly1305)
//   wizard.rs  — Interactive collection domain for `/config setup`
//
// The "runtime-resolved model" (`ActiveModel`) lives outside config, at `crate::active_model`.

pub mod loader;
pub mod secrets;
pub mod types;
pub mod wizard;

pub use loader::{home_config_path, load};
pub use types::{McpServerCfg, ModelGroup, ModelRef, Settings, UiConfig};
