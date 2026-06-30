//! mind-tools — capabilities the mind can use to reach the world. First: web browsing.
//!
//! `Fetcher` is the injectable seam (real HTTP vs scripted-for-tests). Browsing is READ-ONLY here
//! and its output must be treated as untrusted by callers (the conversation layer wraps it as
//! reference-data-not-instructions — web pages are a prompt-injection surface). Any *action* on a
//! page (forms, clicks, logins) is a separate, harm-gated capability and is deliberately not here.

use async_trait::async_trait;
use std::io::Read;
use std::net::{IpAddr, ToSocketAddrs};

pub mod mail;
pub use mail::{
    render_inbox_digest, EmailMsg, ImapClient, MailClient, MailSender, ScriptedMailClient,
    ScriptedMailSender, SmtpMailSender,
};

pub mod executor;
pub use executor::ToolActionExecutor;

pub mod search;
pub use search::{render_search, DdgSearch, ScriptedSearch, SearchHit, WebSearch};

pub mod sandbox;
pub use sandbox::{ExecResult, Limits, Sandbox};

pub mod coder;
pub use coder::{render_coder, Coder, CoderResult};

pub mod workers;
pub use workers::WorkerPool;

pub mod github;
pub use github::{
    render_github_digest, ApiGithubClient, GithubClient, GithubNotification, GithubWriter,
    ScriptedGithubClient, ScriptedGithubWriter,
};

pub mod homeassistant;
pub use homeassistant::{
    home_alerts, render_home_digest, ApiHomeAssistantClient, HaEntity, HomeAssistantClient,
    ScriptedHomeAssistantClient,
};

pub mod news;
pub use news::{render_news, GoogleNews, NewsClient, NewsItem, ScriptedNews};

pub mod weather;
pub use weather::{OpenMeteo, ScriptedWeather, WeatherClient};

pub mod wikipedia;
pub use wikipedia::{ScriptedWiki, WikiClient, Wikipedia};

pub mod markets;
pub use markets::{LiveMarkets, MarketsClient, Quote, ScriptedMarkets};

pub mod translate;
pub use translate::{GoogleTranslate, ScriptedTranslator, Translator};

pub mod mcp;
pub use mcp::{McpHub, McpServerConfig, McpTool};

#[async_trait]
pub trait Fetcher: Send + Sync {
    /// Fetch a URL and return readable text (HTML stripped, bounded length).
    async fn fetch(&self, url: &str) -> anyhow::Result<String>;
}

/// Pull the host out of an http(s) URL (handles userinfo + bracketed IPv6).
fn host_of(url: &str) -> Option<String> {
    let after = url.splitn(2, "://").nth(1)?;
    let authority = after.split(['/', '?', '#']).next()?;
    let authority = authority.rsplitn(2, '@').next()?; // drop userinfo
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next()?.to_string() // IPv6 literal
    } else {
        authority.split(':').next()?.to_string()
    };
    (!host.is_empty()).then_some(host)
}

/// SSRF guard: is this resolved IP private/internal and therefore off-limits?
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            v.is_loopback() || v.is_private() || v.is_link_local() || v.is_unspecified()
                || v.is_broadcast() || v.is_documentation()
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            v.is_loopback() || v.is_unspecified()
                || (s[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
        }
    }
}

/// Refuse to fetch internal/private targets (resolves the host first, so a public name that
/// points at a private IP is also blocked).
fn ssrf_check(url: &str) -> anyhow::Result<()> {
    let host = host_of(url).ok_or_else(|| anyhow::anyhow!("bad url"))?;
    if let Ok(addrs) = (host.as_str(), 443u16).to_socket_addrs() {
        for a in addrs {
            if is_blocked_ip(a.ip()) {
                anyhow::bail!("refusing to fetch a private/internal address (SSRF guard): {host}");
            }
        }
    }
    Ok(())
}

/// Remove every `<tag …>…</tag>` block (case-insensitive, boundary-checked so `<nav>` ≠ `<navbar>`).
/// Used to strip boilerplate/noise (scripts, nav, footers…) BEFORE html→text, so the reader sees the
/// article instead of menus — the difference between a usable web fetch and a wall of chrome.
fn strip_block(html: &str, tag: &str) -> String {
    let lower = html.to_lowercase();
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        match lower[i..].find(&open) {
            Some(rel) => {
                let start = i + rel;
                let after = lower[start + open.len()..].chars().next();
                // require a real tag boundary after "<tag"
                if !matches!(after, Some('>') | Some('/') | Some(' ') | Some('\t') | Some('\n') | Some('\r') | None) {
                    out.push_str(&html[i..start + open.len()]);
                    i = start + open.len();
                    continue;
                }
                out.push_str(&html[i..start]);
                match lower[start..].find(&close) {
                    Some(crel) => i = start + crel + close.len(),
                    None => i = html.len(), // unclosed → drop the rest
                }
            }
            None => {
                out.push_str(&html[i..]);
                break;
            }
        }
    }
    out
}

/// Readability-lite: drop the non-content blocks so the extracted text is the actual article. Keeps
/// <head> so html2text still emits the page <title>.
fn declutter(html: &str) -> String {
    let mut s = html.to_string();
    for tag in ["script", "style", "noscript", "svg", "nav", "header", "footer", "aside", "form", "iframe", "button", "select"] {
        s = strip_block(&s, tag);
    }
    s
}

/// Real HTTP fetcher: GET → declutter → HTML→readable text → bound length. Blocking ureq on the
/// blocking pool so it never stalls the async runtime.
pub struct HttpFetcher {
    max_chars: usize,
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self { max_chars: 8000 }
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

/// A real Chrome UA — header-less requests get blocked or served junk by many sites.
const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

/// Collapse html2text's runs of blank lines so the content is dense, not sparse.
fn compact_blanks(text: &str) -> String {
    let mut compact = String::with_capacity(text.len());
    let mut blanks = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks <= 1 {
                compact.push('\n');
            }
        } else {
            blanks = 0;
            compact.push_str(line.trim_end());
            compact.push('\n');
        }
    }
    compact
}

/// Direct fetch with real browser headers + redirect-following → declutter → readable text.
fn fetch_direct(url: &str) -> anyhow::Result<String> {
    let resp = ureq::get(url)
        .timeout(std::time::Duration::from_secs(20))
        .set("User-Agent", BROWSER_UA)
        .set("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8")
        .set("Accept-Language", "en-US,en;q=0.9")
        .call()?;
    let mut bytes = Vec::new();
    resp.into_reader().take(2_000_000).read_to_end(&mut bytes)?; // 2 MB cap (memory wall)
    let cleaned = declutter(&String::from_utf8_lossy(&bytes));
    Ok(compact_blanks(&html2text::from_read(cleaned.as_bytes(), 100)))
}

/// Reader-proxy fallback: a server-side reader renders the page with a REAL browser and returns clean
/// markdown — getting content from sites that block our direct request (bot/TLS/JS walls). The target
/// is already SSRF-checked + public; the fetched text remains untrusted reference data.
fn fetch_reader(url: &str) -> anyhow::Result<String> {
    let resp = ureq::get(&format!("https://r.jina.ai/{url}"))
        .timeout(std::time::Duration::from_secs(30))
        .set("User-Agent", BROWSER_UA)
        .set("X-Return-Format", "markdown")
        .call()?;
    let mut bytes = Vec::new();
    resp.into_reader().take(2_000_000).read_to_end(&mut bytes)?;
    Ok(compact_blanks(&String::from_utf8_lossy(&bytes)))
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
            // SSRF guard FIRST: never let an (injected) URL pull from the local/internal network
            // (this also gates what we'd hand to the reader proxy).
            ssrf_check(&url)?;
            // 1. Direct (fast, private). Accept it if it returned real content.
            let direct = fetch_direct(&url);
            if let Ok(t) = &direct {
                if t.trim().chars().count() >= 250 {
                    return Ok(t.clone());
                }
            }
            // 2. Blocked/empty/too-thin → the reader proxy (real browser server-side). This is the
            //    "browser capability to read any site" path.
            match fetch_reader(&url) {
                Ok(t) if t.trim().chars().count() > 80 => Ok(t),
                _ => direct.or_else(|_| anyhow::bail!("couldn't fetch (direct + reader both failed)")),
            }
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

    #[test]
    fn declutter_drops_noise_keeps_content() {
        let html = "<html><head><title>Hi</title></head><body>\
            <nav>menu home about</nav>\
            <script>var x = 1; track();</script>\
            <article>The real article text.</article>\
            <footer>copyright junk</footer>\
            <navbar>keep this</navbar></body></html>";
        let out = declutter(html);
        assert!(out.contains("The real article text."), "keeps article: {out}");
        assert!(out.contains("<title>Hi</title>"), "keeps head/title");
        assert!(!out.contains("track()"), "drops script");
        assert!(!out.contains("menu home about"), "drops nav");
        assert!(!out.contains("copyright junk"), "drops footer");
        assert!(out.contains("keep this"), "boundary: <navbar> is NOT <nav>");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scripted_fetcher_returns_canned() {
        let f = ScriptedFetcher::new("hello world");
        assert_eq!(f.fetch("https://anything").await.unwrap(), "hello world");
    }

    #[test]
    fn host_of_handles_userinfo_ports_and_ipv6() {
        assert_eq!(host_of("https://example.com/x").as_deref(), Some("example.com"));
        assert_eq!(host_of("http://u:p@10.0.0.5:8080/x").as_deref(), Some("10.0.0.5"));
        assert_eq!(host_of("http://[::1]:7438/health").as_deref(), Some("::1"));
    }

    #[test]
    fn ssrf_guard_blocks_private_loopback_and_metadata() {
        for u in [
            "http://127.0.0.1/",
            "http://localhost/",
            "http://192.168.4.140:7438/v1/health", // the YDB cluster on the LAN
            "http://10.0.0.5/",
            "http://169.254.169.254/latest/meta-data/", // cloud metadata
            "http://[::1]:7438/",
        ] {
            assert!(ssrf_check(u).is_err(), "should block internal target: {u}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_fetcher_refuses_internal_targets() {
        let f = HttpFetcher::new();
        let err = f.fetch("http://192.168.4.140:7438/v1/health").await.unwrap_err();
        assert!(err.to_string().contains("SSRF"), "expected SSRF refusal, got: {err}");
    }
}
