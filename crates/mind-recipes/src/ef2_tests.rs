//! E.F2 — the goal replans itself within budget. Fixtures for the preregistered witness and
//! kills: the full drift → park → replan → complete chain; budget exhaustion as a terminal,
//! planner-free failure; a rejected revision retried through a bound control; a malformed
//! lifecycle as a terminal integrity failure written exactly once; and the door with and
//! without a declared assumption.
use super::*;
use crate::tests::ScriptedHost;
use mind_inference::{ScriptedLLM, SequencedLLM};
use mind_spec::{HorizonControlAction, HorizonLifecycleEvent, HorizonStatus};
use yantrik_ml::LLMBackend;

fn engine_with(llm: Arc<dyn LLMBackend>, store: Arc<RecipeStore>) -> RecipeEngine {
    let pool = InferencePool::new(llm, 1);
    RecipeEngine::new(pool, Arc::new(ScriptedHost), "JARVIS").with_store(store)
}

const READ_ONLY_PLAN: &str = r#"[
    {"Tool":{"tool_name":"inbox","args":{"limit":2},"store_as":"fresh"}},
    {"Think":{"prompt":"Summarize {{fresh}}","store_as":"answer"}}
]"#;

fn events(store: &RecipeStore, goal_id: &str) -> Vec<HorizonLifecycleEvent> {
    store
        .load_horizon_lifecycle(goal_id)
        .expect("the chain verifies")
        .iter()
        .map(|r| r.event)
        .collect()
}

/// A goal that declares `inbox_state = "no messages"` and observes the inbox on its first read.
fn declared_goal(
    engine: &RecipeEngine,
    goal_id: &str,
    max_replans: u32,
    start: u64,
) -> anyhow::Result<()> {
    let mut run = HorizonRun::start(
        goal_id,
        "Watch the inbox and summarize what needs attention",
        vec!["Run one bounded observation segment".into()],
        BTreeMap::from([("inbox_state".to_string(), "no messages".to_string())]),
        mind_spec::HorizonBudget {
            max_actions: 3,
            max_replans,
            max_cost_units: 20,
            max_elapsed_ms: 86_400_000,
        },
        start,
    )
    .unwrap();
    let var = reserved_observation_var("inbox_state");
    let job = HorizonJob {
        goal_id: goal_id.into(),
        kind: HorizonJobKind::Segment,
        segment_id: "segment:1".into(),
        recipe: Recipe {
            id: "horizon-recipe:t".into(),
            name: "observe the inbox".into(),
            steps: vec![RecipeStep::Tool {
                tool_name: "inbox".into(),
                args: serde_json::json!({"limit": 2}),
                store_as: var.clone(),
                on_error: ErrorAction::Fail,
            }],
        },
        assumption_vars: BTreeMap::from([("inbox_state".to_string(), var)]),
        wake_at_ms: start,
        cost_units: 1,
        complete_on_success: true,
    };
    engine.schedule_horizon_segment(&mut run, job, start)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drifted_goal_replans_within_budget_and_completes_with_a_traceable_chain() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(Arc::new(ScriptedLLM::new(READ_ONLY_PLAN)), store.clone());
    let start = now_ms();
    declared_goal(&engine, "goal:replan", 1, start).unwrap();

    // Segment 1 observes "INBOX: 2 messages…" against "no messages": drift, park, carrier.
    let parked = engine.resume_due_horizons(start).await;
    assert_eq!(parked.len(), 1);
    assert_eq!(
        parked[0].state,
        HorizonTickState::AwaitingReplan,
        "{:?}",
        parked[0].error
    );
    let (carrier, status, _) = store.queued_horizon_job("goal:replan").unwrap().unwrap();
    assert_eq!(status, "pending");
    assert_eq!(
        carrier.kind,
        HorizonJobKind::Replan {
            assumption_id: mind_spec::assumption_id("inbox_state"),
            target_revision: 1,
        }
    );
    assert!(carrier.recipe.steps.is_empty());
    assert_eq!(
        events(&store, "goal:replan"),
        vec![
            HorizonLifecycleEvent::Scheduled,
            HorizonLifecycleEvent::WakeStarted,
            HorizonLifecycleEvent::AwaitingReplan,
        ]
    );

    // The carrier is claimed; the branch authors a revision, applies it, schedules segment 2.
    let replanned = engine.resume_due_horizons(start + 1).await;
    assert_eq!(replanned.len(), 1);
    assert_eq!(
        replanned[0].state,
        HorizonTickState::Replanned,
        "{:?}",
        replanned[0].error
    );
    let run = store
        .load_horizon("goal:replan", start + 2)
        .unwrap()
        .unwrap();
    assert_eq!(run.status, HorizonStatus::Active);
    assert_eq!(run.plan_revision, 1);
    assert!(run
        .assumption_changes
        .iter()
        .all(|c| c.addressed_by_revision == Some(1)));
    let (next, status, _) = store.queued_horizon_job("goal:replan").unwrap().unwrap();
    assert_eq!(status, "pending");
    assert_eq!(next.kind, HorizonJobKind::Segment);
    assert_eq!(next.segment_id, "segment:2");
    let var = reserved_observation_var("inbox_state");
    assert_eq!(next.assumption_vars.get("inbox_state"), Some(&var));
    match &next.recipe.steps[0] {
        RecipeStep::Tool { store_as, .. } => assert_eq!(store_as, &var),
        other => panic!("first step must be the read: {other:?}"),
    }
    match &next.recipe.steps[1] {
        RecipeStep::Think { prompt, .. } => {
            assert!(prompt.contains(&format!("{{{{{var}}}}}")));
            assert!(!prompt.contains("{{fresh}}"));
        }
        other => panic!("second step must be the think: {other:?}"),
    }
    assert_eq!(
        events(&store, "goal:replan"),
        vec![
            HorizonLifecycleEvent::Scheduled,
            HorizonLifecycleEvent::WakeStarted,
            HorizonLifecycleEvent::AwaitingReplan,
            HorizonLifecycleEvent::WakeStarted,
            HorizonLifecycleEvent::ReplanStarted,
            HorizonLifecycleEvent::Replanned,
            HorizonLifecycleEvent::Scheduled,
        ]
    );
    let receipts = store.load_horizon_lifecycle("goal:replan").unwrap();
    // Receipts carry the opaque id and the ordinal, never the key or a value.
    for r in &receipts {
        let json = serde_json::to_string(r).unwrap();
        assert!(!json.contains("inbox_state") && !json.contains("no messages"));
    }
    let started = &receipts[4];
    assert_eq!(started.replan.as_ref().and_then(|d| d.attempt), Some(1));
    assert_eq!(
        started
            .replan
            .as_ref()
            .and_then(|d| d.assumption_id.clone()),
        Some(mind_spec::assumption_id("inbox_state"))
    );

    // Segment 2 observes the same (new) value: no drift, the goal completes; the outcome
    // receipt counts one replan.
    let done = engine.resume_due_horizons(start + 2).await;
    assert_eq!(done.len(), 1);
    assert_eq!(
        done[0].state,
        HorizonTickState::Completed,
        "{:?}",
        done[0].error
    );
    let receipt = done[0].receipt.as_ref().expect("outcome receipt");
    assert!(receipt.verify());
    assert_eq!(receipt.replans, 1);
    assert_eq!(
        events(&store, "goal:replan").last(),
        Some(&HorizonLifecycleEvent::Completed)
    );
    assert!(engine.resume_due_horizons(start + 3).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_exhausted_replan_budget_is_terminal_and_spends_no_planner_call() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let seq = Arc::new(SequencedLLM::new(vec![READ_ONLY_PLAN]));
    let engine = engine_with(seq.clone() as Arc<dyn LLMBackend>, store.clone());
    let start = now_ms();
    declared_goal(&engine, "goal:nobudget", 0, start).unwrap();
    assert_eq!(
        engine.resume_due_horizons(start).await[0].state,
        HorizonTickState::AwaitingReplan
    );
    let outcome = engine.resume_due_horizons(start + 1).await;
    assert_eq!(outcome.len(), 1);
    assert_eq!(outcome[0].state, HorizonTickState::Failed);
    assert!(outcome[0]
        .error
        .as_deref()
        .is_some_and(|e| e.contains("budget")));
    assert_eq!(seq.call_count(), 0, "an exhausted budget spends nothing");
    let (_, status, code) = store.queued_horizon_job("goal:nobudget").unwrap().unwrap();
    assert_eq!(status, "failed");
    assert_eq!(code.as_deref(), Some("replan_budget_exhausted"));
    let receipts = store.load_horizon_lifecycle("goal:nobudget").unwrap();
    let tail: Vec<_> = receipts.iter().rev().take(2).map(|r| r.event).collect();
    assert_eq!(
        tail,
        vec![
            HorizonLifecycleEvent::Failed,
            HorizonLifecycleEvent::ReplanStarted
        ]
    );
    assert_eq!(
        receipts
            .last()
            .unwrap()
            .replan
            .as_ref()
            .and_then(|d| d.attempt),
        Some(1)
    );
    // Terminal: the retry control refuses, and nothing is claimable.
    let refused = store.control_horizon("goal:nobudget", HorizonControlAction::Retry, start + 2);
    assert!(refused
        .err()
        .is_some_and(|e| e.to_string().contains("terminal")));
    assert!(engine.resume_due_horizons(start + 3).await.is_empty());
    assert_eq!(seq.call_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_revision_is_retryable_only_through_a_bound_retry_control() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    // The planner proposes an outward step: the revision must fail validation, retryably.
    let outward = r#"[
        {"Tool":{"tool_name":"inbox","args":{"limit":2},"store_as":"fresh"}},
        {"Act":{"kind":"send_email","target":"boss@acme.com","summary":"reply","payload":"{{fresh}}"}}
    ]"#;
    let engine = engine_with(Arc::new(ScriptedLLM::new(outward)), store.clone());
    let start = now_ms();
    declared_goal(&engine, "goal:retry", 2, start).unwrap();
    engine.resume_due_horizons(start).await;
    let first = engine.resume_due_horizons(start + 1).await;
    assert_eq!(first[0].state, HorizonTickState::Failed);
    let (_, status, code) = store.queued_horizon_job("goal:retry").unwrap().unwrap();
    assert_eq!(
        (status.as_str(), code.as_deref()),
        ("failed", Some("replan_validation_failed"))
    );
    // The goal stays parked, its checkpoint intact.
    let run = store
        .load_horizon("goal:retry", start + 2)
        .unwrap()
        .unwrap();
    assert_eq!(run.status, HorizonStatus::AwaitingReplan);
    assert_eq!(run.plan_revision, 0);
    // Retry is accepted for this code and binds the next acquisition (Branch C, attempt 2).
    store
        .control_horizon("goal:retry", HorizonControlAction::Retry, start + 2)
        .unwrap();
    let second = engine.resume_due_horizons(start + 3).await;
    assert_eq!(second[0].state, HorizonTickState::Failed);
    let receipts = store.load_horizon_lifecycle("goal:retry").unwrap();
    let attempts: Vec<u32> = receipts
        .iter()
        .filter(|r| r.event == HorizonLifecycleEvent::ReplanStarted)
        .filter_map(|r| r.replan.as_ref().and_then(|d| d.attempt))
        .collect();
    assert_eq!(attempts, vec![1, 2]);
    let closes: Vec<u32> = receipts
        .iter()
        .filter(|r| r.event == HorizonLifecycleEvent::Failed)
        .filter_map(|r| r.replan.as_ref().and_then(|d| d.attempt))
        .collect();
    assert_eq!(closes, vec![1, 2]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_malformed_lifecycle_is_a_terminal_integrity_failure_written_exactly_once() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(Arc::new(ScriptedLLM::new(READ_ONLY_PLAN)), store.clone());
    let start = now_ms();
    declared_goal(&engine, "goal:integrity", 1, start).unwrap();
    engine.resume_due_horizons(start).await;
    // Claim the carrier as the scheduler would, then acquire it under a DIFFERENT identity
    // than the parking receipt named — the reducer must refuse, terminally, atomically.
    let claimed = store.claim_due_horizon_jobs(start + 1).unwrap();
    assert_eq!(claimed.len(), 1);
    let sha = store
        .active_checkpoint_sha256("goal:integrity")
        .unwrap()
        .unwrap();
    let wrong = mind_spec::assumption_id("weather");
    let first = store
        .acquire_replan("goal:integrity", start + 1, &sha, &wrong, 1)
        .unwrap();
    assert!(matches!(
        first,
        mind_spec::ReplanAcquisition::Blocked(mind_spec::ReplanBlock::Mismatch { .. })
    ));
    let receipts = store.load_horizon_lifecycle("goal:integrity").unwrap();
    let integrity: Vec<_> = receipts
        .iter()
        .filter(|r| r.event == HorizonLifecycleEvent::ReplanIntegrityFailed)
        .collect();
    assert_eq!(integrity.len(), 1);
    let digest = integrity[0]
        .replan
        .as_ref()
        .and_then(|d| d.chain_digest.clone())
        .unwrap();
    // The digest is the prefix ending immediately before the integrity receipt.
    assert_eq!(mind_spec::reduce_replan(&receipts).prefix_digest, digest);
    let (_, status, code) = store.queued_horizon_job("goal:integrity").unwrap().unwrap();
    assert_eq!(
        (status.as_str(), code.as_deref()),
        ("failed", Some("replan_lifecycle_mismatch"))
    );
    // Re-entry by a stale in-flight claimant: no second receipt, no state change.
    store.force_job_running_for_test("goal:integrity").unwrap();
    let again = store
        .acquire_replan("goal:integrity", start + 2, &sha, &wrong, 1)
        .unwrap();
    assert_eq!(
        again,
        mind_spec::ReplanAcquisition::Blocked(mind_spec::ReplanBlock::IntegrityAlreadyFailed)
    );
    assert_eq!(
        store
            .load_horizon_lifecycle("goal:integrity")
            .unwrap()
            .len(),
        receipts.len()
    );
    let (_, status, _) = store.queued_horizon_job("goal:integrity").unwrap().unwrap();
    assert_eq!(status, "failed");
    // Terminal: retry refused; the scheduler claims nothing.
    assert!(store
        .control_horizon("goal:integrity", HorizonControlAction::Retry, start + 3)
        .err()
        .is_some_and(|e| e.to_string().contains("terminal")));
    assert!(engine.resume_due_horizons(start + 4).await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_door_is_byte_identical_without_assuming_and_binds_the_reserved_observation_with_it() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(Arc::new(ScriptedLLM::new(READ_ONLY_PLAN)), store.clone());
    let start = now_ms();
    // Without `assuming`: exactly today's contract.
    let plain = engine
        .schedule_read_only_horizon("Check my inbox and summarize it", 60_000, start)
        .await
        .unwrap();
    let run = store.load_horizon(&plain, start).unwrap().unwrap();
    assert_eq!(run.budget.max_actions, 1);
    assert_eq!(run.budget.max_replans, 0);
    assert_eq!(run.budget.max_cost_units, 2);
    assert!(run.assumptions.is_empty());
    let (job, _, _) = store.queued_horizon_job(&plain).unwrap().unwrap();
    assert_eq!(job.kind, HorizonJobKind::Segment);
    assert!(job.assumption_vars.is_empty());
    match &job.recipe.steps[0] {
        RecipeStep::Tool { store_as, .. } => assert_eq!(store_as, "fresh"),
        other => panic!("{other:?}"),
    }

    // With `assuming`: room for one replan, the assumption declared, the first read bound to
    // the reserved observation and later references rewritten.
    let declared = engine
        .schedule_read_only_horizon_assuming(
            "Check my inbox and summarize it",
            60_000,
            start + 1,
            Some(("inbox_state".into(), "no messages".into())),
        )
        .await
        .unwrap();
    let run = store.load_horizon(&declared, start + 1).unwrap().unwrap();
    assert_eq!(run.budget.max_actions, 2);
    assert_eq!(run.budget.max_replans, 1);
    assert_eq!(run.budget.max_cost_units, 5);
    assert_eq!(
        run.assumptions.get("inbox_state").map(|a| a.value.as_str()),
        Some("no messages")
    );
    let (job, _, _) = store.queued_horizon_job(&declared).unwrap().unwrap();
    let var = reserved_observation_var("inbox_state");
    assert_eq!(job.assumption_vars.get("inbox_state"), Some(&var));
    match &job.recipe.steps[0] {
        RecipeStep::Tool { store_as, .. } => assert_eq!(store_as, &var),
        other => panic!("{other:?}"),
    }
    match &job.recipe.steps[1] {
        RecipeStep::Think { prompt, .. } => assert!(prompt.contains("{{__assume_inbox_state}}")),
        other => panic!("{other:?}"),
    }
    // A malformed key is refused at the door, before anything is persisted.
    assert!(engine
        .schedule_read_only_horizon_assuming(
            "Check my inbox and summarize it",
            60_000,
            start + 2,
            Some(("Inbox State".into(), "x".into())),
        )
        .await
        .is_err());
    assert!(store
        .load_horizon(&format!("goal:horizon:{:x}", start + 2), start + 2)
        .unwrap()
        .is_none());

    // Both goals wake on the same tick: the plain one completes untouched, the declared one
    // observes the drift and parks. Then the declared goal runs the witness: replan, complete.
    let wake = start + 1 + 60_000;
    let tick = engine.resume_due_horizons(wake).await;
    let states: BTreeMap<&str, HorizonTickState> = tick
        .iter()
        .map(|o| (o.goal_id.as_str(), o.state.clone()))
        .collect();
    assert_eq!(
        states.get(plain.as_str()),
        Some(&HorizonTickState::Completed)
    );
    assert_eq!(
        states.get(declared.as_str()),
        Some(&HorizonTickState::AwaitingReplan)
    );
    let replanned = engine.resume_due_horizons(wake + 1).await;
    assert_eq!(replanned.len(), 1);
    assert_eq!(
        replanned[0].state,
        HorizonTickState::Replanned,
        "{:?}",
        replanned[0].error
    );
    let done = engine.resume_due_horizons(wake + 2).await;
    assert_eq!(done.len(), 1);
    assert_eq!(
        done[0].state,
        HorizonTickState::Completed,
        "{:?}",
        done[0].error
    );
    assert_eq!(done[0].receipt.as_ref().unwrap().replans, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plan_whose_later_step_writes_the_reserved_observation_is_refused_at_the_door() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let colliding = r#"[
        {"Tool":{"tool_name":"inbox","args":{"limit":2},"store_as":"fresh"}},
        {"Think":{"prompt":"Summarize {{fresh}}","store_as":"__assume_inbox_state"}}
    ]"#;
    let engine = engine_with(Arc::new(ScriptedLLM::new(colliding)), store.clone());
    let start = now_ms();
    let refused = engine
        .schedule_read_only_horizon_assuming(
            "Check my inbox and summarize it",
            60_000,
            start,
            Some(("inbox_state".into(), "no messages".into())),
        )
        .await;
    assert!(refused
        .err()
        .is_some_and(|e| e.to_string().contains("assumption_observation_failed")));
    // Nothing was persisted: preflight rejects before the checkpoint or the job exist.
    assert!(store
        .load_horizon(&format!("goal:horizon:{start:x}"), start)
        .unwrap()
        .is_none());
    // Without a declared assumption the same plan is accepted unchanged: the reserved name is
    // only reserved once an assumption claims it.
    assert!(engine
        .schedule_read_only_horizon("Check my inbox and summarize it", 60_000, start + 1)
        .await
        .is_ok());
}

#[test]
fn the_reserved_binding_rewrites_in_dataflow_order_and_stops_at_a_shadowing_definition() {
    // Tool -> fresh; Think({{fresh}}) -> fresh (shadows); Render(fresh) reads the THINK.
    let mut steps = vec![
        RecipeStep::Tool {
            tool_name: "inbox".into(),
            args: serde_json::json!({"limit": 2}),
            store_as: "fresh".into(),
            on_error: ErrorAction::Fail,
        },
        RecipeStep::Think {
            prompt: "Summarize {{fresh}}".into(),
            store_as: "fresh".into(),
            max_tokens: None,
            think: None,
            on_error: ErrorAction::Fail,
        },
        RecipeStep::Render {
            input_var: "fresh".into(),
            store_as: "brief".into(),
            format: RenderFormat::Summary,
        },
        RecipeStep::Think {
            prompt: "Polish {{fresh}}".into(),
            store_as: "final".into(),
            max_tokens: None,
            think: None,
            on_error: ErrorAction::Fail,
        },
    ];
    let var = reserved_observation_var("inbox_state");
    bind_reserved_observation(&mut steps, &var).unwrap();
    match &steps[0] {
        RecipeStep::Tool { store_as, .. } => assert_eq!(store_as, &var),
        other => panic!("{other:?}"),
    }
    // The shadowing step's own input reads the first read; its output keeps the old name.
    match &steps[1] {
        RecipeStep::Think {
            prompt, store_as, ..
        } => {
            assert_eq!(prompt, &format!("Summarize {{{{{var}}}}}"));
            assert_eq!(store_as, "fresh");
        }
        other => panic!("{other:?}"),
    }
    // Everything after the shadow reads the shadow, untouched.
    match &steps[2] {
        RecipeStep::Render { input_var, .. } => assert_eq!(input_var, "fresh"),
        other => panic!("{other:?}"),
    }
    match &steps[3] {
        RecipeStep::Think { prompt, .. } => assert_eq!(prompt, "Polish {{fresh}}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acquisition_reads_its_own_evidence_and_refuses_a_carrier_that_is_not_the_claimed_one() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(Arc::new(ScriptedLLM::new(READ_ONLY_PLAN)), store.clone());
    let start = now_ms();
    declared_goal(&engine, "goal:evidence", 1, start).unwrap();
    engine.resume_due_horizons(start).await;
    let sha = store
        .active_checkpoint_sha256("goal:evidence")
        .unwrap()
        .unwrap();
    let right = mind_spec::assumption_id("inbox_state");
    // Not claimed yet: the carrier is pending, not running — no acquisition, nothing written.
    assert!(store
        .acquire_replan("goal:evidence", start + 1, &sha, &right, 1)
        .is_err());
    assert_eq!(
        store.load_horizon_lifecycle("goal:evidence").unwrap().len(),
        3
    );
    // A wrong digest is refused before anything is read further.
    store.claim_due_horizon_jobs(start + 1).unwrap();
    assert!(store
        .acquire_replan("goal:evidence", start + 1, &"0".repeat(64), &right, 1)
        .is_err());
    // The caller names a different target revision than the stored carrier: mismatch,
    // terminal, written once — even though the chain itself was well formed.
    let blocked = store
        .acquire_replan("goal:evidence", start + 1, &sha, &right, 2)
        .unwrap();
    assert!(matches!(
        blocked,
        mind_spec::ReplanAcquisition::Blocked(mind_spec::ReplanBlock::Mismatch { .. })
    ));
    let receipts = store.load_horizon_lifecycle("goal:evidence").unwrap();
    assert_eq!(
        receipts.last().map(|r| r.event),
        Some(HorizonLifecycleEvent::ReplanIntegrityFailed)
    );
}

/// Park a declared goal and claim its carrier, returning the checkpoint digest.
async fn parked_and_claimed(
    engine: &RecipeEngine,
    store: &RecipeStore,
    goal: &str,
    start: u64,
) -> String {
    declared_goal(engine, goal, 1, start).unwrap();
    engine.resume_due_horizons(start).await;
    assert_eq!(store.claim_due_horizon_jobs(start + 1).unwrap().len(), 1);
    store.active_checkpoint_sha256(goal).unwrap().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_closure_that_does_not_close_the_open_marker_is_the_terminal_integrity_outcome() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(Arc::new(ScriptedLLM::new(READ_ONLY_PLAN)), store.clone());
    let start = now_ms();
    let id = mind_spec::assumption_id("inbox_state");

    // Success closure with the wrong attempt: nothing advances, the checkpoint stays parked.
    let sha = parked_and_claimed(&engine, &store, "goal:close-a", start).await;
    let acquired = store
        .acquire_replan("goal:close-a", start + 1, &sha, &id, 1)
        .unwrap();
    assert_eq!(
        acquired,
        mind_spec::ReplanAcquisition::Initial { attempt: 1 }
    );
    let mut run = store
        .load_horizon("goal:close-a", start + 2)
        .unwrap()
        .unwrap();
    run.replan(vec!["revision 1: read".into()]).unwrap();
    let checkpoint = run.checkpoint(start + 2).unwrap();
    let (carrier, _, _) = store.queued_horizon_job("goal:close-a").unwrap().unwrap();
    let mut next = carrier.clone();
    next.kind = HorizonJobKind::Segment;
    next.segment_id = "segment:2".into();
    next.recipe.steps = vec![RecipeStep::Tool {
        tool_name: "inbox".into(),
        args: serde_json::json!({"limit": 2}),
        store_as: "fresh".into(),
        on_error: ErrorAction::Fail,
    }];
    next.cost_units = 1;
    next.complete_on_success = true;
    let committed = store.commit_replan(&checkpoint, &next, &id, 2, 1).unwrap();
    assert!(!committed, "attempt 2 is not the open marker");
    let parked = store
        .load_horizon("goal:close-a", start + 3)
        .unwrap()
        .unwrap();
    assert_eq!(parked.status, HorizonStatus::AwaitingReplan);
    assert_eq!(parked.plan_revision, 0);
    let receipts = store.load_horizon_lifecycle("goal:close-a").unwrap();
    assert_eq!(
        receipts.last().map(|r| r.event),
        Some(HorizonLifecycleEvent::ReplanIntegrityFailed)
    );
    assert_eq!(
        receipts
            .iter()
            .filter(|r| r.event == HorizonLifecycleEvent::ReplanIntegrityFailed)
            .count(),
        1
    );
    let (_, status, code) = store.queued_horizon_job("goal:close-a").unwrap().unwrap();
    assert_eq!(
        (status.as_str(), code.as_deref()),
        ("failed", Some("replan_lifecycle_mismatch"))
    );
    assert!(store
        .control_horizon("goal:close-a", HorizonControlAction::Retry, start + 4)
        .err()
        .is_some_and(|e| e.to_string().contains("terminal")));
    // After the integrity receipt, a failure closure appends nothing.
    assert!(!store
        .fail_replan_attempt(
            "goal:close-a",
            HorizonFailureReason::ReplanPlanner,
            1,
            start + 5
        )
        .unwrap());
    assert_eq!(
        store.load_horizon_lifecycle("goal:close-a").unwrap().len(),
        receipts.len()
    );

    // Failure closure with the wrong attempt: the same terminal outcome.
    let sha = parked_and_claimed(&engine, &store, "goal:close-b", start).await;
    store
        .acquire_replan("goal:close-b", start + 1, &sha, &id, 1)
        .unwrap();
    assert!(!store
        .fail_replan_attempt(
            "goal:close-b",
            HorizonFailureReason::ReplanPlanner,
            2,
            start + 2
        )
        .unwrap());
    let receipts = store.load_horizon_lifecycle("goal:close-b").unwrap();
    assert_eq!(
        receipts.last().map(|r| r.event),
        Some(HorizonLifecycleEvent::ReplanIntegrityFailed)
    );
    let (_, status, code) = store.queued_horizon_job("goal:close-b").unwrap().unwrap();
    assert_eq!(
        (status.as_str(), code.as_deref()),
        ("failed", Some("replan_lifecycle_mismatch"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stored_integrity_digest_that_is_not_the_prefix_stays_terminal_and_writes_nothing() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(Arc::new(ScriptedLLM::new(READ_ONLY_PLAN)), store.clone());
    let start = now_ms();
    let id = mind_spec::assumption_id("inbox_state");
    let sha = parked_and_claimed(&engine, &store, "goal:digest", start).await;
    // An integrity record whose digest is not the chain prefix's.
    store
        .append_integrity_for_test("goal:digest", start + 1, "f".repeat(64))
        .unwrap();
    let before = store.load_horizon_lifecycle("goal:digest").unwrap().len();
    store.force_job_running_for_test("goal:digest").unwrap();
    let blocked = store
        .acquire_replan("goal:digest", start + 2, &sha, &id, 1)
        .unwrap();
    assert_eq!(
        blocked,
        mind_spec::ReplanAcquisition::Blocked(mind_spec::ReplanBlock::IntegrityRecordMismatch)
    );
    assert_eq!(
        store.load_horizon_lifecycle("goal:digest").unwrap().len(),
        before
    );
    let (_, status, code) = store.queued_horizon_job("goal:digest").unwrap().unwrap();
    assert_eq!(
        (status.as_str(), code.as_deref()),
        ("failed", Some("replan_lifecycle_mismatch"))
    );
    assert!(store
        .control_horizon("goal:digest", HorizonControlAction::Retry, start + 3)
        .err()
        .is_some_and(|e| e.to_string().contains("terminal")));
    // Through the scheduler the same claim ends as a blocked failure with no plain FAILED.
    store.force_job_running_for_test("goal:digest").unwrap();
    let (carrier, _, _) = store.queued_horizon_job("goal:digest").unwrap().unwrap();
    let mut run = store
        .load_horizon("goal:digest", start + 4)
        .unwrap()
        .unwrap();
    let outcome = engine
        .replan_horizon(&store, &mut run, &carrier, &id, 1, start + 4)
        .await;
    assert_eq!(outcome.state, HorizonTickState::Failed);
    assert!(outcome
        .error
        .as_deref()
        .is_some_and(|e| e.contains("blocked")));
    assert_eq!(
        store.load_horizon_lifecycle("goal:digest").unwrap().len(),
        before
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_or_malformed_failure_closure_is_never_an_idempotent_no_op() {
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let engine = engine_with(Arc::new(ScriptedLLM::new(READ_ONLY_PLAN)), store.clone());
    let start = now_ms();
    let id = mind_spec::assumption_id("inbox_state");

    // Closed attempt 1, then retry opens attempt 2, then a STALE close of attempt 1 arrives.
    let sha = parked_and_claimed(&engine, &store, "goal:stale", start).await;
    assert_eq!(
        store
            .acquire_replan("goal:stale", start + 1, &sha, &id, 1)
            .unwrap(),
        mind_spec::ReplanAcquisition::Initial { attempt: 1 }
    );
    assert!(store
        .fail_replan_attempt(
            "goal:stale",
            HorizonFailureReason::ReplanPlanner,
            1,
            start + 2
        )
        .unwrap());
    // The same closure again, with nothing newer: idempotent, nothing appended.
    let before = store.load_horizon_lifecycle("goal:stale").unwrap().len();
    store.force_job_running_for_test("goal:stale").unwrap();
    assert!(store
        .fail_replan_attempt(
            "goal:stale",
            HorizonFailureReason::ReplanPlanner,
            1,
            start + 3
        )
        .unwrap());
    assert_eq!(
        store.load_horizon_lifecycle("goal:stale").unwrap().len(),
        before
    );
    // Back to the state the scheduler leaves (failed, retryable), then Retry → claim →
    // Branch C opens attempt 2.
    store
        .force_job_failed_for_test("goal:stale", "replan_planner_failed")
        .unwrap();
    store
        .control_horizon("goal:stale", HorizonControlAction::Retry, start + 5)
        .unwrap();
    assert_eq!(store.claim_due_horizon_jobs(start + 6).unwrap().len(), 1);
    assert_eq!(
        store
            .acquire_replan("goal:stale", start + 6, &sha, &id, 1)
            .unwrap(),
        mind_spec::ReplanAcquisition::Retry { attempt: 2 }
    );
    // Now the stale closure of attempt 1: not a re-entry — the terminal integrity outcome.
    let before = store.load_horizon_lifecycle("goal:stale").unwrap().len();
    assert!(!store
        .fail_replan_attempt(
            "goal:stale",
            HorizonFailureReason::ReplanPlanner,
            1,
            start + 7
        )
        .unwrap());
    let receipts = store.load_horizon_lifecycle("goal:stale").unwrap();
    assert_eq!(receipts.len(), before + 1);
    assert_eq!(
        receipts.last().map(|r| r.event),
        Some(HorizonLifecycleEvent::ReplanIntegrityFailed)
    );
    let (_, status, code) = store.queued_horizon_job("goal:stale").unwrap().unwrap();
    assert_eq!(
        (status.as_str(), code.as_deref()),
        ("failed", Some("replan_lifecycle_mismatch"))
    );

    // Malformed chain that contains the closed target attempt: a skipped ordinal after it.
    let sha = parked_and_claimed(&engine, &store, "goal:malformed", start).await;
    store
        .acquire_replan("goal:malformed", start + 1, &sha, &id, 1)
        .unwrap();
    assert!(store
        .fail_replan_attempt(
            "goal:malformed",
            HorizonFailureReason::ReplanPlanner,
            1,
            start + 2
        )
        .unwrap());
    store
        .append_started_for_test("goal:malformed", start + 3, &id, 3, 1)
        .unwrap();
    store.force_job_running_for_test("goal:malformed").unwrap();
    let before = store
        .load_horizon_lifecycle("goal:malformed")
        .unwrap()
        .len();
    assert!(!store
        .fail_replan_attempt(
            "goal:malformed",
            HorizonFailureReason::ReplanPlanner,
            1,
            start + 4
        )
        .unwrap());
    let receipts = store.load_horizon_lifecycle("goal:malformed").unwrap();
    assert_eq!(receipts.len(), before + 1);
    assert_eq!(
        receipts.last().map(|r| r.event),
        Some(HorizonLifecycleEvent::ReplanIntegrityFailed)
    );
}
