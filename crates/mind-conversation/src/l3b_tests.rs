//! L3b — the engine's side of the delivery seam: notices queue under a delivery kind with a
//! per-day dedupe key; a queued notice never counts as spoken; the lease/acknowledge cycle works
//! through the engine; `why deliveries` reports the queue's depth; without a store the queue is
//! an error, never a silent drop.
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

fn harness(with_store: bool) -> (ConversationEngine, Option<Arc<RecipeStore>>) {
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(
        Arc::new(ScriptedLLM::new("unused")) as Arc<dyn LLMBackend>,
        1,
    );
    let store = with_store.then(|| Arc::new(RecipeStore::open(":memory:").unwrap()));
    let mut recipes = RecipeEngine::new(pool.clone(), Arc::new(NoTools), "JARVIS");
    if let Some(store) = &store {
        recipes = recipes.with_store(store.clone());
    }
    let conv = ConversationEngine::new(mem, pool, "JARVIS").with_recipes(Arc::new(recipes));
    (conv, store)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_queued_notice_is_deduped_per_day_and_never_counts_as_spoken() {
    let (conv, _store) = harness(true);
    assert!(conv.has_notice_queue());
    let a = conv
        .queue_notice(
            mind_observability::DeliveryKind::HorizonTick,
            "⌛ Long-horizon goal goal:x expired unfinished: budget_elapsed. Nothing was sent.",
        )
        .unwrap();
    assert!(a.fresh);
    let again = conv
        .queue_notice(
            mind_observability::DeliveryKind::HorizonTick,
            "⌛ Long-horizon goal goal:x expired unfinished: budget_elapsed. Nothing was sent.",
        )
        .unwrap();
    assert!(!again.fresh, "the same line in the same day is one notice");
    assert_eq!(again.notice_id, a.notice_id);
    // Raw variants that render identically (trailing whitespace, a control byte) are ONE notice.
    let variant = conv
        .queue_notice(
            mind_observability::DeliveryKind::HorizonTick,
            "⌛ Long-horizon goal goal:x expired unfinished: budget_elapsed. Nothing was sent.\u{7}  ",
        )
        .unwrap();
    assert!(!variant.fresh);
    assert_eq!(variant.notice_id, a.notice_id);
    let other = conv
        .queue_notice(mind_observability::DeliveryKind::Verdict, "✅ verdict")
        .unwrap();
    assert!(other.fresh);
    // Nothing about queueing is "spoken": no proactive send is pending.
    assert!(!conv.spoke_recently(24 * 60 * 60 * 1000).await);
    assert_eq!(conv.notice_queue_depth().unwrap(), (2, 0));
    // Lease through the engine, acknowledge, and the depth drops.
    let leased = conv.lease_notices(60_000, 10).unwrap();
    assert_eq!(leased.len(), 2);
    assert_eq!(leased[0].notice_id, a.notice_id, "oldest first");
    assert!(conv
        .ack_notice_shown(&leased[0].notice_id, &leased[0].lease_id)
        .unwrap());
    assert_eq!(conv.notice_queue_depth().unwrap(), (1, 1));
    let history = conv.notice_history(10).unwrap();
    assert_eq!(history.len(), 2);
    // The console readout names the queue by counts only.
    let out = conv
        .cli_dispatch(
            "why deliveries",
            &mind_types::AccessContext::operator_audit(),
        )
        .await;
    assert!(
        out.contains("CONSOLE NOTICE QUEUE — unseen 1 · under a live lease 1"),
        "{out}"
    );
    assert!(!out.contains("budget_elapsed"), "never the text");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_store_the_queue_is_an_error_not_a_silent_drop() {
    let (conv, _none) = harness(false);
    assert!(!conv.has_notice_queue());
    assert!(conv
        .queue_notice(mind_observability::DeliveryKind::Pattern, "💡 found")
        .is_err());
    assert!(conv.lease_notices(60_000, 10).is_err());
    let out = conv
        .cli_dispatch(
            "why deliveries",
            &mind_types::AccessContext::operator_audit(),
        )
        .await;
    assert!(
        out.contains("CONSOLE NOTICE QUEUE — unavailable on this build"),
        "{out}"
    );
}
