//! L4-0 — the spend ledger's behaviour at the seam: exactly one terminal row per logical
//! request (served / refused / failed), attempts counted at the observable boundary, the
//! streaming path the same, a private-grounded escalation as two intentional rows, the loop
//! opportunity carried by identity, and the oracle the ledger did not write (the household
//! callsite counters) agreeing over one process lifetime.
use crate::*;
use mind_inference::{household_callsite_stats, within_opportunity, PrivacyScope, ScriptedLLM};
use mind_memory::MemoryHandle;
use mind_observability::{parse_inference_call, DecisionLog, InferenceOutcome};
use mind_recipes::RecipeStore;
use yantrik_ml::{ChatMessage, GenerationConfig, LLMBackend, LLMResponse};

struct NoTools;
#[async_trait::async_trait]
impl RecipeHost for NoTools {
    async fn call_tool(&self, _tool: &str, _args: &serde_json::Value) -> anyhow::Result<String> {
        anyhow::bail!("no tools in this fixture")
    }
}

/// A backend that fails `fail_first` invocations with the given error text, then serves.
struct FlakyLLM {
    fail_first: usize,
    error: &'static str,
    calls: std::sync::atomic::AtomicUsize,
}
impl LLMBackend for FlakyLLM {
    fn chat(
        &self,
        _messages: &[ChatMessage],
        _config: &GenerationConfig,
        _tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n < self.fail_first {
            anyhow::bail!("{}", self.error);
        }
        Ok(LLMResponse {
            text: "ok".into(),
            stop_reason: "stop".into(),
            ..Default::default()
        })
    }
    fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<LLMResponse> {
        let r = LLMBackend::chat(self, messages, config, tools)?;
        on_token(&r.text);
        Ok(r)
    }
    fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
        Ok(text.len() / 4)
    }
    fn backend_name(&self) -> &str {
        "scripted"
    }
}

fn harness_with(
    backend: Arc<dyn LLMBackend>,
) -> (ConversationEngine, Arc<DecisionLog>, InferencePool) {
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(backend, 1);
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let recipes =
        RecipeEngine::new(pool.clone(), Arc::new(NoTools), "JARVIS").with_store(store.clone());
    let path = std::env::temp_dir().join(format!(
        "ym-l4-0-{}-{}.jsonl",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let log = Arc::new(DecisionLog::open(path));
    let conv = ConversationEngine::new(mem, pool.clone(), "JARVIS")
        .with_recipes(Arc::new(recipes))
        .with_recorder(log.clone());
    (conv, log, pool)
}

fn spend_rows(log: &DecisionLog) -> Vec<mind_observability::ParsedInferenceCall> {
    let events = log.read_all();
    let calls: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "inference_call")
        .collect();
    let parsed: Vec<_> = calls
        .iter()
        .filter_map(|e| parse_inference_call(e))
        .collect();
    assert_eq!(
        parsed.len(),
        calls.len(),
        "every written row parses under the reader"
    );
    parsed
}

fn user(text: &str) -> Vec<ChatMessage> {
    vec![ChatMessage::user(text)]
}

/// One logical request, one row: served through the household lane with one attempt on a leaf
/// backend; the same through the streaming path; refused at the gate with zero attempts.
#[tokio::test]
async fn one_logical_request_writes_one_terminal_row_on_every_path() {
    let (conv, log, _pool) = harness_with(Arc::new(ScriptedLLM::new("fine")));
    conv.inference
        .chat_household_attributed(
            user("hello"),
            GenerationConfig::default(),
            concat!(module_path!(), ":served"),
        )
        .await
        .unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    conv.inference
        .chat_streaming_sink(
            user("hello"),
            GenerationConfig::default(),
            tx,
            PrivacyScope::Household,
        )
        .await
        .unwrap();
    // Private with no private lane configured: the gate refuses (nothing dispatched).
    assert!(conv
        .inference
        .chat_scoped(
            user("secret"),
            GenerationConfig::default(),
            PrivacyScope::Private
        )
        .await
        .is_err());
    let rows = spend_rows(&log);
    assert_eq!(rows.len(), 3);
    let served = &rows[0];
    assert_eq!(
        (
            served.callsite.as_str(),
            served.outcome,
            served.attempts,
            served.streaming
        ),
        (
            concat!(module_path!(), ":served"),
            InferenceOutcome::Served,
            1,
            false
        )
    );
    assert_eq!(served.served_by.as_deref(), Some("scripted"));
    assert_eq!(served.route, "scripted");
    let stream = &rows[1];
    assert_eq!(
        (
            stream.callsite.as_str(),
            stream.outcome,
            stream.attempts,
            stream.streaming
        ),
        ("chat_streaming_sink", InferenceOutcome::Served, 1, true)
    );
    let refused = &rows[2];
    assert_eq!(
        (
            refused.outcome,
            refused.attempts,
            refused.served_by.is_none()
        ),
        (InferenceOutcome::Refused, 0, true)
    );
    assert_eq!(refused.lane, mind_observability::InferenceLane::Private);
    assert!(rows.iter().all(|r| r.opportunity.is_none()));
    assert!(rows
        .windows(2)
        .all(|w| w[0].process_start_ms == w[1].process_start_ms));
}

/// Attempts are backend invocations at the observable boundary: a transient error retried by
/// the pool then served is ONE served row with attempts 2; a non-transient error is one failed
/// row with attempts 1; three transient errors are one failed row with attempts 3.
#[tokio::test]
async fn attempts_count_backend_invocations_and_a_retry_that_serves_is_one_served_row() {
    let flaky = Arc::new(FlakyLLM {
        fail_first: 1,
        error: "HTTP 502 gateway",
        calls: Default::default(),
    });
    let (conv, log, _pool) = harness_with(flaky);
    conv.inference
        .chat_household_attributed(
            user("x"),
            GenerationConfig::default(),
            concat!(module_path!(), ":retry"),
        )
        .await
        .unwrap();
    let rows = spend_rows(&log);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].outcome, rows[0].attempts),
        (InferenceOutcome::Served, 2)
    );

    let hard = Arc::new(FlakyLLM {
        fail_first: 9,
        error: "bad request 400",
        calls: Default::default(),
    });
    let (conv, log, _pool) = harness_with(hard);
    assert!(conv
        .inference
        .chat_household_attributed(
            user("x"),
            GenerationConfig::default(),
            concat!(module_path!(), ":hard")
        )
        .await
        .is_err());
    let rows = spend_rows(&log);
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].outcome,
            rows[0].attempts,
            rows[0].served_by.is_none()
        ),
        (InferenceOutcome::Failed, 1, true)
    );

    let down = Arc::new(FlakyLLM {
        fail_first: 9,
        error: "HTTP 503",
        calls: Default::default(),
    });
    let (conv, log, _pool) = harness_with(down);
    assert!(conv
        .inference
        .chat_household_attributed(
            user("x"),
            GenerationConfig::default(),
            concat!(module_path!(), ":down")
        )
        .await
        .is_err());
    let rows = spend_rows(&log);
    assert_eq!(
        (rows.len(), rows[0].outcome, rows[0].attempts),
        (1, InferenceOutcome::Failed, 3)
    );
}

/// A private-grounded call with no private lane escalates: a refused PRIVATE row and a served
/// HOUSEHOLD row — two logical requests in two lanes, intentional, never a duplicate.
#[tokio::test]
async fn a_private_grounded_escalation_is_two_intentional_rows_in_two_lanes() {
    let (conv, log, _pool) = harness_with(Arc::new(ScriptedLLM::new("fine")));
    conv.inference
        .chat_grounded(user("family"), GenerationConfig::default())
        .await
        .unwrap();
    let rows = spend_rows(&log);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (rows[0].lane, rows[0].outcome, rows[0].attempts),
        (
            mind_observability::InferenceLane::Private,
            InferenceOutcome::Refused,
            0
        )
    );
    assert_eq!(
        (rows[1].lane, rows[1].outcome, rows[1].attempts),
        (
            mind_observability::InferenceLane::Household,
            InferenceOutcome::Served,
            1
        )
    );
    assert_eq!(rows[0].callsite, "private-grounded");
}

/// The loop opportunity rides on the row by identity when the host sets it, and the reducer
/// attributes the request to that loop; a call outside any opportunity carries none.
#[tokio::test]
async fn the_loop_opportunity_rides_on_the_row_by_identity() {
    let (conv, log, _pool) = harness_with(Arc::new(ScriptedLLM::new("fine")));
    let opp = mind_observability::LoopOpportunity::Window {
        loop_id: mind_observability::LoopId::Dmn,
        process_start_ms: 1000,
        key: 5,
    };
    within_opportunity(opp.id(), async {
        conv.inference
            .chat_household_attributed(
                user("x"),
                GenerationConfig::default(),
                concat!(module_path!(), ":in-loop"),
            )
            .await
            .unwrap();
    })
    .await;
    conv.inference
        .chat_household_attributed(
            user("x"),
            GenerationConfig::default(),
            concat!(module_path!(), ":outside"),
        )
        .await
        .unwrap();
    let rows = spend_rows(&log);
    assert_eq!(rows[0].opportunity, Some(opp));
    assert_eq!(rows[1].opportunity, None);
    let ledger = mind_observability::spend_ledger(
        &log.read_all(),
        mind_observability::now_ms() + 1,
        3_600_000,
    );
    assert_eq!(ledger.by_loop.get("dmn"), Some(&(1, 1)));
    assert_eq!(ledger.unattributed_requests, 1);
}

/// The oracle the ledger did not write: over one process lifetime, served + failed household
/// rows for a callsite equal that callsite's household dispatch counter delta.
#[tokio::test]
async fn served_plus_failed_household_rows_equal_the_callsite_counter() {
    const SITE: &str = concat!(module_path!(), ":oracle");
    let site: &'static str = SITE;
    let before = household_callsite_stats()
        .into_iter()
        .find(|(k, _)| k == site)
        .map_or(0, |(_, n)| n);
    let flaky = Arc::new(FlakyLLM {
        fail_first: 1,
        error: "bad request 400",
        calls: Default::default(),
    });
    let (conv, log, _pool) = harness_with(flaky);
    let _ = conv
        .inference
        .chat_household_attributed(user("a"), GenerationConfig::default(), site)
        .await;
    conv.inference
        .chat_household_attributed(user("b"), GenerationConfig::default(), site)
        .await
        .unwrap();
    conv.inference
        .chat_household_attributed(user("c"), GenerationConfig::default(), site)
        .await
        .unwrap();
    let after = household_callsite_stats()
        .into_iter()
        .find(|(k, _)| k == site)
        .map_or(0, |(_, n)| n);
    let rows = spend_rows(&log);
    let ledger_count = rows
        .iter()
        .filter(|r| r.callsite == site && r.lane == mind_observability::InferenceLane::Household)
        .filter(|r| {
            matches!(
                r.outcome,
                InferenceOutcome::Served | InferenceOutcome::Failed
            )
        })
        .count() as u64;
    assert_eq!(ledger_count, 3);
    assert_eq!(
        after - before,
        ledger_count,
        "the counter the ledger did not write agrees"
    );
}

/// A backend that panics inside the call — the blocking task dies with a JoinError.
struct PanickingLLM;
impl LLMBackend for PanickingLLM {
    fn chat(
        &self,
        _messages: &[ChatMessage],
        _config: &GenerationConfig,
        _tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        panic!("backend panicked mid-call");
    }
    fn chat_streaming(
        &self,
        _messages: &[ChatMessage],
        _config: &GenerationConfig,
        _tools: Option<&[serde_json::Value]>,
        _on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<LLMResponse> {
        panic!("backend panicked mid-stream");
    }
    fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
        Ok(text.len() / 4)
    }
    fn backend_name(&self) -> &str {
        "scripted"
    }
}

/// A backend panic on the blocking thread is a FAILED request with the attempt it was making —
/// never a missing row — on the plain and the streaming path alike.
#[tokio::test]
async fn a_backend_panic_is_one_failed_row_with_its_attempt_on_both_paths() {
    let (conv, log, _pool) = harness_with(Arc::new(PanickingLLM));
    assert!(conv
        .inference
        .chat_household_attributed(
            user("x"),
            GenerationConfig::default(),
            concat!(module_path!(), ":panic")
        )
        .await
        .is_err());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(conv
        .inference
        .chat_streaming_sink(
            user("x"),
            GenerationConfig::default(),
            tx,
            PrivacyScope::Household
        )
        .await
        .is_err());
    let rows = spend_rows(&log);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        (
            rows[0].outcome,
            rows[0].attempts,
            rows[0].streaming,
            rows[0].served_by.is_none()
        ),
        (InferenceOutcome::Failed, 1, false, true)
    );
    assert_eq!(
        (rows[1].outcome, rows[1].attempts, rows[1].streaming),
        (InferenceOutcome::Failed, 1, true)
    );
}

/// One process, one identity: two engines bound at different moments stamp the same pinned
/// process start on their rows, and it is the process's own.
#[tokio::test]
async fn every_engine_in_one_process_stamps_the_same_pinned_process_start() {
    let (a, log_a, _) = harness_with(Arc::new(ScriptedLLM::new("a")));
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let (b, log_b, _) = harness_with(Arc::new(ScriptedLLM::new("b")));
    a.inference
        .chat_household_attributed(
            user("x"),
            GenerationConfig::default(),
            concat!(module_path!(), ":pid-a"),
        )
        .await
        .unwrap();
    b.inference
        .chat_household_attributed(
            user("x"),
            GenerationConfig::default(),
            concat!(module_path!(), ":pid-b"),
        )
        .await
        .unwrap();
    let ra = spend_rows(&log_a);
    let rb = spend_rows(&log_b);
    assert_eq!(ra[0].process_start_ms, rb[0].process_start_ms);
    assert_eq!(ra[0].process_start_ms, crate::process_started_ms());
}

/// The named critic pool joins the house pool's ledger family — asserted in source, since the
/// critic is built from an env spec: the constructor shares the slot and both call sites pass
/// the house pool.
#[test]
fn the_named_critic_records_into_the_house_ledger() {
    let src = include_str!("delegate.rs");
    let ctor_at = src
        .find("pub(crate) fn critic_from_env(family: &InferencePool)")
        .expect("ctor");
    let ctor = &src[ctor_at..ctor_at + 700];
    assert!(ctor.contains(".share_ledger_slot(family)"));
    assert_eq!(
        src.matches("critic_from_env(&house)").count(),
        2,
        "both call sites"
    );
    assert_eq!(
        src.matches("critic_from_env()").count(),
        0,
        "no unshared critic"
    );
}

/// A derived (role) pool records into the same ledger as the pool it came from, and `why
/// spend` renders from the verified log.
#[tokio::test]
async fn role_pools_share_the_ledger_and_why_spend_reads_it() {
    let (conv, log, pool) = harness_with(Arc::new(ScriptedLLM::new("fine")));
    let role = InferencePool::new(Arc::new(ScriptedLLM::new("role")), 1).share_ledger_slot(&pool);
    role.chat_household_attributed(
        user("x"),
        GenerationConfig::default(),
        concat!(module_path!(), ":role"),
    )
    .await
    .unwrap();
    assert_eq!(spend_rows(&log).len(), 1);
    let text = conv
        .cli_dispatch("why spend", &mind_types::AccessContext::operator_audit())
        .await;
    assert!(text.contains(concat!(module_path!(), ":role")), "{text}");
    assert!(text.contains("Tokens: absent"));
}
