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
