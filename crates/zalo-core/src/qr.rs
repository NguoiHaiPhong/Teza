//! QR login flow (`reqqr` -> SVG render -> `waiting` -> `pc-status`).

use anyhow::{bail, Result};
use qrcode::render::svg;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::time::Duration;

use crate::session::{signed_query, truncate, Session, ZPW_TYPE, ZPW_VER};

const TAG: &str = "reqqr";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrTicket {
    pub token_id: String,
    pub qr_payload: String,
    pub qr_svg: String,
    pub chk_wait_scan: String,
    pub chk_wait_cfirm: String,
    pub qr_timeout: u64,
    pub qr_retry: u32,
}

impl QrTicket {
    pub fn from_data(d: &Value) -> Result<Self> {
        let s = |k: &str| d.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let qr_payload = s("qr_cnt");
        let chk_wait_scan = s("chk_wait_scan");
        if chk_wait_scan.is_empty() || qr_payload.is_empty() {
            bail!("reqqr: phản hồi thiếu chk_wait_scan hoặc qr_cnt");
        }
        let code = qrcode::QrCode::new(qr_payload.as_bytes())?;
        let qr_svg = code
            .render::<svg::Color<'_>>()
            .min_dimensions(220, 220)
            .dark_color(svg::Color("#111827"))
            .light_color(svg::Color("#ffffff"))
            .build();
        Ok(Self {
            token_id: s("token_id"),
            qr_payload,
            qr_svg,
            chk_wait_scan,
            chk_wait_cfirm: s("chk_wait_cfirm"),
            qr_timeout: d.get("qr_timeout").and_then(Value::as_u64).unwrap_or(180),
            qr_retry: d.get("qr_retry").and_then(Value::as_u64).unwrap_or(3) as u32,
        })
    }
}

pub async fn request_qr(session: &Session, language: &str) -> Result<QrTicket> {
    let mut m = Map::new();
    m.insert("language".into(), json!(language));
    m.insert("client_time".into(), json!(chrono::Utc::now().timestamp()));
    m.insert("imei".into(), json!(session.imei));
    m.insert("computer_name".into(), json!(session.computer_name));
    m.insert("logged_uids".into(), json!("[]"));
    m.insert("self_g".into(), json!(1));
    m.insert("qr_form".into(), json!(0));
    m.insert("type".into(), json!(ZPW_TYPE));
    m.insert("client_version".into(), json!(ZPW_VER));

    let url = format!("{}/api/login/reqqr?{}", session.domains.auth, signed_query(&m, TAG));
    let (status, text, _) = get_raw(session, &url).await?;
    if status != 200 {
        bail!("reqqr: HTTP {status}: {}", truncate(&text, 200));
    }
    let v: Value = serde_json::from_str(&text)?;
    let code = v.get("error_code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let msg = v.get("error_message").and_then(Value::as_str).unwrap_or("");
        bail!("reqqr: server error {code}: {msg}");
    }
    let data = v
        .get("data")
        .ok_or_else(|| anyhow::anyhow!("reqqr: thiếu data"))?;
    QrTicket::from_data(data)
}

pub async fn wait_for_scan(session: &Session, ticket: &QrTicket) -> Result<Option<String>> {
    let url = format!("{}&lang=vi", ticket.chk_wait_scan);
    let deadline = std::time::Instant::now() + Duration::from_secs(ticket.qr_timeout.max(30));
    let mut attempts: u32 = 0;

    loop {
        if std::time::Instant::now() >= deadline {
            bail!("Mã QR đã hết hạn");
        }
        let (status, text, _) = get_raw(session, &url).await?;
        match status {
            408 => tokio::time::sleep(Duration::from_millis(800)).await,
            200 => {
                let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                let code = v.get("error_code").and_then(Value::as_i64).unwrap_or(0);
                if code == 0 {
                    if let Some(data) = v.get("data").filter(|d| !d.is_null()) {
                        let name = data
                            .get("display_name")
                            .or_else(|| data.get("displayName"))
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        return Ok(name);
                    }
                } else {
                    attempts += 1;
                    if attempts > ticket.qr_retry.max(3) {
                        let msg = v.get("error_message").and_then(Value::as_str).unwrap_or("");
                        bail!("waiting scan: lỗi {code}: {msg}");
                    }
                }
                tokio::time::sleep(Duration::from_millis(800)).await;
            }
            other => bail!("waiting scan: HTTP {other}: {}", truncate(&text, 160)),
        }
    }
}

pub async fn wait_for_confirm(session: &mut Session, ticket: &QrTicket) -> Result<()> {
    if ticket.chk_wait_cfirm.is_empty() {
        bail!("reqqr: thiếu chk_wait_cfirm");
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(ticket.qr_timeout.max(60));
    loop {
        if std::time::Instant::now() >= deadline {
            bail!("Hết thời gian chờ xác nhận trên điện thoại");
        }
        let (status, text, cookie_sek) = get_raw(session, &ticket.chk_wait_cfirm).await?;
        if status == 408 {
            tokio::time::sleep(Duration::from_millis(800)).await;
            continue;
        }
        if status != 200 {
            bail!("pc-status: HTTP {status}: {}", truncate(&text, 160));
        }
        let v: Value = serde_json::from_str(&text)?;
        let code = v.get("error_code").and_then(Value::as_i64).unwrap_or(0);
        if code == 0 {
            let json_sek = extract_zpw_sek(&v);
            let chosen_sek = cookie_sek
                .clone()
                .or(json_sek.clone())
                .or_else(|| session.effective_zpw_sek());
            eprintln!(
                "[litechat][qr] pc-status ok: cookie_sek={}, json_sek={}, effective_sek={}",
                cookie_sek.is_some(),
                json_sek.is_some(),
                chosen_sek.is_some()
            );
            if let Some(sek) = chosen_sek {
                session.set_zpw_sek(sek);
            }
            if let Some(data) = v.get("data") {
                session.apply_login_config(data);
            }
            crate::login::complete_login(session).await?;
            return Ok(());
        } else if code == 18012 {
            tokio::time::sleep(Duration::from_millis(800)).await;
        } else {
            let msg = v.get("error_message").and_then(Value::as_str).unwrap_or("");
            bail!("pc-status: lỗi {code}: {msg}");
        }
    }
}

fn extract_zpw_sek(v: &Value) -> Option<String> {
    for key in ["zpw_sek", "zpw_kt", "zpw_wpsk", "token", "session_key"] {
        for container in [
            Some(v),
            v.get("data"),
            v.get("data").and_then(|d| d.get("account")),
        ] {
            if let Some(s) = container.and_then(|c| c.get(key)).and_then(Value::as_str) {
                if !s.is_empty() && s != "EXPIRED" {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

async fn get_raw(session: &Session, url: &str) -> Result<(u16, String, Option<String>)> {
    if !crate::session::is_allowed_zalo_http_url(url) {
        bail!("Từ chối kết nối tới host QR không hợp lệ");
    }
    let resp = session.http.get(url).send().await?;
    let status = resp.status().as_u16();
    let mut sek = None;
    for val in resp.headers().get_all("set-cookie") {
        if let Ok(sc) = val.to_str() {
            if let Some(pos) = sc.find("zpw_sek=") {
                let token = sc[pos + 8..].split(';').next().unwrap_or("").trim();
                if !token.is_empty() && token != "EXPIRED" {
                    sek = Some(token.to_string());
                }
            }
        }
    }
    let text = resp.text().await?;
    Ok((status, text, sek))
}
