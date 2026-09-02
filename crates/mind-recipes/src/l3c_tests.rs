//! L3c — engaging notices: a canonical marker and a show-by bound bound into the identity; one
//! outstanding knock per operator per day over verified chains; expiry is terminal and written
//! by the sweep and by the lease path; the acknowledgement is a durable outbox record; pre-marker
//! rows keep validating under the L3b formula.
use super::*;
use mind_spec::{EngagementMarker, NoticeEvent, NoticeKind};

fn knock_marker(pkt: &str) -> EngagementMarker {
    EngagementMarker::knock(pkt, 612, 75, "eval:0001").unwrap()
}

fn events(store: &RecipeStore, id: &str) -> Vec<NoticeEvent> {
    store
        .notice_history("primary", 50)
        .unwrap()
        .into_iter()
        .find(|e| e.notice.notice_id == id)
        .map(|e| e.receipts.iter().map(|r| r.event).collect())
        .unwrap_or_default()
}

#[test]
fn an_engaging_notice_binds_its_marker_and_bound_and_refuses_wrong_shapes() {
    let store = RecipeStore::open(":memory:").unwrap();
    let m = knock_marker("pkt:a1");
    let q = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Knock,
            "you'll want to see this",
            "k:1",
            &m,
            61_000,
            1_000,
        )
        .unwrap();
    assert!(q.fresh);
    assert_eq!(q.marker.as_ref(), Some(&m));
    assert_eq!(q.show_by_ms, Some(61_000));
    assert!(q
        .notice_id
        .strip_prefix("notice:")
        .is_some_and(|h| h.len() == 64));
    // A plain kind cannot carry a marker; a marker of another kind is refused; a bound at now is refused.
    assert!(store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Pattern,
            "x",
            "p:1",
            &m,
            61_000,
            1_000
        )
        .is_err());
    let ask = EngagementMarker::ask("name", 300).unwrap();
    assert!(store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Knock,
            "x",
            "k:2",
            &ask,
            61_000,
            1_000
        )
        .is_err());
    assert!(store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Ask,
            "what should I call you?",
            "a:1",
            &ask,
            1_000,
            1_000
        )
        .is_err());
    // The dedupe key names the existing notice, marker and all, writing nothing.
    let again = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Knock,
            "different",
            "k:1",
            &m,
            99_000,
            2_000,
        )
        .unwrap();
    assert!(!again.fresh);
    assert_eq!(again.notice_id, q.notice_id);
    assert_eq!(again.show_by_ms, Some(61_000));
    // Tampering with the bound or the marker column breaks the identity: every reader fails closed.
    store.shift_show_by_for_test(&q.notice_id).unwrap();
    assert!(store.lease_notices("primary", 2_000, 10_000, 5).is_err());
    assert!(store.notice_history("primary", 5).is_err());
    assert!(store.shown_engagements("primary").is_err());
}

#[test]
fn at_most_one_outstanding_knock_per_operator_per_day_over_verified_chains() {
    let store = RecipeStore::open(":memory:").unwrap();
    let day = 1_788_300_000_000u64; // some UTC instant
    let m1 = knock_marker("pkt:a1");
    let first = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Knock,
            "knock one",
            "k:1",
            &m1,
            day + 60_000,
            day,
        )
        .unwrap();
    let m2 = knock_marker("pkt:b2");
    // A second knock the same day while the first is outstanding: refused.
    assert!(store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Knock,
            "knock two",
            "k:2",
            &m2,
            day + 90_000,
            day + 1_000
        )
        .is_err());
    // Another operator is not bound by it.
    assert!(store
        .queue_engaging_notice(
            "other",
            NoticeKind::Knock,
            "knock two",
            "k:2",
            &m2,
            day + 90_000,
            day + 1_000
        )
        .is_ok());
    // Once the first expires (terminal), the day's slot frees.
    assert_eq!(
        store
            .sweep_engaging_expiry("primary", day + 60_001)
            .unwrap(),
        1
    );
    assert_eq!(
        events(&store, &first.notice_id),
        vec![NoticeEvent::Queued, NoticeEvent::Expired]
    );
    assert!(store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Knock,
            "knock two",
            "k:2",
            &m2,
            day + 200_000,
            day + 60_002
        )
        .is_ok());
    // A digest is never bound by the knock rule.
    let d = EngagementMarker::digest_line("0123456789abcdef", 300).unwrap();
    assert!(store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Digest,
            "digest",
            "d:1",
            &d,
            day + 200_000,
            day + 60_003
        )
        .is_ok());
}

#[test]
fn expiry_is_terminal_and_the_lease_path_never_shows_a_late_line() {
    let store = RecipeStore::open(":memory:").unwrap();
    let m = knock_marker("pkt:a1");
    let q = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Knock,
            "knock",
            "k:1",
            &m,
            31_000,
            1_000,
        )
        .unwrap();
    // Within the window it leases; not acknowledged; the lease runs out; the bound passes.
    let lease = store
        .lease_notices("primary", 2_000, 5_000, 5)
        .unwrap()
        .remove(0);
    assert_eq!(lease.notice_id, q.notice_id);
    // A late poll leases nothing and writes the expiry instead.
    assert!(store
        .lease_notices("primary", 40_000, 5_000, 5)
        .unwrap()
        .is_empty());
    assert_eq!(
        events(&store, &q.notice_id),
        vec![
            NoticeEvent::Queued,
            NoticeEvent::Leased,
            NoticeEvent::Expired
        ]
    );
    // The sweep is idempotent; an acknowledgement after expiry is refused; the depth ignores it.
    assert_eq!(store.sweep_engaging_expiry("primary", 41_000).unwrap(), 0);
    assert!(store
        .ack_notice_shown(&q.notice_id, &lease.lease_id, 41_000)
        .is_err());
    assert_eq!(store.notice_queue_depth("primary", 41_000).unwrap(), (0, 0));
    // A live lease at the bound is not expired out from under the cockpit.
    let m2 = knock_marker("pkt:b2");
    let q2 = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Knock,
            "knock two",
            "k:2",
            &m2,
            100_000,
            50_000,
        )
        .unwrap();
    let lease2 = store
        .lease_notices("primary", 99_000, 60_000, 5)
        .unwrap()
        .remove(0);
    assert_eq!(store.sweep_engaging_expiry("primary", 101_000).unwrap(), 0);
    let ack = store
        .ack_notice_shown(&q2.notice_id, &lease2.lease_id, 102_000)
        .unwrap();
    assert!(ack.shown_now);
    assert_eq!(ack.marker.as_ref(), Some(&m2));
    assert_eq!(ack.kind, NoticeKind::Knock);
}

#[test]
fn the_acknowledgement_is_a_durable_outbox_record_and_the_reconciler_reads_it() {
    let store = RecipeStore::open(":memory:").unwrap();
    let d = EngagementMarker::digest_line("0123456789abcdef", 300).unwrap();
    let q = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Digest,
            "digest",
            "d:1",
            &d,
            61_000,
            1_000,
        )
        .unwrap();
    assert!(
        store.shown_engagements("primary").unwrap().is_empty(),
        "queued is not shown"
    );
    let lease = store
        .lease_notices("primary", 2_000, 10_000, 5)
        .unwrap()
        .remove(0);
    let ack = store
        .ack_notice_shown(&q.notice_id, &lease.lease_id, 3_000)
        .unwrap();
    assert!(ack.shown_now);
    assert_eq!(ack.shown_ms, 3_000);
    assert_eq!(ack.marker.as_ref(), Some(&d));
    // The same lease again: the ORIGINAL instant and marker, so a crashed commit can finish.
    let again = store
        .ack_notice_shown(&q.notice_id, &lease.lease_id, 9_000)
        .unwrap();
    assert!(!again.shown_now);
    assert_eq!(again.shown_ms, 3_000);
    assert_eq!(again.marker.as_ref(), Some(&d));
    let shown = store.shown_engagements("primary").unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].notice_id, q.notice_id);
    assert_eq!(shown[0].shown_ms, 3_000);
    assert_eq!(shown[0].marker, d);
}

#[test]
fn pre_marker_rows_keep_validating_and_a_plain_row_cannot_grow_a_marker() {
    let store = RecipeStore::open(":memory:").unwrap();
    let plain = store
        .queue_notice("primary", NoticeKind::Verdict, "verdict", "v:1", 1_000)
        .unwrap();
    assert_eq!(plain.marker, None);
    let lease = store.lease_notices("primary", 2_000, 10_000, 5).unwrap();
    assert_eq!(lease.len(), 1);
    let ack = store
        .ack_notice_shown(&plain.notice_id, &lease[0].lease_id, 3_000)
        .unwrap();
    assert_eq!(ack.marker, None);
    assert!(store.shown_engagements("primary").unwrap().is_empty());
    // A marker smuggled onto a plain row breaks its identity.
    store.smuggle_marker_for_test(&plain.notice_id).unwrap();
    assert!(store.notice_history("primary", 5).is_err());
}

/// Codex's outbox amend: completion is a receipt after `shown`; the outbox returns only the
/// uncompleted; three hundred completed items never come back.
#[test]
fn the_outbox_returns_only_uncompleted_items_and_completed_ones_never_replay() {
    let store = RecipeStore::open(":memory:").unwrap();
    for i in 0..300u32 {
        let hex = format!("{:016x}", i);
        let m = EngagementMarker::digest_line(&hex, 300).unwrap();
        let at = 1_000 + u64::from(i) * 10;
        let q = store
            .queue_engaging_notice(
                "primary",
                NoticeKind::Digest,
                "digest",
                &format!("d:{i}"),
                &m,
                at + 100_000,
                at,
            )
            .unwrap();
        let lease = store
            .lease_notices("primary", at + 1, 50_000, 1)
            .unwrap()
            .remove(0);
        assert_eq!(lease.notice_id, q.notice_id);
        let ack = store
            .ack_notice_shown(&q.notice_id, &lease.lease_id, at + 2)
            .unwrap();
        assert!(ack.shown_now);
        // Completion before shown is impossible; after shown it is written once.
        assert!(store
            .mark_engagement_committed(&q.notice_id, at + 3)
            .unwrap());
        assert!(!store
            .mark_engagement_committed(&q.notice_id, at + 4)
            .unwrap());
        assert_eq!(
            events(&store, &q.notice_id),
            vec![
                NoticeEvent::Queued,
                NoticeEvent::Leased,
                NoticeEvent::Shown,
                NoticeEvent::Committed
            ]
        );
        // The same lease still answers with the original instant after completion.
        let again = store
            .ack_notice_shown(&q.notice_id, &lease.lease_id, at + 5)
            .unwrap();
        assert!(!again.shown_now);
        assert_eq!(again.shown_ms, at + 2);
    }
    assert!(store.shown_engagements("primary").unwrap().is_empty());
    // One more, shown but not completed: the only item in the outbox.
    let m = EngagementMarker::digest_line("ffffffffffffffff", 300).unwrap();
    let q = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Digest,
            "digest",
            "d:last",
            &m,
            9_000_000,
            8_000_000,
        )
        .unwrap();
    let lease = store
        .lease_notices("primary", 8_000_001, 50_000, 1)
        .unwrap()
        .remove(0);
    store
        .ack_notice_shown(&q.notice_id, &lease.lease_id, 8_000_002)
        .unwrap();
    let outbox = store.shown_engagements("primary").unwrap();
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].notice_id, q.notice_id);
    // Completion cannot be written for a line nobody saw.
    let m2 = EngagementMarker::digest_line("eeeeeeeeeeeeeeee", 300).unwrap();
    let unseen = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Digest,
            "digest",
            "d:unseen",
            &m2,
            9_000_000,
            8_000_003,
        )
        .unwrap();
    assert!(store
        .mark_engagement_committed(&unseen.notice_id, 8_000_004)
        .is_err());
    // Nor for a plain notice.
    let plain = store
        .queue_notice("primary", NoticeKind::Verdict, "v", "v:1", 8_000_005)
        .unwrap();
    assert!(store
        .mark_engagement_committed(&plain.notice_id, 8_000_006)
        .is_err());
}

/// Codex's L3c-2 amend (3): dedupe is per opportunity — an outstanding line is one row; an
/// expired one is history and the same key queues a fresh attempt.
#[test]
fn an_expired_engaging_line_can_queue_again_while_an_outstanding_one_dedupes() {
    let store = RecipeStore::open(":memory:").unwrap();
    let m = EngagementMarker::ask("name-0011aabb", 300).unwrap();
    let first = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Ask,
            "what should I call you?",
            "ask:name-0011aabb",
            &m,
            31_000,
            1_000,
        )
        .unwrap();
    assert!(first.fresh);
    // Outstanding: the same key names the same row and writes nothing.
    let same = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Ask,
            "what should I call you?",
            "ask:name-0011aabb",
            &m,
            40_000,
            2_000,
        )
        .unwrap();
    assert!(!same.fresh);
    assert_eq!(same.notice_id, first.notice_id);
    // Expired: history. The same key now inserts a fresh attempt with its own identity.
    assert_eq!(store.sweep_engaging_expiry("primary", 31_001).unwrap(), 1);
    let again = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Ask,
            "what should I call you?",
            "ask:name-0011aabb",
            &m,
            90_000,
            32_000,
        )
        .unwrap();
    assert!(again.fresh);
    assert_ne!(again.notice_id, first.notice_id);
    assert_eq!(store.notice_history("primary", 10).unwrap().len(), 2);
    // A shown-and-completed line is history too.
    let lease = store
        .lease_notices("primary", 33_000, 10_000, 5)
        .unwrap()
        .remove(0);
    store
        .ack_notice_shown(&again.notice_id, &lease.lease_id, 34_000)
        .unwrap();
    store
        .mark_engagement_committed(&again.notice_id, 34_001)
        .unwrap();
    let third = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Ask,
            "what should I call you?",
            "ask:name-0011aabb",
            &m,
            200_000,
            100_000,
        )
        .unwrap();
    assert!(third.fresh);
    assert_eq!(store.notice_history("primary", 10).unwrap().len(), 3);
}

/// Codex's L3c-2 addendum (C): `_` in a key is a character, not a wildcard — attempts of one
/// key never capture another's rows.
#[test]
fn dedupe_attempt_matching_is_exact_and_underscore_is_not_a_wildcard() {
    let store = RecipeStore::open(":memory:").unwrap();
    let m1 = EngagementMarker::ask("a_b-00000000", 300).unwrap();
    let m2 = EngagementMarker::ask("axb-00000000", 300).unwrap();
    let first = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Ask,
            "q1",
            "ask:a_b-00000000",
            &m1,
            31_000,
            1_000,
        )
        .unwrap();
    assert_eq!(store.sweep_engaging_expiry("primary", 31_001).unwrap(), 1);
    // A key that a LIKE wildcard would confuse with the first: its own row, attempt zero.
    let other = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Ask,
            "q2",
            "ask:axb-00000000",
            &m2,
            90_000,
            32_000,
        )
        .unwrap();
    assert!(other.fresh);
    assert_ne!(other.notice_id, first.notice_id);
    // The first key again: a fresh attempt of ITS OWN lineage, not the other's outstanding row.
    let again = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Ask,
            "q1",
            "ask:a_b-00000000",
            &m1,
            90_000,
            33_000,
        )
        .unwrap();
    assert!(again.fresh);
    assert_ne!(again.notice_id, other.notice_id);
    // And the other key still dedupes to its outstanding row.
    let same = store
        .queue_engaging_notice(
            "primary",
            NoticeKind::Ask,
            "q2",
            "ask:axb-00000000",
            &m2,
            90_000,
            34_000,
        )
        .unwrap();
    assert!(!same.fresh);
    assert_eq!(same.notice_id, other.notice_id);
}
