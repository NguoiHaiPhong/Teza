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

/// Strip Vietnamese diacritics and lowercase for accent-insensitive search (`Ctrl+K`).
pub fn strip_vi_accents(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        let mapped = match ch {
            'à' | 'á' | 'ạ' | 'ả' | 'ã' | 'â' | 'ầ' | 'ấ' | 'ậ' | 'ẩ' | 'ẫ' | 'ă' | 'ằ' | 'ắ'
            | 'ặ' | 'ẳ' | 'ẵ' | 'À' | 'Á' | 'Ạ' | 'Ả' | 'Ã' | 'Â' | 'Ầ' | 'Ấ' | 'Ậ' | 'Ẩ'
            | 'Ẫ' | 'Ă' | 'Ằ' | 'Ắ' | 'Ặ' | 'Ẳ' | 'Ẵ' => 'a',
            'è' | 'é' | 'ẹ' | 'ẻ' | 'ẽ' | 'ê' | 'ề' | 'ế' | 'ệ' | 'ể' | 'ễ' | 'È' | 'É' | 'Ẹ'
            | 'Ẻ' | 'Ẽ' | 'Ê' | 'Ề' | 'Ế' | 'Ệ' | 'Ể' | 'Ễ' => 'e',
            'ì' | 'í' | 'ị' | 'ỉ' | 'ĩ' | 'Ì' | 'Í' | 'Ị' | 'Ỉ' | 'Ĩ' => 'i',
            'ò' | 'ó' | 'ọ' | 'ỏ' | 'õ' | 'ô' | 'ồ' | 'ố' | 'ộ' | 'ổ' | 'ỗ' | 'ơ' | 'ờ' | 'ớ'
            | 'ợ' | 'ở' | 'ỡ' | 'Ò' | 'Ó' | 'Ọ' | 'Ỏ' | 'Õ' | 'Ô' | 'Ồ' | 'Ố' | 'Ộ' | 'Ổ'
            | 'Ỗ' | 'Ơ' | 'Ờ' | 'Ớ' | 'Ợ' | 'Ở' | 'Ỡ' => 'o',
            'ù' | 'ú' | 'ụ' | 'ủ' | 'ũ' | 'ư' | 'ừ' | 'ứ' | 'ự' | 'ử' | 'ữ' | 'Ù' | 'Ú' | 'Ụ'
            | 'Ủ' | 'Ũ' | 'Ư' | 'Ừ' | 'Ứ' | 'Ự' | 'Ử' | 'Ữ' => 'u',
            'ỳ' | 'ý' | 'ỵ' | 'ỷ' | 'ỹ' | 'Ỳ' | 'Ý' | 'Ỵ' | 'Ỷ' | 'Ỹ' => 'y',
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
}
