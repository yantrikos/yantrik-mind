//! E.PORT1-B — a delegation must exist on the board before it asks a model anything.
//!
//! The measured failure: every request to a provider was rejected, the routing call never returned,
//! and because the ledger row was written AFTER routing, an accepted delegation stayed invisible for
//! thirty minutes — no job on the board, nothing for the poller to see, no error anywhere. These
//! tests drive the real engine with a backend that never answers, which is the only way to tell a
//! bounded acknowledgement from a lucky fast one.

use crate::delegate::{bounded_route, ROUTE_BUDGET};
use crate::*;
use mind_memory::MemoryHandle;
use mind_recipes::RecipeStore;
use std::sync::Arc;
use yantrik_ml::{ChatMessage, GenerationConfig, LLMBackend, LLMResponse};

struct NoTools;
#[async_trait::async_trait]
impl RecipeHost for NoTools {
    async fn call_tool(&self, _tool: &str, _args: &serde_json::Value) -> anyhow::Result<String> {
        anyhow::bail!("no tools in this fixture")
    }
}

/// A provider that accepts the request and never answers — the shape of a blocking HTTP attempt
/// inside `spawn_blocking`, which is what actually happened on NIM.
struct NeverAnswers {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl LLMBackend for NeverAnswers {
    fn chat(
        &self,
        _messages: &[ChatMessage],
        _config: &GenerationConfig,
        _tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Long enough that a test asserting a bound cannot pass by racing it.
        std::thread::sleep(std::time::Duration::from_secs(3));
        anyhow::bail!("never answers")
    }
    fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
        _on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<LLMResponse> {
        self.chat(messages, config, tools)
    }
    fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
        Ok(text.len() / 4)
    }
    fn backend_name(&self) -> &str {
        "never-answers"
    }
}

/// `two_kinds` decides whether the ROUTER is consulted at all: with one executor there is nothing
/// to choose between, and asking a model to pick from a menu of one would be a call for nothing.
fn engine_with(
    backend: Arc<dyn LLMBackend>,
    two_kinds: bool,
) -> (ConversationEngine, Arc<dyn MemoryFacade>) {
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(backend, 2);
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let recipes =
        RecipeEngine::new(pool.clone(), Arc::new(NoTools), "JARVIS").with_store(store.clone());
    let mut conv = ConversationEngine::new(mem.clone(), pool.clone(), "JARVIS")
        .with_recipes(Arc::new(recipes))
        // A 300 ms budget drives the same timeout path a 20 s one does, without making every suite
        // run wait it out. The backend still takes an order of magnitude longer to answer, so a
        // bounded acknowledgement cannot pass by racing it.
        .with_route_budget(std::time::Duration::from_millis(300));
    if two_kinds {
        conv = conv.with_researcher(Arc::new(mind_agents::SubAgent::new(
            pool,
            Arc::new(NoTools),
            "JARVIS",
            vec!["recall".into()],
            2,
        )));
    }
    (conv, mem)
}

async fn board(mem: &Arc<dyn MemoryFacade>) -> Vec<serde_json::Value> {
    mem.profile_get("delegations")
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delegation_is_acknowledged_and_on_the_board_while_the_model_never_answers() {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (conv, mem) = engine_with(
        Arc::new(NeverAnswers {
            calls: calls.clone(),
        }),
        true,
    );

    let started = std::time::Instant::now();
    let reply = conv
        .delegate_cmd("pagejob: build a one page portfolio site with four project cards")
        .await;
    let elapsed = started.elapsed();

    // BOUNDED ACKNOWLEDGEMENT: the person who typed this is owed an answer in seconds. The failure
    // this pins had no bound at all — the caller waited until an 1800 s harness wall.
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "acknowledgement took {elapsed:?} against a 300 ms routing budget and a backend that needs          seconds: not bounded by the budget"
    );
    assert!(
        ROUTE_BUDGET >= std::time::Duration::from_secs(5),
        "the production budget stays a human-scale wait, not a millisecond one"
    );
    assert!(
        !reply.contains("isn't configured"),
        "the page executor is configured in this fixture: {reply}"
    );

    // TRUTHFUL JOB STATE: the board knows about the job, with a kind its executor supports.
    let rows = board(&mem).await;
    assert_eq!(
        rows.len(),
        1,
        "exactly one delegation on the board: {rows:?}"
    );
    let row = &rows[0];
    assert_eq!(row["name"], "pagejob");
    assert_eq!(
        row["kind"], "page",
        "the deterministic floor decided the kind, not the model that never answered"
    );
    assert_eq!(row["status"], "running");
    assert!(
        row["id"].as_str().is_some_and(|i| !i.is_empty()),
        "the job has an id: {row:?}"
    );

    // NO HIDDEN WORK: the routing request really was issued, so the timeout notice is not
    // hypothetical — it is reporting an attempt that is still out there.
    assert!(
        calls.load(std::sync::atomic::Ordering::Relaxed) >= 1,
        "the routing call was made and abandoned, which is the case the notice exists for"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_routing_budget_falls_back_to_the_floor_and_says_so_exactly_once() {
    let fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let f = fired.clone();
    let never = async {
        std::future::pending::<()>().await;
        "code"
    };
    let started = std::time::Instant::now();
    let kind = bounded_route(
        never,
        "research",
        std::time::Duration::from_millis(50),
        || {
            f.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        },
    )
    .await;
    assert_eq!(kind, "research", "the floor stands when routing times out");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(
        fired.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the operator is told once, not never and not repeatedly"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_routing_answer_within_budget_is_used_and_nothing_is_reported() {
    let fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let f = fired.clone();
    let quick = async { "code" };
    let kind = bounded_route(
        quick,
        "research",
        std::time::Duration::from_secs(30),
        || {
            f.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        },
    )
    .await;
    assert_eq!(kind, "code", "a routing answer refines the floor");
    assert_eq!(
        fired.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "nothing to report when the router answered in time"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_one_executor_the_router_is_not_consulted_at_all() {
    // A menu of one needs no model. This is why the first version of the test above saw zero calls:
    // worth pinning, because a routing call for a decision with one possible answer would be pure
    // latency on every delegation on a single-executor box.
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (conv, mem) = engine_with(
        Arc::new(NeverAnswers {
            calls: calls.clone(),
        }),
        false,
    );
    let started = std::time::Instant::now();
    let _ = conv
        .delegate_cmd("solo: build a one page portfolio site")
        .await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "no router, no wait"
    );
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the router must not be asked to choose from a menu of one"
    );
    let rows = board(&mem).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"], "page");
}
