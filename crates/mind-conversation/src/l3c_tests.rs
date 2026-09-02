//! L3c-1 — the engine's accounting for engagement on a box with no phone: one displayed line
//! earns one claim, idempotent by ref; the knock and the ask commit only when delivered (Telegram)
//! or shown (console); the shown receipt is a durable outbox the reconciler drains without
//! duplicating or moving the clock; the resolver grades a knock only by its reply or its
//! deadline; the console has its own calibration domain, cold-started from the global rate.
use crate::proactive::KnockCandidate;
use crate::*;
use mind_inference::ScriptedLLM;
use mind_memory::MemoryHandle;
use mind_recipes::RecipeStore;
use mind_spec::EngagementMarker;
use yantrik_ml::LLMBackend;

struct NoTools;
#[async_trait::async_trait]
impl RecipeHost for NoTools {
    async fn call_tool(&self, _tool: &str, _args: &serde_json::Value) -> anyhow::Result<String> {
        anyhow::bail!("no tools in this fixture")
    }
}

fn harness() -> (ConversationEngine, Arc<RecipeStore>) {
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(
        Arc::new(ScriptedLLM::new("unused")) as Arc<dyn LLMBackend>,
        1,
    );
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let recipes =
        RecipeEngine::new(pool.clone(), Arc::new(NoTools), "JARVIS").with_store(store.clone());
    let conv = ConversationEngine::new(mem, pool, "JARVIS").with_recipes(Arc::new(recipes));
    (conv, store)
}

async fn ledger_rows(conv: &ConversationEngine, r#ref: &str) -> Vec<serde_json::Value> {
    let led: Vec<serde_json::Value> = conv
        .memory
        .profile_get("judgment_ledger")
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    led.into_iter()
        .filter(|r| r.get("ref").and_then(|x| x.as_str()) == Some(r#ref))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_line_earns_one_claim_and_a_repeat_commit_writes_nothing() {
    let (conv, _store) = harness();
    let now = chrono::Utc::now().timestamp_millis();
    assert!(
        conv.commit_engagement(
            "proactive",
            "digest:0123456789abcdef",
            "console",
            now,
            0.4,
            "recipient engages within 90m"
        )
        .await
    );
    assert!(
        !conv
            .commit_engagement(
                "proactive",
                "digest:0123456789abcdef",
                "console",
                now + 5,
                0.9,
                "recipient engages within 90m"
            )
            .await,
        "same ref: nothing"
    );
    let rows = ledger_rows(&conv, "digest:0123456789abcdef").await;
    assert_eq!(rows.len(), 1, "one claim");
    assert_eq!(rows[0]["domain"].as_str(), Some("engagement-console"));
    // The Telegram beat keeps its legacy ref and domain, byte-compatible.
    let legacy_ref = conv.note_proactive_sent().await;
    let rows = ledger_rows(&conv, &legacy_ref).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["domain"].as_str(), Some("engagement"));
    assert!(conv.spoke_recently(60_000).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_pending_list_reads_legacy_forms_and_the_resolver_grades_each_ref_once() {
    let (conv, _store) = harness();
    let now = chrono::Utc::now().timestamp_millis();
    // Legacy: a bare list of send instants, plus one typed knock entry that is still fresh.
    let stale = now - 91 * 60_000;
    let fresh = now - 5 * 60_000;
    let raw = format!(
        "[{stale}, {{\"sent_ms\":{fresh},\"ref\":\"knock:pkt1\",\"surface\":\"telegram\"}}, {{\"sent_ms\":{fresh},\"ref\":\"digest:0123456789abcdef\",\"surface\":\"console\"}}]"
    );
    conv.memory
        .profile_set("proactive_pending", &raw)
        .await
        .unwrap();
    // A user turn: the legacy beat is stale → ignored; the fresh digest → engaged; the knock is
    // NOT graded by an ordinary turn and stays pending.
    conv.resolve_proactive(true).await;
    let pend = conv
        .memory
        .profile_get("proactive_pending")
        .await
        .unwrap()
        .unwrap_or_default();
    assert!(pend.contains("knock:pkt1"), "{pend}");
    assert!(!pend.contains("digest:"), "{pend}");
    assert!(!pend.contains(&stale.to_string()), "{pend}");
    // The knock's explicit reply grades it and retires it; the resolver then has nothing.
    conv.memory
        .profile_set("knock_pending", "pkt1")
        .await
        .unwrap();
    assert!(conv.knock_reply("later").await.is_some());
    let pend = conv
        .memory
        .profile_get("proactive_pending")
        .await
        .unwrap()
        .unwrap_or_default();
    assert!(!pend.contains("knock:pkt1"), "{pend}");
    // A knock left unanswered past its deadline is graded by the stale pass alone.
    let old_knock = format!(
        "[{{\"sent_ms\":{},\"ref\":\"knock:pkt2\",\"surface\":\"console\"}}]",
        now - 91 * 60_000
    );
    conv.memory
        .profile_set("proactive_pending", &old_knock)
        .await
        .unwrap();
    conv.resolve_proactive(false).await;
    let pend = conv
        .memory
        .profile_get("proactive_pending")
        .await
        .unwrap()
        .unwrap_or_default();
    assert!(pend.is_empty(), "{pend}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ask_arms_its_slot_only_when_committed_and_the_knock_commits_once() {
    let (conv, _store) = harness();
    // Prepare arms nothing; commit arms the slot.
    let candidate = conv
        .prepare_ask()
        .await
        .expect("a fresh mind has a name to ask for");
    assert_eq!(candidate.slot.as_deref(), Some("name"));
    assert!(!conv.has_pending_slot().await, "prepared, not armed");
    conv.commit_ask(&candidate).await;
    assert!(conv.has_pending_slot().await);
    // The knock's commit is one claim, the day's cap, the reply slot and the disposition — once.
    let c = KnockCandidate {
        pkt_id: "pkt9".into(),
        band: 75,
        p: 0.61,
        eval_id: "eval:1".into(),
        trigger: "t".into(),
        title: "x".into(),
        receptive: Some(true),
    };
    let now = chrono::Utc::now().timestamp_millis();
    assert!(conv.commit_knock(&c, now, "console").await);
    assert!(
        !conv.commit_knock(&c, now + 1, "console").await,
        "same packet: nothing"
    );
    assert_eq!(ledger_rows(&conv, "knock:pkt9").await.len(), 1);
    assert_eq!(
        ledger_rows(&conv, "knock:pkt9").await[0]["domain"].as_str(),
        Some("engagement-console")
    );
    assert_eq!(
        conv.memory
            .profile_get("knock_pending")
            .await
            .unwrap()
            .as_deref(),
        Some("pkt9")
    );
    assert!(conv
        .memory
        .profile_get("knock_last_date")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_shown_receipt_is_an_outbox_the_reconciler_drains_once_without_moving_the_clock() {
    let (conv, _store) = harness();
    let marker = EngagementMarker::digest_line("0123456789abcdef", 400).unwrap();
    let now = ConversationEngine::now_ms();
    let q = conv
        .queue_engaging_notice(
            mind_observability::DeliveryKind::Digest,
            "digest line",
            &marker,
            now + 600_000,
        )
        .unwrap();
    assert!(q.fresh);
    // Queued commits nothing.
    assert!(ledger_rows(&conv, "digest:0123456789abcdef")
        .await
        .is_empty());
    assert_eq!(conv.reconcile_shown_engagements().await, 0);
    let leased = conv.lease_notices(60_000, 5).unwrap();
    assert_eq!(leased.len(), 1);
    // Leased commits nothing either.
    assert!(ledger_rows(&conv, "digest:0123456789abcdef")
        .await
        .is_empty());
    // Shown: the durable receipt exists; the engine commit is a separate step (here: skipped,
    // as a crash between the two would leave it).
    let ack = conv
        .ack_notice_shown(&q.notice_id, &leased[0].lease_id)
        .unwrap();
    assert!(ack.shown_now);
    assert_eq!(ack.marker.as_ref(), Some(&marker));
    // The reconciler finishes it exactly once, at the SHOWN instant.
    assert_eq!(conv.reconcile_shown_engagements().await, 1);
    assert_eq!(conv.reconcile_shown_engagements().await, 0);
    let rows = ledger_rows(&conv, "digest:0123456789abcdef").await;
    assert_eq!(rows.len(), 1);
    let due = rows[0]["grade_due"].as_i64().unwrap_or(0);
    assert_eq!(
        due,
        i64::try_from(ack.shown_ms).unwrap() + 90 * 60_000,
        "{rows:?}"
    );
    // A repeated acknowledgement under the same lease returns the original instant and commits nothing new.
    let again = conv
        .ack_notice_shown(&q.notice_id, &leased[0].lease_id)
        .unwrap();
    assert!(!again.shown_now);
    assert_eq!(again.shown_ms, ack.shown_ms);
    assert!(
        !conv
            .commit_shown_engagement(&q.notice_id, &marker, again.shown_ms)
            .await
    );
    assert_eq!(ledger_rows(&conv, "digest:0123456789abcdef").await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shown_ask_arms_its_slot_and_the_console_domain_cold_starts_from_the_global_rate() {
    let (conv, _store) = harness();
    let marker = EngagementMarker::ask("name-0011aabb", 400).unwrap();
    let now = ConversationEngine::now_ms();
    let q = conv
        .queue_engaging_notice(
            mind_observability::DeliveryKind::Ask,
            "what should I call you?",
            &marker,
            now + 600_000,
        )
        .unwrap();
    assert!(!conv.has_pending_slot().await, "queued does not arm");
    let leased = conv.lease_notices(60_000, 5).unwrap();
    assert!(!conv.has_pending_slot().await, "leased does not arm");
    let ack = conv
        .ack_notice_shown(&q.notice_id, &leased[0].lease_id)
        .unwrap();
    assert!(
        conv.commit_shown_engagement(&q.notice_id, &marker, ack.shown_ms)
            .await
    );
    assert!(conv.has_pending_slot().await, "shown arms");
    // The console domain has no grades: the probability is the global estimate, clamped.
    let p = conv.console_engagement_p().await;
    assert!((0.05..=0.95).contains(&p));
    // An engaging kind through the plain door is refused.
    assert!(conv
        .queue_notice(mind_observability::DeliveryKind::Knock, "knock")
        .is_err());
}

/// Codex's concurrency amend: the commit is serialised against itself and against the resolver.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_concurrent_commits_of_one_ref_write_one_row_and_a_resolver_cannot_erase_a_commit() {
    let (conv, _store) = harness();
    let conv = Arc::new(conv);
    let now = chrono::Utc::now().timestamp_millis();
    // commit-vs-commit, many rounds: exactly one of each pair wins and exactly one row exists.
    for i in 0..16u32 {
        let r#ref = format!("digest:{:016x}", i);
        let (a, b) = {
            let c1 = conv.clone();
            let c2 = conv.clone();
            let r1 = r#ref.clone();
            let r2 = r#ref.clone();
            tokio::join!(
                tokio::spawn(async move {
                    c1.commit_engagement(
                        "proactive",
                        &r1,
                        "console",
                        now,
                        0.4,
                        "recipient engages within 90m",
                    )
                    .await
                }),
                tokio::spawn(async move {
                    c2.commit_engagement(
                        "proactive",
                        &r2,
                        "console",
                        now,
                        0.4,
                        "recipient engages within 90m",
                    )
                    .await
                })
            )
        };
        let (a, b) = (a.unwrap(), b.unwrap());
        assert!(a ^ b, "exactly one commit wins for {ref}");
        assert_eq!(ledger_rows(&conv, &r#ref).await.len(), 1, "{ref}");
    }
    // commit-vs-resolve: a stale legacy beat is being resolved while a new ref commits; the new
    // ref must survive in the pending list and later grade exactly once.
    let stale = now - 91 * 60_000;
    conv.memory
        .profile_set("proactive_pending", &format!("[{stale}]"))
        .await
        .unwrap();
    let new_ref = "digest:feedfeedfeedfeed".to_string();
    {
        let c1 = conv.clone();
        let c2 = conv.clone();
        let r = new_ref.clone();
        let (x, y) = tokio::join!(
            tokio::spawn(async move { c1.resolve_proactive(true).await }),
            tokio::spawn(async move {
                c2.commit_engagement(
                    "proactive",
                    &r,
                    "console",
                    now,
                    0.4,
                    "recipient engages within 90m",
                )
                .await
            })
        );
        x.unwrap();
        assert!(y.unwrap());
    }
    let pend = conv
        .memory
        .profile_get("proactive_pending")
        .await
        .unwrap()
        .unwrap_or_default();
    // THE ORDER IS THE SCHEDULER'S, AND BOTH ORDERS ARE CORRECT.
    //
    // This asserted only one of them and failed about one run in four. Both tasks take the same
    // engagement lock, so they are serialised, but which wins is not ours to choose. Resolve first:
    // the stale beat grades, the pending list empties, the commit lands, and `new_ref` is pending.
    // Commit first: `new_ref` is pending when the resolver runs, and a resolver invoked BY A USER
    // TURN answers every beat whose window still contains it — a beat committed microseconds
    // earlier is inside its window, so it grades immediately and correctly. The property the code
    // actually guarantees is that the fresh commit is never LOST: it is either still pending, or
    // already graded exactly once. That is what this now asserts.
    //
    // Diagnosed by review after the flake was recorded rather than re-run until green.
    let claim = ledger_rows(&conv, &new_ref).await;
    assert_eq!(
        claim.len(),
        1,
        "the commit wrote exactly one claim row whatever the order: {claim:?}"
    );
    // The claim row exists from the moment of commit with a null outcome; GRADED means that outcome
    // is filled in. Conflating the two is how the first attempt at this fix failed.
    let graded = claim[0]["outcome"].as_i64().is_some();
    assert!(
        pend.contains(&new_ref) || graded,
        "the fresh commit was lost: not pending and not graded (pending {pend}, row {claim:?})"
    );
    if graded {
        assert_eq!(
            claim[0]["outcome"].as_i64(),
            Some(1),
            "a beat graded inside its own window is a hit: {claim:?}"
        );
    }
    assert!(
        !pend.contains(&stale.to_string()),
        "the stale beat was graded: {pend}"
    );
    // The next person turn grades the new ref if it is still pending, and a further pass has
    // nothing to do. Either way the ref is graded EXACTLY ONCE across the whole test: that is the
    // property, and it holds under both orderings above.
    conv.resolve_proactive(true).await;
    conv.resolve_proactive(true).await;
    let after_first = ledger_rows(&conv, &new_ref).await;
    assert_eq!(after_first.len(), 1, "one claim row: {after_first:?}");
    assert_eq!(
        after_first[0]["outcome"].as_i64(),
        Some(1),
        "{after_first:?}"
    );
    // GRADED ONCE is about the grading, not the row count: a grade mutates the row in place, so a
    // second grading would leave the count at one and pass unseen. Review caught that conflation
    // here — the same one this test's own history is about — so the observable is the grading
    // timestamp, which a further pass must not move.
    let graded_at = after_first[0]["outcome_at"].clone();
    assert!(
        !graded_at.is_null(),
        "a graded row carries when: {after_first:?}"
    );

    // RE-ARM BEFORE RE-RESOLVING, or this proves nothing. Review caught the first version
    // asserting immutability through a pass that returns at the door: `resolve_proactive` exits
    // immediately on an empty pending list, and the list is empty by now — so `outcome_at` could
    // not have moved for ANY reason, including a missing immutability guard. Putting the ref back
    // makes the resolver reach the grading path with a row that is already answered, which is the
    // case the guard exists for.
    let pending_before_probe = conv
        .memory
        .profile_get("proactive_pending")
        .await
        .unwrap()
        .unwrap_or_default();
    // The REAL entry shape (`PendingSend`), not a bare string: a string does not deserialise into
    // an entry, so the resolver skipped it and the "re-grade" never happened — the fix for a check
    // that cannot fail was itself a check that cannot fail, one layer down. Caught by deleting the
    // immutability guard and watching this test stay green.
    conv.memory
        .profile_set(
            "proactive_pending",
            &format!(
                "[{{\"sent_ms\":{now},\"ref\":\"{new_ref}\",\"surface\":\"console\"}}]"
            ),
        )
        .await
        .unwrap();
    // A gap wide enough that a re-grade would stamp a DIFFERENT millisecond. Without it this test
    // passed with the immutability guard deleted, because the two gradings landed in the same
    // millisecond and the timestamps compared equal — a check that cannot fail, one layer below the
    // one review had just found.
    tokio::time::sleep(std::time::Duration::from_millis(8)).await;
    conv.resolve_proactive(true).await;
    let after_second = ledger_rows(&conv, &new_ref).await;
    assert_eq!(after_second.len(), 1, "still one row: {after_second:?}");
    assert_eq!(
        after_second[0]["outcome_at"], graded_at,
        "a second pass re-graded a beat that was already answered: {after_second:?}"
    );
    // The probe put the ref back deliberately; an already-answered ref is not removed again, so
    // restore the state the rest of this test is about. (That an answered ref left in the pending
    // list stays there is real behaviour, not a leak this test invented — it is simply not what
    // this test is measuring.)
    conv.memory
        .profile_set("proactive_pending", &pending_before_probe)
        .await
        .unwrap();
    let pend = conv
        .memory
        .profile_get("proactive_pending")
        .await
        .unwrap()
        .unwrap_or_default();
    assert!(pend.is_empty(), "{pend}");
    conv.resolve_proactive(true).await;
    assert_eq!(ledger_rows(&conv, &new_ref).await.len(), 1);
}

/// Codex's outbox amend: every crash point converges to one pending claim, one judgment row and
/// one durable completion; a completed item never replays; the second reconcile pass is empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_crash_point_converges_to_one_claim_one_row_one_completion() {
    let (conv, store) = harness();
    let marker = EngagementMarker::digest_line("00000000000000aa", 400).unwrap();
    let now = ConversationEngine::now_ms();
    let q = conv
        .queue_engaging_notice(
            mind_observability::DeliveryKind::Digest,
            "line a",
            &marker,
            now + 600_000,
        )
        .unwrap();
    let leased = conv.lease_notices(60_000, 5).unwrap();
    let ack = conv
        .ack_notice_shown(&q.notice_id, &leased[0].lease_id)
        .unwrap();
    // Crash point 1: the pending entry was written, the judgment row was not.
    conv.memory
        .profile_set(
            "proactive_pending",
            &format!(
                "[{{\"sent_ms\":{},\"ref\":\"digest:00000000000000aa\",\"surface\":\"console\"}}]",
                ack.shown_ms
            ),
        )
        .await
        .unwrap();
    assert!(
        conv.commit_shown_engagement(&q.notice_id, &marker, ack.shown_ms)
            .await,
        "the row is written on retry"
    );
    assert_eq!(ledger_rows(&conv, "digest:00000000000000aa").await.len(), 1);
    let pend = conv
        .memory
        .profile_get("proactive_pending")
        .await
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        pend.matches("00000000000000aa").count(),
        1,
        "one pending entry: {pend}"
    );
    assert!(
        store.shown_engagements("primary").unwrap().is_empty(),
        "completed: not in the outbox"
    );
    // Crash point 2: the row exists but the completion receipt was never written.
    let marker_b = EngagementMarker::digest_line("00000000000000bb", 400).unwrap();
    let qb = conv
        .queue_engaging_notice(
            mind_observability::DeliveryKind::Digest,
            "line b",
            &marker_b,
            now + 600_000,
        )
        .unwrap();
    let leased = conv.lease_notices(60_000, 5).unwrap();
    let ack_b = conv
        .ack_notice_shown(&qb.notice_id, &leased[0].lease_id)
        .unwrap();
    conv.commit_engagement(
        "proactive",
        "digest:00000000000000bb",
        "console",
        ack_b.shown_ms as i64,
        0.4,
        "recipient engages within 90m",
    )
    .await;
    assert_eq!(
        store.shown_engagements("primary").unwrap().len(),
        1,
        "still in the outbox"
    );
    assert_eq!(conv.reconcile_shown_engagements().await, 0, "no second row");
    assert_eq!(ledger_rows(&conv, "digest:00000000000000bb").await.len(), 1);
    assert!(
        store.shown_engagements("primary").unwrap().is_empty(),
        "completion written by the reconciler"
    );
    assert_eq!(
        conv.reconcile_shown_engagements().await,
        0,
        "second pass is empty"
    );
    // Crash point 3: the row exists AND was graded, the pending entry is gone — nothing is re-added.
    conv.memory
        .profile_set("proactive_pending", "")
        .await
        .unwrap();
    conv.resolve_proactive(true).await;
    let marker_c = EngagementMarker::digest_line("00000000000000cc", 400).unwrap();
    let qc = conv
        .queue_engaging_notice(
            mind_observability::DeliveryKind::Digest,
            "line c",
            &marker_c,
            now + 600_000,
        )
        .unwrap();
    let leased = conv.lease_notices(60_000, 5).unwrap();
    let ack_c = conv
        .ack_notice_shown(&qc.notice_id, &leased[0].lease_id)
        .unwrap();
    assert!(
        conv.commit_shown_engagement(&qc.notice_id, &marker_c, ack_c.shown_ms)
            .await
    );
    conv.resolve_proactive(true).await; // graded engaged, pending emptied
    assert!(
        !conv
            .commit_shown_engagement(&qc.notice_id, &marker_c, ack_c.shown_ms)
            .await
    );
    let pend = conv
        .memory
        .profile_get("proactive_pending")
        .await
        .unwrap()
        .unwrap_or_default();
    assert!(
        !pend.contains("00000000000000cc"),
        "a graded claim is not re-armed: {pend}"
    );
    assert_eq!(ledger_rows(&conv, "digest:00000000000000cc").await.len(), 1);
    // A committed item stays committed: the acknowledgement is still idempotent afterwards.
    let again = conv
        .ack_notice_shown(&qc.notice_id, &leased[0].lease_id)
        .unwrap();
    assert!(!again.shown_now);
    assert_eq!(again.shown_ms, ack_c.shown_ms);
}

/// Codex's boundary: a future or extreme send stamp is graded by nobody and never counts as
/// having spoken.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn future_and_extreme_send_stamps_stay_pending_and_never_count_as_spoken() {
    let (conv, _store) = harness();
    let now = chrono::Utc::now().timestamp_millis();
    let raw = format!(
        "[{{\"sent_ms\":{},\"ref\":\"digest:0000000000000001\",\"surface\":\"console\"}},{{\"sent_ms\":{},\"ref\":\"digest:0000000000000002\",\"surface\":\"console\"}},{{\"sent_ms\":{},\"ref\":\"digest:0000000000000003\",\"surface\":\"console\"}},{{\"sent_ms\":-5,\"ref\":\"digest:0000000000000004\",\"surface\":\"console\"}}]",
        now + 3_600_000,
        i64::MIN,
        i64::MAX
    );
    conv.memory
        .profile_set("proactive_pending", &raw)
        .await
        .unwrap();
    assert!(
        !conv.spoke_recently(i64::MAX).await,
        "a future stamp is not a send"
    );
    conv.resolve_proactive(true).await;
    conv.resolve_proactive(false).await;
    let pend = conv
        .memory
        .profile_get("proactive_pending")
        .await
        .unwrap()
        .unwrap_or_default();
    for r in [
        "0000000000000001",
        "0000000000000002",
        "0000000000000003",
        "0000000000000004",
    ] {
        assert!(pend.contains(r), "{r} stays pending: {pend}");
    }
}

/// Codex's L3c-2 amend (1): the knock's and the ask's side effects converge past the row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn knock_and_ask_side_effects_converge_after_a_crash_past_the_row_and_never_re_arm_a_graded_claim(
) {
    let (conv, store) = harness();
    // A knock whose judgment row exists (written before a crash) but whose cap, slot and
    // disposition were never written.
    let now = chrono::Utc::now().timestamp_millis();
    conv.commit_engagement(
        "knock",
        "knock:pkt7",
        "console",
        now,
        0.61,
        "recipient engages with the 75% knock within 90m",
    )
    .await;
    assert!(conv
        .memory
        .profile_get("knock_pending")
        .await
        .unwrap()
        .is_none());
    let c = KnockCandidate {
        pkt_id: "pkt7".into(),
        band: 75,
        p: 0.61,
        eval_id: "eval:7".into(),
        trigger: String::new(),
        title: String::new(),
        receptive: None,
    };
    assert!(
        !conv.commit_knock(&c, now, "console").await,
        "the row already existed"
    );
    assert_eq!(
        conv.memory
            .profile_get("knock_pending")
            .await
            .unwrap()
            .as_deref(),
        Some("pkt7")
    );
    assert!(conv
        .memory
        .profile_get("knock_last_date")
        .await
        .unwrap()
        .is_some());
    assert_eq!(funnel_sent(&conv).await, 1, "the funnel stage bumped once");
    assert_eq!(ledger_rows(&conv, "knock:pkt7").await.len(), 1);
    // A second pass changes nothing.
    let date = conv.memory.profile_get("knock_last_date").await.unwrap();
    assert!(!conv.commit_knock(&c, now + 5, "console").await);
    assert_eq!(
        conv.memory.profile_get("knock_last_date").await.unwrap(),
        date
    );
    // Once the claim is graded (the reply), a late reconcile never re-arms the knock.
    assert!(conv.knock_reply("later").await.is_some());
    assert_eq!(
        conv.memory
            .profile_get("knock_pending")
            .await
            .unwrap()
            .as_deref(),
        Some("")
    );
    assert!(!conv.commit_knock(&c, now + 6, "console").await);
    assert_eq!(
        conv.memory
            .profile_get("knock_pending")
            .await
            .unwrap()
            .as_deref(),
        Some("")
    );
    // The ask: shown outbox with a preexisting ungraded row arms the slot once; graded → never.
    let marker = EngagementMarker::ask("purpose-0011aabb", 400).unwrap();
    let q = conv
        .queue_engaging_notice(
            mind_observability::DeliveryKind::Ask,
            "what would you like help with?",
            &marker,
            ConversationEngine::now_ms() + 600_000,
        )
        .unwrap();
    let leased = conv.lease_notices(60_000, 5).unwrap();
    let ack = conv
        .ack_notice_shown(&q.notice_id, &leased[0].lease_id)
        .unwrap();
    conv.commit_engagement(
        "proactive",
        &marker.r#ref,
        "console",
        ack.shown_ms as i64,
        0.4,
        "recipient engages within 90m",
    )
    .await;
    assert!(
        !conv.has_pending_slot().await,
        "row exists, crash before arming"
    );
    assert_eq!(conv.reconcile_shown_engagements().await, 0, "no second row");
    assert!(conv.has_pending_slot().await, "the slot converged");
    assert!(
        store.shown_engagements("primary").unwrap().is_empty(),
        "completed"
    );
    conv.resolve_proactive(true).await; // graded
    conv.set_pending_slot(None).await;
    assert!(
        !conv
            .commit_shown_engagement(&q.notice_id, &marker, ack.shown_ms)
            .await
    );
    assert!(
        !conv.has_pending_slot().await,
        "a graded ask is never re-armed"
    );
}

/// Codex's L3c-2 amend (3): refs are opportunity-unique with bounded canonical shapes.
#[test]
fn digest_and_ask_refs_are_unique_per_opportunity_and_still_canonical() {
    let a = mind_conversation_digest_ref("the same line", 1);
    let b = mind_conversation_digest_ref("the same line", 2);
    assert_ne!(a, b);
    assert_eq!(
        mind_conversation_digest_ref("the same line", 1),
        a,
        "deterministic"
    );
    assert!(EngagementMarker::digest_line(a.strip_prefix("digest:").unwrap(), 300).is_some());
    let open1 = crate::ask_ref_for(None, 10);
    let open2 = crate::ask_ref_for(None, 11);
    assert_ne!(open1, open2);
    assert!(open1.starts_with("ask:open-"));
    assert!(EngagementMarker::ask(open1.strip_prefix("ask:").unwrap(), 300).is_some());
    let slotted = crate::ask_ref_for(Some("interest:companies"), 10);
    assert!(EngagementMarker::ask(slotted.strip_prefix("ask:").unwrap(), 300).is_some());
}

fn mind_conversation_digest_ref(text: &str, window: u64) -> String {
    crate::digest_ref_for(text, window)
}

async fn funnel_sent(conv: &ConversationEngine) -> u64 {
    let counters: serde_json::Value = conv
        .memory
        .profile_get("funnel_counters")
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::json!({}));
    counters
        .as_object()
        .map(|days| {
            days.values()
                .filter_map(|d| d.get("knock:sent").and_then(|n| n.as_u64()))
                .sum()
        })
        .unwrap_or(0)
}

/// Codex's L3c-2 addendum (B): the knock's side effects are once by ref across two refs and
/// across the crash boundary between the durable flag and the bump.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn knock_side_effects_are_once_by_ref_across_two_refs_and_the_flag_bump_boundary() {
    let (conv, _store) = harness();
    let now = chrono::Utc::now().timestamp_millis();
    let c1 = KnockCandidate {
        pkt_id: "pkt1".into(),
        band: 75,
        p: 0.6,
        eval_id: "e1".into(),
        trigger: String::new(),
        title: String::new(),
        receptive: None,
    };
    let c2 = KnockCandidate {
        pkt_id: "pkt2".into(),
        band: 60,
        p: 0.5,
        eval_id: "e2".into(),
        trigger: String::new(),
        title: String::new(),
        receptive: None,
    };
    assert!(conv.commit_knock(&c1, now, "console").await);
    assert!(conv.commit_knock(&c2, now + 1, "console").await);
    assert_eq!(funnel_sent(&conv).await, 2);
    // Retries in any order — an older outbox reconciled after a newer knock — bump nothing.
    assert!(!conv.commit_knock(&c1, now + 2, "console").await);
    assert!(!conv.commit_knock(&c2, now + 3, "console").await);
    assert!(!conv.commit_knock(&c1, now + 4, "console").await);
    assert_eq!(funnel_sent(&conv).await, 2);
    assert_eq!(ledger_rows(&conv, "knock:pkt1").await.len(), 1);
    assert_eq!(ledger_rows(&conv, "knock:pkt2").await.len(), 1);
    // The crash boundary: the flag was written, the bump was not — a retry bumps nothing more
    // (at most once, never twice).
    let c3 = KnockCandidate {
        pkt_id: "pkt3".into(),
        band: 90,
        p: 0.9,
        eval_id: "e3".into(),
        trigger: String::new(),
        title: String::new(),
        receptive: None,
    };
    conv.commit_engagement(
        "knock",
        "knock:pkt3",
        "console",
        now,
        0.9,
        "recipient engages with the 90% knock within 90m",
    )
    .await;
    // Simulate: flag set before the crash.
    let mut led: Vec<serde_json::Value> = serde_json::from_str(
        &conv
            .memory
            .profile_get("judgment_ledger")
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    for r in led.iter_mut() {
        if r["ref"].as_str() == Some("knock:pkt3") {
            r["funnel_sent"] = serde_json::json!(true);
        }
    }
    conv.memory
        .profile_set("judgment_ledger", &serde_json::to_string(&led).unwrap())
        .await
        .unwrap();
    assert!(!conv.commit_knock(&c3, now + 5, "console").await);
    assert_eq!(
        funnel_sent(&conv).await,
        2,
        "the flagged ref is not bumped again"
    );
    assert_eq!(
        conv.memory
            .profile_get("knock_pending")
            .await
            .unwrap()
            .as_deref(),
        Some("pkt3"),
        "the other artifacts still converge"
    );
}
