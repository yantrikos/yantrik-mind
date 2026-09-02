//! L1d-B — the detached speakers' evidence: one side-effect-free state read per loop supplies
//! the last stamp and the EFFECTIVE period to both the gate and the ledger's cadence line; the
//! forge opportunity is the exact target the act would consume and the act revalidates it; the
//! news act returns each topic with its pre-advance stamp.
use crate::*;
use mind_inference::ScriptedLLM;
use mind_memory::MemoryHandle;
use mind_recipes::RecipeStore;
use yantrik_ml::LLMBackend;

struct NoTools;
#[async_trait::async_trait]
impl RecipeHost for NoTools {
    async fn call_tool(&self, _tool: &str, _args: &serde_json::Value) -> anyhow::Result<String> {
        anyhow::bail!("no tools in this fixture")
    }
}

fn harness() -> ConversationEngine {
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(
        Arc::new(ScriptedLLM::new("unused")) as Arc<dyn LLMBackend>,
        1,
    );
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let recipes =
        RecipeEngine::new(pool.clone(), Arc::new(NoTools), "JARVIS").with_store(store.clone());
    ConversationEngine::new(mem, pool, "JARVIS").with_recipes(Arc::new(recipes))
}

/// The state helpers report the period the due rule actually consults — defaults, the f64
/// hour envs' shape, the gift domain pace — and the persisted last stamp verbatim.
#[tokio::test]
async fn detached_state_helpers_report_the_effective_period_and_the_last_stamp() {
    let conv = harness();
    assert_eq!(conv.work_watch_state().await, (0, 8 * 3_600_000));
    assert_eq!(conv.work_radar_state().await, (0, 6 * 3_600_000));
    assert_eq!(conv.report_state().await, (0, 7 * 86_400_000));
    assert_eq!(conv.gift_scout_state().await, (0, 86_400_000));
    // The gift pace multiplies the period; the stamp is read verbatim.
    let _ = conv.memory.profile_set("pace:gift", "2").await;
    let _ = conv.memory.profile_set("gift_scout_last", "1234567").await;
    assert_eq!(conv.gift_scout_state().await, (1_234_567, 2 * 86_400_000));
    let _ = conv.memory.profile_set("workops_last", "42").await;
    let _ = conv.memory.profile_set("radar_last", "43").await;
    let _ = conv.memory.profile_set("report_last", "44").await;
    assert_eq!(conv.work_watch_state().await.0, 42);
    assert_eq!(conv.work_radar_state().await.0, 43);
    assert_eq!(conv.report_state().await.0, 44);
    // Negative or unreadable stamps read as 0, never as a huge unsigned value.
    let _ = conv.memory.profile_set("workops_last", "-5").await;
    let _ = conv.memory.profile_set("radar_last", "junk").await;
    assert_eq!(conv.work_watch_state().await.0, 0);
    assert_eq!(conv.work_radar_state().await.0, 0);
}

/// The forge opportunity is ONE exact due target from ONE read — the venture the act would
/// consume (the first active one), only when that venture itself has cooled — and the act
/// refuses any target whose id, stage or stamp moved, touching nothing.
#[tokio::test]
async fn forge_due_target_is_the_exact_cooled_target_and_the_act_revalidates_all_of_it() {
    let conv = harness();
    assert_eq!(
        conv.forge_due_target().await,
        None,
        "no ventures, no target"
    );
    let now = chrono::Utc::now().timestamp_millis();
    // The first active venture is FRESH while a later one is cooled: no due target — a fresh
    // first venture is never re-run because a later venture is due.
    let fresh_first = serde_json::json!({
        "a": {"id": "a", "idea": "done", "stage": "shipped", "updated_ms": 1},
        "b": {"id": "b", "idea": "next", "stage": "research", "updated_ms": now},
        "c": {"id": "c", "idea": "later", "stage": "spec", "updated_ms": 7}
    });
    conv.forge_save(&fresh_first).await;
    assert_eq!(conv.forge_due_target().await, None);
    let ventures = serde_json::json!({
        "a": {"id": "a", "idea": "done", "stage": "shipped", "updated_ms": 1},
        "b": {"id": "b", "idea": "next", "stage": "research", "updated_ms": 5},
        "c": {"id": "c", "idea": "later", "stage": "spec", "updated_ms": 7}
    });
    conv.forge_save(&ventures).await;
    assert_eq!(
        conv.forge_due_target().await,
        Some(("b".into(), "research".into(), 5)),
        "the first active venture, cooled, with its stage and pre-act stamp"
    );
    // Any moved component — another id, the same id at another stage, the same id at another
    // stamp — is refused before any draw, stage work or state write.
    for expect in [("c", "spec", 7), ("b", "spec", 5), ("b", "research", 6)] {
        assert_eq!(
            conv.forge_tick_for(true, Some(expect)).await,
            None,
            "{expect:?}"
        );
        assert_eq!(
            conv.forge_load().await,
            ventures,
            "byte-identical after {expect:?}"
        );
    }
    assert_eq!(
        conv.forge_due_target().await,
        Some(("b".into(), "research".into(), 5))
    );
}

/// Opaque per-item keys: two news topics sharing one stamp (both 0 on a first run) mint two
/// opportunities; one topic's successive digests mint distinct ones; two events starting at
/// the same instant stay two opportunities; both keys are stable.
#[test]
fn per_item_keys_never_collide_on_a_shared_stamp_or_start() {
    assert_ne!(
        ConversationEngine::news_digest_key("oil", 0),
        ConversationEngine::news_digest_key("markets", 0)
    );
    assert_ne!(
        ConversationEngine::news_digest_key("oil", 0),
        ConversationEngine::news_digest_key("oil", 1)
    );
    assert_eq!(
        ConversationEngine::news_digest_key("oil", 0),
        ConversationEngine::news_digest_key("oil", 0)
    );
    assert_ne!(
        ConversationEngine::event_prep_key("Dentist", 1_700_000_000_000),
        ConversationEngine::event_prep_key("Standup", 1_700_000_000_000)
    );
    assert_ne!(
        ConversationEngine::event_prep_key("Dentist", 1_700_000_000_000),
        ConversationEngine::event_prep_key("Dentist", 1_700_000_000_001)
    );
    assert_eq!(
        ConversationEngine::event_prep_key("Dentist", 1_700_000_000_000),
        mind_observability::opportunity_key_digest("Dentist|1700000000000"),
        "the exact persisted key"
    );
}

/// With no news source there is nothing due — keyed and unkeyed agree.
#[tokio::test]
async fn news_digests_due_keyed_agrees_with_the_topic_list() {
    let conv = harness();
    let keyed = conv.news_digests_due_keyed().await;
    let topics = conv.news_digests_due().await;
    assert_eq!(
        keyed.iter().map(|(t, _)| t.clone()).collect::<Vec<_>>(),
        topics
    );
    assert!(keyed.is_empty());
}
