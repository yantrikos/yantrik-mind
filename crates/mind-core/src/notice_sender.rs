//! E.NOTICE1 (Stage A, the Mind half): obligation notices -- a reminder the person asked for come
//! due, the outcome of work they delegated -- leave the Mind's durable outbox for the desktop.
//!
//! The outbox is the L3b notice queue, not a second one: a notice is queued once (its dedupe key),
//! leased for a send, and closed with a `delivered` receipt only when the desktop says it stored it.
//! A send that is not acknowledged leaves the lease to run out, and the next pass leases it again --
//! that is the retry; the desktop deduplicates by notice id. The transport is a [`NoticeSink`]; until
//! the desktop advertises that it keeps notices there is no sink, and nothing is leased or lost.
use mind_conversation::ConversationEngine;

/// How long one send may hold a notice before it is leased again.
pub(crate) const NOTICE_LEASE_MS: u64 = 60_000;
/// How many notices one pass sends at most.
pub(crate) const NOTICE_BATCH: usize = 10;

/// Where an obligation notice is stored for the person: the desktop's inbox.
pub(crate) trait NoticeSink {
    /// Store one notice durably. `Ok(true)` only once the desktop said it stored this notice id;
    /// anything else leaves it to be sent again.
    fn store(
        &self,
        notice: &mind_recipes::LeasedNotice,
    ) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send;
}

/// One send pass at `now_ms`: lease the obligation notices, store each, and close the ones the
/// desktop kept. Returns (delivered, left for a retry).
pub(crate) async fn send_obligations<S: NoticeSink>(
    conv: &ConversationEngine,
    sink: &S,
    now_ms: u64,
) -> anyhow::Result<(usize, usize)> {
    let leased = conv.lease_obligation_notices(now_ms, NOTICE_LEASE_MS, NOTICE_BATCH)?;
    let (mut delivered, mut retry) = (0, 0);
    for n in leased {
        match sink.store(&n).await {
            Ok(true) => {
                conv.ack_notice_delivered(&n.notice_id, &n.lease_id, now_ms)?;
                delivered += 1;
            }
            Ok(false) | Err(_) => retry += 1,
        }
    }
    Ok((delivered, retry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct NoTools;
    #[mind_recipes::async_trait_rt::async_trait]
    impl mind_recipes::RecipeHost for NoTools {
        async fn call_tool(&self, _tool: &str, _args: &serde_json::Value) -> anyhow::Result<String> {
            anyhow::bail!("no tools in this fixture")
        }
    }

    fn engine() -> (Arc<ConversationEngine>, Arc<dyn mind_types::MemoryFacade>) {
        let mem: Arc<dyn mind_types::MemoryFacade> =
            Arc::new(mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap());
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("unused")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let store = Arc::new(mind_recipes::RecipeStore::open(":memory:").unwrap());
        let recipes = mind_recipes::RecipeEngine::new(pool.clone(), Arc::new(NoTools), "YM").with_store(store);
        (Arc::new(ConversationEngine::new(mem.clone(), pool, "YM").with_recipes(Arc::new(recipes))), mem)
    }

    /// The desktop as a test: it keeps what it is told to keep, or says nothing.
    struct Desk {
        keeps: bool,
        got: Mutex<Vec<String>>,
    }
    impl NoticeSink for Desk {
        async fn store(&self, n: &mind_recipes::LeasedNotice) -> anyhow::Result<bool> {
            self.got.lock().unwrap().push(n.text.clone());
            Ok(self.keeps)
        }
    }

    const HOUR: u64 = 3_600_000;

    /// A reminder due an hour ago is announced once however often the scan runs; one overdue by
    /// two days is not "coming due" and is not announced.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_due_reminder_is_one_notice_and_an_old_one_none() {
        let (conv, mem) = engine();
        let now = 10_000 * HOUR;
        mem.add_task("Stretch", "medium", Some(now - HOUR)).await.unwrap();
        mem.add_task("Renew the passport form", "medium", Some(now - 48 * HOUR)).await.unwrap();
        mem.add_task("Call the bank", "medium", Some(now + HOUR)).await.unwrap();
        let mut fresh = 0;
        for i in 0..3 {
            fresh += conv.queue_due_reminders_at(now + i * 60_000).await.unwrap();
        }
        assert_eq!(fresh, 1, "one occurrence, one notice");
        let leased = conv.lease_obligation_notices(now, NOTICE_LEASE_MS, 10).unwrap();
        assert_eq!(leased.len(), 1);
        assert!(leased[0].text.contains("Stretch"), "{}", leased[0].text);
    }

    /// Kept by the desktop: delivered, never sent again. Not kept: sent again once the lease ran out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_kept_notice_is_not_resent_and_an_unkept_one_is() {
        let (conv, mem) = engine();
        let now = 10_000 * HOUR;
        mem.add_task("Stretch", "medium", Some(now - HOUR)).await.unwrap();
        conv.queue_due_reminders_at(now).await.unwrap();

        let silent = Desk { keeps: false, got: Mutex::new(Vec::new()) };
        assert_eq!(send_obligations(&conv, &silent, now).await.unwrap(), (0, 1));
        // Still under the first lease: nothing to send.
        assert_eq!(send_obligations(&conv, &silent, now + 1_000).await.unwrap(), (0, 0));
        let desk = Desk { keeps: true, got: Mutex::new(Vec::new()) };
        // After the lease runs out it is sent again, and this time kept.
        assert_eq!(send_obligations(&conv, &desk, now + NOTICE_LEASE_MS).await.unwrap(), (1, 0));
        assert_eq!(send_obligations(&conv, &desk, now + 10 * NOTICE_LEASE_MS).await.unwrap(), (0, 0));
        assert_eq!(desk.got.lock().unwrap().len(), 1);
    }

    /// The loop step: it queues where no phone is reachable, and leaves a reachable phone's own
    /// reminder loop to announce it -- one announcement either way.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_loop_step_queues_only_where_no_phone_is_reachable() {
        use std::sync::atomic::AtomicI64;
        let now = crate::telegram::now_ms();
        let (conv, mem) = engine();
        mem.add_task("Stretch", "medium", Some(now - 60_000)).await.unwrap();
        let phone = crate::delivery::Delivery::new(
            conv.clone(),
            Some(crate::delivery::TelegramTarget { api: "unused".into(), active_chat: Arc::new(AtomicI64::new(42)) }),
        );
        crate::loops::run_reminders(&conv, &phone, 0).await;
        assert!(conv.lease_obligation_notices(now, NOTICE_LEASE_MS, 10).unwrap().is_empty(), "the phone announces it");
        let desk = crate::delivery::Delivery::new(conv.clone(), None);
        crate::loops::run_reminders(&conv, &desk, 0).await;
        assert_eq!(conv.lease_obligation_notices(now + 1, NOTICE_LEASE_MS, 10).unwrap().len(), 1);
    }

    /// The console's own lines are not obligations: the desktop sender never takes them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_desktop_sender_leaves_the_consoles_lines_alone() {
        let (conv, _mem) = engine();
        conv.queue_notice(mind_observability::DeliveryKind::Verdict, "A prediction resolved").unwrap();
        let desk = Desk { keeps: true, got: Mutex::new(Vec::new()) };
        assert_eq!(send_obligations(&conv, &desk, crate::telegram::now_ms()).await.unwrap(), (0, 0));
        assert!(desk.got.lock().unwrap().is_empty());
    }
}
