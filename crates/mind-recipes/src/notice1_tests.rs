//! E.NOTICE1 — the outbox's `delivered` receipt: the desktop stored a leased notice. It closes the
//! live lease once, is idempotent under that lease, ends leasing (a delivered notice is never sent
//! again), needs the live lease (an expired one is leased again -- the retry), and is terminal in
//! the chain. The obligation kinds round-trip.
use super::*;
use mind_spec::{NoticeEvent, NoticeKind, NoticeReceipt};

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
fn obligation_kinds_round_trip_and_are_not_engaging() {
    for (k, s) in [(NoticeKind::Reminder, "reminder"), (NoticeKind::Delegated, "delegated")] {
        assert_eq!(k.as_str(), s);
        assert_eq!(NoticeKind::parse(s), Some(k));
        assert!(!k.is_engaging());
    }
    assert_eq!(NoticeEvent::Delivered.as_str(), "delivered");
}

#[test]
fn delivered_closes_the_lease_once_and_ends_leasing() {
    let store = RecipeStore::open(":memory:").unwrap();
    let n = store
        .queue_notice("primary", NoticeKind::Reminder, "Stretch", "reminder:t1:1000", 1_000)
        .unwrap();
    let lease = store.lease_notices("primary", 2_000, 60_000, 10).unwrap().remove(0);
    assert_eq!(lease.notice_id, n.notice_id);
    assert!(store.ack_notice_delivered(&n.notice_id, &lease.lease_id, 3_000).unwrap());
    // The same ack again is idempotent, not a second receipt.
    assert!(!store.ack_notice_delivered(&n.notice_id, &lease.lease_id, 3_500).unwrap());
    assert_eq!(chain(&store, &n.notice_id), vec![NoticeEvent::Queued, NoticeEvent::Leased, NoticeEvent::Delivered]);
    // Never leased again, even long after.
    assert!(store.lease_notices("primary", 10_000_000, 60_000, 10).unwrap().is_empty());
    // Not counted unseen by the console's depth, and the console cannot `shown` it.
    assert!(store.ack_notice_shown(&n.notice_id, &lease.lease_id, 4_000).is_err());
}

#[test]
fn an_expired_lease_is_leased_again_and_only_the_live_lease_delivers() {
    let store = RecipeStore::open(":memory:").unwrap();
    let n = store
        .queue_notice("primary", NoticeKind::Delegated, "Research done", "delegated:job1", 1_000)
        .unwrap();
    let first = store.lease_notices("primary", 2_000, 10_000, 10).unwrap().remove(0);
    // No ack: the lease runs out and the notice comes back -- that is the retry.
    assert!(store.ack_notice_delivered(&n.notice_id, &first.lease_id, 13_000).is_err(), "an expired lease cannot deliver");
    let second = store.lease_notices("primary", 13_000, 10_000, 10).unwrap().remove(0);
    assert_ne!(first.lease_id, second.lease_id);
    assert!(store.ack_notice_delivered(&n.notice_id, &first.lease_id, 14_000).is_err(), "a stale lease cannot deliver");
    assert!(store.ack_notice_delivered(&n.notice_id, &second.lease_id, 14_000).unwrap());
    assert_eq!(
        chain(&store, &n.notice_id),
        vec![NoticeEvent::Queued, NoticeEvent::Leased, NoticeEvent::Leased, NoticeEvent::Delivered]
    );
}

#[test]
fn nothing_follows_delivered_in_a_chain() {
    let store = RecipeStore::open(":memory:").unwrap();
    let n = store
        .queue_notice("primary", NoticeKind::Reminder, "Stretch", "reminder:t1:1000", 1_000)
        .unwrap();
    let lease = store.lease_notices("primary", 2_000, 60_000, 10).unwrap().remove(0);
    store.ack_notice_delivered(&n.notice_id, &lease.lease_id, 3_000).unwrap();
    let prev = store.last_receipt_sha_for_test(&n.notice_id).unwrap();
    let after = NoticeReceipt::issue(
        n.notice_id.clone(),
        "primary",
        NoticeEvent::Leased,
        100_000,
        Some("fedcba9876543210".into()),
        Some(200_000),
        Some(prev),
    )
    .unwrap();
    store.insert_receipt_for_test(&after).unwrap();
    assert!(store.notice_history("primary", 10).is_err(), "a lease after delivered must fail the chain");
}

#[test]
fn a_forged_delivered_under_a_foreign_lease_fails_the_chain() {
    let store = RecipeStore::open(":memory:").unwrap();
    let n = store
        .queue_notice("primary", NoticeKind::Reminder, "Stretch", "reminder:t1:1000", 1_000)
        .unwrap();
    let _lease = store.lease_notices("primary", 2_000, 60_000, 10).unwrap().remove(0);
    let prev = store.last_receipt_sha_for_test(&n.notice_id).unwrap();
    let foreign = NoticeReceipt::issue(
        n.notice_id.clone(),
        "primary",
        NoticeEvent::Delivered,
        3_000,
        Some("fedcba9876543210".into()),
        None,
        Some(prev),
    )
    .unwrap();
    store.insert_receipt_for_test(&foreign).unwrap();
    assert!(store.notice_history("primary", 10).is_err());
}
