//! H.264 Annex-B normalization, Zalo `0x1C` video fragmentation (`PT 98`/`97`),
//! RFC 6184 fallback depacketization, and Constrained Baseline synthetic test vectors.

use std::collections::VecDeque;

/// Delta video frame RTP payload type (`PT 97`).
pub const VIDEO_PT_DELTA: u8 = 97;

/// Keyframe (IDR-bearing AU) RTP payload type (`PT 98`).
///
/// Per `libzrtc.so` (`RTPSender::SendOutgoingData` / `RTPReceiverVideo::ParseRtpPacket`),
/// all IDR access units MUST use `PT 98` (even unfragmented single-packet keyframes),
/// otherwise the peer treats the frame as a delta frame (`DeltaFrame`) and emits PLIs.
pub const VIDEO_PT_KEYFRAME: u8 = 98;

/// Standard video RTP clock rate (90 kHz).
pub const VIDEO_CLOCK_HZ: u32 = 90_000;

/// 2-byte Zalo fragment indicator byte (`0x1C <flags> <annexb_chunk>`).
pub const ZALO_FRAG_INDICATOR: u8 = 0x1C;
pub const ZALO_FRAG_START: u8 = 0x80;
pub const ZALO_FRAG_END: u8 = 0x40;

/// Target max RTP payload per video packet (including the 2-byte `0x1C <flags>` header).
pub const VIDEO_RTP_MAX_PAYLOAD: usize = 1200;

/// Upper bound on an accumulated fragmented access unit before discarding (1 MiB).
pub const ZALO_FRAG_BUF_MAX: usize = 1 << 20;

/// Access Unit Delimiter NAL (`primary_pic_type = 7`), written before AUs for pipe decoders.
pub const H264_AUD: &[u8] = &[0x00, 0x00, 0x00, 0x01, 0x09, 0xF0];

// Valid libx264-encoded H.264 Constrained Baseline (320x240, 15 fps) SPS/PPS/IDR/P-slice NAL units.
pub const H264_SYNTH_SPS: &[u8] = &[
    0x67, 0x42, 0xC0, 0x1E, 0xDA, 0x05, 0x07, 0xEC, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40, 0x00,
    0x00, 0x07, 0x83, 0xC5, 0x8B, 0xA8,
];
pub const H264_SYNTH_PPS: &[u8] = &[0x68, 0xCE, 0x0F, 0x2C, 0x80];
pub const H264_SYNTH_IDR: &[u8] = &[
    0x65, 0x88, 0x84, 0x5C, 0x46, 0x28, 0x00, 0x08, 0x6C, 0xC7, 0x00, 0x02, 0x51, 0xC0, 0x19, 0x39,
    0x39, 0x39, 0x39, 0x39, 0x39, 0x39, 0x39, 0x39, 0x39, 0x39, 0x39, 0x39, 0x39, 0x39, 0x39, 0x39,
    0x39, 0x3A, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE,
    0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA,
    0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB,
    0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE,
    0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA,
    0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB,
    0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE,
    0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA,
    0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB,
    0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE,
    0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA,
    0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB,
    0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE,
    0xBA, 0xEB, 0xAE, 0xBC,
];
pub const H264_SYNTH_IDR_ALT: &[u8] = &[
    0x65, 0x88, 0x82, 0x1F, 0x11, 0x8A, 0x00, 0x02, 0x22, 0xF1, 0xC0, 0x00, 0xB4, 0x70, 0x01, 0x93,
    0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93, 0x93,
    0x93, 0x93, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA,
    0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB,
    0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE,
    0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA,
    0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB,
    0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE,
    0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA,
    0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB,
    0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE,
    0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA,
    0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB,
    0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE,
    0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA, 0xEB, 0xAE, 0xBA,
    0xEB, 0xAE, 0xBA, 0xEB, 0xC0,
];
pub const H264_SYNTH_PSLICES: [&[u8]; 14] = [
    &[0x41, 0x9A, 0x21, 0x70, 0x09, 0x6C],
    &[0x41, 0x9A, 0x41, 0x70, 0x09, 0x6C],
    &[0x41, 0x9A, 0x61, 0x70, 0x09, 0x6C],
    &[0x41, 0x9A, 0x81, 0x70, 0x09, 0x6C],
    &[0x41, 0x9A, 0xA1, 0x70, 0x09, 0x6C],
    &[0x41, 0x9A, 0xC1, 0x70, 0x09, 0x6C],
    &[0x41, 0x9A, 0xE1, 0x70, 0x09, 0x6C],
    &[0x41, 0x9B, 0x01, 0x70, 0x09, 0x6C],
    &[0x41, 0x9B, 0x21, 0x70, 0x09, 0x6C],
    &[0x41, 0x9B, 0x41, 0x70, 0x09, 0x6C],
    &[0x41, 0x9B, 0x61, 0x70, 0x09, 0x6C],
    &[0x41, 0x9B, 0x81, 0x70, 0x09, 0x6C],
    &[0x41, 0x9B, 0xA1, 0x70, 0x09, 0x6C],
    &[0x41, 0x9B, 0xC1, 0x70, 0x09, 0x6C],
];

/// Return `true` for RTP payload types used by 1-1 Zalo video streams.
pub fn is_video_payload_type(pt: u8) -> bool {
    matches!(pt, 96 | 97 | 98 | 121 | 122)
}

/// Return `true` when `p` starts with a 3-byte or 4-byte Annex-B start code.
pub fn is_annexb(p: &[u8]) -> bool {
    p.starts_with(&[0, 0, 0, 1]) || p.starts_with(&[0, 0, 1])
}

/// Return `true` when `p` uses Zalo's `0x1C <flags> <chunk>` fragmentation header.
pub fn is_zalo_frag(p: &[u8]) -> bool {
    p.len() >= 2 && (p[0] & 0x1F) == ZALO_FRAG_INDICATOR && (p[1] & 0x1F) == 0
}

/// Concatenate raw H.264 NAL units (without start codes) into one 4-byte Annex-B Access Unit.
pub fn annexb_au(nals: &[&[u8]]) -> Vec<u8> {
    let total: usize = nals.iter().map(|n| 4 + n.len()).sum();
    let mut v = Vec::with_capacity(total);
    for n in nals {
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        v.extend_from_slice(n);
    }
    v
}

/// Return `true` when `au` contains any Annex-B NAL unit whose 5-bit `nal_unit_type` is in `types`.
pub fn au_has_nal_type(au: &[u8], types: &[u8]) -> bool {
    let mut i = 0usize;
    while i + 3 < au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            if types.contains(&(au[i + 3] & 0x1F)) {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// Return `true` when the Annex-B access unit contains an IDR slice (`nal_unit_type == 5`).
pub fn au_has_idr(au: &[u8]) -> bool {
    au_has_nal_type(au, &[5])
}

/// Return `true` when the access unit can start a decoder sync (contains an IDR slice, `nal_unit_type == 5`).
pub fn au_is_sync_point(au: &[u8]) -> bool {
    au_has_idr(au)
}

fn split_annexb_nals(au: &[u8]) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    let mut i = 0usize;
    let mut cur_start: Option<usize> = None;
    while i + 3 <= au.len() {
        let sc_len = if i + 4 <= au.len() && au[i..i + 4] == [0, 0, 0, 1] {
            4
        } else if au[i..i + 3] == [0, 0, 1] {
            3
        } else {
            0
        };
        if sc_len > 0 {
            if let Some(s) = cur_start {
                if i > s {
                    nals.push(&au[s..i]);
                }
            }
            cur_start = Some(i + sc_len);
            i += sc_len;
        } else {
            i += 1;
        }
    }
    if let Some(s) = cur_start {
        if au.len() > s {
            nals.push(&au[s..]);
        }
    }
    nals
}

/// Locate the next 4-byte Annex-B Access Unit Delimiter (`00 00 00 01 <type=9>`) at or after `start_from`.
pub fn find_aud_delimiter(buf: &[u8], start_from: usize) -> Option<usize> {
    if buf.len() < start_from + 6 {
        return None;
    }
    (start_from..=buf.len() - 6).find(|&i| {
        buf[i] == 0
            && buf[i + 1] == 0
            && buf[i + 2] == 0
            && buf[i + 3] == 1
            && (buf[i + 4] & 0x1F) == 9
    })
}

/// Parse an Annex-B Access Unit, stripping SEI (`6`) and AUD (`9`) NAL units and rewriting every
/// remaining NAL unit with a canonical 4-byte `00 00 00 01` start code.
pub fn normalize_annexb_au_4byte(au: &[u8]) -> Vec<u8> {
    let mut nal_ranges: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    let mut cur_start: Option<usize> = None;
    while i + 3 <= au.len() {
        let sc_len = if i + 4 <= au.len() && au[i..i + 4] == [0, 0, 0, 1] {
            4
        } else if au[i..i + 3] == [0, 0, 1] {
            3
        } else {
            0
        };
        if sc_len > 0 {
            if let Some(s) = cur_start {
                if i > s {
                    nal_ranges.push((s, i));
                }
            }
            cur_start = Some(i + sc_len);
            i += sc_len;
        } else {
            i += 1;
        }
    }
    if let Some(s) = cur_start {
        if au.len() > s {
            nal_ranges.push((s, au.len()));
        }
    }
    if nal_ranges.is_empty() {
        return au.to_vec();
    }
    let mut out = Vec::with_capacity(au.len() + nal_ranges.len() * 2);
    for (s, e) in nal_ranges {
        let nal = &au[s..e];
        if nal.is_empty() {
            continue;
        }
        let nal_type = nal[0] & 0x1F;
        if nal_type == 6 || nal_type == 9 {
            continue;
        }
        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
        out.extend_from_slice(nal);
    }
    out
}

/// Strip leading/embedded AUD & SEI NAL units and normalize all start codes to 4 bytes.
pub fn strip_leading_aud(au: &[u8]) -> Vec<u8> {
    normalize_annexb_au_4byte(au)
}

/// Return a compact human-readable description of the NAL unit types in `au` (for diagnostics/tests).
pub fn describe_nal_types(au: &[u8]) -> String {
    let mut labels: Vec<&'static str> = Vec::new();
    let mut i = 0usize;
    while i + 3 < au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            let tag = match au[i + 3] & 0x1F {
                1 => "P",
                5 => "IDR",
                6 => "SEI",
                7 => "SPS",
                8 => "PPS",
                9 => "AUD",
                _ => "OTHER",
            };
            labels.push(tag);
            i += 3;
        } else {
            i += 1;
        }
    }
    if labels.is_empty() {
        "NONE".to_string()
    } else {
        labels.join("+")
    }
}

/// Packetize one Annex-B Access Unit into `(payload_type, wire_payload)` chunks for RTP.
///
/// Matches Zalo `libzrtc.so`:
/// - `au_has_idr(au)` -> `VIDEO_PT_KEYFRAME` (`98`), otherwise `VIDEO_PT_DELTA` (`97`), even when
///   the keyframe fits inside a single RTP packet.
/// - When `au.len() > max_payload`, splits `au` evenly into `ceil(len / (max_payload - 2))` parts,
///   each prefixed with `[0x1C, flags]` (`0x80` start, `0x00` middle, `0x40` end).
pub fn packetize_zalo_au(au: &[u8], max_payload: usize) -> Vec<(u8, Vec<u8>)> {
    if au.is_empty() {
        return Vec::new();
    }
    let pt = if au_has_idr(au) {
        VIDEO_PT_KEYFRAME
    } else {
        VIDEO_PT_DELTA
    };
    if au.len() <= max_payload || max_payload < 3 {
        return vec![(pt, au.to_vec())];
    }
    let parts = au.len().div_ceil(max_payload - 2);
    let chunk = au.len().div_ceil(parts);
    let n = au.len().div_ceil(chunk);
    au.chunks(chunk)
        .enumerate()
        .map(|(i, c)| {
            let mut flags = 0u8;
            if i == 0 {
                flags |= ZALO_FRAG_START;
            }
            if i + 1 == n {
                flags |= ZALO_FRAG_END;
            }
            let mut v = Vec::with_capacity(2 + c.len());
            v.push(ZALO_FRAG_INDICATOR);
            v.push(flags);
            v.extend_from_slice(c);
            (pt, v)
        })
        .collect()
}

/// Result of feeding one incoming RTP video packet into [`VideoDepacketizer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepacketizeOutcome {
    /// Duplicate `(ssrc, seq)` datagram ignored.
    Duplicate,
    /// Fragment buffered; awaiting subsequent fragment(s) to complete the Access Unit.
    Incomplete,
    /// Complete P-frame arrived before the first SPS/IDR sync point and was dropped cleanly.
    DroppedBeforeSync,
    /// Complete canonical 4-byte Annex-B Access Unit ready for H.264 decoding.
    AccessUnit {
        au: Vec<u8>,
        is_keyframe: bool,
        just_synced: bool,
    },
}

const MAX_FRAGS_PER_AU: usize = 512;

fn is_stale_ts(incoming: u32, active: u32) -> bool {
    let dt = incoming.wrapping_sub(active) as i32;
    dt < 0 && dt.unsigned_abs() < VIDEO_CLOCK_HZ * 10
}

#[derive(Debug, Default)]
struct FragAssembly {
    active: bool,
    timestamp: u32,
    is_fua: bool,
    fua_nal_hdr: u8,
    start_seq: Option<u16>,
    end_seq: Option<u16>,
    total_bytes: usize,
    chunks: Vec<(u16, Vec<u8>)>,
}

impl FragAssembly {
    fn clear(&mut self) {
        self.active = false;
        self.timestamp = 0;
        self.is_fua = false;
        self.fua_nal_hdr = 0;
        self.start_seq = None;
        self.end_seq = None;
        self.total_bytes = 0;
        self.chunks.clear();
    }
}

/// Reassembles incoming Zalo video RTP payloads (`Annex-B`, `0x1C` fragments, and RFC 6184 fallback),
/// deduplicates `(ssrc, seq)`, enforces contiguous sequence/timestamp assembly, caches SPS/PPS
/// parameter sets, and gates output until the first IDR (`nal_unit_type == 5`) sync point arrives.
#[derive(Debug)]
pub struct VideoDepacketizer {
    recent_seqs: VecDeque<(u32, u16)>,
    frag: FragAssembly,
    cached_sps: Option<Vec<u8>>,
    cached_pps: Option<Vec<u8>>,
    last_emitted_ts: Option<u32>,
    last_ssrc: Option<u32>,
    video_synced: bool,
    dropped_before_sync: u64,
    dropped_incomplete: u64,
    access_units_out: u64,
    keyframes_out: u64,
}

impl Default for VideoDepacketizer {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoDepacketizer {
    pub fn new() -> Self {
        Self {
            recent_seqs: VecDeque::with_capacity(256),
            frag: FragAssembly::default(),
            cached_sps: None,
            cached_pps: None,
            last_emitted_ts: None,
            last_ssrc: None,
            video_synced: false,
            dropped_before_sync: 0,
            dropped_incomplete: 0,
            access_units_out: 0,
            keyframes_out: 0,
        }
    }

    pub fn is_synced(&self) -> bool {
        self.video_synced
    }

    pub fn dropped_before_sync(&self) -> u64 {
        self.dropped_before_sync
    }

    pub fn dropped_incomplete(&self) -> u64 {
        self.dropped_incomplete
    }

    pub fn access_units_out(&self) -> u64 {
        self.access_units_out
    }

    pub fn keyframes_out(&self) -> u64 {
        self.keyframes_out
    }

    /// Reset fragment, sequence deduplication, timestamp, and sync state (for example after peer
    /// camera turn-off/turn-on, idle timeout, or SSRC switch).
    pub fn reset_sync(&mut self) {
        self.frag.clear();
        self.recent_seqs.clear();
        self.last_emitted_ts = None;
        self.video_synced = false;
    }

    fn accept_unfragmented_ts(&mut self, timestamp: u32) -> bool {
        if self.frag.active {
            if is_stale_ts(timestamp, self.frag.timestamp) {
                return false;
            }
            if timestamp != self.frag.timestamp {
                self.dropped_incomplete += 1;
                self.frag.clear();
            }
        }
        !self
            .last_emitted_ts
            .is_some_and(|last_ts| is_stale_ts(timestamp, last_ts))
    }

    /// Feed one parsed incoming video RTP packet.
    pub fn push_rtp(
        &mut self,
        ssrc: u32,
        seq: u16,
        timestamp: u32,
        payload: &[u8],
    ) -> DepacketizeOutcome {
        if payload.is_empty() {
            return DepacketizeOutcome::Incomplete;
        }
        if ssrc != 0 {
            if self.last_ssrc.is_some_and(|prev| prev != ssrc) {
                self.cached_sps = None;
                self.cached_pps = None;
                self.reset_sync();
            }
            self.last_ssrc = Some(ssrc);
        }
        let key = (ssrc, seq);
        if self.recent_seqs.contains(&key) {
            return DepacketizeOutcome::Duplicate;
        }
        if self.recent_seqs.len() >= 256 {
            self.recent_seqs.pop_front();
        }
        self.recent_seqs.push_back(key);

        let completed = if is_annexb(payload) {
            if !self.accept_unfragmented_ts(timestamp) {
                None
            } else {
                Some(payload.to_vec())
            }
        } else if is_zalo_frag(payload) {
            let flags = payload[1];
            self.push_fragment(
                seq,
                timestamp,
                false,
                0,
                (flags & ZALO_FRAG_START) != 0,
                (flags & ZALO_FRAG_END) != 0,
                &payload[2..],
            )
        } else {
            self.push_rfc6184(seq, timestamp, payload)
        };

        let Some(raw_au) = completed else {
            return DepacketizeOutcome::Incomplete;
        };
        let normalized = normalize_annexb_au_4byte(&raw_au);
        if normalized.is_empty() {
            return DepacketizeOutcome::Incomplete;
        }
        let nals = split_annexb_nals(&normalized);
        if nals.is_empty() {
            return DepacketizeOutcome::Incomplete;
        }

        let mut has_sps = false;
        let mut has_pps = false;
        let mut has_idr = false;
        let mut has_vcl = false;
        for nal in &nals {
            if nal.is_empty() {
                continue;
            }
            match nal[0] & 0x1F {
                7 => {
                    has_sps = true;
                    self.cached_sps = Some(nal.to_vec());
                }
                8 => {
                    has_pps = true;
                    self.cached_pps = Some(nal.to_vec());
                }
                5 => {
                    has_idr = true;
                    has_vcl = true;
                }
                1..=4 => {
                    has_vcl = true;
                }
                _ => {}
            }
        }
        if !has_vcl {
            return DepacketizeOutcome::Incomplete;
        }
        if !self.video_synced && !has_idr {
            self.dropped_before_sync += 1;
            return DepacketizeOutcome::DroppedBeforeSync;
        }

        let au = if has_idr
            && ((!has_sps && self.cached_sps.is_some()) || (!has_pps && self.cached_pps.is_some()))
        {
            let mut out = Vec::with_capacity(
                normalized.len()
                    + self.cached_sps.as_ref().map_or(0, |s| 4 + s.len())
                    + self.cached_pps.as_ref().map_or(0, |p| 4 + p.len()),
            );
            if has_sps {
                for nal in &nals {
                    if !nal.is_empty() && (nal[0] & 0x1F) == 7 {
                        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        out.extend_from_slice(nal);
                    }
                }
            } else if let Some(ref sps) = self.cached_sps {
                out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                out.extend_from_slice(sps);
            }
            if has_pps {
                for nal in &nals {
                    if !nal.is_empty() && (nal[0] & 0x1F) == 8 {
                        out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        out.extend_from_slice(nal);
                    }
                }
            } else if let Some(ref pps) = self.cached_pps {
                out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                out.extend_from_slice(pps);
            }
            for nal in &nals {
                if !nal.is_empty() && !matches!(nal[0] & 0x1F, 7 | 8) {
                    out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                    out.extend_from_slice(nal);
                }
            }
            out
        } else {
            normalized
        };

        let just_synced = !self.video_synced && has_idr;
        self.video_synced = true;
        self.last_emitted_ts = Some(timestamp);
        self.access_units_out += 1;
        if has_idr {
            self.keyframes_out += 1;
        }
        DepacketizeOutcome::AccessUnit {
            au,
            is_keyframe: has_idr,
            just_synced,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push_fragment(
        &mut self,
        seq: u16,
        timestamp: u32,
        is_fua: bool,
        fua_nal_hdr: u8,
        is_start: bool,
        is_end: bool,
        body: &[u8],
    ) -> Option<Vec<u8>> {
        if self.frag.active {
            if self.frag.is_fua != is_fua {
                self.dropped_incomplete += 1;
                self.frag.clear();
            } else if self.frag.timestamp != timestamp {
                if is_stale_ts(timestamp, self.frag.timestamp) {
                    return None;
                }
                self.dropped_incomplete += 1;
                self.frag.clear();
            } else if (is_fua && self.frag.fua_nal_hdr != fua_nal_hdr)
                || (is_start && self.frag.start_seq.is_some_and(|s| s != seq))
            {
                self.dropped_incomplete += 1;
                self.frag.clear();
            }
        }
        if !self.frag.active {
            if self
                .last_emitted_ts
                .is_some_and(|last_ts| is_stale_ts(timestamp, last_ts))
            {
                return None;
            }
            self.frag.active = true;
            self.frag.timestamp = timestamp;
            self.frag.is_fua = is_fua;
            self.frag.fua_nal_hdr = fua_nal_hdr;
        }
        if self.frag.chunks.iter().any(|(s, _)| *s == seq) {
            return None;
        }
        if self.frag.chunks.len() >= MAX_FRAGS_PER_AU
            || self.frag.total_bytes.saturating_add(body.len()) > ZALO_FRAG_BUF_MAX
        {
            self.dropped_incomplete += 1;
            self.frag.clear();
            return None;
        }
        if is_start {
            self.frag.start_seq = Some(seq);
        }
        if is_end {
            self.frag.end_seq = Some(seq);
        }
        self.frag.total_bytes += body.len();
        self.frag.chunks.push((seq, body.to_vec()));

        let (Some(start), Some(end)) = (self.frag.start_seq, self.frag.end_seq) else {
            return None;
        };
        let span = end.wrapping_sub(start) as usize;
        if span >= MAX_FRAGS_PER_AU {
            self.dropped_incomplete += 1;
            self.frag.clear();
            return None;
        }
        let expected = span + 1;
        if self.frag.chunks.len() < expected {
            return None;
        }

        let mut ordered: Vec<Option<&[u8]>> = vec![None; expected];
        let mut valid_count = 0usize;
        for (s, data) in &self.frag.chunks {
            let idx = s.wrapping_sub(start) as usize;
            if idx < expected && ordered[idx].is_none() {
                ordered[idx] = Some(data.as_slice());
                valid_count += 1;
            }
        }
        if valid_count < expected {
            return None;
        }

        let prefix_len = if self.frag.is_fua { 5 } else { 0 };
        let mut out = Vec::with_capacity(prefix_len + self.frag.total_bytes);
        if self.frag.is_fua {
            out.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, self.frag.fua_nal_hdr]);
        }
        for chunk in ordered.into_iter().flatten() {
            out.extend_from_slice(chunk);
        }
        self.frag.clear();
        (!out.is_empty()).then_some(out)
    }

    fn push_rfc6184(&mut self, seq: u16, timestamp: u32, payload: &[u8]) -> Option<Vec<u8>> {
        let nal_type = payload[0] & 0x1F;
        match nal_type {
            1..=23 => {
                if !self.accept_unfragmented_ts(timestamp) {
                    return None;
                }
                let mut annexb = Vec::with_capacity(4 + payload.len());
                annexb.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                annexb.extend_from_slice(payload);
                Some(annexb)
            }
            24 => {
                if !self.accept_unfragmented_ts(timestamp) {
                    return None;
                }
                let mut idx = 1usize;
                let mut annexb = Vec::new();
                while idx + 2 <= payload.len() {
                    let sz = u16::from_be_bytes([payload[idx], payload[idx + 1]]) as usize;
                    idx += 2;
                    if sz > 0 && idx + sz <= payload.len() {
                        annexb.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
                        annexb.extend_from_slice(&payload[idx..idx + sz]);
                        idx += sz;
                    } else {
                        break;
                    }
                }
                (!annexb.is_empty()).then_some(annexb)
            }
            28 if payload.len() >= 2 => {
                let fu_ind = payload[0];
                let fu_hdr = payload[1];
                let start_bit = (fu_hdr & 0x80) != 0;
                let end_bit = (fu_hdr & 0x40) != 0;
                let orig_nal_hdr = (fu_ind & 0xE0) | (fu_hdr & 0x1F);
                self.push_fragment(
                    seq,
                    timestamp,
                    true,
                    orig_nal_hdr,
                    start_bit,
                    end_bit,
                    &payload[2..],
                )
            }
            _ => None,
        }
    }
}

/// Deterministic Constrained Baseline (320x240) H.264 Annex-B test-pattern generator used
/// for offline loopback verification and CLI testing without a physical webcam.
#[derive(Debug, Default)]
pub struct SyntheticH264Generator {
    frame_no: u32,
    gop_pos: usize,
    idr_parity: u8,
}

impl SyntheticH264Generator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_frame(&mut self, force_keyframe: bool) -> Vec<u8> {
        let emit_idr = force_keyframe
            || self.frame_no == 0
            || self.gop_pos >= H264_SYNTH_PSLICES.len()
            || self.frame_no.is_multiple_of(15);
        self.frame_no = self.frame_no.wrapping_add(1);
        if emit_idr {
            let idr = if (self.idr_parity & 1) == 0 {
                H264_SYNTH_IDR
            } else {
                H264_SYNTH_IDR_ALT
            };
            self.idr_parity = self.idr_parity.wrapping_add(1);
            self.gop_pos = 0;
            annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS, idr])
        } else {
            let p = H264_SYNTH_PSLICES[self.gop_pos.min(H264_SYNTH_PSLICES.len() - 1)];
            self.gop_pos += 1;
            annexb_au(&[p])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packetize_uses_pt98_for_single_and_multi_packet_keyframes_and_pt97_for_delta() {
        let small_kf = annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS, H264_SYNTH_IDR]);
        assert!(small_kf.len() < VIDEO_RTP_MAX_PAYLOAD);
        let single_kf = packetize_zalo_au(&small_kf, VIDEO_RTP_MAX_PAYLOAD);
        assert_eq!(single_kf.len(), 1);
        assert_eq!(single_kf[0].0, VIDEO_PT_KEYFRAME);
        assert_eq!(single_kf[0].1, small_kf);

        let delta = annexb_au(&[H264_SYNTH_PSLICES[0]]);
        let single_delta = packetize_zalo_au(&delta, VIDEO_RTP_MAX_PAYLOAD);
        assert_eq!(single_delta.len(), 1);
        assert_eq!(single_delta[0].0, VIDEO_PT_DELTA);
        assert_eq!(single_delta[0].1, delta);
    }

    fn build_big_kf_4pkts() -> (Vec<u8>, Vec<(u8, Vec<u8>)>) {
        // Construct a ~3.6 KiB keyframe AU so it splits into 4 Zalo 0x1C fragments at max=1200.
        let big_idr: Vec<u8> = std::iter::once(0x65)
            .chain(std::iter::repeat_n(0xAB, 3600))
            .collect();
        let big_kf = annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS, &big_idr]);
        let pkts = packetize_zalo_au(&big_kf, VIDEO_RTP_MAX_PAYLOAD);
        (big_kf, pkts)
    }

    #[test]
    fn zalo_frag_even_split_roundtrip_and_lost_start_safety() {
        // Construct a ~2.5 KiB keyframe AU so it splits evenly across 3 fragments at max=1200.
        let big_idr: Vec<u8> = std::iter::once(0x65)
            .chain(std::iter::repeat_n(0xAB, 2400))
            .collect();
        let big_kf = annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS, &big_idr]);
        let pkts = packetize_zalo_au(&big_kf, VIDEO_RTP_MAX_PAYLOAD);
        assert_eq!(pkts.len(), 3);
        assert!(pkts.iter().all(|(pt, _)| *pt == VIDEO_PT_KEYFRAME));
        assert_eq!(pkts[0].1[..2], [ZALO_FRAG_INDICATOR, ZALO_FRAG_START]);
        assert_eq!(pkts[1].1[..2], [ZALO_FRAG_INDICATOR, 0x00]);
        assert_eq!(pkts[2].1[..2], [ZALO_FRAG_INDICATOR, ZALO_FRAG_END]);
        // Even split: sizes differ by at most `parts - 1` bytes (e.g. 816/816/814, matching 853/853/851)
        let min_len = pkts.iter().map(|(_, p)| p.len()).min().unwrap();
        let max_len = pkts.iter().map(|(_, p)| p.len()).max().unwrap();
        assert!(max_len - min_len < pkts.len(), "min={min_len} max={max_len}");

        let mut dep = VideoDepacketizer::new();
        assert_eq!(
            dep.push_rtp(0x11, 10, 3000, &pkts[0].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x11, 11, 3000, &pkts[1].1),
            DepacketizeOutcome::Incomplete
        );
        // Duplicate seq 11 ignored
        assert_eq!(
            dep.push_rtp(0x11, 11, 3000, &pkts[1].1),
            DepacketizeOutcome::Duplicate
        );
        match dep.push_rtp(0x11, 12, 3000, &pkts[2].1) {
            DepacketizeOutcome::AccessUnit {
                au,
                is_keyframe,
                just_synced,
            } => {
                assert!(is_keyframe);
                assert!(just_synced);
                assert_eq!(au, big_kf);
            }
            other => panic!("expected keyframe AU, got {other:?}"),
        }

        // Lost start fragment -> trailing fragments must NOT emit garbage!
        assert_eq!(
            dep.push_rtp(0x11, 13, 6000, &pkts[1].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x11, 14, 6000, &pkts[2].1),
            DepacketizeOutcome::Incomplete
        );
    }

    #[test]
    fn depacketizer_drops_incomplete_zalo_frag_on_missing_middle() {
        let (_big_kf, pkts) = build_big_kf_4pkts();
        assert_eq!(pkts.len(), 4);

        let mut dep = VideoDepacketizer::new();
        // Drop seq 101 in the middle of ts=90_000
        assert_eq!(
            dep.push_rtp(0x11, 100, 90_000, &pkts[0].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x11, 102, 90_000, &pkts[2].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x11, 103, 90_000, &pkts[3].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(dep.access_units_out(), 0);
        assert_eq!(dep.dropped_incomplete(), 0);

        // Next complete frame (ts=93_000) evicts incomplete frame and emits cleanly
        let next_kf = annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS, H264_SYNTH_IDR]);
        match dep.push_rtp(0x11, 104, 93_000, &next_kf) {
            DepacketizeOutcome::AccessUnit {
                au,
                is_keyframe,
                just_synced,
            } => {
                assert!(is_keyframe);
                assert!(just_synced);
                assert_eq!(au, next_kf);
            }
            other => panic!("expected clean next frame AU, got {other:?}"),
        }
        assert_eq!(dep.dropped_incomplete(), 1);
    }

    #[test]
    fn depacketizer_reassembles_out_of_order_and_ignores_stale_straggler() {
        let (big_kf, pkts) = build_big_kf_4pkts();
        assert_eq!(pkts.len(), 4);

        let mut dep = VideoDepacketizer::new();
        // Feed ts=90_000 out of order: 101, late straggler from ts=87_000, 100, 103, 102
        assert_eq!(
            dep.push_rtp(0x11, 101, 90_000, &pkts[1].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x11, 98, 87_000, &pkts[0].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x11, 100, 90_000, &pkts[0].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x11, 103, 90_000, &pkts[3].1),
            DepacketizeOutcome::Incomplete
        );
        match dep.push_rtp(0x11, 102, 90_000, &pkts[2].1) {
            DepacketizeOutcome::AccessUnit {
                au,
                is_keyframe,
                just_synced,
            } => {
                assert!(is_keyframe);
                assert!(just_synced);
                assert_eq!(au, big_kf);
            }
            other => panic!("expected bit-exact out-of-order AU, got {other:?}"),
        }
        assert_eq!(dep.dropped_incomplete(), 0);
    }

    #[test]
    fn depacketizer_handles_zalo_frag_seq_wraparound() {
        let (big_kf, pkts) = build_big_kf_4pkts();
        assert_eq!(pkts.len(), 4);

        let mut dep = VideoDepacketizer::new();
        assert_eq!(
            dep.push_rtp(0x11, 65534, 180_000, &pkts[0].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x11, 65535, 180_000, &pkts[1].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x11, 0, 180_000, &pkts[2].1),
            DepacketizeOutcome::Incomplete
        );
        match dep.push_rtp(0x11, 1, 180_000, &pkts[3].1) {
            DepacketizeOutcome::AccessUnit {
                au,
                is_keyframe,
                just_synced,
            } => {
                assert!(is_keyframe);
                assert!(just_synced);
                assert_eq!(au, big_kf);
            }
            other => panic!("expected wraparound AU, got {other:?}"),
        }
        assert_eq!(dep.dropped_incomplete(), 0);
    }

    #[test]
    fn depacketizer_drops_incomplete_rfc6184_fua_on_missing_middle() {
        let mut dep = VideoDepacketizer::new();
        // Sync first with IDR
        let idr = annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS, H264_SYNTH_IDR]);
        assert!(matches!(
            dep.push_rtp(0x22, 199, 87_000, &idr),
            DepacketizeOutcome::AccessUnit { .. }
        ));

        // 3-fragment FU-A P-slice where seq 201 is missing
        let p = H264_SYNTH_PSLICES[0];
        let fu_ind = (p[0] & 0xE0) | 28;
        let fu0 = vec![fu_ind, 0x80 | (p[0] & 0x1F), p[1], p[2]];
        let mut fu2 = vec![fu_ind, 0x40 | (p[0] & 0x1F)];
        fu2.extend_from_slice(&p[4..]);

        assert_eq!(
            dep.push_rtp(0x22, 200, 90_000, &fu0),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x22, 202, 90_000, &fu2),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(dep.access_units_out(), 1);

        // Next complete P-frame arrives cleanly and records the dropped incomplete FU-A
        let next_p = annexb_au(&[H264_SYNTH_PSLICES[1]]);
        match dep.push_rtp(0x22, 203, 93_000, &next_p) {
            DepacketizeOutcome::AccessUnit { au, is_keyframe, .. } => {
                assert!(!is_keyframe);
                assert_eq!(au, next_p);
            }
            other => panic!("expected clean next P-slice AU, got {other:?}"),
        }
        assert_eq!(dep.dropped_incomplete(), 1);
    }

    #[test]
    fn depacketizer_sync_gate_and_rfc6184_fallback_and_normalize() {
        let mut dep = VideoDepacketizer::new();
        let p_frame = annexb_au(&[H264_SYNTH_PSLICES[0]]);
        assert_eq!(
            dep.push_rtp(0x22, 1, 3000, &p_frame),
            DepacketizeOutcome::DroppedBeforeSync
        );
        assert_eq!(dep.dropped_before_sync(), 1);
        assert!(!dep.is_synced());

        // Single-NAL IDR (RFC 6184 type 5 without start code) unlocks sync
        let rfc_idr = H264_SYNTH_IDR.to_vec();
        match dep.push_rtp(0x22, 2, 6000, &rfc_idr) {
            DepacketizeOutcome::AccessUnit {
                au,
                is_keyframe,
                just_synced,
            } => {
                assert!(is_keyframe);
                assert!(just_synced);
                assert_eq!(au, annexb_au(&[H264_SYNTH_IDR]));
            }
            other => panic!("expected RFC 6184 IDR, got {other:?}"),
        }

        // STAP-A carrying SPS + PPS only is cached without emitting a VCL-less AU
        let mut stap_a = vec![24u8];
        stap_a.extend_from_slice(&(H264_SYNTH_SPS.len() as u16).to_be_bytes());
        stap_a.extend_from_slice(H264_SYNTH_SPS);
        stap_a.extend_from_slice(&(H264_SYNTH_PPS.len() as u16).to_be_bytes());
        stap_a.extend_from_slice(H264_SYNTH_PPS);
        assert_eq!(
            dep.push_rtp(0x22, 3, 9000, &stap_a),
            DepacketizeOutcome::Incomplete
        );
        // Subsequent bare IDR prepends the cached SPS + PPS cleanly
        match dep.push_rtp(0x22, 4, 9000, &rfc_idr) {
            DepacketizeOutcome::AccessUnit {
                au,
                is_keyframe,
                just_synced,
            } => {
                assert!(is_keyframe);
                assert!(!just_synced);
                assert_eq!(au, annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS, H264_SYNTH_IDR]));
            }
            other => panic!("expected IDR with cached SPS+PPS, got {other:?}"),
        }

        // FU-A fragmented P-slice (type 1, 2 parts)
        let p = H264_SYNTH_PSLICES[1];
        let fu_ind = (p[0] & 0xE0) | 28;
        let fu1 = vec![fu_ind, 0x80 | (p[0] & 0x1F), p[1], p[2]];
        let mut fu2 = vec![fu_ind, 0x40 | (p[0] & 0x1F)];
        fu2.extend_from_slice(&p[3..]);
        assert_eq!(
            dep.push_rtp(0x22, 5, 12_000, &fu1),
            DepacketizeOutcome::Incomplete
        );
        match dep.push_rtp(0x22, 6, 12_000, &fu2) {
            DepacketizeOutcome::AccessUnit { au, is_keyframe, .. } => {
                assert!(!is_keyframe);
                assert_eq!(au, annexb_au(&[p]));
            }
            other => panic!("expected FU-A P-slice, got {other:?}"),
        }

        // 3-byte start codes + SEI(6) + AUD(9) normalized cleanly to 4-byte without SEI/AUD
        let raw_with_sei = [
            0, 0, 0, 1, 0x09, 0xF0, // AUD
            0, 0, 0, 1, 0x06, 0x05, 0xAA, // SEI
            0, 0, 1, 0x41, 0x9A, 0x24, // 3-byte P-slice
        ];
        assert_eq!(
            normalize_annexb_au_4byte(&raw_with_sei),
            vec![0, 0, 0, 1, 0x41, 0x9A, 0x24]
        );
    }

    #[test]
    fn synthetic_h264_generator_produces_periodic_and_forced_keyframes() {
        let mut gen = SyntheticH264Generator::new();
        let f0 = gen.next_frame(false);
        assert!(au_is_sync_point(&f0) && au_has_idr(&f0));
        assert_eq!(describe_nal_types(&f0), "SPS+PPS+IDR");

        let f1 = gen.next_frame(false);
        assert!(!au_has_idr(&f1));
        assert_eq!(describe_nal_types(&f1), "P");

        let f2 = gen.next_frame(true);
        assert!(au_has_idr(&f2));
    }

    #[test]
    fn depacketizer_caches_sps_pps_requires_idr_for_sync_and_prepends_params() {
        assert!(!au_is_sync_point(&annexb_au(&[H264_SYNTH_SPS])));
        assert!(!au_is_sync_point(&annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS])));
        assert!(au_is_sync_point(&annexb_au(&[H264_SYNTH_IDR])));

        let mut dep = VideoDepacketizer::new();
        // Standalone SPS & PPS do NOT unlock sync and do NOT emit VCL-less AUs
        assert_eq!(
            dep.push_rtp(0x33, 1, 3000, H264_SYNTH_SPS),
            DepacketizeOutcome::Incomplete
        );
        assert!(!dep.is_synced());
        assert_eq!(dep.access_units_out(), 0);

        assert_eq!(
            dep.push_rtp(0x33, 2, 3000, H264_SYNTH_PPS),
            DepacketizeOutcome::Incomplete
        );
        assert!(!dep.is_synced());
        assert_eq!(dep.access_units_out(), 0);

        // Delta frame after parameter sets but before IDR is still dropped before sync
        assert_eq!(
            dep.push_rtp(0x33, 3, 6000, H264_SYNTH_PSLICES[0]),
            DepacketizeOutcome::DroppedBeforeSync
        );
        assert!(!dep.is_synced());
        assert_eq!(dep.dropped_before_sync(), 1);

        // Bare IDR unlocks sync and prepends cached SPS + PPS
        match dep.push_rtp(0x33, 4, 9000, H264_SYNTH_IDR) {
            DepacketizeOutcome::AccessUnit {
                au,
                is_keyframe,
                just_synced,
            } => {
                assert!(is_keyframe);
                assert!(just_synced);
                assert_eq!(
                    au,
                    annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS, H264_SYNTH_IDR])
                );
            }
            other => panic!("expected IDR with prepended SPS+PPS, got {other:?}"),
        }
        assert!(dep.is_synced());
        assert_eq!(dep.keyframes_out(), 1);
    }

    #[test]
    fn depacketizer_reset_sync_clears_recent_seqs_for_peer_restart() {
        let mut dep = VideoDepacketizer::new();
        let kf = annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS, H264_SYNTH_IDR]);
        let p = annexb_au(&[H264_SYNTH_PSLICES[0]]);

        assert!(matches!(
            dep.push_rtp(0x44, 0, 0, &kf),
            DepacketizeOutcome::AccessUnit {
                is_keyframe: true,
                just_synced: true,
                ..
            }
        ));
        assert!(matches!(
            dep.push_rtp(0x44, 1, 6000, &p),
            DepacketizeOutcome::AccessUnit {
                is_keyframe: false,
                ..
            }
        ));

        // After idle timeout or camera restart, peer restarts at seq=0 with same SSRC
        dep.reset_sync();
        assert!(!dep.is_synced());
        match dep.push_rtp(0x44, 0, 0, &kf) {
            DepacketizeOutcome::AccessUnit {
                au,
                is_keyframe,
                just_synced,
            } => {
                assert!(is_keyframe);
                assert!(just_synced);
                assert_eq!(au, kf);
            }
            other => panic!("seq=0 after reset_sync must not be dropped as Duplicate, got {other:?}"),
        }
    }

    #[test]
    fn depacketizer_drops_stale_unfragmented_frames_and_rejects_fua_header_mismatch() {
        let (big_kf, pkts) = build_big_kf_4pkts();
        assert_eq!(pkts.len(), 4);

        let mut dep = VideoDepacketizer::new();
        // Start assembling fragmented ts=90_000
        assert_eq!(
            dep.push_rtp(0x55, 100, 90_000, &pkts[0].1),
            DepacketizeOutcome::Incomplete
        );
        // Stale unfragmented packet at ts=87_000 MUST be dropped, NOT emitted out of order
        let stale_kf = annexb_au(&[H264_SYNTH_SPS, H264_SYNTH_PPS, H264_SYNTH_IDR]);
        assert_eq!(
            dep.push_rtp(0x55, 99, 87_000, &stale_kf),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(dep.access_units_out(), 0);

        // Active ts=90_000 finishes cleanly
        assert_eq!(
            dep.push_rtp(0x55, 101, 90_000, &pkts[1].1),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x55, 102, 90_000, &pkts[2].1),
            DepacketizeOutcome::Incomplete
        );
        match dep.push_rtp(0x55, 103, 90_000, &pkts[3].1) {
            DepacketizeOutcome::AccessUnit { au, .. } => assert_eq!(au, big_kf),
            other => panic!("expected ts=90_000 AU, got {other:?}"),
        }

        // Stale unfragmented packet arriving after ts=90_000 was emitted is also dropped
        let stale_p = annexb_au(&[H264_SYNTH_PSLICES[0]]);
        assert_eq!(
            dep.push_rtp(0x55, 104, 87_000, &stale_p),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x55, 105, 84_000, H264_SYNTH_PSLICES[1]),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(dep.access_units_out(), 1);

        // FU-A fragments sharing a timestamp with mismatched NAL header MUST NOT combine
        let idr_fu_ind = (H264_SYNTH_IDR[0] & 0xE0) | 28;
        let idr_fu_start = vec![idr_fu_ind, 0x80 | (H264_SYNTH_IDR[0] & 0x1F), 0x88, 0x84];
        let p = H264_SYNTH_PSLICES[0];
        let p_fu_ind = (p[0] & 0xE0) | 28;
        let p_fu_end_mismatch = vec![p_fu_ind, 0x40 | (p[0] & 0x1F), p[1], p[2]];

        assert_eq!(
            dep.push_rtp(0x55, 110, 96_000, &idr_fu_start),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(
            dep.push_rtp(0x55, 111, 96_000, &p_fu_end_mismatch),
            DepacketizeOutcome::Incomplete
        );
        assert_eq!(dep.access_units_out(), 1);
        assert_eq!(dep.dropped_incomplete(), 1);
    }
}
