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
pub use search::{render_search, DdgSearch, ScriptedSearch, SearchHit, SearxngSearch, WebSearch};

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

    /// Render a HOSTILE page with a headful real browser (beats headless-fingerprint walls — Amazon,
    /// Target). Slow; use only when the normal ladder gets walled. Default delegates to `fetch`.
    async fn fetch_rendered(&self, url: &str) -> anyhow::Result<String> {
        self.fetch(url).await
    }
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

/// Tier-3 fetch: a LOCAL headless Chromium (Playwright + stealth) renders the page with a real browser
/// → readable text. Beats JS-rendered + most bot-walled sites that block both direct and the reader
/// proxy. A no-op (bails) when the helper script isn't present, so non-box builds are unaffected.
/// `timeout` hard-kills a hung chromium; the script also bounds its own navigation.
fn fetch_headless(url: &str) -> anyhow::Result<String> {
    let script = std::env::var("YM_HEADLESS_SCRIPT").unwrap_or_else(|_| "/opt/yantrik-mind/headless_fetch.js".to_string());
    if !std::path::Path::new(&script).exists() {
        anyhow::bail!("headless fetch not available");
    }
    let browsers = std::env::var("PLAYWRIGHT_BROWSERS_PATH").unwrap_or_else(|_| "/opt/yantrik-mind/pw-browsers".to_string());
    let dir = std::path::Path::new(&script).parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."));
    let out = std::process::Command::new("timeout")
        .arg("45")
        .arg("node")
        .arg(&script)
        .arg(url)
        .current_dir(&dir) // so node resolves ./node_modules (playwright)
        .env("PLAYWRIGHT_BROWSERS_PATH", browsers)
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.trim().chars().count() < 80 {
        anyhow::bail!("headless returned nothing useful");
    }
    Ok(compact_blanks(&text))
}

/// Tier-4: a HEADFUL (on-screen, via Xvfb) real Chromium — defeats the headless-fingerprint walls that
/// block tier-3 (Amazon, Target render real product grids under headful where headless gets 0 chars).
/// Slower + heavier, so it's only used deliberately (hostile retail), never in the default `fetch` ladder.
fn fetch_headful(url: &str) -> anyhow::Result<String> {
    let script = std::env::var("YM_HEADFUL_SCRIPT").unwrap_or_else(|_| "/opt/yantrik-mind/headful_fetch.js".to_string());
    if !std::path::Path::new(&script).exists() {
        anyhow::bail!("headful fetch not available");
    }
    let browsers = std::env::var("PLAYWRIGHT_BROWSERS_PATH").unwrap_or_else(|_| "/opt/yantrik-mind/pw-browsers".to_string());
    let dir = std::path::Path::new(&script).parent().map(|p| p.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."));
    let out = std::process::Command::new("timeout")
        .arg("85")
        .arg("xvfb-run")
        .arg("-a")
        .arg("node")
        .arg(&script)
        .arg(url)
        .current_dir(&dir)
        .env("PLAYWRIGHT_BROWSERS_PATH", browsers)
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.trim().chars().count() < 80 {
        anyhow::bail!("headful returned nothing useful");
    }
    Ok(compact_blanks(&text))
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
            // 2. Blocked/empty/too-thin → the reader proxy (real browser server-side, free keyless).
            if let Ok(t) = fetch_reader(&url) {
                if t.trim().chars().count() > 80 {
                    return Ok(t);
                }
            }
            // 3. Still nothing → a LOCAL headless browser (real Chrome, our IP). Beats JS-rendered +
            //    most bot walls the proxy can't. This is the "browser capability to read any site" path.
            if let Ok(t) = fetch_headless(&url) {
                return Ok(t);
            }
            direct.or_else(|_| anyhow::bail!("couldn't fetch (direct + reader + headless all failed)"))
        })
        .await??;
        let mut t = text.trim().to_string();
        if t.len() > max {
            // Truncate on a CHAR boundary — a raw byte-index truncate panics when `max` lands inside a
            // multi-byte UTF-8 char (em-dash, accented letters, emoji), which arbitrary web pages contain.
            let mut end = max.min(t.len());
            while end > 0 && !t.is_char_boundary(end) {
                end -= 1;
            }
            t.truncate(end);
            t.push_str("\n…(truncated)");
        }
        Ok(t)
    }

    async fn fetch_rendered(&self, url: &str) -> anyhow::Result<String> {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            anyhow::bail!("only http(s) urls are fetchable");
        }
        let u = url.to_string();
        let max = self.max_chars;
        let res = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            ssrf_check(&u)?;
            // Headless FIRST — with self-consistent headers (real UA + Sec-CH-UA, no stale spoof) it now
            // clears Amazon/Target and is far lighter/faster than headful. Escalate to headful only if
            // headless comes back thin (a block page is short; a real product grid is thousands of chars).
            if let Ok(t) = fetch_headless(&u) {
                if t.trim().chars().count() > 400 {
                    return Ok(t);
                }
            }
            fetch_headful(&u)
        })
        .await?;
        match res {
            Ok(mut t) => {
                t = t.trim().to_string();
                if t.len() > max {
                    let mut end = max.min(t.len());
                    while end > 0 && !t.is_char_boundary(end) {
                        end -= 1;
                    }
                    t.truncate(end);
                }
                Ok(t)
            }
            // Headful failed/blocked → fall back to the normal ladder rather than error.
            Err(_) => self.fetch(url).await,
        }
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

/// Vision — analyze an image with a multimodal model (openai-compatible providers). Powers
/// see_page (rendered-page screenshots) and Telegram photo understanding. Fully env-configured:
/// YM_VISION_MODEL = "provider:model" (default nanogpt:gpt-4o-mini) + that provider's key env.
pub struct VisionClient {
    base: String,
    key: String,
    model: String,
    /// Local ollama uses its NATIVE /api/chat with an `images:[b64]` array. Some models (qwen3.6)
    /// genuinely see there but return EMPTY on the OpenAI /v1 image_url shape — so local goes native.
    native: bool,
}

impl VisionClient {
    pub fn from_env() -> Option<VisionClient> {
        // Default: LOCAL vision on the LAN GPU box — zero cloud cost, via ollama's native format so
        // vision-capable text-flagships (qwen3.6) work. "provider:model" (model may contain ':').
        let spec = std::env::var("YM_VISION_MODEL").unwrap_or_else(|_| "ollama-local:qwen3.6:27b".into());
        let (prov, model) = spec.split_once(':').unwrap_or(("ollama-local", spec.as_str()));
        if prov == "ollama-local" || prov == "local" {
            // base = ollama root (strip a trailing /v1 if present) — native calls hit /api/chat.
            let raw = std::env::var("YM_OLLAMA_LOCAL_URL").unwrap_or_else(|_| "http://192.168.4.35:11434".into());
            let base = raw.trim_end_matches("/v1").trim_end_matches('/').to_string();
            return Some(VisionClient { base, key: "ollama".into(), model: model.to_string(), native: true });
        }
        let (base, key_env) = match prov {
            "ollama-cloud" => ("https://ollama.com/v1", "OLLAMA_CLOUD_KEY"),
            "openrouter" => ("https://openrouter.ai/api/v1", "OPEN_ROUTER_KEY"),
            "grok" => ("https://api.x.ai/v1", "GROK_API_KEY"),
            _ => ("https://nano-gpt.com/api/v1", "NANOGPT_KEY"),
        };
        let key = std::env::var(key_env).ok().filter(|k| !k.trim().is_empty())?;
        Some(VisionClient { base: base.into(), key, model: model.to_string(), native: false })
    }

    pub async fn analyze(&self, prompt: &str, image: Vec<u8>, _mime: &str) -> anyhow::Result<String> {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&image);
        let (base, key, model, native) = (self.base.clone(), self.key.clone(), self.model.clone(), self.native);
        let prompt = prompt.to_string();
        let mime = _mime.to_string();
        let text = tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            if native {
                // Ollama native: /api/chat with images array (600s — CPU/GPU local can be slow).
                // Thinking models (qwen3.6) burn most of each call on chain-of-thought we discard —
                // think:false makes extraction 2-5× faster; retry with default thinking if the
                // model rejects the parameter.
                let call = |think_off: bool| -> anyhow::Result<serde_json::Value> {
                    let mut body = serde_json::json!({
                        "model": model, "stream": false,
                        "messages": [{ "role": "user", "content": prompt, "images": [b64] }],
                    });
                    if think_off {
                        body["think"] = serde_json::json!(false);
                    }
                    Ok(ureq::post(&format!("{base}/api/chat"))
                        .set("content-type", "application/json")
                        .timeout(std::time::Duration::from_secs(600))
                        .send_json(body)?
                        .into_json()?)
                };
                let resp = call(true).or_else(|_| call(false))?;
                Ok(resp["message"]["content"].as_str().unwrap_or("").to_string())
            } else {
                let body = serde_json::json!({
                    "model": model, "max_tokens": 900,
                    "messages": [{ "role": "user", "content": [
                        { "type": "text", "text": prompt },
                        { "type": "image_url", "image_url": { "url": format!("data:{mime};base64,{b64}") } }
                    ]}]
                });
                let resp: serde_json::Value = ureq::post(&format!("{base}/chat/completions"))
                    .set("authorization", &format!("Bearer {key}"))
                    .set("content-type", "application/json")
                    .timeout(std::time::Duration::from_secs(120))
                    .send_json(body)?
                    .into_json()?;
                Ok(resp["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string())
            }
        })
        .await??;
        if text.trim().is_empty() {
            anyhow::bail!("vision model returned nothing");
        }
        Ok(text.trim().to_string())
    }
}

/// Fetch raw image bytes from a public URL (FB CDN photos) for the vision lane. SSRF-guarded,
/// 8 MB cap, None on any failure.
pub async fn fetch_image_bytes(url: &str) -> Option<Vec<u8>> {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        use std::io::Read;
        ssrf_check(&url).ok()?;
        let mut buf = Vec::new();
        ureq::get(&url)
            .timeout(std::time::Duration::from_secs(30))
            .call()
            .ok()?
            .into_reader()
            .take(8_000_000)
            .read_to_end(&mut buf)
            .ok()?;
        if buf.len() < 1000 { None } else { Some(buf) }
    })
    .await
    .ok()?
}

/// Screenshot a rendered page (headless Chromium via snap_page.js) — JPEG bytes. SSRF-guarded like
/// every fetch path; None on any failure (block/timeout) so callers stay honest about having no image.
pub async fn screenshot_page(url: &str) -> Option<Vec<u8>> {
    let url = url.to_string();
    tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        ssrf_check(&url).ok()?;
        let script = std::env::var("YM_SNAP_SCRIPT").unwrap_or_else(|_| "/opt/yantrik-mind/snap_page.js".into());
        let dir = std::path::Path::new(&script).parent()?.to_path_buf();
        let out = std::env::temp_dir().join(format!(
            "ym_snap_{}_{}.jpg",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()
        ));
        let st = std::process::Command::new("timeout")
            .arg("60")
            .arg("node")
            .arg(&script)
            .arg(&url)
            .arg(out.to_str()?)
            .current_dir(&dir)
            .status()
            .ok()?;
        if !st.success() {
            let _ = std::fs::remove_file(&out);
            return None;
        }
        let bytes = std::fs::read(&out).ok()?;
        let _ = std::fs::remove_file(&out);
        if bytes.len() < 2000 {
            return None; // a blank/failed render, not a real page
        }
        Some(bytes)
    })
    .await
    .ok()?
}

/// Facebook Graph API (READ-ONLY, the user's own profile via their own app token). The "know me"
/// lane: profile facts, likes (interest mining), events (calendar spine), post cadence. The token
/// lives only in env (FB_USER_TOKEN); requests go out, nothing is ever posted.
pub struct FbClient {
    token: String,
}

impl FbClient {
    pub fn from_env() -> Option<FbClient> {
        std::env::var("FB_USER_TOKEN").ok().filter(|t| t.len() > 20).map(|token| FbClient { token })
    }

    async fn get(&self, path: &str, fields: &str, limit: usize) -> anyhow::Result<serde_json::Value> {
        let url = format!(
            "https://graph.facebook.com/v19.0/{path}?fields={}&limit={limit}&access_token={}",
            urlencoding::encode(fields),
            self.token
        );
        tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
            Ok(ureq::get(&url).timeout(std::time::Duration::from_secs(30)).call()?.into_json()?)
        })
        .await?
    }

    pub async fn profile(&self) -> anyhow::Result<serde_json::Value> {
        self.get("me", "name,birthday,hometown,location{name},email", 1).await
    }
    pub async fn likes(&self, limit: usize) -> anyhow::Result<serde_json::Value> {
        self.get("me/likes", "name,category", limit).await
    }
    pub async fn events(&self, limit: usize) -> anyhow::Result<serde_json::Value> {
        self.get("me/events", "name,start_time,place{name},rsvp_status", limit).await
    }
    pub async fn posts(&self, limit: usize) -> anyhow::Result<serde_json::Value> {
        self.get("me/posts", "message,created_time", limit).await
    }
    /// Photos of a given kind ("uploaded" | "tagged") with image URLs.
    pub async fn photos(&self, kind: &str, limit: usize) -> anyhow::Result<serde_json::Value> {
        let url = format!(
            "https://graph.facebook.com/v19.0/me/photos?type={kind}&fields=images&limit={limit}&access_token={}",
            self.token
        );
        tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
            Ok(ureq::get(&url).timeout(std::time::Duration::from_secs(30)).call()?.into_json()?)
        })
        .await?
    }
    /// Days until the token dies (Graph debug_token self-introspection). None if unknown.
    pub async fn days_to_expiry(&self) -> Option<i64> {
        let url = format!(
            "https://graph.facebook.com/v19.0/debug_token?input_token={t}&access_token={t}",
            t = self.token
        );
        let v: serde_json::Value = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
            Ok(ureq::get(&url).timeout(std::time::Duration::from_secs(30)).call()?.into_json()?)
        })
        .await
        .ok()?
        .ok()?;
        let exp = v["data"]["expires_at"].as_i64()?;
        if exp == 0 {
            return Some(9999); // never-expiring
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
        Some((exp - now) / 86_400)
    }
}

/// Immich (self-hosted photo server). The real "know my life in pictures" lane: its
/// LOCAL ML already named faces (Brishti, Aadrisha, ...) and indexed every asset — we just query it.
/// Env: IMMICH_SERVER (host or URL), IMMICH_USER_API_KEY (x-api-key). The ONLY writes ever
/// performed are person naming + merging — both driven by the user's own who-is-this answers
/// (opt-in granted 2026-07-02); nothing is ever deleted or uploaded.
pub struct ImmichClient {
    base: String,
    key: String,
}

impl ImmichClient {
    pub fn from_env() -> Option<ImmichClient> {
        let raw = std::env::var("IMMICH_SERVER").ok()?;
        let raw = raw.trim().trim_end_matches('/');
        if raw.is_empty() {
            return None;
        }
        let base = if raw.starts_with("http") { raw.to_string() } else { format!("http://{raw}") };
        let key = std::env::var("IMMICH_USER_API_KEY").ok().filter(|k| k.len() > 8)?;
        Some(ImmichClient { base, key })
    }

    fn get_blocking(base: &str, key: &str, path: &str) -> anyhow::Result<serde_json::Value> {
        Ok(ureq::get(&format!("{base}{path}"))
            .set("x-api-key", key)
            .timeout(std::time::Duration::from_secs(25))
            .call()?
            .into_json()?)
    }

    pub async fn people(&self) -> anyhow::Result<serde_json::Value> {
        let (b, k) = (self.base.clone(), self.key.clone());
        tokio::task::spawn_blocking(move || Self::get_blocking(&b, &k, "/api/people?withHidden=false")).await?
    }

    /// Photo assets (IMAGE only) for a recognized person, newest first. Returns Vec<(asset_id,
    /// date, place)> for the caller to thumbnail + vision-analyze.
    pub async fn assets_of_person(&self, person_id: &str, size: usize) -> anyhow::Result<Vec<(String, String, String)>> {
        let (b, k, pid) = (self.base.clone(), self.key.clone(), person_id.to_string());
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(String, String, String)>> {
            let body = serde_json::json!({ "personIds": [pid], "size": size, "type": "IMAGE", "withExif": true });
            let v: serde_json::Value = ureq::post(&format!("{b}/api/search/metadata"))
                .set("x-api-key", &k)
                .set("content-type", "application/json")
                .timeout(std::time::Duration::from_secs(30))
                .send_json(body)?
                .into_json()?;
            let mut out = Vec::new();
            for a in v["assets"]["items"].as_array().cloned().unwrap_or_default() {
                let id = a["id"].as_str().unwrap_or("").to_string();
                if id.is_empty() {
                    continue;
                }
                let date = a["fileCreatedAt"].as_str().unwrap_or("").chars().take(10).collect();
                let ex = &a["exifInfo"];
                let place = match (ex["city"].as_str(), ex["country"].as_str()) {
                    (Some(c), Some(co)) => format!("{c}, {co}"),
                    (Some(c), None) => c.to_string(),
                    _ => String::new(),
                };
                out.push((id, date, place));
            }
            Ok(out)
        })
        .await?
    }

    /// A downscaled thumbnail JPEG for an asset (perfect + cheap for a vision model).
    pub async fn thumbnail(&self, asset_id: &str) -> Option<Vec<u8>> {
        let (b, k, id) = (self.base.clone(), self.key.clone(), asset_id.to_string());
        tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
            use std::io::Read;
            let mut buf = Vec::new();
            ureq::get(&format!("{b}/api/assets/{id}/thumbnail?size=preview"))
                .set("x-api-key", &k)
                .timeout(std::time::Duration::from_secs(25))
                .call()
                .ok()?
                .into_reader()
                .take(8_000_000)
                .read_to_end(&mut buf)
                .ok()?;
            if buf.len() < 500 { None } else { Some(buf) }
        })
        .await
        .ok()?
    }
}

/// ---------- PHOTO-SOURCE PLUGIN LAYER (acquisition) ----------
/// HOW images arrive is a plugin concern, decoupled from WHAT the mind does with them (the
/// understanding layer in mind-conversation: pattern learning, retrieval, ask-who-is-who). Each
/// connected service is one arm of this enum; adding Google Photos / OneDrive / a phone camera
/// roll later is a new arm + from_env wiring — the learning layer above never changes.
pub struct PhotoPerson {
    pub id: String,
    /// "" = the source clustered this face but has no name yet (ask-who-is-who fuel).
    pub name: String,
}

#[derive(Clone)]
pub struct PhotoAsset {
    /// Source-native id (Immich asset id; FB uses the image URL itself).
    pub id: String,
    /// YYYY-MM-DD when known.
    pub date: String,
    /// EXIF city/country when known.
    pub place: String,
}

pub enum PhotoSource {
    Immich(ImmichClient),
    Facebook(FbClient),
}

impl PhotoSource {
    /// Every configured source, moat-first (face-aware Immich before generic FB).
    pub fn all_from_env() -> Vec<PhotoSource> {
        let mut v = Vec::new();
        if let Some(im) = ImmichClient::from_env() {
            v.push(PhotoSource::Immich(im));
        }
        if let Some(fb) = FbClient::from_env() {
            v.push(PhotoSource::Facebook(fb));
        }
        v
    }

    pub fn name(&self) -> &'static str {
        match self {
            PhotoSource::Immich(_) => "immich",
            PhotoSource::Facebook(_) => "facebook",
        }
    }

    /// Whether this source knows WHO is in a photo (face recognition). Immich yes; FB's API never
    /// says (post-2018 lockdown) — that asymmetry is why per-person reads route to face-aware sources.
    pub fn knows_people(&self) -> bool {
        matches!(self, PhotoSource::Immich(_))
    }

    /// Face-clustered people (named + unnamed) — empty for sources without face data.
    pub async fn list_people(&self) -> Vec<PhotoPerson> {
        match self {
            PhotoSource::Immich(im) => im
                .people()
                .await
                .ok()
                .and_then(|v| v["people"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter_map(|p| {
                    Some(PhotoPerson {
                        id: p["id"].as_str()?.to_string(),
                        name: p["name"].as_str().unwrap_or("").trim().to_string(),
                    })
                })
                .collect(),
            PhotoSource::Facebook(_) => Vec::new(),
        }
    }

    pub async fn assets_of_person(&self, person_id: &str, n: usize) -> Vec<PhotoAsset> {
        match self {
            PhotoSource::Immich(im) => im
                .assets_of_person(person_id, n)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, date, place)| PhotoAsset { id, date, place })
                .collect(),
            PhotoSource::Facebook(_) => Vec::new(),
        }
    }

    /// Recent photos regardless of who's in them — the generic sweep every source can do.
    pub async fn recent_assets(&self, n: usize) -> Vec<PhotoAsset> {
        match self {
            PhotoSource::Immich(im) => im
                .recent_assets(n)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, date, place)| PhotoAsset { id, date, place })
                .collect(),
            PhotoSource::Facebook(fb) => {
                let mut out: Vec<PhotoAsset> = Vec::new();
                for kind in ["uploaded", "tagged"] {
                    if let Ok(r) = fb.photos(kind, n).await {
                        for p in r.get("data").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                            if let Some(src) = p["images"].as_array().and_then(|a| a.first()).and_then(|im| im["source"].as_str()) {
                                if !out.iter().any(|a| a.id == src) {
                                    out.push(PhotoAsset { id: src.to_string(), date: String::new(), place: String::new() });
                                }
                            }
                        }
                    }
                }
                out.truncate(n);
                out
            }
        }
    }

    /// Whether the source can search photos by MEANING (CLIP) — "wedding", "beach sunset".
    pub fn supports_search(&self) -> bool {
        matches!(self, PhotoSource::Immich(_))
    }

    /// Semantic search over the whole archive, optionally filtered to people (AND).
    pub async fn search(&self, query: &str, person_ids: &[String], n: usize) -> Vec<PhotoAsset> {
        match self {
            PhotoSource::Immich(im) => im
                .smart_search(query, person_ids, n)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, date, place)| PhotoAsset { id, date, place })
                .collect(),
            PhotoSource::Facebook(_) => Vec::new(),
        }
    }

    /// Photos containing ALL the given people (couple/group shots).
    pub async fn assets_of_people(&self, person_ids: &[String], n: usize, oldest_first: bool) -> Vec<PhotoAsset> {
        match self {
            PhotoSource::Immich(im) => im
                .assets_of_people(person_ids, n, oldest_first)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, date, place)| PhotoAsset { id, date, place })
                .collect(),
            PhotoSource::Facebook(_) => Vec::new(),
        }
    }

    /// The image bytes for an asset (downscaled where the source supports it).
    pub async fn image_bytes(&self, asset: &PhotoAsset) -> Option<Vec<u8>> {
        match self {
            PhotoSource::Immich(im) => im.thumbnail(&asset.id).await,
            PhotoSource::Facebook(_) => fetch_image_bytes(&asset.id).await,
        }
    }

    /// A face-crop thumbnail for a person cluster (who-is-this questions). Face-aware sources only.
    pub async fn face_thumbnail(&self, person_id: &str) -> Option<Vec<u8>> {
        match self {
            PhotoSource::Immich(im) => im.person_thumbnail(person_id).await,
            PhotoSource::Facebook(_) => None,
        }
    }

    /// WHO is in one image (named people + count of unrecognized faces) from the source's saved
    /// face data — the "from an image, derive who is who" primitive.
    pub async fn people_in(&self, asset_id: &str) -> (Vec<String>, usize) {
        match self {
            PhotoSource::Immich(im) => im.people_in_asset(asset_id).await,
            PhotoSource::Facebook(_) => (Vec::new(), 0),
        }
    }

    /// Assets taken in a date range (face-aware sources; empty elsewhere).
    pub async fn taken_between(&self, after: &str, before: &str, person_ids: &[String], n: usize) -> Vec<PhotoAsset> {
        match self {
            PhotoSource::Immich(im) => im
                .taken_between(after, before, person_ids, n)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(id, date, place)| PhotoAsset { id, date, place })
                .collect(),
            PhotoSource::Facebook(_) => Vec::new(),
        }
    }

    /// One person's face box in one asset (normalized 0..1 + pixel width), from saved face data.
    pub async fn face_box(&self, asset_id: &str, person_id: &str) -> Option<(f32, f32, f32, f32, f32)> {
        match self {
            PhotoSource::Immich(im) => im.face_box(asset_id, person_id).await,
            PhotoSource::Facebook(_) => None,
        }
    }

    /// Write a name back to the source's face cluster (sources that support it). Human-driven only:
    /// this fires exclusively from the user's own who-is-this answers.
    pub async fn name_person(&self, person_id: &str, name: &str) -> bool {
        match self {
            PhotoSource::Immich(im) => im.rename_person(person_id, name).await,
            PhotoSource::Facebook(_) => false,
        }
    }

    /// Merge clusters into a target person (the user said they're the same human).
    pub async fn merge_people(&self, target_id: &str, source_ids: &[String]) -> bool {
        match self {
            PhotoSource::Immich(im) => im.merge_person(target_id, source_ids).await,
            PhotoSource::Facebook(_) => false,
        }
    }

    /// How many photos a person cluster appears in (ranks which unknown face is worth asking about).
    pub async fn person_photo_count(&self, person_id: &str) -> Option<u64> {
        match self {
            PhotoSource::Immich(im) => im.person_stats(person_id).await,
            PhotoSource::Facebook(_) => None,
        }
    }
}

impl ImmichClient {
    /// Asset count for a person (GET /api/people/{id}/statistics — needs the statistics permission).
    pub async fn person_stats(&self, person_id: &str) -> Option<u64> {
        let (b, k, id) = (self.base.clone(), self.key.clone(), person_id.to_string());
        tokio::task::spawn_blocking(move || -> Option<u64> {
            let v = Self::get_blocking(&b, &k, &format!("/api/people/{id}/statistics")).ok()?;
            v["assets"].as_u64()
        })
        .await
        .ok()?
    }

    /// The face-crop thumbnail Immich shows for a person cluster.
    pub async fn person_thumbnail(&self, person_id: &str) -> Option<Vec<u8>> {
        let (b, k, id) = (self.base.clone(), self.key.clone(), person_id.to_string());
        tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
            use std::io::Read;
            let mut buf = Vec::new();
            ureq::get(&format!("{b}/api/people/{id}/thumbnail"))
                .set("x-api-key", &k)
                .timeout(std::time::Duration::from_secs(25))
                .call()
                .ok()?
                .into_reader()
                .take(4_000_000)
                .read_to_end(&mut buf)
                .ok()?;
            if buf.len() < 300 { None } else { Some(buf) }
        })
        .await
        .ok()?
    }

    /// Semantic (CLIP) search over the whole library, optionally person-filtered. This is the
    /// "our wedding" lane: meaning-based recall across years of photos, not just recency.
    pub async fn smart_search(&self, query: &str, person_ids: &[String], size: usize) -> anyhow::Result<Vec<(String, String, String)>> {
        let (b, k, q, pids) = (self.base.clone(), self.key.clone(), query.to_string(), person_ids.to_vec());
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(String, String, String)>> {
            let mut body = serde_json::json!({ "query": q, "size": size, "type": "IMAGE", "withExif": true });
            if !pids.is_empty() {
                body["personIds"] = serde_json::json!(pids);
            }
            let v: serde_json::Value = ureq::post(&format!("{b}/api/search/smart"))
                .set("x-api-key", &k)
                .set("content-type", "application/json")
                .timeout(std::time::Duration::from_secs(60))
                .send_json(body)?
                .into_json()?;
            Ok(Self::parse_asset_items(&v))
        })
        .await?
    }

    /// Photo assets containing ALL the given people (AND filter) — couples/groups.
    pub async fn assets_of_people(&self, person_ids: &[String], size: usize, oldest_first: bool) -> anyhow::Result<Vec<(String, String, String)>> {
        let (b, k, pids) = (self.base.clone(), self.key.clone(), person_ids.to_vec());
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(String, String, String)>> {
            let body = serde_json::json!({ "personIds": pids, "size": size, "type": "IMAGE", "withExif": true, "order": if oldest_first { "asc" } else { "desc" } });
            let v: serde_json::Value = ureq::post(&format!("{b}/api/search/metadata"))
                .set("x-api-key", &k)
                .set("content-type", "application/json")
                .timeout(std::time::Duration::from_secs(30))
                .send_json(body)?
                .into_json()?;
            Ok(Self::parse_asset_items(&v))
        })
        .await?
    }

    /// WHO is in one image, from the saved face data (GET /api/faces?id=<asset>): named people
    /// first, plus a count of faces the library hasn't identified yet.
    pub async fn people_in_asset(&self, asset_id: &str) -> (Vec<String>, usize) {
        let (b, k, id) = (self.base.clone(), self.key.clone(), asset_id.to_string());
        tokio::task::spawn_blocking(move || -> (Vec<String>, usize) {
            let Ok(v) = Self::get_blocking(&b, &k, &format!("/api/faces?id={id}")) else {
                return (Vec::new(), 0);
            };
            let mut names: Vec<String> = Vec::new();
            let mut unknown = 0usize;
            for face in v.as_array().cloned().unwrap_or_default() {
                let name = face["person"]["name"].as_str().unwrap_or("").trim().to_string();
                if name.is_empty() {
                    unknown += 1;
                } else if !names.contains(&name) {
                    names.push(name);
                }
            }
            (names, unknown)
        })
        .await
        .unwrap_or((Vec::new(), 0))
    }

    /// Assets taken in a date range (ISO strings), optionally filtered to people — powers the
    /// month-by-month reel walk and "on this day N years ago".
    pub async fn taken_between(&self, after: &str, before: &str, person_ids: &[String], size: usize) -> anyhow::Result<Vec<(String, String, String)>> {
        let (b, k, af, bf, pids) = (self.base.clone(), self.key.clone(), after.to_string(), before.to_string(), person_ids.to_vec());
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(String, String, String)>> {
            let mut body = serde_json::json!({ "takenAfter": af, "takenBefore": bf, "size": size, "type": "IMAGE", "withExif": true });
            if !pids.is_empty() {
                body["personIds"] = serde_json::json!(pids);
            }
            let v: serde_json::Value = ureq::post(&format!("{b}/api/search/metadata"))
                .set("x-api-key", &k)
                .set("content-type", "application/json")
                .timeout(std::time::Duration::from_secs(30))
                .send_json(body)?
                .into_json()?;
            Ok(Self::parse_asset_items(&v))
        })
        .await?
    }

    /// The bounding box of ONE person's face in ONE asset, normalized to 0..1 — plus the face's
    /// pixel width in the original image (a quality floor for reel frames). Largest match wins.
    pub async fn face_box(&self, asset_id: &str, person_id: &str) -> Option<(f32, f32, f32, f32, f32)> {
        let (b, k, aid, pid) = (self.base.clone(), self.key.clone(), asset_id.to_string(), person_id.to_string());
        tokio::task::spawn_blocking(move || -> Option<(f32, f32, f32, f32, f32)> {
            let v = Self::get_blocking(&b, &k, &format!("/api/faces?id={aid}")).ok()?;
            let mut best: Option<(f32, f32, f32, f32, f32)> = None;
            for face in v.as_array().cloned().unwrap_or_default() {
                if face["person"]["id"].as_str() != Some(pid.as_str()) {
                    continue;
                }
                let (w, h) = (face["imageWidth"].as_f64()? as f32, face["imageHeight"].as_f64()? as f32);
                if w < 1.0 || h < 1.0 {
                    continue;
                }
                let (x1, y1, x2, y2) = (
                    face["boundingBoxX1"].as_f64().unwrap_or(0.0) as f32,
                    face["boundingBoxY1"].as_f64().unwrap_or(0.0) as f32,
                    face["boundingBoxX2"].as_f64().unwrap_or(0.0) as f32,
                    face["boundingBoxY2"].as_f64().unwrap_or(0.0) as f32,
                );
                let pxw = x2 - x1;
                if best.as_ref().map_or(true, |b| pxw > b.4) {
                    best = Some((x1 / w, y1 / h, x2 / w, y2 / h, pxw));
                }
            }
            best
        })
        .await
        .ok()?
    }

    /// Shared response walk: (asset_id, YYYY-MM-DD, "City, Country") triples.
    fn parse_asset_items(v: &serde_json::Value) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        for a in v["assets"]["items"].as_array().cloned().unwrap_or_default() {
            let id = a["id"].as_str().unwrap_or("").to_string();
            if id.is_empty() {
                continue;
            }
            let date = a["fileCreatedAt"].as_str().unwrap_or("").chars().take(10).collect();
            let ex = &a["exifInfo"];
            let place = match (ex["city"].as_str(), ex["country"].as_str()) {
                (Some(c), Some(co)) => format!("{c}, {co}"),
                (Some(c), None) => c.to_string(),
                _ => String::new(),
            };
            out.push((id, date, place));
        }
        out
    }

    /// Name a face cluster (PUT /api/people/{id}) — the who-is-who write-back.
    pub async fn rename_person(&self, person_id: &str, name: &str) -> bool {
        let (b, k, id, nm) = (self.base.clone(), self.key.clone(), person_id.to_string(), name.to_string());
        tokio::task::spawn_blocking(move || -> bool {
            ureq::put(&format!("{b}/api/people/{id}"))
                .set("x-api-key", &k)
                .set("content-type", "application/json")
                .timeout(std::time::Duration::from_secs(25))
                .send_json(serde_json::json!({ "name": nm }))
                .is_ok()
        })
        .await
        .unwrap_or(false)
    }

    /// Merge face clusters INTO a target person (POST /api/people/{id}/merge) — an unnamed cluster
    /// the user identified as an already-named person folds into them (stronger recognition anchor).
    pub async fn merge_person(&self, target_id: &str, source_ids: &[String]) -> bool {
        let (b, k, id, ids) = (self.base.clone(), self.key.clone(), target_id.to_string(), source_ids.to_vec());
        tokio::task::spawn_blocking(move || -> bool {
            ureq::post(&format!("{b}/api/people/{id}/merge"))
                .set("x-api-key", &k)
                .set("content-type", "application/json")
                .timeout(std::time::Duration::from_secs(25))
                .send_json(serde_json::json!({ "ids": ids }))
                .is_ok()
        })
        .await
        .unwrap_or(false)
    }

    /// Recent IMAGE assets regardless of person — same shape as assets_of_person, no filter.
    pub async fn recent_assets(&self, n: usize) -> anyhow::Result<Vec<(String, String, String)>> {
        let (b, k) = (self.base.clone(), self.key.clone());
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(String, String, String)>> {
            let body = serde_json::json!({ "size": n, "type": "IMAGE", "order": "desc", "withExif": true });
            let v: serde_json::Value = ureq::post(&format!("{b}/api/search/metadata"))
                .set("x-api-key", &k)
                .set("content-type", "application/json")
                .timeout(std::time::Duration::from_secs(30))
                .send_json(body)?
                .into_json()?;
            let mut out = Vec::new();
            for a in v["assets"]["items"].as_array().cloned().unwrap_or_default() {
                let id = a["id"].as_str().unwrap_or("").to_string();
                if id.is_empty() {
                    continue;
                }
                let date = a["fileCreatedAt"].as_str().unwrap_or("").chars().take(10).collect();
                let ex = &a["exifInfo"];
                let place = match (ex["city"].as_str(), ex["country"].as_str()) {
                    (Some(c), Some(co)) => format!("{c}, {co}"),
                    (Some(c), None) => c.to_string(),
                    _ => String::new(),
                };
                out.push((id, date, place));
            }
            Ok(out)
        })
        .await?
    }
}

/// Render a "growing up" reel, cinematic cut: each face-centered frame becomes a ~0.87s clip with
/// a slow Ken Burns zoom and a month/year label, crossfaded into the next — 720p30 H.264. Frames
/// arrive as (image bytes, normalized face box, label). None if too few frames or encoding fails.
pub async fn face_reel_video(frames: Vec<(Vec<u8>, (f32, f32, f32, f32), String)>) -> Option<Vec<u8>> {
    tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        let tag = format!("{}_{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_millis());
        let dir = std::env::temp_dir().join(format!("ym_reel_{tag}"));
        std::fs::create_dir_all(&dir).ok()?;
        let mut labels: Vec<String> = Vec::new();
        for (bytes, (nx1, ny1, nx2, ny2), label) in &frames {
            let Ok(img) = image::load_from_memory(bytes) else { continue };
            let (w, h) = (img.width() as f32, img.height() as f32);
            let (x1, y1, x2, y2) = (nx1 * w, ny1 * h, nx2 * w, ny2 * h);
            let (cx, cy) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
            let side = ((x2 - x1).max(y2 - y1) * 1.9).min(w.min(h)).max(32.0);
            let half = side / 2.0;
            let left = (cx - half).clamp(0.0, (w - side).max(0.0));
            let top = (cy - half).clamp(0.0, (h - side).max(0.0));
            let crop = img.crop_imm(left as u32, top as u32, side as u32, side as u32);
            let frame = crop.resize_exact(720, 720, image::imageops::FilterType::Triangle);
            if frame.to_rgb8().save(dir.join(format!("f_{:04}.jpg", labels.len()))).is_ok() {
                labels.push(label.clone());
            }
        }
        let n = labels.len();
        if n < 6 {
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        // Cinematic filtergraph: per-clip Ken Burns zoom + label, then a crossfade chain.
        let clip_frames = 26; // @30fps ≈ 0.87s per month
        let clip = clip_frames as f32 / 30.0;
        let fade = 0.35f32;
        let adv = clip - fade;
        let font = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf";
        let have_font = std::path::Path::new(font).exists();
        let mut fg = String::new();
        for (i, label) in labels.iter().enumerate() {
            let dt = if have_font {
                format!(
                    ",drawtext=fontfile={font}:text='{}':x=36:y=h-84:fontsize=40:fontcolor=white@0.9:box=1:boxcolor=black@0.28:boxborderw=10",
                    label.replace('\'', "")
                )
            } else {
                String::new()
            };
            fg.push_str(&format!(
                "[{i}:v]zoompan=z='min(zoom+0.0016,1.10)':d={clip_frames}:x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':s=720x720:fps=30{dt}[v{i}];\n"
            ));
        }
        let mut prev = "v0".to_string();
        for k in 1..n {
            let out_lbl = format!("x{k}");
            fg.push_str(&format!(
                "[{prev}][v{k}]xfade=transition=fade:duration={fade}:offset={:.3}[{out_lbl}];\n",
                adv * k as f32
            ));
            prev = out_lbl;
        }
        let fg = fg.trim_end().trim_end_matches(';').to_string();
        let script = dir.join("fg.txt");
        std::fs::write(&script, &fg).ok()?;
        let out = dir.join("reel.mp4");
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.arg("-y").arg("-loglevel").arg("error");
        for i in 0..n {
            cmd.arg("-i").arg(dir.join(format!("f_{:04}.jpg", i)));
        }
        cmd.arg("-filter_complex_script")
            .arg(&script)
            .arg("-map")
            .arg(format!("[{prev}]"))
            .arg("-c:v")
            .arg("libx264")
            .arg("-preset")
            .arg("veryfast")
            .arg("-crf")
            .arg("21")
            .arg("-pix_fmt")
            .arg("yuv420p")
            .arg("-movflags")
            .arg("+faststart")
            .arg(&out);
        let st = cmd.status().ok()?;
        let bytes = if st.success() { std::fs::read(&out).ok() } else { None };
        let _ = std::fs::remove_dir_all(&dir);
        bytes.filter(|b| b.len() > 10_000)
    })
    .await
    .ok()?
}

/// Photo enhancement (local, instant): auto = saturation lift + contrast + gentle sharpen; plus
/// bw/warm/bright modes. Classical ops that give a real "pop" — honest about not being generative.
pub async fn enhance_photo(bytes: Vec<u8>, mode: &str) -> Option<Vec<u8>> {
    let mode = mode.to_string();
    tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        let img = image::load_from_memory(&bytes).ok()?;
        let out: image::DynamicImage = match mode.as_str() {
            "bw" => img.grayscale().adjust_contrast(14.0),
            "bright" => img.brighten(20).adjust_contrast(6.0),
            "warm" => {
                let mut rgb = img.to_rgb8();
                for p in rgb.pixels_mut() {
                    p.0[0] = (p.0[0] as f32 * 1.07).min(255.0) as u8;
                    p.0[2] = (p.0[2] as f32 * 0.93) as u8;
                }
                image::DynamicImage::ImageRgb8(rgb).brighten(5)
            }
            _ => {
                let mut rgb = img.to_rgb8();
                for p in rgb.pixels_mut() {
                    let l = 0.299 * p.0[0] as f32 + 0.587 * p.0[1] as f32 + 0.114 * p.0[2] as f32;
                    for c in 0..3 {
                        let v = l + (p.0[c] as f32 - l) * 1.22;
                        p.0[c] = v.clamp(0.0, 255.0) as u8;
                    }
                }
                image::DynamicImage::ImageRgb8(rgb).adjust_contrast(9.0).brighten(4).unsharpen(1.2, 3)
            }
        };
        let mut buf = std::io::Cursor::new(Vec::new());
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 92);
        out.write_with_encoder(enc).ok()?;
        Some(buf.into_inner())
    })
    .await
    .ok()?
}

/// Technical quality read: (sharpness = Laplacian variance, mean luma, contrast = luma stddev)
/// on a 256px grayscale downscale. Free and instant — kills blurry/dark/blown frames before any
/// model spends time on them. Rough thresholds: sharpness <30 blurry; luma <35 dark, >220 blown.
pub fn photo_quality(bytes: &[u8]) -> Option<(f32, f32, f32)> {
    let img = image::load_from_memory(bytes).ok()?;
    let g = img.resize(256, 256, image::imageops::FilterType::Triangle).to_luma8();
    let (w, h) = g.dimensions();
    if w < 8 || h < 8 {
        return None;
    }
    let (mut sum, mut sum2, mut n) = (0f64, 0f64, 0f64);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = g.get_pixel(x, y).0[0] as f64;
            let lap = 4.0 * c
                - g.get_pixel(x - 1, y).0[0] as f64
                - g.get_pixel(x + 1, y).0[0] as f64
                - g.get_pixel(x, y - 1).0[0] as f64
                - g.get_pixel(x, y + 1).0[0] as f64;
            sum += lap;
            sum2 += lap * lap;
            n += 1.0;
        }
    }
    let mean = sum / n;
    let sharpness = ((sum2 / n) - mean * mean).max(0.0) as f32;
    let (mut ls, mut ls2) = (0f64, 0f64);
    let ln = (w * h) as f64;
    for p in g.pixels() {
        let v = p.0[0] as f64;
        ls += v;
        ls2 += v * v;
    }
    let lmean = ls / ln;
    let contrast = ((ls2 / ln) - lmean * lmean).max(0.0).sqrt() as f32;
    Some((sharpness, lmean as f32, contrast))
}

/// Gentle per-cell polish: nudge brightness toward the grid's target luma (cohesion), light
/// contrast + saturation lift, mild unsharp. Deliberately subtler than `enhance_photo` — a clean
/// album look, not an Instagram filter.
fn polish_cell(img: &image::RgbImage, target_luma: f32) -> image::RgbImage {
    let n = (img.width() * img.height()).max(1) as f32;
    let mean: f32 = img
        .pixels()
        .map(|p| 0.299 * p.0[0] as f32 + 0.587 * p.0[1] as f32 + 0.114 * p.0[2] as f32)
        .sum::<f32>()
        / n;
    let delta = ((target_luma - mean) * 0.5).clamp(-22.0, 22.0) as i32;
    let mut out = image::imageops::colorops::brighten(img, delta);
    image::imageops::colorops::contrast_in_place(&mut out, 7.0);
    for p in out.pixels_mut() {
        let l = 0.299 * p.0[0] as f32 + 0.587 * p.0[1] as f32 + 0.114 * p.0[2] as f32;
        for c in 0..3 {
            p.0[c] = (l + (p.0[c] as f32 - l) * 1.10).clamp(0.0, 255.0) as u8;
        }
    }
    image::imageops::unsharpen(&out, 0.8, 2)
}

/// Compose a collage: face-centered square cells (when a face box is known) on a warm gutter grid.
/// 2→2x1, 3→3x1, 4→2x2, 5-6→3x2, 7+→3x3. Returns JPEG bytes.
pub async fn make_collage(cells: Vec<(Vec<u8>, Option<(f32, f32, f32, f32)>)>) -> Option<Vec<u8>> {
    tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        let n = cells.len().min(9);
        if n < 2 {
            return None;
        }
        let (cols, rows) = match n {
            2 => (2u32, 1u32),
            3 => (3, 1),
            4 => (2, 2),
            5 | 6 => (3, 2),
            _ => (3, 3),
        };
        let n_used = (cols * rows) as usize;
        let cell = 512u32;
        let gut = 10u32;
        let w = cols * cell + (cols + 1) * gut;
        let h = rows * cell + (rows + 1) * gut;
        let mut canvas = image::RgbImage::from_pixel(w, h, image::Rgb([246, 242, 236]));
        // Phase 1: face-centered crops.
        let mut crops: Vec<image::RgbImage> = Vec::new();
        for (bytes, bbox) in cells.iter().take(n_used) {
            let Ok(img) = image::load_from_memory(bytes) else { continue };
            let (iw, ih) = (img.width() as f32, img.height() as f32);
            let (cx, cy, side) = match bbox {
                Some((x1, y1, x2, y2)) => {
                    let fx = (x1 + x2) / 2.0 * iw;
                    let fy = (y1 + y2) / 2.0 * ih;
                    // Wider than the reel crop — collages want outfit/body context, not just the face.
                    let s = ((x2 - x1) * iw).max((y2 - y1) * ih) * 3.4;
                    (fx, fy, s.min(iw.min(ih)).max(64.0))
                }
                None => (iw / 2.0, ih / 2.0, iw.min(ih)),
            };
            let half = side / 2.0;
            let left = (cx - half).clamp(0.0, (iw - side).max(0.0));
            let top = (cy - half).clamp(0.0, (ih - side).max(0.0));
            let crop = img
                .crop_imm(left as u32, top as u32, side as u32, side as u32)
                .resize_exact(cell, cell, image::imageops::FilterType::Triangle)
                .to_rgb8();
            crops.push(crop);
        }
        // Phase 2: polish every cell toward the grid's median luma — one cohesive album page,
        // not nine mismatched exposures.
        let mut lumas: Vec<f32> = crops
            .iter()
            .map(|c| {
                let n = (c.width() * c.height()).max(1) as f32;
                c.pixels().map(|p| 0.299 * p.0[0] as f32 + 0.587 * p.0[1] as f32 + 0.114 * p.0[2] as f32).sum::<f32>() / n
            })
            .collect();
        lumas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let target = lumas.get(lumas.len() / 2).copied().unwrap_or(128.0).clamp(95.0, 165.0);
        let mut placed = 0u32;
        for crop in &crops {
            let polished = polish_cell(crop, target);
            let ox = gut + (placed % cols) * (cell + gut);
            let oy = gut + (placed / cols) * (cell + gut);
            image::imageops::overlay(&mut canvas, &polished, ox as i64, oy as i64);
            placed += 1;
        }
        if placed < 2 {
            return None;
        }
        let mut buf = std::io::Cursor::new(Vec::new());
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 90);
        image::DynamicImage::ImageRgb8(canvas).write_with_encoder(enc).ok()?;
        Some(buf.into_inner())
    })
    .await
    .ok()?
}

/// ---------- FACE ENGINE (our own recognition) ----------
/// The embedder is a self-hosted stateless function (YM_FACE_ML_URL — currently the repaired
/// Immich ML container; swappable for any InsightFace-compatible service). The GALLERY — who these
/// embeddings belong to — lives in OUR substrate, so identity survives any third-party system.
pub struct FaceEngine {
    url: String,
}

/// One detected face: normalized box (0..1), detection score, 512-d embedding.
pub struct DetectedFace {
    pub bbox: (f32, f32, f32, f32),
    pub score: f32,
    pub embedding: Vec<f32>,
}

impl FaceEngine {
    pub fn from_env() -> Option<FaceEngine> {
        let url = std::env::var("YM_FACE_ML_URL").ok().filter(|u| !u.trim().is_empty())?;
        Some(FaceEngine { url: url.trim().trim_end_matches('/').to_string() })
    }

    /// Detect + embed every face in an image (hand-built multipart — ureq has none).
    pub async fn faces(&self, image: Vec<u8>) -> anyhow::Result<Vec<DetectedFace>> {
        let url = format!("{}/predict", self.url);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<DetectedFace>> {
            let boundary = "ym-face-boundary-7c1";
            let entries = r#"{"facial-recognition":{"detection":{"modelName":"buffalo_l","options":{"minScore":0.5}},"recognition":{"modelName":"buffalo_l"}}}"#;
            let mut body: Vec<u8> = Vec::new();
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"entries\"\r\n\r\n{entries}\r\n").as_bytes());
            body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"img.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n").as_bytes());
            body.extend_from_slice(&image);
            body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
            let resp: serde_json::Value = ureq::post(&url)
                .set("content-type", &format!("multipart/form-data; boundary={boundary}"))
                .timeout(std::time::Duration::from_secs(90))
                .send_bytes(&body)?
                .into_json()?;
            let (w, h) = (
                resp["imageWidth"].as_f64().unwrap_or(1.0) as f32,
                resp["imageHeight"].as_f64().unwrap_or(1.0) as f32,
            );
            let mut out = Vec::new();
            for face in resp["facial-recognition"].as_array().cloned().unwrap_or_default() {
                let b = &face["boundingBox"];
                let bbox = (
                    b["x1"].as_f64().unwrap_or(0.0) as f32 / w.max(1.0),
                    b["y1"].as_f64().unwrap_or(0.0) as f32 / h.max(1.0),
                    b["x2"].as_f64().unwrap_or(0.0) as f32 / w.max(1.0),
                    b["y2"].as_f64().unwrap_or(0.0) as f32 / h.max(1.0),
                );
                let score = face["score"].as_f64().unwrap_or(0.0) as f32;
                // The embedding arrives either as a JSON array or as a stringified array.
                let embedding: Vec<f32> = match &face["embedding"] {
                    serde_json::Value::Array(a) => a.iter().filter_map(|v| v.as_f64().map(|x| x as f32)).collect(),
                    serde_json::Value::String(st) => serde_json::from_str::<Vec<f32>>(st).unwrap_or_default(),
                    _ => Vec::new(),
                };
                if embedding.len() >= 128 {
                    out.push(DetectedFace { bbox, score, embedding });
                }
            }
            Ok(out)
        })
        .await?
    }
}

/// Cosine similarity between two vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f32, 0f32, 0f32);
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

