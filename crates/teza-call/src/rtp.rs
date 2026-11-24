//! RFC 3550 RTP/RTCP framing, reception statistics, and sequence-ordered audio jitter buffer.

use std::collections::BTreeMap;

/// Zalo 1-1 Opus 16 kHz mono payload type (`opus/16000/1`).
pub const OPUS_AUDIO_PT: u8 = 112;

/// Standard audio clock rate for Zalo 1-1 Opus RTP streams (16 kHz).
pub const AUDIO_SAMPLE_RATE: u32 = 16_000;

/// 20 ms frame size at 16 kHz mono.
pub const AUDIO_FRAME_SAMPLES: usize = 320;

/// Minimum RFC 3550 RTP header size.
pub const RTP_HEADER_LEN: usize = 12;

/// Parsed RFC 3550 RTP v2 packet view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpPacket<'a> {
    pub version: u8,
    pub padding: bool,
    pub extension: bool,
    pub csrc_count: u8,
    pub marker: bool,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: &'a [u8],
}

/// Build a 12-byte-header RFC 3550 RTP v2 packet.
pub fn build_rtp_packet(
    payload_type: u8,
    marker: bool,
    seq: u16,
    timestamp: u32,
    ssrc: u32,
    payload: &[u8],
) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(RTP_HEADER_LEN + payload.len());
    pkt.push(0x80); // V=2, P=0, X=0, CC=0
    pkt.push(if marker {
        0x80 | (payload_type & 0x7F)
    } else {
        payload_type & 0x7F
    });
    pkt.extend_from_slice(&seq.to_be_bytes());
    pkt.extend_from_slice(&timestamp.to_be_bytes());
    pkt.extend_from_slice(&ssrc.to_be_bytes());
    pkt.extend_from_slice(payload);
    pkt
}

/// Parse an RFC 3550 RTP v2 packet, validating CSRC list, header extension, and padding.
pub fn parse_rtp_packet(buf: &[u8]) -> Option<RtpPacket<'_>> {
    if buf.len() < RTP_HEADER_LEN {
        return None;
    }
    let version = buf[0] >> 6;
    if version != 2 {
        return None;
    }
    // Reject RTCP packet types (RFC 5761 200..=207 -> 72..=79 masked with marker bit)
    if (200..=207).contains(&buf[1]) {
        return None;
    }
    let padding = (buf[0] & 0x20) != 0;
    let extension = (buf[0] & 0x10) != 0;
    let csrc_count = buf[0] & 0x0F;
    let marker = (buf[1] & 0x80) != 0;
    let payload_type = buf[1] & 0x7F;
    let sequence_number = u16::from_be_bytes([buf[2], buf[3]]);
    let timestamp = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let ssrc = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);

    let mut offset = RTP_HEADER_LEN + (csrc_count as usize) * 4;
    if offset > buf.len() {
        return None;
    }
    if extension {
        if offset + 4 > buf.len() {
            return None;
        }
        let ext_words = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]) as usize;
        offset = offset.checked_add(4 + ext_words * 4)?;
        if offset > buf.len() {
            return None;
        }
    }
    let mut end = buf.len();
    if padding {
        if end <= offset {
            return None;
        }
        let pad_len = buf[end - 1] as usize;
        if pad_len == 0 || pad_len > end - offset {
            return None;
        }
        end -= pad_len;
    }
    Some(RtpPacket {
        version,
        padding,
        extension,
        csrc_count,
        marker,
        payload_type,
        sequence_number,
        timestamp,
        ssrc,
        payload: &buf[offset..end],
    })
}

/// Returns `true` if `buf` looks like an RFC 3550 RTCP v2 packet (PT 200..=207).
pub fn is_rtcp_packet(buf: &[u8]) -> bool {
    buf.len() >= 8 && (buf[0] & 0xC0) == 0x80 && (200..=207).contains(&buf[1])
}

/// Extract the middle 32 bits of the NTP timestamp from an RTCP Sender Report (PT 200),
/// walking compound RTCP sub-packets (RFC 3550 §6.4.1) for computing LSR/DLSR in Receiver Reports.
pub fn rtcp_sr_middle_ntp(rtcp: &[u8]) -> Option<u32> {
    let mut off = 0usize;
    while off + 4 <= rtcp.len() {
        if (rtcp[off] & 0xC0) != 0x80 {
            break;
        }
        let pt = rtcp[off + 1];
        let length_words = u16::from_be_bytes([rtcp[off + 2], rtcp[off + 3]]) as usize;
        let pkt_len = (length_words + 1).saturating_mul(4);
        if pt == 200 && off + 16 <= rtcp.len() {
            return Some(u32::from_be_bytes([
                rtcp[off + 10],
                rtcp[off + 11],
                rtcp[off + 12],
                rtcp[off + 13],
            ]));
        }
        if pkt_len < 4 || off.checked_add(pkt_len).is_none_or(|next| next > rtcp.len()) {
            break;
        }
        off += pkt_len;
    }
    None
}

/// RFC 3550 §A.3 reception statistics for building RTCP Receiver Reports.
#[derive(Debug, Default, Clone)]
pub struct RtpRxStats {
    started: bool,
    base_seq: u16,
    max_seq: u16,
    cycles: u32,
    received: u32,
    expected_prior: u32,
    received_prior: u32,
}

impl RtpRxStats {
    /// Record an incoming RTP sequence number. Returns `true` if a forward gap (packet loss) occurred.
    pub fn update(&mut self, seq: u16) -> bool {
        if !self.started {
            self.started = true;
            self.base_seq = seq;
            self.max_seq = seq;
            self.received = 1;
            return false;
        }
        self.received = self.received.wrapping_add(1);
        let delta = seq.wrapping_sub(self.max_seq);
        if delta > 0 && delta < 0x8000 {
            if seq < self.max_seq {
                self.cycles = self.cycles.wrapping_add(1 << 16);
            }
            self.max_seq = seq;
            delta > 1
        } else {
            false
        }
    }

    pub fn started(&self) -> bool {
        self.started
    }

    pub fn received(&self) -> u32 {
        self.received
    }

    pub fn extended_highest_seq(&self) -> u32 {
        self.cycles | u32::from(self.max_seq)
    }

    /// Build a 24-byte RFC 3550 Reception Report block for `peer_ssrc`.
    pub fn report_block(&mut self, peer_ssrc: u32, lsr: u32, dlsr: u32) -> Option<[u8; 24]> {
        if !self.started || peer_ssrc == 0 {
            return None;
        }
        let extended_max = self.extended_highest_seq();
        let expected = extended_max
            .wrapping_sub(u32::from(self.base_seq))
            .wrapping_add(1);
        let lost = expected.saturating_sub(self.received).min(0x00FF_FFFF);
        let expected_interval = expected.saturating_sub(self.expected_prior);
        self.expected_prior = expected;
        let received_interval = self.received.saturating_sub(self.received_prior);
        self.received_prior = self.received;
        let lost_interval = expected_interval.saturating_sub(received_interval);
        let fraction: u8 = if expected_interval == 0 || lost_interval == 0 {
            0
        } else {
            ((lost_interval.saturating_mul(256)) / expected_interval).min(255) as u8
        };

        let mut block = [0u8; 24];
        block[0..4].copy_from_slice(&peer_ssrc.to_be_bytes());
        block[4] = fraction;
        let lost_bytes = lost.to_be_bytes();
        block[5..8].copy_from_slice(&lost_bytes[1..4]);
        block[8..12].copy_from_slice(&extended_max.to_be_bytes());
        // Interarrival jitter (bytes 12..16) left 0 when arrival clock is not tracked
        block[16..20].copy_from_slice(&lsr.to_be_bytes());
        block[20..24].copy_from_slice(&dlsr.to_be_bytes());
        Some(block)
    }
}

/// Build an RFC 3550 RTCP Receiver Report (PT 201), optionally carrying one 24-byte report block.
pub fn build_rtcp_rr(sender_ssrc: u32, report_block: Option<[u8; 24]>) -> Vec<u8> {
    match report_block {
        Some(block) => {
            let mut rr = Vec::with_capacity(32);
            rr.extend_from_slice(&[0x81, 201, 0x00, 0x07]); // V=2, RC=1, PT=201, len=7 words
            rr.extend_from_slice(&sender_ssrc.to_be_bytes());
            rr.extend_from_slice(&block);
            rr
        }
        None => {
            let mut rr = Vec::with_capacity(8);
            rr.extend_from_slice(&[0x80, 201, 0x00, 0x01]); // V=2, RC=0, PT=201, len=1 word
            rr.extend_from_slice(&sender_ssrc.to_be_bytes());
            rr
        }
    }
}

/// Outcome of popping one 20 ms playout slot from [`JitterBuffer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JitterPop {
    /// Still building the initial playout cushion; output silence.
    Buffering,
    /// In-order Opus packet ready to decode.
    Packet(Vec<u8>),
    /// Packet for this playout slot is missing, but the subsequent packet is already buffered
    /// so the decoder can attempt in-band FEC recovery before falling back to PLC.
    MissingWithNext(Vec<u8>),
    /// Packet missing and no lookahead packet is available; run Opus PLC.
    Plc,
    /// Stream appears idle after consecutive misses; output silence and wait to re-prime.
    Silence,
}

/// Diagnostic counters for [`JitterBuffer`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterStats {
    pub pushed: u64,
    pub popped_packets: u64,
    pub plc_frames: u64,
    pub fec_candidates: u64,
    pub dropped_late: u64,
    pub dropped_overflow: u64,
}

/// Sequence-ordered audio jitter buffer supporting RTP `u16` wraparound, reordering,
/// bounded latency, in-band FEC lookahead, and PLC fallback.
#[derive(Debug)]
pub struct JitterBuffer {
    target_frames: usize,
    max_frames: usize,
    max_plc_frames: usize,
    primed: bool,
    next_ext_seq: Option<i64>,
    consecutive_missing: usize,
    packets: BTreeMap<i64, Vec<u8>>,
    stats: JitterStats,
}

impl JitterBuffer {
    /// Create a jitter buffer with `target_frames` pre-roll (e.g. `2` = 40 ms) and `max_frames` cap.
    pub fn new(target_frames: usize, max_frames: usize) -> Self {
        let target = target_frames.max(1);
        let max = max_frames.max(target + 1);
        Self {
            target_frames: target,
            max_frames: max,
            max_plc_frames: 5,
            primed: false,
            next_ext_seq: None,
            consecutive_missing: 0,
            packets: BTreeMap::new(),
            stats: JitterStats::default(),
        }
    }

    /// Create a default 20 ms VoIP jitter buffer (40 ms target, 240 ms cap).
    pub fn voip_default() -> Self {
        Self::new(2, 12)
    }

    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    pub fn reset(&mut self) {
        self.primed = false;
        self.next_ext_seq = None;
        self.consecutive_missing = 0;
        self.packets.clear();
    }

    /// Insert an incoming RTP packet payload by its 16-bit RTP sequence number.
    pub fn push(&mut self, seq: u16, payload: Vec<u8>) {
        if payload.is_empty() {
            return;
        }
        self.stats.pushed += 1;

        let ext_seq = match self.next_ext_seq {
            None => {
                let initial = i64::from(seq);
                self.next_ext_seq = Some(initial);
                initial
            }
            Some(anchor) => {
                let anchor_u16 = (anchor & 0xFFFF) as u16;
                let diff = i64::from(seq.wrapping_sub(anchor_u16) as i16);
                let candidate = anchor + diff;
                if candidate < anchor {
                    // Late packet behind playout head: if we aren't primed yet and this is only
                    // slightly earlier (out-of-order start of stream), pull the playout head back.
                    if !self.primed && anchor - candidate <= self.max_frames as i64 {
                        self.next_ext_seq = Some(candidate);
                        candidate
                    } else {
                        self.stats.dropped_late += 1;
                        return;
                    }
                } else {
                    candidate
                }
            }
        };

        self.packets.insert(ext_seq, payload);

        // Enforce bounded latency: if packets span too far ahead, advance playout head.
        while self.packets.len() > self.max_frames {
            if let Some((&oldest_seq, _)) = self.packets.iter().next() {
                self.packets.remove(&oldest_seq);
                self.stats.dropped_overflow += 1;
                if let Some(next_seq) = self.next_ext_seq {
                    if oldest_seq >= next_seq {
                        self.next_ext_seq = Some(oldest_seq + 1);
                    }
                }
            } else {
                break;
            }
        }

        if let Some(&first_seq) = self.packets.keys().next() {
            if let Some(next_seq) = self.next_ext_seq {
                if first_seq - next_seq > self.max_frames as i64 {
                    self.next_ext_seq = Some(first_seq);
                }
            }
        }

        if self.packets.len() >= self.target_frames {
            self.primed = true;
        }
    }

    /// Advance the playout clock by one 20 ms frame and decide whether to decode, run FEC/PLC, or output silence.
    pub fn pop(&mut self) -> JitterPop {
        let Some(next_seq) = self.next_ext_seq else {
            return JitterPop::Buffering;
        };
        if !self.primed {
            return JitterPop::Buffering;
        }

        if let Some(pkt) = self.packets.remove(&next_seq) {
            self.next_ext_seq = Some(next_seq + 1);
            self.consecutive_missing = 0;
            self.stats.popped_packets += 1;
            return JitterPop::Packet(pkt);
        }

        // If the very next packet is already buffered, offer it for Opus in-band FEC.
        if let Some(lookahead) = self.packets.get(&(next_seq + 1)).cloned() {
            self.next_ext_seq = Some(next_seq + 1);
            self.consecutive_missing += 1;
            self.stats.fec_candidates += 1;
            return JitterPop::MissingWithNext(lookahead);
        }

        // If buffered packets sit far ahead (e.g. after DTX silence or a large burst loss)
        // or we have already exhausted PLC frames while future packets wait, resync directly
        // to the earliest buffered packet instead of running unbounded PLC.
        if let Some(&first_seq) = self.packets.keys().next() {
            if first_seq - next_seq > self.max_frames as i64
                || self.consecutive_missing >= self.max_plc_frames
            {
                if let Some(pkt) = self.packets.remove(&first_seq) {
                    self.next_ext_seq = Some(first_seq + 1);
                    self.consecutive_missing = 0;
                    self.stats.popped_packets += 1;
                    return JitterPop::Packet(pkt);
                }
            }
        }

        if self.packets.is_empty() && self.consecutive_missing >= self.max_plc_frames {
            // Talkspurt ended or prolonged outage: stop PLC and re-prime cleanly on next packet.
            self.primed = false;
            self.next_ext_seq = None;
            self.consecutive_missing = 0;
            return JitterPop::Silence;
        }

        self.next_ext_seq = Some(next_seq + 1);
        self.consecutive_missing += 1;
        self.stats.plc_frames += 1;
        JitterPop::Plc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_packet_roundtrip_and_extension_padding_parsing() {
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let pkt = build_rtp_packet(OPUS_AUDIO_PT, true, 1234, 96_000, 0x1122_3344, &payload);
        let parsed = parse_rtp_packet(&pkt).unwrap();
        assert_eq!(parsed.version, 2);
        assert!(parsed.marker);
        assert_eq!(parsed.payload_type, OPUS_AUDIO_PT);
        assert_eq!(parsed.sequence_number, 1234);
        assert_eq!(parsed.timestamp, 96_000);
        assert_eq!(parsed.ssrc, 0x1122_3344);
        assert_eq!(parsed.payload, &payload);

        // RTP packet with 1 CSRC, 1-word header extension, and 2 bytes padding
        let mut ext_pkt = vec![
            0xB1, // V=2, P=1, X=1, CC=1
            OPUS_AUDIO_PT,
            0x00, 0x05, // seq = 5
            0x00, 0x00, 0x01, 0x40, // ts = 320
            0xAA, 0xBB, 0xCC, 0xDD, // ssrc
            0x01, 0x02, 0x03, 0x04, // CSRC[0]
            0xBE, 0xDE, 0x00, 0x01, // ext profile + 1 word
            0x20, 0x4A, 0x00, 0x00, // ext body
            0x55, 0x66, 0x77, // payload
            0x00, 0x02, // 2-byte padding
        ];
        let p2 = parse_rtp_packet(&ext_pkt).unwrap();
        assert_eq!(p2.csrc_count, 1);
        assert!(p2.extension);
        assert!(p2.padding);
        assert_eq!(p2.payload, &[0x55, 0x66, 0x77]);

        // Corrupt padding length > payload must fail cleanly
        *ext_pkt.last_mut().unwrap() = 99;
        assert!(parse_rtp_packet(&ext_pkt).is_none());

        // RTCP packets are distinguished from RTP
        let rr = build_rtcp_rr(0x1122_3344, None);
        assert!(is_rtcp_packet(&rr));
        assert!(parse_rtp_packet(&rr).is_none());
    }

    #[test]
    fn rtcp_rx_stats_computes_loss_and_builds_report_block() {
        let mut rx = RtpRxStats::default();
        assert!(!rx.update(100));
        assert!(!rx.update(101));
        assert!(rx.update(103)); // 102 lost
        assert!(!rx.update(104));
        assert_eq!(rx.received(), 4);
        assert_eq!(rx.extended_highest_seq(), 104);

        let block = rx.report_block(0xCAFE_BABE, 0x1234_5678, 0x0001_0000).unwrap();
        assert_eq!(u32::from_be_bytes(block[0..4].try_into().unwrap()), 0xCAFE_BABE);
        // 1 lost out of 5 expected -> floor(256/5) = 51
        assert_eq!(block[4], 51);
        let rr = build_rtcp_rr(0x1122_3344, Some(block));
        assert_eq!(rr.len(), 32);
        assert!(is_rtcp_packet(&rr));
    }

    #[test]
    fn jitter_buffer_reorders_handles_wraparound_and_signals_fec_and_plc() {
        let mut jb = JitterBuffer::new(2, 6);
        assert_eq!(jb.pop(), JitterPop::Buffering);

        // Out-of-order across u16 wraparound: 65535, 0, 1 arrive as 0, 65535, 2 (1 is lost)
        jb.push(0, vec![20]);
        assert_eq!(jb.pop(), JitterPop::Buffering);
        jb.push(65535, vec![10]);
        jb.push(2, vec![40]);

        assert_eq!(jb.pop(), JitterPop::Packet(vec![10]));
        assert_eq!(jb.pop(), JitterPop::Packet(vec![20]));
        // Seq 1 is missing, seq 2 is already buffered -> MissingWithNext(vec![40])
        assert_eq!(jb.pop(), JitterPop::MissingWithNext(vec![40]));
        assert_eq!(jb.pop(), JitterPop::Packet(vec![40]));

        // Late packet behind head is dropped
        jb.push(65535, vec![99]);
        assert_eq!(jb.stats().dropped_late, 1);

        // Subsequent missing packets trigger PLC up to max_plc_frames, then Silence
        for _ in 0..5 {
            assert_eq!(jb.pop(), JitterPop::Plc);
        }
        assert_eq!(jb.pop(), JitterPop::Silence);
    }

    #[test]
    fn jitter_buffer_bounds_overflow_latency() {
        let mut jb = JitterBuffer::new(2, 4);
        for seq in 0u16..10 {
            jb.push(seq, vec![seq as u8]);
        }
        assert_eq!(jb.len(), 4);
        assert_eq!(jb.stats().dropped_overflow, 6);
        // Should play out the newest 4 frames (6, 7, 8, 9) in order
        assert_eq!(jb.pop(), JitterPop::Packet(vec![6]));
        assert_eq!(jb.pop(), JitterPop::Packet(vec![7]));
        assert_eq!(jb.pop(), JitterPop::Packet(vec![8]));
        assert_eq!(jb.pop(), JitterPop::Packet(vec![9]));
    }

    #[test]
    fn jitter_buffer_resyncs_cleanly_after_large_forward_dtx_gap() {
        let mut jb = JitterBuffer::new(2, 6);
        jb.push(10, vec![10]);
        jb.push(11, vec![11]);
        assert_eq!(jb.pop(), JitterPop::Packet(vec![10]));
        // Large forward jump (e.g., 89-packet DTX gap) while seq 11 is still buffered
        jb.push(100, vec![100]);
        assert_eq!(jb.pop(), JitterPop::Packet(vec![11]));
        // Next pop must snap directly to seq 100 rather than emitting 88 PLC frames
        assert_eq!(jb.pop(), JitterPop::Packet(vec![100]));
        assert_eq!(jb.stats().plc_frames, 0);
    }

    #[test]
    fn rtcp_sr_middle_ntp_walks_compound_rtcp_subpackets() {
        // Standalone RR (PT=201, 8 bytes) followed by SR (PT=200, 28 bytes) carrying mid NTP 0xAABB_CCDD
        let mut compound = build_rtcp_rr(0x1122_3344, None);
        let mut sr = vec![0u8; 28];
        sr[0] = 0x80; // V=2
        sr[1] = 200; // PT=SR
        sr[2..4].copy_from_slice(&6u16.to_be_bytes()); // (6 + 1) * 4 = 28 bytes
        sr[4..8].copy_from_slice(&0x5566_7788u32.to_be_bytes());
        sr[8..16].copy_from_slice(&[0x11, 0x22, 0xAA, 0xBB, 0xCC, 0xDD, 0x33, 0x44]);
        compound.extend_from_slice(&sr);

        assert_eq!(rtcp_sr_middle_ntp(&sr), Some(0xAABB_CCDD));
        assert_eq!(rtcp_sr_middle_ntp(&compound), Some(0xAABB_CCDD));
        assert_eq!(rtcp_sr_middle_ntp(&build_rtcp_rr(0x1122_3344, None)), None);
    }
}
