//! search — keyless web search (DuckDuckGo HTML endpoint), the discovery half of research. Pairs
//! with the SSRF-guarded `HttpFetcher` (fetch) so a sub-agent can search → fetch → synthesize.
//! Results are UNTRUSTED (titles/snippets are attacker-controllable) — the caller wraps them.

use async_trait::async_trait;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[async_trait]
pub trait WebSearch: Send + Sync {
    async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchHit>>;
}

/// Render hits as a compact, numbered list for an agent's observation.
pub fn render_search(hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return "(no results)".to_string();
    }
    hits.iter()
        .enumerate()
        .map(|(i, h)| format!("{}. {} — {}\n   {}", i + 1, h.title, h.url, h.snippet))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Keyless DuckDuckGo HTML search.
pub struct DdgSearch {
    max: usize,
}

impl Default for DdgSearch {
    fn default() -> Self {
        Self { max: 8 }
    }
}

impl DdgSearch {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl WebSearch for DdgSearch {
    async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchHit>> {
        let q = query.to_string();
        let want = limit.min(self.max).max(1);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<SearchHit>> {
            let resp = ureq::post("https://html.duckduckgo.com/html/")
                .timeout(std::time::Duration::from_secs(20))
                .set("User-Agent", "Mozilla/5.0 (compatible; yantrik-mind/1.0)")
                .send_form(&[("q", q.as_str())])?;
            let html = resp.into_string()?;
            Ok(parse_ddg(&html, want))
        })
        .await?
    }
}

/// Self-hosted SearXNG metasearch via its JSON API — aggregates many engines (Google/Bing/Brave/…),
/// no bot-challenge, no rate limit, private, and it indexes sites our direct fetch can't reach. A
/// strict upgrade over scraping DDG. Falls back to a backup searcher (e.g. DDG) on error/empty so a
/// down instance never blanks search. Results are UNTRUSTED (the caller wraps them).
pub struct SearxngSearch {
    base: String,
    fallback: Option<Arc<dyn WebSearch>>,
}

impl SearxngSearch {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into().trim_end_matches('/').to_string(), fallback: None }
    }
    pub fn with_fallback(mut self, fb: Arc<dyn WebSearch>) -> Self {
        self.fallback = Some(fb);
        self
    }
}

#[async_trait]
impl WebSearch for SearxngSearch {
    async fn search(&self, query: &str, limit: usize) -> anyhow::Result<Vec<SearchHit>> {
        let url = format!("{}/search", self.base);
        let (q, want) = (query.to_string(), limit.max(1));
        let res = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<SearchHit>> {
            let v: serde_json::Value = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(15))
                .set("User-Agent", "Mozilla/5.0 (compatible; yantrik-mind/1.0)")
                .query("q", &q)
                .query("format", "json")
                .query("language", "en")
                .call()?
                .into_json()?;
            Ok(parse_searxng(&v, want))
        })
        .await?;
        // On error OR empty, fall back to the backup searcher if one is wired.
        match res {
            Ok(hits) if !hits.is_empty() => Ok(hits),
            other => match &self.fallback {
                Some(fb) => fb.search(query, limit).await,
                None => other,
            },
        }
    }
}

/// Map a SearXNG JSON response (`{results:[{title,url,content}]}`) to hits.
fn parse_searxng(v: &serde_json::Value, limit: usize) -> Vec<SearchHit> {
    v.get("results")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    let url = r.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let title = r.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    if !url.starts_with("http") || title.is_empty() {
                        return None;
                    }
                    let snippet = r.get("content").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    Some(SearchHit { title, url, snippet })
                })
                .take(limit)
                .collect()
        })
        .unwrap_or_default()
}

/// Deterministic search for tests.
pub struct ScriptedSearch {
    pub hits: Vec<SearchHit>,
}

impl ScriptedSearch {
    pub fn new(hits: Vec<SearchHit>) -> Self {
        Self { hits }
    }
}

#[async_trait]
impl WebSearch for ScriptedSearch {
    async fn search(&self, _query: &str, limit: usize) -> anyhow::Result<Vec<SearchHit>> {
        Ok(self.hits.iter().take(limit).cloned().collect())
    }
}

// ── Parsing ─────────────────────────────────────────────────────────────────────────────────

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(n) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(n as char);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { ' ' } else { b[i] as char });
        i += 1;
    }
    out
}

fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

/// Pull (title, url, snippet) triples out of DDG's HTML results.
fn parse_ddg(html: &str, limit: usize) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    for chunk in html.split("class=\"result__a\"").skip(1) {
        if hits.len() >= limit {
            break;
        }
        let href = chunk.split("href=\"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("");
        let mut url = href.to_string();
        // Some DDG variants wrap the target in a redirect: //duckduckgo.com/l/?uddg=<encoded>
        if let Some(idx) = url.find("uddg=") {
            let enc = &url[idx + 5..];
            let enc = enc.split('&').next().unwrap_or(enc);
            url = pct_decode(enc);
        }
        if url.starts_with("//") {
            url = format!("https:{url}");
        }
        let title = strip_tags(
            chunk.splitn(2, '>').nth(1).unwrap_or("").split("</a>").next().unwrap_or(""),
        );
        let snippet = chunk
            .split("result__snippet")
            .nth(1)
            .and_then(|s| s.splitn(2, '>').nth(1))
            .and_then(|s| s.split("</a>").next())
            .map(strip_tags)
            .unwrap_or_default();
        if url.starts_with("http") && !title.is_empty() {
            hits.push(SearchHit { title, url, snippet });
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_decode_basics() {
        assert_eq!(pct_decode("https%3A%2F%2Fa.com%2Fx"), "https://a.com/x");
    }

    #[test]
    fn parse_direct_and_redirect_links() {
        let html = r#"
          <a class="result__a" href="https://rust-lang.org/async">Rust Async &amp; You</a>
          <a class="result__snippet" href="x">A guide to <b>async</b> in Rust.</a>
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Ftokio.rs%2F&rut=z">Tokio</a>
          <a class="result__snippet">The async runtime.</a>
        "#;
        let hits = parse_ddg(html, 5);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://rust-lang.org/async");
        assert_eq!(hits[0].title, "Rust Async & You");
        assert!(hits[0].snippet.contains("async in Rust"));
        assert_eq!(hits[1].url, "https://tokio.rs/", "uddg redirect must be decoded");
    }

    #[test]
    fn parse_searxng_json_to_hits() {
        let v = serde_json::json!({
            "results": [
                {"title": "Rust Async", "url": "https://rust-lang.org/async", "content": "A guide."},
                {"title": "No URL", "url": "", "content": "skip me"},
                {"title": "Tokio", "url": "https://tokio.rs/", "content": "The runtime."}
            ]
        });
        let hits = parse_searxng(&v, 5);
        assert_eq!(hits.len(), 2, "drops the result with no http url");
        assert_eq!(hits[0].url, "https://rust-lang.org/async");
        assert_eq!(hits[0].snippet, "A guide.");
        assert_eq!(hits[1].title, "Tokio");
        // limit is respected
        assert_eq!(parse_searxng(&v, 1).len(), 1);
        // a malformed response → empty (so the caller can fall back)
        assert!(parse_searxng(&serde_json::json!({"oops": true}), 5).is_empty());
    }

    #[test]
    fn render_is_numbered() {
        let h = vec![SearchHit { title: "T".into(), url: "https://x".into(), snippet: "s".into() }];
        assert!(render_search(&h).starts_with("1. T — https://x"));
        assert_eq!(render_search(&[]), "(no results)");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scripted_respects_limit() {
        let s = ScriptedSearch::new(vec![
            SearchHit { title: "a".into(), url: "https://a".into(), snippet: String::new() },
            SearchHit { title: "b".into(), url: "https://b".into(), snippet: String::new() },
        ]);
        assert_eq!(s.search("q", 1).await.unwrap().len(), 1);
    }
}
