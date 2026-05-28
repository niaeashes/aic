// config — 設定管理モジュール（SPEC §4, §11）。
//
// サブモジュール構成:
//   types.rs   — 設定型の定義と impl（Settings / ModelGroup / ModelRef / ActiveModel 等）
//   loader.rs  — config.yaml のロードと 2 層マージ
//   secrets.rs — secrets の復号と ${VAR} 展開（macOS Keychain + ChaCha20-Poly1305）

pub mod loader;
pub mod secrets;
pub mod types;

pub use loader::{home_config_path, load};
pub use types::{ActiveModel, McpServerCfg, ModelGroup, ModelRef, Settings, UiConfig};
