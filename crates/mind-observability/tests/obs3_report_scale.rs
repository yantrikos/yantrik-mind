//! E.OBS3 — an operator report must stay readable at any row count.
//!
//! `ym why attention` shipped emitting 2,240 lines and 74 KB against a real box, because every
//! test written for it fed three rows. The defect was visible at review time — it emitted one
//! block PER ROW where every sibling report aggregates PER CATEGORY — and neither its own tests
//! nor the full suite could see it, because none of them supplied enough data for the difference
//! to appear.
//!
//! This is the guard that would have caught it. It is deliberately narrow about what it claims:
//! a report is only counted as GUARDED if this file can feed it a corpus it actually reads.
//! Feeding a generic corpus to all twenty-three renderers would make most answer "no data yet"
//! and pass trivially — twenty-three reports apparently guarded, four actually guarded. The
//! NOT_COVERED list below is that honesty made explicit.

use mind_observability::*;

/// Reports this file feeds real data and asserts a bound on.
const GUARDED: &[&str] = &[
    "render_attention_shadow",
    "render_world_shadow",
    "render_calibration",
    "render_lane_coverage",
    "render_evaluator_coverage",
];

/// Per-row BY DESIGN: it renders one trace's own events and is bounded by that trace, not by the
/// log. Exempt deliberately, and stated here rather than left out silently.
const EXEMPT: &[&str] = &["render_trace"];

/// Reports this file cannot feed a meaningful corpus without inventing their event shapes. Listed
/// so the gap is auditable: an unguarded report named here is a known gap, an unguarded report
/// missing from every list is a test that has quietly stopped covering the surface.
const NOT_COVERED: &[&str] = &[
    "render_latency_coverage",
    "render_semantic_coverage",
    "render_context_coverage",
    "render_goal_id_coverage",
    "render_tool_version_coverage",
    "render_model_route_coverage",
    "render_model_call_resources",
    "render_tool_chain_completeness",
    "render_packet_chain_completeness",
    "render_forecast_chain_completeness",
    "render_goal_contribution",
    "render_pack_evidence",
    "render_pack_routes",
    "render_policy_flips",
    "render_delivery_ledger",
    "render_loop_ledger",
    "render_spend_ledger",
    // Found by this guard on its FIRST run: I enumerated the surface by eye from a truncated
    // listing and missed it. That is precisely the rot the completeness wall exists to stop.
    "render_spend_ledger_1h",
];

/// Every operator report must fit in a terminal scrollback a person will actually read.
const MAX_LINES: usize = 200;
const ROWS: u64 = 400;

fn shadow(n: u64) -> DecisionEvent {
    let cycle = CycleId::new(100, n);
    let mut e = DecisionEvent::new(&format!("attn-{}", cycle.render()), "attention_shadow");
    e.ts_ms = 1_000 + n;
    e.object_id = Some(cycle.render());
    e.candidates = vec!["lease-sweep".into()];
    e.chosen = Some("lease-sweep".into());
    e.verdict = Some("ranked".into());
    e.lane = Some("shadow".into());
    e.actor = Some("attention".into());
    e.evaluator_id = Some("attention-policy-v1".into());
    e
}

fn world(n: u64) -> DecisionEvent {
    let mut e = DecisionEvent::new(&format!("world-shadow-{n}"), "world_shadow");
    e.ts_ms = 1_000 + n;
    e.goal_id = Some("worldshadow:headless-cadence".into());
    e.outcome = Some("unknown".into());
    e.verdict = Some("shadowed".into());
    e.lane = Some("primary".into());
    e.actor = Some("proactive".into());
    e.evaluator_id = Some("world-state-v1.1".into());
    e
}

fn predicted(n: u64) -> DecisionEvent {
    let mut e = DecisionEvent::new("t", "tool_predicted");
    e.ts_ms = 1_000 + n;
    e.event_id = Some(format!("p{n}"));
    e.confidence = Some(0.8);
    e.lane = Some("primary".into());
    e.actor = Some("conversation".into());
    e.evaluator_id = Some("tool-outcome-v1".into());
    e
}

fn observed(n: u64) -> DecisionEvent {
    let mut e = DecisionEvent::new("t", "tool_observed");
    e.ts_ms = 1_000 + n;
    e.parent_event_id = Some(format!("p{n}"));
    e.verdict = Some(if n % 3 == 0 { "empty" } else { "ok" }.into());
    e.semantic_success = Some(n % 3 != 0);
    e.lane = Some("primary".into());
    e.actor = Some("conversation".into());
    e.evaluator_id = Some("tool-outcome-v1".into());
    e
}

fn corpus() -> Vec<DecisionEvent> {
    let mut v = Vec::new();
    for n in 1..=ROWS {
        v.push(shadow(n));
        v.push(world(n));
        v.push(predicted(n));
        v.push(observed(n));
    }
    v
}

fn check(name: &str, out: String, events: usize) {
    let lines = out.lines().count();
    assert!(
        lines <= MAX_LINES,
        "{name} rendered {lines} lines from {events} events (cap {MAX_LINES}). An operator report \
         must aggregate by CATEGORY, not emit a block per row."
    );
    assert!(
        !out.trim().is_empty(),
        "{name} rendered nothing from {events} events — this guard would be vacuous for it"
    );
}

#[test]
fn every_guarded_report_stays_bounded_at_scale() {
    let evs = corpus();
    let n = evs.len();
    check("render_attention_shadow", render_attention_shadow(&evs), n);
    check("render_world_shadow", render_world_shadow(&evs), n);
    check("render_calibration", render_calibration(&evs), n);
    check("render_lane_coverage", render_lane_coverage(&evs), n);
    check("render_evaluator_coverage", render_evaluator_coverage(&evs), n);
}

/// KILL 3: a report added to the crate and to none of the three lists FAILS here.
///
/// Without this, the guard rots the moment somebody adds a renderer — which is the same shape as
/// the check nobody ran, the shadow nobody read and the judge nobody built. The lists are the
/// claim; this test is what keeps the claim true.
#[test]
fn every_report_in_the_crate_is_guarded_exempt_or_named_as_a_gap() {
    const SRC: &str = include_str!("../src/lib.rs");
    let mut found: Vec<String> = Vec::new();
    for line in SRC.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("pub fn render_") {
            if let Some(name) = rest.split('(').next() {
                if line.contains("(events: &[DecisionEvent]) -> String") {
                    found.push(format!("render_{name}"));
                }
            }
        }
    }
    assert!(
        found.len() >= 20,
        "the scan found only {} report(s); it has stopped matching the surface",
        found.len()
    );
    let mut unlisted: Vec<&String> = found
        .iter()
        .filter(|f| {
            !GUARDED.contains(&f.as_str())
                && !EXEMPT.contains(&f.as_str())
                && !NOT_COVERED.contains(&f.as_str())
        })
        .collect();
    unlisted.sort();
    assert!(
        unlisted.is_empty(),
        "report(s) in the crate appear in none of GUARDED / EXEMPT / NOT_COVERED: {unlisted:?}. \
         Add real coverage, or name the gap — but do not let the guard silently stop covering \
         the surface it claims to cover."
    );
}
