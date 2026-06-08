// keychain — macOS Keychain での暗号鍵管理（SPEC §5.4）。
//
// このファイルが cfg gate を **唯一抱える**:
//   - macOS: keyring crate 経由で `service=aic, account=env-key` の base64 鍵を取得 / 生成
//   - 非 macOS: 同じ signature の関数を実行時に `bail!` させる
//
// 上位の `storage.rs` / `cli.rs` から cfg 分岐を消せるのがこの設計の主目的。
// 「非 macOS でビルドは通り、実行して env.json.enc に触ろうとした瞬間に分かりやすく
//  失敗する」がユーザ体験。
//
// 鍵は 32 byte（ChaCha20-Poly1305 の要件）。Keychain には base64 文字列として保存。

use anyhow::Result;

pub(super) const SERVICE: &str = "aic";
pub(super) const ACCOUNT: &str = "env-key";

#[cfg(target_os = "macos")]
pub(super) fn load_key() -> Result<Option<[u8; 32]>> {
    use anyhow::{anyhow, bail, Context};
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

    let entry = keyring::Entry::new(SERVICE, ACCOUNT)
        .context("Keychain エントリの初期化に失敗")?;
    match entry.get_password() {
        Ok(b64) => {
            let bytes = B64
                .decode(b64.trim())
                .context("Keychain 内の鍵が base64 デコードできない")?;
            if bytes.len() != 32 {
                bail!("Keychain の鍵長が不正: 32 byte 期待、{} byte", bytes.len());
            }
            let mut k = [0u8; 32];
            k.copy_from_slice(&bytes);
            Ok(Some(k))
        }
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!("Keychain 取得失敗: {e}")),
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn load_key() -> Result<Option<[u8; 32]>> {
    anyhow::bail!(
        "`env.json.enc` の暗号化／復号は macOS Keychain が必要です（このプラットフォームでは未対応）"
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
        .context("Keychain エントリの初期化に失敗")?;
    entry
        .set_password(&B64.encode(k))
        .context("Keychain への鍵保存に失敗")?;
    Ok(k)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn load_or_create_key() -> Result<[u8; 32]> {
    anyhow::bail!("`aic env seal` は macOS Keychain が必要です（このプラットフォームでは未対応）")
}
