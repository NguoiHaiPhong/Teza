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

#[derive(Debug, Clone)]
pub struct TranslateConfig {
    pub provider: TranslateProvider,
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
            target_lang: if file.target_lang.trim().is_empty() {
                default_target_lang()
            } else {
                file.target_lang.trim().to_uppercase()
            },
            deepl_key: dpapi::unprotect_secret(&file.sealed_deepl_key).unwrap_or_default(),
            gemini_key: dpapi::unprotect_secret(&file.sealed_gemini_key).unwrap_or_default(),
            gemini_model: if file.gemini_model.trim().is_empty() {
                default_gemini_model()
            } else {
                file.gemini_model.trim().to_string()
            },
            azure_key: dpapi::unprotect_secret(&file.sealed_azure_key).unwrap_or_default(),
            azure_region: file.azure_region.trim().to_string(),
        }
    }

    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        let payload = SealedTranslateFile {
            provider: self.provider,
            target_lang: self.target_lang.trim().to_uppercase(),
            sealed_deepl_key: dpapi::protect_secret(self.deepl_key.trim())?,
            sealed_gemini_key: dpapi::protect_secret(self.gemini_key.trim())?,
            gemini_model: self.gemini_model.trim().to_string(),
            sealed_azure_key: dpapi::protect_secret(self.azure_key.trim())?,
            azure_region: self.azure_region.trim().to_string(),
        };
        let json = serde_json::to_string_pretty(&payload)?;
        data_dir::write_private_atomic(path, json.as_bytes())
    }

    pub fn summary(&self) -> TranslateConfigSummary {
        TranslateConfigSummary {
            provider: self.provider,
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
            let clean: String = m
                .trim()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || matches!(*c, '-' | '.' | '_'))
                .collect();
            if !clean.is_empty() {
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
        )
}

pub async fn translate_batch(
    cfg: &TranslateConfig,
    items: &[TranslateInputItem],
    target_lang_override: Option<&str>,
    provider_override: Option<TranslateProvider>,
) -> Result<Vec<TranslationResult>> {
    translate_batch_with_relay(cfg, &RelayConfig::default(), items, target_lang_override, provider_override).await
}

pub async fn translate_batch_with_relay(
    cfg: &TranslateConfig,
    relay: &RelayConfig,
    items: &[TranslateInputItem],
    target_lang_override: Option<&str>,
    provider_override: Option<TranslateProvider>,
) -> Result<Vec<TranslationResult>> {
    let filtered: Vec<&TranslateInputItem> = items
        .iter()
        .filter(|it| !is_non_translatable_placeholder(&it.text))
        .collect();
    if filtered.is_empty() {
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

    let mut results = Vec::with_capacity(filtered.len());
    for chunk in filtered.chunks(25) {
        let mut part = match provider {
            TranslateProvider::Deepl => translate_deepl(&client, &cfg.deepl_key, chunk, &target_lang).await?,
            TranslateProvider::Gemini => {
                translate_gemini(&client, &cfg.gemini_key, &cfg.gemini_model, chunk, &target_lang).await?
            }
            TranslateProvider::Azure => {
                translate_azure(&client, &cfg.azure_key, &cfg.azure_region, chunk, &target_lang).await?
            }
        };
        results.append(&mut part);
    }
    Ok(results)
}

async fn translate_deepl(
    client: &Client,
    api_key: &str,
    items: &[&TranslateInputItem],
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
        })
        .collect())
}

async fn translate_gemini(
    client: &Client,
    api_key: &str,
    model: &str,
    items: &[&TranslateInputItem],
    target_lang: &str,
) -> Result<Vec<TranslationResult>> {
    let key = api_key.trim();
    if key.is_empty() {
        bail!("Chưa cấu hình Gemini API key trong Cài đặt dịch thuật");
    }
    let model_name = if model.trim().is_empty() {
        "gemini-2.0-flash"
    } else {
        model.trim()
    };
    let input_json: Vec<Value> = items
        .iter()
        .map(|it| json!({ "id": it.msg_id, "text": it.text }))
        .collect();
    let lang_name = target_language_label(target_lang);
    let prompt = format!(
        "Translate each object's `text` field naturally into {lang_name} ({target_lang}). Preserve emojis, line breaks, and @mentions. Return a JSON array of objects with exact fields: `id` (matching input `id`), `text` (translated string), and `src` (2-letter detected source language code).\nInput:\n{}",
        serde_json::to_string(&input_json)?
    );

    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model_name}:generateContent?key={key}"
    );
    let resp = client
        .post(&url)
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
    let parsed: Vec<Value> =
        serde_json::from_str(raw_text).context("Không parse được JSON phản hồi từ Gemini")?;

    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.iter().enumerate() {
        let found = parsed
            .iter()
            .find(|v| v.get("id").and_then(Value::as_str) == Some(item.msg_id.as_str()))
            .or_else(|| parsed.get(idx));
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
            });
        }
    }
    Ok(out)
}

async fn translate_azure(
    client: &Client,
    api_key: &str,
    region: &str,
    items: &[&TranslateInputItem],
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

        let cfg = TranslateConfig {
            provider: TranslateProvider::Deepl,
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
        assert_eq!(loaded.deepl_key, "secret-deepl-test:fx");
        assert_eq!(loaded.gemini_key, "secret-gemini-test");
        assert_eq!(loaded.azure_key, "secret-azure-test");
        assert_eq!(loaded.azure_region, "southeastasia");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
