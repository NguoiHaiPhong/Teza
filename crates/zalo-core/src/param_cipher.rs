//! ParamCipher v2 — per-request parameter encryption using OS CSPRNG (`getrandom`).

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Map, Value};

use crate::crypto;

/// Static 32-byte AES key used to encrypt `zcid`.
pub const PARAM_CIPHER_KEY: &[u8] = b"3FC4F0D2AB50057BCE0D90D9187A22B1";

fn split_even_odd(s: &str) -> (Vec<char>, Vec<char>) {
    let chars: Vec<char> = s.chars().collect();
    let even = chars.iter().step_by(2).copied().collect();
    let odd = chars.iter().skip(1).step_by(2).copied().collect();
    (even, odd)
}

/// Generate `len` characters from `alphabet` using the OS CSPRNG (rejection-sampled to avoid modulo bias).
fn random_from_alphabet(alphabet: &[u8], len: usize) -> Result<String> {
    let mut out = String::with_capacity(len);
    let max_valid = (256 / alphabet.len()) * alphabet.len();
    let mut buf = [0u8; 64];
    while out.len() < len {
        getrandom::fill(&mut buf).context("OS CSPRNG failure")?;
        for &b in &buf {
            if (b as usize) < max_valid {
                out.push(alphabet[(b as usize) % alphabet.len()] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Generate `len` lowercase hex characters using OS CSPRNG.
pub fn random_hex(len: usize) -> Result<String> {
    random_from_alphabet(b"0123456789abcdef", len)
}

/// Generate `len` base62 characters using OS CSPRNG.
pub fn random_base62(len: usize) -> Result<String> {
    random_from_alphabet(
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
        len,
    )
}

fn encode_hex_upper(key: &[u8], text: &str) -> Result<String> {
    let b64 = crypto::encode_aes(key, text)?;
    let raw = B64.decode(b64)?;
    Ok(raw.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02X}");
        acc
    }))
}

pub struct ParamCipher {
    pub zcid: String,
    pub zcid_ext: String,
    pub encrypt_key: String,
    pub enc_ver: &'static str,
}

impl ParamCipher {
    pub fn new(api_type: &str, imei: &str, first_launch_time: i64) -> Result<Self> {
        let plain = format!("{api_type},{imei},{first_launch_time}");
        let zcid = encode_hex_upper(PARAM_CIPHER_KEY, &plain)?;
        let zcid_ext = random_hex(10)?;

        let md5_upper = crypto::md5_hex(&zcid_ext).to_uppercase();
        let (n_even, _) = split_even_odd(&md5_upper);
        let (a_even, mut a_odd) = split_even_odd(&zcid);
        a_odd.reverse();

        let key = format!(
            "{}{}{}",
            n_even.iter().take(8).collect::<String>(),
            a_even.iter().take(12).collect::<String>(),
            a_odd.iter().take(12).collect::<String>()
        );

        Ok(Self {
            zcid,
            zcid_ext,
            encrypt_key: key,
            enc_ver: "v2",
        })
    }

    pub fn params(&self) -> Value {
        json!({
            "zcid": self.zcid,
            "zcid_ext": self.zcid_ext,
            "enc_ver": self.enc_ver,
        })
    }

    pub fn encrypt_payload(&self, payload: &Value) -> Result<String> {
        crypto::encode_aes(self.encrypt_key.as_bytes(), &payload.to_string())
    }

    pub fn build_body(
        &self,
        api_type: &str,
        api_version: &str,
        payload: &Value,
        tag: &str,
    ) -> Result<Value> {
        let encrypted = self.encrypt_payload(payload)?;
        let mut m = Map::new();
        for (k, v) in self.params().as_object().cloned().unwrap_or_default() {
            m.insert(k, v);
        }
        m.insert("params".into(), json!(encrypted));
        m.insert("type".into(), json!(api_type));
        m.insert("client_version".into(), json!(api_version));
        let sig = crypto::sign_key(tag, &m);
        m.insert("signkey".into(), json!(sig));
        Ok(Value::Object(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_hex_has_exact_length_and_charset() {
        let s = random_hex(16).unwrap();
        assert_eq!(s.len(), 16);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn param_cipher_encrypts_and_signs() {
        let pc = ParamCipher::new("30", "test-imei-001", 1_700_000_000).unwrap();
        let body = pc
            .build_body("30", "694", &json!({"foo": "bar"}), "getlogininfo")
            .unwrap();
        assert!(body.get("signkey").and_then(Value::as_str).is_some());
        assert!(body.get("params").and_then(Value::as_str).is_some());
    }
}
