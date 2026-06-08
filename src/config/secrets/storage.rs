// storage — env.json / env.json.enc のファイル I/O。
//
// 「ファイルから HashMap<String,String> を作る」ロジックだけを置く。
// 暗号は `crypto`、鍵は `keychain` に委ねる。非 macOS では `keychain::load_key()` が
// 即 bail するため、`decrypt_env_file` 側に cfg gate は不要 — 上位で `Err` を握れば
// そのままフォールバック動作になる（SPEC §5.2 末尾）。

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

use super::{crypto, keychain};

pub(super) fn read_env_file(path: &Path) -> Result<HashMap<String, String>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("読み込み失敗: {}", path.display()))?;
    let map: HashMap<String, String> = serde_json::from_str(&raw)
        .with_context(|| format!("JSON パース失敗: {}", path.display()))?;
    Ok(map)
}

pub(super) fn decrypt_env_file(path: &Path) -> Result<HashMap<String, String>> {
    let b64 = std::fs::read_to_string(path)
        .with_context(|| format!("読み込み失敗: {}", path.display()))?;
    let key = keychain::load_key()?.ok_or_else(|| {
        anyhow!(
            "Keychain に鍵 (service={}, account={}) が見つからない。\
             `aic env seal` を実行してください",
            keychain::SERVICE,
            keychain::ACCOUNT
        )
    })?;
    let plaintext = crypto::decrypt(b64.trim(), &key)?;
    let map: HashMap<String, String> =
        serde_json::from_slice(&plaintext).context("復号後の JSON パース失敗")?;
    Ok(map)
}
