//! Video-agnostic auxiliary media hook traits and UDP relay transport context.
//!
//! Keeps `teza-call` decoupled from video codecs, packetization, and capture
//! while allowing external crates such as `teza-video` to attach onto the
//! negotiated ZRTP UDP relay sockets and RTCP/RTP demux path.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::signaling::CallType;

/// Negotiated UDP socket and relay metadata passed to an [`AuxiliaryMediaHook`]
/// when a connected call initializes its media pipeline.
pub struct MediaTransportContext {
    pub call_id: i64,
    pub call_type: CallType,
    pub is_caller: bool,
    pub from_uid_u32: u32,
    pub partner_uid_u32: u32,
    pub ssrc: u32,
    pub rtp_target: SocketAddr,
    pub rtcp_target: Option<SocketAddr>,
    pub relay_token: u32,
    pub live_relay_token: Option<Arc<AtomicU32>>,
    pub live_rtcp_relay_token: Option<Arc<AtomicU32>>,
    pub udp_rtp_sock: Arc<UdpSocket>,
    pub udp_rtcp_sock: Option<Arc<UdpSocket>>,
    pub initial_cam_enabled: bool,
}

impl MediaTransportContext {
    /// Return the active 32-bit relay token (`live_relay_token` if nonzero, else static `relay_token`).
    pub fn active_relay_token(&self) -> u32 {
        self.live_relay_token
            .as_ref()
            .map(|a| a.load(Ordering::Relaxed))
            .filter(|&t| t != 0)
            .unwrap_or(self.relay_token)
    }

    /// Resolve `(relay_token, target_addr, use_rtcp_sock)` for outbound auxiliary RTCP packets.
    ///
    /// Uses `udp_rtcp_sock` only when `rtcp_target` differs from `rtp_target`, the RTCP relay
    /// port has acknowledged `InitZRTP` (`live_rtcp_relay_token != 0`), and `udp_rtcp_sock` exists.
    pub fn resolve_rtcp_send_route(&self) -> (u32, SocketAddr, bool) {
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

/// Active auxiliary media session (for example a `VideoSession`) attached to a call.
pub trait AuxiliaryMediaSession: Send + Sync {
    /// Called synchronously from the UDP RX loop for every non-Opus RFC 3550 RTP v2 packet.
    fn on_incoming_rtp(&self, outer_byte: u8, inner_rtp: &[u8]);
    /// Called synchronously from the UDP RX loop for every RTCP v2 packet.
    fn on_incoming_rtcp(&self, outer_byte: u8, inner_rtcp: &[u8]);
    /// Toggle local camera transmission state (`12006` / local TX gate).
    fn set_cam_enabled(&self, enabled: bool);
    /// Request an immediate keyframe (RTCP PLI) from the remote peer and reset decoder sync.
    fn request_remote_keyframe(&self) {}
    /// Stop all worker threads/resources owned by this session cleanly.
    fn stop(&self);
}

/// Factory hook registered on [`crate::CallEngine`] to attach auxiliary media streams.
pub trait AuxiliaryMediaHook: Send + Sync {
    /// Invoked once media sockets are bound for a call. Returning `None` leaves the call audio-only.
    fn on_media_start(&self, ctx: MediaTransportContext) -> Option<Arc<dyn AuxiliaryMediaSession>>;
}
