//! E.PORT1-B — a delegation must exist on the board before it asks a model anything.
//!
//! The measured failure: every request to a provider was rejected, the routing call never returned,
//! and because the ledger row was written AFTER routing, an accepted delegation stayed invisible for
//! thirty minutes — no job on the board, nothing for the poller to see, no error anywhere.
//!
//! These tests inspect the board WHILE the routing call is still blocked, which is the only way to
//! tell "the row is written first" from "the wait is merely bounded". A first version of this fix
//! bounded the wait and still wrote the row afterwards; a test that only looked after the call
//! returned could not see the difference, and did not.

use crate::delegate::{bounded_route, ROUTE_BUDGET};
use crate::*;
use mind_memory::MemoryHandle;
use mind_recipes::RecipeStore;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use yantrik_ml::{ChatMessage, GenerationConfig, LLMBackend, LLMResponse};

struct NoTools;
#[async_trait::async_trait]
impl RecipeHost for NoTools {
    async fn call_tool(&self, _tool: &str, _args: &serde_json::Value) -> anyhow::Result<String> {
        anyhow::bail!("no tools in this fixture")
    }
}

/// A provider that accepts a request, announces that it has begun, and answers nothing until it is
/// released — the shape of a blocking HTTP attempt that never returns.
///
/// Released explicitly rather than by a long sleep: the runtime waits for spawned blocking work at
/// shutdown, so a sleeping backend makes the whole suite wait with it.
struct HeldBackend {
    /// One-shot "a call has begun". A `std::sync::Barrier` was wrong here: it CYCLES, so a second
    /// call to this backend would wait forever for a partner that never comes, on a blocking thread
    /// the runtime joins at shutdown — a hung suite rather than a red test. Review caught it before
    /// it fired. This signal is set once and every later call passes straight through.
    started: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    calls: Arc<AtomicUsize>,
}

/// Wait for a one-shot flag, with a ceiling so a failed assertion can never hang a test runner.
fn wait_flag(flag: &(std::sync::Mutex<bool>, std::sync::Condvar), secs: u64) -> bool {
    let (lock, cv) = flag;
    let mut set = lock.lock().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while !*set {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return false;
        }
        let (g, _t) = cv.wait_timeout(set, left).unwrap();
        set = g;
    }
    true
}

fn set_flag(flag: &(std::sync::Mutex<bool>, std::sync::Condvar)) {
    let (lock, cv) = flag;
    *lock.lock().unwrap() = true;
    cv.notify_all();
}

impl LLMBackend for HeldBackend {
    fn chat(
        &self,
        _messages: &[ChatMessage],
        _config: &GenerationConfig,
        _tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        set_flag(&self.started);
        wait_flag(&self.release, 20);
        anyhow::bail!("this provider never answers")
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
        "held"
    }
}

struct Fixture {
    conv: ConversationEngine,
    mem: Arc<dyn MemoryFacade>,
    started: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    calls: Arc<AtomicUsize>,
}

impl Fixture {
    fn release(&self) {
        set_flag(&self.release);
    }
}

/// `two_kinds` decides whether the ROUTER is consulted at all: with one executor there is nothing to
/// choose between.
fn fixture(two_kinds: bool, budget: std::time::Duration) -> Fixture {
    let started = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(HeldBackend {
        started: started.clone(),
        release: release.clone(),
        calls: calls.clone(),
    }) as Arc<dyn LLMBackend>;

    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(backend, 2);
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let recipes =
        RecipeEngine::new(pool.clone(), Arc::new(NoTools), "JARVIS").with_store(store.clone());
    let mut conv = ConversationEngine::new(mem.clone(), pool.clone(), "JARVIS")
        .with_recipes(Arc::new(recipes))
        .with_route_budget(budget);
    if two_kinds {
        conv = conv.with_researcher(Arc::new(mind_agents::SubAgent::new(
            pool,
            Arc::new(NoTools),
            "JARVIS",
            vec!["recall".into()],
            2,
        )));
    }
    Fixture {
        conv,
        mem,
        started,
        release,
        calls,
    }
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
async fn the_job_is_on_the_board_while_the_routing_call_is_still_blocked() {
    // THE test. Anything that only looks after delegate_cmd returns cannot tell this fix from a
    // merely bounded wait, and the first version of this change failed exactly there.
    let f = fixture(true, std::time::Duration::from_secs(30));
    let mem = f.mem.clone();
    let started = f.started.clone();
    let release = f.release.clone();
    let calls = f.calls.clone();

    let conv = f.conv;
    let call = tokio::spawn(async move {
        conv.delegate_cmd("pagejob: build a one page portfolio site with four project cards")
            .await
    });

    // Wait until the provider has actually been called and is holding the request open.
    let began = tokio::task::spawn_blocking(move || wait_flag(&started, 20))
        .await
        .expect("join");
    assert!(began, "the provider was never called");

    // The routing model is now blocked and cannot answer. The board must ALREADY know about the job.
    let rows = board(&mem).await;
    assert_eq!(
        rows.len(),
        1,
        "the job must be on the board before the model answers: {rows:?}"
    );
    assert_eq!(rows[0]["name"], "pagejob");
    assert_eq!(
        rows[0]["kind"], "page",
        "the deterministic floor names the kind while routing is still out"
    );
    assert_eq!(rows[0]["status"], "running");
    assert!(rows[0]["id"].as_str().is_some_and(|i| !i.is_empty()));

    {
        let (lock, cv) = &*release;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
    let reply = call.await.expect("delegate_cmd finished");
    assert!(!reply.contains("configured"), "{reply}");
    assert!(
        calls.load(Ordering::Relaxed) >= 1,
        "the routing call really was made"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_acknowledgement_is_bounded_by_the_routing_budget() {
    // A 300 ms budget against a provider that will not answer until released: the acknowledgement
    // must come back on the budget, not on the provider.
    let f = fixture(true, std::time::Duration::from_millis(300));

    let t0 = std::time::Instant::now();
    let reply = f.conv.delegate_cmd("pagejob: build a portfolio page").await;
    let elapsed = t0.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "acknowledgement took {elapsed:?} against a 300 ms budget and a provider that never answers"
    );
    assert!(!reply.contains("configured"), "{reply}");
    assert!(
        ROUTE_BUDGET >= std::time::Duration::from_secs(5),
        "the production budget stays a human-scale wait, not a millisecond one"
    );
    f.release();
}

/// A backend that only counts. The held one would block a test that is about a call which must
/// never happen at all.
struct CountingBackend {
    calls: Arc<AtomicUsize>,
}

impl LLMBackend for CountingBackend {
    fn chat(
        &self,
        _messages: &[ChatMessage],
        _config: &GenerationConfig,
        _tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        anyhow::bail!("nothing should ask this backend anything")
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
        "counting"
    }
    fn model_id(&self) -> &str {
        "counting"
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_page_brief_on_a_recipe_box_still_runs_as_a_page() {
    // THIS TEST'S PREMISE CHANGED and the change is the point. It used to say "one executor, so no
    // router" — but E.FILES2 makes the recipe engine provide TWO kinds, `page` and `build`, so a
    // recipe-only box is no longer a menu of one and the router is legitimately consulted. What
    // must not change is where a PAGE brief lands. The no-router-for-a-menu-of-one property moved
    // to the test below, which uses a genuinely single-executor box.
    let f = fixture(false, std::time::Duration::from_millis(300));
    let _ = f
        .conv
        .delegate_cmd("solo: build a one page portfolio site")
        .await;
    let rows = board(&f.mem).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["kind"], "page",
        "a page brief must still be a page: E.FILES2 adds a kind, it does not steal one"
    );
    f.release();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn with_one_executor_the_router_is_not_consulted_at_all() {
    // A menu of one needs no model, and a routing call for a decision with one possible answer is
    // pure latency on every delegation. The fixture that used to be single-executor now offers two,
    // so this builds a genuinely single-kind engine: a researcher and nothing else.
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let backend = Arc::new(CountingBackend {
        calls: calls.clone(),
    }) as Arc<dyn LLMBackend>;
    let pool = InferencePool::new(backend, 2);
    let conv = ConversationEngine::new(mem.clone(), pool.clone(), "JARVIS")
        .with_researcher(Arc::new(mind_agents::SubAgent::new(
            pool,
            Arc::new(NoTools),
            "JARVIS",
            vec!["recall".into()],
            2,
        )))
        .with_route_budget(std::time::Duration::from_millis(300));
    assert_eq!(
        conv.available_kinds_for_test(),
        vec!["research"],
        "the premise: exactly one kind"
    );
    let t0 = std::time::Instant::now();
    let _ = conv.delegate_cmd("solo: find out what the neighbours pay for water").await;
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(3),
        "no router, no wait"
    );
    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "the router must not be asked to choose from a menu of one"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timed_out_routing_task_is_detached_and_still_runs_to_completion() {
    // The accounting claim, made testable. `timeout` cancels by DROPPING the inner future, and the
    // spend ledger's terminal row is written after the blocking call's await resumes — so a dropped
    // routing future would leave a request that ran, cost something and was never recorded.
    // bounded_route spawns instead, and dropping a JoinHandle merely detaches the task.
    let finished = Arc::new(AtomicUsize::new(0));
    let f = finished.clone();
    let fired = Arc::new(AtomicUsize::new(0));
    let fi = fired.clone();

    let slow = async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        f.fetch_add(1, Ordering::Relaxed); // stands in for record_call after the await resumes
        "code"
    };
    let t0 = std::time::Instant::now();
    let kind = bounded_route(
        slow,
        "research",
        std::time::Duration::from_millis(50),
        move || {
            fi.fetch_add(1, Ordering::Relaxed);
        },
    )
    .await;
    let elapsed = t0.elapsed();

    assert_eq!(kind, "research", "the floor stands when routing times out");
    assert!(
        elapsed < std::time::Duration::from_millis(300),
        "the caller waited {elapsed:?} for a 50 ms budget: not bounded"
    );
    assert_eq!(
        fired.load(Ordering::Relaxed),
        1,
        "the operator is told once, not never and not repeatedly"
    );
    assert_eq!(
        finished.load(Ordering::Relaxed),
        0,
        "not finished at the moment of the timeout"
    );

    // The abandoned task keeps running and completes on its own.
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert_eq!(
        finished.load(Ordering::Relaxed),
        1,
        "a timed-out routing task must be DETACHED, not cancelled, or its spend row is never written"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_routing_answer_within_budget_is_used_and_nothing_is_reported() {
    let fired = Arc::new(AtomicUsize::new(0));
    let f = fired.clone();
    let quick = async { "code" };
    let kind = bounded_route(
        quick,
        "research",
        std::time::Duration::from_secs(30),
        move || {
            f.fetch_add(1, Ordering::Relaxed);
        },
    )
    .await;
    assert_eq!(kind, "code", "a routing answer refines the floor");
    assert_eq!(
        fired.load(Ordering::Relaxed),
        0,
        "nothing to report when the router answered in time"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_executor_box_runs_the_job_even_when_the_classifier_names_another_kind() {
    // THE REGRESSION, from a graded run. The benchmark brief classifies as `code`; the harness
    // configures only the page executor. Before this test, moving the executor check ahead of
    // routing made the mind refuse — with a 200, no job, no model call and no error — where it had
    // previously done the work. `route()` has always collapsed a one-option box to that option, and
    // the floor has to do the same.
    let brief = std::fs::read_to_string("../mind-evals/fixtures/cb2n/briefs/T1.txt")
        .or_else(|_| std::fs::read_to_string("crates/mind-evals/fixtures/cb2n/briefs/T1.txt"))
        .expect("the T1 brief");
    let brief = brief.trim();
    assert_eq!(
        crate::delegate::classify(brief),
        "code",
        "the premise of this test: the brief classifies to a kind the fixture cannot run"
    );

    // NO CODER HERE. It must neither refuse nor quietly become a page: it runs as `build`, which
    // produces a set of files through the mind's own inference path. Rebadging it as `page` is what
    // this box used to do, and a graded reading measured the result — a Python CLI brief answered
    // with one HTML document, scored zero, announced as live.
    let f = fixture(false, std::time::Duration::from_millis(300));
    let reply = f.conv.delegate_cmd(&format!("cb2-t1: {brief}")).await;
    assert!(
        !reply.contains("no code executor"),
        "a box that can build must not refuse code work: {reply}"
    );
    let rows = board(&f.mem).await;
    assert_eq!(rows.len(), 1, "the job must exist: {rows:?}");
    assert_eq!(
        rows[0]["kind"], "build",
        "code work with no coder routes to the executor that can still do it, not to a page"
    );
    // The router IS consulted now, and correctly: this box offers `page` and `build`, so there IS
    // something to choose between. It never answers (the fixture's backend hangs), the budget
    // expires, and the floor stands — which is the property that matters. The old assertion of ZERO
    // calls was pinning a premise that E.FILES2 changed, not a behaviour worth keeping.
    assert_eq!(
        f.calls.load(Ordering::Relaxed),
        1,
        "one bounded routing call, and the floor survives it not answering"
    );
    f.release();

    // THE DEFAULT BOX — page, research and now build, because the coder needs a key and build does
    // not. This is the configuration the graded harness runs in, and the assertion below is the
    // measured point of E.FILES2.
    let f2 = fixture(true, std::time::Duration::from_millis(200));
    let reply2 = f2.conv.delegate_cmd(&format!("cb2-t1: {brief}")).await;
    assert!(
        !reply2.contains("configured"),
        "a box holding two executors that could do the work must not refuse it: {reply2}"
    );
    let rows2 = board(&f2.mem).await;
    assert_eq!(
        rows2.len(),
        1,
        "the job must exist on a two-executor box: {rows2:?}"
    );
    // THIS EXPECTATION CHANGED, and the old one is what scored 2/11. The T1 brief classifies as
    // `code` — it asks for a run script, a server and an appending JSON store, none of which one
    // HTML document can be. It used to land as `page` because no coder existed and the floor took
    // the first executor it found. It lands as `build` now: the same deliverable the brief asked
    // for, by the route that can actually produce it.
    //
    // Not `!= "page"`: that would pass for `research` too, and a build brief handed to the
    // researcher is the same silent wrong-executor failure with a different name.
    assert_eq!(
        rows2[0]["kind"], "build",
        "a brief asking for a run script and a data file is build work: {rows2:?}"
    );
    f2.release();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_routing_future_that_never_completes_still_returns_the_floor_on_the_budget() {
    // The ledger row claimed a test used "a future that never completes" and asserted the floor came
    // back "within the budget". Neither was true of the test that existed; review caught the claim.
    // This is that test.
    let fired = Arc::new(AtomicUsize::new(0));
    let f = fired.clone();
    let never = async {
        std::future::pending::<()>().await;
        "code"
    };
    let t0 = std::time::Instant::now();
    let kind = bounded_route(
        never,
        "research",
        std::time::Duration::from_millis(80),
        move || {
            f.fetch_add(1, Ordering::Relaxed);
        },
    )
    .await;
    let elapsed = t0.elapsed();
    assert_eq!(kind, "research");
    assert!(
        elapsed >= std::time::Duration::from_millis(80)
            && elapsed < std::time::Duration::from_secs(2),
        "returned after {elapsed:?}, which is not the budget"
    );
    assert_eq!(fired.load(Ordering::Relaxed), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_abandoned_routing_call_is_counted_where_a_test_can_see_it() {
    // The notice is an eprintln with no capturable seam, so the row's claim that a test asserted it
    // was false. The count is the assertable half, and it is also the honest basis for surfacing the
    // number to an operator later.
    // THIS engine's counter, not the process-global one. The global is what an operator wants and
    // exactly what a test cannot assert: sibling tests in this file abandon their own routing calls
    // concurrently, so an exact delta was flaky (2 in 10 full-suite runs) and a `>=` was satisfied
    // by someone else's timeout — a check that could pass while this call counted nothing. Both
    // versions were review findings; this one is about this call.
    let f = fixture(true, std::time::Duration::from_millis(200));
    let before = f.conv.route_timeouts();
    let global_before = crate::delegate::route_timeouts();
    let reply = f.conv.delegate_cmd("pagejob: build a portfolio page").await;
    assert!(!reply.contains("configured"), "{reply}");
    assert_eq!(
        f.conv.route_timeouts(),
        before + 1,
        "this engine abandoned exactly one routing call and must count exactly one"
    );
    assert!(
        crate::delegate::route_timeouts() > global_before,
        "the process-wide counter moves too, since that is what an operator reads"
    );
    f.release();
}

/// A provider that fails immediately, so the call completes and writes its terminal spend row
/// without anything having to release it.
struct FailsFast;
impl LLMBackend for FailsFast {
    fn chat(
        &self,
        _m: &[ChatMessage],
        _c: &GenerationConfig,
        _t: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        anyhow::bail!("provider refused")
    }
    fn chat_streaming(
        &self,
        m: &[ChatMessage],
        c: &GenerationConfig,
        t: Option<&[serde_json::Value]>,
        _on: &mut dyn FnMut(&str),
    ) -> anyhow::Result<LLMResponse> {
        self.chat(m, c, t)
    }
    fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
        Ok(text.len() / 4)
    }
    fn backend_name(&self) -> &str {
        "fails-fast"
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delegations_model_calls_carry_the_opportunity_that_started_it() {
    // The fix review found shipped with NO test. Task-locals do not cross `tokio::spawn`, so both
    // the routing call and the job would have written spend rows with no opportunity, and a loop
    // that started a delegation would be charged for nothing it did. The observable is the ROW: the
    // pool reads the opportunity in its async frame and puts it on every terminal row it writes.
    // Reverting the routing carry or the page-kind carry makes this fail. The other four spawns
    // are covered structurally by the guard below, because a behavioural test for each would need a
    // banked skill, a coder and a resume fixture to prove one line apiece.
    // A REAL opportunity identity: the spend reader parses this field, and a made-up string would
    // make every row malformed and the test green for the wrong reason.
    const OPP: &str = "dmn:bucket:7";
    assert!(
        mind_observability::LoopOpportunity::parse(OPP).is_some(),
        "the fixture's opportunity must be one the reader accepts"
    );
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(Arc::new(FailsFast) as Arc<dyn LLMBackend>, 2);
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let recipes =
        RecipeEngine::new(pool.clone(), Arc::new(NoTools), "JARVIS").with_store(store.clone());
    let path = std::env::temp_dir().join(format!(
        "ym-eport1b-{}-{}.jsonl",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    let log = Arc::new(mind_observability::DecisionLog::open(path));
    let conv = ConversationEngine::new(mem, pool.clone(), "JARVIS")
        .with_recipes(Arc::new(recipes))
        .with_recorder(log.clone())
        .with_researcher(Arc::new(mind_agents::SubAgent::new(
            pool,
            Arc::new(NoTools),
            "JARVIS",
            vec!["recall".into()],
            2,
        )));

    mind_inference::within_opportunity(OPP.to_string(), async {
        let reply = conv.delegate_cmd("pagejob: build a portfolio page").await;
        assert!(!reply.contains("configured"), "{reply}");
    })
    .await;
    // let the detached routing task and the spawned job reach their terminal rows
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    let events = log.read_all();
    let rows: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "inference_call")
        .filter_map(mind_observability::parse_inference_call)
        .collect();
    assert!(
        !rows.is_empty(),
        "the delegation must have produced at least one spend row that the reader accepts;          the log held {} inference_call events",
        events.iter().filter(|e| e.kind == "inference_call").count()
    );
    for r in &rows {
        assert_eq!(
            r.opportunity,
            mind_observability::LoopOpportunity::parse(OPP),
            "a spend row from a delegation started inside a loop must carry that loop: {r:?}"
        );
    }
}

#[test]
fn every_spawn_in_the_delegation_path_carries_its_opportunity() {
    // Review found the FIRST version of this guard could not fail for the case it named: it matched
    // lines STARTING with `tokio::spawn(`, and the one spawn its own comment excepted is written
    // `let handle = tokio::spawn(...)`. So the exception was dead code, the anti-vacuity count was
    // computed the same blind way and reported five while a sixth sat unexamined, and the comment
    // claimed a check the code did not perform. Stripping the carry from THAT spawn left it green.
    //
    // Now: every occurrence anywhere in a line, an exact expected count, and the exception named by
    // line content rather than assumed.
    //
    // Its limits, stated: it is a source scan, so it cannot see a spawn that carries an opportunity
    // captured from the wrong scope, and it does not reach spawns in other files reachable from
    // delegate_cmd (code.rs, research.rs) — those are covered by the behavioural test above only for
    // the page path.
    const SRC: &str = include_str!("delegate.rs");
    let mut carried = Vec::new();
    let mut bare = Vec::new();
    for (n, line) in SRC.lines().enumerate() {
        if !line.contains("tokio::spawn(") && !line.contains("tokio::task::spawn(") {
            continue;
        }
        let lineno = n + 1;
        // bounded_route's spawn re-enters the scope INSIDE the future rather than wrapping it, so
        // it is identified by that body, not by position.
        let inline_carry = SRC
            .lines()
            .skip(n)
            .take(6)
            .any(|l| l.contains("within_opportunity(id, routing)"));
        if line.contains("in_opportunity(") || inline_carry {
            carried.push(lineno);
        } else {
            bare.push(lineno);
        }
    }
    assert!(
        bare.is_empty(),
        "these spawns in delegate.rs lose the opportunity their delegation was created under, so a loop that started the work is charged for none of it — lines {bare:?}"
    );
    // Anti-vacuity, counted the same way the guard counts: an exact number, so a spawn that becomes
    // invisible to the scan fails here instead of passing quietly.
    assert_eq!(
        carried.len(),
        7,
        "expected six carrying spawns in delegate.rs (five wrapped plus bounded_route's inline carry, plus E.FILES2's build arm); found {carried:?} — if a spawn was added or removed, decide about it here"
    );
}
