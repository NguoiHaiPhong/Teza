//! Multi-provider message translation (DeepL, Gemini, Azure Translator) with DPAPI-sealed API keys.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::relay::RelayConfig;
use crate::{data_dir, dpapi};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TranslateProvider {
    #[default]
    Deepl,
    Gemini,
    Azure,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TranslateDetectMode {
    #[default]
    ApiCache,
    LocalSmartSkip,
}

pub fn provider_name(p: TranslateProvider) -> &'static str {
    match p {
        TranslateProvider::Deepl => "deepl",
        TranslateProvider::Gemini => "gemini",
        TranslateProvider::Azure => "azure",
    }
}

#[derive(Debug, Clone)]
pub struct TranslateConfig {
    pub provider: TranslateProvider,
    pub detect_mode: TranslateDetectMode,
    pub target_lang: String,
    pub deepl_key: String,
    pub gemini_key: String,
    pub gemini_model: String,
    pub azure_key: String,
    pub azure_region: String,
}

impl Default for TranslateConfig {
    fn default() -> Self {
        Self {
            provider: TranslateProvider::Deepl,
            detect_mode: TranslateDetectMode::ApiCache,
            target_lang: "VI".into(),
            deepl_key: String::new(),
            gemini_key: String::new(),
            gemini_model: "gemini-2.0-flash".into(),
            azure_key: String::new(),
            azure_region: "eastasia".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SealedTranslateFile {
    #[serde(default)]
    provider: TranslateProvider,
    #[serde(default)]
    detect_mode: TranslateDetectMode,
    #[serde(default = "default_target_lang")]
    target_lang: String,
    #[serde(default)]
    sealed_deepl_key: String,
    #[serde(default)]
    sealed_gemini_key: String,
    #[serde(default = "default_gemini_model")]
    gemini_model: String,
    #[serde(default)]
    sealed_azure_key: String,
    #[serde(default = "default_azure_region")]
    azure_region: String,
}

fn default_target_lang() -> String {
    "VI".into()
}

fn default_gemini_model() -> String {
    "gemini-2.0-flash".into()
}

fn default_azure_region() -> String {
    "eastasia".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct TranslateConfigSummary {
    pub provider: TranslateProvider,
    pub detect_mode: TranslateDetectMode,
    pub target_lang: String,
    pub gemini_model: String,
    pub azure_region: String,
    pub has_deepl_key: bool,
    pub has_gemini_key: bool,
    pub has_azure_key: bool,
    pub deepl_preview: String,
    pub gemini_preview: String,
    pub azure_preview: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TranslateConfigUpdate {
    pub provider: TranslateProvider,
    pub target_lang: String,
    pub detect_mode: Option<TranslateDetectMode>,
    pub deepl_key: Option<String>,
    pub gemini_key: Option<String>,
    pub gemini_model: Option<String>,
    pub azure_key: Option<String>,
    pub azure_region: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslateInputItem {
    pub msg_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationResult {
    pub msg_id: String,
    pub translated_text: String,
    pub detected_source_lang: Option<String>,
    #[serde(default)]
    pub skipped: bool,
}

fn mask_key(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = t.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len());
    }
    let prefix: String = chars[..4].iter().collect();
    let suffix: String = chars[chars.len() - 4..].iter().collect();
    format!("{prefix}...{suffix}")
}

fn sanitize_gemini_model_opt(raw: &str) -> Option<String> {
    let clean: String = raw
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(*c, '-' | '.' | '_'))
        .collect();
    (!clean.is_empty()).then_some(clean)
}

fn sanitize_gemini_model(raw: &str) -> String {
    sanitize_gemini_model_opt(raw).unwrap_or_else(default_gemini_model)
}

impl TranslateConfig {
    pub fn load_from_file(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(file) = serde_json::from_str::<SealedTranslateFile>(&raw) else {
            return Self::default();
        };
        Self {
            provider: file.provider,
            detect_mode: file.detect_mode,
            target_lang: if file.target_lang.trim().is_empty() {
                default_target_lang()
            } else {
                file.target_lang.trim().to_uppercase()
            },
            deepl_key: dpapi::unprotect_secret(&file.sealed_deepl_key).unwrap_or_default(),
            gemini_key: dpapi::unprotect_secret(&file.sealed_gemini_key).unwrap_or_default(),
            gemini_model: sanitize_gemini_model(&file.gemini_model),
            azure_key: dpapi::unprotect_secret(&file.sealed_azure_key).unwrap_or_default(),
            azure_region: file.azure_region.trim().to_string(),
        }
    }

    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        let payload = SealedTranslateFile {
            provider: self.provider,
            detect_mode: self.detect_mode,
            target_lang: self.target_lang.trim().to_uppercase(),
            sealed_deepl_key: dpapi::protect_secret(self.deepl_key.trim())?,
            sealed_gemini_key: dpapi::protect_secret(self.gemini_key.trim())?,
            gemini_model: sanitize_gemini_model(&self.gemini_model),
            sealed_azure_key: dpapi::protect_secret(self.azure_key.trim())?,
            azure_region: self.azure_region.trim().to_string(),
        };
        let json = serde_json::to_string_pretty(&payload)?;
        data_dir::write_private_atomic(path, json.as_bytes())
    }

    pub fn summary(&self) -> TranslateConfigSummary {
        TranslateConfigSummary {
            provider: self.provider,
            detect_mode: self.detect_mode,
            target_lang: self.target_lang.clone(),
            gemini_model: self.gemini_model.clone(),
            azure_region: self.azure_region.clone(),
            has_deepl_key: !self.deepl_key.trim().is_empty(),
            has_gemini_key: !self.gemini_key.trim().is_empty(),
            has_azure_key: !self.azure_key.trim().is_empty(),
            deepl_preview: mask_key(&self.deepl_key),
            gemini_preview: mask_key(&self.gemini_key),
            azure_preview: mask_key(&self.azure_key),
        }
    }

    pub fn apply_update(&mut self, upd: TranslateConfigUpdate) {
        self.provider = upd.provider;
        if let Some(m) = upd.detect_mode {
            self.detect_mode = m;
        }
        if !upd.target_lang.trim().is_empty() {
            self.target_lang = upd.target_lang.trim().to_uppercase();
        }
        if let Some(k) = upd.deepl_key {
            let trimmed = k.trim();
            if !trimmed.is_empty() {
                self.deepl_key = trimmed.to_string();
            }
        }
        if let Some(k) = upd.gemini_key {
            let trimmed = k.trim();
            if !trimmed.is_empty() {
                self.gemini_key = trimmed.to_string();
            }
        }
        if let Some(m) = upd.gemini_model {
            if let Some(clean) = sanitize_gemini_model_opt(&m) {
                self.gemini_model = clean;
            }
        }
        if let Some(k) = upd.azure_key {
            let trimmed = k.trim();
            if !trimmed.is_empty() {
                self.azure_key = trimmed.to_string();
            }
        }
        if let Some(r) = upd.azure_region {
            self.azure_region = r.trim().to_string();
        }
    }
}

pub fn deepl_endpoint_for_key(key: &str) -> &'static str {
    if key.trim().ends_with(":fx") {
        "https://api-free.deepl.com/v2/translate"
    } else {
        "https://api.deepl.com/v2/translate"
    }
}

fn normalize_azure_lang(lang: &str) -> String {
    match lang.trim().to_ascii_uppercase().as_str() {
        "EN-US" | "EN-GB" => "en".into(),
        "ZH" | "ZH-HANS" => "zh-Hans".into(),
        "ZH-HANT" => "zh-Hant".into(),
        other => other.to_ascii_lowercase(),
    }
}

fn target_language_label(code: &str) -> &str {
    match code.trim().to_ascii_uppercase().as_str() {
        "VI" => "Vietnamese",
        "EN" | "EN-US" | "EN-GB" => "English",
        "JA" => "Japanese",
        "KO" => "Korean",
        "ZH" | "ZH-HANS" => "Simplified Chinese",
        "ZH-HANT" => "Traditional Chinese",
        "FR" => "French",
        "DE" => "German",
        "ES" => "Spanish",
        "TH" => "Thai",
        "ID" => "Indonesian",
        "RU" => "Russian",
        _ => code,
    }
}

fn normalize_lang_base(code: &str) -> String {
    let upper = code.trim().to_ascii_uppercase();
    match upper.as_str() {
        "EN" | "EN-US" | "EN-GB" => "EN".into(),
        "ZH" | "ZH-HANS" | "ZH-CN" => "ZH".into(),
        "ZH-HANT" | "ZH-TW" | "ZH-HK" => "ZH-HANT".into(),
        _ => upper.split('-').next().unwrap_or(&upper).to_string(),
    }
}

pub fn normalize_lang_base_code(code: &str) -> String {
    normalize_lang_base(code)
}

pub fn is_same_lang_code(detected: Option<&str>, target: &str) -> bool {
    let Some(det) = detected.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    let d_base = normalize_lang_base(det);
    let t_base = normalize_lang_base(target);
    if d_base == t_base {
        return true;
    }
    let t_upper = target.trim().to_ascii_uppercase();
    t_upper == "ZH" && (d_base == "ZH" || d_base == "ZH-HANT")
}

pub fn is_effectively_same_text(orig: &str, translated: &str) -> bool {
    let norm = |s: &str| -> String {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let t_norm = norm(translated);
    t_norm.is_empty() || t_norm == norm(orig)
}

fn is_non_translatable_placeholder(text: &str) -> bool {
    let t = text.trim();
    t.is_empty()
        || matches!(
            t,
            "[Nhãn dán]"
                | "[Hình ảnh]"
                | "[Ảnh GIF]"
                | "[Tin nhắn thoại]"
                | "[Video]"
                | "[Tin nhắn đã thu hồi]"
                | "[Tệp đính kèm]"
                | "[Thiệp/Danh thiếp]"
                | "[Vị trí]"
                | "[Cuộc gọi]"
                | "[BUZZ!]"
                | "BUZZ!"
        )
}

fn compose_vi_combining_pair(base: char, mark: char) -> Option<char> {
    Some(match (base, mark) {
        ('a', '\u{0300}') => 'à',
        ('a', '\u{0301}') => 'á',
        ('a', '\u{0302}') => 'â',
        ('a', '\u{0303}') => 'ã',
        ('a', '\u{0306}') => 'ă',
        ('a', '\u{0309}') => 'ả',
        ('a', '\u{0323}') => 'ạ',
        ('A', '\u{0300}') => 'À',
        ('A', '\u{0301}') => 'Á',
        ('A', '\u{0302}') => 'Â',
        ('A', '\u{0303}') => 'Ã',
        ('A', '\u{0306}') => 'Ă',
        ('A', '\u{0309}') => 'Ả',
        ('A', '\u{0323}') => 'Ạ',
        ('ă', '\u{0300}') => 'ằ',
        ('ă', '\u{0301}') => 'ắ',
        ('ă', '\u{0303}') => 'ẵ',
        ('ă', '\u{0309}') => 'ẳ',
        ('ă', '\u{0323}') => 'ặ',
        ('Ă', '\u{0300}') => 'Ằ',
        ('Ă', '\u{0301}') => 'Ắ',
        ('Ă', '\u{0303}') => 'Ẵ',
        ('Ă', '\u{0309}') => 'Ẳ',
        ('Ă', '\u{0323}') => 'Ặ',
        ('â', '\u{0300}') => 'ầ',
        ('â', '\u{0301}') => 'ấ',
        ('â', '\u{0303}') => 'ẫ',
        ('â', '\u{0309}') => 'ẩ',
        ('â', '\u{0323}') => 'ậ',
        ('Â', '\u{0300}') => 'Ầ',
        ('Â', '\u{0301}') => 'Ấ',
        ('Â', '\u{0303}') => 'Ẫ',
        ('Â', '\u{0309}') => 'Ẩ',
        ('Â', '\u{0323}') => 'Ậ',
        ('e', '\u{0300}') => 'è',
        ('e', '\u{0301}') => 'é',
        ('e', '\u{0302}') => 'ê',
        ('e', '\u{0303}') => 'ẽ',
        ('e', '\u{0309}') => 'ẻ',
        ('e', '\u{0323}') => 'ẹ',
        ('E', '\u{0300}') => 'È',
        ('E', '\u{0301}') => 'É',
        ('E', '\u{0302}') => 'Ê',
        ('E', '\u{0303}') => 'Ẽ',
        ('E', '\u{0309}') => 'Ẻ',
        ('E', '\u{0323}') => 'Ẹ',
        ('ê', '\u{0300}') => 'ề',
        ('ê', '\u{0301}') => 'ế',
        ('ê', '\u{0303}') => 'ễ',
        ('ê', '\u{0309}') => 'ể',
        ('ê', '\u{0323}') => 'ệ',
        ('Ê', '\u{0300}') => 'Ề',
        ('Ê', '\u{0301}') => 'Ế',
        ('Ê', '\u{0303}') => 'Ễ',
        ('Ê', '\u{0309}') => 'Ể',
        ('Ê', '\u{0323}') => 'Ệ',
        ('i', '\u{0300}') => 'ì',
        ('i', '\u{0301}') => 'í',
        ('i', '\u{0303}') => 'ĩ',
        ('i', '\u{0309}') => 'ỉ',
        ('i', '\u{0323}') => 'ị',
        ('I', '\u{0300}') => 'Ì',
        ('I', '\u{0301}') => 'Í',
        ('I', '\u{0303}') => 'Ĩ',
        ('I', '\u{0309}') => 'Ỉ',
        ('I', '\u{0323}') => 'Ị',
        ('o', '\u{0300}') => 'ò',
        ('o', '\u{0301}') => 'ó',
        ('o', '\u{0302}') => 'ô',
        ('o', '\u{0303}') => 'õ',
        ('o', '\u{0309}') => 'ỏ',
        ('o', '\u{031B}') => 'ơ',
        ('o', '\u{0323}') => 'ọ',
        ('O', '\u{0300}') => 'Ò',
        ('O', '\u{0301}') => 'Ó',
        ('O', '\u{0302}') => 'Ô',
        ('O', '\u{0303}') => 'Õ',
        ('O', '\u{0309}') => 'Ỏ',
        ('O', '\u{031B}') => 'Ơ',
        ('O', '\u{0323}') => 'Ọ',
        ('ô', '\u{0300}') => 'ồ',
        ('ô', '\u{0301}') => 'ố',
        ('ô', '\u{0303}') => 'ỗ',
        ('ô', '\u{0309}') => 'ổ',
        ('ô', '\u{0323}') => 'ộ',
        ('Ô', '\u{0300}') => 'Ồ',
        ('Ô', '\u{0301}') => 'Ố',
        ('Ô', '\u{0303}') => 'Ỗ',
        ('Ô', '\u{0309}') => 'Ổ',
        ('Ô', '\u{0323}') => 'Ộ',
        ('ơ', '\u{0300}') => 'ờ',
        ('ơ', '\u{0301}') => 'ớ',
        ('ơ', '\u{0303}') => 'ỡ',
        ('ơ', '\u{0309}') => 'ở',
        ('ơ', '\u{0323}') => 'ợ',
        ('Ơ', '\u{0300}') => 'Ờ',
        ('Ơ', '\u{0301}') => 'Ớ',
        ('Ơ', '\u{0303}') => 'Ỡ',
        ('Ơ', '\u{0309}') => 'Ở',
        ('Ơ', '\u{0323}') => 'Ợ',
        ('u', '\u{0300}') => 'ù',
        ('u', '\u{0301}') => 'ú',
        ('u', '\u{0303}') => 'ũ',
        ('u', '\u{0309}') => 'ủ',
        ('u', '\u{031B}') => 'ư',
        ('u', '\u{0323}') => 'ụ',
        ('U', '\u{0300}') => 'Ù',
        ('U', '\u{0301}') => 'Ú',
        ('U', '\u{0303}') => 'Ũ',
        ('U', '\u{0309}') => 'Ủ',
        ('U', '\u{031B}') => 'Ư',
        ('U', '\u{0323}') => 'Ụ',
        ('ư', '\u{0300}') => 'ừ',
        ('ư', '\u{0301}') => 'ứ',
        ('ư', '\u{0303}') => 'ữ',
        ('ư', '\u{0309}') => 'ử',
        ('ư', '\u{0323}') => 'ự',
        ('Ư', '\u{0300}') => 'Ừ',
        ('Ư', '\u{0301}') => 'Ứ',
        ('Ư', '\u{0303}') => 'Ữ',
        ('Ư', '\u{0309}') => 'Ử',
        ('Ư', '\u{0323}') => 'Ự',
        ('y', '\u{0300}') => 'ỳ',
        ('y', '\u{0301}') => 'ý',
        ('y', '\u{0303}') => 'ỹ',
        ('y', '\u{0309}') => 'ỷ',
        ('y', '\u{0323}') => 'ỵ',
        ('Y', '\u{0300}') => 'Ỳ',
        ('Y', '\u{0301}') => 'Ý',
        ('Y', '\u{0303}') => 'Ỹ',
        ('Y', '\u{0309}') => 'Ỷ',
        ('Y', '\u{0323}') => 'Ỵ',
        // Canonical NFD puts U+0323 (CCC 220) before U+0302/U+0306 (CCC 230)
        ('ạ', '\u{0302}') => 'ậ',
        ('Ạ', '\u{0302}') => 'Ậ',
        ('ạ', '\u{0306}') => 'ặ',
        ('Ạ', '\u{0306}') => 'Ặ',
        ('ẹ', '\u{0302}') => 'ệ',
        ('Ẹ', '\u{0302}') => 'Ệ',
        ('ọ', '\u{0302}') => 'ộ',
        ('Ọ', '\u{0302}') => 'Ộ',
        _ => return None,
    })
}

fn normalize_vi_nfc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if let Some(prev) = out.chars().next_back() {
            if let Some(composed) = compose_vi_combining_pair(prev, ch) {
                out.pop();
                out.push(composed);
                continue;
            }
        }
        out.push(ch);
    }
    out
}

fn strip_file_prefix_or_placeholder(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if is_non_translatable_placeholder(trimmed) {
        return None;
    }
    let prefix: String = trimmed.chars().take(9).collect();
    if prefix.to_lowercase() == "[tập tin]" {
        let (_, caption) = trimmed.split_once('\n')?;
        let cap = caption.trim();
        return (!cap.is_empty()).then_some(cap);
    }
    Some(trimmed)
}

pub fn is_static_non_translatable(text: &str) -> bool {
    if text.trim().is_empty() {
        return true;
    }
    let nfc = normalize_vi_nfc(text);
    strip_file_prefix_or_placeholder(&nfc).is_none()
}

fn is_title_case_token(word: &str) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_uppercase() {
        return false;
    }
    let mut rest_count = 0usize;
    for ch in chars {
        if !ch.is_lowercase() {
            return false;
        }
        rest_count += 1;
    }
    rest_count >= 1
}

fn is_mention_continuation_stopword(lower: &str) -> bool {
    matches!(
        lower,
        "please"
            | "dear"
            | "hello"
            | "hi"
            | "good"
            | "thanks"
            | "thank"
            | "sorry"
            | "could"
            | "would"
            | "should"
            | "will"
            | "we"
            | "you"
            | "they"
            | "this"
            | "that"
            | "the"
            | "there"
            | "here"
            | "today"
            | "tomorrow"
            | "yesterday"
            | "update"
            | "meeting"
            | "check"
            | "send"
            | "share"
            | "attend"
            | "confirm"
            | "verify"
            | "review"
            | "note"
            | "urgent"
            | "important"
            | "how"
            | "what"
            | "when"
            | "where"
            | "why"
            | "who"
            | "yes"
            | "no"
            | "ok"
            | "okay"
            | "sure"
            | "done"
            | "underfill"
            | "auto"
            | "pickup"
            | "iconic"
            | "still"
            | "making"
    )
}

fn is_accepted_mention_continuation(core: &str) -> bool {
    if core.is_empty() {
        return false;
    }
    if core.chars().count() == 1 {
        return core != "I" && core.chars().all(char::is_uppercase);
    }
    is_title_case_token(core) && !is_mention_continuation_stopword(&core.to_lowercase())
}

fn strip_non_linguistic_tokens(text: &str) -> String {
    const SYMBOLIC_EMOTICONS: &[&str] = &[
        ">:D<", "=))", ":))", ";))", ":-)", ":-(", ":-D", ":-P", ":'(", "B-)", "O:)", ":-*",
        ":)", ":(", ";)", ":D", ":P", ":-/", ":/", ":O", ":-O", "^^", "<3",
    ];
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut kept_tokens = Vec::with_capacity(tokens.len());
    let mut i = 0usize;
    while i < tokens.len() {
        let raw_tok = tokens[i];
        let trimmed = raw_tok.trim_matches(|c: char| {
            matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\'' | ',' | ';' | '.' | '!' | '?')
        });
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www.") {
            i += 1;
            continue;
        }
        if trimmed.starts_with('@') && trimmed.len() > 1 {
            let mention_core = trimmed[1..].to_lowercase();
            let raw_ends_clause = matches!(
                raw_tok.chars().next_back(),
                Some(',' | '.' | ':' | ';' | '!' | '?' | ')' | ']')
            );
            if !raw_ends_clause && mention_core != "all" && mention_core != "everyone" {
                let mut consumed = 0usize;
                while consumed < 3 && i + 1 < tokens.len() {
                    let raw_next = tokens[i + 1];
                    let core = raw_next.trim_end_matches(|c: char| {
                        matches!(c, ',' | '.' | ':' | ';' | '!' | '?' | ')' | ']')
                    });
                    if !is_accepted_mention_continuation(core) {
                        break;
                    }
                    i += 1;
                    consumed += 1;
                    if core.len() < raw_next.len() {
                        break;
                    }
                }
            }
            i += 1;
            continue;
        }
        if let Some(at_pos) = trimmed.find('@') {
            if at_pos > 0 && trimmed[at_pos + 1..].contains('.') {
                i += 1;
                continue;
            }
        }
        let exact = raw_tok.trim();
        if exact.eq_ignore_ascii_case("xd")
            || exact == "T_T"
            || exact == "ToT"
            || exact == "B-)"
            || exact == "O:)"
            || SYMBOLIC_EMOTICONS.contains(&exact)
        {
            i += 1;
            continue;
        }
        let mut cleaned = raw_tok.to_string();
        for emo in SYMBOLIC_EMOTICONS {
            if cleaned.contains(emo) {
                cleaned = cleaned.replace(emo, " ");
            }
        }
        if !cleaned.trim().is_empty() {
            kept_tokens.push(cleaned);
        }
        i += 1;
    }
    kept_tokens.join(" ")
}

fn is_pure_model_code(cleaned: &str) -> bool {
    let mut total_tokens = 0usize;
    let mut digit_bearing_tokens = 0usize;
    let mut has_upper_and_digit = false;
    for tok in cleaned.split(|c: char| !c.is_alphanumeric()) {
        if tok.is_empty() {
            continue;
        }
        total_tokens += 1;
        if tok.chars().any(|c| c.is_ascii_lowercase()) {
            return false;
        }
        let has_upper = tok.chars().any(|c| c.is_ascii_uppercase());
        let has_digit = tok.chars().any(|c| c.is_ascii_digit());
        if has_digit {
            digit_bearing_tokens += 1;
        } else if tok.chars().count() >= 5 || is_en_stopword(&tok.to_ascii_lowercase()) {
            return false;
        }
        if has_upper && has_digit {
            has_upper_and_digit = true;
        }
    }
    total_tokens > 0 && has_upper_and_digit && digit_bearing_tokens * 2 >= total_tokens
}

fn is_uniquely_vietnamese_char(c: char) -> bool {
    matches!(
        c,
        'đ' | 'Đ'
            | 'ă'
            | 'Ă'
            | 'ơ'
            | 'Ơ'
            | 'ư'
            | 'Ư'
            | 'ả'
            | 'Ả'
            | 'ẻ'
            | 'Ẻ'
            | 'ỉ'
            | 'Ỉ'
            | 'ỏ'
            | 'Ỏ'
            | 'ủ'
            | 'Ủ'
            | 'ỷ'
            | 'Ỷ'
            | 'ạ'
            | 'Ạ'
            | 'ẹ'
            | 'Ẹ'
            | 'ị'
            | 'Ị'
            | 'ọ'
            | 'Ọ'
            | 'ụ'
            | 'Ụ'
            | 'ỵ'
            | 'Ỵ'
            | 'ấ'
            | 'Ấ'
            | 'ầ'
            | 'Ầ'
            | 'ẩ'
            | 'Ẩ'
            | 'ẫ'
            | 'Ẫ'
            | 'ậ'
            | 'Ậ'
            | 'ắ'
            | 'Ắ'
            | 'ằ'
            | 'Ằ'
            | 'ẳ'
            | 'Ẳ'
            | 'ẵ'
            | 'Ẵ'
            | 'ặ'
            | 'Ặ'
            | 'ế'
            | 'Ế'
            | 'ề'
            | 'Ề'
            | 'ể'
            | 'Ể'
            | 'ễ'
            | 'Ễ'
            | 'ệ'
            | 'Ệ'
            | 'ố'
            | 'Ố'
            | 'ồ'
            | 'Ồ'
            | 'ổ'
            | 'Ổ'
            | 'ỗ'
            | 'Ỗ'
            | 'ộ'
            | 'Ộ'
            | 'ớ'
            | 'Ớ'
            | 'ờ'
            | 'Ờ'
            | 'ở'
            | 'Ở'
            | 'ỡ'
            | 'Ỡ'
            | 'ợ'
            | 'Ợ'
            | 'ứ'
            | 'Ứ'
            | 'ừ'
            | 'Ừ'
            | 'ử'
            | 'Ử'
            | 'ữ'
            | 'Ữ'
            | 'ự'
            | 'Ự'
            | 'ỳ'
            | 'Ỳ'
            | 'ỹ'
            | 'Ỹ'
    )
}

fn is_en_stopword(w: &str) -> bool {
    matches!(
        w,
        "the"
            | "and"
            | "you"
            | "that"
            | "have"
            | "for"
            | "with"
            | "this"
            | "from"
            | "they"
            | "would"
            | "there"
            | "their"
            | "what"
            | "about"
            | "which"
            | "when"
            | "make"
            | "please"
            | "thanks"
            | "thank"
            | "hello"
            | "sorry"
            | "could"
            | "should"
            | "your"
            | "been"
            | "were"
            | "will"
            | "are"
            | "was"
            | "not"
            | "but"
            | "all"
            | "how"
            | "why"
            | "where"
            | "who"
            | "does"
            | "did"
            | "has"
            | "had"
            | "into"
            | "just"
            | "like"
            | "know"
            | "take"
            | "want"
            | "because"
            | "before"
            | "after"
            | "between"
            | "through"
            | "without"
            | "during"
            | "under"
            | "again"
            | "here"
            | "very"
            | "really"
            | "good"
            | "morning"
            | "afternoon"
            | "evening"
            | "night"
            | "today"
            | "tomorrow"
            | "yesterday"
            | "meeting"
            | "schedule"
            | "update"
            | "issue"
            | "problem"
            | "working"
            | "need"
            | "help"
            | "send"
            | "sent"
            | "check"
            | "email"
            | "message"
            | "server"
            | "returned"
            | "error"
            | "session"
            | "expired"
            | "account"
            | "suspended"
            | "share"
            | "plan"
            | "moving"
            | "attend"
            | "review"
            | "list"
            | "verify"
            | "confirm"
            | "report"
            | "result"
            | "results"
            | "production"
            | "customer"
            | "finish"
            | "finished"
            | "making"
            | "still"
            | "job"
            | "congratulation"
            | "congratulations"
            | "together"
            | "dear"
            | "sir"
            | "these"
            | "those"
            | "already"
            | "status"
            | "progress"
            | "quality"
            | "feedback"
            | "request"
            | "support"
            | "reply"
            | "team"
            | "regards"
            | "noted"
            | "pls"
            | "everyone"
            | "i'm"
            | "i'll"
            | "i've"
            | "i'd"
            | "we're"
            | "we'll"
            | "you're"
            | "you'll"
            | "don't"
            | "doesn't"
            | "didn't"
            | "can't"
            | "won't"
            | "isn't"
            | "aren't"
            | "hasn't"
            | "haven't"
            | "it's"
            | "that's"
            | "let's"
    )
}

fn is_unambiguous_vi_word(w: &str) -> bool {
    matches!(
        w,
        "không"
            | "được"
            | "chưa"
            | "nhưng"
            | "người"
            | "chúng"
            | "mình"
            | "nhiều"
            | "giúp"
            | "cảm"
            | "chào"
            | "vậy"
            | "nhé"
            | "rồi"
            | "đang"
            | "với"
            | "của"
            | "từ"
            | "trong"
            | "ngoài"
            | "trên"
            | "dưới"
            | "hoặc"
            | "lắm"
            | "quá"
            | "rất"
            | "một"
            | "hôm"
            | "ngày"
            | "giờ"
            | "phút"
            | "tuần"
            | "tháng"
            | "bây"
            | "lúc"
            | "sáng"
            | "trưa"
            | "chiều"
            | "tối"
            | "ăn"
            | "uống"
            | "ngủ"
            | "làm"
            | "đi"
            | "về"
            | "đến"
            | "tới"
            | "gửi"
            | "gọi"
            | "nhắn"
            | "nói"
            | "hỏi"
            | "biết"
            | "hiểu"
            | "thấy"
            | "cần"
            | "muốn"
            | "thích"
            | "phải"
            | "đúng"
            | "tốt"
            | "đẹp"
            | "mới"
            | "cũ"
            | "đâu"
            | "thế"
            | "gì"
            | "nào"
            | "mọi"
            | "việc"
            | "tiền"
            | "hàng"
            | "nhóm"
            | "cuộc"
            | "họp"
            | "lỗi"
            | "sửa"
            | "nhận"
            | "chuyển"
            | "khoản"
            | "vâng"
            | "dạ"
            | "ơi"
            | "luôn"
            | "sếp"
            | "lịch"
            | "hình"
            | "ảnh"
            | "bảo"
            | "quản"
            | "là"
            | "sẽ"
            | "như"
            | "này"
            | "chút"
            | "thôi"
            | "ạ"
            | "bên"
            | "vẫn"
            | "để"
            | "có"
            | "vào"
            | "nhờ"
            | "đây"
            | "đó"
            | "chị"
            | "bạn"
            | "tôi"
            | "đã"
            | "thì"
            | "mà"
            | "ít"
            | "bốn"
            | "năm"
            | "sáu"
            | "bảy"
            | "tám"
            | "chín"
            | "mười"
            | "đêm"
            | "chậm"
            | "nhà"
            | "máy"
            | "điện"
            | "thoại"
            | "báo"
            | "cáo"
            | "trạng"
            | "nguyên"
            | "nhân"
            | "đối"
            | "sách"
            | "tình"
            | "khong"
            | "duoc"
            | "chua"
            | "nhung"
            | "nguoi"
            | "chung"
            | "minh"
            | "nhieu"
            | "giup"
            | "kiem"
            | "chuyen"
            | "khoan"
            | "chao"
            | "hom"
            | "ngay"
            | "tuan"
            | "thang"
            | "phut"
            | "chieu"
            | "viec"
            | "tien"
            | "nhom"
            | "biet"
            | "hieu"
            | "thay"
            | "muon"
            | "thich"
            | "phai"
            | "dung"
            | "vang"
            | "nhe"
            | "xong"
            | "gui"
            | "nhan"
            | "sua"
            | "loi"
            | "luon"
            | "truoc"
            | "duoi"
            | "ngoai"
            | "voi"
            | "cua"
            | "vay"
            | "roi"
            | "dang"
            | "giay"
            | "dien"
            | "thoai"
    )
}

struct WordMeta {
    lower: String,
    is_title_case: bool,
    is_all_caps: bool,
    is_single_upper_non_i: bool,
    has_uniquely_vi: bool,
    has_accented_latin: bool,
}

fn extract_word_metas(cleaned: &str) -> Vec<WordMeta> {
    let chars: Vec<char> = cleaned.chars().collect();
    let mut words = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_alphabetic() {
            i += 1;
            continue;
        }
        let mut raw_alpha = Vec::new();
        let mut lower = String::new();
        let mut has_apostrophe = false;
        while i < chars.len() {
            let c = chars[i];
            if c.is_alphabetic() {
                raw_alpha.push(c);
                for lc in c.to_lowercase() {
                    lower.push(lc);
                }
                i += 1;
            } else if matches!(c, '\'' | '\u{2019}')
                && i + 1 < chars.len()
                && chars[i + 1].is_alphabetic()
            {
                has_apostrophe = true;
                lower.push('\'');
                i += 1;
            } else {
                break;
            }
        }
        let is_title_case = !has_apostrophe
            && raw_alpha.len() >= 2
            && raw_alpha[0].is_uppercase()
            && raw_alpha[1..].iter().all(|c| c.is_lowercase());
        let is_all_caps =
            !has_apostrophe && raw_alpha.len() >= 2 && raw_alpha.iter().all(|c| c.is_ascii_uppercase());
        let is_single_upper_non_i =
            !has_apostrophe && raw_alpha.len() == 1 && raw_alpha[0].is_ascii_uppercase() && raw_alpha[0] != 'I';
        let has_uniquely_vi = raw_alpha.iter().any(|&c| is_uniquely_vietnamese_char(c));
        let has_accented_latin = raw_alpha.iter().any(|&c| !c.is_ascii());
        words.push(WordMeta {
            lower,
            is_title_case,
            is_all_caps,
            is_single_upper_non_i,
            has_uniquely_vi,
            has_accented_latin,
        });
    }
    words
}

pub fn classify_skip_reason(text: &str, target_lang: &str) -> Option<&'static str> {
    let normalized = normalize_vi_nfc(text);
    let Some(effective_text) = strip_file_prefix_or_placeholder(&normalized) else {
        return Some("non_linguistic");
    };
    let cleaned = strip_non_linguistic_tokens(effective_text);

    let mut hangul = 0usize;
    let mut kana = 0usize;
    let mut han = 0usize;
    let mut cyrillic = 0usize;
    let mut thai = 0usize;
    let mut accented_latin = 0usize;
    let mut total_letters = 0usize;

    for c in cleaned.chars() {
        if !c.is_alphabetic() {
            continue;
        }
        total_letters += 1;
        match c {
            '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}' | '\u{3130}'..='\u{318F}' => {
                hangul += 1;
            }
            '\u{3040}'..='\u{309F}' | '\u{30A0}'..='\u{30FF}' => {
                kana += 1;
            }
            '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' => {
                han += 1;
            }
            '\u{0400}'..='\u{04FF}' => {
                cyrillic += 1;
            }
            '\u{0E00}'..='\u{0E7F}' => {
                thai += 1;
            }
            _ => {
                if !c.is_ascii() {
                    accented_latin += 1;
                }
            }
        }
    }

    if total_letters < 2 {
        return Some("non_linguistic");
    }
    if hangul == 0
        && kana == 0
        && han == 0
        && cyrillic == 0
        && thai == 0
        && accented_latin == 0
        && is_pure_model_code(&cleaned)
    {
        return Some("non_linguistic");
    }

    let words = extract_word_metas(&cleaned);
    let vi_strong_words = words
        .iter()
        .filter(|w| is_unambiguous_vi_word(&w.lower) || (w.has_uniquely_vi && !w.is_title_case))
        .count();
    let lowercase_uniquely_vi_words = words
        .iter()
        .filter(|w| w.has_uniquely_vi && !w.is_title_case)
        .count();
    let en_strong_words = words.iter().filter(|w| is_en_stopword(&w.lower)).count();
    let non_title_accented_latin = words
        .iter()
        .filter(|w| !w.is_title_case && w.has_accented_latin)
        .count();
    let natural_words = words
        .iter()
        .filter(|w| !w.is_all_caps && !w.is_single_upper_non_i)
        .count();

    let mut max_en_clause_strong = 0usize;
    let mut cur_en_run = 0usize;
    for w in &words {
        if is_unambiguous_vi_word(&w.lower) || (w.has_uniquely_vi && !w.is_title_case) {
            cur_en_run = 0;
        } else if is_en_stopword(&w.lower) {
            cur_en_run += 1;
            if cur_en_run > max_en_clause_strong {
                max_en_clause_strong = cur_en_run;
            }
        }
    }

    let target_upper = target_lang.trim().to_ascii_uppercase();
    let target_base = normalize_lang_base(&target_upper);

    match target_base.as_str() {
        "VI" => {
            if hangul == 0 && kana == 0 && han == 0 && cyrillic == 0 && thai == 0 {
                if max_en_clause_strong >= 4
                    || (en_strong_words >= 3 && en_strong_words >= vi_strong_words)
                    || (en_strong_words >= 2 && vi_strong_words == 0)
                {
                    return None;
                }
                if vi_strong_words >= 2
                    || (vi_strong_words >= 1 && words.len() <= 4 && en_strong_words == 0)
                    || (lowercase_uniquely_vi_words >= 1 && en_strong_words <= 1)
                {
                    return Some("same_language");
                }
            }
        }
        "KO" if hangul >= 1
            && kana == 0
            && cyrillic == 0
            && thai == 0
            && vi_strong_words == 0
            && max_en_clause_strong < 4
            && en_strong_words < 3
            && (hangul >= 3 || hangul * 2 >= total_letters) =>
        {
            return Some("same_language");
        }
        "JA" if kana >= 1
            && hangul == 0
            && cyrillic == 0
            && thai == 0
            && vi_strong_words == 0
            && max_en_clause_strong < 4
            && en_strong_words < 3
            && (kana >= 2 || (kana + han) * 2 >= total_letters) =>
        {
            return Some("same_language");
        }
        "ZH" if target_upper == "ZH"
            && han >= 2
            && kana == 0
            && hangul == 0
            && cyrillic == 0
            && thai == 0
            && vi_strong_words == 0
            && max_en_clause_strong < 4
            && en_strong_words < 3
            && (han >= 4 || han * 5 >= total_letters * 3) =>
        {
            return Some("same_language");
        }
        "RU" if cyrillic >= 2
            && hangul == 0
            && kana == 0
            && han == 0
            && thai == 0
            && vi_strong_words == 0
            && en_strong_words < 3
            && cyrillic * 5 >= total_letters * 3 =>
        {
            return Some("same_language");
        }
        "TH" if thai >= 2
            && hangul == 0
            && kana == 0
            && han == 0
            && cyrillic == 0
            && vi_strong_words == 0
            && en_strong_words < 3
            && thai * 5 >= total_letters * 3 =>
        {
            return Some("same_language");
        }
        "EN" if hangul == 0
            && kana == 0
            && han == 0
            && cyrillic == 0
            && thai == 0
            && vi_strong_words == 0
            && non_title_accented_latin == 0
            && ((en_strong_words >= 2 && en_strong_words * 3 >= natural_words.max(1))
                || (natural_words <= 3 && en_strong_words >= 1)) =>
        {
            return Some("same_language");
        }
        _ => {}
    }

    None
}

pub fn classify_skip_reason_for_mode(
    text: &str,
    target_lang: &str,
    mode: TranslateDetectMode,
) -> Option<&'static str> {
    match classify_skip_reason(text, target_lang) {
        Some("non_linguistic") => Some("non_linguistic"),
        Some("same_language") if mode == TranslateDetectMode::LocalSmartSkip => {
            Some("same_language")
        }
        _ => None,
    }
}

pub async fn translate_batch(
    cfg: &TranslateConfig,
    items: &[TranslateInputItem],
    target_lang_override: Option<&str>,
    provider_override: Option<TranslateProvider>,
) -> Result<Vec<TranslationResult>> {
    translate_batch_with_relay(cfg, &RelayConfig::default(), items, target_lang_override, provider_override).await
}

pub async fn translate_api_only_with_relay(
    cfg: &TranslateConfig,
    relay: &RelayConfig,
    items: &[TranslateInputItem],
    target_lang_override: Option<&str>,
    provider_override: Option<TranslateProvider>,
) -> Result<Vec<TranslationResult>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }

    let provider = provider_override.unwrap_or(cfg.provider);
    let target_lang = target_lang_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(cfg.target_lang.as_str())
        .to_ascii_uppercase();

    let client = relay
        .apply_to_client_builder(Client::builder().timeout(Duration::from_secs(20)))?
        .build()
        .context("Không khởi tạo được HTTP client dịch thuật")?;

    let mut results = Vec::with_capacity(items.len());
    for chunk in items.chunks(25) {
        let mut part = match provider {
            TranslateProvider::Deepl => translate_deepl(&client, &cfg.deepl_key, chunk, &target_lang).await?,
            TranslateProvider::Gemini => {
                translate_gemini(&client, &cfg.gemini_key, &cfg.gemini_model, chunk, &target_lang).await?
            }
            TranslateProvider::Azure => {
                translate_azure(&client, &cfg.azure_key, &cfg.azure_region, chunk, &target_lang).await?
            }
        };
        part.retain_mut(|r| {
            let Some(orig) = chunk.iter().find(|it| it.msg_id == r.msg_id) else {
                return false;
            };
            let same_lang = is_same_lang_code(r.detected_source_lang.as_deref(), &target_lang);
            if r.translated_text.trim().is_empty() && !same_lang {
                return false;
            }
            if same_lang || is_effectively_same_text(&orig.text, &r.translated_text) {
                r.skipped = true;
                r.translated_text = orig.text.clone();
            }
            true
        });
        results.append(&mut part);
    }
    Ok(results)
}

pub async fn translate_batch_with_relay(
    cfg: &TranslateConfig,
    relay: &RelayConfig,
    items: &[TranslateInputItem],
    target_lang_override: Option<&str>,
    provider_override: Option<TranslateProvider>,
) -> Result<Vec<TranslationResult>> {
    let target_lang = target_lang_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(cfg.target_lang.as_str())
        .to_ascii_uppercase();
    let target_base = normalize_lang_base(&target_lang);

    let mut results = Vec::with_capacity(items.len());
    let mut to_translate: Vec<TranslateInputItem> = Vec::new();
    for it in items {
        if let Some(reason) = classify_skip_reason_for_mode(&it.text, &target_lang, cfg.detect_mode) {
            results.push(TranslationResult {
                msg_id: it.msg_id.clone(),
                translated_text: it.text.clone(),
                detected_source_lang: (reason == "same_language").then(|| target_base.clone()),
                skipped: true,
            });
        } else {
            to_translate.push(it.clone());
        }
    }

    if !to_translate.is_empty() {
        let mut api_res = translate_api_only_with_relay(
            cfg,
            relay,
            &to_translate,
            Some(&target_lang),
            provider_override,
        )
        .await?;
        results.append(&mut api_res);
    }
    Ok(results)
}

async fn translate_deepl(
    client: &Client,
    api_key: &str,
    items: &[TranslateInputItem],
    target_lang: &str,
) -> Result<Vec<TranslationResult>> {
    let key = api_key.trim();
    if key.is_empty() {
        bail!("Chưa cấu hình DeepL API key trong Cài đặt dịch thuật");
    }
    let endpoint = deepl_endpoint_for_key(key);
    let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
    let resp = client
        .post(endpoint)
        .header("Authorization", format!("DeepL-Auth-Key {key}"))
        .json(&json!({
            "text": texts,
            "target_lang": target_lang,
        }))
        .send()
        .await
        .context("Không kết nối được DeepL API")?;

    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let detail = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Lỗi không xác định");
        bail!("DeepL lỗi HTTP {}: {detail}", status.as_u16());
    }

    let arr = body
        .get("translations")
        .and_then(Value::as_array)
        .context("Phản hồi DeepL thiếu translations")?;
    Ok(items
        .iter()
        .zip(arr.iter())
        .map(|(item, tr)| TranslationResult {
            msg_id: item.msg_id.clone(),
            translated_text: tr
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            detected_source_lang: tr
                .get("detected_source_language")
                .and_then(Value::as_str)
                .map(str::to_string),
            skipped: false,
        })
        .collect())
}

fn json_id_matches(v: &Value, expected: &str) -> bool {
    let Some(id_val) = v.get("id") else {
        return false;
    };
    if let Some(s) = id_val.as_str() {
        return s.trim() == expected;
    }
    if let Some(n) = id_val.as_i64() {
        return n.to_string() == expected;
    }
    if let Some(n) = id_val.as_u64() {
        return n.to_string() == expected;
    }
    false
}

fn has_nonempty_json_id(v: &Value) -> bool {
    match v.get("id") {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(_) => true,
    }
}

fn parse_gemini_results(
    raw_text: &str,
    items: &[TranslateInputItem],
) -> Result<Vec<TranslationResult>> {
    let parsed: Vec<Value> =
        serde_json::from_str(raw_text).context("Không parse được JSON phản hồi từ Gemini")?;
    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let found = parsed
            .iter()
            .find(|v| json_id_matches(v, &item.msg_id))
            .or_else(|| {
                if parsed.len() == items.len() {
                    parsed.get(idx).filter(|v| !has_nonempty_json_id(v))
                } else {
                    None
                }
            });
        if let Some(tr) = found {
            out.push(TranslationResult {
                msg_id: item.msg_id.clone(),
                translated_text: tr
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                detected_source_lang: tr
                    .get("src")
                    .and_then(Value::as_str)
                    .map(|s| s.to_ascii_uppercase()),
                skipped: false,
            });
        }
    }
    Ok(out)
}

async fn translate_gemini(
    client: &Client,
    api_key: &str,
    model: &str,
    items: &[TranslateInputItem],
    target_lang: &str,
) -> Result<Vec<TranslationResult>> {
    let key = api_key.trim();
    if key.is_empty() {
        bail!("Chưa cấu hình Gemini API key trong Cài đặt dịch thuật");
    }
    let model_name = sanitize_gemini_model(model);
    let input_json: Vec<Value> = items
        .iter()
        .map(|it| json!({ "id": it.msg_id, "text": it.text }))
        .collect();
    let lang_name = target_language_label(target_lang);
    let target_base = normalize_lang_base(target_lang);
    let prompt = format!(
        "Translate each object's `text` field naturally into {lang_name} ({target_lang}). If a message's text is already written in {lang_name} ({target_lang}), return its original `text` UNCHANGED (do not paraphrase or rewrite) and set `src` to \"{target_base}\". Preserve emojis, line breaks, and @mentions. Return a JSON array of objects with exact fields: `id` (matching input `id`), `text` (translated string), and `src` (2-letter detected source language code).\nInput:\n{}",
        serde_json::to_string(&input_json)?
    );

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model_name}:generateContent"
    );
    let resp = client
        .post(&url)
        .header("x-goog-api-key", key)
        .json(&json!({
            "contents": [{ "parts": [{ "text": prompt }] }],
            "generationConfig": {
                "responseMimeType": "application/json",
                "temperature": 0.1
            }
        }))
        .send()
        .await
        .context("Không kết nối được Gemini API")?;

    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("Lỗi Gemini API");
        bail!("Gemini lỗi HTTP {}: {msg}", status.as_u16());
    }

    let raw_text = body
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(Value::as_str)
        .context("Gemini không trả về nội dung dịch")?;
    parse_gemini_results(raw_text, items)
}

async fn translate_azure(
    client: &Client,
    api_key: &str,
    region: &str,
    items: &[TranslateInputItem],
    target_lang: &str,
) -> Result<Vec<TranslationResult>> {
    let key = api_key.trim();
    if key.is_empty() {
        bail!("Chưa cấu hình Azure Translator key trong Cài đặt dịch thuật");
    }
    let to_lang = normalize_azure_lang(target_lang);
    let url = format!(
        "https://api.cognitive.microsofttranslator.com/translate?api-version=3.0&to={to_lang}"
    );
    let payload: Vec<Value> = items.iter().map(|it| json!({ "Text": it.text })).collect();

    let mut req = client
        .post(&url)
        .header("Ocp-Apim-Subscription-Key", key)
        .json(&payload);
    if !region.trim().is_empty() {
        req = req.header("Ocp-Apim-Subscription-Region", region.trim());
    }

    let resp = req
        .send()
        .await
        .context("Không kết nối được Azure Translator API")?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("Lỗi Azure Translator");
        bail!("Azure lỗi HTTP {}: {msg}", status.as_u16());
    }

    let arr = body.as_array().context("Phản hồi Azure không hợp lệ")?;
    Ok(items
        .iter()
        .zip(arr.iter())
        .map(|(item, entry)| {
            let tr = entry.pointer("/translations/0");
            TranslationResult {
                msg_id: item.msg_id.clone(),
                translated_text: tr
                    .and_then(|v| v.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                detected_source_lang: entry
                    .pointer("/detectedLanguage/language")
                    .and_then(Value::as_str)
                    .map(|s| s.to_ascii_uppercase()),
                skipped: false,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepl_endpoint_switches_between_free_and_pro() {
        assert_eq!(
            deepl_endpoint_for_key("abc-123:fx"),
            "https://api-free.deepl.com/v2/translate"
        );
        assert_eq!(
            deepl_endpoint_for_key("abc-123-pro"),
            "https://api.deepl.com/v2/translate"
        );
    }

    #[test]
    fn dpapi_seals_and_unseals_translation_keys() {
        let dir = std::env::temp_dir().join(format!("litechat-tr-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("translate.json");

        assert_eq!(TranslateConfig::default().detect_mode, TranslateDetectMode::ApiCache);
        let cfg = TranslateConfig {
            provider: TranslateProvider::Deepl,
            detect_mode: TranslateDetectMode::LocalSmartSkip,
            target_lang: "VI".into(),
            deepl_key: "secret-deepl-test:fx".into(),
            gemini_key: "secret-gemini-test".into(),
            gemini_model: "gemini-2.0-flash".into(),
            azure_key: "secret-azure-test".into(),
            azure_region: "southeastasia".into(),
        };
        cfg.save_to_file(&file).unwrap();

        let disk = std::fs::read_to_string(&file).unwrap();
        assert!(!disk.contains("secret-deepl-test:fx"));
        assert!(!disk.contains("secret-gemini-test"));
        assert!(!disk.contains("secret-azure-test"));

        let loaded = TranslateConfig::load_from_file(&file);
        assert_eq!(loaded.detect_mode, TranslateDetectMode::LocalSmartSkip);
        assert_eq!(loaded.deepl_key, "secret-deepl-test:fx");
        assert_eq!(loaded.gemini_key, "secret-gemini-test");
        assert_eq!(loaded.azure_key, "secret-azure-test");
        assert_eq!(loaded.azure_region, "southeastasia");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn classify_skip_reason_skips_same_language_and_non_linguistic() {
        // Already Vietnamese -> VI skipped
        assert_eq!(classify_skip_reason("Hôm nay họp lúc 2 giờ chiều nhé", "VI"), Some("same_language"));
        assert_eq!(classify_skip_reason("minh gui file cho ban roi nha", "VI"), Some("same_language"));
        assert_eq!(classify_skip_reason("ban cho toi xem", "VI"), None);
        assert_eq!(classify_skip_reason("khong duoc dau nha hom nay ban roi", "VI"), Some("same_language"));

        // Mixed VI + short tech loanword stays VI (skipped), long EN clause translates
        assert_eq!(classify_skip_reason("Mình deploy server mới rồi nhé", "VI"), Some("same_language"));
        assert_eq!(
            classify_skip_reason(
                "Nhờ bạn xem giúp: the server returned an error and please check your account",
                "VI"
            ),
            None
        );

        // English -> VI translates, English -> EN skips
        assert_eq!(classify_skip_reason("The meeting is scheduled for tomorrow morning", "VI"), None);
        assert_eq!(classify_skip_reason("The meeting is scheduled for tomorrow morning", "EN"), Some("same_language"));
        assert_eq!(classify_skip_reason("Thank you", "EN-US"), Some("same_language"));

        // French should NOT be treated as Vietnamese
        assert_eq!(classify_skip_reason("Très bien à bientôt mon ami", "VI"), None);

        // Non-Latin scripts
        assert_eq!(classify_skip_reason("안녕하세요 오늘 회의 있어요?", "KO"), Some("same_language"));
        assert_eq!(classify_skip_reason("안녕하세요 오늘 회의 있어요?", "VI"), None);
        assert_eq!(classify_skip_reason("明日の会議は10時です", "JA"), Some("same_language"));
        assert_eq!(classify_skip_reason("明天的会议十点开始", "ZH"), Some("same_language"));
        assert_eq!(classify_skip_reason("明天的会议十点开始", "ZH-HANT"), None);
        assert_eq!(classify_skip_reason("Привет, как дела сегодня?", "RU"), Some("same_language"));
        assert_eq!(classify_skip_reason("สวัสดีครับ วันนี้ว่างไหม", "TH"), Some("same_language"));

        // Non-linguistic messages
        assert_eq!(classify_skip_reason("https://teza.app/docs?v=1", "VI"), Some("non_linguistic"));
        assert_eq!(classify_skip_reason("👍 🎉 :)) =)) 12345", "VI"), Some("non_linguistic"));
        assert_eq!(classify_skip_reason("@Minh 100%", "VI"), Some("non_linguistic"));
        assert_eq!(classify_skip_reason("[BUZZ!]", "EN"), Some("non_linguistic"));
        assert_eq!(classify_skip_reason("[Tập tin] Todolist 2026.11.20.xlsx", "VI"), Some("non_linguistic"));
        assert_eq!(classify_skip_reason("[Tập tin] Todolist 2026.11.20.xlsx", "EN"), Some("non_linguistic"));
        assert_eq!(
            classify_skip_reason("[Tập tin] Todolist.xlsx\nGửi giúp mình nhé", "VI"),
            Some("same_language")
        );
        assert_eq!(classify_skip_reason("ICON25", "VI"), Some("non_linguistic"));
        assert_eq!(classify_skip_reason("ICON25 SF", "EN"), Some("non_linguistic"));

        // Multi-word @mentions & proper nouns must not pollute target language detection
        assert_eq!(classify_skip_reason("@Đào Nụ please share moving plan", "VI"), None);
        assert_eq!(classify_skip_reason("@Đào Nụ please share moving plan", "EN"), Some("same_language"));
        assert_eq!(classify_skip_reason("Dear Ms. Đào Nụ, please verify this", "VI"), None);
        assert_eq!(classify_skip_reason("Dear Ms. Đào Nụ, please verify this", "EN"), Some("same_language"));
        assert_eq!(
            classify_skip_reason("@Ahn Bm I'm still making, I'll send after finish , sir", "EN"),
            Some("same_language")
        );
        assert_eq!(
            classify_skip_reason("@Ahn Bm I'm still making, I'll send after finish , sir", "VI"),
            None
        );
        assert_eq!(
            classify_skip_reason("@Nguyễn Thanh R&D San together up-date pls", "EN"),
            Some("same_language")
        );
        assert_eq!(
            classify_skip_reason("@Vu Thi Anh RD 인원 확인하고 있나요?", "KO"),
            Some("same_language")
        );
        assert_eq!(
            classify_skip_reason("@Khangdx @Vu Thi Anh iconic NG는 무슨 불량인지?", "KO"),
            Some("same_language")
        );
        assert_eq!(
            classify_skip_reason("@Vu Thi Anh ??. PICKUP LOSS 중 7~10월 증가한 사유 분석해서 보고 할 것.", "KO"),
            Some("same_language")
        );
        // NFD Vietnamese message with multi-word @mentions
        assert_eq!(
            classify_skip_reason(
                "@HCường , @Doan Hoan , @Thang ND mn va\u{0300}o ho\u{0323}p vơ\u{0301}i mr Kang nhe\u{0301}.",
                "VI"
            ),
            Some("same_language")
        );
    }

    #[test]
    fn same_lang_code_and_same_text_helpers_normalize_variants() {
        assert!(is_same_lang_code(Some("en-US"), "EN"));
        assert!(is_same_lang_code(Some("vi"), "VI"));
        assert!(is_same_lang_code(Some("zh-Hans"), "ZH"));
        assert!(is_same_lang_code(Some("zh-TW"), "ZH-HANT"));
        assert!(!is_same_lang_code(Some("EN"), "VI"));
        assert!(!is_same_lang_code(None, "VI"));

        assert!(is_effectively_same_text("Xin   chào ", "xin chào"));
        assert!(is_effectively_same_text("Hello", ""));
        assert!(!is_effectively_same_text("Hello", "Xin chào"));
    }

    #[test]
    fn classify_skip_reason_for_mode_respects_api_cache_vs_local_smart_skip() {
        assert_eq!(
            classify_skip_reason_for_mode("Hôm nay họp lúc 2 giờ chiều nhé", "VI", TranslateDetectMode::ApiCache),
            None
        );
        assert_eq!(
            classify_skip_reason_for_mode(
                "Hôm nay họp lúc 2 giờ chiều nhé",
                "VI",
                TranslateDetectMode::LocalSmartSkip
            ),
            Some("same_language")
        );
        assert_eq!(
            classify_skip_reason_for_mode("[Hình ảnh]", "VI", TranslateDetectMode::ApiCache),
            Some("non_linguistic")
        );
        assert_eq!(
            classify_skip_reason_for_mode("https://teza.app/docs", "VI", TranslateDetectMode::ApiCache),
            Some("non_linguistic")
        );
    }

    #[tokio::test]
    async fn translate_batch_short_circuits_when_all_items_skipped_without_api_key() {
        let cfg_local = TranslateConfig {
            detect_mode: TranslateDetectMode::LocalSmartSkip,
            ..Default::default()
        };
        let items = vec![
            TranslateInputItem {
                msg_id: "m1".into(),
                text: "Chào bạn, hôm nay khỏe không?".into(),
            },
            TranslateInputItem {
                msg_id: "m2".into(),
                text: "https://example.com :))" .into(),
            },
        ];
        let res = translate_batch(&cfg_local, &items, Some("VI"), None).await.unwrap();
        assert_eq!(res.len(), 2);
        assert!(res[0].skipped);
        assert_eq!(res[0].translated_text, "Chào bạn, hôm nay khỏe không?");
        assert_eq!(res[0].detected_source_lang.as_deref(), Some("VI"));
        assert!(res[1].skipped);
        assert_eq!(res[1].detected_source_lang, None);

        // Default ApiCache mode still short-circuits non-linguistic items with zero API keys
        let cfg_api = TranslateConfig::default();
        let non_ling_only = vec![TranslateInputItem {
            msg_id: "m2".into(),
            text: "https://example.com :))" .into(),
        }];
        let res_non_ling = translate_batch(&cfg_api, &non_ling_only, Some("VI"), None)
            .await
            .unwrap();
        assert_eq!(res_non_ling.len(), 1);
        assert!(res_non_ling[0].skipped);
        assert_eq!(res_non_ling[0].detected_source_lang, None);

        // Default ApiCache mode sends natural text to API (failing cleanly when API key is missing)
        let err = translate_batch(&cfg_api, &items, Some("VI"), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("API key"));
    }

    #[test]
    fn all_caps_english_not_misclassified_while_factory_bom_codes_skip() {
        // ALL-CAPS English sentences containing alphanumeric times/codes must still translate
        assert_eq!(
            classify_skip_reason_for_mode(
                "MEETING AT 10AM TOMORROW",
                "VI",
                TranslateDetectMode::ApiCache
            ),
            None
        );
        assert_eq!(
            classify_skip_reason_for_mode(
                "PLEASE CHECK ICON25 STATUS BEFORE 5PM",
                "VI",
                TranslateDetectMode::ApiCache
            ),
            None
        );
        assert_eq!(
            classify_skip_reason_for_mode(
                "CALL ME AT 5PM",
                "VI",
                TranslateDetectMode::ApiCache
            ),
            None
        );

        // Factory part-number / BOM rows stay non_linguistic
        assert_eq!(
            classify_skip_reason_for_mode(
                "EBR28193301\t6L1T03X6-0003\t351",
                "VI",
                TranslateDetectMode::ApiCache
            ),
            Some("non_linguistic")
        );
        assert_eq!(
            classify_skip_reason_for_mode(
                "EAE65236402\tFOC1-BOX-10",
                "VI",
                TranslateDetectMode::ApiCache
            ),
            Some("non_linguistic")
        );
        assert_eq!(
            classify_skip_reason_for_mode("ICON25", "VI", TranslateDetectMode::ApiCache),
            Some("non_linguistic")
        );
    }

    #[test]
    fn normalize_vi_nfc_handles_canonical_nfd_order_and_static_placeholders() {
        assert_eq!(normalize_vi_nfc("e\u{0323}\u{0302}"), "ệ");
        assert_eq!(normalize_vi_nfc("a\u{0323}\u{0302}"), "ậ");
        assert_eq!(normalize_vi_nfc("a\u{0323}\u{0306}"), "ặ");
        assert_eq!(normalize_vi_nfc("o\u{0323}\u{0302}"), "ộ");
        assert_eq!(
            classify_skip_reason("vie\u{0323}\u{0302}c na\u{0300}y xong ro\u{0302}\u{0300}i", "VI"),
            Some("same_language")
        );
        assert_eq!(
            classify_skip_reason("[Ta\u{0323}\u{0302}p tin] report.xlsx", "VI"),
            Some("non_linguistic")
        );
        assert!(is_static_non_translatable("[Ta\u{0323}\u{0302}p tin] report.xlsx"));
        assert!(!is_static_non_translatable("MEETING AT 10AM TOMORROW"));
    }

    #[test]
    fn parse_gemini_results_never_cross_wires_on_missing_or_numeric_ids() {
        let items = vec![
            TranslateInputItem {
                msg_id: "101".into(),
                text: "Hello".into(),
            },
            TranslateInputItem {
                msg_id: "102".into(),
                text: "Goodbye".into(),
            },
        ];
        // When item 101 is omitted, item 102 must NOT fall into 101 by index
        let partial = r#"[{"id":"102","text":"Tạm biệt","src":"en"}]"#;
        let partial_res = parse_gemini_results(partial, &items).unwrap();
        assert_eq!(partial_res.len(), 1);
        assert_eq!(partial_res[0].msg_id, "102");
        assert_eq!(partial_res[0].translated_text, "Tạm biệt");

        // Numeric JSON id is also matched cleanly
        let numeric = r#"[{"id":101,"text":"Xin chào","src":"en"},{"id":102,"text":"Tạm biệt","src":"en"}]"#;
        let num_res = parse_gemini_results(numeric, &items).unwrap();
        assert_eq!(num_res.len(), 2);
        assert_eq!(num_res[0].msg_id, "101");
        assert_eq!(num_res[1].msg_id, "102");

        // Equal-length response without IDs still matches by position
        let idless = r#"[{"text":"Xin chào","src":"en"},{"text":"Tạm biệt","src":"en"}]"#;
        let idless_res = parse_gemini_results(idless, &items).unwrap();
        assert_eq!(idless_res.len(), 2);
        assert_eq!(idless_res[0].translated_text, "Xin chào");
        assert_eq!(idless_res[1].translated_text, "Tạm biệt");
    }
}
