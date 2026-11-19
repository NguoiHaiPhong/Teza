//! Post-QR login handshake (`/api/login/getLoginInfo`, `/api/login/getServerInfo`).

use anyhow::{bail, Result};
use serde_json::{json, Map, Value};

use crate::param_cipher::ParamCipher;
use crate::session::{truncate, urlencode, Session, SessionError, ZPW_TYPE, ZPW_VER};

fn flatten_query(body: &Value) -> String {
    let Some(obj) = body.as_object() else {
        return String::new();
    };
    let mut keys: Vec<&String> = obj.keys().collect();
    keys.sort();
    keys.into_iter()
        .map(|k| {
            let s = match &obj[k] {
                Value::String(st) => st.clone(),
                other => other.to_string(),
            };
            format!("{k}={}", urlencode(&s))
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Call `/api/login/getLoginInfo` using `ParamCipher` (`tag = "getlogininfo"`)
/// and decrypt the response using `cipher.key()`.
pub async fn get_login_info(session: &Session) -> Result<Value> {
    let client_time = chrono::Utc::now().timestamp();
    let mut payload = Map::new();
    payload.insert("imei".into(), json!(session.imei));
    payload.insert("computer_name".into(), json!(session.computer_name));
    payload.insert("language".into(), json!("vi"));
    payload.insert("ts".into(), json!(client_time));

    let cipher = ParamCipher::new(ZPW_TYPE, &session.imei, client_time)?;
    let body = cipher.build_body(ZPW_TYPE, ZPW_VER, &Value::Object(payload), "getlogininfo")?;
    let query = flatten_query(&body);

    let hosts = [session.domains.auth.as_str(), session.domains.wpa_chat.as_str()];
    let mut last_err = anyhow::anyhow!("getLoginInfo thất bại trên tất cả máy chủ");

    for host in hosts {
        if host.is_empty() {
            continue;
        }
        let url = format!("{host}/api/login/getLoginInfo?{query}&nretry=0");
        match session.get_json(&url).await {
            Ok(resp) => {
                let decrypted = session.maybe_decrypt(resp, cipher.encrypt_key.as_bytes());
                let code = decrypted
                    .get("error_code")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                eprintln!("[litechat][login] getLoginInfo {host} -> error_code={code}");
                if code == 0 {
                    return Ok(decrypted);
                }
                if crate::session::is_session_expired_code(code) {
                    return Err(SessionError::Expired { code }.into());
                }
                let msg = decrypted
                    .get("error_message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                last_err = anyhow::anyhow!("getLoginInfo lỗi {code}: {msg}");
            }
            Err(e) => {
                eprintln!("[litechat][login] getLoginInfo {host} -> err={e}");
                last_err = e;
            }
        }
    }
    Err(last_err)
}

/// Complete authentication after QR confirmation or verify an existing session on startup.
pub async fn complete_login(session: &mut Session) -> Result<()> {
    let mut resp = get_login_info(session).await?;
    if let Some(d_str) = resp.get("data").and_then(Value::as_str) {
        if let Ok(parsed) = serde_json::from_str::<Value>(d_str.trim()) {
            resp["data"] = parsed;
        }
    }
    let data = resp
        .get("data")
        .filter(|d| !d.is_null())
        .unwrap_or(&resp);
    session.apply_login_config(data);
    if !session.is_authenticated() {
        bail!(
            "Đăng nhập chưa hoàn tất (thiếu zpw_sek hoặc zpw_enk): {}",
            truncate(&resp.to_string(), 160)
        );
    }
    Ok(())
}
