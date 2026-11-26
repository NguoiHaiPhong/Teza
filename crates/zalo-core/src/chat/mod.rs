//! Domain models for Telegram-style folders, threads, messages, and Zalo chat/sync APIs.

pub mod messaging;
pub mod parser;
pub mod threads;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadKind {
    Direct,
    Group,
    Cloud,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FolderFilter {
    All,
    Unread,
    Direct,
    Group,
    Cloud,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadDto {
    pub id: String,
    pub title: String,
    pub avatar: String,
    pub kind: ThreadKind,
    pub last_preview: String,
    pub last_ts: i64,
    pub unread_count: u32,
    pub is_pinned: bool,
    pub is_muted: bool,
    #[serde(default)]
    pub last_read_in_ts: i64,
    #[serde(default)]
    pub peer_read_ts: i64,
    #[serde(default)]
    pub muted_until_ts: i64,
    #[serde(default)]
    pub is_archived: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadMemberDto {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub role: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroupDetailsResult {
    pub total_members: Option<u32>,
    pub members: Vec<ThreadMemberDto>,
    #[serde(default)]
    pub creator_id: String,
    #[serde(default)]
    pub admin_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadDetailsDto {
    pub thread_id: String,
    pub total_members: Option<u32>,
    pub members: Vec<ThreadMemberDto>,
    pub media_items: Vec<MessageDto>,
    pub file_items: Vec<MessageDto>,
    #[serde(default)]
    pub link_items: Vec<MessageDto>,
    #[serde(default)]
    pub creator_id: String,
    #[serde(default)]
    pub admin_ids: Vec<String>,
}

fn default_folder_icon() -> String {
    "folder".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatFolderDto {
    pub id: String,
    pub name: String,
    #[serde(default = "default_folder_icon")]
    pub icon: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub thread_ids: Vec<String>,
    #[serde(default)]
    pub excluded_thread_ids: Vec<String>,
    #[serde(default)]
    pub include_kinds: Vec<ThreadKind>,
    #[serde(default)]
    pub only_unread: bool,
    #[serde(default)]
    pub exclude_muted: bool,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub remote_label_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MutualGroupDto {
    pub id: String,
    pub title: String,
    pub avatar: String,
    pub total_members: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserProfileDto {
    pub uid: String,
    pub display_name: String,
    #[serde(rename = "account_name", alias = "zalo_name")]
    pub zalo_name: String,
    pub avatar: String,
    pub cover: String,
    pub status: String,
    pub gender: Option<u8>,
    pub sdob: String,
    pub phone_number: String,
    pub is_friend: Option<bool>,
    #[serde(default)]
    pub is_blocked: Option<bool>,
    pub is_self: bool,
    #[serde(default)]
    pub mutual_groups: Vec<MutualGroupDto>,
    #[serde(default)]
    pub local_message_count: u64,
    #[serde(default)]
    pub local_media_count: u64,
    #[serde(default)]
    pub last_interaction_ts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionSummary {
    pub emoji: String,
    pub count: u32,
    pub me: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddGroupMembersResultDto {
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub pending: Vec<String>,
    #[serde(default)]
    pub failed: Vec<String>,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FriendRequestItemDto {
    pub user_id: String,
    pub display_name: String,
    pub account_name: String,
    pub avatar: String,
    pub message: String,
    pub created_ts: i64,
    pub direction: String,
    pub is_seen: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FriendRequestsListDto {
    #[serde(default)]
    pub received: Vec<FriendRequestItemDto>,
    #[serde(default)]
    pub sent: Vec<FriendRequestItemDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FriendActionResultDto {
    pub ok: bool,
    pub user_id: String,
    pub action: String,
    #[serde(default)]
    pub already_friends: bool,
    #[serde(default)]
    pub auto_accepted: bool,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LeaveGroupResultDto {
    pub group_id: String,
    pub left: bool,
    #[serde(default)]
    pub member_error: Vec<String>,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KickGroupMembersResultDto {
    pub group_id: String,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub failed: Vec<String>,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollOptionDto {
    pub id: i64,
    pub content: String,
    #[serde(default)]
    pub votes: u32,
    #[serde(default)]
    pub voted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollDto {
    pub poll_id: i64,
    pub question: String,
    #[serde(default)]
    pub options: Vec<PollOptionDto>,
    #[serde(default)]
    pub allow_multi: bool,
    #[serde(default)]
    pub allow_add_option: bool,
    #[serde(default)]
    pub is_anonymous: bool,
    #[serde(default)]
    pub expired_time: i64,
    #[serde(default)]
    pub closed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuoteRef {
    pub msg_id: String,
    pub sender_name: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageDto {
    pub msg_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_msg_id: Option<String>,
    pub thread_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub text: String,
    pub msg_type: String,
    pub ts: i64,
    pub is_outgoing: bool,
    pub is_recalled: bool,
    pub is_pinned: bool,
    pub quote: Option<QuoteRef>,
    pub reactions: Vec<ReactionSummary>,
    pub attachment_url: Option<String>,
    pub attachment_name: Option<String>,
}

const HANGUL_S_BASE: u32 = 0xAC00;
const HANGUL_L_BASE: u32 = 0x1100;
const HANGUL_V_BASE: u32 = 0x1161;
const HANGUL_T_BASE: u32 = 0x11A7;
const HANGUL_L_COUNT: u32 = 19;
const HANGUL_V_COUNT: u32 = 21;
const HANGUL_T_COUNT: u32 = 28;
const HANGUL_N_COUNT: u32 = HANGUL_V_COUNT * HANGUL_T_COUNT; // 588
const HANGUL_S_COUNT: u32 = HANGUL_L_COUNT * HANGUL_N_COUNT; // 11_172
const HANGUL_S_END: u32 = HANGUL_S_BASE + HANGUL_S_COUNT - 1; // 0xD7A3

const KOREAN_CHOSEONG: [char; 19] = [
    'ㄱ', 'ㄲ', 'ㄴ', 'ㄷ', 'ㄸ', 'ㄹ', 'ㅁ', 'ㅂ', 'ㅃ', 'ㅅ', 'ㅆ', 'ㅇ', 'ㅈ', 'ㅉ', 'ㅊ', 'ㅋ', 'ㅌ',
    'ㅍ', 'ㅎ',
];

#[inline]
fn is_korean_choseong_char(ch: char) -> bool {
    KOREAN_CHOSEONG.contains(&ch)
}

/// Canonical Unicode Hangul Jamo composition pass (NFD -> NFC).
///
/// Composes `L` (`U+1100..=U+1112`) + `V` (`U+1161..=U+1175`) + optional `T`
/// (`U+11A8..=U+11C2`), as well as precomposed `LV` + `T`, into precomposed
/// Hangul syllables (`U+AC00..=U+D7A3`) without touching any other script.
pub fn compose_hangul_nfc(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        let cp = ch as u32;
        if (HANGUL_V_BASE..HANGUL_V_BASE + HANGUL_V_COUNT).contains(&cp) {
            if let Some(prev) = out.chars().next_back() {
                let prev_cp = prev as u32;
                if (HANGUL_L_BASE..HANGUL_L_BASE + HANGUL_L_COUNT).contains(&prev_cp) {
                    let l_idx = prev_cp - HANGUL_L_BASE;
                    let v_idx = cp - HANGUL_V_BASE;
                    let composed = HANGUL_S_BASE + (l_idx * HANGUL_V_COUNT + v_idx) * HANGUL_T_COUNT;
                    if let Some(syllable) = char::from_u32(composed) {
                        out.pop();
                        out.push(syllable);
                        continue;
                    }
                }
            }
        } else if (HANGUL_T_BASE + 1..HANGUL_T_BASE + HANGUL_T_COUNT).contains(&cp) {
            if let Some(prev) = out.chars().next_back() {
                let prev_cp = prev as u32;
                if (HANGUL_S_BASE..=HANGUL_S_END).contains(&prev_cp)
                    && (prev_cp - HANGUL_S_BASE).is_multiple_of(HANGUL_T_COUNT)
                {
                    let t_idx = cp - HANGUL_T_BASE;
                    if let Some(syllable) = char::from_u32(prev_cp + t_idx) {
                        out.pop();
                        out.push(syllable);
                        continue;
                    }
                }
            }
        }
        out.push(ch);
    }
    out
}

/// Returns `true` if `s` contains Hangul (Syllables, Jamo, or Compatibility Jamo),
/// CJK Unified Ideographs, or Japanese Hiragana/Katakana.
pub fn contains_cjk_or_hangul(s: &str) -> bool {
    s.chars().any(|ch| {
        let cp = ch as u32;
        (HANGUL_S_BASE..=HANGUL_S_END).contains(&cp)
            || (0x1100..=0x11FF).contains(&cp)
            || (0x3130..=0x318F).contains(&cp)
            || (0x4E00..=0x9FFF).contains(&cp)
            || (0x3400..=0x4DBF).contains(&cp)
            || (0x3040..=0x30FF).contains(&cp)
    })
}

/// Maps each Hangul syllable (`U+AC00..=U+D7A3`, composing NFD Jamo first) to its
/// compatibility initial consonant (`ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ`),
/// preserving non-Hangul characters in place.
pub fn extract_korean_choseong(s: &str) -> String {
    let composed = compose_hangul_nfc(s);
    let mut out = String::with_capacity(composed.len());
    for ch in composed.chars() {
        let cp = ch as u32;
        if (HANGUL_S_BASE..=HANGUL_S_END).contains(&cp) {
            let cho_idx = ((cp - HANGUL_S_BASE) / HANGUL_N_COUNT) as usize;
            out.push(KOREAN_CHOSEONG[cho_idx]);
        } else if (HANGUL_L_BASE..HANGUL_L_BASE + HANGUL_L_COUNT).contains(&cp) {
            out.push(KOREAN_CHOSEONG[(cp - HANGUL_L_BASE) as usize]);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Returns `true` when `s` has at least one non-whitespace character and every
/// non-whitespace character is a Korean compatibility choseong consonant.
pub fn is_korean_choseong_query(s: &str) -> bool {
    let mut has_choseong = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if !is_korean_choseong_char(ch) {
            return false;
        }
        has_choseong = true;
    }
    has_choseong
}

/// Compose NFD Hangul to NFC, strip Latin/Vietnamese diacritics (both NFC and NFD
/// combining marks `U+0300..=U+036F`), and lowercase for accent-insensitive search (`Ctrl+K`).
pub fn strip_vi_accents(input: &str) -> String {
    let composed = compose_hangul_nfc(input);
    let mut out = String::with_capacity(composed.len());
    for ch in composed.chars() {
        // Strip Latin combining diacritical marks (NFD Vietnamese/Latin) without touching Hangul/CJK.
        if ('\u{0300}'..='\u{036F}').contains(&ch) {
            continue;
        }
        let mapped = match ch {
            'à' | 'á' | 'ạ' | 'ả' | 'ã' | 'â' | 'ầ' | 'ấ' | 'ậ' | 'ẩ' | 'ẫ' | 'ă' | 'ằ' | 'ắ'
            | 'ặ' | 'ẳ' | 'ẵ' | 'ä' | 'å' | 'ā' | 'À' | 'Á' | 'Ạ' | 'Ả' | 'Ã' | 'Â' | 'Ầ' | 'Ấ'
            | 'Ậ' | 'Ẩ' | 'Ẫ' | 'Ă' | 'Ằ' | 'Ắ' | 'Ặ' | 'Ẳ' | 'Ẵ' | 'Ä' | 'Å' | 'Ā' => 'a',
            'ç' | 'Ç' => 'c',
            'è' | 'é' | 'ẹ' | 'ẻ' | 'ẽ' | 'ê' | 'ề' | 'ế' | 'ệ' | 'ể' | 'ễ' | 'ë' | 'ē' | 'È'
            | 'É' | 'Ẹ' | 'Ẻ' | 'Ẽ' | 'Ê' | 'Ề' | 'Ế' | 'Ệ' | 'Ể' | 'Ễ' | 'Ë' | 'Ē' => 'e',
            'ì' | 'í' | 'ị' | 'ỉ' | 'ĩ' | 'î' | 'ï' | 'ī' | 'Ì' | 'Í' | 'Ị' | 'Ỉ' | 'Ĩ' | 'Î'
            | 'Ï' | 'Ī' => 'i',
            'ñ' | 'Ñ' => 'n',
            'ò' | 'ó' | 'ọ' | 'ỏ' | 'õ' | 'ô' | 'ồ' | 'ố' | 'ộ' | 'ổ' | 'ỗ' | 'ơ' | 'ờ' | 'ớ'
            | 'ợ' | 'ở' | 'ỡ' | 'ö' | 'ø' | 'ō' | 'Ò' | 'Ó' | 'Ọ' | 'Ỏ' | 'Õ' | 'Ô' | 'Ồ' | 'Ố'
            | 'Ộ' | 'Ổ' | 'Ỗ' | 'Ơ' | 'Ờ' | 'Ớ' | 'Ợ' | 'Ở' | 'Ỡ' | 'Ö' | 'Ø' | 'Ō' => 'o',
            'ù' | 'ú' | 'ụ' | 'ủ' | 'ũ' | 'ư' | 'ừ' | 'ứ' | 'ự' | 'ử' | 'ữ' | 'û' | 'ü' | 'ū'
            | 'Ù' | 'Ú' | 'Ụ' | 'Ủ' | 'Ũ' | 'Ư' | 'Ừ' | 'Ứ' | 'Ự' | 'Ử' | 'Ữ' | 'Û' | 'Ü' | 'Ū' => {
                'u'
            }
            'ỳ' | 'ý' | 'ỵ' | 'ỷ' | 'ỹ' | 'ÿ' | 'Ỳ' | 'Ý' | 'Ỵ' | 'Ỷ' | 'Ỹ' | 'Ÿ' => 'y',
            'đ' | 'Đ' => 'd',
            other => other,
        };
        for lc in mapped.to_lowercase() {
            out.push(lc);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_profile_dto_serializes_account_name_and_accepts_legacy_alias() {
        let dto = UserProfileDto {
            uid: "u-1".into(),
            display_name: "Alice".into(),
            zalo_name: "alice_handle".into(),
            avatar: String::new(),
            cover: String::new(),
            status: String::new(),
            gender: Some(1),
            sdob: String::new(),
            phone_number: String::new(),
            is_friend: Some(true),
            is_blocked: Some(false),
            is_self: false,
            mutual_groups: Vec::new(),
            local_message_count: 0,
            local_media_count: 0,
            last_interaction_ts: 0,
        };
        let json_str = serde_json::to_string(&dto).unwrap();
        assert!(json_str.contains("\"account_name\":\"alice_handle\""));
        assert!(!json_str.contains("\"zalo_name\""));

        let from_new: UserProfileDto = serde_json::from_str(&json_str).unwrap();
        assert_eq!(from_new.zalo_name, "alice_handle");

        let legacy_json = json_str.replace("\"account_name\"", "\"zalo_name\"");
        let from_legacy: UserProfileDto = serde_json::from_str(&legacy_json).unwrap();
        assert_eq!(from_legacy.zalo_name, "alice_handle");
        assert_eq!(from_legacy.is_blocked, Some(false));
    }

    #[test]
    fn thread_details_and_social_dtos_roundtrip_with_defaults() {
        let minimal_details = r#"{
            "thread_id": "grp-1",
            "total_members": 3,
            "members": [],
            "media_items": [],
            "file_items": []
        }"#;
        let parsed: ThreadDetailsDto = serde_json::from_str(minimal_details).unwrap();
        assert_eq!(parsed.creator_id, "");
        assert!(parsed.admin_ids.is_empty());

        let action = FriendActionResultDto {
            ok: true,
            user_id: "u-9".into(),
            action: "send_request".into(),
            already_friends: false,
            auto_accepted: true,
            message: "auto".into(),
        };
        let encoded = serde_json::to_string(&action).unwrap();
        let decoded: FriendActionResultDto = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, action);
    }

    #[test]
    fn compose_hangul_nfc_composes_nfd_jamo_and_lv_plus_t_without_corrupting_other_scripts() {
        // "한글" decomposed as NFD Jamo: ᄒ (1112) ᅡ (1161) ᆫ (11AB) ᄀ (1100) ᅳ (1173) ᆯ (11AF)
        let nfd_hangul = "\u{1112}\u{1161}\u{11AB}\u{1100}\u{1173}\u{11AF}";
        assert_eq!(compose_hangul_nfc(nfd_hangul), "한글");

        // Precomposed LV ("하" = U+D558) + T (U+11AB) -> "한" (U+D55C)
        assert_eq!(compose_hangul_nfc("\u{D558}\u{11AB}"), "한");

        // Already-composed LVT ("한") + stray T (U+11AB) must NOT double-compose
        assert_eq!(compose_hangul_nfc("한\u{11AB}"), "한\u{11AB}");

        // Boundary syllables: first "가" (L=0,V=0) and last "힣" (L=18,V=20,T=27)
        assert_eq!(compose_hangul_nfc("\u{1100}\u{1161}"), "가");
        assert_eq!(compose_hangul_nfc("\u{1112}\u{1175}\u{11C2}"), "힣");

        // Mixed Korean NFD + Vietnamese + ASCII + emoji preserved cleanly
        let mixed = "Hello \u{1112}\u{116C}\u{110B}\u{1174}\u{1109}\u{1175}\u{11AF} Nguyễn 🎉";
        assert_eq!(compose_hangul_nfc(mixed), "Hello 회의실 Nguyễn 🎉");
    }

    #[test]
    fn strip_vi_accents_handles_nfc_and_nfd_vietnamese_and_preserves_nfc_hangul() {
        // NFC Vietnamese
        assert_eq!(strip_vi_accents("Nguyễn Văn Minh — Đà Nẵng"), "nguyen van minh — da nang");
        // NFD Vietnamese (e + combining circumflex + combining dot below; a + combining acute)
        let nfd_vi = "Nguy\u{0065}\u{0302}\u{0303}n Va\u{0306}n \u{0110}u\u{031B}\u{0301}c";
        assert_eq!(strip_vi_accents(nfd_vi), "nguyen van duc");
        // NFD Hangul normalizes to identical NFC Hangul without decomposing syllables
        let nfd_ko = "\u{1100}\u{1175}\u{11B7}\u{1106}\u{1175}\u{11AB}\u{1109}\u{116E}";
        assert_eq!(strip_vi_accents(nfd_ko), "김민수");
        assert_eq!(strip_vi_accents("김민수"), "김민수");
        assert_ne!(strip_vi_accents("안"), strip_vi_accents("아"));
        // Mixed Korean NFD + Vietnamese NFD + English uppercase
        let mixed = "Team \u{1112}\u{116C}\u{110B}\u{1174} Ba\u{0301}o Ca\u{0301}o";
        assert_eq!(strip_vi_accents(mixed), "team 회의 bao cao");
    }

    #[test]
    fn contains_cjk_or_hangul_detects_hangul_cjk_and_kana_ranges() {
        assert!(contains_cjk_or_hangul("김부장님")); // Hangul Syllables
        assert!(contains_cjk_or_hangul("\u{1112}\u{1161}\u{11AB}")); // Hangul Jamo
        assert!(contains_cjk_or_hangul("ㅎㅇ")); // Compatibility Jamo
        assert!(contains_cjk_or_hangul("会議室")); // CJK Unified
        assert!(contains_cjk_or_hangul("こんにちは")); // Hiragana
        assert!(contains_cjk_or_hangul("カタカナ")); // Katakana
        assert!(contains_cjk_or_hangul("abc 한글 123"));

        assert!(!contains_cjk_or_hangul(""));
        assert!(!contains_cjk_or_hangul("Nguyễn Văn Minh 12345"));
        assert!(!contains_cjk_or_hangul("hello world!?"));
    }

    #[test]
    fn korean_choseong_extraction_and_query_detection() {
        assert_eq!(extract_korean_choseong("김민수"), "ㄱㅁㅅ");
        assert_eq!(extract_korean_choseong("회의실"), "ㅎㅇㅅ");
        assert_eq!(extract_korean_choseong("회의실에서 만나요"), "ㅎㅇㅅㅇㅅ ㅁㄴㅇ");
        assert_eq!(extract_korean_choseong("김민수 부장님"), "ㄱㅁㅅ ㅂㅈㄴ");
        // NFD Hangul input also composes and extracts identical choseong
        let nfd_hoeuishil = "\u{1112}\u{116C}\u{110B}\u{1174}\u{1109}\u{1175}\u{11AF}";
        assert_eq!(extract_korean_choseong(nfd_hoeuishil), "ㅎㅇㅅ");
        // Verify all 19 choseong initials in order
        assert_eq!(
            extract_korean_choseong("가까나다따라마바빠사싸아자짜차카타파하"),
            "ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ"
        );
        // Non-Hangul text passes through untouched
        assert_eq!(extract_korean_choseong("Nguyễn 123"), "Nguyễn 123");

        assert!(is_korean_choseong_query("ㄱㅁㅅ"));
        assert!(is_korean_choseong_query("ㅎㅇ"));
        assert!(is_korean_choseong_query("ㄱㅁㅅ ㅂㅈ"));
        assert!(is_korean_choseong_query("ㄱㄲㄴㄷㄸㄹㅁㅂㅃㅅㅆㅇㅈㅉㅊㅋㅌㅍㅎ"));

        assert!(!is_korean_choseong_query(""));
        assert!(!is_korean_choseong_query("   \t\n"));
        assert!(!is_korean_choseong_query("김민수"));
        assert!(!is_korean_choseong_query("ㄱㅁ1"));
        assert!(!is_korean_choseong_query("ㅏㅓㅗㅜ")); // vowels, not choseong
        assert!(!is_korean_choseong_query("ㄳ")); // compound jongseong, not choseong
        assert!(!is_korean_choseong_query("hello"));
    }
}
