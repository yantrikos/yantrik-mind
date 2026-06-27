//! mind-tools — capabilities the mind can use to reach the world. First: web browsing.
//!
//! `Fetcher` is the injectable seam (real HTTP vs scripted-for-tests). Browsing is READ-ONLY here
//! and its output must be treated as untrusted by callers (the conversation layer wraps it as
//! reference-data-not-instructions — web pages are a prompt-injection surface). Any *action* on a
//! page (forms, clicks, logins) is a separate, harm-gated capability and is deliberately not here.

use async_trait::async_trait;

#[async_trait]
pub trait Fetcher: Send + Sync {
    /// Fetch a URL and return readable text (HTML stripped, bounded length).
    async fn fetch(&self, url: &str) -> anyhow::Result<String>;
}

/// Real HTTP fetcher: GET → strip HTML to readable text → truncate. Blocking ureq on the blocking
/// pool so it never stalls the async runtime.
pub struct HttpFetcher {
    max_chars: usize,
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self { max_chars: 4000 }
    }
}

impl HttpFetcher {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_max_chars(max_chars: usize) -> Self {
        Self { max_chars }
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn fetch(&self, url: &str) -> anyhow::Result<String> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            anyhow::bail!("only http(s) urls are fetchable");
        }
        let url = url.to_string();
        let max = self.max_chars;
        let text = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let html = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(20))
                .call()?
                .into_string()?;
            Ok(html2text::from_read(html.as_bytes(), 100))
        })
        .await??;
        let mut t = text.trim().to_string();
        if t.len() > max {
            t.truncate(max);
            t.push_str("\n…(truncated)");
        }
        Ok(t)
    }
}

/// Deterministic fetcher for tests/evals — returns a canned document for any URL.
pub struct ScriptedFetcher {
    pub doc: String,
}

impl ScriptedFetcher {
    pub fn new(doc: impl Into<String>) -> Self {
        Self { doc: doc.into() }
    }
}

#[async_trait]
impl Fetcher for ScriptedFetcher {
    async fn fetch(&self, _url: &str) -> anyhow::Result<String> {
        Ok(self.doc.clone())
    }
}

/// Pull the first http(s) URL out of a message, if any.
pub fn first_url(text: &str) -> Option<String> {
    let bytes = text;
    for marker in ["https://", "http://"] {
        if let Some(start) = bytes.find(marker) {
            let rest = &bytes[start..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '>' || c == ')' || c == '"')
                .unwrap_or(rest.len());
            let url = rest[..end].trim_end_matches(['.', ',', ';']);
            if url.len() > marker.len() {
                return Some(url.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_url_extracts_and_trims() {
        assert_eq!(first_url("see https://example.com/x, thanks").as_deref(), Some("https://example.com/x"));
        assert_eq!(first_url("no link here"), None);
        assert_eq!(first_url("(http://a.b/c)").as_deref(), Some("http://a.b/c"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scripted_fetcher_returns_canned() {
        let f = ScriptedFetcher::new("hello world");
        assert_eq!(f.fetch("https://anything").await.unwrap(), "hello world");
    }
}
