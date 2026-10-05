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
    /// E.EGRESS3: what the WEB tools (search, fetch) returned this turn -- the only outside text a web
    /// query may draw its words from. Mail and other external tools stay out: their output is the
    /// person's own, and must not launder into a query.
    web_obs: Vec<String>,
    /// E.EGRESS4: the addresses fetched this turn -- what the fetch planner is told it has, and what
    /// the per-page cap counts.
    fetched: Vec<String>,
}

impl GuardState {
    /// E.EGRESS5: the web text a query may draw on, for tests.
    #[cfg(test)]
    pub(crate) fn web_obs_for_test(&self) -> &[String] {
        &self.web_obs
    }
}

/// E.EGRESS3b: the most web text kept this turn as query sources (the newest kept).
const WEB_OBS_CAP: usize = 32_000;

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

/// What the person reads when an outbound call could not be prepared (E.EGRESSMSG1): which part
/// failed, and that nothing left the device. "safe outbound request" stays in it: it is the marker
/// `Outcome::classify` reads as a gate refusal.
pub(crate) fn egress_refusal(tool: &str, failure: crate::egress_planning::CleanArgsFailure) -> String {
    format!(
        "(I couldn't compose a safe outbound request for {tool}: {} — nothing was sent. Try again, or give me the exact URL or search terms.)",
        failure.why()
    )
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
    let (provenance, web_provenance, fetched) = {
        let s = state.lock().unwrap();
        // E.EGRESS4d: the turn's count when there is a turn; this loop's otherwise.
        let fetched = crate::TURN_FETCHES
            .try_with(|f| f.lock().map(|v| v.clone()).unwrap_or_default())
            .unwrap_or_else(|_| s.fetched.clone());
        (s.external_obs.clone(), s.web_obs.clone(), fetched)
    };
    let asked = grounded.clone();
    // E.EGRESS3e (A3): a handed-over file written since it was named stops being a source first.
    if crate::egress_planning::plans_from_handed(tool) {
        engine.recheck_handed(&ConversationEngine::handed_key(id), id).await;
    }
    let args = match engine
        .egress_clean_args_with(tool, user_text, grounded, &provenance, &web_provenance, &fetched, &ConversationEngine::handed_key(id))
        .await
    {
        Ok(args) => {
            // E.RES1: what actually leaves, when it is not what the model asked -- only what was SENT
            // (the review's L1: the model's args may carry private values; the line above already
            // prints them, and this one adds nothing private that did not leave anyway).
            if args != asked {
                eprintln!("[egress] {ctx}: {tool} sent as {args}");
            }
            args
        }
        Err(failure) => {
            return PreVerdict::Refuse { kind: RefusalKind::EgressUnsafe, msg: egress_refusal(tool, failure) };
        }
    };
    // The high-precision exact-value tripwire — a distinctive stored private value the model
    // injected that the user did not type. Catches the residue clean planning can't.
    if let Some(msg) = engine
        .model_injected_private_value(tool, &args, user_text, id)
        .await
    {
        eprintln!("[egress] {ctx}: blocked exact-value exfil via {tool}");
        return PreVerdict::Refuse {
            kind: RefusalKind::EgressUnsafe,
            msg,
        };
    }
    // E.EGRESS4c (the ninth pass): this turn's message set a task -- a web search or fetch went out on it.
    if crate::egress_planning::plans_from_handed(tool) || matches!(tool, "web_fetch" | "fetch" | "web") {
        if let Ok(mut h) = engine.handed_over.lock() {
            h.note_searched(&ConversationEngine::handed_key(id), user_text);
        }
    }
    // E.EGRESS4: a fetch that goes out is one the next fetch planner knows of, and the cap counts.
    if matches!(tool, "web_fetch" | "fetch" | "web") {
        if let Some(url) = args.get("url").and_then(|u| u.as_str()) {
            let in_turn = crate::TURN_FETCHES.try_with(|f| f.lock().map(|mut v| v.push(url.to_string())).is_ok()).unwrap_or(false);
            if !in_turn {
                state.lock().unwrap().fetched.push(url.to_string());
            }
        }
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
    // A malformed-call refusal is the runtime's own text, not something that came back from outside.
    if outcome != crate::tool_outcome::Outcome::Malformed
        && matches!(
            mind_governance::egress::classify(tool),
            Some(mind_governance::egress::EgressClass::External(_))
        )
    {
        s.external_obs.push_str(obs);
        s.external_obs.push('\n');
        if crate::egress_planning::is_web_tool(tool) {
            // E.EGRESS3b: each observation its own source, the total capped, the newest kept.
            s.web_obs.push(obs.to_string());
            while s.web_obs.iter().map(String::len).sum::<usize>() > WEB_OBS_CAP && s.web_obs.len() > 1 {
                s.web_obs.remove(0);
            }
        }
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
            Arc::new(mind_inference::ScriptedLLM::new(
                r#"{"query":"clean authored"}"#,
            )) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        ConversationEngine::new(
            Arc::new(mem.clone()) as Arc<dyn MemoryFacade>,
            pool,
            "JARVIS",
        )
    }

    /// The ban lifecycle across pre and post: first call proceeds, the unavailable observation
    /// arms the ban, the second call — ANY args — is refused without dispatch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_unavailable_observation_arms_the_ban_for_the_turn() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = engine(&mem);
        let state = Mutex::new(GuardState::default());
        let id = TurnIdentity::primary();

        match pre(
            &eng,
            &state,
            &id,
            "check my PRs",
            "github_repo_items",
            serde_json::json!({"repo":"a/x"}),
            "t",
        )
        .await
        {
            PreVerdict::Proceed(_) => {}
            PreVerdict::Refuse { msg, .. } => panic!("first call must dispatch: {msg}"),
        }
        let o = post(&eng, &state, "github_repo_items", "(github not configured)").await;
        assert_eq!(o, Outcome::Unavailable);
        assert!(is_unavailable(&state, "github_repo_items"));

        match pre(
            &eng,
            &state,
            &id,
            "check my PRs",
            "github_repo_items",
            serde_json::json!({"repo":"a/DIFFERENT"}),
            "t",
        )
        .await
        {
            PreVerdict::Refuse {
                kind: RefusalKind::Unavailable,
                ..
            } => {}
            _ => panic!("a known-unavailable tool must never re-dispatch, whatever the args"),
        }
    }

    /// Provenance flows from post into pre: a web result's address is one the SAME turn may fetch.
    /// E.EGRESS4: which one is the clean planner's pick, never the grounded model's -- so an invented
    /// address never leaves, and the fetch goes to a listed result. This is the parity gap that kept
    /// YM_COGNITION off -- the bus path never had clean-authoring or provenance at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn external_observations_become_egress_provenance() {
        use mind_governance::egress::EgressBroker;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = {
            let pool = mind_inference::InferencePool::new(
                // E.EGRESS4: the fetch planner picks the first listed address.
                Arc::new(mind_inference::ScriptedLLM::new(r#"{"pick": 1}"#)) as Arc<dyn yantrik_ml::LLMBackend>,
                1,
            );
            ConversationEngine::new(
                Arc::new(mem.clone()) as Arc<dyn MemoryFacade>,
                pool,
                "JARVIS",
            )
            .with_egress(Arc::new(EgressBroker::open(std::env::temp_dir(), false)))
        };
        let state = Mutex::new(GuardState::default());
        let id = TurnIdentity::primary();

        // An external search "returned" a link this turn.
        let _ = post(
            &eng,
            &state,
            "search",
            "1. An article — https://example.com/article-42",
        )
        .await;

        // Fetching that link goes to it -- the planner's pick among this turn's web results…
        let v = pre(
            &eng,
            &state,
            &id,
            "research the thing",
            "web_fetch",
            serde_json::json!({"url":"https://example.com/article-42"}),
            "t",
        )
        .await;
        match v {
            PreVerdict::Proceed(args) => assert_eq!(
                args["url"], "https://example.com/article-42",
                "the web result's address was not fetched"
            ),
            PreVerdict::Refuse { msg, .. } => panic!("must proceed: {msg}"),
        }
        // …while an invented one never leaves: the grounded choice is discarded, and the fetch goes
        // to a listed result instead (E.EGRESS4).
        let v = pre(
            &eng,
            &state,
            &id,
            "research the thing",
            "web_fetch",
            serde_json::json!({"url":"https://invented.example/nowhere"}),
            "t",
        )
        .await;
        match v {
            PreVerdict::Proceed(args) => assert_eq!(args["url"], "https://example.com/article-42", "the invented address left"),
            PreVerdict::Refuse { msg, .. } => panic!("the listed result should have been picked: {msg}"),
        }
        // E.EGRESS3: a web tool's output becomes the web provenance a query may draw on; another
        // outside tool's (mail) does not.
        let _ = post(&eng, &state, "mail_search", "From: clinic -- oncology appointment").await;
        let s = state.lock().unwrap();
        assert!(s.web_obs.iter().any(|o| o.contains("example.com/article-42")), "the search's output is not web provenance");
        assert!(!s.web_obs.iter().any(|o| o.contains("oncology")), "mail joined the web provenance");
    }

    /// E.EGRESS3b (the review's ask): the exact-value tripwire still runs after a query passes as a
    /// span of a handed-over file -- a stored private value in that file does not leave through it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_tripwire_still_stops_a_passed_through_query() {
        use mind_governance::egress::EgressBroker;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        mem.remember_as_belief(mind_types::BeliefAssertion {
            statement: "Alice's email is alice.private@example.com".into(),
            polarity: 1.0,
            weight: 1.0,
            source_event: Some("test".into()),
            provenance: "told".into(),
        })
        .await
        .unwrap();
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new(r#"{"query":"nothing"}"#)) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let eng = ConversationEngine::new(Arc::new(mem) as Arc<dyn MemoryFacade>, pool, "JARVIS")
            .with_egress(Arc::new(EgressBroker::open(std::env::temp_dir(), false)))
            .with_home_dir(Some("/home/p".into()))
            .with_mcp(desktop_answering(vec![stat("/home/p/notes/contacts.md", 1_790_602_795); 4]));
        let id = TurnIdentity::primary();
        let key = ConversationEngine::handed_key(&id);
        eng.handed_over.lock().unwrap().note_named(&key, "Read ~/notes/contacts.md", Some("/home/p"), ConversationEngine::now_ms());
        eng.note_handed_over(&key, "/home/p/notes/contacts.md", "contact alice.private@example.com for the project");
        let state = Mutex::new(GuardState::default());
        let v = pre(&eng, &state, &id, "Continue.", "search", serde_json::json!({"query": "alice.private@example.com"}), "t").await;
        assert!(matches!(v, PreVerdict::Refuse { kind: RefusalKind::EgressUnsafe, .. }), "a stored private value left through a handed-over file");
        // A span with no private value in it does leave.
        let ok = pre(&eng, &state, &id, "Continue.", "search", serde_json::json!({"query": "for the project"}), "t").await;
        assert!(matches!(ok, PreVerdict::Proceed(_)));
    }

    /// E.EGRESS3e: a desktop whose os_act answers with these, in order.
    fn desktop_answering(replies: Vec<String>) -> Arc<mind_tools::McpHub> {
        let hub = Arc::new(mind_tools::McpHub::new());
        let tool = mind_tools::McpTool {
            server: "yantrik-os".into(),
            name: "os_act".into(),
            description: "os_act on this computer".into(),
            read_only: true,
            open_world: false,
            destructive: false,
            input_schema: serde_json::json!({"type": "object"}),
        };
        hub.add_scripted_tool(tool, replies.into_iter().map(Ok).collect()).unwrap();
        hub
    }

    /// E.EGRESS3e: `files_stat`'s answer with #654's and #657's fields. SYNTHETIC until 4c's captures.
    fn stat(path: &str, changed: u64) -> String {
        format!("Yantrik \u{2014} desktop screen\naccepted: True, settled: True\n{{\n  \"changed\": {changed},\n  \"exists\": true,\n  \"path\": \"{path}\",\n  \"real\": \"{path}\",\n  \"via_link\": false\n}}")
    }

    /// E.EGRESS3e (A3): before a web search is planned, a handed-over file written since it was named
    /// (its ctime after the naming) stops being a source -- the query no longer leaves as its span.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_file_changed_since_it_was_named_is_no_longer_a_source() {
        use mind_governance::egress::EgressBroker;
        let run = |changed: u64| async move {
            let pool = mind_inference::InferencePool::new(
                Arc::new(mind_inference::ScriptedLLM::new(r#"{"query":"planned"}"#)) as Arc<dyn yantrik_ml::LLMBackend>,
                1,
            );
            let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
            let eng = ConversationEngine::new(Arc::new(mem) as Arc<dyn MemoryFacade>, pool, "JARVIS")
                .with_egress(Arc::new(EgressBroker::open(std::env::temp_dir(), false)))
                .with_home_dir(Some("/home/p".into()))
                .with_mcp(desktop_answering(vec![stat("/home/p/mdg/spec.md", changed)]));
            let id = TurnIdentity::primary();
            let key = ConversationEngine::handed_key(&id);
            eng.handed_over.lock().unwrap().note_named(&key, "Read ~/mdg/spec.md", Some("/home/p"), ConversationEngine::now_ms());
            eng.note_handed_over(&key, "/home/p/mdg/spec.md", "a typed semantic graph");
            let state = Mutex::new(GuardState::default());
            let v = pre(&eng, &state, &id, "Continue.", "search", serde_json::json!({"query": "typed semantic graph"}), "t").await;
            let left = match v {
                PreVerdict::Proceed(a) => a,
                PreVerdict::Refuse { msg, .. } => panic!("{msg}"),
            };
            let kept = eng.handed_over.lock().unwrap().texts(&key, ConversationEngine::now_ms()).len();
            (left, kept)
        };
        let (left, kept) = run(1_790_602_795).await;
        assert_eq!((left, kept), (serde_json::json!({"query": "typed semantic graph"}), 1), "an unchanged file stopped being a source");
        let (left, kept) = run(4_102_444_800).await;
        assert_eq!((left, kept), (serde_json::json!({"query": "planned"}), 0), "a file written after it was named stayed a source");
    }

    /// E.EGRESS4d (the eighth pass): the fetch budget is the TURN's -- a second loop's fresh guard state
    /// does not get a fresh budget -- and it counts registrable domains, so a.evil.com and b.evil.com
    /// share one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_fetch_budget_is_the_turns_and_the_domains() {
        use mind_governance::egress::EgressBroker;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let pool = mind_inference::InferencePool::new(Arc::new(mind_inference::ScriptedLLM::new(r#"{"pick": 1}"#)) as Arc<dyn yantrik_ml::LLMBackend>, 1);
        let eng = ConversationEngine::new(Arc::new(mem) as Arc<dyn MemoryFacade>, pool, "JARVIS")
            .with_egress(Arc::new(EgressBroker::open(std::env::temp_dir(), false)));
        let id = TurnIdentity::primary();
        let results = "1. one -- https://a.evil.com/1  2. two -- https://b.evil.com/2";
        let ask = || serde_json::json!({"url": "https://a.evil.com/1"});
        crate::TURN_FETCHES
            .scope(Default::default(), async {
                let first_loop = Mutex::new(GuardState::default());
                let _ = post(&eng, &first_loop, "search", results).await;
                for _ in 0..2 {
                    assert!(matches!(pre(&eng, &first_loop, &id, "research", "web_fetch", ask(), "t").await, PreVerdict::Proceed(_)));
                }
                // The second loop of the same turn: a fresh guard state, the same turn's count.
                let second_loop = Mutex::new(GuardState::default());
                let _ = post(&eng, &second_loop, "search", results).await;
                match pre(&eng, &second_loop, &id, "research", "web_fetch", ask(), "t").await {
                    PreVerdict::Refuse { msg, .. } => assert!(msg.contains("as many pages as it may"), "{msg}"),
                    PreVerdict::Proceed(a) => panic!("a second loop got a fresh budget: {a}"),
                }
            })
            .await;
    }

    #[test]
    fn the_budget_counts_registrable_domains() {
        use crate::egress_planning::budget_domain as d;
        assert_eq!(d("https://a.evil.com/x"), d("https://b.evil.com/y"));
        assert_eq!(d("https://a.evil.com/x"), "evil.com");
        assert_eq!(d("https://www.example.co.uk./p"), "example.co.uk", "a trailing dot or a public suffix split it");
        assert_eq!(d("https://example.com:443/"), d("https://EXAMPLE.com/"), "the default port or the case split it");
        assert_eq!(d("https://b\u{fc}cher.example/"), d("https://xn--bcher-kva.example/"), "IDN and punycode split it");
        assert_eq!(d("http://93.184.216.34/x"), "93.184.216.34");
        assert_ne!(d("https://evil.com/"), d("https://evil.co.uk/"));
    }

    /// E.EGRESS4c: the task message is a span source, never a "the person typed it" exemption -- a
    /// stored private value in an earlier message is still caught by the tripwire on "Continue.".
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_task_message_does_not_exempt_from_the_tripwire() {
        use mind_governance::egress::EgressBroker;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        mem.remember_as_belief(mind_types::BeliefAssertion {
            statement: "Alice's email is alice.private@example.com".into(),
            polarity: 1.0,
            weight: 1.0,
            source_event: Some("test".into()),
            provenance: "told".into(),
        })
        .await
        .unwrap();
        let pool = mind_inference::InferencePool::new(Arc::new(mind_inference::ScriptedLLM::new(r#"{"query":"nothing"}"#)) as Arc<dyn yantrik_ml::LLMBackend>, 1);
        let eng = ConversationEngine::new(Arc::new(mem) as Arc<dyn MemoryFacade>, pool, "JARVIS")
            .with_egress(Arc::new(EgressBroker::open(std::env::temp_dir(), false)));
        let id = TurnIdentity::primary();
        let key = ConversationEngine::handed_key(&id);
        eng.handed_over.lock().unwrap().note_named(&key, "Look up who alice.private@example.com works for", None, ConversationEngine::now_ms());
        eng.handed_over.lock().unwrap().note_searched(&key, "Look up who alice.private@example.com works for");
        let state = Mutex::new(GuardState::default());
        let v = pre(&eng, &state, &id, "Continue.", "search", serde_json::json!({"query": "alice.private@example.com"}), "t").await;
        assert!(matches!(v, PreVerdict::Refuse { kind: RefusalKind::EgressUnsafe, .. }), "the task message exempted a stored private value");
    }

    /// E.EGRESS3b: this turn's web text is capped in all, the newest kept.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_web_text_a_query_may_use_is_capped() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let pool = mind_inference::InferencePool::new(Arc::new(mind_inference::ScriptedLLM::new("ok")) as Arc<dyn yantrik_ml::LLMBackend>, 1);
        let eng = ConversationEngine::new(Arc::new(mem) as Arc<dyn MemoryFacade>, pool, "JARVIS");
        let state = Mutex::new(GuardState::default());
        for i in 0..4 {
            let _ = post(&eng, &state, "search", &format!("result {i} {}", "x".repeat(15_000))).await;
        }
        let s = state.lock().unwrap();
        assert!(s.web_obs.iter().map(String::len).sum::<usize>() <= WEB_OBS_CAP, "uncapped");
        assert!(s.web_obs.last().is_some_and(|o| o.starts_with("result 3")), "the newest was not kept");
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
        let eng =
            engine(&mem).with_egress(Arc::new(EgressBroker::open(std::env::temp_dir(), false)));
        let state = Mutex::new(GuardState::default());
        let id = TurnIdentity::primary();

        // github is external but NOT clean-authored (not eligible) — exactly the residue the
        // tripwire exists for.
        let v = pre(
            &eng,
            &state,
            &id,
            "check the repo",
            "github_repo_items",
            serde_json::json!({"repo":"a/x","query":"secret.owner@example.com"}),
            "t",
        )
        .await;
        match v {
            PreVerdict::Refuse { kind, msg } => {
                assert_eq!(kind, RefusalKind::EgressUnsafe);
                assert!(msg.contains("private detail"), "{msg}");
            }
            PreVerdict::Proceed(a) => {
                panic!("a stored private value the user never typed must not leave: {a}")
            }
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
        match pre(
            &eng,
            &state,
            &TurnIdentity::primary(),
            "hi",
            "remember",
            wrapped,
            "t",
        )
        .await
        {
            PreVerdict::Proceed(args) => {
                assert_eq!(
                    args,
                    normalize_tool_args(serde_json::json!({"text": [2026, 8, 15]})),
                    "the pipeline output IS the normalized form"
                );
            }
            PreVerdict::Refuse { msg, .. } => panic!("{msg}"),
        }
    }
}
