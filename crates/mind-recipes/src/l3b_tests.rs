//! L3b — the console notice queue's store contract: queue + receipt in one transaction; dedupe;
//! a lease excludes shown and live-leased rows and comes back after expiry; the acknowledgement
//! needs the live lease and is idempotent afterwards; one `shown` per notice is the store's own
//! rule; history reads verified chains only.
use super::*;
use mind_spec::{NoticeEvent, NoticeKind, NoticeReceipt, NOTICE_MAX_CHARS};

fn chain(store: &RecipeStore, id: &str) -> Vec<NoticeEvent> {
    store
        .notice_history("primary", 50)
        .unwrap()
        .into_iter()
        .find(|e| e.notice.notice_id == id)
        .map(|e| e.receipts.iter().map(|r| r.event).collect())
        .unwrap_or_default()
}

#[test]
fn queueing_writes_the_notice_and_its_queued_receipt_together_and_dedupes() {
    let store = RecipeStore::open(":memory:").unwrap();
    let raw = format!(
        "💡 a pattern\u{7} line {}",
        "x".repeat(NOTICE_MAX_CHARS * 2)
    );
    let q = store
        .queue_notice(
            "primary",
            NoticeKind::Pattern,
            &raw,
            "pattern:2026-09-02",
            1_000,
        )
        .unwrap();
    assert!(q.fresh, "first queue lands");
    assert_eq!(q.kind, NoticeKind::Pattern);
    assert_eq!(q.chars, NOTICE_MAX_CHARS);
    assert_eq!(chain(&store, &q.notice_id), vec![NoticeEvent::Queued]);
    // The same dedupe key lands nothing — no row, no receipt — and names the existing notice.
    let again = store
        .queue_notice(
            "primary",
            NoticeKind::Pattern,
            "different text",
            "pattern:2026-09-02",
            1_001,
        )
        .unwrap();
    assert!(!again.fresh);
    assert_eq!(again.notice_id, q.notice_id);
    assert_eq!(store.notice_history("primary", 50).unwrap().len(), 1);
    // Empty after bounding is refused outright.
    assert!(store
        .queue_notice(
            "primary",
            NoticeKind::Verdict,
            " \t\u{1} ",
            "verdict:empty",
            1_002
        )
        .is_err());
    assert_eq!(store.notice_queue_depth("primary", 1_003).unwrap(), (1, 0));
}

#[test]
fn a_lease_skips_live_leases_and_shown_rows_and_returns_after_expiry() {
    let store = RecipeStore::open(":memory:").unwrap();
    let a = store
        .queue_notice("primary", NoticeKind::Verdict, "verdict one", "v:1", 1_000)
        .unwrap();
    let b = store
        .queue_notice("primary", NoticeKind::Verdict, "verdict two", "v:2", 1_001)
        .unwrap();
    let _other = store
        .queue_notice(
            "someone-else",
            NoticeKind::Verdict,
            "not yours",
            "v:3",
            1_002,
        )
        .unwrap();
    let first = store.lease_notices("primary", 2_000, 60_000, 10).unwrap();
    assert_eq!(
        first
            .iter()
            .map(|l| l.notice_id.as_str())
            .collect::<Vec<_>>(),
        vec![a.notice_id.as_str(), b.notice_id.as_str()],
        "created order, this operator only"
    );
    assert_eq!(first[0].text, "verdict one");
    assert_eq!(first[0].lease_until_ms, 62_000);
    // Within the lease window nothing comes back; the depth shows both leased.
    assert!(store
        .lease_notices("primary", 3_000, 60_000, 10)
        .unwrap()
        .is_empty());
    assert_eq!(store.notice_queue_depth("primary", 3_000).unwrap(), (2, 2));
    // After the lease expires, the unacknowledged rows come back under NEW lease ids.
    let again = store.lease_notices("primary", 70_000, 60_000, 10).unwrap();
    assert_eq!(again.len(), 2);
    assert_ne!(again[0].lease_id, first[0].lease_id);
    assert_eq!(
        chain(&store, &a.notice_id),
        vec![
            NoticeEvent::Queued,
            NoticeEvent::Leased,
            NoticeEvent::Leased
        ]
    );
    // Acknowledge one; it never comes back, the other still does.
    assert!(store
        .ack_notice_shown(&a.notice_id, &again[0].lease_id, 71_000)
        .unwrap());
    let rest = store.lease_notices("primary", 200_000, 60_000, 10).unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].notice_id, b.notice_id);
    assert_eq!(
        store.notice_queue_depth("primary", 200_001).unwrap(),
        (1, 1)
    );
    // A limit is honoured.
    assert_eq!(
        store
            .lease_notices("primary", 400_000, 60_000, 0)
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn the_acknowledgement_needs_the_live_lease_and_is_idempotent_after() {
    let store = RecipeStore::open(":memory:").unwrap();
    let n = store
        .queue_notice(
            "primary",
            NoticeKind::HorizonTick,
            "⌛ goal expired",
            "h:1",
            1_000,
        )
        .unwrap();
    // Not leased yet: refused.
    assert!(store
        .ack_notice_shown(&n.notice_id, "0123456789abcdef", 1_500)
        .is_err());
    let lease = store
        .lease_notices("primary", 2_000, 10_000, 5)
        .unwrap()
        .remove(0);
    // Wrong lease: refused. Expired lease: refused.
    assert!(store
        .ack_notice_shown(&n.notice_id, "0123456789abcdef", 2_500)
        .is_err());
    assert!(store
        .ack_notice_shown(&n.notice_id, &lease.lease_id, 12_001)
        .is_err());
    // Live lease: shown once.
    assert!(store
        .ack_notice_shown(&n.notice_id, &lease.lease_id, 3_000)
        .unwrap());
    // Again with the same lease: already shown, idempotent false; a foreign lease is still Err.
    assert!(!store
        .ack_notice_shown(&n.notice_id, &lease.lease_id, 3_001)
        .unwrap());
    assert!(store
        .ack_notice_shown(&n.notice_id, "0123456789abcdef", 3_002)
        .is_err());
    assert_eq!(
        chain(&store, &n.notice_id),
        vec![NoticeEvent::Queued, NoticeEvent::Leased, NoticeEvent::Shown]
    );
    // Unknown notice: refused.
    assert!(store
        .ack_notice_shown("notice:nope", &lease.lease_id, 3_003)
        .is_err());
    // The store's own rule: a second shown row cannot exist even by a raw write.
    assert!(store.duplicate_shown_row_for_test(&n.notice_id).is_err());
}

#[test]
fn history_reads_verified_chains_only_and_newest_first() {
    let store = RecipeStore::open(":memory:").unwrap();
    let a = store
        .queue_notice(
            "primary",
            NoticeKind::ProfileRefresh,
            "🧭 refreshed",
            "p:1",
            1_000,
        )
        .unwrap();
    let b = store
        .queue_notice("primary", NoticeKind::Pattern, "💡 found", "p:2", 2_000)
        .unwrap();
    let history = store.notice_history("primary", 10).unwrap();
    assert_eq!(history[0].notice.notice_id, b.notice_id);
    assert_eq!(history[1].notice.notice_id, a.notice_id);
    assert!(history
        .iter()
        .all(|e| e.receipts.iter().all(|r| r.verify())));
    // A tampered receipt row makes the read fail closed rather than show an unverified chain.
    store.tamper_notice_receipt_for_test(&a.notice_id).unwrap();
    assert!(store.notice_history("primary", 10).is_err());
    assert!(store.lease_notices("primary", 3_000, 1_000, 10).is_err());
}

fn fresh(store: &RecipeStore, key: &str, at: u64) -> QueuedNotice {
    store
        .queue_notice("primary", NoticeKind::Verdict, "verdict line", key, at)
        .unwrap()
}

#[test]
fn a_notice_id_is_the_full_digest_of_its_row() {
    let store = RecipeStore::open(":memory:").unwrap();
    let n = fresh(&store, "v:1", 1_000);
    let hex = n.notice_id.strip_prefix("notice:").expect("prefix");
    assert_eq!(hex.len(), 64);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
}

/// A forged `shown` row (valid shape and digest, wrong transition: it follows `queued`) must
/// not silently hide the notice — every reader fails closed instead.
#[test]
fn a_forged_shown_row_cannot_suppress_a_notice() {
    let store = RecipeStore::open(":memory:").unwrap();
    let n = fresh(&store, "v:1", 1_000);
    let prev = store.last_receipt_sha_for_test(&n.notice_id).unwrap();
    let forged = NoticeReceipt::issue(
        n.notice_id.clone(),
        "primary",
        NoticeEvent::Shown,
        1_500,
        Some("0123456789abcdef".into()),
        None,
        Some(prev),
    )
    .unwrap();
    assert!(forged.verify(), "the forgery is a well-formed receipt");
    store.insert_receipt_for_test(&forged).unwrap();
    assert!(store.lease_notices("primary", 2_000, 60_000, 10).is_err());
    assert!(store.notice_queue_depth("primary", 2_000).is_err());
    assert!(store.notice_history("primary", 10).is_err());
}

/// The chain binds the row: a mutated text under a valid chain is an error on every read
/// that could hand it to a renderer, including the dedupe path.
#[test]
fn a_mutated_text_row_is_never_rendered() {
    let store = RecipeStore::open(":memory:").unwrap();
    let n = fresh(&store, "v:1", 1_000);
    store.mutate_notice_text_for_test(&n.notice_id).unwrap();
    assert!(store.lease_notices("primary", 2_000, 60_000, 10).is_err());
    assert!(store.notice_history("primary", 10).is_err());
    assert!(store
        .queue_notice("primary", NoticeKind::Verdict, "verdict line", "v:1", 3_000)
        .is_err());
    assert!(store.notice_queue_depth("primary", 2_000).is_err());
}

/// Transition semantics: a re-lease before the prior lease expires, a `shown` under a
/// different lease, and a receipt whose time moves backward are corruption, not history.
#[test]
fn the_chain_refuses_early_releases_foreign_shown_and_backward_time() {
    // Early re-lease.
    let store = RecipeStore::open(":memory:").unwrap();
    let n = fresh(&store, "v:1", 1_000);
    let lease = store
        .lease_notices("primary", 2_000, 60_000, 10)
        .unwrap()
        .remove(0);
    let prev = store.last_receipt_sha_for_test(&n.notice_id).unwrap();
    let early = NoticeReceipt::issue(
        n.notice_id.clone(),
        "primary",
        NoticeEvent::Leased,
        30_000,
        Some("fedcba9876543210".into()),
        Some(90_000),
        Some(prev.clone()),
    )
    .unwrap();
    store.insert_receipt_for_test(&early).unwrap();
    assert!(store.notice_history("primary", 10).is_err());

    // Shown under a foreign lease.
    let store = RecipeStore::open(":memory:").unwrap();
    let n = fresh(&store, "v:1", 1_000);
    let _lease = store
        .lease_notices("primary", 2_000, 60_000, 10)
        .unwrap()
        .remove(0);
    let prev = store.last_receipt_sha_for_test(&n.notice_id).unwrap();
    let foreign = NoticeReceipt::issue(
        n.notice_id.clone(),
        "primary",
        NoticeEvent::Shown,
        3_000,
        Some("fedcba9876543210".into()),
        None,
        Some(prev),
    )
    .unwrap();
    store.insert_receipt_for_test(&foreign).unwrap();
    assert!(store.notice_history("primary", 10).is_err());
    assert!(store.lease_notices("primary", 70_000, 60_000, 10).is_err());

    // Shown after the lease expired (time consistent, lease dead).
    let store = RecipeStore::open(":memory:").unwrap();
    let n = fresh(&store, "v:1", 1_000);
    let lease2 = store
        .lease_notices("primary", 2_000, 10_000, 10)
        .unwrap()
        .remove(0);
    let prev = store.last_receipt_sha_for_test(&n.notice_id).unwrap();
    let late = NoticeReceipt::issue(
        n.notice_id.clone(),
        "primary",
        NoticeEvent::Shown,
        12_001,
        Some(lease2.lease_id.clone()),
        None,
        Some(prev),
    )
    .unwrap();
    store.insert_receipt_for_test(&late).unwrap();
    assert!(store.notice_history("primary", 10).is_err());

    // Backward time.
    let store = RecipeStore::open(":memory:").unwrap();
    let n = fresh(&store, "v:1", 5_000);
    let prev = store.last_receipt_sha_for_test(&n.notice_id).unwrap();
    let backward = NoticeReceipt::issue(
        n.notice_id.clone(),
        "primary",
        NoticeEvent::Leased,
        4_000,
        Some("0123456789abcdef".into()),
        Some(64_000),
        Some(prev),
    )
    .unwrap();
    store.insert_receipt_for_test(&backward).unwrap();
    assert!(store.notice_history("primary", 10).is_err());
    let _ = lease;
}
