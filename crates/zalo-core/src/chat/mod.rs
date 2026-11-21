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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadMemberDto {
    pub id: String,
    pub name: String,
    pub avatar: String,
    pub role: String,
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
    pub zalo_name: String,
    pub avatar: String,
    pub cover: String,
    pub status: String,
    pub gender: Option<u8>,
    pub sdob: String,
    pub phone_number: String,
    pub is_friend: Option<bool>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
