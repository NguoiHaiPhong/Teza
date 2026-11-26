//! `teza-video` — Isolated pure-Rust 1-1 H.264/RTP/ZFrag/RTCP video pipeline and
//! [`VideoMediaHook`] for `teza-call`.

pub mod ffmpeg_cli;
pub mod framing;
pub mod rtcp;
pub mod session;

pub use ffmpeg_cli::{
    filter_camera_list, is_ffmpeg_available, is_ignored_camera_device, list_cameras,
    FfmpegCameraCapture, H264FileRecorder,
};
pub use framing::{
    annexb_au, au_has_idr, au_has_nal_type, au_is_sync_point, describe_nal_types,
    find_aud_delimiter, is_annexb, is_video_payload_type, is_zalo_frag, normalize_annexb_au_4byte,
    packetize_zalo_au, strip_leading_aud, DepacketizeOutcome, SyntheticH264Generator,
    VideoDepacketizer, H264_AUD, VIDEO_CLOCK_HZ, VIDEO_PT_DELTA, VIDEO_PT_KEYFRAME,
    VIDEO_RTP_MAX_PAYLOAD, ZALO_FRAG_BUF_MAX, ZALO_FRAG_END, ZALO_FRAG_INDICATOR, ZALO_FRAG_START,
};
pub use rtcp::{
    build_rtcp_pli, build_rtcp_remb, build_rtcp_rr_pli, build_rtcp_sdes_empty, build_rtcp_sr,
    build_video_sr_compound, build_zrtp_incall_negotiate_packet, ntp_now, rtcp_requests_keyframe,
};
pub use session::{
    run_local_video_loopback, InboundFrameCallback, InboundVideoFrame, KeyframeRequestCallback,
    VideoMediaHook, VideoSession, VideoSessionConfig, VideoTelemetry,
};
