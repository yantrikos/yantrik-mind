//! Spike C (Phase 0): is the shared identity chain feasible as designed?
//!
//! Confirms the two load-bearing facts for the Phase-7 SyncDaemon: (1) `OplogEntry` survives a
//! cross-PROCESS boundary (serialize to JSON on one node, deserialize + apply on another), and
//! (2) `apply_ops` is idempotent (re-applying the same ops never duplicates). The only missing
//! piece for the central chain is then the network transport + server endpoints — not the data
//! model, which this proves works.

use yantrikdb_core::replication::{apply_ops, extract_ops_since, OplogEntry};
use yantrikdb_core::YantrikDB;

fn memory_count(db: &YantrikDB) -> i64 {
    db.conn()
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get::<_, i64>(0))
        .unwrap()
}

fn record(db: &YantrikDB, text: &str) -> String {
    let meta = serde_json::json!({});
    let zero = vec![0.0f32; 8];
    db.record(
        text, "episodic", 0.5, 0.0, 604_800.0, &meta, &zero, "default", 0.8, "general", "user",
        None,
    )
    .unwrap()
}

#[test]
fn oplog_serializes_cross_process_and_apply_is_idempotent() {
    let a = YantrikDB::new_with_actor(":memory:", 8, "actorA").unwrap();
    let b = YantrikDB::new_with_actor(":memory:", 8, "actorB").unwrap();

    record(&a, "alpha fact");
    record(&a, "beta fact");

    // Extract A's oplog. (0.9.0's `conn()` returns a MutexGuard; bind + reborrow to `&Connection`.)
    let aconn = a.conn();
    let ops = extract_ops_since(&aconn, None, None, None, 10_000).unwrap();
    assert!(!ops.is_empty(), "expected oplog entries from A");

    // Cross-PROCESS boundary: serialize to JSON, ship, deserialize back.
    let wire = serde_json::to_string(&ops).expect("OplogEntry must serialize");
    let ops2: Vec<OplogEntry> = serde_json::from_str(&wire).expect("OplogEntry must deserialize");
    assert_eq!(ops.len(), ops2.len());

    // Apply to B (a different node/actor).
    assert_eq!(memory_count(&b), 0);
    apply_ops(&b, &ops2).unwrap();
    let after_first = memory_count(&b);
    assert!(
        after_first >= 2,
        "B should hold A's two memories after apply, got {after_first}"
    );

    // Idempotent: re-applying the same ops must not duplicate.
    apply_ops(&b, &ops2).unwrap();
    assert_eq!(
        memory_count(&b),
        after_first,
        "re-apply must be idempotent (no duplication)"
    );
}
