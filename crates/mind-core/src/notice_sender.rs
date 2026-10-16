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

/// What the desktop said to one notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stored {
    /// On its disk (a duplicate id too: it already was).
    Kept,
    /// Not now: send nothing more for this many seconds, or retry on the next pass.
    Later(Option<u64>),
    /// Never: this notice is not sent again.
    Refused(String),
}

/// Where an obligation notice is stored for the person: the desktop's inbox.
pub(crate) trait NoticeSink {
    /// Store one notice durably. `Kept` only once the desktop said it stored this notice id; an
    /// error leaves it to be sent again.
    fn store(
        &self,
        notice: &mind_recipes::LeasedNotice,
    ) -> impl std::future::Future<Output = anyhow::Result<Stored>> + Send;
}

/// What one send pass did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Pass {
    pub(crate) delivered: usize,
    /// Left for a retry once its lease runs out.
    pub(crate) retry: usize,
    pub(crate) refused: usize,
    /// The desktop asked for nothing more for this many seconds.
    pub(crate) wait_secs: Option<u64>,
}

/// One send pass at `now_ms`: lease the obligation notices (none the desktop refused, none too
/// old), store each, and close the ones the desktop kept. A "not now" ends the pass.
pub(crate) async fn send_obligations<S: NoticeSink>(
    conv: &ConversationEngine,
    sink: &S,
    now_ms: u64,
    refused: &mut std::collections::HashSet<String>,
) -> anyhow::Result<Pass> {
    let leased = conv.lease_obligation_notices_except(now_ms, NOTICE_LEASE_MS, NOTICE_BATCH, refused)?;
    let mut pass = Pass::default();
    for n in leased {
        match sink.store(&n).await {
            Ok(Stored::Kept) => {
                conv.ack_notice_delivered(&n.notice_id, &n.lease_id, now_ms)?;
                conv.note_card_delivered(&n.text, now_ms).await;
                pass.delivered += 1;
            }
            Ok(Stored::Refused(why)) => {
                eprintln!("[notice] the desktop refused {}: {why}", n.notice_id);
                refused.insert(n.notice_id.clone());
                pass.refused += 1;
            }
            Ok(Stored::Later(wait)) => {
                pass.retry += 1;
                if wait.is_some() {
                    pass.wait_secs = wait;
                    break;
                }
            }
            Err(_) => pass.retry += 1,
        }
    }
    Ok(pass)
}

/// E.NOTICE1 A3: the feature an attach answer names when its desktop keeps notices.
pub(crate) const NOTICE_INBOX: &str = "notice_inbox_v1";

/// Whether a `harness.attach` answer says this desktop keeps notices.
pub(crate) fn takes_notices(attach: &serde_json::Value) -> bool {
    attach["features"].as_array().is_some_and(|f| f.iter().any(|x| x.as_str() == Some(NOTICE_INBOX)))
}

/// A leased obligation notice as `harness.notice` carries it (yantrik-os docs/harness.md).
pub(crate) fn notice_payload(n: &mind_recipes::LeasedNotice) -> serde_json::Value {
    let reminder = n.kind == mind_spec::NoticeKind::Reminder;
    let text = n.text.trim();
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    let first = first.trim();
    let first = if reminder { first.strip_prefix("Reminder:").map(str::trim).unwrap_or(first) } else { first };
    let title: String = first.chars().take(80).collect();
    serde_json::json!({
        "notice_id": n.notice_id,
        "family": if reminder { "reminder" } else { "delegated" },
        "title": title,
        "body": rest.trim(),
        "why": if reminder { "You asked me to remind you." } else { "You handed this to me." },
        "attention": if reminder { "interrupt" } else { "inbox" },
    })
}

/// The desktop's answer to `harness.notice`. Anything it did not say plainly is "not now".
pub(crate) fn read_answer(answer: &serde_json::Value) -> Stored {
    if answer["stored"].as_bool() == Some(true) {
        return Stored::Kept;
    }
    if let Some(why) = answer.get("refused").filter(|r| !r.is_null()) {
        let why = why.as_str().map(str::to_string).unwrap_or_else(|| why.to_string());
        return Stored::Refused(why);
    }
    Stored::Later(answer["retry_after_secs"].as_u64())
}

/// The highest feedback `seq` a poll answer carries, to ack on the next poll; each entry is
/// journalled (acting on it is Stage B).
pub(crate) fn feedback_seen(poll: &serde_json::Value) -> Option<u64> {
    let entries = poll["notice_feedback"].as_array()?;
    let mut top = None;
    for e in entries {
        eprintln!(
            "[notice] feedback: {} on {}",
            e["kind"].as_str().unwrap_or("?"),
            e["notice_id"].as_str().unwrap_or("?")
        );
        if let Some(seq) = e["seq"].as_u64() {
            top = top.max(Some(seq));
        }
    }
    top
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
        async fn store(&self, n: &mind_recipes::LeasedNotice) -> anyhow::Result<Stored> {
            self.got.lock().unwrap().push(n.text.clone());
            Ok(if self.keeps { Stored::Kept } else { Stored::Later(None) })
        }
    }

    fn pass(delivered: usize, retry: usize) -> Pass {
        Pass { delivered, retry, ..Default::default() }
    }

    async fn send(conv: &ConversationEngine, desk: &Desk, now: u64) -> Pass {
        send_obligations(conv, desk, now, &mut Default::default()).await.unwrap()
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

    /// E.REM2 (541: past-due reminders stayed open for hours): a reminder that came due is held for
    /// the next reply once and closed; the backlog an earlier scan queued is closed without a second
    /// hold; one two days overdue is left alone; and the same text can be set again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reminder_that_came_due_is_said_and_closed() {
        let (conv, mem) = engine();
        let now = crate::telegram::now_ms();
        let stretch = mem.add_task("Stretch", "medium", Some(now - HOUR)).await.unwrap();
        let old = mem.add_task("Renew the passport form", "medium", Some(now - 48 * HOUR)).await.unwrap();
        conv.queue_due_reminders_at(now).await.unwrap();
        let held = conv.take_held_notes();
        assert_eq!(held.len(), 1, "{held:?}");
        assert!(held[0].contains("Reminder: Stretch") && held[0].contains("due"), "{}", held[0]);
        let open: Vec<String> = mem.list_tasks(false).await.unwrap().into_iter().map(|t| t.id).collect();
        assert!(!open.contains(&stretch.id), "the reminder that came due is still open");
        assert!(open.contains(&old.id), "a two-day-overdue task was touched");
        conv.queue_due_reminders_at(now + 60_000).await.unwrap();
        assert!(conv.take_held_notes().is_empty(), "held twice");
        // Closed, so the same text is a new reminder (add_task merges only into OPEN tasks).
        let again = mem.add_task("Stretch", "medium", Some(now + 2 * 60_000)).await.unwrap();
        assert_ne!(again.id, stretch.id, "the same text merged into the closed reminder");
    }

    /// E.REM2: a reminder an earlier build queued and left open is closed, and not said again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_backlog_reminder_is_closed_without_a_second_hold() {
        let mem: Arc<dyn mind_types::MemoryFacade> = Arc::new(mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap());
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("unused")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let store = Arc::new(mind_recipes::RecipeStore::open(":memory:").unwrap());
        let recipes = mind_recipes::RecipeEngine::new(pool.clone(), Arc::new(NoTools), "YM").with_store(store.clone());
        let conv = ConversationEngine::new(mem.clone(), pool, "YM").with_recipes(Arc::new(recipes));
        let now = crate::telegram::now_ms();
        let due = now - HOUR;
        let t = mem.add_task("Check the oven", "medium", Some(due)).await.unwrap();
        // What A1 (before E.REM2) left behind: the notice queued, the task still open.
        store
            .queue_notice("primary", mind_spec::NoticeKind::Reminder, "Reminder: Check the oven", &format!("reminder:{}:{due}", t.id), now - HOUR)
            .unwrap();
        conv.queue_due_reminders_at(now).await.unwrap();
        assert!(conv.take_held_notes().is_empty(), "the backlog was said again");
        assert!(!mem.list_tasks(false).await.unwrap().iter().any(|x| x.id == t.id), "the backlog stayed open");
    }

    /// Kept by the desktop: delivered, never sent again. Not kept: sent again once the lease ran out.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_kept_notice_is_not_resent_and_an_unkept_one_is() {
        let (conv, mem) = engine();
        let now = 10_000 * HOUR;
        mem.add_task("Stretch", "medium", Some(now - HOUR)).await.unwrap();
        conv.queue_due_reminders_at(now).await.unwrap();

        let silent = Desk { keeps: false, got: Mutex::new(Vec::new()) };
        assert_eq!(send(&conv, &silent, now).await, pass(0, 1));
        // E.NOTICE1 A3b: what was not kept is not in the conversation's record.
        let said = |mem: Arc<dyn mind_types::MemoryFacade>| async move {
            mem.recent_messages(20, &mind_types::AccessContext::operator_audit())
                .await
                .unwrap()
                .into_iter()
                .filter(|(r, t)| r == "assistant" && t.contains("shown on their desktop as a card"))
                .map(|(_, t)| t)
                .collect::<Vec<_>>()
        };
        assert!(said(mem.clone()).await.is_empty(), "an unkept notice was recorded as shown");
        // Still under the first lease: nothing to send.
        assert_eq!(send(&conv, &silent, now + 1_000).await, pass(0, 0));
        let desk = Desk { keeps: true, got: Mutex::new(Vec::new()) };
        // After the lease runs out it is sent again, and this time kept.
        assert_eq!(send(&conv, &desk, now + NOTICE_LEASE_MS).await, pass(1, 0));
        assert_eq!(send(&conv, &desk, now + 10 * NOTICE_LEASE_MS).await, pass(0, 0));
        assert_eq!(desk.got.lock().unwrap().len(), 1);
        // E.NOTICE1 A3b: the kept card is in the conversation's record, once.
        let shown = said(mem.clone()).await;
        assert_eq!(shown.len(), 1, "{shown:?}");
        assert!(shown[0].starts_with("(The reminder \"Stretch\" fired at") && shown[0].contains("no longer pending"), "{}", shown[0]);
        // E.NOTICE1 A3c: and the next turn's prompt says so, once.
        let line = conv.take_cards_section();
        assert!(line.starts_with("Since your last reply: the reminder \"Stretch\" fired at") && line.contains("not pending"), "{line}");
        assert!(conv.take_cards_section().is_empty(), "said to the model twice");
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

    /// E.NOTICE1 A2 (541, check 2: a research result evaporated): with no phone, a delegated result
    /// is held for the next reply AND queued as a notice; any other result is only held; Telegram's
    /// queues are left for the phone's poll loop to drain.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn finished_results_are_held_and_delegated_ones_become_notices() {
        use std::sync::atomic::AtomicI64;
        let (conv, _mem) = engine();
        conv.push_delegated_result("Research — ext4 vs btrfs: btrfs snapshots, ext4 simpler.");
        conv.push_notification("Google Photos connected".to_string());
        let phone = crate::delivery::Delivery::new(
            conv.clone(),
            Some(crate::delivery::TelegramTarget { api: "unused".into(), active_chat: Arc::new(AtomicI64::new(42)) }),
        );
        crate::loops::run_job_results(&conv, &phone);
        assert!(conv.take_held_notes().is_empty(), "the phone drains its own queues");
        let both = conv.take_notifications();
        assert_eq!(both.len(), 2, "take_notifications drains both queues: {both:?}");

        conv.push_delegated_result("Research — ext4 vs btrfs: btrfs snapshots, ext4 simpler.");
        conv.push_notification("Google Photos connected".to_string());
        let desk = crate::delivery::Delivery::new(conv.clone(), None);
        crate::loops::run_job_results(&conv, &desk);
        let held = conv.take_held_notes();
        assert_eq!(held.len(), 2, "{held:?}");
        let notices = conv.lease_obligation_notices(crate::telegram::now_ms() + 1, NOTICE_LEASE_MS, 10).unwrap();
        assert_eq!(notices.len(), 1, "only the delegated result is an obligation");
        assert!(notices[0].text.contains("ext4"), "{}", notices[0].text);
        assert!(conv.take_notifications().is_empty(), "drained");
    }

    /// E.NOTICE1 A2: every delegated-work site hands its job the typed queue, never notify_queue --
    /// a site that slipped back would make its results un-noticeable again.
    #[test]
    fn delegated_work_reports_through_the_typed_queue() {
        for (name, src, sites) in [
            ("capabilities.rs", include_str!("../../mind-conversation/src/capabilities.rs"), 2),
            ("code.rs", include_str!("../../mind-conversation/src/code.rs"), 3),
            ("delegate.rs", include_str!("../../mind-conversation/src/delegate.rs"), 3),
        ] {
            assert!(!src.contains("notify_queue.clone()"), "{name} hands a job the untyped queue");
            assert_eq!(src.matches("delegated_queue.clone()").count(), sites, "{name}");
        }
    }

    /// The delegated tools promise a delivery only where one will be made.
    #[test]
    fn the_delegated_tools_promise_only_what_is_delivered() {
        let src = include_str!("../../mind-conversation/src/capabilities.rs");
        for (send, next) in [
            ("I'll send what I find here when it's done", "I'll put what I find in my next reply once it's done"),
            ("I'll send the result here when it's done", "I'll put the result in my next reply once it's done"),
        ] {
            let at = src.find(send).unwrap_or_else(|| panic!("{send}"));
            let before = &src[at.saturating_sub(300)..at];
            assert!(before.contains("if host.unprompted_delivery()"), "{send} is not gated");
            assert!(src.contains(next), "{next}");
        }
    }

    /// The console's own lines are not obligations: the desktop sender never takes them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_desktop_sender_leaves_the_consoles_lines_alone() {
        let (conv, _mem) = engine();
        conv.queue_notice(mind_observability::DeliveryKind::Verdict, "A prediction resolved").unwrap();
        let desk = Desk { keeps: true, got: Mutex::new(Vec::new()) };
        assert_eq!(send(&conv, &desk, crate::telegram::now_ms()).await, pass(0, 0));
        assert!(desk.got.lock().unwrap().is_empty());
    }

    /// A desktop that answers each send from a script, recording what it was sent.
    struct Scripted {
        answers: Mutex<Vec<Stored>>,
        got: Mutex<Vec<serde_json::Value>>,
    }
    impl NoticeSink for Scripted {
        async fn store(&self, n: &mind_recipes::LeasedNotice) -> anyhow::Result<Stored> {
            self.got.lock().unwrap().push(notice_payload(n));
            let mut a = self.answers.lock().unwrap();
            Ok(if a.is_empty() { Stored::Kept } else { a.remove(0) })
        }
    }

    /// E.NOTICE1 A3: the attach answer is read for the one feature, and nothing else turns it on.
    #[test]
    fn the_inbox_is_used_only_where_the_desktop_says_it_keeps_notices() {
        assert!(takes_notices(&serde_json::json!({"session": "s1", "features": ["notice_inbox_v1"]})));
        assert!(takes_notices(&serde_json::json!({"features": ["x", "notice_inbox_v1"]})));
        assert!(!takes_notices(&serde_json::json!({"session": "s1"})));
        assert!(!takes_notices(&serde_json::json!({"features": ["notice_inbox_v2"]})));
        assert!(!takes_notices(&serde_json::json!({"features": "notice_inbox_v1"})));
    }

    /// E.NOTICE1 A3: what the desktop is sent for a reminder and for a delegated result.
    #[test]
    fn a_notice_is_sent_as_the_desktop_reads_it() {
        let leased = |kind, text: &str| mind_recipes::LeasedNotice {
            notice_id: "notice:abc".into(),
            lease_id: "l".into(),
            lease_until_ms: 0,
            kind,
            text: text.into(),
            created_ms: 0,
        };
        let r = notice_payload(&leased(mind_spec::NoticeKind::Reminder, "Reminder: Stretch"));
        assert_eq!(
            r,
            serde_json::json!({"notice_id": "notice:abc", "family": "reminder", "title": "Stretch", "body": "",
                               "why": "You asked me to remind you.", "attention": "interrupt"})
        );
        let long = format!("Research done: {}\nbtrfs has snapshots; ext4 is simpler.", "x".repeat(100));
        let d = notice_payload(&leased(mind_spec::NoticeKind::Delegated, &long));
        assert_eq!(d["family"], "delegated");
        assert_eq!(d["attention"], "inbox");
        assert_eq!(d["why"], "You handed this to me.");
        assert_eq!(d["title"].as_str().unwrap().chars().count(), 80);
        assert_eq!(d["body"], "btrfs has snapshots; ext4 is simpler.");
    }

    /// E.NOTICE1 A3: the desktop's answers, as the sender acts on them.
    #[test]
    fn the_desktops_answers_are_read_as_kept_later_or_refused() {
        use serde_json::json;
        assert_eq!(read_answer(&json!({"stored": true, "notice_id": "n"})), Stored::Kept);
        assert_eq!(read_answer(&json!({"stored": true, "duplicate": true})), Stored::Kept);
        assert_eq!(read_answer(&json!({"stored": false, "retry_after_secs": 3600, "why": "20 an hour"})), Stored::Later(Some(3600)));
        assert_eq!(read_answer(&json!({"stored": false, "refused": "unknown family"})), Stored::Refused("unknown family".into()));
        assert_eq!(read_answer(&json!({"error": "huh"})), Stored::Later(None));
    }

    /// E.NOTICE1 A3: kept is delivered once; refused is never sent again while a fresh notice still
    /// goes; "not now" ends the pass and leaves the notice queued; one queued over a day ago is not sent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_sender_acts_on_each_answer() {
        let (conv, mem) = engine();
        let now = 10_000 * HOUR;
        mem.add_task("Stretch", "medium", Some(now - HOUR)).await.unwrap();
        mem.add_task("Water the plants", "medium", Some(now - 2 * HOUR)).await.unwrap();
        conv.queue_due_reminders_at(now).await.unwrap();
        let mut refused = std::collections::HashSet::new();

        // One refused, one put off: the pass ends at the "not now".
        let desk = Scripted { answers: Mutex::new(vec![Stored::Refused("no".into()), Stored::Later(Some(600))]), got: Mutex::new(Vec::new()) };
        let p = send_obligations(&conv, &desk, now, &mut refused).await.unwrap();
        assert_eq!(p, Pass { delivered: 0, retry: 1, refused: 1, wait_secs: Some(600) });
        let first = desk.got.lock().unwrap()[0]["notice_id"].as_str().unwrap().to_string();
        assert!(refused.contains(&first));

        // Later: the refused one is skipped, the other is kept and closed.
        let p = send_obligations(&conv, &desk, now + NOTICE_LEASE_MS, &mut refused).await.unwrap();
        assert_eq!(p, pass(1, 0));
        assert_eq!(desk.got.lock().unwrap().len(), 3);
        assert_ne!(desk.got.lock().unwrap()[2]["notice_id"].as_str().unwrap(), first, "the refused notice was sent again");
        let p = send_obligations(&conv, &desk, now + 2 * NOTICE_LEASE_MS, &mut refused).await.unwrap();
        assert_eq!(p, pass(0, 0), "a kept notice was sent again");

        // A notice queued over a day before the pass is never sent.
        let (conv, mem) = engine();
        mem.add_task("Check the oven", "medium", Some(now - HOUR)).await.unwrap();
        conv.queue_due_reminders_at(now).await.unwrap();
        let desk = Scripted { answers: Mutex::new(Vec::new()), got: Mutex::new(Vec::new()) };
        let late = now + conv_catch_up() + 3_600_000;
        assert_eq!(send_obligations(&conv, &desk, late, &mut Default::default()).await.unwrap(), pass(0, 0));
        assert!(desk.got.lock().unwrap().is_empty(), "a day-old notice was sent");
        assert_eq!(send_obligations(&conv, &desk, now + 1, &mut Default::default()).await.unwrap(), pass(1, 0), "control");
    }

    fn conv_catch_up() -> u64 {
        ConversationEngine::REMINDER_CATCH_UP_MS
    }

    /// E.NOTICE1 A3: where the reminder reaches the person as a card, it is not said again in the
    /// next reply; where it cannot, it is.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reminder_with_a_card_is_not_also_said_in_the_reply() {
        let (conv, mem) = engine();
        let now = crate::telegram::now_ms();
        conv.set_unprompted_delivery(true);
        mem.add_task("Stretch", "medium", Some(now - 60_000)).await.unwrap();
        assert_eq!(conv.queue_due_reminders_at(now).await.unwrap(), 1);
        assert!(conv.take_held_notes().is_empty(), "said twice");
        assert_eq!(conv.lease_obligation_notices(now, NOTICE_LEASE_MS, 10).unwrap().len(), 1, "no card either");
        let (conv, mem) = engine();
        mem.add_task("Stretch", "medium", Some(now - 60_000)).await.unwrap();
        conv.queue_due_reminders_at(now).await.unwrap();
        assert_eq!(conv.take_held_notes().len(), 1, "control: no card, so the reply says it");
    }

    /// E.NOTICE1 A3: the feedback is acked up to the highest seq a poll carried.
    #[test]
    fn feedback_is_acked_to_its_highest_seq() {
        let poll = serde_json::json!({"notice_feedback": [
            {"seq": 7, "notice_id": "notice:a", "kind": "not_now"},
            {"seq": 9, "notice_id": "notice:b", "kind": "useful"},
            {"seq": 8, "notice_id": "notice:c", "kind": "dismissed"}
        ]});
        assert_eq!(feedback_seen(&poll), Some(9));
        assert_eq!(feedback_seen(&serde_json::json!({"turn_id": 3})), None);
        assert_eq!(feedback_seen(&serde_json::json!({"notice_feedback": []})), None);
    }
}
