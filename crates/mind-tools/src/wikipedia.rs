//! wikipedia — keyless factual lookups via the MediaWiki API (search + intro extract in one call).
//! Good for "what/who is X" without scraping. The extract is UNTRUSTED reference text (caller wraps).

use async_trait::async_trait;

#[async_trait]
pub trait WikiClient: Send + Sync {
    /// Best-matching article's title + intro for a free-text query.
    async fn lookup(&self, query: &str) -> anyhow::Result<String>;
}

/// Keyless English Wikipedia.
pub struct Wikipedia {
    chars: usize,
}

impl Default for Wikipedia {
    fn default() -> Self {
        Self { chars: 900 }
    }
}

impl Wikipedia {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl WikiClient for Wikipedia {
    async fn lookup(&self, query: &str) -> anyhow::Result<String> {
        let q = query.trim().to_string();
        if q.is_empty() {
            anyhow::bail!("look up what?");
        }
        let max = self.chars;
        tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            // search + intro extract in one request (generator=search → prop=extracts)
            let v: serde_json::Value = ureq::get("https://en.wikipedia.org/w/api.php")
                .timeout(std::time::Duration::from_secs(15))
                .set("User-Agent", "yantrik-mind/1.0")
                .query("action", "query")
                .query("format", "json")
                .query("prop", "extracts")
                .query("exintro", "1")
                .query("explaintext", "1")
                .query("redirects", "1")
                .query("generator", "search")
                .query("gsrsearch", &q)
                .query("gsrlimit", "1")
                .call()?
                .into_json()?;
            let pages = v["query"]["pages"].as_object().ok_or_else(|| anyhow::anyhow!("nothing on Wikipedia for \"{q}\""))?;
            let page = pages.values().next().ok_or_else(|| anyhow::anyhow!("nothing on Wikipedia for \"{q}\""))?;
            let title = page["title"].as_str().unwrap_or(&q);
            let mut extract = page["extract"].as_str().unwrap_or("").trim().to_string();
            if extract.is_empty() {
                anyhow::bail!("nothing on Wikipedia for \"{q}\"");
            }
            if extract.chars().count() > max {
                extract = extract.chars().take(max).collect::<String>() + "…";
            }
            Ok(format!("📖 {title}\n{extract}"))
        })
        .await?
    }
}

/// Deterministic Wikipedia for tests.
pub struct ScriptedWiki {
    pub text: String,
}

impl ScriptedWiki {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

#[async_trait]
impl WikiClient for ScriptedWiki {
    async fn lookup(&self, _query: &str) -> anyhow::Result<String> {
        Ok(self.text.clone())
    }
}
