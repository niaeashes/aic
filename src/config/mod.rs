// config — 設定管理モジュール（SPEC §4, §11）。
//
// サブモジュール構成:
//   types.rs   — 設定型の定義と impl（Settings / ModelGroup / ModelRef / UiConfig 等）
//   loader.rs  — config.yaml のロードと 2 層マージ
//   secrets.rs — secrets の復号と ${VAR} 展開（macOS Keychain + ChaCha20-Poly1305）
//   wizard.rs  — `/config setup` の対話的構築ドメイン
//
// 「ランタイム解決済みのモデル」(`ActiveModel`) は config の外（`crate::active_model`）。

pub mod loader;
pub mod secrets;
pub mod types;
pub mod wizard;

pub use loader::{home_config_path, load};
pub use types::{McpServerCfg, ModelGroup, ModelRef, Settings, UiConfig};
