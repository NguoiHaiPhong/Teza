//! Single source of truth for verified Zalo Web/PC service endpoints.

pub const SERVICE_MAP_V3: &[(&str, &str)] = &[
    ("profile", "https://tt-profile-wpa.chat.zalo.me"),
    ("friend", "https://tt-friend-wpa.chat.zalo.me"),
    ("conversation", "https://tt-convers-wpa.chat.zalo.me"),
    ("chat", "https://tt-chat3-wpa.chat.zalo.me"),
    ("group", "https://tt-group-wpa.chat.zalo.me"),
    ("sticker", "https://tt-sticker-wpa.chat.zalo.me"),
    ("alias", "https://tt-alias-wpa.chat.zalo.me"),
    ("label", "https://label-wpa.chat.zalo.me"),
    ("reaction", "https://reaction.chat.zalo.me"),
    ("emoji_status", "https://emoji-status.chat.zalo.me"),
    ("zcloud", "https://zcld.chat.zalo.me"),
    ("group_cloud_message", "https://tt-group-cm.api.zaloapp.com"),
    ("file", "https://tt-files-wpa.chat.zalo.me"),
    ("voice_call", "https://voicecall-wpa.chat.zalo.me"),
];

pub const CONVERS_CTL: &str = "https://p5-ctl-chat.zaloapp.com";
pub const MSG_CHAT: &str = "https://p5-msg-chat.zaloapp.com";
pub const CLOUD_MSG_FALLBACK: &str = "https://tt-group-cm.api.zaloapp.com";
pub const REACTION_FALLBACK: &str = "https://reaction.api.zaloapp.com";
pub const VOICE_CALL_FALLBACK: &str = "https://voicecall-wpa.chat.zalo.me";
pub const AUTH: &str = "https://wpa.zaloapp.com";
pub const WPA_CHAT: &str = "https://wpa.chat.zalo.me";
pub const GROUP_BOARD: &str = "https://groupboard-wpa.chat.zalo.me";
pub const FRIEND_BOARD: &str = "https://friendboard-wpa.chat.zalo.me";
pub const BOARD: &str = "https://board-wpa.chat.zalo.me";
pub const REMINDER: &str = "https://reminder-wpa.chat.zalo.me";

/// Look up an authoritative service base URL by key.
pub fn service(key: &str) -> Option<&'static str> {
    SERVICE_MAP_V3
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
}
