// crypto — ChaCha20-Poly1305 AEAD (SPEC §5.3).
//
// Format: `base64( nonce(12B) || ciphertext_with_tag )`. 32-byte key, fixed.
// Pure functions only. No dependency on key management (Keychain) or file I/O —
// to swap the cipher algorithm later, **only this file changes**.
//
// Design choices kept in mind for SPEC §5.3:
//   - The nonce is random per call (so "same key, same plaintext" doesn't produce
//     the same ciphertext)
//   - The tag is appended automatically by AEAD. `decrypt` doesn't distinguish
//     key mismatch from tampering — both come back as Err
//   - Base64 is STANDARD (with padding). env.json.enc is meant to be a single-line text file

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
        .map_err(|e| anyhow!("ChaCha20-Poly1305 encryption failed: {e}"))?;
    let mut blob = Vec::with_capacity(12 + ciphertext.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(B64.encode(&blob))
}

pub(super) fn decrypt(b64: &str, key: &[u8; 32]) -> Result<Vec<u8>> {
    let blob = B64.decode(b64).context("base64 decode failed")?;
    // We need at least 12B nonce + 16B AEAD tag.
    if blob.len() < 12 + 16 {
        bail!("ciphertext too short: {} bytes (less than nonce + tag)", blob.len());
    }
    let (nonce_bytes, ciphertext) = blob.split_at(12);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|e| anyhow!("ChaCha20-Poly1305 decryption failed (key mismatch or tampering): {e}"))?;
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    // Used by decrypt_rejects_truncated_blob to base64-encode an undersized blob.
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
        // Less than 12B nonce + 16B tag → error.
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
        // Same key + same plaintext should still yield different ciphertexts because
        // the nonce is fresh each call.
        let key = [9u8; 32];
        let a = encrypt(b"same", &key).unwrap();
        let b = encrypt(b"same", &key).unwrap();
        assert_ne!(a, b);
    }
}
