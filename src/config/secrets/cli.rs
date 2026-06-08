// cli — `aic env seal` / `aic env unseal` 本体。
//
// `main.rs` から `config::secrets::seal` / `unseal` で呼ばれる（mod.rs の `pub use` で
// パスを再公開しているため、呼び出し側は分割前と完全に同じ import で済む）。
//
// 役割: ファイル読み込み → 鍵取得 → 暗号/復号 → ファイル書き出し のフロー組み立て。
// 暗号ロジックは `crypto`、鍵管理は `keychain` に委ねる — cfg gate は持たない。
// 非 macOS では `keychain::load_or_create_key` / `load_key` が即 bail するため、
// ユーザには分かりやすいエラーが伝わる。

use std::path::Path;

use anyhow::{anyhow, Context, Result};

use super::{crypto, keychain, ENV_JSON, ENV_JSON_ENC};

/// `<config_dir>/env.json` → `<config_dir>/env.json.enc`。
/// Keychain に鍵がなければ新規生成して保存（DoD: 既存鍵があれば再利用）。
pub fn seal(config_dir: &Path) -> Result<()> {
    let plain_path = config_dir.join(ENV_JSON);
    let enc_path = config_dir.join(ENV_JSON_ENC);

    let raw = std::fs::read(&plain_path).with_context(|| {
        format!(
            "env.json が見つからない: {}（先に平文を編集してください）",
            plain_path.display()
        )
    })?;
    // 妥当性: JSON object として読めることを確認。値型は問わない。
    let _: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&raw).with_context(|| {
            format!(
                "env.json は JSON オブジェクトである必要がある: {}",
                plain_path.display()
            )
        })?;

    let key = keychain::load_or_create_key()?;
    let b64 = crypto::encrypt(&raw, &key)?;
    if let Some(parent) = enc_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&enc_path, &b64)
        .with_context(|| format!("書き出し失敗: {}", enc_path.display()))?;
    println!("sealed: {}", enc_path.display());
    Ok(())
}

/// `<config_dir>/env.json.enc` → `<config_dir>/env.json`。
/// 編集用に平文を取り出す。Keychain 鍵が無ければエラー。
pub fn unseal(config_dir: &Path) -> Result<()> {
    let enc_path = config_dir.join(ENV_JSON_ENC);
    let plain_path = config_dir.join(ENV_JSON);

    let b64 = std::fs::read_to_string(&enc_path)
        .with_context(|| format!("読み込み失敗: {}", enc_path.display()))?;
    let key = keychain::load_key()?.ok_or_else(|| {
        anyhow!(
            "Keychain に鍵が無い (service={}, account={})。\
             別マシンの env.json.enc は復号できません",
            keychain::SERVICE,
            keychain::ACCOUNT
        )
    })?;
    let plaintext = crypto::decrypt(b64.trim(), &key)?;
    if let Some(parent) = plain_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&plain_path, &plaintext)
        .with_context(|| format!("書き出し失敗: {}", plain_path.display()))?;
    println!("unsealed: {}", plain_path.display());
    Ok(())
}
