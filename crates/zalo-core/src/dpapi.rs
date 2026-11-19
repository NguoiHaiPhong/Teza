//! Windows DPAPI (`CryptProtectData` / `CryptUnprotectData`) helper for sealing session secrets at rest.
//!
//! On non-Windows targets, falls back to Base64 encoding (with OS `0o600` file permissions handled by `data_dir`).

use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};

/// Prefix marking DPAPI-sealed payloads on Windows so we never confuse plaintext with sealed blobs.
const DPAPI_PREFIX: &str = "dpapi:v1:";

/// Protect sensitive UTF-8 secret material for the current Windows user.
pub fn protect_secret(plain: &str) -> Result<String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    #[cfg(target_os = "windows")]
    {
        let cipher = win_dpapi_protect(plain.as_bytes())?;
        Ok(format!("{DPAPI_PREFIX}{}", B64.encode(cipher)))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Ok(format!("plain:v1:{}", B64.encode(plain.as_bytes())))
    }
}

/// Unprotect secret material previously sealed by [`protect_secret`].
/// Supports migrating legacy unprefixed plaintext values cleanly when explicitly needed.
pub fn unprotect_secret(stored: &str) -> Result<String> {
    if stored.is_empty() {
        return Ok(String::new());
    }
    if let Some(rest) = stored.strip_prefix(DPAPI_PREFIX) {
        #[cfg(target_os = "windows")]
        {
            let raw = B64.decode(rest.trim())?;
            let plain = win_dpapi_unprotect(&raw)?;
            return Ok(String::from_utf8(plain)?);
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = rest;
            bail!("DPAPI-sealed session cannot be decrypted on non-Windows host");
        }
    }
    if let Some(rest) = stored.strip_prefix("plain:v1:") {
        let raw = B64.decode(rest.trim())?;
        return Ok(String::from_utf8(raw)?);
    }
    bail!("Unsupported secret envelope format")
}

#[cfg(target_os = "windows")]
fn win_dpapi_protect(data: &[u8]) -> Result<Vec<u8>> {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    #[link(name = "crypt32")]
    extern "system" {
        fn CryptProtectData(
            p_data_in: *const DataBlob,
            sz_data_descr: *const u16,
            p_optional_entropy: *const DataBlob,
            pv_reserved: *mut c_void,
            p_prompt_struct: *mut c_void,
            dw_flags: u32,
            p_data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(h_mem: *mut c_void) -> *mut c_void;
    }

    let in_blob = DataBlob {
        cb_data: u32::try_from(data.len())?,
        pb_data: data.as_ptr() as *mut u8,
    };
    let mut out_blob = DataBlob {
        cb_data: 0,
        pb_data: null_mut(),
    };

    // SAFETY: `in_blob` points to valid slice memory for the duration of the synchronous call;
    // Windows allocates `out_blob.pb_data` which we copy and free via `LocalFree`.
    let ok = unsafe {
        CryptProtectData(
            &in_blob,
            null(),
            null(),
            null_mut(),
            null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
    };
    if ok == 0 || out_blob.pb_data.is_null() {
        bail!("CryptProtectData failed");
    }
    let out = unsafe {
        let slice = std::slice::from_raw_parts(out_blob.pb_data, out_blob.cb_data as usize);
        let copied = slice.to_vec();
        LocalFree(out_blob.pb_data as *mut c_void);
        copied
    };
    Ok(out)
}

#[cfg(target_os = "windows")]
fn win_dpapi_unprotect(data: &[u8]) -> Result<Vec<u8>> {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    #[repr(C)]
    struct DataBlob {
        cb_data: u32,
        pb_data: *mut u8,
    }

    const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;

    #[link(name = "crypt32")]
    extern "system" {
        fn CryptUnprotectData(
            p_data_in: *const DataBlob,
            ppsz_data_descr: *mut *mut u16,
            p_optional_entropy: *const DataBlob,
            pv_reserved: *mut c_void,
            p_prompt_struct: *mut c_void,
            dw_flags: u32,
            p_data_out: *mut DataBlob,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(h_mem: *mut c_void) -> *mut c_void;
    }

    let in_blob = DataBlob {
        cb_data: u32::try_from(data.len())?,
        pb_data: data.as_ptr() as *mut u8,
    };
    let mut out_blob = DataBlob {
        cb_data: 0,
        pb_data: null_mut(),
    };

    // SAFETY: Same contract as `win_dpapi_protect`.
    let ok = unsafe {
        CryptUnprotectData(
            &in_blob,
            null_mut(),
            null(),
            null_mut(),
            null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out_blob,
        )
    };
    if ok == 0 || out_blob.pb_data.is_null() {
        bail!("CryptUnprotectData failed");
    }
    let out = unsafe {
        let slice = std::slice::from_raw_parts(out_blob.pb_data, out_blob.cb_data as usize);
        let copied = slice.to_vec();
        LocalFree(out_blob.pb_data as *mut c_void);
        copied
    };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protect_and_unprotect_roundtrip() {
        let secret = "zpw_sek_super_secret_token_value";
        let sealed = protect_secret(secret).unwrap();
        assert_ne!(sealed, secret);
        assert!(!sealed.contains(secret));
        let restored = unprotect_secret(&sealed).unwrap();
        assert_eq!(restored, secret);
    }
}
