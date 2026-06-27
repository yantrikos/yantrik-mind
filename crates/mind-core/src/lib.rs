//! mind-core — the orchestrator + runnable REPL.
//!
//! v1 wires the slice that proves the bet end-to-end: a real LLM backend (chosen at runtime) →
//! `InferencePool` → `ConversationEngine` grounded in the `mind-memory` typed moat. The REPL lets
//! you talk to it and watch memory evolve live (assert beliefs, see contradictions, recall).

use std::sync::Arc;

use mind_conversation::ConversationEngine;
use mind_memory::MemoryHandle;
use mind_types::{BeliefAssertion, MemoryFacade};

pub mod telegram;

/// One REPL line → an outcome. Split out of `main` so it's deterministically testable with a
/// `ScriptedLLM` (no real model).
pub enum Outcome {
    Quit,
    Said(String),
}

/// Handle a single REPL line. Commands start with `:`; anything else is a chat turn.
///   `:remember + <statement>` / `:remember - <statement>`  assert evidence for/against a belief
///   `:conflicts`                                            list open contradictions
///   `:explain <statement>`                                  show a belief + its evidence count
///   `:quit`
pub async fn handle_line(line: &str, mem: &MemoryHandle, conv: &ConversationEngine) -> Outcome {
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
    // plain chat turn
    match conv.handle_turn(t).await {
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

    let mut eng = ConversationEngine::new(memory.clone(), pool.clone(), persona.clone())
        .with_web(Arc::new(mind_tools::HttpFetcher::new()));
    if let Some(m) = &mail_read {
        eng = eng.with_mail(m.clone());
    }
    if let Some(g) = &github_read {
        eng = eng.with_github(g.clone());
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
            .with_web(
                Arc::new(mind_tools::HttpFetcher::new()),
                Arc::new(mind_tools::DdgSearch::new()),
            ),
    );

    // A research sub-agent: web search + fetch + the mind's own read tools. Bounded ReAct, read-only.
    let researcher = mind_agents::SubAgent::new(
        pool.clone(),
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
    let mut recipe_engine = mind_recipes::RecipeEngine::new(pool.clone(), host.clone(), persona.clone());
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
    eng
}

#[cfg(test)]
mod tests {
    use super::*;
    use mind_inference::{InferencePool, ScriptedLLM};
    use yantrik_ml::LLMBackend;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn repl_commands_drive_typed_memory_and_chat() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let scripted = Arc::new(ScriptedLLM::new("ok"));
        let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
        let conv = engine(&mem, pool);

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
}
