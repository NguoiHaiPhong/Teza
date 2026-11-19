//! Cryptographic primitives compatible with Zalo Web/PC protocol contracts.

use aes::{Aes128, Aes256};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use md5::{Digest, Md5};

/// Zalo `params` AES-CBC contract uses a fixed 16-byte zero IV.
const ZERO_IV: [u8; 16] = [0u8; 16];

/// Encode UTF-8 text with AES-128/256-CBC (zero IV, PKCS#7) and return Base64.
pub fn encode_aes(key: &[u8], text: &str) -> anyhow::Result<String> {
    let mut buf = vec![0u8; text.len() + 16];
    buf[..text.len()].copy_from_slice(text.as_bytes());
    let ct = match key.len() {
        16 => {
            let enc = cbc::Encryptor::<Aes128>::new_from_slices(key, &ZERO_IV)?;
            enc.encrypt_padded_mut::<Pkcs7>(&mut buf, text.len())
                .map_err(|_| anyhow::anyhow!("AES-128 padding failed"))?
        }
        32 => {
            let enc = cbc::Encryptor::<Aes256>::new_from_slices(key, &ZERO_IV)?;
            enc.encrypt_padded_mut::<Pkcs7>(&mut buf, text.len())
                .map_err(|_| anyhow::anyhow!("AES-256 padding failed"))?
        }
        len => anyhow::bail!("Invalid AES key length: {len} (expected 16 or 32)"),
    };
    Ok(B64.encode(ct))
}

/// Decode Base64 AES-128/256-CBC (zero IV, PKCS#7) ciphertext into UTF-8.
pub fn decode_aes(key: &[u8], b64: &str) -> anyhow::Result<String> {
    let mut buf = B64.decode(b64.trim())?;
    let pt = match key.len() {
        16 => {
            let dec = cbc::Decryptor::<Aes128>::new_from_slices(key, &ZERO_IV)?;
            dec.decrypt_padded_mut::<Pkcs7>(&mut buf)
                .map_err(|_| anyhow::anyhow!("AES-128 unpad failed"))?
        }
        32 => {
            let dec = cbc::Decryptor::<Aes256>::new_from_slices(key, &ZERO_IV)?;
            dec.decrypt_padded_mut::<Pkcs7>(&mut buf)
                .map_err(|_| anyhow::anyhow!("AES-256 unpad failed"))?
        }
        len => anyhow::bail!("Invalid AES key length: {len} (expected 16 or 32)"),
    };
    Ok(String::from_utf8(pt.to_vec())?)
}

/// Parse `zpw_enk` as raw 16/32 ASCII bytes or Base64-encoded 16/32-byte key.
pub fn parse_secret_key(zpw_enk: &str) -> Vec<u8> {
    let trimmed = zpw_enk.trim();
    if trimmed.len() == 16 || trimmed.len() == 32 {
        return trimmed.as_bytes().to_vec();
    }
    if let Ok(raw) = B64.decode(trimmed) {
        if raw.len() == 16 || raw.len() == 32 {
            return raw;
        }
    }
    trimmed.as_bytes().to_vec()
}

/// Compute lowercase MD5 hex digest (used strictly for protocol `signkey` compatibility).
pub fn md5_hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    let out = h.finalize();
    out.iter().fold(String::with_capacity(32), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// `getSignKey(tag, params)` = `md5("zsecure" + tag + sorted_values_concat)`.
pub fn sign_key(tag: &str, params: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort();
    let mut s = String::from("zsecure");
    s.push_str(tag);
    for k in keys {
        match &params[k] {
            serde_json::Value::String(v) => s.push_str(v),
            v => s.push_str(&v.to_string()),
        }
    }
    md5_hex(&s)
}

/// Constant-time equality check for secret tokens and MACs.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_roundtrip_32_byte_key() {
        let key = b"0123456789abcdef0123456789abcdef";
        let ct = encode_aes(key, r#"{"hello":"litechat"}"#).unwrap();
        assert_eq!(decode_aes(key, &ct).unwrap(), r#"{"hello":"litechat"}"#);
    }

    #[test]
    fn signkey_sorts_keys() {
        let mut m = serde_json::Map::new();
        m.insert("b".into(), serde_json::json!("2"));
        m.insert("a".into(), serde_json::json!("1"));
        assert_eq!(sign_key("10053", &m), md5_hex("zsecure1005312"));
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"secret123", b"secret123"));
        assert!(!constant_time_eq(b"secret123", b"secret124"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }
}
