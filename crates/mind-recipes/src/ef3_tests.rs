//! E.F3 — an expired commitment is a receipt and a notice, never a silent corpse. Fixtures for
//! the preregistered witness and kills: the strict boundary, one receipt per goal, the sweep's
//! scope (pending / failed / paused / no row; never running), terminal after expiry, the
//! legacy goal without a chain, and the listing order.
use super::*;
use crate::tests::ScriptedHost;
use mind_inference::ScriptedLLM;
use mind_spec::{HorizonControlAction, HorizonLifecycleEvent};
use yantrik_ml::LLMBackend;

fn engine_with(store: Arc<RecipeStore>) -> RecipeEngine {
    let pool = InferencePool::new(
        Arc::new(ScriptedLLM::new("unused")) as Arc<dyn LLMBackend>,
        1,
    );
    RecipeEngine::new(pool, Arc::new(ScriptedHost), "JARVIS").with_store(store)
}

/// A goal with a one-minute elapsed budget whose one segment wakes at +30 s, inside the budget
/// (the scheduler refuses a wake beyond it). Expiry arises when the sweep is MISSED past +60 s —
/// a box that was down, or a paused row — which is the feasible witness shape.
fn short_lived(engine: &RecipeEngine, goal_id: &str, start: u64) {
    let mut run = HorizonRun::start(
        goal_id,
        "Check the inbox once, later",
        vec!["Run one bounded observation segment".into()],
        BTreeMap::new(),
        mind_spec::HorizonBudget {
            max_actions: 1,
            max_replans: 0,
            max_cost_units: 2,
            max_elapsed_ms: 60_000,
        },
        start,
    )
    .unwrap();
    let job = HorizonJob {
        goal_id: goal_id.into(),
        kind: HorizonJobKind::Segment,
        segment_id: "segment:1".into(),
        recipe: Recipe {
            id: "horizon-recipe:short".into(),
            name: "observe".into(),
            steps: vec![RecipeStep::Tool {
                tool_name: "inbox".into(),
                args: serde_json::json!({"limit": 1}),
                store_as: "fresh".into(),
                on_error: ErrorAction::Fail,
            }],
        },
        assumption_vars: BTreeMap::new(),
        wake_at_ms: start + 30_000,
        cost_units: 1,
        complete_on_success: true,
    };
    engine
        .schedule_horizon_segment(&mut run, job, start)
        .unwrap();
}

fn events(store: &RecipeStore, goal_id: &str) -> Vec<HorizonLifecycleEvent> {
    store
        .load_horizon_lifecycle(goal_id)
        .expect("the chain verifies")
        .iter()
        .map(|r| r.event)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_goal_whose_budget_elapses_before_it_wakes_expires_once_with_a_receipt_and_stays_terminal(
) {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(store.clone());
    let start = 1_788_300_000_000u64;
    short_lived(&engine, "goal:short", start);
    // Not yet due, not yet expired: nothing.
    assert!(engine.resume_due_horizons(start + 10_000).await.is_empty());
    // One millisecond past the boundary, before any claim: expired, once, atomically. (At exactly
    // +60 000 ms `check_time` still PERMITS the run — equality is inside the budget — and the
    // strict-boundary fixture below pins that no receipt is issued there.)
    let tick = engine.resume_due_horizons(start + 60_001).await;
    assert_eq!(tick.len(), 1, "{tick:?}");
    assert_eq!(tick[0].state, HorizonTickState::Expired);
    assert_eq!(tick[0].goal_id, "goal:short");
    assert_eq!(tick[0].error.as_deref(), Some("budget_elapsed"));
    assert_eq!(
        events(&store, "goal:short"),
        vec![
            HorizonLifecycleEvent::Scheduled,
            HorizonLifecycleEvent::Expired
        ]
    );
    let receipts = store.load_horizon_lifecycle("goal:short").unwrap();
    let expired = &receipts[1];
    assert_eq!(expired.previous_queue_status.as_deref(), Some("pending"));
    assert_eq!(expired.next_queue_status, None);
    assert_eq!(expired.failure_reason.as_deref(), Some("budget_elapsed"));
    assert!(expired.state_sha256.is_some());
    // The queue row is gone; the checkpoint remains as history; the listing marks it.
    assert!(store.queued_horizon_job("goal:short").unwrap().is_none());
    let views = engine.list_horizons(start + 60_002).unwrap();
    assert_eq!(views.len(), 1);
    assert!(views[0].expired);
    // Terminal: no second receipt on later ticks, no control, no new job, no checkpoint write.
    assert!(engine.resume_due_horizons(start + 120_000).await.is_empty());
    assert_eq!(store.load_horizon_lifecycle("goal:short").unwrap().len(), 2);
    for action in [
        HorizonControlAction::Retry,
        HorizonControlAction::Pause,
        HorizonControlAction::Resume,
        HorizonControlAction::Cancel,
    ] {
        assert!(
            store
                .control_horizon("goal:short", action, start + 120_001)
                .is_err(),
            "{action:?} on an expired goal"
        );
    }
    let mut run = store.load_horizon("goal:short", start).unwrap().unwrap();
    let job = HorizonJob {
        goal_id: "goal:short".into(),
        kind: HorizonJobKind::Segment,
        segment_id: "segment:2".into(),
        recipe: Recipe {
            id: "horizon-recipe:short2".into(),
            name: "observe".into(),
            steps: vec![RecipeStep::Tool {
                tool_name: "inbox".into(),
                args: serde_json::json!({"limit": 1}),
                store_as: "fresh".into(),
                on_error: ErrorAction::Fail,
            }],
        },
        assumption_vars: BTreeMap::new(),
        wake_at_ms: start + 5_000,
        cost_units: 1,
        complete_on_success: true,
    };
    assert!(engine
        .schedule_horizon_segment(&mut run, job, start + 1_000)
        .is_err());
    assert!(
        store.load_horizon("goal:short", start).unwrap().is_some(),
        "history kept"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_boundary_is_strict_and_a_running_row_is_never_swept() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(store.clone());
    let start = 1_788_300_000_000u64;
    short_lived(&engine, "goal:edge", start);
    // At exactly max_elapsed: inside the budget (check_time permits equality), so not expired.
    // The segment is due (wake at +30 s) and is claimed; whatever it does, no `expired` receipt
    // may exist at this instant.
    let _ = engine.resume_due_horizons(start + 60_000).await;
    assert!(!events(&store, "goal:edge").contains(&HorizonLifecycleEvent::Expired));

    // A running row is never swept, however elapsed the budget.
    short_lived(&engine, "goal:running", start);
    store.force_job_running_for_test("goal:running").unwrap();
    let tick = engine.resume_due_horizons(start + 600_000).await;
    assert!(
        !tick
            .iter()
            .any(|o| o.goal_id == "goal:running" && o.state == HorizonTickState::Expired),
        "{tick:?}"
    );
    assert!(!events(&store, "goal:running").contains(&HorizonLifecycleEvent::Expired));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_legacy_goal_with_no_chain_and_no_row_expires_with_its_first_and_last_receipt() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(store.clone());
    let start = 1_788_300_000_000u64;
    // The canary's shape: a checkpoint saved with no job row and no lifecycle receipts.
    let mut run = HorizonRun::start(
        "goal:legacy",
        "verify staging stayed healthy overnight",
        vec!["Run one bounded observation segment".into()],
        BTreeMap::new(),
        mind_spec::HorizonBudget {
            max_actions: 1,
            max_replans: 0,
            max_cost_units: 3,
            max_elapsed_ms: 60_000,
        },
        start,
    )
    .unwrap();
    let checkpoint = run.checkpoint(start).unwrap();
    store.save_horizon_checkpoint(&checkpoint).unwrap();
    assert!(store
        .load_horizon_lifecycle("goal:legacy")
        .unwrap()
        .is_empty());
    let tick = engine.resume_due_horizons(start + 60_001).await;
    assert_eq!(tick.len(), 1);
    assert_eq!(tick[0].state, HorizonTickState::Expired);
    let receipts = store.load_horizon_lifecycle("goal:legacy").unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].event, HorizonLifecycleEvent::Expired);
    assert_eq!(receipts[0].previous_queue_status, None);
    assert_eq!(receipts[0].previous_receipt_sha256, None);
    assert!(receipts[0].verify());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_goals_are_listed_first_and_a_live_goal_is_untouched() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(store.clone());
    let start = 1_788_300_000_000u64;
    short_lived(&engine, "goal:a-short", start);
    // A long-lived goal scheduled later, with a generous budget.
    let mut run = HorizonRun::start(
        "goal:b-long",
        "Check the inbox once, much later",
        vec!["Run one bounded observation segment".into()],
        BTreeMap::new(),
        mind_spec::HorizonBudget {
            max_actions: 1,
            max_replans: 0,
            max_cost_units: 2,
            max_elapsed_ms: 86_400_000,
        },
        start,
    )
    .unwrap();
    let job = HorizonJob {
        goal_id: "goal:b-long".into(),
        kind: HorizonJobKind::Segment,
        segment_id: "segment:1".into(),
        recipe: Recipe {
            id: "horizon-recipe:long".into(),
            name: "observe".into(),
            steps: vec![RecipeStep::Tool {
                tool_name: "inbox".into(),
                args: serde_json::json!({"limit": 1}),
                store_as: "fresh".into(),
                on_error: ErrorAction::Fail,
            }],
        },
        assumption_vars: BTreeMap::new(),
        wake_at_ms: start + 3_600_000,
        cost_units: 1,
        complete_on_success: true,
    };
    engine
        .schedule_horizon_segment(&mut run, job, start)
        .unwrap();
    let before = store.load_horizon_lifecycle("goal:b-long").unwrap();
    let tick = engine.resume_due_horizons(start + 60_001).await;
    assert_eq!(tick.len(), 1);
    assert_eq!(tick[0].goal_id, "goal:a-short");
    // The live goal's receipts are byte-identical to before the sweep.
    assert_eq!(store.load_horizon_lifecycle("goal:b-long").unwrap(), before);
    let views = engine.list_horizons(start + 60_002).unwrap();
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].goal_id, "goal:a-short");
    assert!(views[0].expired);
    assert!(!views[1].expired);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_row_with_a_retryable_code_expires_from_failed_to_none_and_the_row_is_gone() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(store.clone());
    let start = 1_788_300_000_000u64;
    short_lived(&engine, "goal:failed", start);
    store
        .force_job_failed_for_test("goal:failed", HorizonFailureReason::ReplanPlanner.as_str())
        .unwrap();
    let tick = engine.resume_due_horizons(start + 60_001).await;
    assert_eq!(tick.len(), 1, "{tick:?}");
    assert_eq!(tick[0].state, HorizonTickState::Expired);
    let receipts = store.load_horizon_lifecycle("goal:failed").unwrap();
    let last = receipts.last().unwrap();
    assert_eq!(last.event, HorizonLifecycleEvent::Expired);
    assert_eq!(last.previous_queue_status.as_deref(), Some("failed"));
    assert_eq!(last.next_queue_status, None);
    assert!(store.queued_horizon_job("goal:failed").unwrap().is_none());
    // Idempotent: the next tick appends nothing and deletes nothing.
    assert!(engine.resume_due_horizons(start + 60_002).await.is_empty());
    assert_eq!(
        store.load_horizon_lifecycle("goal:failed").unwrap(),
        receipts
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paused_row_expires_from_paused_to_none_and_no_control_revives_it() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(store.clone());
    let start = 1_788_300_000_000u64;
    short_lived(&engine, "goal:paused", start);
    store
        .control_horizon("goal:paused", HorizonControlAction::Pause, start + 1_000)
        .unwrap();
    // Paused past its budget: the sweep still owns the row and expires it.
    let tick = engine.resume_due_horizons(start + 60_001).await;
    assert_eq!(tick.len(), 1, "{tick:?}");
    assert_eq!(tick[0].state, HorizonTickState::Expired);
    let last = store
        .load_horizon_lifecycle("goal:paused")
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(last.previous_queue_status.as_deref(), Some("paused"));
    assert_eq!(last.next_queue_status, None);
    assert!(store.queued_horizon_job("goal:paused").unwrap().is_none());
    assert!(store
        .control_horizon("goal:paused", HorizonControlAction::Resume, start + 60_002)
        .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_already_terminal_failure_gets_no_second_terminal_receipt() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(store.clone());
    let start = 1_788_300_000_000u64;
    // A failed row carrying a terminal E.F2 code: the goal is already terminal.
    short_lived(&engine, "goal:terminal", start);
    store
        .force_job_failed_for_test(
            "goal:terminal",
            HorizonFailureReason::ReplanBudgetExhausted.as_str(),
        )
        .unwrap();
    let before = store.load_horizon_lifecycle("goal:terminal").unwrap();
    let tick = engine.resume_due_horizons(start + 600_000).await;
    assert!(
        !tick
            .iter()
            .any(|o| o.goal_id == "goal:terminal" && o.state == HorizonTickState::Expired),
        "{tick:?}"
    );
    assert_eq!(
        store.load_horizon_lifecycle("goal:terminal").unwrap(),
        before
    );
    assert!(
        store.queued_horizon_job("goal:terminal").unwrap().is_some(),
        "row untouched"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_checkpoint_whose_digest_does_not_match_is_never_expired() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(store.clone());
    let start = 1_788_300_000_000u64;
    short_lived(&engine, "goal:corrupt", start);
    store
        .corrupt_checkpoint_digest_for_test("goal:corrupt")
        .unwrap();
    // Fail closed: the sweep refuses the tick as a whole rather than sign a receipt over a
    // checkpoint that is not provably the goal's own; nothing is appended or deleted.
    let tick = engine.resume_due_horizons(start + 60_001).await;
    assert_eq!(tick.len(), 1, "{tick:?}");
    assert_eq!(tick[0].goal_id, "scheduler");
    assert_eq!(tick[0].state, HorizonTickState::Failed);
    let raw = store.load_horizon_lifecycle("goal:corrupt").unwrap();
    assert!(!raw
        .iter()
        .any(|r| r.event == HorizonLifecycleEvent::Expired));
    assert!(store.queued_horizon_job("goal:corrupt").unwrap().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_follows_an_expiry_receipt_and_a_tampered_chain_fails_every_consumer() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(store.clone());
    let start = 1_788_300_000_000u64;
    short_lived(&engine, "goal:sealed", start);
    assert_eq!(engine.resume_due_horizons(start + 60_001).await.len(), 1);
    // The store's own appender refuses anything after the expiry receipt.
    assert!(store
        .append_started_for_test("goal:sealed", start + 70_000, "0123456789abcdef", 1, 1)
        .is_err());
    assert_eq!(
        store.load_horizon_lifecycle("goal:sealed").unwrap().len(),
        2
    );
    // A raw row smuggled in after the expiry receipt makes the chain fail verification, and the
    // verified readers — listing, control, scheduling — fail closed with it.
    store
        .duplicate_first_receipt_row_for_test("goal:sealed")
        .unwrap();
    assert!(store.load_horizon_lifecycle("goal:sealed").is_err());
    assert!(engine.list_horizons(start + 60_002).is_err());
    assert!(store
        .control_horizon("goal:sealed", HorizonControlAction::Cancel, start + 60_003)
        .is_err());
    let mut run = store.load_horizon("goal:sealed", start).unwrap().unwrap();
    let job = HorizonJob {
        goal_id: "goal:sealed".into(),
        kind: HorizonJobKind::Segment,
        segment_id: "segment:2".into(),
        recipe: Recipe {
            id: "horizon-recipe:sealed".into(),
            name: "observe".into(),
            steps: vec![RecipeStep::Tool {
                tool_name: "inbox".into(),
                args: serde_json::json!({"limit": 1}),
                store_as: "fresh".into(),
                on_error: ErrorAction::Fail,
            }],
        },
        assumption_vars: BTreeMap::new(),
        wake_at_ms: start + 5_000,
        cost_units: 1,
        complete_on_success: true,
    };
    assert!(engine
        .schedule_horizon_segment(&mut run, job, start + 1_000)
        .is_err());
}
