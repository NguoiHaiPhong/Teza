//! Lightweight dual-protocol (SOCKS5h + HTTP CONNECT) relay server for `litechat`.
//!
//! Designed to run on a Windows 10/11 or Linux bridge machine (`10.7.12.233`) so internal
//! workstations without Internet access can run `litechat` through a single authenticated port.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

const DEFAULT_LISTEN: &str = "0.0.0.0:1088";
const MAX_CONCURRENT_CONNS: usize = 512;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

const DEFAULT_ALLOWED_SUFFIXES: &[&str] = &[
    "zalo.me",
    "zaloapp.com",
    "zdn.vn",
    "zadn.vn",
    "zalo.vn",
    "api-free.deepl.com",
    "api.deepl.com",
    "generativelanguage.googleapis.com",
    "api.cognitive.microsofttranslator.com",
    "github.com",
    "githubusercontent.com",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RelayServerConfig {
    pub listen: String,
    pub username: String,
    pub password: String,
    pub allow_all: bool,
    pub extra_allowed_hosts: Vec<String>,
    pub allow_private_targets: bool,
    pub verbose: bool,
}

impl Default for RelayServerConfig {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN.into(),
            username: String::new(),
            password: String::new(),
            allow_all: false,
            extra_allowed_hosts: Vec::new(),
            allow_private_targets: false,
            verbose: false,
        }
    }
}

impl RelayServerConfig {
    pub fn auth_required(&self) -> bool {
        !self.username.is_empty() || !self.password.is_empty()
    }

    pub fn verify_credentials(&self, user: &str, pass: &str) -> bool {
        if !self.auth_required() {
            return true;
        }
        constant_time_eq(user.as_bytes(), self.username.as_bytes())
            && constant_time_eq(pass.as_bytes(), self.password.as_bytes())
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let max_len = a.len().max(b.len());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..max_len {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        diff |= x ^ y;
    }
    diff == 0
}

fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64) // CGNAT 100.64.0.0/10
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden_ip(IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || ((v6.segments()[0] & 0xfe00) == 0xfc00) // ULA fc00::/7
                || ((v6.segments()[0] & 0xffc0) == 0xfe80) // link-local fe80::/10
        }
    }
}

fn host_matches_suffix(host: &str, suffix: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let s = suffix.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() || s.is_empty() {
        return false;
    }
    h == s || h.ends_with(&format!(".{s}"))
}

fn is_allowed_target(host: &str, port: u16, cfg: &RelayServerConfig) -> bool {
    if host.trim().is_empty() || port == 0 {
        return false;
    }
    if cfg.allow_all {
        return true;
    }
    if port != 443 {
        return false;
    }
    if DEFAULT_ALLOWED_SUFFIXES
        .iter()
        .any(|s| host_matches_suffix(host, s))
    {
        return true;
    }
    cfg.extra_allowed_hosts
        .iter()
        .any(|s| host_matches_suffix(host, s))
}

async fn connect_outbound(host: &str, port: u16, cfg: &RelayServerConfig) -> Result<TcpStream> {
    if !is_allowed_target(host, port, cfg) {
        bail!("Target {host}:{port} bị từ chối bởi chính sách relay");
    }

    let addrs: Vec<SocketAddr> = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::lookup_host((host, port)),
    )
    .await
    .context("Timeout phân giải DNS đích")??
    .collect();

    if addrs.is_empty() {
        bail!("Không phân giải được địa chỉ đích {host}:{port}");
    }

    let usable: Vec<SocketAddr> = addrs
        .into_iter()
        .filter(|addr| cfg.allow_private_targets || !is_forbidden_ip(addr.ip()))
        .collect();

    if usable.is_empty() {
        bail!("Địa chỉ IP đích {host}:{port} nằm trong dải nội bộ bị cấm");
    }

    let mut last_err = None;
    for addr in usable {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Ok(Err(e)) => last_err = Some(anyhow::anyhow!(e)),
            Err(_) => last_err = Some(anyhow::anyhow!("Hết thời gian kết nối tới {addr}")),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Không kết nối được tới {host}:{port}")))
}

async fn handle_client(client: TcpStream, peer: SocketAddr, cfg: Arc<RelayServerConfig>) -> Result<()> {
    let _ = client.set_nodelay(true);
    let mut first = [0u8; 1];
    let n = tokio::time::timeout(HANDSHAKE_TIMEOUT, client.peek(&mut first))
        .await
        .context("Timeout đọc byte đầu")??;
    if n == 0 {
        return Ok(());
    }

    if first[0] == 0x05 {
        handle_socks5(client, peer, cfg).await
    } else {
        handle_http_connect(client, peer, cfg).await
    }
}

async fn reply_and_close(stream: &mut TcpStream, bytes: &[u8]) {
    let _ = stream.write_all(bytes).await;
    let _ = stream.shutdown().await;
    // Drain small unread request bytes (e.g. SOCKS5 dst addr or HTTP body) so Windows
    // Winsock does not emit TCP RST on closesocket() before the client reads the reply.
    let mut drain = [0u8; 512];
    let _ = tokio::time::timeout(Duration::from_millis(25), stream.read(&mut drain)).await;
}

async fn handle_socks5(
    mut client: TcpStream,
    peer: SocketAddr,
    cfg: Arc<RelayServerConfig>,
) -> Result<()> {
    let (host, port) = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let mut head = [0u8; 2];
        client.read_exact(&mut head).await?;
        if head[0] != 0x05 {
            bail!("Phiên bản SOCKS không hợp lệ");
        }
        let nmethods = head[1] as usize;
        if nmethods == 0 {
            bail!("SOCKS5 thiếu danh sách phương thức xác thực");
        }
        let mut methods = vec![0u8; nmethods];
        client.read_exact(&mut methods).await?;

        if cfg.auth_required() {
            if !methods.contains(&0x02) {
                reply_and_close(&mut client, &[0x05, 0xFF]).await;
                bail!("Proxy yêu cầu xác thực username/password");
            }
            client.write_all(&[0x05, 0x02]).await?;

            let mut auth_head = [0u8; 2];
            client.read_exact(&mut auth_head).await?;
            if auth_head[0] != 0x01 {
                reply_and_close(&mut client, &[0x01, 0x01]).await;
                bail!("Phiên bản xác thực SOCKS5 không hợp lệ");
            }
            let ulen = auth_head[1] as usize;
            let mut ubuf = vec![0u8; ulen];
            client.read_exact(&mut ubuf).await?;
            let plen = client.read_u8().await? as usize;
            let mut pbuf = vec![0u8; plen];
            client.read_exact(&mut pbuf).await?;

            let user = String::from_utf8_lossy(&ubuf);
            let pass = String::from_utf8_lossy(&pbuf);
            if !cfg.verify_credentials(&user, &pass) {
                reply_and_close(&mut client, &[0x01, 0x01]).await;
                bail!("Sai tài khoản hoặc mật khẩu proxy từ {peer}");
            }
            client.write_all(&[0x01, 0x00]).await?;
        } else if methods.contains(&0x00) {
            client.write_all(&[0x05, 0x00]).await?;
        } else {
            reply_and_close(&mut client, &[0x05, 0xFF]).await;
            bail!("Không có phương thức SOCKS5 tương thích");
        }

        let mut req_head = [0u8; 4];
        client.read_exact(&mut req_head).await?;
        if req_head[0] != 0x05 || req_head[1] != 0x01 {
            reply_and_close(&mut client, &[0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await;
            bail!("Chỉ hỗ trợ lệnh SOCKS5 CONNECT");
        }

        let host = match req_head[3] {
            0x01 => {
                let mut ip = [0u8; 4];
                client.read_exact(&mut ip).await?;
                std::net::Ipv4Addr::from(ip).to_string()
            }
            0x03 => {
                let len = client.read_u8().await? as usize;
                let mut domain = vec![0u8; len];
                client.read_exact(&mut domain).await?;
                String::from_utf8_lossy(&domain).into_owned()
            }
            0x04 => {
                let mut ip = [0u8; 16];
                client.read_exact(&mut ip).await?;
                std::net::Ipv6Addr::from(ip).to_string()
            }
            _ => {
                reply_and_close(&mut client, &[0x05, 0x08, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await;
                bail!("Kiểu địa chỉ SOCKS5 không hỗ trợ");
            }
        };
        let port = client.read_u16().await?;
        Ok::<_, anyhow::Error>((host, port))
    })
    .await??;

    let mut outbound = match connect_outbound(&host, port, &cfg).await {
        Ok(stream) => stream,
        Err(e) => {
            reply_and_close(&mut client, &[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await;
            return Err(e);
        }
    };

    if cfg.verbose {
        eprintln!("[SOCKS5] {peer} -> {host}:{port}");
    }

    client
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    let _ = tokio::io::copy_bidirectional(&mut client, &mut outbound).await;
    Ok(())
}

fn parse_connect_authority(target: &str) -> Option<(String, u16)> {
    let trimmed = target.trim();
    if let Some(rest) = trimmed.strip_prefix('[') {
        let (host, after) = rest.split_once("]:")?;
        let port = after.parse::<u16>().ok()?;
        return Some((host.to_string(), port));
    }
    let (host, port_str) = trimmed.rsplit_once(':')?;
    let port = port_str.parse::<u16>().ok()?;
    Some((host.to_string(), port))
}

fn verify_basic_proxy_auth(header_val: &str, cfg: &RelayServerConfig) -> bool {
    let Some(b64) = header_val
        .trim()
        .strip_prefix("Basic ")
        .or_else(|| header_val.trim().strip_prefix("basic "))
    else {
        return false;
    };
    let Ok(raw) = B64.decode(b64.trim()) else {
        return false;
    };
    let Ok(decoded) = std::str::from_utf8(&raw) else {
        return false;
    };
    let Some((user, pass)) = decoded.split_once(':') else {
        return false;
    };
    cfg.verify_credentials(user, pass)
}

async fn handle_http_connect(
    client: TcpStream,
    peer: SocketAddr,
    cfg: Arc<RelayServerConfig>,
) -> Result<()> {
    let mut reader = BufReader::new(client);
    let (host, port, auth_header) = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        let mut request_line = String::new();
        reader.read_line(&mut request_line).await?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("");

        let mut auth_header: Option<String> = None;
        let mut header_count = 0usize;
        loop {
            let mut line = String::new();
            let read = reader.read_line(&mut line).await?;
            if read == 0 || line == "\r\n" || line == "\n" {
                break;
            }
            header_count += 1;
            if header_count > 64 {
                bail!("Quá nhiều HTTP header");
            }
            if let Some((name, val)) = line.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Proxy-Authorization") {
                    auth_header = Some(val.trim().to_string());
                }
            }
        }

        if !method.eq_ignore_ascii_case("CONNECT") {
            reply_and_close(
                reader.get_mut(),
                b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\n\r\n",
            )
            .await;
            bail!("Chỉ hỗ trợ HTTP CONNECT tunnel");
        }

        let Some((host, port)) = parse_connect_authority(target) else {
            reply_and_close(
                reader.get_mut(),
                b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
            )
            .await;
            bail!("Đích CONNECT không hợp lệ: {target}");
        };

        Ok::<_, anyhow::Error>((host, port, auth_header))
    })
    .await??;

    if cfg.auth_required()
        && !auth_header
            .as_deref()
            .is_some_and(|h| verify_basic_proxy_auth(h, &cfg))
    {
        reply_and_close(
            reader.get_mut(),
            b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"LiteChat Relay\"\r\nConnection: close\r\n\r\n",
        )
        .await;
        bail!("Sai hoặc thiếu xác thực HTTP CONNECT từ {peer}");
    }

    let mut outbound = match connect_outbound(&host, port, &cfg).await {
        Ok(stream) => stream,
        Err(e) => {
            reply_and_close(
                reader.get_mut(),
                b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n",
            )
            .await;
            return Err(e);
        }
    };

    if cfg.verbose {
        eprintln!("[CONNECT] {peer} -> {host}:{port}");
    }

    reader
        .get_mut()
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    // Keep BufReader around TcpStream so any pipelined bytes read during header parsing are preserved.
    let _ = tokio::io::copy_bidirectional(&mut reader, &mut outbound).await;
    Ok(())
}

fn load_config_from_args() -> Result<RelayServerConfig> {
    let args: Vec<String> = std::env::args().collect();
    let mut config_path: Option<PathBuf> = None;
    let mut i = 1usize;
    while i < args.len() {
        if args[i] == "--config" && i + 1 < args.len() {
            config_path = Some(PathBuf::from(&args[i + 1]));
            break;
        }
        i += 1;
    }

    let mut cfg = if let Some(path) = config_path {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("Không đọc được tệp cấu hình {}", path.display()))?;
        serde_json::from_str::<RelayServerConfig>(&raw)
            .with_context(|| format!("Tệp cấu hình {} không hợp lệ", path.display()))?
    } else {
        RelayServerConfig::default()
    };

    if let Ok(v) = std::env::var("LITECHAT_RELAY_LISTEN") {
        if !v.trim().is_empty() {
            cfg.listen = v.trim().to_string();
        }
    }
    if let Ok(v) = std::env::var("LITECHAT_RELAY_USER") {
        cfg.username = v;
    }
    if let Ok(v) = std::env::var("LITECHAT_RELAY_PASS") {
        cfg.password = v;
    }

    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => i += 2,
            "--listen" if i + 1 < args.len() => {
                cfg.listen = args[i + 1].clone();
                i += 2;
            }
            "--user" if i + 1 < args.len() => {
                cfg.username = args[i + 1].clone();
                i += 2;
            }
            "--pass" if i + 1 < args.len() => {
                cfg.password = args[i + 1].clone();
                i += 2;
            }
            "--allow-all" => {
                cfg.allow_all = true;
                i += 1;
            }
            "--verbose" | "-v" => {
                cfg.verbose = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    Ok(cfg)
}

pub async fn run_server(listener: TcpListener, cfg: Arc<RelayServerConfig>) {
    let sem = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNS));
    loop {
        let Ok((stream, peer)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let Ok(permit) = sem.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let cfg_clone = cfg.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(err) = handle_client(stream, peer, cfg_clone.clone()).await {
                if cfg_clone.verbose {
                    eprintln!("[ERR] {peer}: {err}");
                }
            }
        });
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Arc::new(load_config_from_args()?);
    let listener = TcpListener::bind(&cfg.listen)
        .await
        .with_context(|| format!("Không mở được cổng relay {}", cfg.listen))?;
    eprintln!(
        "litechat-relay đang lắng nghe trên {} (auth: {}, mode: {})",
        cfg.listen,
        if cfg.auth_required() { "bật" } else { "tắt" },
        if cfg.allow_all { "allow_all" } else { "allowlist" }
    );
    run_server(listener, cfg).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowlist_and_private_ip_guards() {
        let mut cfg = RelayServerConfig::default();
        assert!(is_allowed_target("chat.zalo.me", 443, &cfg));
        assert!(is_allowed_target("CHAT.ZALO.ME.", 443, &cfg));
        assert!(is_allowed_target("f10-zpc.zdn.vn", 443, &cfg));
        assert!(is_allowed_target("api-free.deepl.com", 443, &cfg));
        assert!(is_allowed_target("objects.githubusercontent.com", 443, &cfg));
        assert!(!is_allowed_target("chat.zalo.me", 80, &cfg));
        assert!(!is_allowed_target("evilzalo.me", 443, &cfg));
        assert!(!is_allowed_target("facebook.com", 443, &cfg));

        cfg.extra_allowed_hosts = vec!["custom.example.org".into()];
        assert!(is_allowed_target("api.custom.example.org", 443, &cfg));
        cfg.allow_all = true;
        assert!(is_allowed_target("facebook.com", 8080, &cfg));

        assert!(is_forbidden_ip("127.0.0.1".parse().unwrap()));
        assert!(is_forbidden_ip("10.7.12.233".parse().unwrap()));
        assert!(is_forbidden_ip("100.64.0.1".parse().unwrap()));
        assert!(is_forbidden_ip("192.168.1.1".parse().unwrap()));
        assert!(is_forbidden_ip("169.254.169.254".parse().unwrap()));
        assert!(is_forbidden_ip("::1".parse().unwrap()));
        assert!(is_forbidden_ip("fd00::1".parse().unwrap()));
        assert!(is_forbidden_ip("fe80::1".parse().unwrap()));
        assert!(is_forbidden_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_forbidden_ip("::ffff:10.7.12.233".parse().unwrap()));
        assert!(is_forbidden_ip("::ffff:169.254.169.254".parse().unwrap()));
        assert!(!is_forbidden_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_forbidden_ip("2606:4700:4700::1111".parse().unwrap()));

        assert_eq!(
            parse_connect_authority("chat.zalo.me:443"),
            Some(("chat.zalo.me".into(), 443))
        );
        assert_eq!(
            parse_connect_authority("[::1]:443"),
            Some(("::1".into(), 443))
        );
        assert_eq!(parse_connect_authority("bad-no-port"), None);
    }

    #[tokio::test]
    async fn http_connect_auth_policy_and_pipelined_payload() {
        let cfg = RelayServerConfig {
            username: "lite".into(),
            password: "secret".into(),
            allow_all: false,
            allow_private_targets: true,
            ..Default::default()
        };
        let (_, relay_addr) = spawn_echo_and_relay(cfg).await;

        // Missing auth -> 407
        let mut s407 = TcpStream::connect(relay_addr).await.unwrap();
        s407.write_all(b"CONNECT chat.zalo.me:443 HTTP/1.1\r\nHost: chat.zalo.me:443\r\n\r\n")
            .await
            .unwrap();
        let mut buf = [0u8; 256];
        let n = s407.read(&mut buf).await.unwrap();
        assert!(std::str::from_utf8(&buf[..n]).unwrap().contains("407"));

        // Non-CONNECT verb -> 405
        let mut s405 = TcpStream::connect(relay_addr).await.unwrap();
        s405.write_all(b"GET https://chat.zalo.me/ HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let n = s405.read(&mut buf).await.unwrap();
        assert!(std::str::from_utf8(&buf[..n]).unwrap().contains("405"));

        // Disallowed target with valid auth -> 403
        let auth = B64.encode("lite:secret");
        let mut s403 = TcpStream::connect(relay_addr).await.unwrap();
        let req403 = format!(
            "CONNECT evil.example:443 HTTP/1.1\r\nProxy-Authorization: Basic {auth}\r\n\r\n"
        );
        s403.write_all(req403.as_bytes()).await.unwrap();
        let n = s403.read(&mut buf).await.unwrap();
        assert!(std::str::from_utf8(&buf[..n]).unwrap().contains("403"));

        // Pipelined CONNECT + payload must preserve early payload bytes in BufReader
        let pipe_cfg = RelayServerConfig {
            username: "lite".into(),
            password: "secret".into(),
            allow_all: true,
            allow_private_targets: true,
            ..Default::default()
        };
        let (echo_addr, pipe_relay) = spawn_echo_and_relay(pipe_cfg).await;
        let mut spipe = TcpStream::connect(pipe_relay).await.unwrap();
        let combined = format!(
            "CONNECT 127.0.0.1:{} HTTP/1.1\r\nProxy-Authorization: Basic {auth}\r\n\r\nPIPELINED",
            echo_addr.port()
        );
        spipe.write_all(combined.as_bytes()).await.unwrap();
        let expected_head = b"HTTP/1.1 200 Connection Established\r\n\r\nPIPELINED";
        let mut got = vec![0u8; expected_head.len()];
        spipe.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, expected_head);
    }

    #[tokio::test]
    async fn socks5_rejects_bad_auth_non_connect_and_mapped_loopback() {
        let cfg = RelayServerConfig {
            username: "lite".into(),
            password: "secret".into(),
            allow_all: true,
            allow_private_targets: false,
            ..Default::default()
        };
        let (echo_addr, relay_addr) = spawn_echo_and_relay(cfg).await;

        // Wrong password -> auth failure [0x01, 0x01]
        let mut bad_auth = TcpStream::connect(relay_addr).await.unwrap();
        bad_auth.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
        let mut rep = [0u8; 2];
        bad_auth.read_exact(&mut rep).await.unwrap();
        assert_eq!(rep, [0x05, 0x02]);
        bad_auth.write_all(b"\x01\x04lite\x05wrong").await.unwrap();
        bad_auth.read_exact(&mut rep).await.unwrap();
        assert_eq!(rep, [0x01, 0x01]);

        // IPv4-mapped IPv6 loopback (::ffff:127.0.0.1) blocked when allow_private_targets = false
        let mut mapped = TcpStream::connect(relay_addr).await.unwrap();
        mapped.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
        mapped.read_exact(&mut rep).await.unwrap();
        mapped.write_all(b"\x01\x04lite\x06secret").await.unwrap();
        mapped.read_exact(&mut rep).await.unwrap();
        assert_eq!(rep, [0x01, 0x00]);
        let mapped_v6: std::net::Ipv6Addr = "::ffff:127.0.0.1".parse().unwrap();
        let mut req = vec![0x05, 0x01, 0x00, 0x04];
        req.extend_from_slice(&mapped_v6.octets());
        req.extend_from_slice(&echo_addr.port().to_be_bytes());
        mapped.write_all(&req).await.unwrap();
        let mut conn_rep = [0u8; 10];
        mapped.read_exact(&mut conn_rep).await.unwrap();
        assert_ne!(conn_rep[1], 0x00);

        // Non-CONNECT command (0x02 BIND) rejected with 0x07
        let mut bad_cmd = TcpStream::connect(relay_addr).await.unwrap();
        bad_cmd.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
        bad_cmd.read_exact(&mut rep).await.unwrap();
        bad_cmd.write_all(b"\x01\x04lite\x06secret").await.unwrap();
        bad_cmd.read_exact(&mut rep).await.unwrap();
        bad_cmd
            .write_all(&[0x05, 0x02, 0x00, 0x01, 1, 1, 1, 1, 0x01, 0xBB])
            .await
            .unwrap();
        bad_cmd.read_exact(&mut conn_rep).await.unwrap();
        assert_eq!(conn_rep[1], 0x07);
    }

    async fn spawn_echo_and_relay(cfg: RelayServerConfig) -> (SocketAddr, SocketAddr) {
        let echo_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo_listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = echo_listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 64];
                    if let Ok(n) = s.read(&mut buf).await {
                        let _ = s.write_all(&buf[..n]).await;
                    }
                });
            }
        });

        let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay_listener.local_addr().unwrap();
        tokio::spawn(run_server(relay_listener, Arc::new(cfg)));
        (echo_addr, relay_addr)
    }

    #[tokio::test]
    async fn socks5_and_http_connect_relay_roundtrip_work() {
        let cfg = RelayServerConfig {
            username: "lite".into(),
            password: "secret".into(),
            allow_all: true,
            allow_private_targets: true,
            ..Default::default()
        };
        let (echo_addr, relay_addr) = spawn_echo_and_relay(cfg).await;

        // 1. SOCKS5 with auth
        let mut s = TcpStream::connect(relay_addr).await.unwrap();
        s.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
        let mut rep = [0u8; 2];
        s.read_exact(&mut rep).await.unwrap();
        assert_eq!(rep, [0x05, 0x02]);
        s.write_all(b"\x01\x04lite\x06secret").await.unwrap();
        s.read_exact(&mut rep).await.unwrap();
        assert_eq!(rep, [0x01, 0x00]);
        let host = b"127.0.0.1";
        let mut req = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
        req.extend_from_slice(host);
        req.extend_from_slice(&echo_addr.port().to_be_bytes());
        s.write_all(&req).await.unwrap();
        let mut conn_rep = [0u8; 10];
        s.read_exact(&mut conn_rep).await.unwrap();
        assert_eq!(conn_rep[1], 0x00);
        s.write_all(b"ping-socks5").await.unwrap();
        let mut echo = [0u8; 11];
        s.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"ping-socks5");

        // 2. HTTP CONNECT with auth
        let mut h = TcpStream::connect(relay_addr).await.unwrap();
        let auth = B64.encode("lite:secret");
        let connect_req = format!(
            "CONNECT 127.0.0.1:{} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nProxy-Authorization: Basic {auth}\r\n\r\n",
            echo_addr.port(),
            echo_addr.port()
        );
        h.write_all(connect_req.as_bytes()).await.unwrap();
        let mut line_buf = vec![0u8; 128];
        let n = h.read(&mut line_buf).await.unwrap();
        let head = std::str::from_utf8(&line_buf[..n]).unwrap();
        assert!(head.contains("200 Connection Established"));
        h.write_all(b"ping-http").await.unwrap();
        let mut echo2 = [0u8; 9];
        h.read_exact(&mut echo2).await.unwrap();
        assert_eq!(&echo2, b"ping-http");
    }
}
