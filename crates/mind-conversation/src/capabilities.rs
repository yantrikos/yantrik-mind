//! Small ported capabilities — web/utility domains routed by the registry instead of match arms.
//! Each preserves its old arm's guards exactly: a guard that failed used to fall through the
//! match, so here it returns None and the legacy fallback answers as before. Domains with a
//! fuller life (finance, home, news) keep their capability next to their domain code instead.

use serde_json::Value;

use crate::plugins::CapabilityHandler;
use crate::ConversationEngine;

fn arg(args: &Value, k: &str) -> String {
    args.get(k).and_then(|x| x.as_str()).unwrap_or("").trim().to_string()
}

/// Web search — discovery, then web_fetch reads.
pub struct WebSearchCapability;

#[async_trait::async_trait]
impl CapabilityHandler for WebSearchCapability {
    fn id(&self) -> &'static str {
        "web_search"
    }

    async fn handle_command(&self, host: &ConversationEngine, cmd: &str, rest: &str) -> Option<String> {
        match cmd {
            "search" | "google" | "ddg" if !rest.is_empty() => Some(host.run_agent_tool("search", &serde_json::json!({ "query": rest })).await),
            _ => None,
        }
    }

    async fn handle_tool(&self, host: &ConversationEngine, tool: &str, args: &Value) -> Option<String> {
        Some(match tool {
            "search" | "web_search" => match &host.searcher {
                Some(se) => {
                    let q = { let a = arg(args, "query"); if a.is_empty() { arg(args, "q") } else { a } };
                    if q.len() < 2 {
                        return Some("(what should I search for?)".to_string());
                    }
                    match se.search(&q, 6).await {
                        Ok(hits) => mind_tools::render_search(&hits),
                        Err(e) => format!("(search error: {e})"),
                    }
                }
                None => "(search not configured)".to_string(),
            },
            _ => return None,
        })
    }
}

/// Web fetch — read one page.
pub struct WebFetchCapability;

#[async_trait::async_trait]
impl CapabilityHandler for WebFetchCapability {
    fn id(&self) -> &'static str {
        "web_fetch"
    }

    async fn handle_command(&self, host: &ConversationEngine, cmd: &str, rest: &str) -> Option<String> {
        match cmd {
            "web" | "fetch" if host.web.is_some() && !rest.is_empty() => Some(host.run_agent_tool("web_fetch", &serde_json::json!({ "url": rest })).await),
            _ => None,
        }
    }

    async fn handle_tool(&self, host: &ConversationEngine, tool: &str, args: &Value) -> Option<String> {
        Some(match tool {
            "web_fetch" => match &host.web {
                Some(w) => {
                    // A weak model often passes a messy url ("https://x.com and tell me…"); extract the
                    // first real http(s) url from whatever it gave so ureq doesn't choke (IdnaError).
                    let raw = arg(args, "url");
                    let url = mind_tools::first_url(&raw).unwrap_or(raw);
                    match w.fetch(&url).await { Ok(t) => t.chars().take(6000).collect(), Err(e) => format!("(fetch error: {e})") }
                }
                None => "(web not configured)".to_string(),
            },
            _ => return None,
        })
    }
}

/// Weather — current conditions + today's forecast.
pub struct WeatherCapability;

#[async_trait::async_trait]
impl CapabilityHandler for WeatherCapability {
    fn id(&self) -> &'static str {
        "weather"
    }

    async fn handle_command(&self, host: &ConversationEngine, cmd: &str, rest: &str) -> Option<String> {
        match cmd {
            "weather" | "wx" if !rest.is_empty() => Some(host.run_agent_tool("weather", &serde_json::json!({ "place": rest })).await),
            _ => None,
        }
    }

    async fn handle_tool(&self, host: &ConversationEngine, tool: &str, args: &Value) -> Option<String> {
        Some(match tool {
            "weather" => match &host.weather {
                Some(w) => match w.report(&{ let p = arg(args, "place"); if p.is_empty() { arg(args, "city") } else { p } }).await { Ok(r) => r, Err(e) => format!("(weather: {e})") },
                None => "(weather isn't configured)".to_string(),
            },
            _ => return None,
        })
    }
}

/// Wikipedia — factual summaries.
pub struct WikipediaCapability;

#[async_trait::async_trait]
impl CapabilityHandler for WikipediaCapability {
    fn id(&self) -> &'static str {
        "wikipedia"
    }

    async fn handle_command(&self, host: &ConversationEngine, cmd: &str, rest: &str) -> Option<String> {
        match cmd {
            "wiki" | "wikipedia" if !rest.is_empty() => Some(host.run_agent_tool("wikipedia", &serde_json::json!({ "query": rest })).await),
            _ => None,
        }
    }

    async fn handle_tool(&self, host: &ConversationEngine, tool: &str, args: &Value) -> Option<String> {
        Some(match tool {
            "wikipedia" | "wiki" => match &host.wiki {
                Some(w) => match w.lookup(&{ let q = arg(args, "query"); if q.is_empty() { arg(args, "topic") } else { q } }).await { Ok(r) => r, Err(e) => format!("(wikipedia: {e})") },
                None => "(wikipedia isn't configured)".to_string(),
            },
            _ => return None,
        })
    }
}

/// Calculator — local arithmetic, no model, no network.
pub struct CalculatorCapability;

#[async_trait::async_trait]
impl CapabilityHandler for CalculatorCapability {
    fn id(&self) -> &'static str {
        "calculator"
    }

    async fn handle_command(&self, _host: &ConversationEngine, cmd: &str, rest: &str) -> Option<String> {
        match cmd {
            "calc" | "calculate" | "math" if !rest.is_empty() => Some(crate::calc(rest)),
            _ => None,
        }
    }

    async fn handle_tool(&self, _host: &ConversationEngine, tool: &str, args: &Value) -> Option<String> {
        Some(match tool {
            "calc" | "calculate" | "math" => crate::calc(&{ let e = arg(args, "expression"); if e.is_empty() { arg(args, "expr") } else { e } }),
            _ => return None,
        })
    }
}

/// Market quotes — crypto + stock, live.
pub struct MarketsCapability;

#[async_trait::async_trait]
impl CapabilityHandler for MarketsCapability {
    fn id(&self) -> &'static str {
        "markets"
    }

    async fn handle_command(&self, host: &ConversationEngine, cmd: &str, rest: &str) -> Option<String> {
        match cmd {
            "crypto" | "coin" if !rest.is_empty() => Some(host.run_agent_tool("crypto", &serde_json::json!({ "coin": rest })).await),
            "stock" | "ticker" if !rest.is_empty() => Some(host.run_agent_tool("stock", &serde_json::json!({ "symbol": rest })).await),
            _ => None,
        }
    }

    async fn handle_tool(&self, host: &ConversationEngine, tool: &str, args: &Value) -> Option<String> {
        Some(match tool {
            "crypto" | "coin" => match &host.markets {
                Some(m) => match m.crypto(&{ let c = arg(args, "coin"); if c.is_empty() { arg(args, "query") } else { c } }).await { Ok(r) => r, Err(e) => format!("(crypto: {e})") },
                None => "(markets aren't configured)".to_string(),
            },
            "stock" | "ticker" => match &host.markets {
                Some(m) => match m.stock(&{ let t = arg(args, "symbol"); if t.is_empty() { arg(args, "ticker") } else { t } }).await { Ok(r) => r, Err(e) => format!("(stock: {e})") },
                None => "(markets aren't configured)".to_string(),
            },
            _ => return None,
        })
    }
}

/// Translate — target language + text, source auto-detected.
pub struct TranslateCapability;

#[async_trait::async_trait]
impl CapabilityHandler for TranslateCapability {
    fn id(&self) -> &'static str {
        "translate"
    }

    async fn handle_command(&self, host: &ConversationEngine, cmd: &str, rest: &str) -> Option<String> {
        match cmd {
            "translate" | "tr" if !rest.is_empty() => {
                // `ym translate <lang> <text…>` — first token is the target language.
                let mut p = rest.splitn(2, char::is_whitespace);
                let lang = p.next().unwrap_or("");
                let text = p.next().unwrap_or("").trim();
                Some(if text.is_empty() {
                    "Usage: ym translate <language> <text>  (e.g. ym translate french good morning)".to_string()
                } else {
                    host.run_agent_tool("translate", &serde_json::json!({ "to": lang, "text": text })).await
                })
            }
            _ => None,
        }
    }

    async fn handle_tool(&self, host: &ConversationEngine, tool: &str, args: &Value) -> Option<String> {
        Some(match tool {
            "translate" => match &host.translator {
                Some(tr) => match tr.translate(&{ let l = arg(args, "to"); if l.is_empty() { arg(args, "language") } else { l } }, &arg(args, "text")).await { Ok(r) => r, Err(e) => format!("(translate: {e})") },
                None => "(translator isn't configured)".to_string(),
            },
            _ => return None,
        })
    }
}

/// GitHub — open items on a repo + the notification digest.
pub struct GithubCapability;

#[async_trait::async_trait]
impl CapabilityHandler for GithubCapability {
    fn id(&self) -> &'static str {
        "github"
    }

    async fn handle_command(&self, host: &ConversationEngine, cmd: &str, rest: &str) -> Option<String> {
        match cmd {
            "github" | "gh" if host.github.is_some() => Some(if rest.contains('/') {
                host.run_agent_tool("github_repo_items", &serde_json::json!({ "repo": rest })).await
            } else {
                host.run_agent_tool("github_notifications", &serde_json::json!({})).await
            }),
            _ => None,
        }
    }

    async fn handle_tool(&self, host: &ConversationEngine, tool: &str, args: &Value) -> Option<String> {
        Some(match tool {
            "github_repo_items" => match &host.github {
                Some(g) => {
                    let repo = arg(args, "repo");
                    match g.repo_open_items(&repo, 15).await {
                        Ok(items) if !items.is_empty() => format!("{repo} — {} open:\n", items.len())
                            + &items.iter().map(|i| format!("#{} [{}] {} (by {})", i.number, i.kind, i.title, i.author)).collect::<Vec<_>>().join("\n"),
                        Ok(_) => format!("{repo}: no open issues/PRs"),
                        Err(e) => format!("(github error for {repo}: {e})"),
                    }
                }
                None => "(github not configured)".to_string(),
            },
            "github_notifications" => match &host.github {
                Some(g) => match g.notifications(15).await { Ok(n) => mind_tools::render_github_digest(&n), Err(e) => format!("(error: {e})") },
                None => "(github not configured)".to_string(),
            },
            _ => return None,
        })
    }
}
