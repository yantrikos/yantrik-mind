//! Small ported capabilities — web/utility domains routed by the registry instead of match arms.
//! Each preserves its old arm's guards exactly: a guard that failed used to fall through the
//! match, so here it returns None and the legacy fallback answers as before. Domains with a
//! fuller life (finance, home, news) keep their capability next to their domain code instead.

use serde_json::Value;

use crate::plugins::CapabilityHandler;
use crate::ConversationEngine;

fn arg(args: &Value, k: &str) -> String {
    args.get(k)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// The capability handlers' argument reader, resolving through the ONE alias table the boundary
/// validates against (`tool_catalog::read_arg`) — so `calc {"expr"}`, `weather {"city"}`,
/// `stock {"ticker"}` and the rest are declared once and served identically on both paths (P.2f).
fn targ(tool: &str, args: &Value, k: &str) -> String {
    crate::tool_catalog::read_arg(tool, args, k)
}

/// Web search — discovery, then web_fetch reads.
pub struct WebSearchCapability;

#[async_trait::async_trait]
impl CapabilityHandler for WebSearchCapability {
    fn id(&self) -> &str {
        "web_search"
    }

    async fn handle_command(
        &self,
        host: &ConversationEngine,
        cmd: &str,
        rest: &str,
    ) -> Option<String> {
        match cmd {
            "search" | "google" | "ddg" if !rest.is_empty() => Some(
                host.run_agent_tool("search", &serde_json::json!({ "query": rest }))
                    .await,
            ),
            _ => None,
        }
    }

    async fn handle_tool(
        &self,
        host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        Some(match tool {
            "search" | "web_search" => match &host.searcher {
                Some(se) => {
                    let q = targ(tool, args, "query");
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
    fn id(&self) -> &str {
        "web_fetch"
    }

    async fn handle_command(
        &self,
        host: &ConversationEngine,
        cmd: &str,
        rest: &str,
    ) -> Option<String> {
        match cmd {
            "web" | "fetch" if host.web.is_some() && !rest.is_empty() => Some(
                host.run_agent_tool("web_fetch", &serde_json::json!({ "url": rest }))
                    .await,
            ),
            _ => None,
        }
    }

    async fn handle_tool(
        &self,
        host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        Some(match tool {
            "web_fetch" => match &host.web {
                Some(w) => {
                    // A weak model often passes a messy url ("https://x.com and tell me…"); extract the
                    // first real http(s) url from whatever it gave so ureq doesn't choke (IdnaError).
                    let raw = arg(args, "url");
                    let url = mind_tools::first_url(&raw).unwrap_or(raw);
                    match w.fetch(&url).await {
                        Ok(t) => t.chars().take(6000).collect(),
                        Err(e) => format!("(fetch error: {e})"),
                    }
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
    fn id(&self) -> &str {
        "weather"
    }

    async fn handle_command(
        &self,
        host: &ConversationEngine,
        cmd: &str,
        rest: &str,
    ) -> Option<String> {
        match cmd {
            "weather" | "wx" if !rest.is_empty() => Some(
                host.run_agent_tool("weather", &serde_json::json!({ "place": rest }))
                    .await,
            ),
            _ => None,
        }
    }

    async fn handle_tool(
        &self,
        host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        Some(match tool {
            "weather" => match &host.weather {
                Some(w) => match w.report(&targ(tool, args, "place")).await {
                    Ok(r) => r,
                    Err(e) => format!("(weather: {e})"),
                },
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
    fn id(&self) -> &str {
        "wikipedia"
    }

    async fn handle_command(
        &self,
        host: &ConversationEngine,
        cmd: &str,
        rest: &str,
    ) -> Option<String> {
        match cmd {
            "wiki" | "wikipedia" if !rest.is_empty() => Some(
                host.run_agent_tool("wikipedia", &serde_json::json!({ "query": rest }))
                    .await,
            ),
            _ => None,
        }
    }

    async fn handle_tool(
        &self,
        host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        Some(match tool {
            "wikipedia" | "wiki" => match &host.wiki {
                Some(w) => match w.lookup(&targ(tool, args, "query")).await {
                    Ok(r) => r,
                    Err(e) => format!("(wikipedia: {e})"),
                },
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
    fn id(&self) -> &str {
        "calculator"
    }

    async fn handle_command(
        &self,
        _host: &ConversationEngine,
        cmd: &str,
        rest: &str,
    ) -> Option<String> {
        match cmd {
            "calc" | "calculate" | "math" if !rest.is_empty() => Some(crate::calc(rest)),
            _ => None,
        }
    }

    async fn handle_tool(
        &self,
        _host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        Some(match tool {
            "calc" | "calculate" | "math" => crate::calc(&targ(tool, args, "expression")),
            _ => return None,
        })
    }
}

/// Market quotes — crypto + stock, live.
pub struct MarketsCapability;

#[async_trait::async_trait]
impl CapabilityHandler for MarketsCapability {
    fn id(&self) -> &str {
        "markets"
    }

    async fn handle_command(
        &self,
        host: &ConversationEngine,
        cmd: &str,
        rest: &str,
    ) -> Option<String> {
        match cmd {
            "crypto" | "coin" if !rest.is_empty() => Some(
                host.run_agent_tool("crypto", &serde_json::json!({ "coin": rest }))
                    .await,
            ),
            "stock" | "ticker" if !rest.is_empty() => Some(
                host.run_agent_tool("stock", &serde_json::json!({ "symbol": rest }))
                    .await,
            ),
            _ => None,
        }
    }

    async fn handle_tool(
        &self,
        host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        Some(match tool {
            "crypto" | "coin" => match &host.markets {
                Some(m) => match m.crypto(&targ(tool, args, "coin")).await {
                    Ok(r) => r,
                    Err(e) => format!("(crypto: {e})"),
                },
                None => "(markets aren't configured)".to_string(),
            },
            "stock" | "ticker" => match &host.markets {
                Some(m) => match m.stock(&targ(tool, args, "symbol")).await {
                    Ok(r) => r,
                    Err(e) => format!("(stock: {e})"),
                },
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
    fn id(&self) -> &str {
        "translate"
    }

    async fn handle_command(
        &self,
        host: &ConversationEngine,
        cmd: &str,
        rest: &str,
    ) -> Option<String> {
        match cmd {
            "translate" | "tr" if !rest.is_empty() => {
                // `ym translate <lang> <text…>` — first token is the target language.
                let mut p = rest.splitn(2, char::is_whitespace);
                let lang = p.next().unwrap_or("");
                let text = p.next().unwrap_or("").trim();
                Some(if text.is_empty() {
                    "Usage: ym translate <language> <text>  (e.g. ym translate french good morning)"
                        .to_string()
                } else {
                    host.run_agent_tool(
                        "translate",
                        &serde_json::json!({ "to": lang, "text": text }),
                    )
                    .await
                })
            }
            _ => None,
        }
    }

    async fn handle_tool(
        &self,
        host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        Some(match tool {
            "translate" => match &host.translator {
                Some(tr) => match tr
                    .translate(&targ(tool, args, "to"), &arg(args, "text"))
                    .await
                {
                    Ok(r) => r,
                    Err(e) => format!("(translate: {e})"),
                },
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
    fn id(&self) -> &str {
        "github"
    }

    async fn handle_command(
        &self,
        host: &ConversationEngine,
        cmd: &str,
        rest: &str,
    ) -> Option<String> {
        match cmd {
            "github" | "gh" if host.github.is_some() => Some(if rest.contains('/') {
                host.run_agent_tool("github_repo_items", &serde_json::json!({ "repo": rest }))
                    .await
            } else {
                host.run_agent_tool("github_notifications", &serde_json::json!({}))
                    .await
            }),
            _ => None,
        }
    }

    async fn handle_tool(
        &self,
        host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        Some(match tool {
            "github_repo_items" => match &host.github {
                Some(g) => {
                    let repo = arg(args, "repo");
                    match g.repo_open_items(&repo, 15).await {
                        Ok(items) if !items.is_empty() => {
                            format!("{repo} — {} open:\n", items.len())
                                + &items
                                    .iter()
                                    .map(|i| {
                                        format!(
                                            "#{} [{}] {} (by {})",
                                            i.number, i.kind, i.title, i.author
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                        }
                        Ok(_) => format!("{repo}: no open issues/PRs"),
                        Err(e) => format!("(github error for {repo}: {e})"),
                    }
                }
                None => "(github not configured)".to_string(),
            },
            "github_notifications" => match &host.github {
                Some(g) => match g.notifications(15).await {
                    Ok(n) => mind_tools::render_github_digest(&n),
                    Err(e) => format!("(error: {e})"),
                },
                None => "(github not configured)".to_string(),
            },
            _ => return None,
        })
    }
}

/// Deep research — a delegated background job with an ack now and delivery via the notify drain.
/// Tool-only: there was never a `ym research` command, so none is invented here.
pub struct ResearchCapability;

#[async_trait::async_trait]
impl CapabilityHandler for ResearchCapability {
    fn id(&self) -> &str {
        "research"
    }

    fn handles_commands(&self) -> bool {
        false
    }

    async fn handle_command(
        &self,
        _host: &ConversationEngine,
        _cmd: &str,
        _rest: &str,
    ) -> Option<String> {
        None
    }

    async fn handle_tool(
        &self,
        host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        if tool != "research" {
            return None;
        }
        let topic = targ(tool, args, "query");
        if topic.len() < 3 {
            return Some("(what should I research? give me a topic)".to_string());
        }
        Some(match &host.researcher {
            Some(r) => {
                if !host.try_acquire_bg(2) {
                    return Some("(I've got a couple of background jobs running already — let those finish and ask again.)".to_string());
                }
                let (r, q, jobs, topic2) = (
                    r.clone(),
                    host.notify_queue.clone(),
                    host.bg_jobs.clone(),
                    topic.clone(),
                );
                tokio::spawn(async move {
                    let res = r.run(&topic2).await;
                    let mut msg = format!("🔎 Research — {topic2}:\n\n{}", res.answer);
                    if !res.sources.is_empty() {
                        msg.push_str("\n\nSources:\n");
                        for u in res.sources.iter().take(6) {
                            msg.push_str(&format!("- {u}\n"));
                        }
                    }
                    q.lock().unwrap().push(msg);
                    jobs.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                });
                format!("On it — researching \"{topic}\" in the background. I'll send what I find here when it's done.")
            }
            None => "(research isn't configured)".to_string(),
        })
    }
}

/// The coder — a delegated sandbox build job. Tool-only: `ym code` stays the repo browser's word.
pub struct CoderCapability;

#[async_trait::async_trait]
impl CapabilityHandler for CoderCapability {
    fn id(&self) -> &str {
        "coder"
    }

    fn handles_commands(&self) -> bool {
        false
    }

    async fn handle_command(
        &self,
        _host: &ConversationEngine,
        _cmd: &str,
        _rest: &str,
    ) -> Option<String> {
        None
    }

    async fn handle_tool(
        &self,
        host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        if tool != "code" {
            return None;
        }
        let task = targ(tool, args, "task");
        if task.len() < 3 {
            return Some("(what should I build? describe the script/task)".to_string());
        }
        Some(match &host.coder {
            Some(c) => {
                if !host.try_acquire_bg(2) {
                    return Some("(I've got a couple of background jobs running already — let those finish and ask again.)".to_string());
                }
                let (c, q, jobs, task2) = (
                    c.clone(),
                    host.notify_queue.clone(),
                    host.bg_jobs.clone(),
                    task.clone(),
                );
                tokio::spawn(async move {
                    let out = match c.run(&task2).await {
                        Ok(r) => format!("🛠️ Code — {task2}:\n\n{}", mind_tools::render_coder(&r)),
                        Err(e) => format!("🛠️ Code — \"{task2}\" failed: {e}"),
                    };
                    q.lock().unwrap().push(out);
                    jobs.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                });
                format!("On it — building \"{task}\" in the background (isolated sandbox; can take a few minutes). I'll send the result here when it's done.")
            }
            None => "(the coder isn't configured)".to_string(),
        })
    }
}

/// Monitors — a standing watch built as a recipe (poll + notify). Tool-only.
pub struct MonitorsCapability;

#[async_trait::async_trait]
impl CapabilityHandler for MonitorsCapability {
    fn id(&self) -> &str {
        "monitors"
    }

    fn handles_commands(&self) -> bool {
        false
    }

    async fn handle_command(
        &self,
        _host: &ConversationEngine,
        _cmd: &str,
        _rest: &str,
    ) -> Option<String> {
        None
    }

    async fn handle_tool(
        &self,
        host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        if tool != "set_monitor" {
            return None;
        }
        use mind_recipes::{Condition, Recipe, RecipeStep};
        let Some(recipes) = &host.recipes else {
            return Some("(monitor engine unavailable)".to_string());
        };
        let (source, target) = (arg(args, "source"), arg(args, "target"));
        if target.len() < 2 {
            return Some("(need a target to watch for)".to_string());
        }
        let (tool_name, var, targs, label): (&str, &str, serde_json::Value, &str) =
            match source.as_str() {
                "web" => (
                    "fetch",
                    "page",
                    serde_json::json!({ "url": arg(args, "url") }),
                    "web page",
                ),
                "inbox" | "email" => (
                    "inbox",
                    "inbox",
                    serde_json::json!({ "limit": 10 }),
                    "inbox",
                ),
                _ => (
                    "github",
                    "github",
                    serde_json::json!({ "limit": 15 }),
                    "GitHub",
                ),
            };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let rec = Recipe {
            id: "watch".into(),
            name: format!("watch {label}: {target}"),
            steps: vec![
                RecipeStep::WaitForCondition {
                    tool_name: tool_name.into(),
                    args: targs,
                    store_as: var.into(),
                    condition: Condition::VarContains {
                        var: var.into(),
                        substring: target.clone(),
                    },
                    poll_secs: 120,
                    expire_ms: now + 24 * 3600 * 1000,
                },
                RecipeStep::Notify {
                    message: format!("📡 the {label} now matches \"{target}\"."),
                },
            ],
        };
        let out = recipes
            .run_with(&rec, std::collections::HashMap::new())
            .await;
        Some(if out.sleeping_until.is_some() {
            format!("Watching the {label} for \"{target}\" — I'll ping you when it matches.")
        } else if !out.notifications.is_empty() {
            out.notifications.join("\n")
        } else {
            format!(
                "(couldn't start watching: {})",
                out.error.unwrap_or_else(|| "tool unavailable".into())
            )
        })
    }
}

/// Dashboards & pages — structured data in, rendered + hosted + verify-served HTML out. Tool-only.
pub struct DashboardsCapability;

#[async_trait::async_trait]
impl CapabilityHandler for DashboardsCapability {
    fn id(&self) -> &str {
        "dashboards"
    }

    fn handles_commands(&self) -> bool {
        false
    }

    async fn handle_command(
        &self,
        _host: &ConversationEngine,
        _cmd: &str,
        _rest: &str,
    ) -> Option<String> {
        None
    }

    async fn handle_tool(
        &self,
        _host: &ConversationEngine,
        tool: &str,
        args: &Value,
    ) -> Option<String> {
        use crate::PageServe;
        Some(match tool {
            // E.FILES2: a whole project, not one document. The deliverable of a build is a set of
            // files, and this is the only way the mind can produce one without an external CLI or
            // somebody else's subscription — it runs on the mind's own inference path, which is
            // OpenAI-compatible, so it works with the providers we actually have and inside
            // containment, where the Anthropic-protocol coder cannot go at all.
            "write_files" => {
                let (project, stream) = (arg(args, "project"), arg(args, "stream"));
                if project.trim().is_empty() {
                    return Some("(a build needs a project name)".to_string());
                }
                match crate::publish_file_set(&project, &stream) {
                    Ok((url, written, unterminated)) => {
                        let mut msg = format!("{url} ({} files: {})", written.len(), written.join(", "));
                        if !unterminated.is_empty() {
                            msg.push_str(&format!(
                                " — NOTE: the stream ended without a newline, so {} may be incomplete",
                                unterminated.join(", ")
                            ));
                        }
                        msg
                    }
                    Err(why) => format!("(couldn't write the project: {why})"),
                }
            }
            "publish_page" => {
                let (name, html) = (arg(args, "name"), arg(args, "html"));
                if html.len() < 10 {
                    return Some("(need html content to publish)".to_string());
                }
                match crate::publish_html(if name.is_empty() { "page" } else { &name }, &html) {
                    Some(url) => match crate::verify_served(&url, &html).await {
                        PageServe::Ok => format!("Published & verified live — the page loads with the right content (works on your home network):\n{url}"),
                        PageServe::Mismatch => format!("Published, and the server responds, but the content served back didn't match what I generated (possibly a stale file) — worth a look:\n{url}"),
                        PageServe::Down => format!("I saved the page but my web server didn't serve it back (it may be off). File: {url} — tell me if you want me to check the server."),
                    },
                    None => "(couldn't publish the page)".to_string(),
                }
            }
            "make_dashboard" => {
                // The robust dashboard path: the model gives small STRUCTURED data, Rust renders the
                // (guaranteed-valid, escaped) HTML — no giant inline HTML string to truncate.
                let title = arg(args, "title");
                if title.is_empty() && args.get("sections").is_none() && args.get("items").is_none()
                {
                    return Some(
                        "(need at least a title and some sections/items for the dashboard)"
                            .to_string(),
                    );
                }
                let html = crate::render_dashboard(args);
                let name = if title.is_empty() {
                    "dashboard".to_string()
                } else {
                    title
                };
                match crate::publish_html(&name, &html) {
                    Some(url) => match crate::verify_served(&url, &html).await {
                        PageServe::Ok => format!("Done & verified live — the dashboard loads with the right content (works on your home network):\n{url}"),
                        PageServe::Mismatch => format!("Built it, and the server responds, but the content served back didn't match what I generated (possibly a stale file) — worth a look:\n{url}"),
                        PageServe::Down => format!("I built the dashboard but my web server didn't serve it back (it may be off). File: {url} — tell me if you want me to check the server."),
                    },
                    None => "(couldn't publish the dashboard)".to_string(),
                }
            }
            _ => return None,
        })
    }
}
