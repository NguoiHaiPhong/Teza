//! Intranet relay (SOCKS5h / HTTP CONNECT) configuration, DPAPI persistence, and tunnel transport.

use std::net::IpAddr;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use reqwest::{Client, ClientBuilder, Proxy, Url};
use rustls::pki_types::ServerName;
use rustls::RootCertStore;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

use crate::{data_dir, dpapi};

const DEFAULT_RELAY_PORT: u16 = 1088;
const RELAY_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RelayMode {
    #[default]
    Socks5,
    Http,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RelayConfig {
    pub enabled: bool,
    pub mode: RelayMode,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub route_webview_media: bool,
}

impl std::fmt::Debug for RelayConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayConfig")
            .field("enabled", &self.enabled)
            .field("mode", &self.mode)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field(
                "password",
                &if self.password.is_empty() {
                    ""
                } else {
                    "[REDACTED]"
                },
            )
            .field("route_webview_media", &self.route_webview_media)
            .finish()
    }
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: RelayMode::Socks5,
            host: String::new(),
            port: DEFAULT_RELAY_PORT,
            username: String::new(),
            password: String::new(),
            route_webview_media: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RelayConfigSummary {
    pub enabled: bool,
    pub mode: RelayMode,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub has_password: bool,
    pub password_preview: String,
    pub route_webview_media: bool,
}

#[derive(Clone, Deserialize)]
pub struct RelayConfigUpdate {
    pub enabled: bool,
    pub mode: RelayMode,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub clear_password: bool,
    #[serde(default = "default_true")]
    pub route_webview_media: bool,
}

impl std::fmt::Debug for RelayConfigUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayConfigUpdate")
            .field("enabled", &self.enabled)
            .field("mode", &self.mode)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field(
                "password",
                &self.password.as_deref().map(|p| {
                    if p.is_empty() {
                        ""
                    } else {
                        "[REDACTED]"
                    }
                }),
            )
            .field("clear_password", &self.clear_password)
            .field("route_webview_media", &self.route_webview_media)
            .finish()
    }
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
struct StoredRelayConfig {
    enabled: bool,
    mode: RelayMode,
    host: String,
    port: u16,
    username: String,
    sealed_password: String,
    #[serde(default = "default_true")]
    route_webview_media: bool,
}

fn mask_secret(s: &str) -> String {
    if s.trim().is_empty() {
        String::new()
    } else {
        "••••••••".to_string()
    }
}

fn seal_opt(raw: &str) -> Result<Option<String>> {
    if raw.is_empty() {
        Ok(None)
    } else {
        dpapi::protect_secret(raw)
            .context("Không thể mã hóa mật khẩu Relay bằng DPAPI")
            .map(Some)
    }
}

fn unseal_opt(sealed: &str) -> String {
    if sealed.trim().is_empty() {
        String::new()
    } else {
        dpapi::unprotect_secret(sealed).unwrap_or_default()
    }
}

fn validate_relay_host(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("Vui lòng nhập địa chỉ IP hoặc tên miền máy chủ Relay");
    }
    if trimmed.len() > 253 {
        bail!("Địa chỉ máy chủ Relay quá dài");
    }
    if trimmed
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '@' | '/' | '\\' | '?' | '#'))
    {
        bail!("Địa chỉ máy chủ Relay không hợp lệ (chỉ nhập IP hoặc hostname, không kèm http:// hoặc /)");
    }
    if trimmed.contains(':') && trimmed.parse::<IpAddr>().is_err() {
        bail!("Địa chỉ máy chủ Relay không kèm cổng (:port); hãy nhập cổng vào ô Cổng riêng");
    }
    Ok(trimmed.to_string())
}

impl RelayConfig {
    pub fn load_from_file(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(stored) = serde_json::from_str::<StoredRelayConfig>(&raw) else {
            return Self::default();
        };
        Self {
            enabled: stored.enabled,
            mode: stored.mode,
            host: stored.host,
            port: if stored.port == 0 {
                DEFAULT_RELAY_PORT
            } else {
                stored.port
            },
            username: stored.username,
            password: unseal_opt(&stored.sealed_password),
            route_webview_media: stored.route_webview_media,
        }
    }

    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        if self.enabled {
            self.validate()?;
        }
        let sealed_password = seal_opt(&self.password)?.unwrap_or_default();
        let stored = StoredRelayConfig {
            enabled: self.enabled,
            mode: self.mode,
            host: self.host.trim().to_string(),
            port: if self.port == 0 {
                DEFAULT_RELAY_PORT
            } else {
                self.port
            },
            username: self.username.trim().to_string(),
            sealed_password,
            route_webview_media: self.route_webview_media,
        };
        let json = serde_json::to_string_pretty(&stored)?;
        data_dir::write_private_atomic(path, json.as_bytes())
    }

    pub fn summary(&self) -> RelayConfigSummary {
        RelayConfigSummary {
            enabled: self.enabled,
            mode: self.mode,
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            has_password: !self.password.is_empty(),
            password_preview: mask_secret(&self.password),
            route_webview_media: self.route_webview_media,
        }
    }

    pub fn apply_update(&mut self, upd: RelayConfigUpdate) -> Result<()> {
        let clean_host = if upd.enabled || !upd.host.trim().is_empty() {
            validate_relay_host(&upd.host)?
        } else {
            String::new()
        };
        if upd.enabled && upd.port == 0 {
            bail!("Cổng Relay phải nằm trong khoảng 1..=65535");
        }
        let user = upd.username.trim().to_string();
        if user.len() > 255 {
            bail!("Tài khoản Relay tối đa 255 ký tự");
        }

        self.enabled = upd.enabled;
        self.mode = upd.mode;
        self.host = clean_host;
        self.port = if upd.port == 0 {
            DEFAULT_RELAY_PORT
        } else {
            upd.port
        };
        self.username = user;
        self.route_webview_media = upd.route_webview_media;

        if upd.clear_password {
            self.password.clear();
        } else if let Some(pw) = upd.password {
            if !pw.is_empty() {
                if pw.len() > 255 {
                    bail!("Mật khẩu Relay tối đa 255 ký tự");
                }
                self.password = pw;
            }
        }
        Ok(())
    }

    pub fn with_update(&self, upd: RelayConfigUpdate) -> Result<Self> {
        let mut next = self.clone();
        next.apply_update(upd)?;
        Ok(next)
    }

    pub fn validate(&self) -> Result<()> {
        let _ = validate_relay_host(&self.host)?;
        if self.port == 0 {
            bail!("Cổng Relay phải nằm trong khoảng 1..=65535");
        }
        if self.username.trim().len() > 255 || self.password.len() > 255 {
            bail!("Tài khoản và mật khẩu Relay tối đa 255 ký tự");
        }
        Ok(())
    }

    pub fn proxy_url_string(&self) -> Result<Option<String>> {
        if !self.enabled {
            return Ok(None);
        }
        self.validate()?;
        let host = self.host.trim();
        let host_part = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        let scheme = match self.mode {
            RelayMode::Socks5 => "socks5h",
            RelayMode::Http => "http",
        };
        let mut url = Url::parse(&format!("{scheme}://{host_part}:{}", self.port))
            .context("Không tạo được URL cấu hình Relay")?;
        if !self.username.trim().is_empty() || !self.password.is_empty() {
            url.set_username(self.username.trim())
                .map_err(|_| anyhow::anyhow!("Tài khoản Relay không hợp lệ"))?;
            if !self.password.is_empty() {
                url.set_password(Some(&self.password))
                    .map_err(|_| anyhow::anyhow!("Mật khẩu Relay không hợp lệ"))?;
            }
        }
        Ok(Some(url.into()))
    }

    pub fn apply_to_client_builder(&self, builder: ClientBuilder) -> Result<ClientBuilder> {
        if let Some(proxy_url) = self.proxy_url_string()? {
            let proxy = Proxy::all(&proxy_url)
                .context("Không áp dụng được cấu hình Relay HTTP")?
                .no_proxy(None);
            Ok(builder.no_proxy().proxy(proxy))
        } else {
            Ok(builder)
        }
    }

    pub async fn connect_tcp(&self, target_host: &str, target_port: u16) -> Result<TcpStream> {
        if !self.enabled {
            let stream = tokio::time::timeout(
                RELAY_HANDSHAKE_TIMEOUT,
                TcpStream::connect((target_host, target_port)),
            )
            .await
            .context("Hết thời gian kết nối TCP trực tiếp")??;
            let _ = stream.set_nodelay(true);
            return Ok(stream);
        }

        self.validate()?;
        let mut stream = tokio::time::timeout(
            RELAY_HANDSHAKE_TIMEOUT,
            TcpStream::connect((self.host.trim(), self.port)),
        )
        .await
        .with_context(|| format!("Hết thời gian kết nối tới máy chủ Relay {}:{}", self.host.trim(), self.port))?
        .with_context(|| format!("Không kết nối được tới máy chủ Relay {}:{}", self.host.trim(), self.port))?;
        let _ = stream.set_nodelay(true);

        match self.mode {
            RelayMode::Socks5 => {
                socks5_connect(
                    &mut stream,
                    target_host,
                    target_port,
                    self.username.trim(),
                    &self.password,
                )
                .await?;
            }
            RelayMode::Http => {
                http_connect(
                    &mut stream,
                    target_host,
                    target_port,
                    self.username.trim(),
                    &self.password,
                )
                .await?;
            }
        }
        Ok(stream)
    }

    pub async fn connect_tls(
        &self,
        target_host: &str,
        target_port: u16,
    ) -> Result<TlsStream<TcpStream>> {
        let tcp = self.connect_tcp(target_host, target_port).await?;
        let tls_cfg = shared_tls_config()?;
        let connector = TlsConnector::from(tls_cfg);
        let server_name = ServerName::try_from(target_host.to_string())
            .map_err(|_| anyhow::anyhow!("Tên miền TLS không hợp lệ: {target_host}"))?;
        let stream = tokio::time::timeout(
            RELAY_HANDSHAKE_TIMEOUT,
            connector.connect(server_name, tcp),
        )
        .await
        .context("Hết thời gian bắt tay TLS qua Relay")?
        .context("Bắt tay TLS qua Relay thất bại")?;
        Ok(stream)
    }

    pub async fn test_connection(&self) -> Result<u64> {
        if !self.enabled {
            bail!("Hãy bật công tắc Sử dụng mạng Relay trước khi kiểm tra");
        }
        self.validate()?;
        let started = Instant::now();
        let builder = Client::builder()
            .user_agent(concat!(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) LiteChat/",
                env!("CARGO_PKG_VERSION"),
                " Safari/537.36"
            ))
            .timeout(Duration::from_secs(12));
        let client = self.apply_to_client_builder(builder)?.build()?;
        let resp = client
            .get("https://wpa.zaloapp.com/")
            .send()
            .await
            .context("Không kết nối được qua máy chủ Relay tới máy chủ chat")?;
        let code = resp.status().as_u16();
        if code >= 500 {
            bail!("Máy chủ đích trả HTTP {code} qua Relay");
        }
        Ok(started.elapsed().as_millis() as u64)
    }
}

fn shared_tls_config() -> Result<Arc<rustls::ClientConfig>> {
    static TLS_CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if let Some(existing) = TLS_CFG.get() {
        return Ok(existing.clone());
    }
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("Không cấu hình được phiên bản TLS")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let arc = Arc::new(cfg);
    let _ = TLS_CFG.set(arc.clone());
    Ok(arc)
}

async fn socks5_connect(
    stream: &mut TcpStream,
    target_host: &str,
    target_port: u16,
    username: &str,
    password: &str,
) -> Result<()> {
    let host_bytes = target_host.as_bytes();
    if host_bytes.is_empty() || host_bytes.len() > 255 {
        bail!("Tên miền đích SOCKS5 không hợp lệ");
    }
    let has_creds = !username.is_empty() || !password.is_empty();

    tokio::time::timeout(RELAY_HANDSHAKE_TIMEOUT, async {
        if has_creds {
            stream.write_all(&[0x05, 0x02, 0x00, 0x02]).await?;
        } else {
            stream.write_all(&[0x05, 0x01, 0x00]).await?;
        }
        let mut choice = [0u8; 2];
        stream.read_exact(&mut choice).await?;
        if choice[0] != 0x05 {
            bail!("Phản hồi SOCKS5 không hợp lệ từ máy chủ Relay");
        }
        match choice[1] {
            0x00 => {}
            0x02 => {
                if !has_creds {
                    bail!("Máy chủ Relay yêu cầu tài khoản và mật khẩu");
                }
                let u = username.as_bytes();
                let p = password.as_bytes();
                let mut auth_pkt = Vec::with_capacity(3 + u.len() + p.len());
                auth_pkt.push(0x01);
                auth_pkt.push(u.len() as u8);
                auth_pkt.extend_from_slice(u);
                auth_pkt.push(p.len() as u8);
                auth_pkt.extend_from_slice(p);
                stream.write_all(&auth_pkt).await?;

                let mut auth_rep = [0u8; 2];
                stream.read_exact(&mut auth_rep).await?;
                if auth_rep[1] != 0x00 {
                    bail!("Sai tài khoản hoặc mật khẩu Relay (SOCKS5)");
                }
            }
            0xFF => bail!("Máy chủ Relay từ chối phương thức xác thực SOCKS5"),
            other => bail!("Phương thức xác thực SOCKS5 không hỗ trợ ({other})"),
        }

        let mut req = Vec::with_capacity(7 + host_bytes.len());
        req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8]);
        req.extend_from_slice(host_bytes);
        req.extend_from_slice(&target_port.to_be_bytes());
        stream.write_all(&req).await?;

        let mut rep = [0u8; 4];
        stream.read_exact(&mut rep).await?;
        if rep[0] != 0x05 || rep[1] != 0x00 {
            bail!(
                "Relay SOCKS5 không kết nối được tới {target_host}:{target_port} (mã {})",
                rep[1]
            );
        }
        match rep[3] {
            0x01 => {
                let mut skip = [0u8; 6];
                stream.read_exact(&mut skip).await?;
            }
            0x03 => {
                let len = stream.read_u8().await? as usize;
                let mut skip = vec![0u8; len + 2];
                stream.read_exact(&mut skip).await?;
            }
            0x04 => {
                let mut skip = [0u8; 18];
                stream.read_exact(&mut skip).await?;
            }
            other => bail!("Kiểu địa chỉ phản hồi SOCKS5 không hợp lệ ({other})"),
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Hết thời gian bắt tay SOCKS5 với Relay")??;
    Ok(())
}

fn validate_http_connect_host(target_host: &str) -> Result<&str> {
    if target_host.is_empty() || target_host.len() > 255 {
        bail!("Tên miền đích HTTP CONNECT không hợp lệ");
    }
    if target_host.bytes().any(|b| {
        b == b'\r'
            || b == b'\n'
            || b.is_ascii_whitespace()
            || b.is_ascii_control()
            || !b.is_ascii()
    }) || target_host.contains(['/', '?', '#', '@'])
    {
        bail!("Tên miền đích HTTP CONNECT chứa ký tự không hợp lệ");
    }
    Ok(target_host)
}

async fn http_connect(
    stream: &mut TcpStream,
    target_host: &str,
    target_port: u16,
    username: &str,
    password: &str,
) -> Result<()> {
    let target_host = validate_http_connect_host(target_host)?;
    tokio::time::timeout(RELAY_HANDSHAKE_TIMEOUT, async {
        let mut req = format!(
            "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\nProxy-Connection: Keep-Alive\r\n"
        );
        if !username.is_empty() || !password.is_empty() {
            let token = B64.encode(format!("{username}:{password}"));
            req.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await?;

        // Read byte-by-byte until CRLFCRLF so TLS handshake bytes stay intact in kernel stream.
        let mut raw_head = Vec::with_capacity(256);
        let mut byte = [0u8; 1];
        loop {
            if raw_head.len() >= 8192 {
                bail!("Header phản hồi HTTP CONNECT quá dài");
            }
            let n = stream.read(&mut byte).await?;
            if n == 0 {
                bail!("Máy chủ Relay đóng kết nối khi đang bắt tay HTTP CONNECT");
            }
            raw_head.push(byte[0]);
            if raw_head.ends_with(b"\r\n\r\n") {
                break;
            }
        }

        let head_str = String::from_utf8_lossy(&raw_head);
        let status_line = head_str.lines().next().unwrap_or("").trim();
        let status_code = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        if status_code == 407 {
            bail!("Sai tài khoản hoặc mật khẩu Relay (HTTP 407)");
        }
        if status_code != 200 {
            bail!("Relay HTTP CONNECT thất bại: {status_line}");
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("Hết thời gian bắt tay HTTP CONNECT với Relay")??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn dpapi_seals_and_unseals_relay_password() {
        let dir = std::env::temp_dir().join(format!("litechat-relay-test-{}", std::process::id()));
        let paths = data_dir::StoragePaths::from_dir(&dir).unwrap();
        let cfg = RelayConfig {
            enabled: true,
            mode: RelayMode::Socks5,
            host: "10.7.12.233".into(),
            port: 1088,
            username: "litechat".into(),
            password: "super_secret_relay_pw!".into(),
            route_webview_media: true,
        };
        cfg.save_to_file(&paths.relay_file()).unwrap();

        let disk_raw = std::fs::read_to_string(paths.relay_file()).unwrap();
        assert!(!disk_raw.contains("super_secret_relay_pw!"));

        let loaded = RelayConfig::load_from_file(&paths.relay_file());
        assert!(loaded.enabled);
        assert_eq!(loaded.host, "10.7.12.233");
        assert_eq!(loaded.port, 1088);
        assert_eq!(loaded.password, "super_secret_relay_pw!");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validates_host_and_builds_escaped_proxy_url() {
        let mut cfg = RelayConfig {
            enabled: true,
            mode: RelayMode::Socks5,
            host: "10.7.12.233".into(),
            port: 1088,
            username: "u@corp".into(),
            password: "p:w/1".into(),
            route_webview_media: true,
        };
        let url = cfg.proxy_url_string().unwrap().unwrap();
        assert!(url.starts_with("socks5h://u%40corp:p%3Aw%2F1@10.7.12.233:1088"));

        cfg.mode = RelayMode::Http;
        let http_url = cfg.proxy_url_string().unwrap().unwrap();
        assert!(http_url.starts_with("http://u%40corp:p%3Aw%2F1@10.7.12.233:1088"));

        cfg.host = "http://10.7.12.233".into();
        assert!(cfg.proxy_url_string().is_err());

        cfg.host = "10.7.12.233:1088".into();
        assert!(cfg.proxy_url_string().is_err());
    }

    #[tokio::test]
    async fn socks5_and_http_connect_clients_tunnel_bytes() {
        // Mock SOCKS5 server
        let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socks_addr = socks_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = socks_listener.accept().await.unwrap();
            let mut g = [0u8; 4];
            s.read_exact(&mut g).await.unwrap();
            s.write_all(&[0x05, 0x02]).await.unwrap();
            let mut abuf = [0u8; 16];
            let n = s.read(&mut abuf).await.unwrap();
            assert!(n >= 5);
            s.write_all(&[0x01, 0x00]).await.unwrap();
            let mut req = [0u8; 64];
            let _ = s.read(&mut req).await.unwrap();
            s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            let mut payload = [0u8; 5];
            s.read_exact(&mut payload).await.unwrap();
            s.write_all(&payload).await.unwrap();
        });

        let cfg = RelayConfig {
            enabled: true,
            mode: RelayMode::Socks5,
            host: "127.0.0.1".into(),
            port: socks_addr.port(),
            username: "u1".into(),
            password: "p1".into(),
            route_webview_media: true,
        };
        let mut st = cfg.connect_tcp("chat.zalo.me", 443).await.unwrap();
        st.write_all(b"hello").await.unwrap();
        let mut back = [0u8; 5];
        st.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"hello");

        // Mock HTTP CONNECT server
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_addr = http_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = http_listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let n = s.read(&mut buf).await.unwrap();
            let req = std::str::from_utf8(&buf[..n]).unwrap();
            assert!(req.contains("CONNECT chat.zalo.me:443"));
            assert!(req.contains("Proxy-Authorization: Basic "));
            s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let mut payload = [0u8; 5];
            s.read_exact(&mut payload).await.unwrap();
            s.write_all(&payload).await.unwrap();
        });

        let cfg_http = RelayConfig {
            enabled: true,
            mode: RelayMode::Http,
            host: "127.0.0.1".into(),
            port: http_addr.port(),
            username: "u1".into(),
            password: "p1".into(),
            route_webview_media: true,
        };
        let mut hst = cfg_http.connect_tcp("chat.zalo.me", 443).await.unwrap();
        hst.write_all(b"world").await.unwrap();
        let mut back2 = [0u8; 5];
        hst.read_exact(&mut back2).await.unwrap();
        assert_eq!(&back2, b"world");
    }

    #[test]
    fn debug_impls_redact_relay_password_and_seal_opt_returns_result() {
        let cfg = RelayConfig {
            enabled: true,
            mode: RelayMode::Socks5,
            host: "10.7.12.233".into(),
            port: 1088,
            username: "litechat".into(),
            password: "relay_top_secret_pw!".into(),
            route_webview_media: true,
        };
        let cfg_dbg = format!("{cfg:?}");
        assert!(!cfg_dbg.contains("relay_top_secret_pw!"));
        assert!(cfg_dbg.contains("[REDACTED]"));
        assert!(cfg_dbg.contains("10.7.12.233"));

        let upd = RelayConfigUpdate {
            enabled: true,
            mode: RelayMode::Http,
            host: "10.7.12.233".into(),
            port: 1088,
            username: "litechat".into(),
            password: Some("update_top_secret_pw!".into()),
            clear_password: false,
            route_webview_media: true,
        };
        let upd_dbg = format!("{upd:?}");
        assert!(!upd_dbg.contains("update_top_secret_pw!"));
        assert!(upd_dbg.contains("[REDACTED]"));

        assert_eq!(seal_opt("").unwrap(), None);
        let sealed = seal_opt("relay_top_secret_pw!").unwrap().unwrap();
        assert!(!sealed.contains("relay_top_secret_pw!"));
        assert_eq!(unseal_opt(&sealed), "relay_top_secret_pw!");
    }

    #[tokio::test]
    async fn http_connect_rejects_crlf_whitespace_and_control_chars_in_target_host() {
        assert_eq!(
            validate_http_connect_host("chat.zalo.me").unwrap(),
            "chat.zalo.me"
        );
        assert_eq!(
            validate_http_connect_host("127.0.0.1").unwrap(),
            "127.0.0.1"
        );
        assert_eq!(validate_http_connect_host("[::1]").unwrap(), "[::1]");

        for bad in [
            "",
            "chat.zalo.me\r\nX-Injected: 1",
            "chat.zalo.me\nHost: evil.example",
            "chat.zalo.me\rHost: evil.example",
            "chat.zalo.me HTTP/1.1",
            "chat.zalo.me\t443",
            "chat.zalo.me\x00evil",
            "chat.zalo.me\x7f",
            "chat.zalo.me/path",
            "chat.zalo.me?q=1",
            "user@chat.zalo.me",
        ] {
            assert!(
                validate_http_connect_host(bad).is_err(),
                "expected http_connect host rejection for {bad:?}"
            );
        }

        // Verify `http_connect` rejects malicious target_host before writing any bytes on the wire
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            s.read(&mut buf).await.unwrap()
        });

        let mut client = TcpStream::connect(addr).await.unwrap();
        let err = http_connect(&mut client, "chat.zalo.me\r\nInjected: 1", 443, "", "")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("không hợp lệ"));
        drop(client);
        assert_eq!(srv.await.unwrap(), 0);
    }

    #[tokio::test]
    async fn apply_to_client_builder_builds_and_routes_socks5_and_http_modes() {
        // Disabled mode builds cleanly without modifying proxy settings.
        let disabled = RelayConfig::default();
        assert!(disabled
            .apply_to_client_builder(Client::builder())
            .unwrap()
            .build()
            .is_ok());

        // SOCKS5 mode (with auth): client builds cleanly and initiates a SOCKS5 handshake to the configured relay.
        let socks_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socks_addr = socks_listener.local_addr().unwrap();
        let socks_srv = tokio::spawn(async move {
            let (mut s, _) = socks_listener.accept().await.unwrap();
            let mut hello = [0u8; 4];
            let n = s.read(&mut hello).await.unwrap();
            // Reply with auth-not-acceptable so reqwest fails fast after proving it hit our SOCKS5 relay.
            let _ = s.write_all(&[0x05, 0xFF]).await;
            (n, hello[0])
        });

        let socks_cfg = RelayConfig {
            enabled: true,
            mode: RelayMode::Socks5,
            host: "127.0.0.1".into(),
            port: socks_addr.port(),
            username: "relay_u".into(),
            password: "relay_p".into(),
            route_webview_media: true,
        };
        let socks_client = socks_cfg
            .apply_to_client_builder(Client::builder().timeout(Duration::from_secs(3)))
            .unwrap()
            .build()
            .unwrap();
        assert!(socks_client.get("https://file-stal-9.dlfl.vn/test").send().await.is_err());
        let (socks_n, socks_ver) = socks_srv.await.unwrap();
        assert!(socks_n >= 3);
        assert_eq!(socks_ver, 0x05);

        // HTTP mode (with auth): client builds cleanly and sends CONNECT to the configured relay.
        let http_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_addr = http_listener.local_addr().unwrap();
        let http_srv = tokio::spawn(async move {
            let (mut s, _) = http_listener.accept().await.unwrap();
            let mut buf = [0u8; 512];
            let n = s.read(&mut buf).await.unwrap();
            let _ = s
                .write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n")
                .await;
            String::from_utf8_lossy(&buf[..n]).into_owned()
        });

        let http_cfg = RelayConfig {
            enabled: true,
            mode: RelayMode::Http,
            host: "127.0.0.1".into(),
            port: http_addr.port(),
            username: "relay_u".into(),
            password: "relay_p".into(),
            route_webview_media: true,
        };
        let http_client = http_cfg
            .apply_to_client_builder(Client::builder().timeout(Duration::from_secs(3)))
            .unwrap()
            .build()
            .unwrap();
        assert!(http_client.get("https://file-stal-9.dlfl.vn/test").send().await.is_err());
        let req_head = http_srv.await.unwrap();
        assert!(
            req_head.starts_with("CONNECT file-stal-9.dlfl.vn:443 HTTP/1.1"),
            "expected CONNECT via configured HTTP relay, got: {req_head}"
        );
        assert!(req_head.contains("Proxy-Authorization: Basic ") || req_head.contains("proxy-authorization: Basic "));
    }
}
