//! Error types and network-mode guards for `teza-call`.

use thiserror::Error;
use zalo_core::session::Session;

/// Result alias for `teza-call` operations.
pub type CallResult<T> = std::result::Result<T, CallError>;

/// Errors emitted by VoIP signaling, UDP ZRTP transport, audio pipeline, and call engine.
#[derive(Debug, Error)]
pub enum CallError {
    /// Voice/video calling requires direct UDP/network access and is explicitly disabled
    /// whenever intranet Relay mode (`session.relay.enabled == true`) is active.
    #[error("Gọi thoại/video không hỗ trợ khi đang bật chế độ Relay (mạng nội bộ)")]
    RelayUnsupported,

    #[error("Phiên đăng nhập chưa được xác thực")]
    NotAuthenticated,

    #[error("Phiên đăng nhập đã hết hạn (mã {code})")]
    SessionExpired { code: i64 },

    #[error("Lỗi tín hiệu cuộc gọi (mã {code}): {message}")]
    Signaling { code: i64, message: String },

    #[error("Lỗi giao thức VoIP: {0}")]
    Protocol(String),

    #[error("Lỗi mạng/UDP: {0}")]
    Network(String),

    #[error("Lỗi âm thanh/codec: {0}")]
    Audio(String),

    #[error("Trạng thái cuộc gọi không hợp lệ: {0}")]
    InvalidState(String),

    #[error("Tham số không hợp lệ: {0}")]
    InvalidArgument(String),
}

impl CallError {
    /// Returns `true` if this error is [`CallError::RelayUnsupported`].
    pub fn is_relay_unsupported(&self) -> bool {
        matches!(self, Self::RelayUnsupported)
    }

    /// Returns `true` if this error represents a transport timeout (used to re-arm idle long-polls
    /// without error backoff).
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Network(msg) if msg.starts_with("timeout:") || msg.contains("timed out"))
    }
}

/// Enforce the hard product invariant: calling is never permitted when Relay mode is enabled.
pub fn ensure_direct_network(session: &Session) -> CallResult<()> {
    if session.relay.enabled {
        return Err(CallError::RelayUnsupported);
    }
    Ok(())
}

/// Enforce both direct-network mode and an authenticated `zalo-core::Session`.
pub fn ensure_callable_session(session: &Session) -> CallResult<()> {
    ensure_direct_network(session)?;
    if !session.is_authenticated() || session.effective_zpw_sek().is_none() {
        return Err(CallError::NotAuthenticated);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zalo_core::relay::{RelayConfig, RelayMode};

    #[test]
    fn relay_enabled_is_rejected_before_auth_check() {
        let mut sess = Session::new().unwrap();
        sess.set_relay(RelayConfig {
            enabled: true,
            mode: RelayMode::Socks5,
            host: "10.0.0.1".into(),
            port: 1088,
            username: String::new(),
            password: String::new(),
            route_webview_media: true,
        })
        .unwrap();

        let err = ensure_direct_network(&sess).unwrap_err();
        assert!(err.is_relay_unsupported());
        let err2 = ensure_callable_session(&sess).unwrap_err();
        assert!(err2.is_relay_unsupported());
    }

    #[test]
    fn unauthenticated_direct_session_returns_not_authenticated() {
        let sess = Session::new().unwrap();
        ensure_direct_network(&sess).unwrap();
        let err = ensure_callable_session(&sess).unwrap_err();
        assert!(matches!(err, CallError::NotAuthenticated));
    }

    #[test]
    fn authenticated_direct_session_passes_guard() {
        let mut sess = Session::new().unwrap();
        sess.set_zpw_sek("valid_sek".into());
        sess.set_zpw_enk("0123456789abcdef0123456789abcdef");
        ensure_callable_session(&sess).unwrap();
    }
}
