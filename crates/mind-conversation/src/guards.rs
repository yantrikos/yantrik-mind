//! guards — ONE ordered tool-call guard pipeline, shared by both loops.
//!
//! # Why this module exists
//!
//! The guards used to live inline in `agent_loop`, and the bounded loop's bus re-acquired them one
//! at a time: the outcome classifier forked, the terminal list forked, the exact-value tripwire was
//! simply absent, and egress clean-authoring never arrived at all — each one a separately
//! discovered, separately fixed "the other loop is missing a guard" defect. The DeepSeek Harness
//! shape (every tool call flows through `tools/pre-execute → execute → post-execute`) fixes the
//! CLASS: both loops call [`pre`] before dispatch and [`post`] after it, so a guard added here is
//! on both paths the day it lands, by construction.
//!
//! # What is deliberately NOT here
//!
//! Loop-control: repeat/dedup signatures, barren counting, compose-vs-refuse-step responses. The
//! legacy loop answers a repeat by composing from its work log; the bounded loop's capsule refuses
//! the step and keeps its own attempt history. Those are different, correct responses owned by
//! each loop — forcing them through one seam would flatten a real difference. What IS here is
//! everything where the two loops must never disagree: availability, egress safety, and what an
//! observation did.

use std::collections::HashSet;
use std::sync::Mutex;

use super::{normalize_tool_args, ConversationEngine, TurnIdentity};

/// Per-turn guard state. Owned by whichever loop is running the turn; the pipeline functions take
/// it behind a `Mutex` so the bus (whose `call` is `&self`) and the legacy loop share one shape.
/// Locks are held only around state reads/writes — never across an await.
#[derive(Default)]
pub(crate) struct GuardState {
    /// Tools observed UNAVAILABLE this turn (not configured, no credential). Re-executing one
    /// teaches nothing; the pipeline answers for it instead. Per-turn on purpose: the operator may
    /// configure the tool between turns.
    unavailable: HashSet<String>,
    /// What EXTERNAL services returned this turn — the provenance the egress cleaner may pass a
    /// URL through against. Only external observations accumulate; a private tool's output must
    /// never join, or a stored private link would launder itself into fetchable.
    external_obs: String,
}

/// Why [`pre`] refused, so each loop can respond in its own idiom (the legacy loop counts an
/// unavailable-repeat toward its barren limit; an egress refusal is not barren — the model should
/// try different terms, not fewer).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum RefusalKind {
    /// The tool was already established unavailable this turn.
    Unavailable,
    /// The egress machinery could not produce a safe outbound call (clean-authoring failed closed,
    /// or the exact-value tripwire matched).
    EgressUnsafe,
}

pub(crate) enum PreVerdict {
    /// Dispatch with THESE args — normalized, and for eligible egress tools, clean-authored.
    Proceed(serde_json::Value),
    /// Do not dispatch. `msg` is what the work log / observation should carry.
    Refuse { kind: RefusalKind, msg: String },
}

/// Everything that must happen BEFORE a tool call reaches dispatch, in guard order:
/// availability → arg normalization → egress clean-authoring → exact-value tripwire.
/// `ctx` labels journal lines ("step 3", "bus").
pub(crate) async fn pre(
    engine: &ConversationEngine,
    state: &Mutex<GuardState>,
    id: &TurnIdentity,
    user_text: &str,
    tool: &str,
    raw_args: serde_json::Value,
    ctx: &str,
) -> PreVerdict {
    if state.lock().unwrap().unavailable.contains(tool) {
        eprintln!("[agent] {ctx}: {tool} known unavailable this turn — not re-executed");
        return PreVerdict::Refuse {
            kind: RefusalKind::Unavailable,
            msg: "(unavailable on this box — established earlier this turn; do NOT call it again: use another route or tell the user plainly)".to_string(),
        };
    }
    // Normalise BEFORE the guards and the dispatch: every downstream reader (the egress cleaner,
    // the exact-value guard, the loop signature, the tool itself) assumes plain `{name: value}`,
    // and a content-block wrapper defeats all four at once. Log both shapes — tool arguments have
    // been wrong in three distinct ways while the model was right on the wire, and each time the
    // only evidence was a rendered line printed AFTER normalisation.
    let grounded = normalize_tool_args(raw_args.clone());
    if raw_args != grounded {
        eprintln!("[agent] {ctx}: {tool} args normalised {raw_args} -> {grounded}");
    } else {
        eprintln!("[agent] {ctx}: {tool} raw args {raw_args}");
    }
    // ARCH-3 slice 2: for an eligible EGRESS tool, re-author the args in a clean context that
    // never saw private memory (the grounded args are discarded). None = fail-closed refusal.
    // The provenance snapshot is cloned out of the lock; append-only, so the worst staleness can
    // do is clean-author a URL it could have passed through — the safe direction.
    let provenance = state.lock().unwrap().external_obs.clone();
    let args = match engine.egress_clean_args(tool, user_text, grounded, &provenance).await {
        Some(a) => a,
        None => {
            return PreVerdict::Refuse {
                kind: RefusalKind::EgressUnsafe,
                msg: format!("(I couldn't compose a safe outbound request for {tool} without pulling in private context — tell me the exact terms you want me to search/fetch)"),
            }
        }
    };
    // The high-precision exact-value tripwire — a distinctive stored private value the model
    // injected that the user did not type. Catches the residue clean planning can't.
    if let Some(msg) = engine.model_injected_private_value(tool, &args, user_text, id).await {
        eprintln!("[egress] {ctx}: blocked exact-value exfil via {tool}");
        return PreVerdict::Refuse { kind: RefusalKind::EgressUnsafe, msg };
    }
    PreVerdict::Proceed(args)
}

/// Everything an observation must feed, wherever it was produced: the reliability ledger, the
/// unavailable set, and the egress provenance. Returns the five-way outcome for the loop's own
/// rendering (badge, note, barren accounting).
pub(crate) async fn post(
    engine: &ConversationEngine,
    state: &Mutex<GuardState>,
    tool: &str,
    obs: &str,
) -> crate::tool_outcome::Outcome {
    let outcome = crate::tool_outcome::Outcome::classify(tool, obs);
    // The mind learning its OWN tools: every call's outcome feeds the engine bandit, so
    // reliability is measured self-knowledge — see `tool_outcome` for why this is five-way.
    if let Some(ok) = outcome.counts_toward_reliability() {
        let _ = engine.memory.record_tool_outcome(tool, ok).await;
    }
    let mut s = state.lock().unwrap();
    if outcome == crate::tool_outcome::Outcome::Unavailable {
        s.unavailable.insert(tool.to_string());
    }
    // Only EXTERNAL tools feed the egress provenance: what came back from the outside world is
    // already outside.
    if matches!(mind_governance::egress::classify(tool), Some(mind_governance::egress::EgressClass::External(_))) {
        s.external_obs.push_str(obs);
        s.external_obs.push('\n');
    }
    outcome
}

/// Is this tool already known unavailable? (The legacy loop's identical-repeat special case needs
/// the answer before it decides between composing and refusing the step.)
pub(crate) fn is_unavailable(state: &Mutex<GuardState>, tool: &str) -> bool {
    state.lock().unwrap().unavailable.contains(tool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_outcome::Outcome;
    use mind_memory::MemoryHandle;
    use mind_types::MemoryFacade;
    use std::sync::Arc;

    fn engine(mem: &MemoryHandle) -> ConversationEngine {
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new(r#"{"query":"clean authored"}"#)) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        ConversationEngine::new(Arc::new(mem.clone()) as Arc<dyn MemoryFacade>, pool, "JARVIS")
    }

    /// The ban lifecycle across pre and post: first call proceeds, the unavailable observation
    /// arms the ban, the second call — ANY args — is refused without dispatch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_unavailable_observation_arms_the_ban_for_the_turn() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = engine(&mem);
        let state = Mutex::new(GuardState::default());
        let id = TurnIdentity::primary();

        match pre(&eng, &state, &id, "check my PRs", "github_repo_items", serde_json::json!({"repo":"a/x"}), "t").await {
            PreVerdict::Proceed(_) => {}
            PreVerdict::Refuse { msg, .. } => panic!("first call must dispatch: {msg}"),
        }
        let o = post(&eng, &state, "github_repo_items", "(github not configured)").await;
        assert_eq!(o, Outcome::Unavailable);
        assert!(is_unavailable(&state, "github_repo_items"));

        match pre(&eng, &state, &id, "check my PRs", "github_repo_items", serde_json::json!({"repo":"a/DIFFERENT"}), "t").await {
            PreVerdict::Refuse { kind: RefusalKind::Unavailable, .. } => {}
            _ => panic!("a known-unavailable tool must never re-dispatch, whatever the args"),
        }
    }

    /// Provenance flows from post into pre: an external observation containing a URL lets the SAME
    /// turn fetch that URL exactly as chosen, while an unprovenanced URL still gets clean-authored.
    /// This is the parity gap that kept YM_COGNITION off — the bus path never had clean-authoring
    /// or provenance at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn external_observations_become_egress_provenance() {
        use mind_governance::egress::EgressBroker;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = {
            let pool = mind_inference::InferencePool::new(
                // The clean planner is scripted to MANGLE any url — pass-through is only provable
                // when this reply does NOT come back.
                Arc::new(mind_inference::ScriptedLLM::new(r#"{"url":"https://mangled.example/x"}"#)) as Arc<dyn yantrik_ml::LLMBackend>,
                1,
            );
            ConversationEngine::new(Arc::new(mem.clone()) as Arc<dyn MemoryFacade>, pool, "JARVIS")
                .with_egress(Arc::new(EgressBroker::open(std::env::temp_dir(), false)))
        };
        let state = Mutex::new(GuardState::default());
        let id = TurnIdentity::primary();

        // An external search "returned" a link this turn.
        let _ = post(&eng, &state, "search", "1. An article — https://example.com/article-42").await;

        // Fetching that link passes through untouched…
        let v = pre(&eng, &state, &id, "research the thing", "web_fetch", serde_json::json!({"url":"https://example.com/article-42"}), "t").await;
        match v {
            PreVerdict::Proceed(args) => assert_eq!(args["url"], "https://example.com/article-42", "provenanced URL must dispatch exactly as chosen"),
            PreVerdict::Refuse { msg, .. } => panic!("must proceed: {msg}"),
        }
        // …while an invented one is re-authored by the clean planner.
        let v = pre(&eng, &state, &id, "research the thing", "web_fetch", serde_json::json!({"url":"https://invented.example/nowhere"}), "t").await;
        match v {
            PreVerdict::Proceed(args) => assert_eq!(args["url"], "https://mangled.example/x", "an unprovenanced URL must be clean-authored"),
            PreVerdict::Refuse { msg, .. } => panic!("clean-authoring should have produced args: {msg}"),
        }
    }

    /// The exact-value tripwire refuses through the pipeline, with the egress kind — so a loop
    /// never counts it as the tool's fault or as a barren repeat.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_model_injected_private_value_refuses_as_egress_unsafe() {
        use mind_governance::egress::EgressBroker;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let _ = mem
            .remember_as_belief(mind_types::BeliefAssertion {
                statement: "Pranab's private email is secret.owner@example.com".into(),
                polarity: 1.0,
                weight: 1.5,
                source_event: None,
                provenance: "told".into(),
            })
            .await;
        let eng = engine(&mem).with_egress(Arc::new(EgressBroker::open(std::env::temp_dir(), false)));
        let state = Mutex::new(GuardState::default());
        let id = TurnIdentity::primary();

        // github is external but NOT clean-authored (not eligible) — exactly the residue the
        // tripwire exists for.
        let v = pre(&eng, &state, &id, "check the repo", "github_repo_items", serde_json::json!({"repo":"a/x","query":"secret.owner@example.com"}), "t").await;
        match v {
            PreVerdict::Refuse { kind, msg } => {
                assert_eq!(kind, RefusalKind::EgressUnsafe);
                assert!(msg.contains("private detail"), "{msg}");
            }
            PreVerdict::Proceed(a) => panic!("a stored private value the user never typed must not leave: {a}"),
        }
    }

    /// Normalization happens inside the pipeline, so the bus path gets it too — a content-block
    /// wrapper must never reach a tool from either loop.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn args_are_normalized_for_every_caller() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = engine(&mem);
        let state = Mutex::new(GuardState::default());
        let wrapped = serde_json::json!({"text": [2026, 8, 15]});
        match pre(&eng, &state, &TurnIdentity::primary(), "hi", "remember", wrapped, "t").await {
            PreVerdict::Proceed(args) => {
                assert_eq!(args, normalize_tool_args(serde_json::json!({"text": [2026, 8, 15]})), "the pipeline output IS the normalized form");
            }
            PreVerdict::Refuse { msg, .. } => panic!("{msg}"),
        }
    }
}
