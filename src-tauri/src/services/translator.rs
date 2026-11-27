//! Description translation via the unofficial Google Translate endpoint.
//! Mirrors the Python `description_translation_service` using
//! `deep_translator` — we call the single-endpoint JSON API directly.

use parking_lot::Mutex;
use std::collections::HashMap;
use url::form_urlencoded;

#[derive(Default)]
pub struct TranslatorCache {
    cache: Mutex<HashMap<String, String>>,
}

impl TranslatorCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(target: &str, source: &str, text: &str) -> String {
        format!("{target}::{source}::{text}")
    }

    pub fn get(&self, target: &str, source: &str, text: &str) -> Option<String> {
        self.cache
            .lock()
            .get(&Self::key(target, source, text))
            .cloned()
    }

    pub fn put(&self, target: &str, source: &str, text: &str, value: String) {
        self.cache
            .lock()
            .insert(Self::key(target, source, text), value);
    }
}

pub async fn translate_text(
    text: &str,
    source_lang: &str,
    target_lang: &str,
) -> anyhow::Result<String> {
    if text.trim().is_empty() {
        return Ok(text.to_string());
    }

    let source = map_language(source_lang);
    let target = map_language(target_lang);
    if source == target {
        return Ok(text.to_string());
    }

    let params: String = form_urlencoded::Serializer::new(String::new())
        .append_pair("client", "gtx")
        .append_pair("sl", source)
        .append_pair("tl", target)
        .append_pair("dt", "t")
        .append_pair("q", text)
        .finish();

    let url = format!("https://translate.googleapis.com/translate_a/single?{params}");

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 WEave")
        .timeout(TRANSLATE_TIMEOUT)
        .build()?;

    let body = client.get(url).send().await?.text().await?;
    let json: serde_json::Value = serde_json::from_str(&body)?;

    let mut result = String::new();
    if let Some(arr) = json.get(0).and_then(|v| v.as_array()) {
        for entry in arr {
            if let Some(s) = entry.get(0).and_then(|v| v.as_str()) {
                result.push_str(s);
            }
        }
    }

    if result.is_empty() {
        anyhow::bail!("translation returned empty result");
    }

    Ok(result)
}

fn map_language(code: &str) -> &str {
    match code {
        // Google's translate_a/single endpoint accepts `auto` as the
        // source language and infers it from the text. The previous
        // mapping silently fell through to "en", which broke detection
        // of any non-English Workshop description.
        "auto" | "" => "auto",
        "en" => "en",
        "ru" => "ru",
        "de" => "de",
        "es" => "es",
        "fr" => "fr",
        "pt" => "pt",
        "it" => "it",
        "pl" => "pl",
        "ja" => "ja",
        "ko" => "ko",
        "zh" => "zh-CN",
        "zh-CN" | "zh-Hans" => "zh-CN",
        "zh-TW" | "zh-Hant" => "zh-TW",
        other => {
            // For an unknown locale, ask Google to auto-detect rather
            // than mistranslating from English.
            if other.contains('-') {
                other
            } else {
                "auto"
            }
        }
    }
}

// ── Timing constants ──────────────────────────────────────────
/// HTTP timeout for a translation request.
const TRANSLATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_and_empty_both_mean_autodetect() {
        assert_eq!(map_language("auto"), "auto");
        assert_eq!(map_language(""), "auto");
    }

    #[test]
    fn plain_codes_pass_through() {
        for code in ["en", "ru", "de", "es", "fr", "pt", "it", "pl", "ja", "ko"] {
            assert_eq!(map_language(code), code);
        }
    }

    #[test]
    fn chinese_variants_normalise_to_the_google_codes() {
        assert_eq!(map_language("zh"), "zh-CN");
        assert_eq!(map_language("zh-CN"), "zh-CN");
        assert_eq!(map_language("zh-Hans"), "zh-CN");
        assert_eq!(map_language("zh-TW"), "zh-TW");
        assert_eq!(map_language("zh-Hant"), "zh-TW");
    }

    #[test]
    fn an_unknown_hyphenated_locale_is_passed_through() {
        // A regional variant Google may understand; better than forcing en.
        assert_eq!(map_language("pt-BR"), "pt-BR");
        assert_eq!(map_language("sr-Latn"), "sr-Latn");
    }

    #[test]
    fn an_unknown_bare_code_falls_back_to_autodetect() {
        // The old behaviour forced "en", which mistranslated every
        // non-English description whose locale we did not list.
        assert_eq!(map_language("xx"), "auto");
        assert_eq!(map_language("klingon"), "auto");
    }

    #[test]
    fn the_cache_key_separates_all_three_parts() {
        assert_eq!(
            TranslatorCache::key("zh-CN", "en", "hello"),
            "zh-CN::en::hello"
        );
        // Distinct inputs must not collide.
        assert_ne!(
            TranslatorCache::key("zh-CN", "en", "a::b"),
            TranslatorCache::key("zh-CN", "a", "b")
        );
    }

    #[test]
    fn the_cache_round_trips_a_value() {
        let cache = TranslatorCache::new();
        assert_eq!(cache.get("zh-CN", "en", "hello"), None);
        cache.put("zh-CN", "en", "hello", "你好".to_string());
        assert_eq!(cache.get("zh-CN", "en", "hello").as_deref(), Some("你好"));
    }

    #[test]
    fn the_cache_is_keyed_by_all_three_parts() {
        let cache = TranslatorCache::new();
        cache.put("zh-CN", "en", "hello", "你好".to_string());
        assert_eq!(cache.get("zh-TW", "en", "hello"), None);
        assert_eq!(cache.get("zh-CN", "de", "hello"), None);
        assert_eq!(cache.get("zh-CN", "en", "bye"), None);
    }
}
