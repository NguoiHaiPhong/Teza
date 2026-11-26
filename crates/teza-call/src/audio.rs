//! Audio capture/playback (`cpal`), 16 kHz mono resampling, pure-Rust Opus (`opus-pure`),
//! and UDP RTP/RTCP media pipeline with jitter buffering and PLC/FEC concealment.

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use opus_pure::{Application, OpusDecoder, OpusEncoder, SoftClip, MAX_PACKET_BYTES};
use serde::{Deserialize, Serialize};

use crate::error::{CallError, CallResult};
use crate::media_hook::AuxiliaryMediaSession;
use crate::rtp::{
    build_rtcp_rr, build_rtp_packet, is_rtcp_packet, parse_rtp_packet, JitterBuffer, JitterPop,
    RtpRxStats, AUDIO_FRAME_SAMPLES, AUDIO_SAMPLE_RATE, OPUS_AUDIO_PT,
};
use crate::zrtp::{
    parse_zrtp_init_response, unwrap_relay_media, wrap_relay_audio_rtcp, wrap_relay_audio_rtp,
    RELAY_INCOMING_VIDEO_RTP, RELAY_WRAP_AUDIO_RTCP, RELAY_WRAP_AUDIO_RTP, RELAY_WRAP_VIDEO_RTCP,
    RELAY_WRAP_VIDEO_RTP,
};

/// Default VoIP Opus target bitrate (24 kbps mono wideband speech).
pub const DEFAULT_OPUS_BITRATE_BPS: i32 = 24_000;

/// Maximum buffered capture/playback samples at 16 kHz (~500 ms cap to keep call latency tight).
const MAX_RING_SAMPLES: usize = AUDIO_FRAME_SAMPLES * 25;

/// Audio hardware device descriptor returned by device enumeration APIs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub is_default: bool,
    #[serde(default)]
    pub is_virtual: bool,
}

/// Returns `true` when `name` looks like a virtual audio cable, loopback mix, or monitor sink
/// that produces two-way silence if auto-selected instead of a real physical headset/mic.
pub fn is_virtual_or_loopback_audio_device(name: &str) -> bool {
    let low = name.trim().to_ascii_lowercase();
    if low.is_empty() {
        return false;
    }
    const PATTERNS: &[&str] = &[
        "vb-audio",
        "virtual cable",
        "cable input",
        "cable output",
        "cable in ",
        "cable out ",
        "stereo mix",
        "what u hear",
        "wave out mix",
        "loopback",
        "monitor of ",
        "blackhole",
        "soundflower",
        "virtual audio",
    ];
    PATTERNS.iter().any(|pat| low.contains(pat))
}

/// Returns `true` when an output name looks like a raw S/PDIF optical/coax port rather than
/// analog speakers or headphones, so automatic fallback avoids picking it over real speakers.
pub fn is_spdif_digital_output(name: &str) -> bool {
    let low = name.trim().to_ascii_lowercase();
    if low.is_empty() {
        return false;
    }
    ["digital output", "s/pdif", "spdif", "iec958"]
        .iter()
        .any(|pat| low.contains(pat))
}

/// Resolve the best device name given an explicit user preference, the OS default, and the
/// enumerated host device list.
///
/// Always honors an explicit non-empty `preferred` selection (even if virtual). When using OS
/// default (`preferred == None`), avoids virtual/loopback OS defaults whenever a physical device
/// is available (and prefers analog speakers/headphones over S/PDIF digital outputs).
pub(crate) fn pick_preferred_device_name(
    preferred: Option<&str>,
    default_name: Option<&str>,
    candidates: &[String],
    avoid_spdif_on_auto: bool,
) -> Option<String> {
    if let Some(pref) = preferred.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(matched) = candidates.iter().find(|c| c.trim().eq_ignore_ascii_case(pref)) {
            return Some(matched.clone());
        }
    }

    let clean_default = default_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(ref def) = clean_default {
        if !is_virtual_or_loopback_audio_device(def) {
            return Some(def.clone());
        }
    }

    if let Some(phys) = candidates.iter().find(|c| {
        !is_virtual_or_loopback_audio_device(c)
            && (!avoid_spdif_on_auto || !is_spdif_digital_output(c))
    }) {
        return Some(phys.clone());
    }

    if avoid_spdif_on_auto {
        if let Some(phys) = candidates
            .iter()
            .find(|c| !is_virtual_or_loopback_audio_device(c))
        {
            return Some(phys.clone());
        }
    }

    clean_default.or_else(|| candidates.first().cloned())
}

/// Phase-preserving linear resampler for streaming PCM callbacks.
#[derive(Debug, Clone)]
pub struct LinearResampler {
    ratio: f64,
    buf: Vec<f32>,
    pos: f64,
}

impl LinearResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let in_r = in_rate.max(1) as f64;
        let out_r = out_rate.max(1) as f64;
        Self {
            ratio: in_r / out_r,
            buf: Vec::new(),
            pos: 0.0,
        }
    }

    /// Push mono input frames at `in_rate` and return all complete resampled frames at `out_rate`.
    pub fn push(&mut self, frames: &[f32]) -> Vec<f32> {
        if frames.is_empty() {
            return Vec::new();
        }
        if (self.ratio - 1.0).abs() < 1e-9 && self.buf.is_empty() && self.pos == 0.0 {
            return frames.to_vec();
        }
        self.buf.extend_from_slice(frames);
        let len = self.buf.len();
        let est = ((frames.len() as f64) / self.ratio).ceil().max(1.0) as usize + 2;
        let mut out = Vec::with_capacity(est);
        while self.pos + 1.0 < len as f64 {
            let i0 = self.pos as usize;
            let frac = (self.pos - i0 as f64) as f32;
            let a = self.buf[i0];
            let b = self.buf[i0 + 1];
            out.push(a + (b - a) * frac);
            self.pos += self.ratio;
        }
        let consumed = (self.pos as usize).min(self.buf.len());
        self.buf.drain(..consumed);
        self.pos -= consumed as f64;
        out
    }

    /// Drain remaining tail samples on stream teardown.
    pub fn flush(&mut self) -> Vec<f32> {
        let mut out = Vec::new();
        let len = self.buf.len();
        while self.pos < len as f64 {
            out.push(self.buf[(self.pos as usize).min(len.saturating_sub(1))]);
            self.pos += self.ratio;
        }
        self.buf.clear();
        self.pos = 0.0;
        out
    }
}

/// Pure-Rust 16 kHz mono Opus VoIP encoder + decoder with in-band FEC, PLC, and soft clipping.
pub struct OpusVoiceCodec {
    encoder: OpusEncoder,
    decoder: OpusDecoder,
    soft_clip: SoftClip,
    packet_scratch: Vec<u8>,
}

impl OpusVoiceCodec {
    pub fn new() -> CallResult<Self> {
        let mut encoder = OpusEncoder::new(AUDIO_SAMPLE_RATE as i32, 1, Application::Voip)
            .map_err(|e| CallError::Audio(format!("Không tạo được Opus encoder: {e}")))?;
        encoder.bitrate_bps = DEFAULT_OPUS_BITRATE_BPS;
        encoder.use_inband_fec = true;
        encoder.packet_loss_perc = 10;

        let decoder = OpusDecoder::new(AUDIO_SAMPLE_RATE as i32, 1)
            .map_err(|e| CallError::Audio(format!("Không tạo được Opus decoder: {e}")))?;

        Ok(Self {
            encoder,
            decoder,
            soft_clip: SoftClip::new(1),
            packet_scratch: vec![0u8; MAX_PACKET_BYTES],
        })
    }

    /// Encode one 20 ms frame (320 samples at 16 kHz mono) into a raw Opus packet.
    pub fn encode_frame(&mut self, pcm_320: &[f32]) -> CallResult<Vec<u8>> {
        if pcm_320.len() != AUDIO_FRAME_SAMPLES {
            return Err(CallError::InvalidArgument(format!(
                "Opus frame cần đúng {AUDIO_FRAME_SAMPLES} mẫu (nhận {})",
                pcm_320.len()
            )));
        }
        let n = self
            .encoder
            .encode(pcm_320, AUDIO_FRAME_SAMPLES, &mut self.packet_scratch)
            .map_err(|e| CallError::Audio(format!("Opus encode lỗi: {e}")))?;
        Ok(self.packet_scratch[..n].to_vec())
    }

    /// Decode a received Opus packet into 16 kHz mono PCM (soft-clipped to `[-1.0, 1.0]`).
    pub fn decode_frame(&mut self, packet: &[u8]) -> CallResult<Vec<f32>> {
        if packet.is_empty() {
            return self.decode_plc();
        }
        let mut out = vec![0.0_f32; AUDIO_FRAME_SAMPLES * 6]; // up to 120 ms packet safety
        let decoded_samples = self
            .decoder
            .decode(packet, out.len(), &mut out)
            .map_err(|e| CallError::Audio(format!("Opus decode lỗi: {e}")))?;
        out.truncate(decoded_samples);
        self.soft_clip.apply(&mut out);
        Ok(out)
    }

    /// Run Opus Packet Loss Concealment (PLC) for one missing 20 ms frame.
    pub fn decode_plc(&mut self) -> CallResult<Vec<f32>> {
        let mut out = vec![0.0_f32; AUDIO_FRAME_SAMPLES];
        let decoded_samples = self
            .decoder
            .decode(&[], AUDIO_FRAME_SAMPLES, &mut out)
            .map_err(|e| CallError::Audio(format!("Opus PLC lỗi: {e}")))?;
        out.truncate(decoded_samples);
        self.soft_clip.apply(&mut out);
        Ok(out)
    }

    /// Attempt in-band FEC recovery using the subsequent packet; fall back to PLC if unavailable.
    pub fn decode_fec_or_plc(&mut self, next_packet: &[u8]) -> CallResult<Vec<f32>> {
        let mut out = vec![0.0_f32; AUDIO_FRAME_SAMPLES];
        match self
            .decoder
            .decode_fec(next_packet, AUDIO_FRAME_SAMPLES, &mut out)
        {
            Ok(n) if n > 0 => {
                out.truncate(n);
                self.soft_clip.apply(&mut out);
                Ok(out)
            }
            _ => self.decode_plc(),
        }
    }
}

/// Execute a `cpal` host/device enumeration or selection closure on a dedicated persistent
/// Windows STA thread so `cpal 0.15`'s global `static ENUMERATOR` (`IMMDeviceEnumerator`) is
/// always created and accessed from the same COM apartment and `CoUninitialize()` never tears
/// down COM / unloads `MMDevApi.dll` out from under the cached enumerator when ephemeral worker
/// or test threads exit (which otherwise triggers `0xc0000005 STATUS_ACCESS_VIOLATION` on headless
/// Windows CI runners and after blocking-pool thread expiry).
#[cfg(target_os = "windows")]
fn run_on_audio_host_thread<T, F>(f: F) -> CallResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> CallResult<T> + Send + 'static,
{
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{mpsc, OnceLock};

    type HostJob = Box<dyn FnOnce() + Send + 'static>;
    static HOST_TX: OnceLock<mpsc::Sender<HostJob>> = OnceLock::new();

    let tx = HOST_TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<HostJob>();
        thread::Builder::new()
            .name("teza-wasapi-host".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job();
                }
            })
            .expect("failed to spawn teza-wasapi-host thread");
        tx
    });

    let (res_tx, res_rx) = mpsc::sync_channel::<CallResult<T>>(1);
    tx.send(Box::new(move || {
        let res = match catch_unwind(AssertUnwindSafe(f)) {
            Ok(r) => r,
            Err(_) => Err(CallError::Audio(
                "Lỗi nội bộ khi truy vấn thiết bị âm thanh hệ thống".to_string(),
            )),
        };
        let _ = res_tx.send(res);
    }))
    .map_err(|_| CallError::Audio("Luồng quản lý thiết bị âm thanh đã dừng".to_string()))?;

    res_rx.recv().map_err(|_| {
        CallError::Audio("Không nhận được phản hồi từ luồng thiết bị âm thanh".to_string())
    })?
}

#[cfg(not(target_os = "windows"))]
fn run_on_audio_host_thread<T, F>(f: F) -> CallResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> CallResult<T> + Send + 'static,
{
    f()
}

/// Enumerate available microphone (input) devices on the default OS audio host.
pub fn list_input_devices() -> CallResult<Vec<AudioDeviceInfo>> {
    run_on_audio_host_thread(|| {
        let host = cpal::default_host();
        let default_name = host
            .default_input_device()
            .and_then(|d| d.name().ok())
            .map(|s| s.trim().to_string());
        let devices = host
            .input_devices()
            .map_err(|e| CallError::Audio(format!("Không liệt kê được thiết bị micro: {e}")))?;
        let mut out = Vec::new();
        for dev in devices {
            if let Ok(name) = dev.name() {
                let clean = name.trim().to_string();
                if !clean.is_empty() {
                    let is_default = default_name.as_deref() == Some(clean.as_str());
                    let is_virtual = is_virtual_or_loopback_audio_device(&clean);
                    out.push(AudioDeviceInfo {
                        name: clean,
                        is_default,
                        is_virtual,
                    });
                }
            }
        }
        Ok(out)
    })
}

/// Enumerate available speaker (output) devices on the default OS audio host.
pub fn list_output_devices() -> CallResult<Vec<AudioDeviceInfo>> {
    run_on_audio_host_thread(|| {
        let host = cpal::default_host();
        let default_name = host
            .default_output_device()
            .and_then(|d| d.name().ok())
            .map(|s| s.trim().to_string());
        let devices = host
            .output_devices()
            .map_err(|e| CallError::Audio(format!("Không liệt kê được thiết bị loa: {e}")))?;
        let mut out = Vec::new();
        for dev in devices {
            if let Ok(name) = dev.name() {
                let clean = name.trim().to_string();
                if !clean.is_empty() {
                    let is_default = default_name.as_deref() == Some(clean.as_str());
                    let is_virtual = is_virtual_or_loopback_audio_device(&clean);
                    out.push(AudioDeviceInfo {
                        name: clean,
                        is_default,
                        is_virtual,
                    });
                }
            }
        }
        Ok(out)
    })
}

/// Thread-safe live audio controls, meters, and transport statistics.
#[derive(Debug)]
pub struct AudioControls {
    mic_muted: AtomicBool,
    has_input_device: AtomicBool,
    mic_gain_percent: AtomicU32,
    mic_level_permille: AtomicU32,
    remote_level_permille: AtomicU32,
    remote_speaking: AtomicBool,
    tx_packets: AtomicU64,
    rx_packets: AtomicU64,
    plc_frames: AtomicU64,
    fec_frames: AtomicU64,
    tx_errors: AtomicU64,
    active_input_device: Mutex<Option<String>>,
    active_output_device: Mutex<Option<String>>,
}

impl Default for AudioControls {
    fn default() -> Self {
        Self {
            mic_muted: AtomicBool::new(false),
            has_input_device: AtomicBool::new(false),
            mic_gain_percent: AtomicU32::new(100),
            mic_level_permille: AtomicU32::new(0),
            remote_level_permille: AtomicU32::new(0),
            remote_speaking: AtomicBool::new(false),
            tx_packets: AtomicU64::new(0),
            rx_packets: AtomicU64::new(0),
            plc_frames: AtomicU64::new(0),
            fec_frames: AtomicU64::new(0),
            tx_errors: AtomicU64::new(0),
            active_input_device: Mutex::new(None),
            active_output_device: Mutex::new(None),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioTelemetry {
    pub mic_muted: bool,
    pub has_input_device: bool,
    pub mic_gain_percent: u32,
    pub mic_level_permille: u32,
    pub remote_level_permille: u32,
    pub remote_speaking: bool,
    pub tx_packets: u64,
    pub rx_packets: u64,
    pub plc_frames: u64,
    pub fec_frames: u64,
    pub tx_errors: u64,
    #[serde(default)]
    pub active_input_device: Option<String>,
    #[serde(default)]
    pub active_output_device: Option<String>,
    #[serde(default)]
    pub input_is_virtual: bool,
    #[serde(default)]
    pub output_is_virtual: bool,
}

impl AudioControls {
    pub fn set_active_devices(&self, input: Option<String>, output: Option<String>) {
        if let Ok(mut g) = self.active_input_device.lock() {
            *g = input.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        }
        if let Ok(mut g) = self.active_output_device.lock() {
            *g = output.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        }
    }

    pub fn set_mic_muted(&self, muted: bool) {
        self.mic_muted.store(muted, Ordering::Relaxed);
        if muted {
            self.mic_level_permille.store(0, Ordering::Relaxed);
        }
    }

    pub fn is_mic_muted(&self) -> bool {
        self.mic_muted.load(Ordering::Relaxed)
    }

    pub fn has_input_device(&self) -> bool {
        self.has_input_device.load(Ordering::Relaxed)
    }

    pub fn set_has_input_device(&self, has_input: bool) {
        self.has_input_device.store(has_input, Ordering::Relaxed);
        if !has_input {
            self.mic_level_permille.store(0, Ordering::Relaxed);
        }
    }

    pub fn set_mic_gain_percent(&self, pct: u32) {
        self.mic_gain_percent
            .store(pct.clamp(0, 200), Ordering::Relaxed);
    }

    pub fn mic_gain_percent(&self) -> u32 {
        self.mic_gain_percent.load(Ordering::Relaxed)
    }

    pub fn mic_level_permille(&self) -> u32 {
        self.mic_level_permille.load(Ordering::Relaxed)
    }

    pub fn remote_level_permille(&self) -> u32 {
        self.remote_level_permille.load(Ordering::Relaxed)
    }

    pub fn is_remote_speaking(&self) -> bool {
        self.remote_speaking.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> AudioTelemetry {
        let active_input_device = self
            .active_input_device
            .lock()
            .ok()
            .and_then(|g| g.clone());
        let active_output_device = self
            .active_output_device
            .lock()
            .ok()
            .and_then(|g| g.clone());
        let input_is_virtual = active_input_device
            .as_deref()
            .is_some_and(is_virtual_or_loopback_audio_device);
        let output_is_virtual = active_output_device
            .as_deref()
            .is_some_and(is_virtual_or_loopback_audio_device);
        AudioTelemetry {
            mic_muted: self.is_mic_muted(),
            has_input_device: self.has_input_device(),
            mic_gain_percent: self.mic_gain_percent(),
            mic_level_permille: self.mic_level_permille(),
            remote_level_permille: self.remote_level_permille(),
            remote_speaking: self.is_remote_speaking(),
            tx_packets: self.tx_packets.load(Ordering::Relaxed),
            rx_packets: self.rx_packets.load(Ordering::Relaxed),
            plc_frames: self.plc_frames.load(Ordering::Relaxed),
            fec_frames: self.fec_frames.load(Ordering::Relaxed),
            tx_errors: self.tx_errors.load(Ordering::Relaxed),
            active_input_device,
            active_output_device,
            input_is_virtual,
            output_is_virtual,
        }
    }
}

/// Device selection configuration for starting an audio session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDeviceSelection {
    pub input_device_name: Option<String>,
    pub output_device_name: Option<String>,
}

/// Configuration for [`AudioMediaSession`].
pub struct AudioMediaConfig {
    pub rtp_target: SocketAddr,
    pub rtcp_target: Option<SocketAddr>,
    pub ssrc: u32,
    pub relay_token: u32,
    /// Optional live relay token atomic shared with `ZrtpRelaySession` so late `InitZRTP` acks
    /// dynamically activate 5-byte relay media wrapping mid-call.
    pub live_relay_token: Option<Arc<AtomicU32>>,
    /// Optional per-server live RTCP relay token atomic when `rtcp_target` differs from
    /// `rtp_target` so distinct RTCP ports never clobber the RTP token or wrap RRs with the wrong token.
    pub live_rtcp_relay_token: Option<Arc<AtomicU32>>,
    /// Optional flag shared with `ZrtpRelaySession` indicating the audio thread owns RTP socket reads.
    pub media_attached: Option<Arc<AtomicBool>>,
    /// Optional notifier shared with `ZrtpRelaySession` woken whenever `media_attached` changes
    /// so `keepalive_task` immediately disarms/rearms its RTP socket receiver without contention.
    pub media_attached_notify: Option<Arc<tokio::sync::Notify>>,
    /// Initial microphone mute state applied before the very first frame is transmitted.
    pub initial_mic_muted: bool,
    pub udp_rtp_sock: Arc<UdpSocket>,
    pub udp_rtcp_sock: Option<Arc<UdpSocket>>,
    pub rtcp_inbox: Option<Arc<Mutex<VecDeque<Vec<u8>>>>>,
    pub devices: AudioDeviceSelection,
    /// Optional video/auxiliary media receiver notified for non-Opus RTP & RTCP datagrams.
    pub aux_session: Option<Arc<dyn AuxiliaryMediaSession>>,
}

impl AudioMediaConfig {
    fn active_relay_token(&self) -> u32 {
        self.live_relay_token
            .as_ref()
            .map(|a| a.load(Ordering::Relaxed))
            .filter(|&t| t != 0)
            .unwrap_or(self.relay_token)
    }

    /// Resolve `(relay_token, target_addr, use_rtcp_sock)` for outbound RTCP Receiver Reports.
    ///
    /// Uses the dedicated `udp_rtcp_sock` ONLY when `rtcp_target` is distinct from `rtp_target`
    /// and the RTCP relay port has acknowledged `InitZRTP` (`live_rtcp_relay_token != 0`).
    /// Otherwise sends via `udp_rtp_sock` using the RTP relay token so the source NAT port matches.
    pub(crate) fn resolve_rtcp_send_route(&self) -> (u32, SocketAddr, bool) {
        let target = self.rtcp_target.unwrap_or(self.rtp_target);
        let distinct_rtcp = target != self.rtp_target;
        if distinct_rtcp {
            if let Some(tok) = self
                .live_rtcp_relay_token
                .as_ref()
                .map(|a| a.load(Ordering::Relaxed))
                .filter(|&t| t != 0)
            {
                if self.udp_rtcp_sock.is_some() {
                    return (tok, target, true);
                }
            }
        }
        (self.active_relay_token(), target, false)
    }
}

struct MediaAttachedGuard {
    flag: Option<Arc<AtomicBool>>,
    notify: Option<Arc<tokio::sync::Notify>>,
}

impl MediaAttachedGuard {
    fn acquire(
        flag: Option<Arc<AtomicBool>>,
        notify: Option<Arc<tokio::sync::Notify>>,
    ) -> Self {
        if let Some(ref f) = flag {
            f.store(true, Ordering::Release);
        }
        if let Some(ref n) = notify {
            n.notify_one();
        }
        Self { flag, notify }
    }
}

impl Drop for MediaAttachedGuard {
    fn drop(&mut self) {
        if let Some(ref f) = self.flag {
            f.store(false, Ordering::Release);
        }
        if let Some(ref n) = self.notify {
            n.notify_one();
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TestAudioFault {
    None,
    MissingInput,
    OutputStartFailure,
}

/// Live full-duplex 16 kHz Opus/RTP audio pipeline running on a dedicated OS thread.
///
/// Dropping the handle cleanly stops capture/playback and joins the worker thread.
pub struct AudioMediaSession {
    stop: Arc<AtomicBool>,
    controls: Arc<AudioControls>,
    thread: Option<JoinHandle<()>>,
}

impl AudioMediaSession {
    pub fn start(cfg: AudioMediaConfig) -> CallResult<Self> {
        Self::start_inner(
            cfg,
            #[cfg(test)]
            TestAudioFault::None,
        )
    }

    #[cfg(test)]
    pub(crate) fn start_with_fault(
        cfg: AudioMediaConfig,
        fault: TestAudioFault,
    ) -> CallResult<Self> {
        Self::start_inner(cfg, fault)
    }

    /// Start a lightweight UDP RTP/RTCP RX pump without opening any OS audio hardware or Opus codec.
    ///
    /// Used when audio capture/playback is disabled (`--no-audio`) or unavailable so auxiliary
    /// media sessions (e.g. `teza-video`) and late `InitZRTP` acks still receive datagrams cleanly.
    pub fn start_aux_pump_only(cfg: AudioMediaConfig) -> CallResult<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let controls = Arc::new(AudioControls::default());
        controls.set_mic_muted(cfg.initial_mic_muted);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<CallResult<()>>(1);

        let stop_t = stop.clone();
        let ctrl_t = controls.clone();
        let thread = thread::Builder::new()
            .name("teza-call-aux-pump".to_string())
            .spawn(move || {
                run_aux_pump_only_loop(stop_t, ctrl_t, cfg, ready_tx);
            })
            .map_err(|e| CallError::Audio(format!("Không tạo được luồng UDP aux pump: {e}")))?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                controls,
                thread: Some(thread),
            }),
            Ok(Err(err)) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                Err(err)
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                Err(CallError::Audio(
                    "Hết thời gian khởi tạo luồng UDP aux pump".to_string(),
                ))
            }
        }
    }

    fn start_inner(
        cfg: AudioMediaConfig,
        #[cfg(test)] fault: TestAudioFault,
    ) -> CallResult<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let controls = Arc::new(AudioControls::default());
        controls.set_mic_muted(cfg.initial_mic_muted);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<CallResult<()>>(1);

        let stop_t = stop.clone();
        let ctrl_t = controls.clone();
        let thread = thread::Builder::new()
            .name("teza-call-media".to_string())
            .spawn(move || {
                run_udp_audio_loop(
                    stop_t,
                    ctrl_t,
                    cfg,
                    ready_tx,
                    #[cfg(test)]
                    fault,
                );
            })
            .map_err(|e| CallError::Audio(format!("Không tạo được luồng audio: {e}")))?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                controls,
                thread: Some(thread),
            }),
            Ok(Err(err)) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                Err(err)
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                Err(CallError::Audio(
                    "Hết thời gian khởi tạo thiết bị âm thanh".to_string(),
                ))
            }
        }
    }

    pub fn controls(&self) -> Arc<AudioControls> {
        self.controls.clone()
    }

    pub fn set_mic_muted(&self, muted: bool) {
        self.controls.set_mic_muted(muted);
    }

    pub fn telemetry(&self) -> AudioTelemetry {
        self.controls.snapshot()
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for AudioMediaSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Local mic -> Opus encode -> JitterBuffer (+ optional synthetic packet loss) -> Opus decode -> speaker
/// loopback handle for independent audio testing in `call_lab`.
pub struct AudioLoopbackSession {
    stop: Arc<AtomicBool>,
    controls: Arc<AudioControls>,
    thread: Option<JoinHandle<()>>,
}

impl AudioLoopbackSession {
    pub fn start(devices: AudioDeviceSelection, simulated_loss_pct: u8) -> CallResult<Self> {
        Self::start_with_tone(devices, simulated_loss_pct, None)
    }

    /// Start local loopback with an optional synthetic sine-wave tone (Hz, e.g. `Some(440.0)`)
    /// injected instead of microphone capture — useful for verifying Opus encode/FEC/PLC/decode
    /// and speaker output on silent virtual audio cables.
    pub fn start_with_tone(
        devices: AudioDeviceSelection,
        simulated_loss_pct: u8,
        inject_tone_hz: Option<f32>,
    ) -> CallResult<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let controls = Arc::new(AudioControls::default());
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<CallResult<()>>(1);

        let stop_t = stop.clone();
        let ctrl_t = controls.clone();
        let loss = simulated_loss_pct.min(90);
        let tone_hz = inject_tone_hz.filter(|hz| hz.is_finite() && *hz > 20.0 && *hz < 7_500.0);
        let thread = thread::Builder::new()
            .name("teza-call-loopback".to_string())
            .spawn(move || {
                run_loopback_audio_loop(stop_t, ctrl_t, devices, loss, tone_hz, ready_tx);
            })
            .map_err(|e| CallError::Audio(format!("Không tạo được luồng loopback: {e}")))?;

        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                controls,
                thread: Some(thread),
            }),
            Ok(Err(err)) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                Err(err)
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                Err(CallError::Audio(
                    "Hết thời gian khởi tạo audio loopback".to_string(),
                ))
            }
        }
    }

    pub fn controls(&self) -> Arc<AudioControls> {
        self.controls.clone()
    }

    pub fn telemetry(&self) -> AudioTelemetry {
        self.controls.snapshot()
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for AudioLoopbackSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct AudioStreams {
    _input: Option<cpal::Stream>,
    _output: cpal::Stream,
    capture_16k: Arc<Mutex<VecDeque<f32>>>,
    playback_16k: Arc<Mutex<VecDeque<f32>>>,
    input_device_name: Option<String>,
    output_device_name: String,
}

impl AudioStreams {
    fn has_input_device(&self) -> bool {
        self._input.is_some()
    }
}

fn select_input_device(
    preferred: Option<&str>,
) -> CallResult<(cpal::Device, String, cpal::SupportedStreamConfig)> {
    let preferred = preferred.map(str::to_string);
    run_on_audio_host_thread(move || {
        let host = cpal::default_host();
        let default_dev = host.default_input_device();
        let default_name = default_dev
            .as_ref()
            .and_then(|d| d.name().ok())
            .map(|n| n.trim().to_string());
        let listed: Vec<(String, cpal::Device)> = host
            .input_devices()
            .ok()
            .map(|devs| {
                devs.filter_map(|d| {
                    d.name()
                        .ok()
                        .map(|n| n.trim().to_string())
                        .filter(|n| !n.is_empty())
                        .map(|n| (n, d))
                })
                .collect()
            })
            .unwrap_or_default();
        let names: Vec<String> = listed.iter().map(|(n, _)| n.clone()).collect();
        let (dev, name) = if let Some(chosen_name) = pick_preferred_device_name(
            preferred.as_deref(),
            default_name.as_deref(),
            &names,
            false,
        ) {
            if let Some((_, dev)) = listed
                .into_iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(&chosen_name))
            {
                (dev, chosen_name)
            } else {
                let dev = default_dev.ok_or_else(|| {
                    CallError::Audio("Không tìm thấy micro mặc định trên máy".to_string())
                })?;
                let fallback_name = dev
                    .name()
                    .ok()
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| "Unknown Microphone".to_string());
                (dev, fallback_name)
            }
        } else {
            let dev = default_dev.ok_or_else(|| {
                CallError::Audio("Không tìm thấy micro mặc định trên máy".to_string())
            })?;
            let fallback_name = dev
                .name()
                .ok()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Unknown Microphone".to_string());
            (dev, fallback_name)
        };
        let in_cfg = dev
            .default_input_config()
            .map_err(|e| CallError::Audio(format!("Không đọc được cấu hình micro: {e}")))?;
        Ok((dev, name, in_cfg))
    })
}

fn select_output_device(
    preferred: Option<&str>,
) -> CallResult<(cpal::Device, String, cpal::SupportedStreamConfig)> {
    let preferred = preferred.map(str::to_string);
    run_on_audio_host_thread(move || {
        let host = cpal::default_host();
        let default_dev = host.default_output_device();
        let default_name = default_dev
            .as_ref()
            .and_then(|d| d.name().ok())
            .map(|n| n.trim().to_string());
        let listed: Vec<(String, cpal::Device)> = host
            .output_devices()
            .ok()
            .map(|devs| {
                devs.filter_map(|d| {
                    d.name()
                        .ok()
                        .map(|n| n.trim().to_string())
                        .filter(|n| !n.is_empty())
                        .map(|n| (n, d))
                })
                .collect()
            })
            .unwrap_or_default();
        let names: Vec<String> = listed.iter().map(|(n, _)| n.clone()).collect();
        let (dev, name) = if let Some(chosen_name) = pick_preferred_device_name(
            preferred.as_deref(),
            default_name.as_deref(),
            &names,
            true,
        ) {
            if let Some((_, dev)) = listed
                .into_iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(&chosen_name))
            {
                (dev, chosen_name)
            } else {
                let dev = default_dev.ok_or_else(|| {
                    CallError::Audio("Không tìm thấy loa mặc định trên máy".to_string())
                })?;
                let fallback_name = dev
                    .name()
                    .ok()
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| "Unknown Speaker".to_string());
                (dev, fallback_name)
            }
        } else {
            let dev = default_dev.ok_or_else(|| {
                CallError::Audio("Không tìm thấy loa mặc định trên máy".to_string())
            })?;
            let fallback_name = dev
                .name()
                .ok()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Unknown Speaker".to_string());
            (dev, fallback_name)
        };
        let out_cfg = dev
            .default_output_config()
            .map_err(|e| CallError::Audio(format!("Không đọc được cấu hình loa: {e}")))?;
        Ok((dev, name, out_cfg))
    })
}

fn push_captured_samples<S: Copy>(
    data: &[S],
    channels: usize,
    resampler: &Arc<Mutex<LinearResampler>>,
    ring: &Arc<Mutex<VecDeque<f32>>>,
    to_f32: fn(S) -> f32,
) {
    let ch = channels.max(1);
    let frames = data.len() / ch;
    if frames == 0 {
        return;
    }
    let mut mono = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut sum = 0.0_f32;
        for c in 0..ch {
            sum += to_f32(data[f * ch + c]);
        }
        mono.push(sum / ch as f32);
    }
    let resampled = match resampler.lock() {
        Ok(mut r) => r.push(&mono),
        Err(_) => return,
    };
    if resampled.is_empty() {
        return;
    }
    if let Ok(mut q) = ring.lock() {
        q.extend(resampled);
        if q.len() > MAX_RING_SAMPLES {
            let drop_n = q.len() - MAX_RING_SAMPLES;
            q.drain(..drop_n);
        }
    }
}

fn fill_playback_samples<S: Copy>(
    out_buf: &mut [S],
    channels: usize,
    out_rate: u32,
    playback_16k: &Arc<Mutex<VecDeque<f32>>>,
    upsampler: &Arc<Mutex<LinearResampler>>,
    out_staging: &Arc<Mutex<VecDeque<f32>>>,
    from_f32: fn(f32) -> S,
) {
    let ch = channels.max(1);
    let needed_frames = out_buf.len() / ch;
    if needed_frames == 0 {
        return;
    }

    let mut stage = match out_staging.lock() {
        Ok(g) => g,
        Err(_) => {
            for s in out_buf.iter_mut() {
                *s = from_f32(0.0);
            }
            return;
        }
    };

    if stage.len() < needed_frames {
        let pulled: Vec<f32> = if let Ok(mut q) = playback_16k.lock() {
            let take_n = q.len().min(AUDIO_FRAME_SAMPLES * 4);
            q.drain(..take_n).collect()
        } else {
            Vec::new()
        };
        if !pulled.is_empty() {
            if let Ok(mut up) = upsampler.lock() {
                stage.extend(up.push(&pulled));
            }
        }
    }

    for f in 0..needed_frames {
        let sample = stage.pop_front().unwrap_or(0.0).clamp(-1.0, 1.0);
        let converted = from_f32(sample);
        for c in 0..ch {
            out_buf[f * ch + c] = converted;
        }
    }

    let max_stage = (out_rate as usize / 2).max(MAX_RING_SAMPLES * 3);
    if stage.len() > max_stage {
        let drop_n = stage.len() - max_stage;
        stage.drain(..drop_n);
    }
}

fn f32_to_i16(v: f32) -> i16 {
    (v.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

fn f32_to_i32(v: f32) -> i32 {
    (f64::from(v.clamp(-1.0, 1.0)) * f64::from(i32::MAX)).round() as i32
}

fn f32_to_u16(v: f32) -> u16 {
    ((v.clamp(-1.0, 1.0) * 32767.0) + 32768.0).round() as u16
}

fn try_open_input_stream(
    devices: &AudioDeviceSelection,
    capture_16k: &Arc<Mutex<VecDeque<f32>>>,
) -> CallResult<(cpal::Stream, String)> {
    let (input_dev, input_name, in_cfg) =
        select_input_device(devices.input_device_name.as_deref())?;

    let in_rate = in_cfg.sample_rate().0.max(8_000);
    let in_channels = (in_cfg.channels() as usize).max(1);
    let in_fmt = in_cfg.sample_format();
    let in_stream_cfg: cpal::StreamConfig = in_cfg.into();

    let in_resampler = Arc::new(Mutex::new(LinearResampler::new(in_rate, AUDIO_SAMPLE_RATE)));
    let in_ring = capture_16k.clone();
    let input_stream = match in_fmt {
        cpal::SampleFormat::F32 => input_dev.build_input_stream(
            &in_stream_cfg,
            move |data: &[f32], _| {
                push_captured_samples(data, in_channels, &in_resampler, &in_ring, |v| v);
            },
            |e| eprintln!("[teza-call][audio] input stream error: {e}"),
            None,
        ),
        cpal::SampleFormat::I16 => input_dev.build_input_stream(
            &in_stream_cfg,
            move |data: &[i16], _| {
                push_captured_samples(data, in_channels, &in_resampler, &in_ring, |v| {
                    f32::from(v) / 32768.0
                });
            },
            |e| eprintln!("[teza-call][audio] input stream error: {e}"),
            None,
        ),
        cpal::SampleFormat::I32 => input_dev.build_input_stream(
            &in_stream_cfg,
            move |data: &[i32], _| {
                push_captured_samples(data, in_channels, &in_resampler, &in_ring, |v| {
                    (f64::from(v) / f64::from(i32::MAX)) as f32
                });
            },
            |e| eprintln!("[teza-call][audio] input stream error: {e}"),
            None,
        ),
        cpal::SampleFormat::U16 => input_dev.build_input_stream(
            &in_stream_cfg,
            move |data: &[u16], _| {
                push_captured_samples(data, in_channels, &in_resampler, &in_ring, |v| {
                    (f32::from(v) - 32768.0) / 32768.0
                });
            },
            |e| eprintln!("[teza-call][audio] input stream error: {e}"),
            None,
        ),
        other => {
            return Err(CallError::Audio(format!(
                "Định dạng mẫu micro không hỗ trợ: {other:?}"
            )));
        }
    }
    .map_err(|e| CallError::Audio(format!("Không mở được luồng micro: {e}")))?;

    input_stream
        .play()
        .map_err(|e| CallError::Audio(format!("Không chạy được micro: {e}")))?;
    Ok((input_stream, input_name))
}

fn open_audio_streams(
    devices: &AudioDeviceSelection,
    allow_missing_input: bool,
) -> CallResult<AudioStreams> {
    open_audio_streams_with(devices, allow_missing_input, try_open_input_stream)
}

fn open_audio_streams_with<F>(
    devices: &AudioDeviceSelection,
    allow_missing_input: bool,
    open_input: F,
) -> CallResult<AudioStreams>
where
    F: FnOnce(
        &AudioDeviceSelection,
        &Arc<Mutex<VecDeque<f32>>>,
    ) -> CallResult<(cpal::Stream, String)>,
{
    let (output_dev, output_device_name, out_cfg) =
        select_output_device(devices.output_device_name.as_deref())?;

    let out_rate = out_cfg.sample_rate().0.max(8_000);
    let out_channels = (out_cfg.channels() as usize).max(1);
    let out_fmt = out_cfg.sample_format();
    let out_stream_cfg: cpal::StreamConfig = out_cfg.into();

    let capture_16k: Arc<Mutex<VecDeque<f32>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(AUDIO_FRAME_SAMPLES * 8)));
    let playback_16k: Arc<Mutex<VecDeque<f32>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(AUDIO_FRAME_SAMPLES * 8)));

    let (input_stream, input_device_name) = match open_input(devices, &capture_16k) {
        Ok((stream, name)) => (Some(stream), Some(name)),
        Err(err) if allow_missing_input => {
            eprintln!(
                "[teza-call][audio] không mở được micro ({err}); chuyển sang chế độ chỉ nghe (receive-only)."
            );
            (None, None)
        }
        Err(err) => return Err(err),
    };

    let out_upsampler = Arc::new(Mutex::new(LinearResampler::new(AUDIO_SAMPLE_RATE, out_rate)));
    let out_staging: Arc<Mutex<VecDeque<f32>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity(AUDIO_FRAME_SAMPLES * 12)));
    let out_ring = playback_16k.clone();
    let output_stream = match out_fmt {
        cpal::SampleFormat::F32 => output_dev.build_output_stream(
            &out_stream_cfg,
            move |data: &mut [f32], _| {
                fill_playback_samples(
                    data,
                    out_channels,
                    out_rate,
                    &out_ring,
                    &out_upsampler,
                    &out_staging,
                    |v| v,
                );
            },
            |e| eprintln!("[teza-call][audio] output stream error: {e}"),
            None,
        ),
        cpal::SampleFormat::I16 => output_dev.build_output_stream(
            &out_stream_cfg,
            move |data: &mut [i16], _| {
                fill_playback_samples(
                    data,
                    out_channels,
                    out_rate,
                    &out_ring,
                    &out_upsampler,
                    &out_staging,
                    f32_to_i16,
                );
            },
            |e| eprintln!("[teza-call][audio] output stream error: {e}"),
            None,
        ),
        cpal::SampleFormat::I32 => output_dev.build_output_stream(
            &out_stream_cfg,
            move |data: &mut [i32], _| {
                fill_playback_samples(
                    data,
                    out_channels,
                    out_rate,
                    &out_ring,
                    &out_upsampler,
                    &out_staging,
                    f32_to_i32,
                );
            },
            |e| eprintln!("[teza-call][audio] output stream error: {e}"),
            None,
        ),
        cpal::SampleFormat::U16 => output_dev.build_output_stream(
            &out_stream_cfg,
            move |data: &mut [u16], _| {
                fill_playback_samples(
                    data,
                    out_channels,
                    out_rate,
                    &out_ring,
                    &out_upsampler,
                    &out_staging,
                    f32_to_u16,
                );
            },
            |e| eprintln!("[teza-call][audio] output stream error: {e}"),
            None,
        ),
        other => {
            return Err(CallError::Audio(format!(
                "Định dạng mẫu loa không hỗ trợ: {other:?}"
            )));
        }
    }
    .map_err(|e| CallError::Audio(format!("Không mở được luồng loa: {e}")))?;

    output_stream
        .play()
        .map_err(|e| CallError::Audio(format!("Không chạy được loa: {e}")))?;

    Ok(AudioStreams {
        _input: input_stream,
        _output: output_stream,
        capture_16k,
        playback_16k,
        input_device_name,
        output_device_name,
    })
}

/// Fixed 20 ms VoIP audio frame interval.
const FRAME_STEP: Duration = Duration::from_millis(20);

/// Stall threshold before synthesizing comfort-noise silence frames when the capture device
/// delivers no callbacks (prevents racing with normal 20–30 ms WASAPI/CoreAudio buffers).
const CAPTURE_STALL_FALLBACK: Duration = Duration::from_millis(60);

fn advance_synthetic_tx_clock(last_tx: &mut Instant, now: Instant) -> bool {
    let elapsed = now.saturating_duration_since(*last_tx);
    if elapsed < FRAME_STEP {
        return false;
    }
    if elapsed > Duration::from_millis(120) {
        *last_tx = now;
    } else {
        *last_tx += FRAME_STEP;
    }
    true
}

fn prepare_capture_frame(
    capture_16k: &Arc<Mutex<VecDeque<f32>>>,
    last_real_capture: &mut Instant,
    last_tx: &mut Instant,
    controls: &AudioControls,
) -> Option<Vec<f32>> {
    let now = Instant::now();
    // In receive-only mode (no microphone attached), emit drift-free 20 ms silence frames so
    // outbound RTP / NAT pinholes stay alive at 50 fps while speaker playback continues normally.
    if !controls.has_input_device() {
        if advance_synthetic_tx_clock(last_tx, now) {
            controls.mic_level_permille.store(0, Ordering::Relaxed);
            return Some(vec![0.0_f32; AUDIO_FRAME_SAMPLES]);
        }
        return None;
    }

    let mut frame: Vec<f32> = {
        let mut q = capture_16k.lock().ok()?;
        if q.len() >= AUDIO_FRAME_SAMPLES {
            q.drain(..AUDIO_FRAME_SAMPLES).collect()
        } else {
            Vec::new()
        }
    };
    if !frame.is_empty() {
        *last_real_capture = now;
        *last_tx = now;
    } else if now.saturating_duration_since(*last_real_capture) >= CAPTURE_STALL_FALLBACK
        && advance_synthetic_tx_clock(last_tx, now)
    {
        frame = vec![0.0_f32; AUDIO_FRAME_SAMPLES];
    } else {
        return None;
    }

    let gain = (controls.mic_gain_percent() as f32 / 100.0).clamp(0.0, 2.0);
    let mut peak = 0.0_f32;
    if controls.is_mic_muted() || gain <= 0.001 {
        frame.fill(0.0);
    } else {
        for s in &mut frame {
            *s = (*s * gain).clamp(-1.0, 1.0);
            peak = peak.max(s.abs());
        }
    }
    controls.mic_level_permille.store(
        (peak * 1000.0).clamp(0.0, 1000.0) as u32,
        Ordering::Relaxed,
    );
    Some(frame)
}

fn push_playback_pcm(
    playback_16k: &Arc<Mutex<VecDeque<f32>>>,
    pcm: &[f32],
    controls: &AudioControls,
) {
    let peak = pcm.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
    controls.remote_level_permille.store(
        (peak * 1000.0).clamp(0.0, 1000.0) as u32,
        Ordering::Relaxed,
    );
    controls
        .remote_speaking
        .store(peak > 0.025, Ordering::Relaxed);
    if let Ok(mut q) = playback_16k.lock() {
        q.extend(pcm.iter().copied());
        if q.len() > MAX_RING_SAMPLES {
            let drop_n = q.len() - MAX_RING_SAMPLES;
            q.drain(..drop_n);
        }
    }
}

/// Extract `(sender_ssrc, middle_32bit_ntp)` from the first RTCP Sender Report (`PT = 200`)
/// in a single or compound RFC 3550 RTCP packet.
fn rtcp_sr_sender_and_middle_ntp(rtcp: &[u8]) -> Option<(u32, u32)> {
    let mut off = 0usize;
    while off + 4 <= rtcp.len() {
        if (rtcp[off] & 0xC0) != 0x80 {
            break;
        }
        let pt = rtcp[off + 1];
        let length_words = u16::from_be_bytes([rtcp[off + 2], rtcp[off + 3]]) as usize;
        let pkt_len = (length_words + 1).saturating_mul(4);
        if pt == 200 && off + 16 <= rtcp.len() {
            let sender_ssrc = u32::from_be_bytes([
                rtcp[off + 4],
                rtcp[off + 5],
                rtcp[off + 6],
                rtcp[off + 7],
            ]);
            let mid_ntp = u32::from_be_bytes([
                rtcp[off + 10],
                rtcp[off + 11],
                rtcp[off + 12],
                rtcp[off + 13],
            ]);
            return Some((sender_ssrc, mid_ntp));
        }
        if pkt_len < 4 || off.checked_add(pkt_len).is_none_or(|next| next > rtcp.len()) {
            break;
        }
        off += pkt_len;
    }
    None
}

/// Process a single raw UDP datagram from either the RTP socket or RTCP inbox:
/// captures late `InitZRTP` tokens (only from `rtp_target` or `rtcp_target`, routing distinct
/// RTCP tokens into `live_rtcp_relay_token` without clobbering `live_relay_token`), tracks RTCP
/// SR LSR timestamps, handles mid-call SSRC resets, and pushes Opus audio payloads into the jitter buffer.
#[allow(clippy::too_many_arguments)]
fn ingest_udp_datagram(
    raw: &[u8],
    peer: Option<SocketAddr>,
    rtp_target: SocketAddr,
    rtcp_target: Option<SocketAddr>,
    live_relay_token: Option<&Arc<AtomicU32>>,
    live_rtcp_relay_token: Option<&Arc<AtomicU32>>,
    peer_ssrc: &mut u32,
    rx_stats: &mut RtpRxStats,
    jitter: &mut JitterBuffer,
    last_peer_sr: &mut Option<(u32, Instant)>,
    controls: &AudioControls,
    aux_session: Option<&Arc<dyn AuxiliaryMediaSession>>,
) {
    let Some(inner) = unwrap_relay_media(raw) else {
        if let Some(init_resp) = parse_zrtp_init_response(raw) {
            if init_resp.res == 0 && init_resp.token != 0 {
                if let Some(p) = peer {
                    if p == rtp_target {
                        if let Some(tok_atomic) = live_relay_token {
                            tok_atomic.store(init_resp.token, Ordering::Relaxed);
                        }
                    } else if rtcp_target == Some(p) {
                        if let Some(rtcp_atomic) = live_rtcp_relay_token {
                            rtcp_atomic.store(init_resp.token, Ordering::Relaxed);
                        } else if let Some(tok_atomic) = live_relay_token {
                            let _ = tok_atomic.compare_exchange(
                                0,
                                init_resp.token,
                                Ordering::Relaxed,
                                Ordering::Relaxed,
                            );
                        }
                    }
                }
            }
        }
        return;
    };
    if is_rtcp_packet(inner) {
        let outer = raw[0];
        let is_video_outer = matches!(
            outer,
            RELAY_WRAP_VIDEO_RTCP | RELAY_WRAP_VIDEO_RTP | RELAY_INCOMING_VIDEO_RTP
        );
        let is_audio_outer = matches!(outer, RELAY_WRAP_AUDIO_RTCP | RELAY_WRAP_AUDIO_RTP | 0x04);
        if !is_video_outer {
            if let Some((sr_ssrc, mid_ntp)) = rtcp_sr_sender_and_middle_ntp(inner) {
                let matches_audio = if is_audio_outer {
                    *peer_ssrc == 0 || sr_ssrc == *peer_ssrc
                } else {
                    *peer_ssrc != 0 && sr_ssrc == *peer_ssrc
                };
                if matches_audio {
                    *last_peer_sr = Some((mid_ntp, Instant::now()));
                }
            }
        }
        if let Some(aux) = aux_session {
            aux.on_incoming_rtcp(outer, inner);
        }
        return;
    }
    let Some(rtp_pkt) = parse_rtp_packet(inner) else {
        return;
    };
    if rtp_pkt.payload_type == OPUS_AUDIO_PT && !rtp_pkt.payload.is_empty() {
        if *peer_ssrc != 0 && rtp_pkt.ssrc != 0 && rtp_pkt.ssrc != *peer_ssrc {
            *rx_stats = RtpRxStats::default();
            jitter.reset();
        }
        if rtp_pkt.ssrc != 0 {
            *peer_ssrc = rtp_pkt.ssrc;
        }
        rx_stats.update(rtp_pkt.sequence_number);
        controls.rx_packets.fetch_add(1, Ordering::Relaxed);
        jitter.push(rtp_pkt.sequence_number, rtp_pkt.payload.to_vec());
    } else if let Some(aux) = aux_session {
        aux.on_incoming_rtp(raw[0], inner);
    }
}

fn drain_udp_datagrams(cfg: &AudioMediaConfig, rx_buf: &mut [u8]) -> Vec<(Option<SocketAddr>, Vec<u8>)> {
    let mut incoming: Vec<(Option<SocketAddr>, Vec<u8>)> = Vec::new();
    if let Some(ref inbox) = cfg.rtcp_inbox {
        if let Ok(mut q) = inbox.lock() {
            incoming.extend(q.drain(..).map(|pkt| (None, pkt)));
        }
    } else if let Some(ref rtcp_sock) = cfg.udp_rtcp_sock {
        if cfg.rtcp_target.is_some_and(|rt| rt != cfg.rtp_target) {
            for _ in 0..16 {
                match rtcp_sock.recv_from(rx_buf) {
                    Ok((n, peer)) => incoming.push((Some(peer), rx_buf[..n].to_vec())),
                    Err(_) => break,
                }
            }
        }
    }
    for _ in 0..64 {
        match cfg.udp_rtp_sock.recv_from(rx_buf) {
            Ok((n, peer)) => incoming.push((Some(peer), rx_buf[..n].to_vec())),
            Err(_) => break,
        }
    }
    incoming
}

fn run_aux_pump_only_loop(
    stop: Arc<AtomicBool>,
    controls: Arc<AudioControls>,
    cfg: AudioMediaConfig,
    ready_tx: std::sync::mpsc::SyncSender<CallResult<()>>,
) {
    let _attached =
        MediaAttachedGuard::acquire(cfg.media_attached.clone(), cfg.media_attached_notify.clone());
    let _ = ready_tx.send(Ok(()));

    let mut peer_ssrc: u32 = 0;
    let mut rx_stats = RtpRxStats::default();
    let mut jitter = JitterBuffer::voip_default();
    let mut last_peer_sr: Option<(u32, Instant)> = None;
    let mut rx_buf = [0u8; 4096];

    while !stop.load(Ordering::Relaxed) {
        let incoming = drain_udp_datagrams(&cfg, &mut rx_buf);
        for (peer, raw) in incoming {
            ingest_udp_datagram(
                &raw,
                peer,
                cfg.rtp_target,
                cfg.rtcp_target,
                cfg.live_relay_token.as_ref(),
                cfg.live_rtcp_relay_token.as_ref(),
                &mut peer_ssrc,
                &mut rx_stats,
                &mut jitter,
                &mut last_peer_sr,
                &controls,
                cfg.aux_session.as_ref(),
            );
        }
        jitter.reset();
        thread::sleep(Duration::from_millis(5));
    }
}

fn run_udp_audio_loop(
    stop: Arc<AtomicBool>,
    controls: Arc<AudioControls>,
    cfg: AudioMediaConfig,
    ready_tx: std::sync::mpsc::SyncSender<CallResult<()>>,
    #[cfg(test)] fault: TestAudioFault,
) {
    let _attached =
        MediaAttachedGuard::acquire(cfg.media_attached.clone(), cfg.media_attached_notify.clone());

    #[cfg(test)]
    let streams_res = match fault {
        TestAudioFault::None => open_audio_streams(&cfg.devices, true),
        TestAudioFault::MissingInput => open_audio_streams_with(&cfg.devices, true, |_, _| {
            Err(CallError::Audio(
                "Không tìm thấy micro mặc định trên máy (injected test fault)".to_string(),
            ))
        }),
        TestAudioFault::OutputStartFailure => Err(CallError::Audio(
            "Không tìm thấy loa mặc định trên máy (injected test fault)".to_string(),
        )),
    };
    #[cfg(not(test))]
    let streams_res = open_audio_streams(&cfg.devices, true);

    let streams = match streams_res {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    controls.set_has_input_device(streams.has_input_device());
    controls.set_active_devices(
        streams.input_device_name.clone(),
        Some(streams.output_device_name.clone()),
    );
    let mut codec = match OpusVoiceCodec::new() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    let _ = ready_tx.send(Ok(()));

    let ssrc = if cfg.ssrc != 0 {
        cfg.ssrc
    } else {
        0x5A41_4C4F
    };
    let mut seq: u16 = 0;
    let mut ts: u32 = 0;
    let mut rx_stats = RtpRxStats::default();
    let mut peer_ssrc: u32 = 0;
    let mut last_peer_sr: Option<(u32, Instant)> = None;
    let mut jitter = JitterBuffer::voip_default();

    let mut last_real_capture = Instant::now();
    let mut last_tx = Instant::now();
    let mut next_playout = Instant::now() + FRAME_STEP;
    let mut last_rtcp = Instant::now() - Duration::from_secs(2);
    let mut rx_buf = [0u8; 4096];

    while !stop.load(Ordering::Relaxed) {
        // 1. Capture -> Opus encode -> RTP -> UDP TX (drain up to 3 frames if OS sleep overshot)
        for _ in 0..3 {
            let Some(frame) = prepare_capture_frame(
                &streams.capture_16k,
                &mut last_real_capture,
                &mut last_tx,
                &controls,
            ) else {
                break;
            };
            if let Ok(opus_pkt) = codec.encode_frame(&frame) {
                let rtp = build_rtp_packet(OPUS_AUDIO_PT, false, seq, ts, ssrc, &opus_pkt);
                let token = cfg.active_relay_token();
                let wire = if token != 0 {
                    wrap_relay_audio_rtp(token, &rtp)
                } else {
                    rtp
                };
                match cfg.udp_rtp_sock.send_to(&wire, cfg.rtp_target) {
                    Ok(_) => {
                        controls.tx_packets.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => {
                        controls.tx_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                seq = seq.wrapping_add(1);
                ts = ts.wrapping_add(AUDIO_FRAME_SAMPLES as u32);
            }
        }

        // 2. UDP RX -> relay unwrap -> RTP / RTCP / late InitZRTP dispatch
        let incoming = drain_udp_datagrams(&cfg, &mut rx_buf);

        for (peer, raw) in incoming {
            ingest_udp_datagram(
                &raw,
                peer,
                cfg.rtp_target,
                cfg.rtcp_target,
                cfg.live_relay_token.as_ref(),
                cfg.live_rtcp_relay_token.as_ref(),
                &mut peer_ssrc,
                &mut rx_stats,
                &mut jitter,
                &mut last_peer_sr,
                &controls,
                cfg.aux_session.as_ref(),
            );
        }

        // 3. Drift-free playout cadence (20 ms) -> JitterBuffer -> Opus decode / FEC / PLC -> speaker
        let now = Instant::now();
        if now.saturating_duration_since(next_playout) > Duration::from_millis(120) {
            next_playout = now;
        }
        let mut playout_steps = 0usize;
        while now >= next_playout && playout_steps < 3 {
            next_playout += FRAME_STEP;
            playout_steps += 1;
            match jitter.pop() {
                JitterPop::Packet(opus_pkt) => {
                    if let Ok(pcm) = codec.decode_frame(&opus_pkt) {
                        push_playback_pcm(&streams.playback_16k, &pcm, &controls);
                    }
                }
                JitterPop::MissingWithNext(next_pkt) => {
                    controls.fec_frames.fetch_add(1, Ordering::Relaxed);
                    if let Ok(pcm) = codec.decode_fec_or_plc(&next_pkt) {
                        push_playback_pcm(&streams.playback_16k, &pcm, &controls);
                    }
                }
                JitterPop::Plc => {
                    controls.plc_frames.fetch_add(1, Ordering::Relaxed);
                    if let Ok(pcm) = codec.decode_plc() {
                        push_playback_pcm(&streams.playback_16k, &pcm, &controls);
                    }
                }
                JitterPop::Buffering | JitterPop::Silence => {
                    controls.remote_speaking.store(false, Ordering::Relaxed);
                    controls.remote_level_permille.store(0, Ordering::Relaxed);
                }
            }
        }

        // 4. Periodic RTCP Receiver Report (every 2 seconds)
        if last_rtcp.elapsed() >= Duration::from_secs(2) {
            last_rtcp = Instant::now();
            let (lsr, dlsr) = match last_peer_sr {
                Some((m, at)) => (m, (at.elapsed().as_secs_f64() * 65536.0) as u32),
                None => (0, 0),
            };
            let rr = build_rtcp_rr(ssrc, rx_stats.report_block(peer_ssrc, lsr, dlsr));
            let (token, target, use_rtcp_sock) = cfg.resolve_rtcp_send_route();
            let wire = if token != 0 {
                wrap_relay_audio_rtcp(token, &rr)
            } else {
                rr
            };
            let sender = if use_rtcp_sock {
                cfg.udp_rtcp_sock.as_ref().unwrap_or(&cfg.udp_rtp_sock)
            } else {
                &cfg.udp_rtp_sock
            };
            let _ = sender.send_to(&wire, target);
        }

        thread::sleep(Duration::from_millis(4));
    }

    drop(streams);
}

fn run_loopback_audio_loop(
    stop: Arc<AtomicBool>,
    controls: Arc<AudioControls>,
    devices: AudioDeviceSelection,
    simulated_loss_pct: u8,
    inject_tone_hz: Option<f32>,
    ready_tx: std::sync::mpsc::SyncSender<CallResult<()>>,
) {
    let allow_missing_input = inject_tone_hz.is_some();
    let streams = match open_audio_streams(&devices, allow_missing_input) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    controls.set_has_input_device(streams.has_input_device() || inject_tone_hz.is_some());
    controls.set_active_devices(
        streams.input_device_name.clone(),
        Some(streams.output_device_name.clone()),
    );
    let mut codec = match OpusVoiceCodec::new() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    let _ = ready_tx.send(Ok(()));

    let mut jitter = JitterBuffer::voip_default();
    let mut seq: u16 = 0;
    let mut tone_sample_idx: u64 = 0;
    let mut last_real_capture = Instant::now();
    let mut last_tx = Instant::now();
    let mut next_playout = Instant::now() + FRAME_STEP;

    while !stop.load(Ordering::Relaxed) {
        for _ in 0..3 {
            let frame_opt = if let Some(freq_hz) = inject_tone_hz {
                let now = Instant::now();
                if advance_synthetic_tx_clock(&mut last_tx, now) {
                    let amp = 0.25_f32;
                    let mut buf = Vec::with_capacity(AUDIO_FRAME_SAMPLES);
                    for _ in 0..AUDIO_FRAME_SAMPLES {
                        let phase = 2.0 * std::f32::consts::PI * freq_hz * (tone_sample_idx as f32)
                            / (AUDIO_SAMPLE_RATE as f32);
                        buf.push(amp * phase.sin());
                        tone_sample_idx = tone_sample_idx.wrapping_add(1);
                    }
                    controls
                        .mic_level_permille
                        .store((amp * 1000.0) as u32, Ordering::Relaxed);
                    Some(buf)
                } else {
                    None
                }
            } else {
                prepare_capture_frame(
                    &streams.capture_16k,
                    &mut last_real_capture,
                    &mut last_tx,
                    &controls,
                )
            };
            let Some(frame) = frame_opt else {
                break;
            };
            if let Ok(opus_pkt) = codec.encode_frame(&frame) {
                controls.tx_packets.fetch_add(1, Ordering::Relaxed);
                let dropped = simulated_loss_pct > 0
                    && ((u32::from(seq) * 37) % 100) < u32::from(simulated_loss_pct);
                if !dropped {
                    controls.rx_packets.fetch_add(1, Ordering::Relaxed);
                    jitter.push(seq, opus_pkt);
                }
                seq = seq.wrapping_add(1);
            }
        }

        let now = Instant::now();
        if now.saturating_duration_since(next_playout) > Duration::from_millis(120) {
            next_playout = now;
        }
        let mut playout_steps = 0usize;
        while now >= next_playout && playout_steps < 3 {
            next_playout += FRAME_STEP;
            playout_steps += 1;
            match jitter.pop() {
                JitterPop::Packet(pkt) => {
                    if let Ok(pcm) = codec.decode_frame(&pkt) {
                        push_playback_pcm(&streams.playback_16k, &pcm, &controls);
                    }
                }
                JitterPop::MissingWithNext(next_pkt) => {
                    controls.fec_frames.fetch_add(1, Ordering::Relaxed);
                    if let Ok(pcm) = codec.decode_fec_or_plc(&next_pkt) {
                        push_playback_pcm(&streams.playback_16k, &pcm, &controls);
                    }
                }
                JitterPop::Plc => {
                    controls.plc_frames.fetch_add(1, Ordering::Relaxed);
                    if let Ok(pcm) = codec.decode_plc() {
                        push_playback_pcm(&streams.playback_16k, &pcm, &controls);
                    }
                }
                JitterPop::Buffering | JitterPop::Silence => {}
            }
        }

        thread::sleep(Duration::from_millis(4));
    }

    drop(streams);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_wave(rate: u32, freq: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| {
                0.6 * (2.0 * std::f32::consts::PI * freq * (i as f32) / (rate as f32)).sin()
            })
            .collect()
    }

    #[test]
    fn resampler_preserves_duration_across_rates_and_callback_chunks() {
        // 48 kHz -> 16 kHz (1 second)
        let input_48k = sine_wave(48_000, 440.0, 48_000);
        let mut r = LinearResampler::new(48_000, 16_000);
        let mut out = Vec::new();
        for chunk in input_48k.chunks(256) {
            out.extend(r.push(chunk));
        }
        out.extend(r.flush());
        assert!((out.len() as i64 - 16_000).abs() <= 2, "got {}", out.len());

        // 44.1 kHz -> 16 kHz (1 second)
        let input_44k = sine_wave(44_100, 440.0, 44_100);
        let mut r2 = LinearResampler::new(44_100, 16_000);
        let mut out2 = Vec::new();
        for chunk in input_44k.chunks(128) {
            out2.extend(r2.push(chunk));
        }
        out2.extend(r2.flush());
        assert!((out2.len() as i64 - 16_000).abs() <= 2, "got {}", out2.len());

        // 16 kHz -> 48 kHz upsample (20 ms -> ~960 samples)
        let input_16k = sine_wave(16_000, 440.0, 16_000);
        let mut up = LinearResampler::new(16_000, 48_000);
        let mut out_up = up.push(&input_16k);
        out_up.extend(up.flush());
        assert!(
            (out_up.len() as i64 - 48_000).abs() <= 3,
            "got {}",
            out_up.len()
        );
    }

    #[test]
    fn opus_codec_roundtrip_plc_and_fec_work_at_16khz_mono_20ms() {
        let mut codec = OpusVoiceCodec::new().unwrap();
        let tone = sine_wave(AUDIO_SAMPLE_RATE, 440.0, AUDIO_FRAME_SAMPLES * 10);

        let mut packets = Vec::new();
        for frame in tone.as_chunks::<AUDIO_FRAME_SAMPLES>().0 {
            let pkt = codec.encode_frame(frame).unwrap();
            assert!(!pkt.is_empty());
            assert!(pkt.len() < 200, "unexpectedly large VoIP packet: {}", pkt.len());
            packets.push(pkt);
        }

        // Decode first 5 frames normally
        let mut last_rms = 0.0_f32;
        for pkt in &packets[..5] {
            let decoded = codec.decode_frame(pkt).unwrap();
            assert_eq!(decoded.len(), AUDIO_FRAME_SAMPLES);
            assert!(decoded.iter().all(|s| s.is_finite() && s.abs() <= 1.001));
            last_rms = (decoded.iter().map(|s| s * s).sum::<f32>() / decoded.len() as f32).sqrt();
        }
        assert!(last_rms > 0.2, "decoded sine RMS too low: {last_rms}");

        // Simulate missing packet #5 -> recover via FEC/PLC using packet #6
        let recovered = codec.decode_fec_or_plc(&packets[6]).unwrap();
        assert_eq!(recovered.len(), AUDIO_FRAME_SAMPLES);
        assert!(recovered.iter().all(|s| s.is_finite() && s.abs() <= 1.001));

        // Pure PLC with no lookahead packet
        let plc = codec.decode_plc().unwrap();
        assert_eq!(plc.len(), AUDIO_FRAME_SAMPLES);
        let plc_rms = (plc.iter().map(|s| s * s).sum::<f32>() / plc.len() as f32).sqrt();
        assert!(plc_rms > 0.01, "PLC after voiced frames should continue signal, rms={plc_rms}");

        // Reject wrong frame length cleanly
        assert!(codec.encode_frame(&[0.0_f32; 160]).is_err());
    }

    #[test]
    fn audio_controls_mute_and_gain_clamp() {
        let ctrl = AudioControls::default();
        assert!(!ctrl.is_mic_muted());
        assert_eq!(ctrl.mic_gain_percent(), 100);

        ctrl.set_mic_gain_percent(999);
        assert_eq!(ctrl.mic_gain_percent(), 200);

        ctrl.mic_level_permille.store(650, Ordering::Relaxed);
        ctrl.set_mic_muted(true);
        assert!(ctrl.is_mic_muted());
        assert_eq!(ctrl.mic_level_permille(), 0);
    }

    #[test]
    fn udp_opus_rtp_relay_roundtrip_from_os_thread_with_packet_loss() {
        // Exercises the exact non-blocking std::net::UdpSocket::send_to / recv_from
        // pattern used inside `run_udp_audio_loop` on a dedicated OS thread.
        let tx_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let rx_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        tx_sock.set_nonblocking(true).unwrap();
        rx_sock.set_nonblocking(true).unwrap();
        let rx_addr = rx_sock.local_addr().unwrap();

        let tx_sock_t = tx_sock.clone();
        let rx_sock_t = rx_sock.clone();
        let (decoded_frames, fec_or_plc_frames) = thread::spawn(move || {
            let mut enc = OpusVoiceCodec::new().unwrap();
            let mut dec = OpusVoiceCodec::new().unwrap();
            let mut jb = JitterBuffer::voip_default();
            let tone = sine_wave(AUDIO_SAMPLE_RATE, 440.0, AUDIO_FRAME_SAMPLES * 8);
            let token = 0x1357_2468_u32;

            // Send 8 frames over localhost UDP wrapped with Zalo 5-byte relay header, dropping seq #3
            for (idx, frame) in tone.as_chunks::<AUDIO_FRAME_SAMPLES>().0.iter().enumerate() {
                if idx == 3 {
                    continue;
                }
                let opus_pkt = enc.encode_frame(frame).unwrap();
                let rtp = build_rtp_packet(
                    OPUS_AUDIO_PT,
                    false,
                    idx as u16,
                    (idx * AUDIO_FRAME_SAMPLES) as u32,
                    0xAABB_CCDD,
                    &opus_pkt,
                );
                let wire = wrap_relay_audio_rtp(token, &rtp);
                tx_sock_t.send_to(&wire, rx_addr).unwrap();
            }

            thread::sleep(Duration::from_millis(30));

            let mut buf = [0u8; 2048];
            while let Ok((n, _)) = rx_sock_t.recv_from(&mut buf) {
                if let Some(inner) = unwrap_relay_media(&buf[..n]) {
                    if let Some(pkt) = parse_rtp_packet(inner) {
                        assert_eq!(pkt.payload_type, OPUS_AUDIO_PT);
                        jb.push(pkt.sequence_number, pkt.payload.to_vec());
                    }
                }
            }

            let mut decoded = 0usize;
            let mut recovered = 0usize;
            for _ in 0..8 {
                match jb.pop() {
                    JitterPop::Packet(p) => {
                        let pcm = dec.decode_frame(&p).unwrap();
                        assert_eq!(pcm.len(), AUDIO_FRAME_SAMPLES);
                        decoded += 1;
                    }
                    JitterPop::MissingWithNext(next_p) => {
                        let pcm = dec.decode_fec_or_plc(&next_p).unwrap();
                        assert_eq!(pcm.len(), AUDIO_FRAME_SAMPLES);
                        recovered += 1;
                    }
                    JitterPop::Plc => {
                        let pcm = dec.decode_plc().unwrap();
                        assert_eq!(pcm.len(), AUDIO_FRAME_SAMPLES);
                        recovered += 1;
                    }
                    JitterPop::Buffering | JitterPop::Silence => {}
                }
            }
            (decoded, recovered)
        })
        .join()
        .unwrap();

        assert_eq!(decoded_frames, 7);
        assert_eq!(fec_or_plc_frames, 1);
    }

    #[test]
    fn ingest_datagram_resets_jitter_on_ssrc_change_and_captures_late_zrtp_token() {
        let live_token = Arc::new(AtomicU32::new(0));
        let live_rtcp_token = Arc::new(AtomicU32::new(0));
        let mut peer_ssrc = 0u32;
        let mut rx_stats = RtpRxStats::default();
        let mut jitter = JitterBuffer::voip_default();
        let mut last_sr = None;
        let controls = AudioControls::default();
        let rtp_target: SocketAddr = "127.0.0.1:4200".parse().unwrap();
        let rtcp_target: SocketAddr = "127.0.0.1:4201".parse().unwrap();
        let other_relay: SocketAddr = "127.0.0.1:9999".parse().unwrap();

        // 0. Late InitZRTP from a DIFFERENT relay server (or unknown peer) MUST NOT contaminate tokens
        let foreign_init = crate::zrtp::encode_zrtp_init_response(
            crate::zrtp::ZRTP_CMD_INIT_CALLER,
            0,
            42,
            0xDEAD_0001,
            None,
        );
        ingest_udp_datagram(
            &foreign_init,
            Some(other_relay),
            rtp_target,
            Some(rtcp_target),
            Some(&live_token),
            Some(&live_rtcp_token),
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        ingest_udp_datagram(
            &foreign_init,
            None,
            rtp_target,
            Some(rtcp_target),
            Some(&live_token),
            Some(&live_rtcp_token),
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        assert_eq!(
            live_token.load(Ordering::Relaxed),
            0,
            "foreign relay InitZRTP must be ignored"
        );
        assert_eq!(live_rtcp_token.load(Ordering::Relaxed), 0);

        // 1. Late InitZRTP response arriving from `rtp_target` updates `live_token` only
        let late_init = crate::zrtp::encode_zrtp_init_response(
            crate::zrtp::ZRTP_CMD_INIT_CALLER,
            0,
            42,
            0xBEEF_CAFE,
            None,
        );
        ingest_udp_datagram(
            &late_init,
            Some(rtp_target),
            rtp_target,
            Some(rtcp_target),
            Some(&live_token),
            Some(&live_rtcp_token),
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        assert_eq!(live_token.load(Ordering::Relaxed), 0xBEEF_CAFE);
        assert_eq!(live_rtcp_token.load(Ordering::Relaxed), 0);

        // 1b. Late InitZRTP from distinct `rtcp_target` updates `live_rtcp_token` WITHOUT
        // clobbering `live_token`!
        let rtcp_init = crate::zrtp::encode_zrtp_init_response(
            crate::zrtp::ZRTP_CMD_INIT_CALLER,
            0,
            42,
            0xCAFE_1234,
            None,
        );
        ingest_udp_datagram(
            &rtcp_init,
            Some(rtcp_target),
            rtp_target,
            Some(rtcp_target),
            Some(&live_token),
            Some(&live_rtcp_token),
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        assert_eq!(
            live_token.load(Ordering::Relaxed),
            0xBEEF_CAFE,
            "distinct RTCP InitZRTP must NOT overwrite RTP token"
        );
        assert_eq!(live_rtcp_token.load(Ordering::Relaxed), 0xCAFE_1234);

        // 1c. When no separate RTCP handle is provided, RTCP InitZRTP seeds a zero RTP token
        // but never clobbers a non-zero RTP token.
        let single_tok = Arc::new(AtomicU32::new(0));
        ingest_udp_datagram(
            &rtcp_init,
            Some(rtcp_target),
            rtp_target,
            Some(rtcp_target),
            Some(&single_tok),
            None,
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        assert_eq!(single_tok.load(Ordering::Relaxed), 0xCAFE_1234);
        ingest_udp_datagram(
            &late_init,
            Some(rtp_target),
            rtp_target,
            Some(rtcp_target),
            Some(&single_tok),
            None,
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        assert_eq!(single_tok.load(Ordering::Relaxed), 0xBEEF_CAFE);
        ingest_udp_datagram(
            &rtcp_init,
            Some(rtcp_target),
            rtp_target,
            Some(rtcp_target),
            Some(&single_tok),
            None,
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        assert_eq!(
            single_tok.load(Ordering::Relaxed),
            0xBEEF_CAFE,
            "distinct RTCP reply must not overwrite an already-known RTP token"
        );

        // 2. Peer streams with SSRC 0x1111_1111 at seq 5000..=5001
        for seq in 5000u16..=5001 {
            let rtp = build_rtp_packet(OPUS_AUDIO_PT, false, seq, 0, 0x1111_1111, &[seq as u8]);
            ingest_udp_datagram(
                &wrap_relay_audio_rtp(0xBEEF_CAFE, &rtp),
                Some(rtp_target),
                rtp_target,
                Some(rtcp_target),
                Some(&live_token),
                Some(&live_rtcp_token),
                &mut peer_ssrc,
                &mut rx_stats,
                &mut jitter,
                &mut last_sr,
                &controls,
                None,
            );
        }
        assert_eq!(peer_ssrc, 0x1111_1111);
        assert_eq!(jitter.pop(), JitterPop::Packet(vec![5000u16 as u8]));

        // 3. Peer switches SSRC mid-call to 0x2222_2222 starting back at seq 1..=2
        for seq in 1u16..=2 {
            let rtp = build_rtp_packet(OPUS_AUDIO_PT, false, seq, 0, 0x2222_2222, &[seq as u8]);
            ingest_udp_datagram(
                &wrap_relay_audio_rtp(0xBEEF_CAFE, &rtp),
                Some(rtp_target),
                rtp_target,
                Some(rtcp_target),
                Some(&live_token),
                Some(&live_rtcp_token),
                &mut peer_ssrc,
                &mut rx_stats,
                &mut jitter,
                &mut last_sr,
                &controls,
                None,
            );
        }
        assert_eq!(peer_ssrc, 0x2222_2222);
        assert_eq!(rx_stats.received(), 2);
        assert_eq!(rx_stats.extended_highest_seq(), 2);
        assert_eq!(jitter.stats().dropped_late, 0);
        assert_eq!(jitter.pop(), JitterPop::Packet(vec![1]));
        assert_eq!(jitter.pop(), JitterPop::Packet(vec![2]));
    }

    #[test]
    fn prepare_capture_frame_emits_drift_free_20ms_silence_in_receive_only_mode() {
        let capture_16k: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let controls = AudioControls::default();
        assert!(!controls.has_input_device());
        assert!(!controls.snapshot().has_input_device);

        // Receive-only mode: does NOT emit before 20 ms cadence
        let mut last_real = Instant::now();
        let mut just_now = Instant::now();
        assert!(
            prepare_capture_frame(&capture_16k, &mut last_real, &mut just_now, &controls).is_none()
        );

        // Receive-only mode: a 45 ms elapsed window yields TWO drift-free 20 ms frames and then
        // pauses (remaining 5 ms < 20 ms), preserving exact 50 fps cadence even under 15.6 ms OS sleeps.
        let mut old_tx = Instant::now() - Duration::from_millis(45);
        let f1 = prepare_capture_frame(&capture_16k, &mut last_real, &mut old_tx, &controls)
            .expect("first 20 ms silence frame");
        assert_eq!(f1.len(), AUDIO_FRAME_SAMPLES);
        assert!(f1.iter().all(|&s| s == 0.0));
        let f2 = prepare_capture_frame(&capture_16k, &mut last_real, &mut old_tx, &controls)
            .expect("second 20 ms silence frame for 45 ms window");
        assert_eq!(f2.len(), AUDIO_FRAME_SAMPLES);
        assert!(f2.iter().all(|&s| s == 0.0));
        assert!(
            prepare_capture_frame(&capture_16k, &mut last_real, &mut old_tx, &controls).is_none(),
            "third frame must wait until another 20 ms elapses"
        );
        assert_eq!(controls.mic_level_permille(), 0);

        // When input device IS present, normal capture drains real samples and updates meter
        controls.set_has_input_device(true);
        assert!(controls.snapshot().has_input_device);
        {
            let mut q = capture_16k.lock().unwrap();
            q.extend(vec![0.5_f32; AUDIO_FRAME_SAMPLES]);
        }
        let real_frame =
            prepare_capture_frame(&capture_16k, &mut last_real, &mut just_now, &controls)
                .expect("should drain captured frame");
        assert_eq!(real_frame.len(), AUDIO_FRAME_SAMPLES);
        assert!((real_frame[0] - 0.5).abs() < 1e-4);
        assert!(controls.mic_level_permille() >= 490);
    }

    #[test]
    fn open_audio_streams_receive_only_fallback_and_distinct_rtcp_rr_token() {
        let has_output = list_output_devices().is_ok_and(|v| !v.is_empty());
        if !has_output {
            return;
        }
        let devices = AudioDeviceSelection::default();

        // 1. `allow_missing_input = false` (loopback mode) MUST fail when mic cannot open
        let err = open_audio_streams_with(&devices, false, |_, _| {
            Err(CallError::Audio("no mic".to_string()))
        });
        assert!(err.is_err(), "allow_missing_input=false must fail without a mic");

        // 2. `allow_missing_input = true` (call media mode) MUST succeed with `_input: None`
        let streams = open_audio_streams_with(&devices, true, |_, _| {
            Err(CallError::Audio("no mic".to_string()))
        })
        .expect("allow_missing_input=true must fall back to receive-only");
        assert!(!streams.has_input_device());
        drop(streams);

        // 3. Full live `AudioMediaSession` in receive-only mode with distinct RTP & RTCP tokens:
        // verifies silence RTP frames wrap with TOKEN_RTP, periodic RTCP RR wraps with TOKEN_RTCP,
        // and echoed RTP packets are received cleanly.
        const TOKEN_RTP: u32 = 0x1122_3344;
        const TOKEN_RTCP: u32 = 0x5566_7788;

        let client_rtp = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let client_rtcp = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        client_rtp.set_nonblocking(true).unwrap();
        client_rtcp.set_nonblocking(true).unwrap();

        let mock_rtp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mock_rtcp = UdpSocket::bind("127.0.0.1:0").unwrap();
        mock_rtp.set_nonblocking(true).unwrap();
        mock_rtcp.set_nonblocking(true).unwrap();
        let mock_rtp_addr = mock_rtp.local_addr().unwrap();
        let mock_rtcp_addr = mock_rtcp.local_addr().unwrap();

        let cfg = AudioMediaConfig {
            rtp_target: mock_rtp_addr,
            rtcp_target: Some(mock_rtcp_addr),
            ssrc: 0x0102_0304,
            relay_token: TOKEN_RTP,
            live_relay_token: Some(Arc::new(AtomicU32::new(TOKEN_RTP))),
            live_rtcp_relay_token: Some(Arc::new(AtomicU32::new(TOKEN_RTCP))),
            media_attached: None,
            media_attached_notify: None,
            initial_mic_muted: false,
            udp_rtp_sock: client_rtp,
            udp_rtcp_sock: Some(client_rtcp),
            rtcp_inbox: None,
            devices,
            aux_session: None,
        };

        let sess = AudioMediaSession::start_with_fault(cfg, TestAudioFault::MissingInput)
            .expect("receive-only AudioMediaSession should start when output exists");
        assert!(
            !sess.telemetry().has_input_device,
            "receive-only session must report has_input_device == false"
        );

        let deadline = Instant::now() + Duration::from_millis(500);
        let mut rtp_ok = 0usize;
        let mut rtcp_rr_ok = 0usize;
        let mut buf = [0u8; 2048];
        while Instant::now() < deadline && (rtp_ok < 5 || rtcp_rr_ok < 1) {
            while let Ok((n, peer)) = mock_rtp.recv_from(&mut buf) {
                if n >= 5 && buf[0] == crate::zrtp::RELAY_WRAP_AUDIO_RTP {
                    let tok = u32::from_le_bytes(buf[1..5].try_into().unwrap());
                    assert_eq!(tok, TOKEN_RTP, "RTP must wrap with RTP token");
                    rtp_ok += 1;
                    // Echo back to exercise receive/playout path in receive-only mode
                    let mut down = Vec::with_capacity(1 + n - 5);
                    down.push(0x04);
                    down.extend_from_slice(&buf[5..n]);
                    let _ = mock_rtp.send_to(&down, peer);
                }
            }
            while let Ok((n, _)) = mock_rtcp.recv_from(&mut buf) {
                if n >= 5 && buf[0] == crate::zrtp::RELAY_WRAP_AUDIO_RTCP {
                    let tok = u32::from_le_bytes(buf[1..5].try_into().unwrap());
                    assert_eq!(tok, TOKEN_RTCP, "distinct RTCP RR must wrap with RTCP token");
                    rtcp_rr_ok += 1;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }

        let telem = sess.telemetry();
        sess.stop();

        assert!(!telem.has_input_device);
        assert_eq!(telem.mic_level_permille, 0);
        assert!(rtp_ok >= 5, "expected >=5 silence RTP packets, got {rtp_ok}");
        assert!(rtcp_rr_ok >= 1, "expected >=1 RTCP RR packet with distinct RTCP token");
        assert!(telem.rx_packets >= 3, "expected >=3 echoed RTP packets received, got {}", telem.rx_packets);
    }

    #[test]
    fn resolve_rtcp_send_route_falls_back_to_rtp_socket_when_rtcp_token_is_zero() {
        let rtp_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let rtcp_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let rtp_target: SocketAddr = "127.0.0.1:10490".parse().unwrap();
        let rtcp_target: SocketAddr = "127.0.0.1:12490".parse().unwrap();
        let live_rtp = Arc::new(AtomicU32::new(0x1111_2222));
        let live_rtcp = Arc::new(AtomicU32::new(0));

        let cfg = AudioMediaConfig {
            rtp_target,
            rtcp_target: Some(rtcp_target),
            ssrc: 1,
            relay_token: 0x1111_2222,
            live_relay_token: Some(live_rtp),
            live_rtcp_relay_token: Some(live_rtcp.clone()),
            media_attached: None,
            media_attached_notify: None,
            initial_mic_muted: false,
            udp_rtp_sock: rtp_sock,
            udp_rtcp_sock: Some(rtcp_sock),
            rtcp_inbox: None,
            devices: AudioDeviceSelection::default(),
            aux_session: None,
        };

        // Before distinct RTCP port acks InitZRTP (token == 0), send from udp_rtp_sock with RTP token.
        assert_eq!(cfg.resolve_rtcp_send_route(), (0x1111_2222, rtcp_target, false));

        // Once distinct RTCP port acks InitZRTP, switch to udp_rtcp_sock with RTCP token.
        live_rtcp.store(0x3333_4444, Ordering::Relaxed);
        assert_eq!(cfg.resolve_rtcp_send_route(), (0x3333_4444, rtcp_target, true));
    }

    #[test]
    fn virtual_and_spdif_audio_device_classification() {
        assert!(is_virtual_or_loopback_audio_device("CABLE Output (VB-Audio Virtual Cable)"));
        assert!(is_virtual_or_loopback_audio_device("CABLE Input (VB-Audio Virtual Cable)"));
        assert!(is_virtual_or_loopback_audio_device("CABLE In 16ch (VB-Audio Virtual Cable)"));
        assert!(is_virtual_or_loopback_audio_device("Stereo Mix (Realtek(R) Audio)"));
        assert!(is_virtual_or_loopback_audio_device("BlackHole 2ch"));
        assert!(is_virtual_or_loopback_audio_device("Monitor of Built-in Audio"));
        assert!(!is_virtual_or_loopback_audio_device("Microphone (Realtek(R) Audio)"));
        assert!(!is_virtual_or_loopback_audio_device("Speakers (Realtek(R) Audio)"));

        assert!(is_spdif_digital_output("Realtek Digital Output (Realtek(R) Audio)"));
        assert!(is_spdif_digital_output("SPDIF Interface (TX)"));
        assert!(!is_spdif_digital_output("Headphones (Sony WH-1000XM5)"));
    }

    #[test]
    fn smart_device_picker_avoids_virtual_default_when_physical_exists_and_honors_explicit() {
        // 1. OS default is VB-Audio, USB headset also present -> auto-select USB headset!
        let outputs = vec![
            "CABLE Input (VB-Audio Virtual Cable)".to_string(),
            "Realtek Digital Output (Realtek(R) Audio)".to_string(),
            "Speakers (USB Audio Device)".to_string(),
        ];
        assert_eq!(
            pick_preferred_device_name(
                None,
                Some("CABLE Input (VB-Audio Virtual Cable)"),
                &outputs,
                true,
            )
            .as_deref(),
            Some("Speakers (USB Audio Device)"),
        );

        let inputs = vec![
            "CABLE Output (VB-Audio Virtual Cable)".to_string(),
            "Stereo Mix (Realtek(R) Audio)".to_string(),
            "Microphone (USB Audio Device)".to_string(),
        ];
        assert_eq!(
            pick_preferred_device_name(
                None,
                Some("CABLE Output (VB-Audio Virtual Cable)"),
                &inputs,
                false,
            )
            .as_deref(),
            Some("Microphone (USB Audio Device)"),
        );

        // 2. When OS default is virtual and ONLY an S/PDIF physical output exists,
        //    auto-picker falls back to S/PDIF instead of staying on the virtual cable.
        let virtual_and_spdif_only = vec![
            "CABLE Input (VB-Audio Virtual Cable)".to_string(),
            "Realtek Digital Output (Realtek(R) Audio)".to_string(),
        ];
        assert_eq!(
            pick_preferred_device_name(
                None,
                Some("CABLE Input (VB-Audio Virtual Cable)"),
                &virtual_and_spdif_only,
                true,
            )
            .as_deref(),
            Some("Realtek Digital Output (Realtek(R) Audio)"),
        );

        // 2b. When NO physical output exists at all, fall back cleanly to OS default.
        let virtual_only = vec![
            "CABLE Input (VB-Audio Virtual Cable)".to_string(),
            "Stereo Mix (Realtek(R) Audio)".to_string(),
        ];
        assert_eq!(
            pick_preferred_device_name(
                None,
                Some("CABLE Input (VB-Audio Virtual Cable)"),
                &virtual_only,
                true,
            )
            .as_deref(),
            Some("CABLE Input (VB-Audio Virtual Cable)"),
        );

        // 3. Explicit user selection (even a virtual cable) is always honored verbatim
        assert_eq!(
            pick_preferred_device_name(
                Some("cable output (vb-audio virtual cable)"),
                Some("Microphone (USB Audio Device)"),
                &inputs,
                false,
            )
            .as_deref(),
            Some("CABLE Output (VB-Audio Virtual Cable)"),
        );
    }

    #[test]
    fn audio_rtcp_sr_ignores_video_rtcp_and_mismatched_raw_ssrc() {
        let make_sr = |ssrc: u32, ntp_msw: u32, ntp_lsw: u32| -> Vec<u8> {
            let mut sr = Vec::with_capacity(28);
            sr.extend_from_slice(&[0x80, 200, 0x00, 0x06]);
            sr.extend_from_slice(&ssrc.to_be_bytes());
            sr.extend_from_slice(&ntp_msw.to_be_bytes());
            sr.extend_from_slice(&ntp_lsw.to_be_bytes());
            sr.extend_from_slice(&[0u8; 12]);
            sr
        };

        let rtp_target: SocketAddr = "127.0.0.1:10490".parse().unwrap();
        let rtcp_target: SocketAddr = "127.0.0.1:12490".parse().unwrap();
        let mut peer_ssrc: u32 = 0x1111_2222;
        let mut rx_stats = RtpRxStats::default();
        let mut jitter = JitterBuffer::voip_default();
        let mut last_sr: Option<(u32, Instant)> = None;
        let controls = AudioControls::default();

        // 1. Audio RTCP SR (0x05) with matching SSRC updates audio last_peer_sr
        let audio_sr = make_sr(0x1111_2222, 0x1234_AAAA, 0xBBBB_5678);
        let audio_wire = wrap_relay_audio_rtcp(0xCAFE_0001, &audio_sr);
        ingest_udp_datagram(
            &audio_wire,
            Some(rtcp_target),
            rtp_target,
            Some(rtcp_target),
            None,
            None,
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        assert_eq!(last_sr.map(|(m, _)| m), Some(0xAAAA_BBBB));

        // 2. Video RTCP SR (0x0F) — even with the SAME SSRC — must NOT overwrite audio last_peer_sr
        let video_sr = make_sr(0x1111_2222, 0x1234_CCCC, 0xDDDD_5678);
        let video_wire = crate::zrtp::wrap_relay_video_rtcp(0xCAFE_0001, &video_sr);
        ingest_udp_datagram(
            &video_wire,
            Some(rtcp_target),
            rtp_target,
            Some(rtcp_target),
            None,
            None,
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        assert_eq!(
            last_sr.map(|(m, _)| m),
            Some(0xAAAA_BBBB),
            "0x0F video RTCP SR must never clobber audio last_peer_sr"
        );

        // 3. Raw (P2P) RTCP SR with mismatched video SSRC must NOT overwrite audio last_peer_sr
        let raw_video_sr = make_sr(0x9999_8888, 0x1234_EEEE, 0xFFFF_5678);
        ingest_udp_datagram(
            &raw_video_sr,
            Some(rtcp_target),
            rtp_target,
            Some(rtcp_target),
            None,
            None,
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        assert_eq!(last_sr.map(|(m, _)| m), Some(0xAAAA_BBBB));

        // 4. Raw (P2P) RTCP SR with matching audio SSRC updates audio last_peer_sr
        let raw_audio_sr = make_sr(0x1111_2222, 0x1234_1357, 0x2468_5678);
        ingest_udp_datagram(
            &raw_audio_sr,
            Some(rtcp_target),
            rtp_target,
            Some(rtcp_target),
            None,
            None,
            &mut peer_ssrc,
            &mut rx_stats,
            &mut jitter,
            &mut last_sr,
            &controls,
            None,
        );
        assert_eq!(last_sr.map(|(m, _)| m), Some(0x1357_2468));
    }

    #[test]
    fn audio_controls_snapshot_reports_active_device_names_and_virtual_flags() {
        let ctrl = AudioControls::default();
        ctrl.set_has_input_device(true);
        ctrl.set_active_devices(
            Some("CABLE Output (VB-Audio Virtual Cable)".to_string()),
            Some("Speakers (USB Audio Device)".to_string()),
        );
        let snap = ctrl.snapshot();
        assert_eq!(
            snap.active_input_device.as_deref(),
            Some("CABLE Output (VB-Audio Virtual Cable)")
        );
        assert_eq!(
            snap.active_output_device.as_deref(),
            Some("Speakers (USB Audio Device)")
        );
        assert!(snap.input_is_virtual);
        assert!(!snap.output_is_virtual);
    }

    #[test]
    fn list_devices_across_short_lived_threads_never_crashes_com() {
        for _ in 0..3 {
            thread::spawn(|| {
                let _ = list_output_devices();
                let _ = list_input_devices();
            })
            .join()
            .expect("sequential audio device enumeration thread must not panic");
            thread::sleep(Duration::from_millis(10));
        }

        let handles: Vec<_> = (0..2)
            .map(|_| {
                thread::spawn(|| {
                    let _ = list_output_devices();
                    let _ = list_input_devices();
                })
            })
            .collect();
        for h in handles {
            h.join()
                .expect("concurrent audio device enumeration thread must not panic");
        }
    }
}
