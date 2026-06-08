// crypto — ChaCha20-Poly1305 AEAD（SPEC §5.3）。
//
// 形式: `base64( nonce(12B) || ciphertext_with_tag )`。鍵長は 32 byte 固定。
// 純関数のみ。鍵管理（Keychain）やファイル I/O には依存しない — 暗号アルゴリズムを
// 差し替えたくなったら **このファイルだけ** を触れば済む。
//
// SPEC §5.3 を念頭に置いた設計上の選択:
//   - nonce は毎回ランダム（同一鍵で「同じ平文 → 同じ暗号文」になるのを防ぐ）
//   - tag は AEAD で内部的に付随。`decrypt` は鍵不一致 / 改竄を区別せず Err
//   - base64 は STANDARD（パディング有り）。env.json.enc は 1 行のテキストファイル想定

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Key, Nonce,
};
use rand::RngCore;

pub(super) fn encrypt(plaintext: &[u8], key: &[u8; 32]) -> Result<String> {
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

pub(super) fn decrypt(b64: &str, key: &[u8; 32]) -> Result<Vec<u8>> {
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
    // decrypt_rejects_truncated_blob で短すぎる blob を base64 化するのに使う。
    use base64::{engine::general_purpose::STANDARD as B64, Engine};

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
