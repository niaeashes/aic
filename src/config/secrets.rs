// secrets — `${VAR}` 解決 + env.json / env.json.enc の取り扱い（SPEC §5）。
//
// 解決順は SPEC §5 の通り「secrets マップ → プロセス環境変数」。
// secrets マップは起動時に以下のフォールバックチェーンで構築する:
//
//   1. (macOS のみ) `<config_dir>/env.json.enc` を Keychain 鍵で復号
//   2. `<config_dir>/env.json`（平文、ローカル編集用）
//   3. 環境変数のみ
//
// 各段で失敗しても警告にとどめて次のフォールバックへ。起動を止めない（SPEC §5.2 末尾）。
//
// 暗号方式: ChaCha20-Poly1305 AEAD（32B 鍵 + 12B nonce）。
// ファイル形式: `base64( nonce(12B) || ciphertext_with_tag )`（SPEC §5.3）。
// 鍵は macOS Keychain（service=`aic`, account=`env-key`）に base64 文字列として保存。

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use rand::RngCore;

const KEYRING_SERVICE: &str = "aic";
const KEYRING_ACCOUNT: &str = "env-key";
const ENV_JSON: &str = "env.json";
const ENV_JSON_ENC: &str = "env.json.enc";

/// `${VAR}` 解決の単一窓口。
///
/// 解決順は SPEC §5 の通り「secrets マップ → プロセス環境変数」。
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    map: HashMap<String, String>,
}

impl Secrets {
    /// 環境変数だけを参照する空 Secrets。テストや非常用フォールバック。
    pub fn from_env_only() -> Self {
        Self::default()
    }

    /// 起動時のロード経路。`config_dir` から env.json(.enc) を解決する。
    ///
    /// どの段階の失敗も `eprintln!` で警告にとどめて次のフォールバックへ。
    /// 戻り値の `Secrets` は最終的に環境変数フォールバックを必ず持つので、
    /// 呼び出し側は失敗を意識する必要が無い（SPEC §5.2 末尾）。
    pub fn load(config_dir: &Path) -> Self {
        let enc_path = config_dir.join(ENV_JSON_ENC);
        let plain_path = config_dir.join(ENV_JSON);

        // macOS では env.json.enc を Keychain 鍵で復号できるか試す。
        // SPEC §5.4: 非 macOS では Keychain を使わない → enc は無視。
        #[cfg(target_os = "macos")]
        {
            if enc_path.exists() {
                match decrypt_env_file(&enc_path) {
                    Ok(map) => return Self { map },
                    Err(e) => {
                        eprintln!(
                            "warning: {} の復号に失敗（{e:#}）。env.json / 環境変数にフォールバックします。",
                            enc_path.display()
                        );
                    }
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            if enc_path.exists() {
                eprintln!(
                    "warning: {} は macOS Keychain を必要とします。env.json / 環境変数にフォールバックします。",
                    enc_path.display()
                );
            }
        }

        if plain_path.exists() {
            match read_env_file(&plain_path) {
                Ok(map) => return Self { map },
                Err(e) => {
                    eprintln!(
                        "warning: {} の読み込みに失敗（{e:#}）。環境変数のみで起動します。",
                        plain_path.display()
                    );
                }
            }
        }
        Self::default()
    }

    /// `${VAR}` 1 個分のキー解決。secrets マップ優先、環境変数フォールバック。
    pub fn get(&self, key: &str) -> Option<String> {
        self.map
            .get(key)
            .cloned()
            .or_else(|| std::env::var(key).ok())
    }

    /// 文字列中の `${VAR}` を全部展開。未解決のものはそのまま残す
    /// （config がプレースホルダのまま動くと API 呼び出し時に分かりやすく死ぬ方が良い）。
    ///
    /// `$$` でリテラル `$` をエスケープする。`$` 単独や `${` の不一致はそのまま残す。
    pub fn expand(&self, input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        let mut chars = input.char_indices().peekable();
        while let Some((_, c)) = chars.next() {
            if c != '$' {
                out.push(c);
                continue;
            }
            match chars.peek().map(|&(_, ch)| ch) {
                Some('$') => {
                    // $$ → $（リテラルエスケープ）
                    chars.next();
                    out.push('$');
                }
                Some('{') => {
                    chars.next();
                    let mut name = String::new();
                    let mut closed = false;
                    while let Some(&(_, ch)) = chars.peek() {
                        chars.next();
                        if ch == '}' {
                            closed = true;
                            break;
                        }
                        name.push(ch);
                    }
                    if !closed {
                        // `${...` で閉じていない → そのまま戻す
                        out.push('$');
                        out.push('{');
                        out.push_str(&name);
                        continue;
                    }
                    match self.get(&name) {
                        Some(v) => out.push_str(&v),
                        None => {
                            // 未解決はプレースホルダのまま残す
                            out.push_str("${");
                            out.push_str(&name);
                            out.push('}');
                        }
                    }
                }
                _ => out.push('$'),
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// env.json / env.json.enc のロード
// ---------------------------------------------------------------------------

fn read_env_file(path: &Path) -> Result<HashMap<String, String>> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("読み込み失敗: {}", path.display()))?;
    let map: HashMap<String, String> = serde_json::from_str(&raw)
        .with_context(|| format!("JSON パース失敗: {}", path.display()))?;
    Ok(map)
}

#[cfg(target_os = "macos")]
fn decrypt_env_file(path: &Path) -> Result<HashMap<String, String>> {
    let b64 = std::fs::read_to_string(path)
        .with_context(|| format!("読み込み失敗: {}", path.display()))?;
    let key = load_key()?.ok_or_else(|| {
        anyhow!(
            "Keychain に鍵 (service={KEYRING_SERVICE}, account={KEYRING_ACCOUNT}) が見つからない。\
             `aic env seal` を実行してください"
        )
    })?;
    let plaintext = decrypt(b64.trim(), &key)?;
    let map: HashMap<String, String> =
        serde_json::from_slice(&plaintext).context("復号後の JSON パース失敗")?;
    Ok(map)
}

// ---------------------------------------------------------------------------
// `aic env seal` / `aic env unseal` の実装
// ---------------------------------------------------------------------------

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

    let key = load_or_create_key()?;
    let b64 = encrypt(&raw, &key)?;
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
    let key = load_key()?.ok_or_else(|| {
        anyhow!(
            "Keychain に鍵が無い (service={KEYRING_SERVICE}, account={KEYRING_ACCOUNT})。\
             別マシンの env.json.enc は復号できません"
        )
    })?;
    let plaintext = decrypt(b64.trim(), &key)?;
    if let Some(parent) = plain_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&plain_path, &plaintext)
        .with_context(|| format!("書き出し失敗: {}", plain_path.display()))?;
    println!("unsealed: {}", plain_path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Keychain 鍵管理（macOS のみ実装、それ以外は明示的エラー）
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn load_key() -> Result<Option<[u8; 32]>> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
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
fn load_key() -> Result<Option<[u8; 32]>> {
    bail!("`env.json.enc` の暗号化／復号は macOS Keychain が必要です（このプラットフォームでは未対応）")
}

#[cfg(target_os = "macos")]
fn load_or_create_key() -> Result<[u8; 32]> {
    if let Some(k) = load_key()? {
        return Ok(k);
    }
    let mut k = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut k);
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .context("Keychain エントリの初期化に失敗")?;
    entry
        .set_password(&B64.encode(k))
        .context("Keychain への鍵保存に失敗")?;
    Ok(k)
}

#[cfg(not(target_os = "macos"))]
fn load_or_create_key() -> Result<[u8; 32]> {
    bail!("`aic env seal` は macOS Keychain が必要です（このプラットフォームでは未対応）")
}

// ---------------------------------------------------------------------------
// ChaCha20-Poly1305 AEAD — SPEC §5.3 の `base64(nonce(12) || ciphertext_with_tag)`
// ---------------------------------------------------------------------------

fn encrypt(plaintext: &[u8], key: &[u8; 32]) -> Result<String> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| anyhow!("ChaCha20-Poly1305 暗号化失敗: {e}"))?;
    let mut blob = Vec::with_capacity(12 + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(B64.encode(&blob))
}

fn decrypt(b64: &str, key: &[u8; 32]) -> Result<Vec<u8>> {
    let blob = B64.decode(b64).context("base64 デコード失敗")?;
    // 12B nonce + 16B AEAD tag が最低限必要
    if blob.len() < 12 + 16 {
        bail!("暗号文が短すぎる: {} byte (nonce + tag 未満)", blob.len());
    }
    let (nonce_bytes, ciphertext) = blob.split_at(12);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|e| anyhow!("ChaCha20-Poly1305 復号失敗（鍵不一致 or 改竄）: {e}"))?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_replaces_known_vars_from_map() {
        let mut s = Secrets::default();
        s.map.insert("FOO".into(), "bar".into());
        assert_eq!(s.expand("Bearer ${FOO}"), "Bearer bar");
    }

    #[test]
    fn expand_keeps_unknown_placeholder() {
        let s = Secrets::default();
        // 環境変数にも無い前提のキー名
        assert_eq!(
            s.expand("x=${__AIC_TEST_DEFINITELY_UNSET}"),
            "x=${__AIC_TEST_DEFINITELY_UNSET}"
        );
    }

    #[test]
    fn expand_handles_double_dollar_escape() {
        let s = Secrets::default();
        assert_eq!(s.expand("price: $$5"), "price: $5");
    }

    #[test]
    fn expand_passes_through_unclosed_brace() {
        let s = Secrets::default();
        assert_eq!(s.expand("${oops"), "${oops");
    }

    #[test]
    fn map_takes_priority_over_env() {
        let mut s = Secrets::default();
        // 環境変数を実際にいじると並行テストで不安定になるため、map のみ検証
        s.map.insert("HOME".into(), "from-map".into());
        assert_eq!(s.expand("${HOME}"), "from-map");
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = [7u8; 32];
        let payload = br#"{"OPENAI_API_KEY":"sk-test","MCP_TOKEN":"tskey-xyz"}"#;
        let blob = encrypt(payload, &key).unwrap();
        let recovered = decrypt(&blob, &key).unwrap();
        assert_eq!(recovered, payload);
    }

    #[test]
    fn decrypt_rejects_truncated_blob() {
        let key = [0u8; 32];
        // 12B nonce + 16B tag に満たない長さ → エラー
        let short = B64.encode([1u8; 10]);
        assert!(decrypt(&short, &key).is_err());
    }

    #[test]
    fn decrypt_rejects_wrong_key() {
        let k1 = [1u8; 32];
        let k2 = [2u8; 32];
        let blob = encrypt(b"hello", &k1).unwrap();
        assert!(decrypt(&blob, &k2).is_err());
    }

    #[test]
    fn encrypt_uses_fresh_nonce_per_call() {
        // 同じ key・同じ平文でも nonce が毎回変わるので暗号文が一致しない。
        let key = [9u8; 32];
        let a = encrypt(b"same", &key).unwrap();
        let b = encrypt(b"same", &key).unwrap();
        assert_ne!(a, b);
    }
}
