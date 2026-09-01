//! E.AGI-A5: the completeness gate can name its window. The all-time aggregate is untouched;
//! the since-start figure is the SAME aggregate over the events of this binary only.

use crate::ConversationEngine;
use mind_observability::{tool_chain_completeness, DecisionEvent};

fn ev(kind: &str, ts_ms: u64, trace: &str) -> DecisionEvent {
    let mut e = DecisionEvent::new(trace, kind);
    e.ts_ms = ts_ms;
    // A call is a prediction joined by its observation: the pair shares the prediction's id.
    if kind == "tool_predicted" {
        e.event_id = Some(format!("pred-{trace}"));
    } else {
        e.parent_event_id = Some(format!("pred-{trace}"));
    }
    e
}

#[test]
fn the_window_excludes_everything_before_its_start_and_changes_nothing_else() {
    // Two old, unstamped prediction/observation pairs (pre-stamping stratigraphy), then one
    // fresh pair after "start". The all-time report sees all three; the window sees one.
    let mut events = Vec::new();
    for (i, t) in [(1u64, 1_000u64), (2, 2_000)] {
        events.push(ev("tool_predicted", t, &format!("old-{i}")));
        events.push(ev("tool_observed", t + 1, &format!("old-{i}")));
    }
    let start = 10_000;
    events.push(ev("tool_predicted", start + 5, "fresh"));
    events.push(ev("tool_observed", start + 6, "fresh"));

    let all = tool_chain_completeness(&events);
    let windowed = ConversationEngine::completeness_since(&events, start);
    assert_eq!(all.total, 3, "all-time counts every call");
    assert_eq!(
        windowed.total, 1,
        "the window counts only calls since start"
    );
    // The window's oldest timestamp can never precede the start.
    if let Some(w) = &windowed.window {
        assert!(
            w.oldest_ts_ms >= start,
            "oldest {} < start {}",
            w.oldest_ts_ms,
            start
        );
    }
    // Identical input, identical all-time number: the window is additive, never a rewrite.
    let again = tool_chain_completeness(&events);
    assert_eq!(again.total, all.total);
    assert_eq!(again.complete, all.complete);
    // An empty window is an honest zero, not an error.
    let none = ConversationEngine::completeness_since(&events, start + 1_000_000);
    assert_eq!(none.total, 0);
}

#[test]
fn the_process_start_is_fixed_for_the_life_of_the_process() {
    let a = crate::process_started_ms();
    std::thread::sleep(std::time::Duration::from_millis(5));
    let b = crate::process_started_ms();
    assert_eq!(a, b);
    assert!(a > 1_700_000_000_000, "a real epoch millisecond");
}

#[test]
fn the_auditor_window_argument_is_start_or_an_epoch_millisecond() {
    let start = 1_788_000_000_000;
    assert_eq!(crate::parse_since_arg("start", start), Some(start));
    assert_eq!(crate::parse_since_arg("since=start", start), Some(start));
    assert_eq!(
        crate::parse_since_arg("since=1788300439881", start),
        Some(1_788_300_439_881)
    );
    assert_eq!(
        crate::parse_since_arg("1788300439881", start),
        Some(1_788_300_439_881)
    );
    for bad in [
        "",
        "since=",
        "since=yesterday",
        "since=-5",
        "since=0",
        "since=12e3",
        "start; drop",
    ] {
        assert_eq!(crate::parse_since_arg(bad, start), None, "{bad:?}");
    }
    assert_eq!(
        crate::window_label(1_788_300_439_881),
        "since 2026-09-01 22:07:19Z"
    );
}

/// Dispatch-level: the auditor's block is ADDITIVE. Every key of the plain report is byte-identical
/// with and without `since=`, the aggregate's own `window` (its timestamp span) survives, and an
/// unreadable argument adds nothing. This is the fixture the live probe asked for after the first
/// cut named the block `window` and clobbered the span.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_auditor_block_is_additive_and_never_clobbers_the_all_time_report() {
    use mind_inference::{InferencePool, ScriptedLLM};
    use mind_memory::MemoryHandle;
    use std::sync::Arc;
    use yantrik_ml::LLMBackend;
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let conv = ConversationEngine::new(
        Arc::new(mem) as Arc<dyn crate::MemoryFacade>,
        InferencePool::new(Arc::new(ScriptedLLM::new("x")) as Arc<dyn LLMBackend>, 1),
        "JARVIS",
    );
    let dir = std::env::temp_dir().join(format!("ym-a5-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = Arc::new(mind_observability::DecisionLog::open(dir.join("d.jsonl")));
    let conv = conv.with_recorder(log);
    // Two linked calls, so the all-time report has a real span.
    for trace in ["one", "two"] {
        conv.recorder().record(ev("tool_predicted", 1_000, trace));
        conv.recorder().record(ev("tool_observed", 1_001, trace));
    }
    let ctx = mind_types::AccessContext::operator_audit();
    let plain: serde_json::Value =
        serde_json::from_str(&conv.cli_dispatch("chains_json", &ctx).await).unwrap();
    let explicit: serde_json::Value =
        serde_json::from_str(&conv.cli_dispatch("chains_json since=1", &ctx).await).unwrap();
    let bad: serde_json::Value =
        serde_json::from_str(&conv.cli_dispatch("chains_json since=yesterday", &ctx).await)
            .unwrap();
    assert_eq!(plain["available"], serde_json::json!(true));
    assert!(
        plain.get("since_start").is_some(),
        "since-start is unconditional"
    );
    assert!(
        plain.get("auditor_window").is_none(),
        "no auditor block without an argument"
    );
    assert!(
        bad.get("auditor_window").is_none(),
        "an unreadable argument adds nothing"
    );
    let aud = explicit.get("auditor_window").expect("the auditor block");
    assert_eq!(aud["since_ms"], serde_json::json!(1));
    assert!(
        aud["label"].as_str().unwrap().starts_with("since "),
        "the block is named"
    );
    // Every key the plain report has is byte-identical in the explicit one — including the
    // aggregate's own `window` span, which the first cut overwrote.
    for (k, v) in plain.as_object().unwrap() {
        assert_eq!(&explicit[k], v, "key {k} must not change under since=");
    }
    let _ = std::fs::remove_dir_all(dir);
}
