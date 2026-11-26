//! Lightweight dual-protocol (SOCKS5h + HTTP CONNECT) relay server for `litechat`.
//!
//! Designed to run on a Windows 10/11 or Linux bridge machine (`10.7.12.233`) so internal
//! workstations without Internet access can run `litechat` through a single authenticated port.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{RwLock, Semaphore};

const DEFAULT_LISTEN: &str = "0.0.0.0:1088";
const MAX_CONCURRENT_CONNS: usize = 512;
const MAX_HTTP_HEADER_BYTES: usize = 8192;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const SYSTEM_DNS_TIMEOUT: Duration = Duration::from_secs(20);
const UDP_DNS_TIMEOUT: Duration = Duration::from_secs(3);
const DNS_CACHE_MAX_ENTRIES: usize = 1024;
const DNS_TTL_MIN_SECS: u32 = 30;
const DNS_TTL_MAX_SECS: u32 = 300;

const DEFAULT_ALLOWED_SUFFIXES: &[&str] = &[
    // Official Zalo / VNG domains (kept synchronized with zalo-core::session::ALLOWED_MEDIA_HOST_SUFFIXES)
    "zalo.me",
    "zaloapp.com",
    "zdn.vn",
    "zadn.vn",
    "zalo.vn",
    "zalo.com",
    "zalo.cloud",
    "zbox.vn",
    "zalo.video",
    "zapps.me",
    "zaloplatforms.com",
    "zing.vn",
    "dlfl.vn",
    "dlfl.me",
    "flchat.vn",
    "dlmd.me",
    "mdchat.me",
    // Translation & updater endpoints
    "api-free.deepl.com",
    "api.deepl.com",
    "generativelanguage.googleapis.com",
    "api.cognitive.microsofttranslator.com",
    "github.com",
    "githubusercontent.com",
];

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RelayServerConfig {
    pub listen: String,
    pub username: String,
    pub password: String,
    pub allow_all: bool,
    pub extra_allowed_hosts: Vec<String>,
    pub allow_private_targets: bool,
    pub dns_servers: Vec<String>,
    pub verbose: bool,
}

impl std::fmt::Debug for RelayServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayServerConfig")
            .field("listen", &self.listen)
            .field("username", &self.username)
            .field(
                "password",
                &if self.password.is_empty() {
                    ""
                } else {
                    "[REDACTED]"
                },
            )
            .field("allow_all", &self.allow_all)
            .field("extra_allowed_hosts", &self.extra_allowed_hosts)
            .field("allow_private_targets", &self.allow_private_targets)
            .field("dns_servers", &self.dns_servers)
            .field("verbose", &self.verbose)
            .finish()
    }
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
            dns_servers: Vec::new(),
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
    let mut diff: usize = a.len() ^ b.len();
    for i in 0..max_len {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        diff |= usize::from(x ^ y);
    }
    diff == 0
}

fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            octets[0] == 0 // "this network" 0.0.0.0/8 (RFC 1122 / RFC 6890)
                || v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || (octets[0] == 100 && (octets[1] & 0xC0) == 64) // CGNAT 100.64.0.0/10
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden_ip(IpAddr::V4(v4));
            }
            let segs = v6.segments();
            // NAT64 well-known prefix 64:ff9b::/96 (RFC 6052) and local-use 64:ff9b:1::/48 (RFC 8215)
            if segs[0] == 0x0064
                && segs[1] == 0xff9b
                && ((segs[2] == 0 && segs[3] == 0 && segs[4] == 0 && segs[5] == 0)
                    || segs[2] == 0x0001)
            {
                return true;
            }
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || ((segs[0] & 0xfe00) == 0xfc00) // ULA fc00::/7
                || ((segs[0] & 0xffc0) == 0xfe80) // link-local fe80::/10
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

type DnsCacheMap = HashMap<String, (Instant, Vec<IpAddr>)>;
static DNS_CACHE: OnceLock<RwLock<DnsCacheMap>> = OnceLock::new();
static DNS_TXID: AtomicU16 = AtomicU16::new(1);

fn dns_cache() -> &'static RwLock<DnsCacheMap> {
    DNS_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn normalize_dns_server(raw: &str) -> Option<SocketAddr> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(sock) = trimmed.parse::<SocketAddr>() {
        return Some(sock);
    }
    let bare = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(trimmed);
    bare.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, 53))
}

fn parse_dns_server_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_extra_host_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| {
            s.trim()
                .trim_start_matches("*.")
                .trim_start_matches('.')
                .trim_end_matches('.')
                .to_ascii_lowercase()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

fn extend_unique_hosts(target: &mut Vec<String>, extra: impl IntoIterator<Item = String>) {
    for host in extra {
        if !host.is_empty() && !target.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
            target.push(host);
        }
    }
}

fn build_dns_query(host: &str, qtype: u16, txid: u16) -> Result<Vec<u8>> {
    let clean = host.trim().trim_end_matches('.');
    if clean.is_empty() || clean.len() > 253 {
        bail!("Tên miền DNS không hợp lệ: {host}");
    }
    let mut pkt = Vec::with_capacity(32 + clean.len());
    pkt.extend_from_slice(&txid.to_be_bytes());
    pkt.extend_from_slice(&0x0100u16.to_be_bytes()); // standard recursive query (RD=1)
    pkt.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT = 1
    pkt.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT = 0
    pkt.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT = 0
    pkt.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT = 0

    for label in clean.split('.') {
        if label.is_empty() || label.len() > 63 {
            bail!("Nhãn tên miền DNS không hợp lệ: {host}");
        }
        pkt.push(label.len() as u8);
        pkt.extend_from_slice(label.as_bytes());
    }
    pkt.push(0);
    pkt.extend_from_slice(&qtype.to_be_bytes());
    pkt.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
    Ok(pkt)
}

fn skip_dns_name(pkt: &[u8], start: usize) -> Result<usize> {
    let mut pos = start;
    for _ in 0..128 {
        let len_byte = *pkt
            .get(pos)
            .context("Gói tin DNS bị cắt cụt khi đọc tên miền")?;
        if len_byte == 0 {
            return Ok(pos + 1);
        }
        if (len_byte & 0xC0) == 0xC0 {
            if pos + 1 >= pkt.len() {
                bail!("Con trỏ nén DNS bị cắt cụt");
            }
            return Ok(pos + 2);
        }
        if (len_byte & 0xC0) != 0 {
            bail!("Cờ nhãn DNS không hợp lệ");
        }
        let next = pos + 1 + len_byte as usize;
        if next > pkt.len() {
            bail!("Nhãn tên miền DNS vượt quá kích thước gói");
        }
        pos = next;
    }
    bail!("Tên miền DNS vượt quá giới hạn nhãn");
}

fn read_dns_name(pkt: &[u8], start: usize) -> Result<(String, usize)> {
    let mut pos = start;
    let mut next_pos: Option<usize> = None;
    let mut labels: Vec<String> = Vec::new();
    let mut total_len = 0usize;
    let mut jumped = false;

    for _ in 0..128 {
        let len_byte = *pkt
            .get(pos)
            .context("Gói tin DNS bị cắt cụt khi đọc tên miền")?;
        if len_byte == 0 {
            let end = next_pos.unwrap_or(pos + 1);
            return Ok((labels.join("."), end));
        }
        if (len_byte & 0xC0) == 0xC0 {
            if pos + 1 >= pkt.len() {
                bail!("Con trỏ nén DNS bị cắt cụt");
            }
            let ptr = (((len_byte & 0x3F) as usize) << 8) | (pkt[pos + 1] as usize);
            if ptr >= pkt.len() {
                bail!("Con trỏ nén DNS trỏ ra ngoài gói tin");
            }
            if !jumped {
                next_pos = Some(pos + 2);
                jumped = true;
            }
            pos = ptr;
            continue;
        }
        if (len_byte & 0xC0) != 0 {
            bail!("Cờ nhãn DNS không hợp lệ");
        }
        let label_len = len_byte as usize;
        let next = pos + 1 + label_len;
        if next > pkt.len() {
            bail!("Nhãn tên miền DNS vượt quá kích thước gói");
        }
        total_len += label_len + usize::from(!labels.is_empty());
        if total_len > 253 {
            bail!("Tên miền DNS vượt quá độ dài tối đa 253 byte");
        }
        let label = std::str::from_utf8(&pkt[pos + 1..next])
            .context("Nhãn tên miền DNS không phải UTF-8 hợp lệ")?;
        labels.push(label.to_ascii_lowercase());
        pos = next;
    }
    bail!("Tên miền DNS vượt quá giới hạn nhãn");
}

#[cfg(target_os = "windows")]
fn os_random_u16() -> Option<u16> {
    use std::ffi::c_void;
    use std::ptr::null_mut;

    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;

    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptGenRandom(
            h_algorithm: *mut c_void,
            pb_buffer: *mut u8,
            cb_buffer: u32,
            dw_flags: u32,
        ) -> i32;
    }

    let mut buf = [0u8; 2];
    // SAFETY: `buf` is a valid 2-byte writable buffer; `h_algorithm` is NULL when
    // `BCRYPT_USE_SYSTEM_PREFERRED_RNG` is set per Windows CNG specification.
    let status = unsafe {
        BCryptGenRandom(
            null_mut(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    (status == 0).then(|| u16::from_ne_bytes(buf))
}

#[cfg(unix)]
fn os_random_u16() -> Option<u16> {
    use std::io::Read;

    let mut f = std::fs::File::open("/dev/urandom").ok()?;
    let mut buf = [0u8; 2];
    f.read_exact(&mut buf).ok()?;
    Some(u16::from_ne_bytes(buf))
}

#[cfg(not(any(target_os = "windows", unix)))]
fn os_random_u16() -> Option<u16> {
    None
}

fn generate_dns_txid() -> u16 {
    if let Some(v) = os_random_u16() {
        return v;
    }
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::time::{SystemTime, UNIX_EPOCH};

    let mut hasher = RandomState::new().build_hasher();
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    hasher.write_u128(now_ns);
    hasher.write_u32(std::process::id());
    hasher.write_u16(DNS_TXID.fetch_add(1, Ordering::Relaxed));
    let stack_marker: u8 = 0;
    hasher.write_usize((&stack_marker as *const u8) as usize);
    let h = hasher.finish();
    ((h ^ (h >> 16) ^ (h >> 32) ^ (h >> 48)) & 0xFFFF) as u16
}

fn parse_dns_response(
    pkt: &[u8],
    expected_txid: u16,
    expected_host: &str,
    expected_qtype: u16,
) -> Result<(Vec<IpAddr>, u32)> {
    if pkt.len() < 12 {
        bail!("Gói tin phản hồi DNS quá ngắn");
    }
    let txid = u16::from_be_bytes([pkt[0], pkt[1]]);
    if txid != expected_txid {
        bail!("Mã giao dịch DNS không khớp");
    }
    let flags = u16::from_be_bytes([pkt[2], pkt[3]]);
    if (flags & 0x8000) == 0 {
        bail!("Gói tin DNS không phải phản hồi");
    }
    let rcode = flags & 0x000F;
    if rcode != 0 {
        bail!("Máy chủ DNS trả mã lỗi RCODE={rcode}");
    }

    let qdcount = u16::from_be_bytes([pkt[4], pkt[5]]);
    if qdcount != 1 {
        bail!("Phản hồi DNS có QDCOUNT không hợp lệ ({qdcount})");
    }
    let ancount = u16::from_be_bytes([pkt[6], pkt[7]]);
    let mut pos = 12usize;

    let (qname, after_qname) = read_dns_name(pkt, pos)?;
    pos = after_qname;
    if pos + 4 > pkt.len() {
        bail!("Phần Question DNS bị cắt cụt");
    }
    let qtype = u16::from_be_bytes([pkt[pos], pkt[pos + 1]]);
    let qclass = u16::from_be_bytes([pkt[pos + 2], pkt[pos + 3]]);
    pos += 4;

    let expected_norm = expected_host
        .trim()
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if qname != expected_norm {
        bail!("Question QNAME DNS không khớp (nhận {qname}, mong đợi {expected_norm})");
    }
    if qtype != expected_qtype || qclass != 1 {
        bail!("Question QTYPE/QCLASS DNS không khớp (qtype={qtype}, qclass={qclass})");
    }

    let mut ips = Vec::new();
    let mut min_ttl: Option<u32> = None;

    for _ in 0..ancount {
        pos = skip_dns_name(pkt, pos)?;
        if pos + 10 > pkt.len() {
            bail!("Phần Answer header DNS bị cắt cụt");
        }
        let rtype = u16::from_be_bytes([pkt[pos], pkt[pos + 1]]);
        let rclass = u16::from_be_bytes([pkt[pos + 2], pkt[pos + 3]]);
        let ttl = u32::from_be_bytes([pkt[pos + 4], pkt[pos + 5], pkt[pos + 6], pkt[pos + 7]]);
        let rdlen = u16::from_be_bytes([pkt[pos + 8], pkt[pos + 9]]) as usize;
        pos += 10;
        if pos + rdlen > pkt.len() {
            bail!("Phần dữ liệu RR DNS bị cắt cụt");
        }
        if rclass == 1 && rtype == expected_qtype {
            if rtype == 1 && rdlen == 4 {
                let ip = Ipv4Addr::new(pkt[pos], pkt[pos + 1], pkt[pos + 2], pkt[pos + 3]);
                ips.push(IpAddr::V4(ip));
                min_ttl = Some(min_ttl.map_or(ttl, |cur| cur.min(ttl)));
            } else if rtype == 28 && rdlen == 16 {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&pkt[pos..pos + 16]);
                ips.push(IpAddr::V6(Ipv6Addr::from(octets)));
                min_ttl = Some(min_ttl.map_or(ttl, |cur| cur.min(ttl)));
            }
        }
        pos += rdlen;
    }

    Ok((ips, min_ttl.unwrap_or(60)))
}

async fn query_udp_dns_once(
    server: SocketAddr,
    host: &str,
    qtype: u16,
) -> Result<(Vec<IpAddr>, u32)> {
    let txid = generate_dns_txid();
    let query = build_dns_query(host, qtype, txid)?;
    let bind_addr = if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let sock = UdpSocket::bind(bind_addr).await?;
    sock.connect(server).await?;
    sock.send(&query).await?;
    let mut buf = [0u8; 1500];
    let n = tokio::time::timeout(UDP_DNS_TIMEOUT, sock.recv(&mut buf))
        .await
        .context("Timeout chờ phản hồi UDP DNS")??;
    parse_dns_response(&buf[..n], txid, host, qtype)
}

async fn resolve_via_udp_dns(host: &str, servers: &[String]) -> Result<(Vec<IpAddr>, Duration)> {
    let addrs: Vec<SocketAddr> = servers
        .iter()
        .filter_map(|s| normalize_dns_server(s))
        .collect();
    if addrs.is_empty() {
        bail!("Không có địa chỉ DNS hợp lệ trong cấu hình");
    }

    let mut last_err = None;
    for qtype in [1u16, 28u16] {
        for &server in &addrs {
            match query_udp_dns_once(server, host, qtype).await {
                Ok((ips, ttl)) if !ips.is_empty() => {
                    let clamped = ttl.clamp(DNS_TTL_MIN_SECS, DNS_TTL_MAX_SECS);
                    return Ok((ips, Duration::from_secs(clamped as u64)));
                }
                Ok(_) => {}
                Err(e) => last_err = Some(e),
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Không phân giải được IP cho {host}")))
}

async fn resolve_target_ips(
    host: &str,
    port: u16,
    cfg: &RelayServerConfig,
) -> Result<(Vec<IpAddr>, Option<String>)> {
    let bare = host.trim().trim_matches(|c| c == '[' || c == ']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return Ok((vec![ip], None));
    }

    let cache_key = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let now = Instant::now();
    {
        let cache = dns_cache().read().await;
        if let Some((expires_at, cached)) = cache.get(&cache_key) {
            if now < *expires_at && !cached.is_empty() {
                return Ok((cached.clone(), Some(cache_key)));
            }
        }
    }

    let mut resolved: Option<(Vec<IpAddr>, Duration)> = None;
    if !cfg.dns_servers.is_empty() {
        match resolve_via_udp_dns(host, &cfg.dns_servers).await {
            Ok(pair) => resolved = Some(pair),
            Err(e) if cfg.verbose => {
                eprintln!("[DNS-UDP] {host}: {e}; fallback về DNS hệ thống");
            }
            Err(_) => {}
        }
    }

    let (ips, ttl_dur) = match resolved {
        Some(pair) => pair,
        None => {
            let addrs: Vec<SocketAddr> =
                tokio::time::timeout(SYSTEM_DNS_TIMEOUT, tokio::net::lookup_host((host, port)))
                    .await
                    .context("Timeout phân giải DNS đích")??
                    .collect();
            let mut dedup = Vec::with_capacity(addrs.len());
            for addr in addrs {
                let ip = addr.ip();
                if !dedup.contains(&ip) {
                    dedup.push(ip);
                }
            }
            (dedup, Duration::from_secs(60))
        }
    };

    if ips.is_empty() {
        bail!("Không phân giải được địa chỉ đích {host}:{port}");
    }

    {
        let mut cache = dns_cache().write().await;
        if cache.len() >= DNS_CACHE_MAX_ENTRIES {
            let cutoff = Instant::now();
            cache.retain(|_, (exp, _)| *exp > cutoff);
            if cache.len() >= DNS_CACHE_MAX_ENTRIES {
                cache.clear();
            }
        }
        cache.insert(cache_key.clone(), (Instant::now() + ttl_dur, ips.clone()));
    }

    Ok((ips, Some(cache_key)))
}

async fn connect_outbound(host: &str, port: u16, cfg: &RelayServerConfig) -> Result<TcpStream> {
    if !is_allowed_target(host, port, cfg) {
        bail!("Target {host}:{port} bị từ chối bởi chính sách relay");
    }

    let (ips, cache_key) = resolve_target_ips(host, port, cfg).await?;

    let mut usable: Vec<SocketAddr> = ips
        .into_iter()
        .filter(|ip| cfg.allow_private_targets || !is_forbidden_ip(*ip))
        .map(|ip| SocketAddr::new(ip, port))
        .collect();

    if usable.is_empty() {
        bail!("Địa chỉ IP đích {host}:{port} nằm trong dải nội bộ bị cấm");
    }

    usable.sort_by_key(|a| if a.is_ipv4() { 0u8 } else { 1u8 });

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
    if let Some(key) = cache_key {
        dns_cache().write().await.remove(&key);
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
        let mut remaining = MAX_HTTP_HEADER_BYTES;
        let mut req_buf = Vec::with_capacity(128);
        let req_read = (&mut reader)
            .take(remaining as u64)
            .read_until(b'\n', &mut req_buf)
            .await?;
        if req_read == 0 {
            bail!("Kết nối đóng trước khi nhận dòng yêu cầu HTTP");
        }
        if !req_buf.ends_with(b"\n") {
            if req_read >= remaining {
                reply_and_close(
                    reader.get_mut(),
                    b"HTTP/1.1 431 Request Header Fields Too Large\r\nConnection: close\r\n\r\n",
                )
                .await;
                bail!("Dòng yêu cầu HTTP vượt quá giới hạn {MAX_HTTP_HEADER_BYTES} byte");
            }
            reply_and_close(
                reader.get_mut(),
                b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
            )
            .await;
            bail!("Dòng yêu cầu HTTP không đầy đủ");
        }
        remaining -= req_read;

        let request_line = match std::str::from_utf8(&req_buf) {
            Ok(s) => s,
            Err(_) => {
                reply_and_close(
                    reader.get_mut(),
                    b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
                )
                .await;
                bail!("Dòng yêu cầu HTTP không phải UTF-8 hợp lệ");
            }
        };
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("");

        let mut auth_header: Option<String> = None;
        let mut header_count = 0usize;
        let mut line_buf = Vec::with_capacity(128);
        loop {
            if remaining == 0 {
                reply_and_close(
                    reader.get_mut(),
                    b"HTTP/1.1 431 Request Header Fields Too Large\r\nConnection: close\r\n\r\n",
                )
                .await;
                bail!("Header HTTP vượt quá giới hạn {MAX_HTTP_HEADER_BYTES} byte");
            }
            line_buf.clear();
            let read = (&mut reader)
                .take(remaining as u64)
                .read_until(b'\n', &mut line_buf)
                .await?;
            if read == 0 {
                reply_and_close(
                    reader.get_mut(),
                    b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
                )
                .await;
                bail!("Kết nối đóng trước khi kết thúc phần header HTTP");
            }
            if !line_buf.ends_with(b"\n") {
                if read >= remaining {
                    reply_and_close(
                        reader.get_mut(),
                        b"HTTP/1.1 431 Request Header Fields Too Large\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                    bail!("Header HTTP vượt quá giới hạn {MAX_HTTP_HEADER_BYTES} byte");
                }
                reply_and_close(
                    reader.get_mut(),
                    b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
                )
                .await;
                bail!("Dòng header HTTP không đầy đủ");
            }
            remaining -= read;
            if line_buf == b"\r\n" || line_buf == b"\n" {
                break;
            }
            header_count += 1;
            if header_count > 64 {
                reply_and_close(
                    reader.get_mut(),
                    b"HTTP/1.1 431 Request Header Fields Too Large\r\nConnection: close\r\n\r\n",
                )
                .await;
                bail!("Quá nhiều HTTP header");
            }
            let line = match std::str::from_utf8(&line_buf) {
                Ok(s) => s,
                Err(_) => {
                    reply_and_close(
                        reader.get_mut(),
                        b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                    bail!("Header HTTP không phải UTF-8 hợp lệ");
                }
            };
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

fn cli_used_pass_flag(args: &[String]) -> bool {
    args.iter()
        .skip(1)
        .any(|a| a == "--pass" || a.starts_with("--pass="))
}

fn is_loopback_bind(listen: &str) -> bool {
    let trimmed = listen.trim();
    if let Ok(sock) = trimmed.parse::<SocketAddr>() {
        return sock.ip().is_loopback();
    }
    let host = if let Some(rest) = trimmed.strip_prefix('[') {
        rest.split_once(']').map(|(h, _)| h).unwrap_or(trimmed)
    } else {
        trimmed.rsplit_once(':').map(|(h, _)| h).unwrap_or(trimmed)
    };
    let normalized = host.trim().trim_end_matches('.');
    normalized.eq_ignore_ascii_case("localhost")
        || normalized
            .parse::<IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn startup_security_warnings(
    cfg: &RelayServerConfig,
    bound_listen: &str,
    pass_from_cli: bool,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if cfg.allow_all && !cfg.auth_required() && !is_loopback_bind(bound_listen) {
        warnings.push(format!(
            "[SECURITY WARNING] --allow-all is enabled on non-loopback bind ({bound_listen}) without authentication! \
             Set LITECHAT_RELAY_USER + LITECHAT_RELAY_PASS (or bind to 127.0.0.1) to avoid running an open proxy."
        ));
    }
    if pass_from_cli {
        warnings.push(
            "[SECURITY WARNING] Passing password via --pass exposes credentials in process listings; \
             prefer setting the LITECHAT_RELAY_PASS environment variable or using a restricted --config file."
                .to_string(),
        );
    }
    warnings
}

fn load_config_from_args() -> Result<(RelayServerConfig, bool)> {
    let args: Vec<String> = std::env::args().collect();
    let used_pass = cli_used_pass_flag(&args);
    let cfg = parse_config_with(&args, |key| std::env::var(key).ok())?;
    Ok((cfg, used_pass))
}

fn parse_config_with<F>(args: &[String], env_lookup: F) -> Result<RelayServerConfig>
where
    F: Fn(&str) -> Option<String>,
{
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

    if let Some(v) = env_lookup("LITECHAT_RELAY_LISTEN") {
        if !v.trim().is_empty() {
            cfg.listen = v.trim().to_string();
        }
    }
    if let Some(v) = env_lookup("LITECHAT_RELAY_USER") {
        cfg.username = v;
    }
    if let Some(v) = env_lookup("LITECHAT_RELAY_PASS") {
        cfg.password = v;
    }
    if let Some(v) = env_lookup("LITECHAT_RELAY_DNS") {
        let parsed = parse_dns_server_list(&v);
        if !parsed.is_empty() {
            cfg.dns_servers = parsed;
        }
    }
    if let Some(v) = env_lookup("LITECHAT_RELAY_EXTRA_HOSTS") {
        extend_unique_hosts(&mut cfg.extra_allowed_hosts, parse_extra_host_list(&v));
    }

    let mut cli_dns = Vec::new();
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
            arg if arg.starts_with("--pass=") => {
                if let Some(val) = arg.strip_prefix("--pass=") {
                    cfg.password = val.to_string();
                }
                i += 1;
            }
            "--dns" if i + 1 < args.len() => {
                cli_dns.extend(parse_dns_server_list(&args[i + 1]));
                i += 2;
            }
            "--extra-host" if i + 1 < args.len() => {
                extend_unique_hosts(
                    &mut cfg.extra_allowed_hosts,
                    parse_extra_host_list(&args[i + 1]),
                );
                i += 2;
            }
            arg if arg.starts_with("--extra-host=") => {
                if let Some(val) = arg.strip_prefix("--extra-host=") {
                    extend_unique_hosts(&mut cfg.extra_allowed_hosts, parse_extra_host_list(val));
                }
                i += 1;
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
    if !cli_dns.is_empty() {
        cfg.dns_servers = cli_dns;
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
    let (cfg_raw, pass_from_cli) = load_config_from_args()?;
    let cfg = Arc::new(cfg_raw);
    let listener = TcpListener::bind(&cfg.listen)
        .await
        .with_context(|| format!("Không mở được cổng relay {}", cfg.listen))?;
    let bound_label = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| cfg.listen.clone());
    for warning in startup_security_warnings(&cfg, &bound_label, pass_from_cli) {
        eprintln!("{warning}");
    }
    let dns_label = if cfg.dns_servers.is_empty() {
        "system".to_string()
    } else {
        format!("custom({})", cfg.dns_servers.len())
    };
    eprintln!(
        "litechat-relay đang lắng nghe trên {} (auth: {}, mode: {}, dns: {})",
        cfg.listen,
        if cfg.auth_required() { "bật" } else { "tắt" },
        if cfg.allow_all { "allow_all" } else { "allowlist" },
        dns_label
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

        assert!(is_forbidden_ip("0.0.0.0".parse().unwrap()));
        assert!(is_forbidden_ip("0.0.0.1".parse().unwrap()));
        assert!(is_forbidden_ip("0.255.255.254".parse().unwrap()));
        assert!(is_forbidden_ip("127.0.0.1".parse().unwrap()));
        assert!(is_forbidden_ip("10.7.12.233".parse().unwrap()));
        assert!(is_forbidden_ip("100.64.0.1".parse().unwrap()));
        assert!(is_forbidden_ip("192.168.1.1".parse().unwrap()));
        assert!(is_forbidden_ip("169.254.169.254".parse().unwrap()));
        assert!(is_forbidden_ip("::1".parse().unwrap()));
        assert!(is_forbidden_ip("fd00::1".parse().unwrap()));
        assert!(is_forbidden_ip("fe80::1".parse().unwrap()));
        assert!(is_forbidden_ip("::ffff:0.0.0.1".parse().unwrap()));
        assert!(is_forbidden_ip("::ffff:127.0.0.1".parse().unwrap()));
        assert!(is_forbidden_ip("::ffff:10.7.12.233".parse().unwrap()));
        assert!(is_forbidden_ip("::ffff:169.254.169.254".parse().unwrap()));
        assert!(is_forbidden_ip("64:ff9b::127.0.0.1".parse().unwrap()));
        assert!(is_forbidden_ip("64:ff9b::10.0.0.1".parse().unwrap()));
        assert!(is_forbidden_ip("64:ff9b::1.1.1.1".parse().unwrap()));
        assert!(is_forbidden_ip("64:ff9b:1::10.0.0.1".parse().unwrap()));
        assert!(!is_forbidden_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_forbidden_ip("2606:4700:4700::1111".parse().unwrap()));
        assert!(!is_forbidden_ip("64:ff9c::1".parse().unwrap()));

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

    #[test]
    fn dns_config_normalization_query_and_response_parsing() {
        assert_eq!(
            normalize_dns_server("8.8.8.8"),
            Some("8.8.8.8:53".parse().unwrap())
        );
        assert_eq!(
            normalize_dns_server("1.1.1.1:5353"),
            Some("1.1.1.1:5353".parse().unwrap())
        );
        assert_eq!(
            normalize_dns_server("[::1]"),
            Some("[::1]:53".parse().unwrap())
        );
        assert_eq!(normalize_dns_server("not-an-ip"), None);
        assert_eq!(
            parse_dns_server_list(" 8.8.8.8 , 1.1.1.1:53 , "),
            vec!["8.8.8.8".to_string(), "1.1.1.1:53".to_string()]
        );

        let q = build_dns_query("chat.zalo.me", 1, 0x1234).unwrap();
        assert_eq!(&q[0..2], &[0x12, 0x34]);
        assert_eq!(&q[12..26], b"\x04chat\x04zalo\x02me\x00");
        assert!(build_dns_query("", 1, 1).is_err());
        assert!(build_dns_query("bad..host", 1, 1).is_err());

        // Craft DNS response containing: 1 Question (chat.zalo.me) + CNAME (compressed pointer) + two A records
        let mut resp = q.clone();
        resp[2] = 0x81; // QR=1, RD=1
        resp[3] = 0x80; // RA=1, RCODE=0
        resp[6..8].copy_from_slice(&3u16.to_be_bytes()); // ANCOUNT = 3
        // Answer 1: CNAME -> alias.zalo.me (TTL 120)
        resp.extend_from_slice(&[0xC0, 0x0C]); // compressed name -> offset 12
        resp.extend_from_slice(&5u16.to_be_bytes()); // CNAME
        resp.extend_from_slice(&1u16.to_be_bytes()); // IN
        resp.extend_from_slice(&120u32.to_be_bytes());
        let cname_target = b"\x05alias\xC0\x11"; // "alias." + pointer to "zalo.me"
        resp.extend_from_slice(&(cname_target.len() as u16).to_be_bytes());
        resp.extend_from_slice(cname_target);
        // Answer 2: A 49.213.95.122 (TTL 45)
        resp.extend_from_slice(&[0xC0, 0x0C]);
        resp.extend_from_slice(&1u16.to_be_bytes()); // A
        resp.extend_from_slice(&1u16.to_be_bytes()); // IN
        resp.extend_from_slice(&45u32.to_be_bytes());
        resp.extend_from_slice(&4u16.to_be_bytes());
        resp.extend_from_slice(&[49, 213, 95, 122]);
        // Answer 3: A 49.213.95.137 (TTL 90)
        resp.extend_from_slice(&[0xC0, 0x0C]);
        resp.extend_from_slice(&1u16.to_be_bytes()); // A
        resp.extend_from_slice(&1u16.to_be_bytes()); // IN
        resp.extend_from_slice(&90u32.to_be_bytes());
        resp.extend_from_slice(&4u16.to_be_bytes());
        resp.extend_from_slice(&[49, 213, 95, 137]);

        let (ips, min_ttl) = parse_dns_response(&resp, 0x1234, "chat.zalo.me", 1).unwrap();
        assert_eq!(min_ttl, 45);
        assert_eq!(
            ips,
            vec![
                "49.213.95.122".parse::<IpAddr>().unwrap(),
                "49.213.95.137".parse::<IpAddr>().unwrap(),
            ]
        );
        // Case-insensitive + trailing dot QNAME still matches cleanly
        assert!(parse_dns_response(&resp, 0x1234, "CHAT.ZALO.ME.", 1).is_ok());

        // Mismatched TXID, truncated packet, mismatched QNAME, mismatched QTYPE, and bad QDCOUNT must fail
        assert!(parse_dns_response(&resp, 0x9999, "chat.zalo.me", 1).is_err());
        assert!(parse_dns_response(&resp[..8], 0x1234, "chat.zalo.me", 1).is_err());
        assert!(parse_dns_response(&resp, 0x1234, "other.zalo.me", 1).is_err());
        assert!(parse_dns_response(&resp, 0x1234, "chat.zalo.me", 28).is_err());

        let mut bad_qdcount = resp.clone();
        bad_qdcount[4..6].copy_from_slice(&0u16.to_be_bytes());
        assert!(parse_dns_response(&bad_qdcount, 0x1234, "chat.zalo.me", 1).is_err());

        let mut bad_qclass = resp.clone();
        // QCLASS sits immediately after QNAME (offset 26) + QTYPE (2 bytes) = offset 28..30
        bad_qclass[28..30].copy_from_slice(&3u16.to_be_bytes());
        assert!(parse_dns_response(&bad_qclass, 0x1234, "chat.zalo.me", 1).is_err());
    }

    #[test]
    fn dns_txid_uses_non_sequential_randomness_and_startup_warnings_work() {
        let draws: Vec<u16> = (0..16).map(|_| generate_dns_txid()).collect();
        let all_same = draws.windows(2).all(|w| w[0] == w[1]);
        let all_seq = draws
            .windows(2)
            .all(|w| w[1] == w[0].wrapping_add(1));
        assert!(!all_same, "DNS TXID must not be constant: {draws:?}");
        assert!(!all_seq, "DNS TXID must not be sequential: {draws:?}");

        // LITECHAT_RELAY_PASS sets auth without needing --pass CLI flag
        let args_env = vec![
            "litechat-relay".to_string(),
            "--allow-all".to_string(),
            "--listen".to_string(),
            "0.0.0.0:1088".to_string(),
        ];
        assert!(!cli_used_pass_flag(&args_env));
        let cfg_env = parse_config_with(&args_env, |k| match k {
            "LITECHAT_RELAY_USER" => Some("relay_user".to_string()),
            "LITECHAT_RELAY_PASS" => Some("env_secret_pw".to_string()),
            _ => None,
        })
        .unwrap();
        assert!(cfg_env.auth_required());
        assert!(cfg_env.verify_credentials("relay_user", "env_secret_pw"));
        assert!(startup_security_warnings(&cfg_env, &cfg_env.listen, false).is_empty());

        // --allow-all on non-loopback without auth triggers open-proxy warning
        let cfg_open = parse_config_with(&args_env, |_| None).unwrap();
        let open_warns = startup_security_warnings(&cfg_open, "0.0.0.0:1088", false);
        assert_eq!(open_warns.len(), 1);
        assert!(open_warns[0].contains("--allow-all"));
        assert!(open_warns[0].contains("LITECHAT_RELAY_PASS"));

        // Loopback binds do not trigger non-loopback open-proxy warning
        assert!(is_loopback_bind("127.0.0.1:1088"));
        assert!(is_loopback_bind("[::1]:1088"));
        assert!(is_loopback_bind("localhost:1088"));
        assert!(!is_loopback_bind("0.0.0.0:1088"));
        assert!(!is_loopback_bind("[::]:1088"));
        assert!(!is_loopback_bind("10.7.12.233:1088"));
        assert!(startup_security_warnings(&cfg_open, "127.0.0.1:1088", false).is_empty());

        // --pass CLI flag emits warning advising LITECHAT_RELAY_PASS
        let args_cli_pass = vec![
            "litechat-relay".to_string(),
            "--user".to_string(),
            "u".to_string(),
            "--pass".to_string(),
            "cli_pw".to_string(),
        ];
        assert!(cli_used_pass_flag(&args_cli_pass));
        let cfg_cli = parse_config_with(&args_cli_pass, |_| None).unwrap();
        assert!(cfg_cli.verify_credentials("u", "cli_pw"));
        let pass_warns = startup_security_warnings(&cfg_cli, &cfg_cli.listen, true);
        assert_eq!(pass_warns.len(), 1);
        assert!(pass_warns[0].contains("--pass"));
        assert!(pass_warns[0].contains("LITECHAT_RELAY_PASS"));
    }

    #[tokio::test]
    async fn udp_dns_resolves_from_mock_server() {
        let dns_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let dns_addr = dns_sock.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            if let Ok((n, peer)) = dns_sock.recv_from(&mut buf).await {
                if n >= 12 {
                    let mut reply = buf[..n].to_vec();
                    reply[2] = 0x81;
                    reply[3] = 0x80;
                    reply[6..8].copy_from_slice(&1u16.to_be_bytes());
                    reply.extend_from_slice(&[0xC0, 0x0C]);
                    reply.extend_from_slice(&1u16.to_be_bytes());
                    reply.extend_from_slice(&1u16.to_be_bytes());
                    reply.extend_from_slice(&15u32.to_be_bytes()); // TTL 15 -> clamped to min 30
                    reply.extend_from_slice(&4u16.to_be_bytes());
                    reply.extend_from_slice(&[49, 213, 95, 122]);
                    let _ = dns_sock.send_to(&reply, peer).await;
                }
            }
        });

        let (ips, ttl) =
            resolve_via_udp_dns("chat.zalo.me", &[dns_addr.to_string()]).await.unwrap();
        assert_eq!(ips, vec!["49.213.95.122".parse::<IpAddr>().unwrap()]);
        assert_eq!(ttl, Duration::from_secs(DNS_TTL_MIN_SECS as u64));
    }

    #[test]
    fn relay_allowlist_syncs_official_domains_and_parses_extra_hosts() {
        let cfg = RelayServerConfig::default();
        for host in [
            "chat.zalo.me",
            "api.zaloapp.com",
            "f10-zpc.zdn.vn",
            "s120-ava-talk.zadn.vn",
            "static.zalo.vn",
            "photo.zalo.com",
            "files.zalo.cloud",
            "dl.zbox.vn",
            "cdn.zalo.video",
            "assets.zapps.me",
            "stc.zaloplatforms.com",
            "mp3.zing.vn",
            "file-stal-9.dlfl.vn",
            "file-stal-1.dlfl.me",
            "file-stal-9-te-vnso-pt-25.flchat.vn",
            "video-stal-20.dlmd.me",
            "video-stal-20-te-vnno-cm-1.mdchat.me",
        ] {
            assert!(is_allowed_target(host, 443, &cfg), "expected 443 allowed: {host}");
            assert!(!is_allowed_target(host, 80, &cfg), "expected 80 blocked: {host}");
        }
        for blocked in [
            "cdn.zalocdn.com",
            "api.zaloapi.com",
            "bucket.vngcloud.vn",
            "cdn.vcdn.vn",
            "cdn.vcdn.com.vn",
        ] {
            assert!(
                !is_allowed_target(blocked, 443, &cfg),
                "expected blocked: {blocked}"
            );
        }

        let args = vec![
            "litechat-relay".to_string(),
            "--extra-host".to_string(),
            "media.tenor.com, *.extra-a.example.".to_string(),
            "--extra-host".to_string(),
            "extra-b.example".to_string(),
            "--extra-host=.extra-c.example,media.tenor.com".to_string(),
        ];
        let parsed = parse_config_with(&args, |key| {
            if key == "LITECHAT_RELAY_EXTRA_HOSTS" {
                Some("env-a.example, *.env-b.example".to_string())
            } else {
                None
            }
        })
        .unwrap();
        assert_eq!(
            parsed.extra_allowed_hosts,
            vec![
                "env-a.example",
                "env-b.example",
                "media.tenor.com",
                "extra-a.example",
                "extra-b.example",
                "extra-c.example",
            ]
        );
        assert!(is_allowed_target("c.media.tenor.com", 443, &parsed));
        assert!(is_allowed_target("cdn.env-b.example", 443, &parsed));
        assert!(!is_allowed_target("cdn.env-b.example", 80, &parsed));
    }

    #[test]
    fn constant_time_eq_rejects_length_mismatch_mod_256_and_debug_redacts_password() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"secret", b"secreT"));
        assert!(!constant_time_eq(
            b"secret",
            &[b"secret".as_slice(), &[0u8; 256]].concat()
        ));
        assert!(!constant_time_eq(
            &[b"secret".as_slice(), &[0u8; 256]].concat(),
            b"secret"
        ));

        let cfg = RelayServerConfig {
            username: "lite".into(),
            password: "relay_super_secret_pw".into(),
            ..Default::default()
        };
        assert!(cfg.verify_credentials("lite", "relay_super_secret_pw"));
        let padded_pw = format!("{}{}", cfg.password, "\0".repeat(256));
        assert!(!cfg.verify_credentials("lite", &padded_pw));

        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("relay_super_secret_pw"));
        assert!(dbg.contains("[REDACTED]"));
        assert!(dbg.contains("lite"));
    }

    #[tokio::test]
    async fn http_connect_rejects_oversized_unterminated_line_and_headers() {
        let cfg = RelayServerConfig {
            username: "lite".into(),
            password: "secret".into(),
            allow_all: false,
            allow_private_targets: true,
            ..Default::default()
        };
        let (_, relay_addr) = spawn_echo_and_relay(cfg).await;

        // 1. Oversized request line without '\n' rejected before auth with 431
        let mut s_req = TcpStream::connect(relay_addr).await.unwrap();
        let huge_req_line = vec![b'A'; MAX_HTTP_HEADER_BYTES + 64];
        s_req.write_all(&huge_req_line).await.unwrap();
        let mut buf = [0u8; 256];
        let n = tokio::time::timeout(Duration::from_secs(3), s_req.read(&mut buf))
            .await
            .expect("server must reject oversized request line immediately")
            .unwrap();
        let resp = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(resp.contains("431"), "expected 431, got: {resp}");

        // 2. Valid request line followed by oversized header without '\n' rejected with 431
        let mut s_hdr = TcpStream::connect(relay_addr).await.unwrap();
        let mut payload = b"CONNECT chat.zalo.me:443 HTTP/1.1\r\nX-Huge: ".to_vec();
        payload.extend(std::iter::repeat_n(b'B', MAX_HTTP_HEADER_BYTES));
        s_hdr.write_all(&payload).await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(3), s_hdr.read(&mut buf))
            .await
            .expect("server must reject oversized header line immediately")
            .unwrap();
        let resp = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(resp.contains("431"), "expected 431, got: {resp}");

        // 3. Multiple headers <= 64 count but > MAX_HTTP_HEADER_BYTES total rejected with 431
        let mut s_multi = TcpStream::connect(relay_addr).await.unwrap();
        let mut multi = b"CONNECT chat.zalo.me:443 HTTP/1.1\r\n".to_vec();
        let one_hdr = format!("X-Pad: {}\r\n", "C".repeat(500));
        for _ in 0..17 {
            multi.extend_from_slice(one_hdr.as_bytes());
        }
        s_multi.write_all(&multi).await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(3), s_multi.read(&mut buf))
            .await
            .expect("server must reject oversized multi-header block immediately")
            .unwrap();
        let resp = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(resp.contains("431"), "expected 431, got: {resp}");
    }
}
