//! mind-core — the orchestrator + runnable REPL.
//!
//! v1 wires the slice that proves the bet end-to-end: a real LLM backend (chosen at runtime) →
//! `InferencePool` → `ConversationEngine` grounded in the `mind-memory` typed moat. The REPL lets
//! you talk to it and watch memory evolve live (assert beliefs, see contradictions, recall).

use std::sync::Arc;

use mind_conversation::ConversationEngine;
use mind_memory::MemoryHandle;
use mind_types::{BeliefAssertion, MemoryFacade, RecallQuery, TensionKind};

pub mod telegram;

/// Parse the two conflicting belief statements from a Contradiction tension's `about` field.
/// Handles both formats emitted by the memory layer:
///   "conflict: A vs B"   (assert-belief auto-detection path)
///   "\"A\" vs \"B\""     (DMN reconciliation path)
fn parse_contradiction_beliefs(about: &str) -> Option<(String, String)> {
    let s = about.strip_prefix("conflict: ").unwrap_or(about);
    let (a, b) = s.split_once(" vs ")?;
    let a = a.trim().trim_matches('"').to_string();
    let b = b.trim().trim_matches('"').to_string();
    if a.is_empty() || b.is_empty() { return None; }
    Some((a, b))
}

/// One REPL line → an outcome. Split out of `main` so it's deterministically testable with a
/// `ScriptedLLM` (no real model).
pub enum Outcome {
    Quit,
    Said(String),
}

const HELP_TEXT: &str = "\
:remember + <stmt>   assert evidence in favour of a belief
:remember - <stmt>   assert evidence against a belief
:beliefs [query]     list top beliefs by confidence (optional semantic filter)
:reflect [topic]     structured self-reflection from typed memory
:conflicts           list open contradictions
:resolve <id>        resolve a contradiction tension (weakens the disputed belief)
:explain <stmt>      show a belief with its evidence count
:goal <text>         store a goal (surfaces in :reflect)
:prefer <text>       store a preference (surfaces in :reflect)
:tasks               list open tasks
:task <desc>         add a new task
:done <id>           mark a task complete
:consolidate         fold recent turns into durable beliefs
:patterns            find non-obvious patterns across what I know + save them as beliefs
:workers             show remote worker-pool status

(ym-only) track <subject>   hold + evolve a living understanding (re-run → what changed)
(ym-only) predictions       open predictions I've committed to being graded on
(ym-only) resolve [all]      grade due predictions against reality (self-scoring)
(ym-only) calibration        the learning curve — hit-rate per domain, trending
:help / :commands    show this help
:quit / :q           exit";

/// Handle a single REPL line. Commands start with `:`; anything else is a chat turn.
///   `:remember + <statement>` / `:remember - <statement>`  assert evidence for/against a belief
///   `:beliefs [query]`                                      list top beliefs by confidence (optional semantic filter)
///   `:reflect [topic]`                                      structured self-reflection from typed memory
///   `:conflicts`                                            list open contradictions
///   `:explain <statement>`                                  show a belief + its evidence count
///   `:help` / `:commands`                                   print every command with a one-line description
///   `:quit`
pub async fn handle_line(line: &str, mem: &MemoryHandle, conv: &ConversationEngine) -> Outcome {
    handle_line_as(line, mem, conv, mind_conversation::TurnIdentity::primary()).await
}

/// As `handle_line`, but the chat turn is attributed to a known household member (read-isolation).
pub async fn handle_line_as(
    line: &str,
    mem: &MemoryHandle,
    conv: &ConversationEngine,
    identity: mind_conversation::TurnIdentity,
) -> Outcome {
    let raw = line.trim();
    if raw.is_empty() {
        return Outcome::Said(String::new());
    }
    // Accept telegram-style '/command' (with optional @botname) as our ':command'.
    let owned;
    let t: &str = if let Some(body) = raw.strip_prefix('/') {
        let (cmd, rest) = body.split_once(' ').unwrap_or((body, ""));
        let cmd = cmd.split('@').next().unwrap_or(cmd);
        owned = if rest.is_empty() { format!(":{cmd}") } else { format!(":{cmd} {rest}") };
        &owned
    } else {
        raw
    };
    if t == ":quit" || t == ":q" {
        return Outcome::Quit;
    }
    if t == ":help" || t == ":commands" {
        return Outcome::Said(HELP_TEXT.into());
    }
    if t == ":consolidate" {
        let n = conv.consolidate().await;
        return Outcome::Said(format!("consolidated {n} durable belief(s) from recent turns"));
    }
    if t == ":workers" {
        return Outcome::Said(conv.workers_status().await);
    }
    if t == ":patterns" || t == ":insights" {
        return Outcome::Said(conv.find_patterns().await);
    }
    if let Some(query) = t.strip_prefix(":beliefs") {
        let query = query.trim();
        return match mem.recall_typed(RecallQuery { text: query.to_string(), top_k: 10, kind: None }).await {
            Ok(rs) if rs.is_empty() => Outcome::Said("(no beliefs stored)".into()),
            Ok(mut rs) => {
                rs.sort_by(|a, b| b.item.confidence.partial_cmp(&a.item.confidence).unwrap_or(std::cmp::Ordering::Equal));
                Outcome::Said(rs.iter().map(|r| format!("• {} ({:.2})", r.item.text, r.item.confidence)).collect::<Vec<_>>().join("\n"))
            }
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(topic) = t.strip_prefix(":reflect") {
        let topic = topic.trim();
        return match mem.reflect(topic).await {
            Err(e) => Outcome::Said(format!("(error: {e})")),
            Ok(r) => {
                let mut out = String::new();
                let (stable, uncertain): (Vec<_>, Vec<_>) =
                    r.beliefs.iter().partition(|b| b.confidence >= 0.70);
                out.push_str("## what I believe\n");
                if stable.is_empty() {
                    out.push_str("  (none)\n");
                } else {
                    for b in &stable {
                        out.push_str(&format!("  • {} ({:.2})\n", b.statement, b.confidence));
                    }
                }
                out.push_str("## uncertain / contradicted\n");
                if uncertain.is_empty() && r.open_conflicts.is_empty() {
                    out.push_str("  (none)\n");
                } else {
                    for b in &uncertain {
                        out.push_str(&format!("  ? {} ({:.2})\n", b.statement, b.confidence));
                    }
                    for c in &r.open_conflicts {
                        out.push_str(&format!(
                            "  \u{27c2} \"{}\" vs \"{}\"\n",
                            c.belief_a, c.belief_b
                        ));
                    }
                }
                out.push_str("## goals\n");
                if r.goals.is_empty() {
                    out.push_str("  (none stored)\n");
                } else {
                    for g in &r.goals {
                        out.push_str(&format!("  \u{25b6} {}\n", g.text));
                    }
                }
                out.push_str("## preferences\n");
                if r.preferences.is_empty() {
                    out.push_str("  (none stored)");
                } else {
                    for p in &r.preferences {
                        out.push_str(&format!("  \u{2605} {}", p.text));
                        out.push('\n');
                    }
                    // trim trailing newline to keep parity with the empty-branch output
                    if out.ends_with('\n') {
                        out.pop();
                    }
                }
                Outcome::Said(out)
            }
        };
    }
    if t == ":conflicts" {
        return match mem.conflicts().await {
            Ok(cs) if cs.is_empty() => Outcome::Said("(no open contradictions)".into()),
            Ok(cs) => Outcome::Said(
                cs.iter()
                    .map(|c| format!("• \"{}\" ⟂ \"{}\" (severity {:.2})", c.belief_a, c.belief_b, c.severity))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(id) = t.strip_prefix(":resolve ") {
        let id = id.trim();
        let tensions = match mem.open_tensions(200).await {
            Ok(ts) => ts,
            Err(e) => return Outcome::Said(format!("(error: {e})")),
        };
        let Some(tension) = tensions.iter().find(|t| t.id == id) else {
            return Outcome::Said(format!("(no open tension with id {id})"));
        };
        if tension.kind != TensionKind::Contradiction {
            let tid = tension.id.clone();
            return match mem.discharge_tension(&tid).await {
                Ok(_) => Outcome::Said("tension discharged".into()),
                Err(e) => Outcome::Said(format!("(error: {e})")),
            };
        }
        let about = tension.about.clone();
        let tid = tension.id.clone();
        let Some((belief_a, belief_b)) = parse_contradiction_beliefs(&about) else {
            return Outcome::Said(format!("(could not parse beliefs from tension: {about})"));
        };
        // Weaken the less-confident side so the contradiction resolves.
        let conf_a = mem.explain_belief(&belief_a).await.ok().flatten().map(|(b, _)| b.confidence).unwrap_or(0.5);
        let conf_b = mem.explain_belief(&belief_b).await.ok().flatten().map(|(b, _)| b.confidence).unwrap_or(0.5);
        let weaker = if conf_a <= conf_b { belief_a } else { belief_b };
        let result = mem
            .remember_as_belief(BeliefAssertion {
                statement: weaker.clone(),
                polarity: -1.0,
                weight: 1.5,
                source_event: Some("resolve".into()),
                provenance: "told".into(),
            })
            .await;
        let _ = mem.discharge_tension(&tid).await;
        return match result {
            Ok(b) => Outcome::Said(format!(
                "resolved: weakened \"{}\" (confidence now {:.2}), tension discharged",
                b.statement, b.confidence
            )),
            Err(e) => Outcome::Said(format!("(could not weaken belief: {e}; tension discharged)")),
        };
    }
    if let Some(stmt) = t.strip_prefix(":explain ") {
        return match mem.explain_belief(stmt.trim()).await {
            Ok(Some((b, ev))) => Outcome::Said(format!(
                "{} — confidence {:.2}, {} evidence item(s), provenance {}",
                b.statement, b.confidence, ev.len().max(b.evidence_count as usize), b.provenance
            )),
            Ok(None) => Outcome::Said("(no such belief)".into()),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(text) = t.strip_prefix(":goal ") {
        return match mem.store_goal(text.trim()).await {
            Ok(()) => Outcome::Said("goal stored".into()),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(text) = t.strip_prefix(":prefer ") {
        return match mem.store_preference(text.trim()).await {
            Ok(()) => Outcome::Said("preference stored".into()),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if t == ":tasks" {
        return match mem.list_tasks(false).await {
            Ok(ts) if ts.is_empty() => Outcome::Said("(no open tasks)".into()),
            Ok(ts) => Outcome::Said(
                ts.iter()
                    .map(|t| format!("• [{}] {} ({})", t.id, t.description, t.priority))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(desc) = t.strip_prefix(":task ") {
        return match mem.add_task(desc.trim(), "medium", None).await {
            Ok(task) => Outcome::Said(format!("added task [{}]: {}", task.id, task.description)),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(id) = t.strip_prefix(":done ") {
        return match mem.complete_task(id.trim()).await {
            Ok(true) => Outcome::Said("task completed".into()),
            Ok(false) => Outcome::Said("(no such task)".into()),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(rest) = t.strip_prefix(":remember ") {
        let rest = rest.trim();
        let (polarity, statement) = if let Some(s) = rest.strip_prefix("- ") {
            (-1.0, s.trim())
        } else if let Some(s) = rest.strip_prefix("+ ") {
            (1.0, s.trim())
        } else {
            (1.0, rest)
        };
        return match mem
            .remember_as_belief(BeliefAssertion {
                statement: statement.to_string(),
                polarity,
                weight: 1.5,
                source_event: Some("repl".into()),
                provenance: "told".into(),
            })
            .await
        {
            Ok(b) => Outcome::Said(format!("remembered: {} (confidence now {:.2})", b.statement, b.confidence)),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    // plain chat turn — attributed to the speaker (group-chat read-isolation)
    match conv.handle_turn_as(t, identity).await {
        Ok(r) => Outcome::Said(r),
        Err(e) => Outcome::Said(format!("(error: {e})")),
    }
}

/// Build a `ConversationEngine` from a memory handle and an inference pool. The operator name is
/// read from config (YM_OPERATOR), never hardcoded — defaults to "the user".
pub fn engine(mem: &MemoryHandle, pool: mind_inference::InferencePool) -> ConversationEngine {
    let operator = std::env::var("YM_OPERATOR").unwrap_or_default();
    let persona = mind_types::default_persona(&operator);
    let memory: Arc<dyn MemoryFacade> = Arc::new(mem.clone());

    // Shared read capabilities (used by both chat grounding and recipes).
    // Gmail needs a 16-char App Password; a non-standard IMAP host can be set with YM_IMAP_HOST.
    let mail_read: Option<Arc<dyn mind_tools::MailClient>> =
        match (std::env::var("YM_EMAIL"), std::env::var("YM_EMAIL_PASSWORD")) {
            (Ok(addr), Ok(pw)) if !addr.is_empty() && !pw.is_empty() => match std::env::var("YM_IMAP_HOST") {
                Ok(host) if !host.is_empty() => {
                    Some(Arc::new(mind_tools::ImapClient::new(host, 993, addr, pw)) as Arc<dyn mind_tools::MailClient>)
                }
                _ => mind_tools::ImapClient::for_address(&addr, pw)
                    .map(|c| Arc::new(c) as Arc<dyn mind_tools::MailClient>),
            },
            _ => None,
        };
    let gh_token = std::env::var("YM_GITHUB_TOKEN").ok().filter(|t| !t.is_empty());
    let github_read: Option<Arc<dyn mind_tools::GithubClient>> = gh_token
        .as_ref()
        .map(|t| Arc::new(mind_tools::ApiGithubClient::new(t.clone())) as Arc<dyn mind_tools::GithubClient>);

    // Per-function model routing: pin a role (chat/research/util/…) to a provider:model via
    // YM_ROLE_<ROLE> (e.g. YM_ROLE_RESEARCH=ollama-cloud:kimi-k2.7); unset roles use the default
    // chain. Conversation→chat, the research sub-agent→research, recipe Think steps→util.
    let router = mind_inference::Router::from_env(pool.clone(), 4);
    let chat_pool = router.pool("chat");
    let research_pool = router.pool("research");
    let util_pool = router.pool("util");

    // Web search backend: a self-hosted SearXNG instance (YM_SEARXNG_URL) when available — aggregates
    // many engines, no bot-challenge/rate-limit, indexes sites our direct fetch can't reach — with
    // keyless DuckDuckGo as the fallback. Falls back to plain DDG when no instance is configured.
    let searcher: Arc<dyn mind_tools::WebSearch> = match std::env::var("YM_SEARXNG_URL").ok().filter(|u| !u.trim().is_empty()) {
        Some(url) => {
            eprintln!("[search] using SearXNG at {url} (DDG fallback)");
            Arc::new(mind_tools::SearxngSearch::new(url).with_fallback(Arc::new(mind_tools::DdgSearch::new())))
        }
        None => Arc::new(mind_tools::DdgSearch::new()),
    };

    let mut eng = ConversationEngine::new(memory.clone(), chat_pool, persona.clone())
        .with_web(Arc::new(mind_tools::HttpFetcher::new()))
        .with_searcher(searcher.clone()) // SearXNG (or DDG) — the discovery half of research
        .with_news(Arc::new(mind_tools::GoogleNews::new())) // keyless news, always on
        .with_weather(Arc::new(mind_tools::OpenMeteo::new())) // keyless weather, always on
        .with_wiki(Arc::new(mind_tools::Wikipedia::new())) // keyless Wikipedia, always on
        .with_markets(Arc::new(mind_tools::LiveMarkets::new())) // keyless crypto + stock quotes
        .with_translator(Arc::new(mind_tools::GoogleTranslate::new())) // keyless translation
        // Declarative plugin manifest: enable/disable + security level, no code edits. Toggles persist.
        .with_plugins_manifest(std::env::var("YM_PLUGINS_CONFIG").unwrap_or_else(|_| "/var/lib/yantrik-mind/plugins.json".to_string()));
    if let Some(m) = &mail_read {
        eng = eng.with_mail(m.clone());
    }
    // A SEPARATE read-only personal inbox for finance discovery (the user's mailbox where subscription
    // receipts live), distinct from the bot's own account. Gmail needs a 16-char App Password.
    if let (Ok(addr), Ok(pw)) = (std::env::var("YM_SCAN_EMAIL"), std::env::var("YM_SCAN_PASSWORD")) {
        if !addr.is_empty() && !pw.is_empty() {
            if let Some(c) = mind_tools::ImapClient::for_address(&addr, pw) {
                eng = eng.with_scan_mail(Arc::new(c) as Arc<dyn mind_tools::MailClient>);
            }
        }
    }
    if let Some(g) = &github_read {
        eng = eng.with_github(g.clone());
    }
    // Smart-home awareness (Home Assistant): read-only entity states, when YM_HA_URL + YM_HA_TOKEN
    // are set. The first domain of the full-life world-model; control comes later, harm-gated.
    if let (Some(url), Some(tok)) = (
        std::env::var("YM_HA_URL").ok().filter(|u| !u.trim().is_empty()),
        std::env::var("YM_HA_TOKEN").ok().filter(|t| !t.trim().is_empty()),
    ) {
        eng = eng.with_home(Arc::new(mind_tools::ApiHomeAssistantClient::new(url, tok)));
    }

    // MCP integrations — THE FORCE MULTIPLIER. A JSON config (YM_MCP_CONFIG, default
    // /etc/yantrik-mind/mcp.json, in the de-facto `mcpServers` shape) lists servers; we connect them on
    // a BACKGROUND thread (a cold `npx` start downloads the package → slow) so startup isn't blocked.
    // The (initially-empty) hub is wired now; its tool catalog fills as servers come online. Read-only
    // tools then run freely in the agent loop; mutating tools are gated (no un-gated write path).
    let mut mcp_hub: Option<Arc<mind_tools::McpHub>> = None;
    {
        let path = std::env::var("YM_MCP_CONFIG").unwrap_or_else(|_| "/etc/yantrik-mind/mcp.json".to_string());
        if let Ok(raw) = std::fs::read_to_string(&path) {
            match serde_json::from_str::<serde_json::Value>(&raw) {
                Ok(v) => {
                    let configs = mind_tools::McpServerConfig::from_json(&v);
                    if !configs.is_empty() {
                        let n = configs.len();
                        let hub = Arc::new(mind_tools::McpHub::new());
                        let h = hub.clone();
                        std::thread::spawn(move || h.connect_all(&configs));
                        eng = eng.with_mcp(hub.clone());
                        mcp_hub = Some(hub); // also hand to the executor so confirmed MCP *writes* can run
                        eprintln!("[mcp] connecting {n} configured server(s) from {path} in the background");
                    }
                }
                Err(e) => eprintln!("[mcp] bad config at {path}: {e}"),
            }
        }
    }

    // Hands: an outward-action runtime, harm-gated + confirmation-required. Grants SendMessage when a
    // transport (email send and/or github comment) is configured. Every action rides the harm-gate.
    let mut executor = mind_tools::ToolActionExecutor::new();
    let mut granted: Vec<mind_types::Capability> = Vec::new();
    if let (Ok(addr), Ok(pw)) = (std::env::var("YM_EMAIL"), std::env::var("YM_EMAIL_PASSWORD")) {
        if !addr.is_empty() && !pw.is_empty() {
            executor = executor.with_mail_sender(Arc::new(mind_tools::SmtpMailSender::for_address(&addr, pw)));
            granted.push(mind_types::Capability::SendMessage);
        }
    }
    if let Some(token) = &gh_token {
        executor = executor.with_github_writer(Arc::new(mind_tools::ApiGithubClient::new(token.clone())));
        if !granted.contains(&mind_types::Capability::SendMessage) {
            granted.push(mind_types::Capability::SendMessage);
        }
    }
    // MCP writes: grant the outward Network capability + hand the executor the hub so a confirmed
    // `mcp_call` action (a mutating integration tool) can actually run. Still fully gated — every MCP
    // write rides the harm-gate and the same "confirm with yes" handshake as email/github.
    if let Some(hub) = &mcp_hub {
        executor = executor.with_mcp_hub(hub.clone());
        granted.push(mind_types::Capability::Network);
    }
    let runtime: Option<Arc<dyn mind_types::ActionRuntime>> = if granted.is_empty() {
        None
    } else {
        Some(Arc::new(mind_governance::GovernedActionRuntime::new(
            Arc::new(mind_governance::RealHarmGate::new()),
            Arc::new(executor),
            granted,
        )))
    };
    if let Some(rt) = &runtime {
        eng = eng.with_runtime(rt.clone());
    }

    // Shared tool host: recipe Tool steps + sub-agent tool calls both go through it. Includes web
    // research tools (keyless DuckDuckGo search + SSRF-guarded fetch).
    let host: Arc<dyn mind_recipes::RecipeHost> = Arc::new(
        mind_conversation::MindRecipeHost::new(mail_read.clone(), github_read.clone(), memory.clone())
            .with_web(Arc::new(mind_tools::HttpFetcher::new()), searcher.clone()),
    );

    // A research sub-agent: web search + fetch + the mind's own read tools. Bounded ReAct, read-only.
    let researcher = mind_agents::SubAgent::new(
        research_pool,
        host.clone(),
        persona.clone(),
        vec![
            "web_search".into(),
            "fetch".into(),
            "recall".into(),
            "inbox".into(),
            "github".into(),
        ],
        6,
    );
    eng = eng.with_researcher(Arc::new(researcher));

    // Recipe engine: citation-validated, adaptive workflows over the read capabilities. Gets the same
    // harm-gated runtime (for Act steps) and a durable store (persistence + crash recovery).
    let mut recipe_engine = mind_recipes::RecipeEngine::new(util_pool, host.clone(), persona.clone());
    if let Some(rt) = &runtime {
        recipe_engine = recipe_engine.with_runtime(rt.clone());
    }
    if let Ok(db) = std::env::var("YM_DB") {
        if !db.is_empty() && db != ":memory:" {
            if let Ok(store) = mind_recipes::RecipeStore::open(&db) {
                recipe_engine = recipe_engine.with_store(Arc::new(store));
            }
        }
    }
    eng = eng.with_recipes(Arc::new(recipe_engine));

    // Code sandbox: isolated (userns + no network), resource-limited execution of shell/python/rust.
    // Masks the mind's own state dir so sandboxed code can't read/corrupt the DB.
    let state_dir = std::env::var("YM_DB")
        .ok()
        .and_then(|p| std::path::Path::new(&p).parent().map(|d| d.to_string_lossy().to_string()))
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/var/lib/yantrik-mind".to_string());
    eng = eng.with_sandbox(Arc::new(mind_tools::Sandbox::new().hiding(state_dir)));

    // Remote worker pool: fan work out to the transferred LXCs over SSH (YM_WORKERS / YM_WORKER_KEY).
    if let Some(pool) = mind_tools::WorkerPool::from_env() {
        eng = eng.with_workers(Arc::new(pool));
    }

    // Agentic coder (the `code` role): Claude Code driven by MiniMax-M2 via MiniMax's Anthropic-compat
    // endpoint — runs on the MiniMax subscription, zero Anthropic cost. Needs the `claude` CLI present
    // + MINIMAX_API_KEY. Isolated scratch under the service user's home; secret-stripped child env.
    if mind_tools::Coder::available() {
        let oauth = std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok().filter(|t| !t.trim().is_empty());
        let minimax = std::env::var("MINIMAX_API_KEY").ok().filter(|k| !k.trim().is_empty());
        if oauth.is_some() || minimax.is_some() {
            let model = std::env::var("YM_CODER_MODEL").unwrap_or_else(|_| "MiniMax-M2".to_string());
            let scratch = std::env::var("YM_CODER_DIR").unwrap_or_else(|_| "/opt/yantrik-mind/coder".to_string());
            let mut coder = mind_tools::Coder::new(minimax.unwrap_or_default(), model, "https://api.minimax.io/anthropic", scratch);
            if let Some(t) = oauth {
                coder = coder.with_oauth(t); // prefer the Max-plan subscription (real Claude)
            }
            eng = eng.with_coder(Arc::new(coder));
        }
    }
    eng
}

#[cfg(test)]
mod tests {
    use super::*;
    use mind_inference::{InferencePool, ScriptedLLM};
    use mind_types::TensionKind;
    use yantrik_ml::LLMBackend;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn repl_commands_drive_typed_memory_and_chat() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let scripted = Arc::new(ScriptedLLM::new("ok"));
        let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
        // This test asserts the LEGACY grounded-chat path (belief reaches the system prompt); the
        // agentic loop grounds in the user prompt instead, so drive the legacy chain explicitly.
        let conv = engine(&mem, pool).with_agent_primary(false);

        // assert two contradicting beliefs + a link via the REPL
        assert!(matches!(handle_line(":remember + Pranab likes coffee", &mem, &conv).await, Outcome::Said(s) if s.contains("confidence")));
        handle_line(":remember + Pranab hates coffee", &mem, &conv).await;
        mem.relate("Pranab likes coffee", "Pranab hates coffee", "contradicts", 0.9).await.unwrap();

        // :conflicts surfaces it
        match handle_line(":conflicts", &mem, &conv).await {
            Outcome::Said(s) => assert!(s.contains("coffee"), "conflicts should list it: {s}"),
            _ => panic!("expected output"),
        }

        // a chat turn grounds in memory (ScriptedLLM saw the belief in its prompt)
        match handle_line("do I like coffee?", &mem, &conv).await {
            Outcome::Said(s) => assert_eq!(s, "ok"),
            _ => panic!("expected reply"),
        }
        assert!(scripted.last_system_prompt().contains("coffee"), "chat should be grounded in memory");

        // :quit
        assert!(matches!(handle_line(":quit", &mem, &conv).await, Outcome::Quit));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn beliefs_command_lists_and_filters() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let scripted = Arc::new(ScriptedLLM::new("ok"));
        let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
        let conv = engine(&mem, pool);

        // no beliefs yet
        assert!(matches!(handle_line(":beliefs", &mem, &conv).await, Outcome::Said(s) if s.contains("no beliefs")));

        // assert a belief and check it appears in :beliefs
        handle_line(":remember + sky is blue", &mem, &conv).await;
        match handle_line(":beliefs", &mem, &conv).await {
            Outcome::Said(s) => assert!(s.contains("sky is blue"), ":beliefs should list it: {s}"),
            _ => panic!("expected output"),
        }

        // query filter
        match handle_line(":beliefs sky", &mem, &conv).await {
            Outcome::Said(s) => assert!(s.contains("sky is blue"), ":beliefs <query> should filter: {s}"),
            _ => panic!("expected output"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn reflect_command_renders_sections() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let scripted = Arc::new(ScriptedLLM::new("ok"));
        let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
        let conv = engine(&mem, pool);

        // empty memory → four sections, nothing in beliefs
        match handle_line(":reflect", &mem, &conv).await {
            Outcome::Said(s) => {
                assert!(s.contains("## what I believe"), "missing belief section: {s}");
                assert!(s.contains("## uncertain / contradicted"), "missing uncertain section: {s}");
                assert!(s.contains("## goals"), "missing goals section: {s}");
                assert!(s.contains("## preferences"), "missing preferences section: {s}");
                assert!(s.contains("(none)"), "should report no beliefs yet: {s}");
            }
            _ => panic!("expected Outcome::Said"),
        }

        // assert a belief and check it surfaces in the reflection
        handle_line(":remember + I prefer concise answers", &mem, &conv).await;
        match handle_line(":reflect", &mem, &conv).await {
            Outcome::Said(s) => {
                assert!(s.contains("concise answers"), "belief should appear in reflection: {s}");
            }
            _ => panic!("expected Outcome::Said"),
        }

        // optional topic argument is forwarded (belief still surfaces when topic overlaps)
        match handle_line(":reflect concise", &mem, &conv).await {
            Outcome::Said(s) => {
                assert!(s.contains("concise"), ":reflect <topic> should surface related belief: {s}");
            }
            _ => panic!("expected Outcome::Said"),
        }

        // store a goal and a preference, then verify they appear in :reflect
        assert!(matches!(handle_line(":goal be helpful and honest", &mem, &conv).await, Outcome::Said(s) if s.contains("stored")));
        assert!(matches!(handle_line(":prefer concise responses", &mem, &conv).await, Outcome::Said(s) if s.contains("stored")));
        match handle_line(":reflect", &mem, &conv).await {
            Outcome::Said(s) => {
                assert!(s.contains("be helpful and honest"), "goal should appear in reflection: {s}");
                assert!(s.contains("concise responses"), "preference should appear in reflection: {s}");
            }
            _ => panic!("expected Outcome::Said"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn telegram_slash_commands_are_accepted() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let scripted = Arc::new(ScriptedLLM::new("ok"));
        let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
        let conv = engine(&mem, pool);
        handle_line("/task@th_ym_c1_bot buy milk", &mem, &conv).await;
        match handle_line("/tasks", &mem, &conv).await {
            Outcome::Said(s) => assert!(s.contains("buy milk"), "slash command should work: {s}"),
            _ => panic!("expected output"),
        }
        assert!(matches!(handle_line("/quit", &mem, &conv).await, Outcome::Quit));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn resolve_command_weakens_contradicted_belief_and_discharges_tension() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let scripted = Arc::new(ScriptedLLM::new("ok"));
        let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
        let conv = engine(&mem, pool);

        // Seed two beliefs that will form a contradiction.
        handle_line(":remember + sky is blue", &mem, &conv).await;
        handle_line(":remember + sky is green", &mem, &conv).await;

        // Record the contradiction tension directly (as the memory layer does automatically).
        mem.record_tension(TensionKind::Contradiction, 0.8, "conflict: sky is blue vs sky is green")
            .await
            .unwrap();

        let tensions = mem.open_tensions(10).await.unwrap();
        let t = tensions.iter().find(|t| t.about.contains("sky")).expect("tension should exist");
        let tid = t.id.clone();

        // Unknown id → helpful error, no crash.
        match handle_line(":resolve 99999", &mem, &conv).await {
            Outcome::Said(s) => assert!(s.contains("no open tension"), "expected missing-id message: {s}"),
            _ => panic!("expected Said"),
        }

        // Resolve the real tension.
        match handle_line(&format!(":resolve {tid}"), &mem, &conv).await {
            Outcome::Said(s) => {
                assert!(s.contains("weakened"), "should report weakened belief: {s}");
                assert!(s.contains("discharged"), "should report tension discharged: {s}");
            }
            _ => panic!("expected Said"),
        }

        // Tension must now be gone from the open list.
        let open = mem.open_tensions(10).await.unwrap();
        assert!(!open.iter().any(|t| t.id == tid), "resolved tension should no longer be open");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn help_command_lists_all_commands() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let scripted = Arc::new(ScriptedLLM::new("ok"));
        let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
        let conv = engine(&mem, pool);

        for cmd in [":help", ":commands"] {
            match handle_line(cmd, &mem, &conv).await {
                Outcome::Said(s) => {
                    for expected in &[":remember", ":beliefs", ":reflect", ":conflicts",
                                      ":resolve", ":explain", ":tasks", ":task", ":done",
                                      ":consolidate", ":workers", ":quit"] {
                        assert!(s.contains(expected), "{cmd}: missing {expected} in help:\n{s}");
                    }
                }
                Outcome::Quit => panic!("{cmd} should return Said, not Quit"),
            }
        }
    }
}
