//! translate — keyless translation via Google's public translate_a endpoint (source auto-detected).
//! No API key. Output is the translated text. Untrusted (it's web-sourced) — caller presents it.

use async_trait::async_trait;

#[async_trait]
pub trait Translator: Send + Sync {
    /// Translate `text` into `target` (a language name like "french" or a code like "fr").
    async fn translate(&self, target: &str, text: &str) -> anyhow::Result<String>;
}

/// Map a language NAME or code to a 2-letter ISO code (codes pass through).
pub fn lang_code(s: &str) -> String {
    let s = s.trim().to_lowercase();
    match s.as_str() {
        "english" | "en" => "en",
        "french" | "francais" | "fr" => "fr",
        "spanish" | "espanol" | "es" => "es",
        "german" | "deutsch" | "de" => "de",
        "italian" | "it" => "it",
        "portuguese" | "pt" => "pt",
        "dutch" | "nl" => "nl",
        "russian" | "ru" => "ru",
        "hindi" | "hi" => "hi",
        "bengali" | "bangla" | "bn" => "bn",
        "tamil" | "ta" => "ta",
        "telugu" | "te" => "te",
        "marathi" | "mr" => "mr",
        "gujarati" | "gu" => "gu",
        "urdu" | "ur" => "ur",
        "arabic" | "ar" => "ar",
        "chinese" | "mandarin" => "zh-CN",
        "japanese" | "ja" => "ja",
        "korean" | "ko" => "ko",
        "turkish" | "tr" => "tr",
        "vietnamese" | "vi" => "vi",
        "thai" | "th" => "th",
        "indonesian" | "id" => "id",
        "polish" | "pl" => "pl",
        "ukrainian" | "uk" => "uk",
        "greek" | "el" => "el",
        "hebrew" | "he" => "he",
        "swedish" | "sv" => "sv",
        other => return other.to_string(), // assume a valid code already
    }
    .to_string()
}

/// Keyless Google translate_a client.
pub struct GoogleTranslate;

impl GoogleTranslate {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GoogleTranslate {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl Translator for GoogleTranslate {
    async fn translate(&self, target: &str, text: &str) -> anyhow::Result<String> {
        let text = text.trim().to_string();
        if text.is_empty() {
            anyhow::bail!("translate what?");
        }
        let tl = lang_code(target);
        tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            // sl=auto detects the source; dt=t returns translation segments.
            let v: serde_json::Value = ureq::get("https://translate.googleapis.com/translate_a/single")
                .timeout(std::time::Duration::from_secs(15))
                .query("client", "gtx")
                .query("sl", "auto")
                .query("tl", &tl)
                .query("dt", "t")
                .query("q", &text)
                .call()?
                .into_json()?;
            // shape: [ [ ["translated","src",...], ["seg2",...] ], ..., "detected-lang" ]
            let segs = v.get(0).and_then(|x| x.as_array()).ok_or_else(|| anyhow::anyhow!("unexpected response"))?;
            let mut out = String::new();
            for seg in segs {
                if let Some(t) = seg.get(0).and_then(|x| x.as_str()) {
                    out.push_str(t);
                }
            }
            let src = v.get(2).and_then(|x| x.as_str()).unwrap_or("");
            if out.trim().is_empty() {
                anyhow::bail!("couldn't translate that");
            }
            Ok(if src.is_empty() || src == tl {
                format!("🌐 {out}")
            } else {
                format!("🌐 ({src}→{tl}) {out}")
            })
        })
        .await?
    }
}

/// Deterministic translator for tests.
pub struct ScriptedTranslator {
    pub text: String,
}

#[async_trait]
impl Translator for ScriptedTranslator {
    async fn translate(&self, _target: &str, _text: &str) -> anyhow::Result<String> {
        Ok(self.text.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_names_and_codes() {
        assert_eq!(lang_code("French"), "fr");
        assert_eq!(lang_code("hindi"), "hi");
        assert_eq!(lang_code("zh"), "zh");
        assert_eq!(lang_code("chinese"), "zh-CN");
        assert_eq!(lang_code("fr"), "fr");
    }
}
