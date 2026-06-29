//! news — keyless news headlines via Google News RSS (no API key, no 401/paywall on the feed itself;
//! it indexes most outlets incl. ones that block direct scraping like Reuters). Works for ANY topic
//! (`/rss/search?q=`) or top world stories. Results are UNTRUSTED (headlines are attacker-influenced).

use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub published: String,
}

#[async_trait]
pub trait NewsClient: Send + Sync {
    /// Headlines for `topic` (a free-text query) or, if None, top world stories.
    async fn headlines(&self, topic: Option<&str>, limit: usize) -> anyhow::Result<Vec<NewsItem>>;
}

/// Render headlines as a compact, numbered digest for chat / an agent observation.
pub fn render_news(items: &[NewsItem]) -> String {
    if items.is_empty() {
        return "(no headlines found)".to_string();
    }
    items
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let src = if n.source.is_empty() { String::new() } else { format!(" — {}", n.source) };
            let when = short_date(&n.published);
            format!("{}. {}{}{}\n   {}", i + 1, n.title, src, when, n.url)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Keyless Google News RSS client.
pub struct GoogleNews {
    /// hl/gl/ceid locale (e.g. "en-US"/"US"/"US:en"); India users may prefer en-IN.
    hl: String,
    gl: String,
}

impl Default for GoogleNews {
    fn default() -> Self {
        Self { hl: "en-US".into(), gl: "US".into() }
    }
}

impl GoogleNews {
    pub fn new() -> Self {
        Self::default()
    }
    /// Locale-tuned (e.g. GoogleNews::with_locale("en-IN", "IN")).
    pub fn with_locale(hl: impl Into<String>, gl: impl Into<String>) -> Self {
        Self { hl: hl.into(), gl: gl.into() }
    }
}

#[async_trait]
impl NewsClient for GoogleNews {
    async fn headlines(&self, topic: Option<&str>, limit: usize) -> anyhow::Result<Vec<NewsItem>> {
        let (hl, gl) = (self.hl.clone(), self.gl.clone());
        let ceid = format!("{gl}:{}", hl.split('-').next().unwrap_or("en"));
        let url = match topic {
            Some(q) if !q.trim().is_empty() => format!(
                "https://news.google.com/rss/search?q={}&hl={hl}&gl={gl}&ceid={ceid}",
                pct_encode(q.trim())
            ),
            _ => format!("https://news.google.com/rss?hl={hl}&gl={gl}&ceid={ceid}"),
        };
        let want = limit.clamp(1, 20);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<NewsItem>> {
            let xml = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(20))
                .set("User-Agent", "Mozilla/5.0 (compatible; yantrik-mind/1.0)")
                .call()?
                .into_string()?;
            Ok(parse_rss(&xml, want))
        })
        .await?
    }
}

/// Deterministic news for tests.
pub struct ScriptedNews {
    pub items: Vec<NewsItem>,
}

impl ScriptedNews {
    pub fn new(items: Vec<NewsItem>) -> Self {
        Self { items }
    }
}

#[async_trait]
impl NewsClient for ScriptedNews {
    async fn headlines(&self, _topic: Option<&str>, limit: usize) -> anyhow::Result<Vec<NewsItem>> {
        Ok(self.items.iter().take(limit).cloned().collect())
    }
}

// ── parsing ──────────────────────────────────────────────────────────────────────────────────

fn pct_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&apos;", "'")
        .trim()
        .to_string()
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let i = s.find(open)? + open.len();
    let j = s[i..].find(close)? + i;
    Some(&s[i..j])
}

fn strip_cdata(s: &str) -> &str {
    s.trim().trim_start_matches("<![CDATA[").trim_end_matches("]]>").trim()
}

/// Pull NewsItems out of a Google News RSS feed.
fn parse_rss(xml: &str, limit: usize) -> Vec<NewsItem> {
    let mut out = Vec::new();
    for item in xml.split("<item>").skip(1) {
        if out.len() >= limit {
            break;
        }
        let mut title = between(item, "<title>", "</title>").map(strip_cdata).map(unescape).unwrap_or_default();
        let url = between(item, "<link>", "</link>").map(strip_cdata).map(unescape).unwrap_or_default();
        let published = between(item, "<pubDate>", "</pubDate>").map(strip_cdata).map(unescape).unwrap_or_default();
        // <source url="...">Name</source>
        let source = item
            .find("<source")
            .and_then(|i| item[i..].find('>').map(|j| i + j + 1))
            .and_then(|start| item[start..].find("</source>").map(|e| &item[start..start + e]))
            .map(strip_cdata)
            .map(unescape)
            .unwrap_or_default();
        // Google News titles are "Headline - Source"; drop the trailing source for a clean line.
        if !source.is_empty() {
            if let Some(stripped) = title.strip_suffix(&format!(" - {source}")) {
                title = stripped.to_string();
            }
        }
        if !title.is_empty() && url.starts_with("http") {
            out.push(NewsItem { title, url, source, published });
        }
    }
    out
}

/// "Mon, 29 Jun 2026 14:03:00 GMT" → " (29 Jun)"; empty/garbage → "".
fn short_date(p: &str) -> String {
    let parts: Vec<&str> = p.split_whitespace().collect();
    if parts.len() >= 3 {
        format!(" ({} {})", parts[1], parts[2])
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_encode_query() {
        assert_eq!(pct_encode("middle east war"), "middle%20east%20war");
    }

    #[test]
    fn parse_google_news_rss() {
        let xml = r#"<rss><channel>
          <item><title>Talks stall in Geneva - Reuters</title>
            <link>https://news.google.com/rss/articles/abc</link>
            <pubDate>Mon, 29 Jun 2026 14:03:00 GMT</pubDate>
            <source url="https://reuters.com">Reuters</source></item>
          <item><title>Sanctions widen &amp; markets dip</title>
            <link>https://news.google.com/rss/articles/def</link>
            <pubDate>Mon, 29 Jun 2026 13:00:00 GMT</pubDate></item>
        </channel></rss>"#;
        let items = parse_rss(xml, 10);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Talks stall in Geneva", "source suffix stripped");
        assert_eq!(items[0].source, "Reuters");
        assert_eq!(items[1].title, "Sanctions widen & markets dip", "entities unescaped");
        let r = render_news(&items);
        assert!(r.contains("1. Talks stall in Geneva — Reuters (29 Jun)"));
    }
}
