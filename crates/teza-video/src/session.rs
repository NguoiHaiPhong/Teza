//! Isolated 1-1 H.264/RTP/ZFrag/RTCP video pipeline and [`VideoMediaHook`] integrating
//! with `teza-call` without affecting voice calls.
//!
//! Exposes a zero-dependency Annex-B H.264 queue/callback interface (`push_outbound_au` and
//! `on_inbound_frame`) ready for WebView2 `WebCodecs` (`VideoEncoder`/`VideoDecoder`) in Phase 2.

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use teza_call::{
    build_rtp_packet, parse_rtp_packet, unwrap_relay_media, wrap_relay_video_rtcp,
    wrap_relay_video_rtp, AuxiliaryMediaHook, AuxiliaryMediaSession, CallType,
    MediaTransportContext, RtpRxStats, RELAY_INCOMING_VIDEO_RTP, RELAY_WRAP_AUDIO_RTCP,
    RELAY_WRAP_AUDIO_RTP, RELAY_WRAP_VIDEO_RTCP, RELAY_WRAP_VIDEO_RTP,
};

use crate::ffmpeg_cli::H264FileRecorder;
use crate::framing::{
    au_has_idr, is_video_payload_type, normalize_annexb_au_4byte, packetize_zalo_au,
    DepacketizeOutcome, SyntheticH264Generator, VideoDepacketizer, VIDEO_CLOCK_HZ,
    VIDEO_RTP_MAX_PAYLOAD,
};
use crate::rtcp::{
    build_rtcp_rr_pli, build_video_sr_compound, build_zrtp_incall_negotiate_packet, ntp_now,
    rtcp_requests_keyframe,
};

const OUTBOUND_AU_QUEUE_CAP: usize = 8;
const PEER_VIDEO_IDLE_TIMEOUT: Duration = Duration::from_millis(1800);
const TX_BURST_PACE_PACKETS: usize = 24;
const TX_BURST_PACE_SLEEP: Duration = Duration::from_micros(500);

type ActiveVideoSlotInner = Mutex<Option<(i64, Arc<VideoSession>)>>;
type ActiveVideoSlot = Arc<ActiveVideoSlotInner>;
type WeakVideoSlot = Weak<ActiveVideoSlotInner>;

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

/// Decoded/reassembled inbound H.264 Annex-B Access Unit delivered to a renderer (e.g. WebCodecs or recorder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundVideoFrame {
    pub call_id: i64,
    pub ssrc: u32,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub annexb_au: Vec<u8>,
    pub is_keyframe: bool,
    pub just_synced: bool,
}

pub type InboundFrameCallback = Arc<dyn Fn(InboundVideoFrame) + Send + Sync>;
pub type KeyframeRequestCallback = Arc<dyn Fn(i64) + Send + Sync>;

/// Live telemetry snapshot exposed by [`VideoSession`] and [`VideoMediaHook`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VideoTelemetry {
    pub cam_enabled: bool,
    pub peer_video_active: bool,
    pub video_synced: bool,
    pub tx_frames: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub tx_keyframes: u64,
    pub tx_errors: u64,
    pub rx_packets: u64,
    pub rx_frames: u64,
    pub rx_keyframes: u64,
    pub rx_duplicates: u64,
    pub rx_dropped_before_sync: u64,
    pub rx_dropped_incomplete: u64,
    pub pli_sent: u64,
    pub pli_received: u64,
}

/// Configuration for [`VideoSession`] and [`VideoMediaHook`].
#[derive(Clone)]
pub struct VideoSessionConfig {
    pub target_fps: u32,
    pub max_payload_bytes: usize,
    pub remb_bitrate_bps: u32,
    /// When `true` and no external frames have been pushed yet, emit valid Constrained Baseline
    /// synthetic H.264 frames while camera is enabled (used for offline loopback / `--cam` tests).
    pub send_synthetic_when_no_external_frames: bool,
    pub on_inbound_frame: Option<InboundFrameCallback>,
    pub on_keyframe_requested: Option<KeyframeRequestCallback>,
}

impl std::fmt::Debug for VideoSessionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoSessionConfig")
            .field("target_fps", &self.target_fps)
            .field("max_payload_bytes", &self.max_payload_bytes)
            .field("remb_bitrate_bps", &self.remb_bitrate_bps)
            .field(
                "send_synthetic_when_no_external_frames",
                &self.send_synthetic_when_no_external_frames,
            )
            .field("has_on_inbound_frame", &self.on_inbound_frame.is_some())
            .field(
                "has_on_keyframe_requested",
                &self.on_keyframe_requested.is_some(),
            )
            .finish()
    }
}

impl Default for VideoSessionConfig {
    fn default() -> Self {
        Self {
            target_fps: 15,
            max_payload_bytes: VIDEO_RTP_MAX_PAYLOAD,
            remb_bitrate_bps: 1_500_000,
            send_synthetic_when_no_external_frames: true,
            on_inbound_frame: None,
            on_keyframe_requested: None,
        }
    }
}

#[derive(Default)]
struct VideoRxState {
    peer_ssrc: u32,
    rx_stats: RtpRxStats,
    depacketizer: VideoDepacketizer,
    last_peer_sr: Option<(u32, Instant)>,
    last_rx_video_at: Option<Instant>,
    pli_gap: bool,
}

struct VideoRuntimeState {
    call_id: i64,
    cam_enabled: AtomicBool,
    want_keyframe: AtomicBool,
    has_external_source: AtomicBool,
    video_synced: AtomicBool,
    immediate_pli: AtomicBool,
    tx_frames: AtomicU64,
    tx_packets: AtomicU64,
    tx_bytes: AtomicU64,
    tx_keyframes: AtomicU64,
    tx_errors: AtomicU64,
    rx_packets: AtomicU64,
    rx_frames: AtomicU64,
    rx_keyframes: AtomicU64,
    rx_duplicates: AtomicU64,
    rx_dropped_before_sync: AtomicU64,
    rx_dropped_incomplete: AtomicU64,
    pli_sent: AtomicU64,
    pli_received: AtomicU64,
    outbound_aus: Mutex<VecDeque<Vec<u8>>>,
    rx_state: Mutex<VideoRxState>,
}

impl VideoRuntimeState {
    fn new(call_id: i64, initial_cam_enabled: bool) -> Self {
        Self {
            call_id,
            cam_enabled: AtomicBool::new(initial_cam_enabled),
            want_keyframe: AtomicBool::new(true),
            has_external_source: AtomicBool::new(false),
            video_synced: AtomicBool::new(false),
            immediate_pli: AtomicBool::new(false),
            tx_frames: AtomicU64::new(0),
            tx_packets: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
            tx_keyframes: AtomicU64::new(0),
            tx_errors: AtomicU64::new(0),
            rx_packets: AtomicU64::new(0),
            rx_frames: AtomicU64::new(0),
            rx_keyframes: AtomicU64::new(0),
            rx_duplicates: AtomicU64::new(0),
            rx_dropped_before_sync: AtomicU64::new(0),
            rx_dropped_incomplete: AtomicU64::new(0),
            pli_sent: AtomicU64::new(0),
            pli_received: AtomicU64::new(0),
            outbound_aus: Mutex::new(VecDeque::with_capacity(OUTBOUND_AU_QUEUE_CAP)),
            rx_state: Mutex::new(VideoRxState::default()),
        }
    }

    fn snapshot(&self) -> VideoTelemetry {
        let peer_video_active = self
            .rx_state
            .lock()
            .ok()
            .and_then(|g| g.last_rx_video_at)
            .is_some_and(|t| t.elapsed() < PEER_VIDEO_IDLE_TIMEOUT);
        VideoTelemetry {
            cam_enabled: self.cam_enabled.load(Ordering::Relaxed),
            peer_video_active,
            video_synced: self.video_synced.load(Ordering::Relaxed),
            tx_frames: self.tx_frames.load(Ordering::Relaxed),
            tx_packets: self.tx_packets.load(Ordering::Relaxed),
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed),
            tx_keyframes: self.tx_keyframes.load(Ordering::Relaxed),
            tx_errors: self.tx_errors.load(Ordering::Relaxed),
            rx_packets: self.rx_packets.load(Ordering::Relaxed),
            rx_frames: self.rx_frames.load(Ordering::Relaxed),
            rx_keyframes: self.rx_keyframes.load(Ordering::Relaxed),
            rx_duplicates: self.rx_duplicates.load(Ordering::Relaxed),
            rx_dropped_before_sync: self.rx_dropped_before_sync.load(Ordering::Relaxed),
            rx_dropped_incomplete: self.rx_dropped_incomplete.load(Ordering::Relaxed),
            pli_sent: self.pli_sent.load(Ordering::Relaxed),
            pli_received: self.pli_received.load(Ordering::Relaxed),
        }
    }
}

/// Active 1-1 video pipeline handling TX pacing, H.264/ZFrag framing, RTCP SR/PLI/REMB,
/// ZRTP `IncallNegotiateInfo`, and inbound video depacketization.
pub struct VideoSession {
    stop: Arc<AtomicBool>,
    state: Arc<VideoRuntimeState>,
    on_inbound_frame: Option<InboundFrameCallback>,
    on_keyframe_requested: Option<KeyframeRequestCallback>,
    worker: Mutex<Option<JoinHandle<()>>>,
    owner_slot: Option<WeakVideoSlot>,
}

impl VideoSession {
    /// Start a live video session over the provided negotiated UDP relay context.
    pub fn start(ctx: MediaTransportContext, cfg: VideoSessionConfig) -> Self {
        Self::start_inner(ctx, cfg, None)
    }

    fn start_inner(
        ctx: MediaTransportContext,
        cfg: VideoSessionConfig,
        owner_slot: Option<WeakVideoSlot>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(VideoRuntimeState::new(
            ctx.call_id,
            ctx.initial_cam_enabled,
        ));
        let on_inbound_frame = cfg.on_inbound_frame.clone();
        let on_keyframe_requested = cfg.on_keyframe_requested.clone();

        let stop_w = stop.clone();
        let state_w = state.clone();
        let worker = thread::Builder::new()
            .name(format!("teza-video-{}", ctx.call_id))
            .spawn(move || {
                run_video_tx_loop(stop_w, state_w, ctx, cfg);
            })
            .ok();

        Self {
            stop,
            state,
            on_inbound_frame,
            on_keyframe_requested,
            worker: Mutex::new(worker),
            owner_slot,
        }
    }

    /// Returns `true` once [`stop`](AuxiliaryMediaSession::stop) has been invoked on this session.
    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Push a raw or normalized H.264 Annex-B Access Unit into the outbound send queue.
    ///
    /// Once called, `has_external_source` is marked true so synthetic frames are never
    /// interleaved with a real encoder's reference chain.
    pub fn push_outbound_au(&self, annexb_au: &[u8]) -> bool {
        if self.is_stopped() {
            return false;
        }
        let norm = normalize_annexb_au_4byte(annexb_au);
        if norm.is_empty() {
            return false;
        }
        self.state
            .has_external_source
            .store(true, Ordering::Relaxed);
        if !self.state.cam_enabled.load(Ordering::Relaxed) {
            return false;
        }
        let (pushed, evicted) = if let Ok(mut q) = self.state.outbound_aus.lock() {
            let mut evicted = false;
            if q.len() >= OUTBOUND_AU_QUEUE_CAP {
                if let Some(pos) = q.iter().position(|au| !au_has_idr(au)) {
                    q.remove(pos);
                } else {
                    q.pop_front();
                }
                evicted = true;
            }
            q.push_back(norm);
            (true, evicted)
        } else {
            (false, false)
        };
        if evicted {
            self.state.want_keyframe.store(true, Ordering::Relaxed);
            if let Some(ref cb) = self.on_keyframe_requested {
                cb(self.state.call_id);
            }
        }
        pushed
    }

    /// Reset inbound depacketizer sync and schedule an immediate RTCP PLI to the remote peer.
    pub fn request_remote_keyframe(&self) {
        if self.is_stopped() {
            return;
        }
        self.state.video_synced.store(false, Ordering::Relaxed);
        if let Ok(mut rx) = self.state.rx_state.lock() {
            rx.depacketizer.reset_sync();
            rx.pli_gap = true;
        }
        self.state.immediate_pli.store(true, Ordering::Relaxed);
    }

    pub fn telemetry(&self) -> VideoTelemetry {
        self.state.snapshot()
    }

    fn shutdown_worker(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(mut g) = self.worker.lock() {
            if let Some(h) = g.take() {
                let _ = h.join();
            }
        }
        // Clear any owning `VideoMediaHook::active` slot pointing to this stopped session,
        // dropping the detached `Arc<VideoSession>` outside the mutex to prevent re-entrancy.
        let evicted = self
            .owner_slot
            .as_ref()
            .and_then(|w| w.upgrade())
            .and_then(|slot| {
                slot.lock().ok().and_then(|mut g| {
                    if g.as_ref()
                        .is_some_and(|(_, s)| std::ptr::eq(Arc::as_ptr(s), self))
                    {
                        g.take()
                    } else {
                        None
                    }
                })
            });
        drop(evicted);
    }
}

impl Drop for VideoSession {
    fn drop(&mut self) {
        self.shutdown_worker();
    }
}

impl AuxiliaryMediaSession for VideoSession {
    fn on_incoming_rtp(&self, outer_byte: u8, inner_rtp: &[u8]) {
        let Some(rtp) = parse_rtp_packet(inner_rtp) else {
            return;
        };
        if !is_video_payload_type(rtp.payload_type) && outer_byte != RELAY_INCOMING_VIDEO_RTP {
            return;
        }
        if rtp.payload.is_empty() {
            return;
        }

        let emitted: Option<InboundVideoFrame> = {
            let Ok(mut rx) = self.state.rx_state.lock() else {
                return;
            };
            if rx.peer_ssrc != 0 && rtp.ssrc != 0 && rtp.ssrc != rx.peer_ssrc {
                rx.rx_stats = RtpRxStats::default();
                rx.depacketizer.reset_sync();
                self.state.video_synced.store(false, Ordering::Relaxed);
            }
            if rtp.ssrc != 0 {
                rx.peer_ssrc = rtp.ssrc;
            }
            let prev_inc = rx.depacketizer.dropped_incomplete();
            let outcome = rx.depacketizer.push_rtp(
                rtp.ssrc,
                rtp.sequence_number,
                rtp.timestamp,
                rtp.payload,
            );
            let new_inc = rx.depacketizer.dropped_incomplete();
            if new_inc > prev_inc {
                self.state
                    .rx_dropped_incomplete
                    .fetch_add(new_inc - prev_inc, Ordering::Relaxed);
                rx.pli_gap = true;
            }
            if matches!(outcome, DepacketizeOutcome::Duplicate) {
                self.state.rx_duplicates.fetch_add(1, Ordering::Relaxed);
                return;
            }

            self.state.rx_packets.fetch_add(1, Ordering::Relaxed);
            rx.last_rx_video_at = Some(Instant::now());
            if rx.rx_stats.update(rtp.sequence_number) {
                rx.pli_gap = true;
            }

            match outcome {
                DepacketizeOutcome::Duplicate | DepacketizeOutcome::Incomplete => None,
                DepacketizeOutcome::DroppedBeforeSync => {
                    self.state
                        .rx_dropped_before_sync
                        .fetch_add(1, Ordering::Relaxed);
                    None
                }
                DepacketizeOutcome::AccessUnit {
                    au,
                    is_keyframe,
                    just_synced,
                } => {
                    self.state.video_synced.store(true, Ordering::Relaxed);
                    self.state.rx_frames.fetch_add(1, Ordering::Relaxed);
                    if is_keyframe {
                        self.state.rx_keyframes.fetch_add(1, Ordering::Relaxed);
                    }
                    Some(InboundVideoFrame {
                        call_id: self.state.call_id,
                        ssrc: rtp.ssrc,
                        sequence_number: rtp.sequence_number,
                        timestamp: rtp.timestamp,
                        annexb_au: au,
                        is_keyframe,
                        just_synced,
                    })
                }
            }
        };

        if let Some(frame) = emitted {
            if let Some(ref cb) = self.on_inbound_frame {
                cb(frame);
            }
        }
    }

    fn on_incoming_rtcp(&self, outer_byte: u8, inner_rtcp: &[u8]) {
        let is_audio_outer = matches!(
            outer_byte,
            RELAY_WRAP_AUDIO_RTCP | RELAY_WRAP_AUDIO_RTP | 0x04
        );
        let is_video_outer = matches!(
            outer_byte,
            RELAY_WRAP_VIDEO_RTCP | RELAY_WRAP_VIDEO_RTP | RELAY_INCOMING_VIDEO_RTP
        );
        if !is_audio_outer {
            if let Some((sr_ssrc, mid_ntp)) = rtcp_sr_sender_and_middle_ntp(inner_rtcp) {
                if let Ok(mut rx) = self.state.rx_state.lock() {
                    let matches_video = if is_video_outer {
                        rx.peer_ssrc == 0 || sr_ssrc == rx.peer_ssrc
                    } else {
                        rx.peer_ssrc != 0 && sr_ssrc == rx.peer_ssrc
                    };
                    if matches_video {
                        rx.last_peer_sr = Some((mid_ntp, Instant::now()));
                    }
                }
            }
        }
        if rtcp_requests_keyframe(inner_rtcp) {
            self.state.pli_received.fetch_add(1, Ordering::Relaxed);
            self.state.want_keyframe.store(true, Ordering::Relaxed);
            if let Some(ref cb) = self.on_keyframe_requested {
                cb(self.state.call_id);
            }
        }
    }

    fn set_cam_enabled(&self, enabled: bool) {
        let was = self.state.cam_enabled.swap(enabled, Ordering::Relaxed);
        if enabled && !was {
            self.state.want_keyframe.store(true, Ordering::Relaxed);
        }
        if !enabled {
            if let Ok(mut q) = self.state.outbound_aus.lock() {
                q.clear();
            }
        }
    }

    fn request_remote_keyframe(&self) {
        VideoSession::request_remote_keyframe(self);
    }

    fn stop(&self) {
        self.shutdown_worker();
    }
}

fn send_rtcp_wire(
    ctx: &MediaTransportContext,
    inner_rtcp: &[u8],
    tx_errors: &AtomicU64,
) {
    let (tok, target, use_rtcp_sock) = ctx.resolve_rtcp_send_route();
    let wire = if tok != 0 {
        wrap_relay_video_rtcp(tok, inner_rtcp)
    } else {
        inner_rtcp.to_vec()
    };
    let sender = if use_rtcp_sock {
        ctx.udp_rtcp_sock.as_ref().unwrap_or(&ctx.udp_rtp_sock)
    } else {
        &ctx.udp_rtp_sock
    };
    if sender.send_to(&wire, target).is_err() {
        tx_errors.fetch_add(1, Ordering::Relaxed);
    }
    if !use_rtcp_sock && target != ctx.rtp_target {
        let _ = ctx.udp_rtp_sock.send_to(&wire, ctx.rtp_target);
    }
}

fn run_video_tx_loop(
    stop: Arc<AtomicBool>,
    state: Arc<VideoRuntimeState>,
    ctx: MediaTransportContext,
    cfg: VideoSessionConfig,
) {
    let fps = cfg.target_fps.clamp(1, 60);
    let frame_step = Duration::from_nanos(1_000_000_000 / u64::from(fps));
    let ts_step = VIDEO_CLOCK_HZ / fps;
    let ssrc = if ctx.ssrc != 0 {
        ctx.ssrc
    } else {
        0x5A41_4C56
    };

    let mut synth = SyntheticH264Generator::new();
    let mut seq: u16 = 0;
    let mut ts: u32 = 0;
    let mut tx_pkts_u32: u32 = 0;
    let mut tx_octets_u32: u32 = 0;
    let mut nego_seq: u32 = 1;

    let mut next_frame_at = Instant::now();
    let mut last_rtcp: Option<Instant> = None;
    let mut last_pli: Option<Instant> = None;

    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        let cam_on = state.cam_enabled.load(Ordering::Relaxed);

        // 1. Paced frame TX (15 fps default)
        if now >= next_frame_at {
            if now.saturating_duration_since(next_frame_at) > Duration::from_millis(250) {
                next_frame_at = now;
            }
            next_frame_at += frame_step;

            if cam_on {
                let queued = state
                    .outbound_aus
                    .lock()
                    .ok()
                    .and_then(|mut q| q.pop_front());
                let au_opt = match queued {
                    Some(au) => Some(au),
                    None if !state.has_external_source.load(Ordering::Relaxed)
                        && cfg.send_synthetic_when_no_external_frames =>
                    {
                        let want_kf = state.want_keyframe.swap(false, Ordering::Relaxed);
                        Some(synth.next_frame(want_kf))
                    }
                    None => None,
                };

                if let Some(au) = au_opt {
                    let is_kf = au_has_idr(&au);
                    let chunks = packetize_zalo_au(&au, cfg.max_payload_bytes);
                    let last = chunks.len().saturating_sub(1);
                    let pace_burst = chunks.len() > TX_BURST_PACE_PACKETS;
                    let mut aborted = false;
                    let token = ctx.active_relay_token();
                    for (idx, (pt, payload)) in chunks.iter().enumerate() {
                        if pace_burst && idx > 0 && idx % TX_BURST_PACE_PACKETS == 0 {
                            if stop.load(Ordering::Relaxed) {
                                aborted = true;
                                break;
                            }
                            thread::sleep(TX_BURST_PACE_SLEEP);
                        }
                        let rtp = build_rtp_packet(*pt, idx == last, seq, ts, ssrc, payload);
                        let wire = if token != 0 {
                            wrap_relay_video_rtp(token, &rtp)
                        } else {
                            rtp
                        };
                        match ctx.udp_rtp_sock.send_to(&wire, ctx.rtp_target) {
                            Ok(_) => {
                                state.tx_packets.fetch_add(1, Ordering::Relaxed);
                                state
                                    .tx_bytes
                                    .fetch_add(payload.len() as u64, Ordering::Relaxed);
                                tx_pkts_u32 = tx_pkts_u32.wrapping_add(1);
                                tx_octets_u32 =
                                    tx_octets_u32.wrapping_add(payload.len() as u32);
                            }
                            Err(_) => {
                                state.tx_errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        seq = seq.wrapping_add(1);
                    }
                    if !aborted {
                        state.tx_frames.fetch_add(1, Ordering::Relaxed);
                        if is_kf {
                            state.tx_keyframes.fetch_add(1, Ordering::Relaxed);
                        }
                        ts = ts.wrapping_add(ts_step);
                    }
                }
            }
        }

        // 2. Peer video idle reset (> 1.8 s with no inbound video RTP)
        if let Ok(mut rx) = state.rx_state.lock() {
            if rx
                .last_rx_video_at
                .is_some_and(|t| now.saturating_duration_since(t) >= PEER_VIDEO_IDLE_TIMEOUT)
            {
                rx.last_rx_video_at = None;
                rx.depacketizer.reset_sync();
                state.video_synced.store(false, Ordering::Relaxed);
            }
        }

        // 3. Periodic compound RTCP SR/RR + SDES + REMB + ZRTP IncallNegotiateInfo (every 2s),
        //    sent for active DirectVideo sessions even when local camera is off (receive-only).
        if last_rtcp.is_none_or(|t| t.elapsed() >= Duration::from_secs(2)) {
            last_rtcp = Some(Instant::now());
            let sr_info = state.rx_state.lock().ok().map(|mut rx| {
                let peer_ssrc = rx.peer_ssrc;
                let (lsr, dlsr) = match rx.last_peer_sr {
                    Some((m, at)) => (m, (at.elapsed().as_secs_f64() * 65_536.0) as u32),
                    None => (0, 0),
                };
                let block = rx.rx_stats.report_block(peer_ssrc, lsr, dlsr);
                (peer_ssrc, block)
            });
            if let Some((peer_ssrc, block)) = sr_info {
                let remb_target = if peer_ssrc != 0 {
                    peer_ssrc
                } else {
                    ctx.partner_uid_u32
                };
                let compound = build_video_sr_compound(
                    ssrc,
                    remb_target,
                    ntp_now(),
                    ts,
                    tx_pkts_u32,
                    tx_octets_u32,
                    block,
                    cfg.remb_bitrate_bps,
                );
                send_rtcp_wire(&ctx, &compound, &state.tx_errors);
            }
            if ctx.call_type == CallType::DirectVideo {
                let tok = ctx.active_relay_token();
                if tok != 0 {
                    let nego =
                        build_zrtp_incall_negotiate_packet(ctx.partner_uid_u32, tok, nego_seq);
                    nego_seq = nego_seq.wrapping_add(1);
                    let _ = ctx.udp_rtp_sock.send_to(&nego, ctx.rtp_target);
                }
            }
        }

        // 4. Rate-limited PLI when unsynced or after incoming RTP sequence gap (<= 2 Hz),
        //    or immediately on explicit keyframe recovery request (coalesced at >= 50 ms).
        let want_immediate = state.immediate_pli.load(Ordering::Relaxed);
        let pli_due = last_pli.is_none_or(|t| {
            let elapsed = t.elapsed();
            (want_immediate && elapsed >= Duration::from_millis(50))
                || elapsed >= Duration::from_millis(500)
        });
        if pli_due {
            let pli_peer = state.rx_state.lock().ok().and_then(|mut rx| {
                let synced = state.video_synced.load(Ordering::Relaxed);
                let target_ssrc = if rx.peer_ssrc != 0 {
                    rx.peer_ssrc
                } else if want_immediate {
                    ctx.partner_uid_u32
                } else {
                    0
                };
                let need = target_ssrc != 0 && (want_immediate || !synced || rx.pli_gap);
                if need {
                    rx.pli_gap = false;
                    Some(target_ssrc)
                } else {
                    None
                }
            });
            if let Some(peer_ssrc) = pli_peer {
                state.immediate_pli.store(false, Ordering::Relaxed);
                last_pli = Some(Instant::now());
                let rr_pli = build_rtcp_rr_pli(ssrc, peer_ssrc);
                send_rtcp_wire(&ctx, &rr_pli, &state.tx_errors);
                state.pli_sent.fetch_add(1, Ordering::Relaxed);
            } else if want_immediate {
                state.immediate_pli.store(false, Ordering::Relaxed);
            }
        }

        thread::sleep(Duration::from_millis(5));
    }
}

/// [`AuxiliaryMediaHook`] factory attaching [`VideoSession`] onto `CallType::DirectVideo` calls
/// while leaving `CallType::DirectVoice` audio calls untouched (`None`).
pub struct VideoMediaHook {
    config: VideoSessionConfig,
    active: ActiveVideoSlot,
}

impl VideoMediaHook {
    pub fn new(config: VideoSessionConfig) -> Self {
        Self {
            config,
            active: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_default_config() -> Self {
        Self::new(VideoSessionConfig::default())
    }

    /// Return the `call_id` of the currently running (non-stopped) [`VideoSession`], if any.
    pub fn active_call_id(&self) -> Option<i64> {
        self.active.lock().ok().and_then(|g| {
            g.as_ref()
                .and_then(|(cid, s)| (!s.is_stopped()).then_some(*cid))
        })
    }

    /// Push an encoded Annex-B Access Unit into the currently active [`VideoSession`].
    pub fn push_outbound_au(&self, annexb_au: &[u8]) -> bool {
        let sess = self
            .active
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|(_, s)| s.clone()));
        sess.is_some_and(|s| s.push_outbound_au(annexb_au))
    }

    /// Push an encoded Annex-B Access Unit only when a live (non-stopped) [`VideoSession`]
    /// matches `call_id` (or any live session when `call_id <= 0`).
    pub fn push_outbound_au_for_call(&self, call_id: i64, annexb_au: &[u8]) -> bool {
        let sess = self.active.lock().ok().and_then(|g| {
            g.as_ref().and_then(|(cid, s)| {
                ((call_id <= 0 || *cid == call_id) && !s.is_stopped()).then(|| s.clone())
            })
        });
        sess.is_some_and(|s| s.push_outbound_au(annexb_au))
    }

    pub fn telemetry(&self) -> Option<VideoTelemetry> {
        let sess = self.active.lock().ok().and_then(|g| {
            g.as_ref()
                .and_then(|(_, s)| (!s.is_stopped()).then(|| s.clone()))
        });
        sess.map(|s| s.telemetry())
    }

    /// Return live telemetry only when an active (non-stopped) [`VideoSession`] matches `call_id`.
    pub fn telemetry_for_call(&self, call_id: i64) -> Option<VideoTelemetry> {
        let sess = self.active.lock().ok().and_then(|g| {
            g.as_ref()
                .and_then(|(cid, s)| (*cid == call_id && !s.is_stopped()).then(|| s.clone()))
        });
        sess.map(|s| s.telemetry())
    }

    pub fn set_cam_enabled(&self, enabled: bool) {
        let sess = self
            .active
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|(_, s)| s.clone()));
        if let Some(s) = sess {
            s.set_cam_enabled(enabled);
        }
    }

    /// Reset decoder sync and trigger an immediate RTCP PLI on the active (non-stopped)
    /// [`VideoSession`] matching `call_id` (or any live session when `call_id` is `None` or `<= 0`).
    pub fn request_remote_keyframe(&self, call_id: Option<i64>) -> bool {
        let target_id = call_id.unwrap_or(0);
        let sess = self.active.lock().ok().and_then(|g| {
            g.as_ref().and_then(|(cid, s)| {
                ((target_id <= 0 || *cid == target_id) && !s.is_stopped()).then(|| s.clone())
            })
        });
        if let Some(s) = sess {
            s.request_remote_keyframe();
            true
        } else {
            false
        }
    }

    pub fn stop_active(&self) {
        let prev = self.active.lock().ok().and_then(|mut g| g.take());
        if let Some((_, s)) = prev {
            s.stop();
        }
    }
}

impl AuxiliaryMediaHook for VideoMediaHook {
    fn on_media_start(&self, ctx: MediaTransportContext) -> Option<Arc<dyn AuxiliaryMediaSession>> {
        if ctx.call_type != CallType::DirectVideo {
            self.stop_active();
            return None;
        }
        let call_id = ctx.call_id;
        let sess = Arc::new(VideoSession::start_inner(
            ctx,
            self.config.clone(),
            Some(Arc::downgrade(&self.active)),
        ));
        let prev = self
            .active
            .lock()
            .ok()
            .and_then(|mut g| g.replace((call_id, sess.clone())));
        if let Some((_, old)) = prev {
            old.stop();
        }
        Some(sess)
    }
}

/// Self-contained local UDP mock-relay loopback verifying `0x0D` TX -> `0x0E` RX, `0x0F` RTCP,
/// `VideoDepacketizer` SPS/IDR sync, and optional raw Annex-B dump to `dump_path`.
pub fn run_local_video_loopback(
    duration: Duration,
    dump_path: Option<&Path>,
) -> anyhow::Result<VideoTelemetry> {
    let relay_sock = UdpSocket::bind("127.0.0.1:0")?;
    let relay_addr: SocketAddr = relay_sock.local_addr()?;
    relay_sock.set_nonblocking(true)?;

    let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0")?);
    client_sock.set_nonblocking(true)?;

    let recorder = match dump_path {
        Some(p) => Some(Arc::new(H264FileRecorder::create(p)?)),
        None => None,
    };

    let stop = Arc::new(AtomicBool::new(false));
    let stop_relay = stop.clone();
    let relay_thread = thread::Builder::new()
        .name("teza-video-mock-relay".to_string())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            while !stop_relay.load(Ordering::Relaxed) {
                match relay_sock.recv_from(&mut buf) {
                    Ok((n, peer)) => {
                        if n >= 5 && buf[0] == RELAY_WRAP_VIDEO_RTP {
                            let mut down = Vec::with_capacity(1 + (n - 5));
                            down.push(RELAY_INCOMING_VIDEO_RTP);
                            down.extend_from_slice(&buf[5..n]);
                            let _ = relay_sock.send_to(&down, peer);
                        } else if n >= 5 && buf[0] == RELAY_WRAP_VIDEO_RTCP {
                            let _ = relay_sock.send_to(&buf[..n], peer);
                        }
                    }
                    Err(_) => thread::sleep(Duration::from_millis(2)),
                }
            }
        })?;

    let ctx = MediaTransportContext {
        call_id: 1,
        call_type: CallType::DirectVideo,
        is_caller: true,
        from_uid_u32: 1001,
        partner_uid_u32: 2002,
        ssrc: 0x1001_2002,
        rtp_target: relay_addr,
        rtcp_target: Some(relay_addr),
        relay_token: 0xCAFE_BABE,
        live_relay_token: None,
        live_rtcp_relay_token: None,
        udp_rtp_sock: client_sock.clone(),
        udp_rtcp_sock: None,
        initial_cam_enabled: true,
    };

    let rec_cb = recorder.clone();
    let cfg = VideoSessionConfig {
        on_inbound_frame: rec_cb.map(|r| -> InboundFrameCallback {
            Arc::new(move |frame| {
                let _ = r.write_au(&frame.annexb_au);
            })
        }),
        ..Default::default()
    };
    let sess = Arc::new(VideoSession::start(ctx, cfg));

    let stop_rx = stop.clone();
    let sess_rx = sess.clone();
    let rx_thread = thread::Builder::new()
        .name("teza-video-loopback-rx".to_string())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            while !stop_rx.load(Ordering::Relaxed) {
                match client_sock.recv_from(&mut buf) {
                    Ok((n, _)) => {
                        if let Some(inner) = unwrap_relay_media(&buf[..n]) {
                            if teza_call::is_rtcp_packet(inner) {
                                sess_rx.on_incoming_rtcp(buf[0], inner);
                            } else {
                                sess_rx.on_incoming_rtp(buf[0], inner);
                            }
                        }
                    }
                    Err(_) => thread::sleep(Duration::from_millis(2)),
                }
            }
        })?;

    thread::sleep(duration.max(Duration::from_millis(200)));
    let telem = sess.telemetry();
    stop.store(true, Ordering::Relaxed);
    sess.stop();
    let _ = rx_thread.join();
    let _ = relay_thread.join();
    Ok(telem)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn video_loopback_transfers_keyframes_delta_frames_and_rtcp() {
        let telem = run_local_video_loopback(Duration::from_millis(450), None).unwrap();
        assert!(telem.cam_enabled);
        assert!(telem.peer_video_active);
        assert!(telem.video_synced, "expected synced video stream: {telem:?}");
        assert!(telem.tx_frames >= 3, "expected >=3 TX frames: {telem:?}");
        assert!(telem.rx_frames >= 3, "expected >=3 RX frames: {telem:?}");
        assert!(
            telem.rx_keyframes >= 1,
            "expected >=1 RX keyframe: {telem:?}"
        );
    }

    #[test]
    fn sequence_gap_triggers_pli_and_incoming_pli_forces_keyframe_and_callback() {
        let relay_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay_sock.local_addr().unwrap();
        relay_sock.set_nonblocking(true).unwrap();

        let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        client_sock.set_nonblocking(true).unwrap();

        let kf_cb_count = Arc::new(AtomicUsize::new(0));
        let kf_cb_clone = kf_cb_count.clone();
        let inbound_count = Arc::new(AtomicUsize::new(0));
        let inbound_clone = inbound_count.clone();

        let cfg = VideoSessionConfig {
            send_synthetic_when_no_external_frames: true,
            on_inbound_frame: Some(Arc::new(move |_f| {
                inbound_clone.fetch_add(1, Ordering::Relaxed);
            })),
            on_keyframe_requested: Some(Arc::new(move |_cid| {
                kf_cb_clone.fetch_add(1, Ordering::Relaxed);
            })),
            ..Default::default()
        };

        let ctx = MediaTransportContext {
            call_id: 42,
            call_type: CallType::DirectVideo,
            is_caller: true,
            from_uid_u32: 1001,
            partner_uid_u32: 2002,
            ssrc: 0x1234_5678,
            rtp_target: relay_addr,
            rtcp_target: Some(relay_addr),
            relay_token: 0xDEAD_BEEF,
            live_relay_token: None,
            live_rtcp_relay_token: None,
            udp_rtp_sock: client_sock.clone(),
            udp_rtcp_sock: None,
            initial_cam_enabled: true,
        };
        let sess = VideoSession::start(ctx, cfg);

        // Inject a P-frame at seq=1 then seq=5 (gap -> pli_gap=true) so outbound PLI fires
        let p_pkt1 = build_rtp_packet(
            crate::framing::VIDEO_PT_DELTA,
            true,
            1,
            3000,
            0x9988_7766,
            &[0, 0, 0, 1, 0x41, 0x9A],
        );
        let p_pkt5 = build_rtp_packet(
            crate::framing::VIDEO_PT_DELTA,
            true,
            5,
            15000,
            0x9988_7766,
            &[0, 0, 0, 1, 0x41, 0x9A],
        );
        sess.on_incoming_rtp(RELAY_INCOMING_VIDEO_RTP, &p_pkt1);
        sess.on_incoming_rtp(RELAY_INCOMING_VIDEO_RTP, &p_pkt5);

        // Wait for the initial GOP keyframe (PT 98) and first P-frame (PT 97) to leave TX first,
        // then inject incoming PLI so we genuinely test mid-GOP forced IDR recovery.
        let mut pt98_tx = 0usize;
        let mut pt97_tx = 0usize;
        let mut saw_pli_tx = false;
        let mut buf = [0u8; 4096];
        let warm_deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < warm_deadline && (pt98_tx < 1 || pt97_tx < 1 || !saw_pli_tx) {
            while let Ok((n, _)) = relay_sock.recv_from(&mut buf) {
                if let Some(inner) = unwrap_relay_media(&buf[..n]) {
                    if teza_call::is_rtcp_packet(inner) && rtcp_requests_keyframe(inner) {
                        saw_pli_tx = true;
                    } else if let Some(rtp) = parse_rtp_packet(inner) {
                        if rtp.payload_type == crate::framing::VIDEO_PT_KEYFRAME {
                            pt98_tx += 1;
                        } else if rtp.payload_type == crate::framing::VIDEO_PT_DELTA {
                            pt97_tx += 1;
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(10));
        }

        // Feed an incoming RTCP PLI for our SSRC -> must trigger keyframe callback & next IDR frame
        let pli = build_rtcp_rr_pli(0x9988_7766, 0x1234_5678);
        sess.on_incoming_rtcp(RELAY_WRAP_VIDEO_RTCP, &pli);

        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline && (!saw_pli_tx || pt98_tx < 2) {
            while let Ok((n, _)) = relay_sock.recv_from(&mut buf) {
                if let Some(inner) = unwrap_relay_media(&buf[..n]) {
                    if teza_call::is_rtcp_packet(inner) && rtcp_requests_keyframe(inner) {
                        saw_pli_tx = true;
                    } else if let Some(rtp) = parse_rtp_packet(inner) {
                        if rtp.payload_type == crate::framing::VIDEO_PT_KEYFRAME {
                            pt98_tx += 1;
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(10));
        }

        let telem = sess.telemetry();
        sess.stop();

        assert_eq!(kf_cb_count.load(Ordering::Relaxed), 1);
        assert_eq!(telem.pli_received, 1);
        assert!(telem.pli_sent >= 1, "expected pli_sent >= 1: {telem:?}");
        assert!(saw_pli_tx, "expected outbound RTCP PLI packet");
        assert!(pt98_tx >= 2, "expected PT 98 keyframes after PLI: {pt98_tx}");
        assert_eq!(
            telem.rx_dropped_before_sync, 2,
            "P frames before first IDR must be dropped"
        );
        assert_eq!(inbound_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn video_media_hook_ignores_voice_calls() {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let addr = sock.local_addr().unwrap();
        let hook = VideoMediaHook::with_default_config();
        let ctx = MediaTransportContext {
            call_id: 99,
            call_type: CallType::DirectVoice,
            is_caller: true,
            from_uid_u32: 1,
            partner_uid_u32: 2,
            ssrc: 1,
            rtp_target: addr,
            rtcp_target: None,
            relay_token: 0,
            live_relay_token: None,
            live_rtcp_relay_token: None,
            udp_rtp_sock: sock,
            udp_rtcp_sock: None,
            initial_cam_enabled: false,
        };
        assert!(hook.on_media_start(ctx).is_none());
        assert!(hook.telemetry().is_none());
        assert_eq!(hook.active_call_id(), None);
    }

    #[test]
    fn video_media_hook_tracks_active_call_id_and_rejects_stale_or_stopped() {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let addr = sock.local_addr().unwrap();
        let hook = VideoMediaHook::new(VideoSessionConfig {
            send_synthetic_when_no_external_frames: false,
            ..Default::default()
        });
        let ctx = MediaTransportContext {
            call_id: 777,
            call_type: CallType::DirectVideo,
            is_caller: true,
            from_uid_u32: 1,
            partner_uid_u32: 2,
            ssrc: 1,
            rtp_target: addr,
            rtcp_target: None,
            relay_token: 0,
            live_relay_token: None,
            live_rtcp_relay_token: None,
            udp_rtp_sock: sock,
            udp_rtcp_sock: None,
            initial_cam_enabled: true,
        };

        let aux = hook.on_media_start(ctx).expect("video session should start");
        assert_eq!(hook.active_call_id(), Some(777));
        assert!(hook.telemetry_for_call(777).is_some());
        assert!(hook.telemetry_for_call(888).is_none());

        let sample_au = [0u8, 0, 0, 1, 0x65, 0x88];
        assert!(!hook.push_outbound_au_for_call(888, &sample_au));
        assert!(hook.push_outbound_au_for_call(777, &sample_au));

        aux.stop();
        assert_eq!(hook.active_call_id(), None);
        assert!(
            hook.active.lock().unwrap().is_none(),
            "stopping active session must release Arc<VideoSession> from VideoMediaHook::active"
        );
        assert!(hook.telemetry().is_none());
        assert!(hook.telemetry_for_call(777).is_none());
        assert!(!hook.push_outbound_au_for_call(777, &sample_au));
        assert!(!hook.push_outbound_au(&sample_au));
    }

    #[test]
    fn request_remote_keyframe_resets_sync_and_triggers_immediate_pli() {
        let relay_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay_sock.local_addr().unwrap();
        relay_sock.set_nonblocking(true).unwrap();

        let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        client_sock.set_nonblocking(true).unwrap();

        let hook = VideoMediaHook::new(VideoSessionConfig {
            send_synthetic_when_no_external_frames: false,
            ..Default::default()
        });
        let ctx = MediaTransportContext {
            call_id: 505,
            call_type: CallType::DirectVideo,
            is_caller: true,
            from_uid_u32: 1001,
            partner_uid_u32: 2002,
            ssrc: 0x1111_2222,
            rtp_target: relay_addr,
            rtcp_target: Some(relay_addr),
            relay_token: 0xCAFE_0001,
            live_relay_token: None,
            live_rtcp_relay_token: None,
            udp_rtp_sock: client_sock,
            udp_rtcp_sock: None,
            initial_cam_enabled: false,
        };
        let aux = hook.on_media_start(ctx).expect("video session should start");

        // Sync the depacketizer with a valid keyframe from peer SSRC 0x9988_7766
        let kf = crate::framing::annexb_au(&[
            crate::framing::H264_SYNTH_SPS,
            crate::framing::H264_SYNTH_PPS,
            crate::framing::H264_SYNTH_IDR,
        ]);
        let kf_rtp = build_rtp_packet(
            crate::framing::VIDEO_PT_KEYFRAME,
            true,
            10,
            90_000,
            0x9988_7766,
            &kf,
        );
        aux.on_incoming_rtp(RELAY_INCOMING_VIDEO_RTP, &kf_rtp);
        assert!(hook.telemetry_for_call(505).unwrap().video_synced);

        // Drain any initial startup RTCP/negotiate packets
        let mut buf = [0u8; 4096];
        thread::sleep(Duration::from_millis(40));
        while relay_sock.recv_from(&mut buf).is_ok() {}

        // Wrong call_id is rejected; matching call_id resets sync and sends immediate PLI
        assert!(!hook.request_remote_keyframe(Some(999)));
        assert!(hook.request_remote_keyframe(Some(505)));
        assert!(!hook.telemetry_for_call(505).unwrap().video_synced);

        let mut saw_pli = false;
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline && !saw_pli {
            while let Ok((n, _)) = relay_sock.recv_from(&mut buf) {
                if let Some(inner) = unwrap_relay_media(&buf[..n]) {
                    if teza_call::is_rtcp_packet(inner) && rtcp_requests_keyframe(inner) {
                        saw_pli = true;
                    }
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        let telem = hook.telemetry_for_call(505).unwrap();
        assert!(saw_pli, "expected immediate RTCP PLI after request_remote_keyframe");
        assert!(telem.pli_sent >= 1);

        // Calling via trait method also works; after stop, hook returns false
        aux.request_remote_keyframe();
        aux.stop();
        assert!(!hook.request_remote_keyframe(Some(505)));
        assert!(!hook.request_remote_keyframe(None));
    }

    #[test]
    fn receive_only_direct_video_sends_incall_negotiate_and_rtcp_when_cam_disabled() {
        let relay_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay_sock.local_addr().unwrap();
        relay_sock.set_nonblocking(true).unwrap();

        let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        client_sock.set_nonblocking(true).unwrap();

        let ctx = MediaTransportContext {
            call_id: 606,
            call_type: CallType::DirectVideo,
            is_caller: false,
            from_uid_u32: 1001,
            partner_uid_u32: 2002,
            ssrc: 0x3333_4444,
            rtp_target: relay_addr,
            rtcp_target: Some(relay_addr),
            relay_token: 0xABCD_1234,
            live_relay_token: None,
            live_rtcp_relay_token: None,
            udp_rtp_sock: client_sock,
            udp_rtcp_sock: None,
            initial_cam_enabled: false,
        };
        let sess = VideoSession::start(
            ctx,
            VideoSessionConfig {
                send_synthetic_when_no_external_frames: false,
                ..Default::default()
            },
        );

        let mut saw_rtcp = false;
        let mut saw_negotiate = false;
        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + Duration::from_millis(400);
        while Instant::now() < deadline && (!saw_rtcp || !saw_negotiate) {
            while let Ok((n, _)) = relay_sock.recv_from(&mut buf) {
                if n == 36
                    && buf[0] == 0x01
                    && buf[1] == 0x01
                    && u16::from_le_bytes([buf[18], buf[19]]) == 0x0020
                    && buf[20] == 0x14
                {
                    saw_negotiate = true;
                } else if n >= 5 && buf[0] == RELAY_WRAP_VIDEO_RTCP {
                    if let Some(inner) = unwrap_relay_media(&buf[..n]) {
                        if teza_call::is_rtcp_packet(inner) {
                            saw_rtcp = true;
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(10));
        }

        sess.stop();
        assert!(
            saw_rtcp,
            "receive-only DirectVideo session (cam_enabled=false) must emit periodic video RTCP"
        );
        assert!(
            saw_negotiate,
            "receive-only DirectVideo session (cam_enabled=false) must emit IncallNegotiateInfo"
        );
    }

    #[test]
    fn outbound_queue_overflow_prefers_evicting_delta_and_requests_keyframe() {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let addr = sock.local_addr().unwrap();

        let kf_cb_count = Arc::new(AtomicUsize::new(0));
        let kf_cb_clone = kf_cb_count.clone();

        let ctx = MediaTransportContext {
            call_id: 707,
            call_type: CallType::DirectVideo,
            is_caller: true,
            from_uid_u32: 1001,
            partner_uid_u32: 2002,
            ssrc: 0x5555_6666,
            rtp_target: addr,
            rtcp_target: None,
            relay_token: 0,
            live_relay_token: None,
            live_rtcp_relay_token: None,
            udp_rtp_sock: sock,
            udp_rtcp_sock: None,
            initial_cam_enabled: true,
        };
        let sess = VideoSession::start(
            ctx,
            VideoSessionConfig {
                target_fps: 1,
                send_synthetic_when_no_external_frames: false,
                on_keyframe_requested: Some(Arc::new(move |cid| {
                    assert_eq!(cid, 707);
                    kf_cb_clone.fetch_add(1, Ordering::Relaxed);
                })),
                ..Default::default()
            },
        );

        // Wait briefly past the initial t=0 tick so next_frame_at is ~1s in the future
        thread::sleep(Duration::from_millis(40));

        let idr = crate::framing::annexb_au(&[
            crate::framing::H264_SYNTH_SPS,
            crate::framing::H264_SYNTH_PPS,
            crate::framing::H264_SYNTH_IDR,
        ]);
        let p_frame = crate::framing::annexb_au(&[crate::framing::H264_SYNTH_PSLICES[0]]);

        // Fill queue to exact capacity: [IDR, P, P, P, P, P, P, P]
        assert!(sess.push_outbound_au(&idr));
        for _ in 0..(OUTBOUND_AU_QUEUE_CAP - 1) {
            assert!(sess.push_outbound_au(&p_frame));
        }
        assert_eq!(kf_cb_count.load(Ordering::Relaxed), 0);

        // Overflow by 2 delta frames -> must evict non-IDR deltas (keeping IDR intact) and fire callback
        assert!(sess.push_outbound_au(&p_frame));
        assert!(sess.push_outbound_au(&p_frame));

        let has_idr_preserved = {
            let q = sess.state.outbound_aus.lock().unwrap();
            assert_eq!(q.len(), OUTBOUND_AU_QUEUE_CAP);
            q.front().is_some_and(|au| au_has_idr(au))
        };
        sess.stop();

        assert!(
            has_idr_preserved,
            "queue overflow must evict non-IDR delta frames before evicting an IDR keyframe"
        );
        assert_eq!(
            kf_cb_count.load(Ordering::Relaxed),
            2,
            "every overflow eviction must request a fresh encoder keyframe"
        );
    }

    #[test]
    fn video_rtcp_sr_ignores_audio_rtcp_and_mismatched_raw_ssrc() {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        let addr = sock.local_addr().unwrap();

        let ctx = MediaTransportContext {
            call_id: 808,
            call_type: CallType::DirectVideo,
            is_caller: true,
            from_uid_u32: 1001,
            partner_uid_u32: 2002,
            ssrc: 0x1001_0001,
            rtp_target: addr,
            rtcp_target: Some(addr),
            relay_token: 0xCAFE_8888,
            live_relay_token: None,
            live_rtcp_relay_token: None,
            udp_rtp_sock: sock,
            udp_rtcp_sock: None,
            initial_cam_enabled: false,
        };
        let sess = VideoSession::start(
            ctx,
            VideoSessionConfig {
                send_synthetic_when_no_external_frames: false,
                ..Default::default()
            },
        );

        // Learn video peer SSRC = 0x7777_8888 from an inbound video RTP packet
        let vid_rtp = build_rtp_packet(
            crate::framing::VIDEO_PT_DELTA,
            true,
            1,
            3000,
            0x7777_8888,
            &[0, 0, 0, 1, 0x41, 0x9A],
        );
        sess.on_incoming_rtp(RELAY_INCOMING_VIDEO_RTP, &vid_rtp);

        // 1. Video RTCP SR (0x0F) with matching SSRC updates video last_peer_sr
        let video_sr = crate::rtcp::build_rtcp_sr(
            0x7777_8888,
            (0x1111_2222, 0x3333_4444),
            90_000,
            10,
            5000,
            None,
        );
        sess.on_incoming_rtcp(RELAY_WRAP_VIDEO_RTCP, &video_sr);
        assert_eq!(
            sess.state.rx_state.lock().unwrap().last_peer_sr.map(|(m, _)| m),
            Some(0x2222_3333)
        );

        // 2. Audio RTCP SR (0x05) — even with the SAME SSRC — must NOT clobber video last_peer_sr
        let audio_sr = crate::rtcp::build_rtcp_sr(
            0x7777_8888,
            (0x1111_AAAA, 0xBBBB_4444),
            16_000,
            50,
            1200,
            None,
        );
        sess.on_incoming_rtcp(RELAY_WRAP_AUDIO_RTCP, &audio_sr);
        assert_eq!(
            sess.state.rx_state.lock().unwrap().last_peer_sr.map(|(m, _)| m),
            Some(0x2222_3333),
            "0x05 audio RTCP SR must never clobber video last_peer_sr"
        );

        // 3. Raw (P2P) RTCP SR from audio SSRC (0x9999_0000 != 0x7777_8888) must NOT clobber
        let raw_audio_sr = crate::rtcp::build_rtcp_sr(
            0x9999_0000,
            (0x1111_CCCC, 0xDDDD_4444),
            16_000,
            50,
            1200,
            None,
        );
        sess.on_incoming_rtcp(raw_audio_sr[0], &raw_audio_sr);
        assert_eq!(
            sess.state.rx_state.lock().unwrap().last_peer_sr.map(|(m, _)| m),
            Some(0x2222_3333)
        );

        // 4. Raw (P2P) RTCP SR from matching video SSRC DOES update video last_peer_sr
        let raw_video_sr = crate::rtcp::build_rtcp_sr(
            0x7777_8888,
            (0x1111_5555, 0x6666_4444),
            93_000,
            11,
            5500,
            None,
        );
        sess.on_incoming_rtcp(raw_video_sr[0], &raw_video_sr);
        assert_eq!(
            sess.state.rx_state.lock().unwrap().last_peer_sr.map(|(m, _)| m),
            Some(0x5555_6666)
        );

        sess.stop();
    }

    #[test]
    fn replacing_hook_session_does_not_clear_new_active_when_old_stops_and_large_keyframe_paces() {
        let relay_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay_sock.local_addr().unwrap();
        relay_sock.set_nonblocking(true).unwrap();

        let client_sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        client_sock.set_nonblocking(true).unwrap();

        let hook = VideoMediaHook::new(VideoSessionConfig {
            target_fps: 30,
            max_payload_bytes: 200,
            send_synthetic_when_no_external_frames: false,
            ..Default::default()
        });
        let make_ctx = |call_id: i64| MediaTransportContext {
            call_id,
            call_type: CallType::DirectVideo,
            is_caller: true,
            from_uid_u32: 1,
            partner_uid_u32: 2,
            ssrc: 0xAAAA_BBBB,
            rtp_target: relay_addr,
            rtcp_target: None,
            relay_token: 0xCAFE_1111,
            live_relay_token: None,
            live_rtcp_relay_token: None,
            udp_rtp_sock: client_sock.clone(),
            udp_rtcp_sock: None,
            initial_cam_enabled: true,
        };

        let old_aux = hook.on_media_start(make_ctx(901)).unwrap();
        let new_aux = hook.on_media_start(make_ctx(902)).unwrap();
        // Stopping old_aux again after replacement must NOT clear call 902 from hook.active
        old_aux.stop();
        assert_eq!(hook.active_call_id(), Some(902));
        assert!(hook.telemetry().is_some());

        // Push a large keyframe (>24 packets at max_payload_bytes=200) to verify burst pacing path
        let mut big_idr = vec![0x65u8; 6_000];
        big_idr[0] = 0x65;
        let big_au = crate::framing::annexb_au(&[
            crate::framing::H264_SYNTH_SPS,
            crate::framing::H264_SYNTH_PPS,
            &big_idr,
        ]);
        let expected_pkts = packetize_zalo_au(&big_au, 200).len();
        assert!(
            expected_pkts > TX_BURST_PACE_PACKETS,
            "test AU must exceed pacing threshold ({expected_pkts} <= {TX_BURST_PACE_PACKETS})"
        );
        assert!(hook.push_outbound_au_for_call(902, &big_au));

        let deadline = Instant::now() + Duration::from_millis(600);
        while Instant::now() < deadline {
            if hook
                .telemetry_for_call(902)
                .is_some_and(|t| t.tx_frames >= 1)
            {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let telem = hook.telemetry_for_call(902).unwrap();
        assert_eq!(telem.tx_frames, 1);
        assert_eq!(telem.tx_keyframes, 1);
        assert_eq!(telem.tx_packets as usize, expected_pkts);

        new_aux.stop();
        assert!(hook.active.lock().unwrap().is_none());
        assert!(hook.telemetry().is_none());
    }
}
