//! Video RTCP builders (`SR`, `SDES`, `RR+PLI`, `REMB`), incoming PLI/FIR inspection,
//! and ZRTP `IncallNegotiateInfo` (`0x0020/0x14`) builders verified against `libzrtc.so`.

use std::time::{SystemTime, UNIX_EPOCH};

/// Return the current 64-bit NTP timestamp `(seconds_since_1900, fraction_2pow_32)`.
pub fn ntp_now() -> (u32, u32) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs().wrapping_add(2_208_988_800) as u32;
    let frac = ((u64::from(d.subsec_nanos())) << 32) / 1_000_000_000;
    (secs, frac as u32)
}

/// Build an RFC 3550 RTCP Sender Report (`PT 200`), optionally carrying one 24-byte report block.
pub fn build_rtcp_sr(
    ssrc: u32,
    ntp: (u32, u32),
    rtp_ts: u32,
    packets: u32,
    octets: u32,
    report_block: Option<[u8; 24]>,
) -> Vec<u8> {
    let rc = u8::from(report_block.is_some());
    let words = 6 + 6 * u16::from(rc);
    let mut p = Vec::with_capacity(28 + usize::from(rc) * 24);
    p.push(0x80 | rc);
    p.push(200);
    p.extend_from_slice(&words.to_be_bytes());
    p.extend_from_slice(&ssrc.to_be_bytes());
    p.extend_from_slice(&ntp.0.to_be_bytes());
    p.extend_from_slice(&ntp.1.to_be_bytes());
    p.extend_from_slice(&rtp_ts.to_be_bytes());
    p.extend_from_slice(&packets.to_be_bytes());
    p.extend_from_slice(&octets.to_be_bytes());
    if let Some(block) = report_block {
        p.extend_from_slice(&block);
    }
    p
}

/// Build an empty 12-byte RFC 3550 RTCP SDES (`PT 202`, `SC = 1`, zero items) matching Zalo peers.
pub fn build_rtcp_sdes_empty(ssrc: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(12);
    p.extend_from_slice(&[0x81, 202, 0x00, 0x02]);
    p.extend_from_slice(&ssrc.to_be_bytes());
    p.extend_from_slice(&[0, 0, 0, 0]);
    p
}

/// Build an RFC 4585 RTCP Picture Loss Indication (`PSFB`, `fmt = 1`, `PT = 206`, 12 bytes).
pub fn build_rtcp_pli(sender_ssrc: u32, media_ssrc: u32) -> Vec<u8> {
    let mut p = Vec::with_capacity(12);
    p.extend_from_slice(&[0x81, 206, 0x00, 0x02]);
    p.extend_from_slice(&sender_ssrc.to_be_bytes());
    p.extend_from_slice(&media_ssrc.to_be_bytes());
    p
}

/// Build a compound RTCP `empty RR + PLI` (20 bytes) per RFC 3550 §6.1 / RFC 4585.
pub fn build_rtcp_rr_pli(sender_ssrc: u32, media_ssrc: u32) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(20);
    pkt.extend_from_slice(&[0x80, 201, 0x00, 0x01]);
    pkt.extend_from_slice(&sender_ssrc.to_be_bytes());
    pkt.extend_from_slice(&build_rtcp_pli(sender_ssrc, media_ssrc));
    pkt
}

/// Build an RTCP REMB (`PSFB`, `fmt = 15`, `PT = 206`, 24 bytes) advertising receiver max bitrate.
pub fn build_rtcp_remb(sender_ssrc: u32, media_ssrc: u32, bitrate_bps: u32) -> Vec<u8> {
    let mut exp: u8 = 0;
    let mut mantissa: u32 = bitrate_bps;
    while mantissa > 0x3FFFF && exp < 63 {
        mantissa >>= 1;
        exp += 1;
    }
    let mut p = Vec::with_capacity(24);
    p.extend_from_slice(&[0x8F, 206, 0x00, 0x05]);
    p.extend_from_slice(&sender_ssrc.to_be_bytes());
    p.extend_from_slice(&0u32.to_be_bytes());
    p.extend_from_slice(b"REMB");
    p.push(1);
    p.push((exp << 2) | ((mantissa >> 16) as u8 & 0x03));
    p.extend_from_slice(&(mantissa as u16).to_be_bytes());
    p.extend_from_slice(&media_ssrc.to_be_bytes());
    p
}

/// Build the periodic compound Video RTCP packet (`SR + empty SDES [+ REMB when peer_ssrc != 0]`).
#[allow(clippy::too_many_arguments)]
pub fn build_video_sr_compound(
    sender_ssrc: u32,
    peer_ssrc: u32,
    ntp: (u32, u32),
    rtp_ts: u32,
    packets: u32,
    octets: u32,
    report_block: Option<[u8; 24]>,
    remb_bitrate_bps: u32,
) -> Vec<u8> {
    let mut pkt = build_rtcp_sr(sender_ssrc, ntp, rtp_ts, packets, octets, report_block);
    pkt.extend_from_slice(&build_rtcp_sdes_empty(sender_ssrc));
    if peer_ssrc != 0 && remb_bitrate_bps > 0 {
        pkt.extend_from_slice(&build_rtcp_remb(sender_ssrc, peer_ssrc, remb_bitrate_bps));
    }
    pkt
}

/// Return `true` when an RFC 3550 (possibly compound) RTCP packet contains a PLI (`PT=206, fmt=1`)
/// or FIR (`PT=206, fmt=4`) keyframe request.
pub fn rtcp_requests_keyframe(rtcp: &[u8]) -> bool {
    let mut i = 0usize;
    while i + 4 <= rtcp.len() {
        if (rtcp[i] & 0xC0) != 0x80 {
            return false;
        }
        let fmt = rtcp[i] & 0x1F;
        let pt = rtcp[i + 1];
        let words = u16::from_be_bytes([rtcp[i + 2], rtcp[i + 3]]) as usize;
        let pkt_len = (words + 1).saturating_mul(4);
        if pkt_len < 4 || i.checked_add(pkt_len).is_none_or(|next| next > rtcp.len()) {
            return false;
        }
        if pt == 206 && (fmt == 1 || fmt == 4) {
            return true;
        }
        i += pkt_len;
    }
    false
}

/// Build `ZRTPPacket::initZRTPPacketIncallNegotiateInfo` (36 bytes):
/// `cmd = 0x0020`, `sub_cmd = 0x14`, carrying `H264DecCapacityInfo(720p, 30fps)` and `NegoNetworkInfo(1)`.
pub fn build_zrtp_incall_negotiate_packet(
    call_or_partner_id: u32,
    relay_token: u32,
    nego_seq: u32,
) -> Vec<u8> {
    let mut body = Vec::with_capacity(11);
    body.extend_from_slice(&nego_seq.to_be_bytes());
    body.push(2);
    body.extend_from_slice(&[0x0B, 0x02, 0xD0, 0x1E]);
    body.extend_from_slice(&[0x29, 0x01]);

    let mut pkt = Vec::with_capacity(25 + body.len());
    pkt.push(0x01);
    pkt.push(0x01);
    pkt.extend_from_slice(&nego_seq.to_le_bytes());
    pkt.extend_from_slice(&0u32.to_le_bytes());
    pkt.extend_from_slice(&call_or_partner_id.to_le_bytes());
    pkt.extend_from_slice(&relay_token.to_le_bytes());
    pkt.extend_from_slice(&0x0020u16.to_le_bytes());
    pkt.push(0x14);
    pkt.extend_from_slice(&(body.len() as u32).to_le_bytes());
    pkt.extend_from_slice(&body);
    pkt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtcp_sr_sdes_remb_and_pli_match_wire_layouts() {
        let block = [0xAA; 24];
        let sr = build_rtcp_sr(
            0x1122_3344,
            (0x1234_5678, 0x9ABC_DEF0),
            9000,
            15,
            12_345,
            Some(block),
        );
        assert_eq!(sr.len(), 52);
        assert_eq!(sr[0], 0x81);
        assert_eq!(sr[1], 200);
        assert_eq!(u16::from_be_bytes([sr[2], sr[3]]), 12);

        let sdes = build_rtcp_sdes_empty(0x1122_3344);
        assert_eq!(sdes.len(), 12);
        assert_eq!(&sdes[0..4], &[0x81, 202, 0x00, 0x02]);

        let remb = build_rtcp_remb(0x19F5_F926, 0x1BBA_1962, 1_500_000);
        assert_eq!(remb.len(), 24);
        assert_eq!(&remb[0..4], &[0x8F, 206, 0x00, 0x05]);
        assert_eq!(&remb[12..16], b"REMB");
        let exp = remb[17] >> 2;
        let mant =
            (((remb[17] & 0x03) as u32) << 16) | (u16::from_be_bytes([remb[18], remb[19]]) as u32);
        let decoded_bps = mant << exp;
        assert!(
            (1_490_000..=1_500_000).contains(&decoded_bps),
            "decoded={decoded_bps}"
        );
        assert_eq!(&remb[20..24], &0x1BBA_1962u32.to_be_bytes());

        let rr_pli = build_rtcp_rr_pli(0x1122_3344, 0x5566_7788);
        assert_eq!(rr_pli.len(), 20);
        assert!(rtcp_requests_keyframe(&rr_pli));
    }

    #[test]
    fn rtcp_requests_keyframe_handles_compound_pli_fir_and_non_pli() {
        let mut compound = build_rtcp_sr(0x01, (1, 2), 3, 4, 5, None);
        compound.extend_from_slice(&build_rtcp_sdes_empty(0x01));
        assert!(!rtcp_requests_keyframe(&compound));

        // Generic NACK (PT=205, fmt=1) must NOT trigger keyframe request
        let nack = [0x81u8, 205, 0x00, 0x02, 0, 0, 0, 1, 0, 0, 0, 2];
        assert!(!rtcp_requests_keyframe(&nack));

        compound.extend_from_slice(&build_rtcp_pli(0x01, 0x02));
        assert!(rtcp_requests_keyframe(&compound));

        // FIR (PT=206, fmt=4)
        let fir = [0x84u8, 206, 0x00, 0x02, 0, 0, 0, 1, 0, 0, 0, 2];
        assert!(rtcp_requests_keyframe(&fir));
    }

    #[test]
    fn incall_negotiate_info_matches_36_byte_wire_contract() {
        let nego = build_zrtp_incall_negotiate_packet(0x1BBA_1962, 0x3955_E3D2, 7);
        assert_eq!(nego.len(), 36);
        assert_eq!(&nego[0..2], &[0x01, 0x01]);
        assert_eq!(u16::from_le_bytes([nego[18], nego[19]]), 0x0020);
        assert_eq!(nego[20], 0x14);
        assert_eq!(
            u32::from_le_bytes([nego[21], nego[22], nego[23], nego[24]]),
            11
        );
        assert_eq!(
            &nego[25..36],
            &[0, 0, 0, 7, 2, 0x0B, 0x02, 0xD0, 0x1E, 0x29, 0x01]
        );
    }
}
