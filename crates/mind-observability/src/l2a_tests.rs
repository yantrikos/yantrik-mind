//! L2-A — the attention policy's arithmetic, pinned before any evidence exists.
//!
//! These tests are the reason the shadow row can be trusted later: the score is a frozen integer
//! function of frozen constants, it agrees with the f64 scoring path the rest of the system uses,
//! and its ranking is total. Nothing here calls a loop, reads a clock, or writes anything.

use crate::{
    attention_constants, attention_rank, attention_score, idle_urgency, window_urgency,
    AttentionCandidate, AttentionConstants, LoopId, ATTENTION_POLICY, ATTENTION_SCOPE,
};
use mind_types::ScoreAxes;

fn axes_of(c: &AttentionConstants, urgency: u64) -> ScoreAxes {
    ScoreAxes {
        urgency: urgency as f64 / 1000.0,
        confidence: c.confidence as f64 / 1000.0,
        expected_value: c.expected_value as f64 / 1000.0,
        annoyance_risk: c.annoyance_risk as f64 / 1000.0,
        acceptance_rate: c.acceptance_rate as f64 / 1000.0,
        ..Default::default()
    }
}

#[test]
fn every_scoped_loop_has_constants_and_nothing_else_does() {
    assert_eq!(
        ATTENTION_SCOPE.len(),
        17,
        "the frozen scope is seventeen loops"
    );
    for id in ATTENTION_SCOPE {
        assert!(
            attention_constants(id).is_some(),
            "{id:?} is in scope but has no constants"
        );
    }
    // A loop outside the scope must not be scorable by accident: L1d added nineteen more ids, and a
    // shadow that silently ranked one of them would be measuring something nobody preregistered.
    for id in [
        LoopId::Heartbeat,
        LoopId::WorldShadow,
        LoopId::Briefing,
        LoopId::Dream,
        LoopId::Forge,
        LoopId::NewsDigest,
    ] {
        assert!(
            attention_constants(id).is_none(),
            "{id:?} is out of scope but has constants"
        );
    }
    let mut seen = ATTENTION_SCOPE.to_vec();
    seen.sort_by_key(|l| format!("{l:?}"));
    seen.dedup();
    assert_eq!(seen.len(), 17, "the scope list repeats a loop");
}

#[test]
fn the_score_is_the_preregistered_arithmetic_exactly() {
    // Hand-computed from the frozen table: Knock is 900/700/400/600.
    // num = 900 × 700 × (1000+0) × (2000−400) × (1000+600) = 630000 × 1000 × 1600 × 1600
    //     = 1_612_800_000_000_000; score = num / 4e12 = 403.
    let knock = attention_constants(LoopId::Knock).expect("knock is in scope");
    assert_eq!(
        knock,
        AttentionConstants {
            expected_value: 900,
            confidence: 700,
            annoyance_risk: 400,
            acceptance_rate: 600
        }
    );
    assert_eq!(attention_score(&knock, 0), 403);
    // At full urgency the first factor doubles: 806.
    assert_eq!(attention_score(&knock, 1000), 806);
    // Resolve is the low-value, zero-annoyance shape: 300/900/0/1000.
    // num = 300 × 900 × 1000 × 2000 × 2000 = 1_080_000_000_000_000 → 270.
    let resolve = attention_constants(LoopId::Resolve).expect("resolve is in scope");
    assert_eq!(attention_score(&resolve, 0), 270);
}

#[test]
fn the_bound_holds_and_the_saturation_is_unreachable() {
    // The co-prereg first stated this maximum as 8×10^18; the correction row records 8×10^15. The
    // code must agree with the correction, and the claim "saturation can never fire" must be a fact
    // rather than a comment.
    let max = AttentionConstants {
        expected_value: 1000,
        confidence: 1000,
        annoyance_risk: 0,
        acceptance_rate: 1000,
    };
    let num: u64 = 1000u64 * 1000 * 2000 * 2000 * 2000;
    assert_eq!(num, 8_000_000_000_000_000, "the true bound is 8e15");
    assert!(num < u64::MAX, "the product fits in u64 with room to spare");
    assert_eq!(
        attention_score(&max, 1000),
        2000,
        "the maximum score is 2000"
    );
    // and the floor
    let zero = AttentionConstants {
        expected_value: 0,
        confidence: 1000,
        annoyance_risk: 0,
        acceptance_rate: 1000,
    };
    assert_eq!(attention_score(&zero, 1000), 0);
}

#[test]
fn out_of_range_inputs_are_clamped_not_wrapped() {
    // Nothing should ever hand these values in, which is exactly why the behaviour must be defined:
    // a wrapped multiply here would put a nonsense score into an append-only ledger.
    let absurd = AttentionConstants {
        expected_value: u64::MAX,
        confidence: 5000,
        annoyance_risk: 9000,
        acceptance_rate: u64::MAX,
    };
    let clamped = AttentionConstants {
        expected_value: 1000,
        confidence: 1000,
        annoyance_risk: 1000,
        acceptance_rate: 1000,
    };
    assert_eq!(
        attention_score(&absurd, u64::MAX),
        attention_score(&clamped, 1000)
    );
    assert!(attention_score(&absurd, u64::MAX) <= 2000);
}

#[test]
fn the_integer_score_agrees_with_the_float_scoring_path() {
    // The equivalence the co-prereg requires: on the full per-mille axis grid in steps of 100, plus
    // the seventeen named loop constants at every urgency step, |score/1000 − priority| < 0.001.
    // The first version tested only the named constants while claiming the full grid in its comment.
    let mut worst = 0.0f64;
    let mut check = |c: AttentionConstants, urg: u64, case: &str| {
        let integer = attention_score(&c, urg) as f64 / 1000.0;
        let float = axes_of(&c, urg).priority();
        let delta = (integer - float).abs();
        assert!(
            delta < 0.001,
            "{case} at urgency {urg}: integer {integer} vs float {float}"
        );
        worst = worst.max(delta);
    };

    for ev in (0..=1000).step_by(100) {
        for conf in (0..=1000).step_by(100) {
            for ann in (0..=1000).step_by(100) {
                for acc in (0..=1000).step_by(100) {
                    let c = AttentionConstants {
                        expected_value: ev,
                        confidence: conf,
                        annoyance_risk: ann,
                        acceptance_rate: acc,
                    };
                    for urg in (0..=1000).step_by(100) {
                        check(c, urg, "full grid");
                    }
                }
            }
        }
    }

    for id in ATTENTION_SCOPE {
        let c = attention_constants(id).expect("in scope");
        for urg in (0..=1000).step_by(100) {
            check(c, urg, &format!("{id:?}"));
        }
    }
    assert!(worst < 0.001, "worst divergence {worst}");
}

#[test]
fn window_urgency_is_zero_before_due_and_capped_one_period_late() {
    let hour = 3_600_000u64;
    // not yet due
    assert_eq!(window_urgency(1_000_000, 1_000_000, hour), 0);
    assert_eq!(window_urgency(1_000_000 + hour, 1_000_000, hour), 0);
    // half a period late
    assert_eq!(
        window_urgency(1_000_000 + hour + hour / 2, 1_000_000, hour),
        500
    );
    // a full period late, and beyond, both cap
    assert_eq!(window_urgency(1_000_000 + 2 * hour, 1_000_000, hour), 1000);
    assert_eq!(window_urgency(1_000_000 + 40 * hour, 1_000_000, hour), 1000);
    // a clock that went backwards must not produce urgency, and a zero period must not divide by zero
    assert_eq!(window_urgency(500, 1_000_000, hour), 0);
    assert_eq!(window_urgency(u64::MAX, 0, 0), 1000);
}

#[test]
fn idle_urgency_measures_the_stretch_past_the_requirement() {
    let ten_min = 600_000u64;
    assert_eq!(idle_urgency(0, ten_min), 0);
    assert_eq!(idle_urgency(ten_min, ten_min), 0);
    assert_eq!(idle_urgency(ten_min + ten_min / 4, ten_min), 250);
    assert_eq!(idle_urgency(3 * ten_min, ten_min), 1000);
    assert_eq!(
        idle_urgency(5, 0),
        1000,
        "a zero requirement must not divide by zero"
    );
}

#[test]
fn the_ranking_is_total_and_breaks_ties_in_the_frozen_order() {
    let c = |loop_id, score, last_ms| AttentionCandidate {
        loop_id,
        score,
        last_ms,
    };
    // Score first.
    let mut v = vec![c(LoopId::Resolve, 270, 0), c(LoopId::Knock, 403, 0)];
    attention_rank(&mut v);
    assert_eq!(v[0].loop_id, LoopId::Knock);

    // Equal scores fall back to the scope order: Dmn precedes Knock precedes Digest.
    let mut v = vec![
        c(LoopId::Digest, 500, 0),
        c(LoopId::Knock, 500, 0),
        c(LoopId::Dmn, 500, 0),
    ];
    attention_rank(&mut v);
    assert_eq!(
        v.iter().map(|x| x.loop_id).collect::<Vec<_>>(),
        vec![LoopId::Dmn, LoopId::Knock, LoopId::Digest]
    );

    // Same score AND same loop: the one that has waited longer wins.
    let mut v = vec![c(LoopId::Ask, 300, 900), c(LoopId::Ask, 300, 100)];
    attention_rank(&mut v);
    assert_eq!(v[0].last_ms, 100);

    // Determinism: the same input in a different starting order gives the same result.
    let mut a = vec![
        c(LoopId::Ics, 200, 5),
        c(LoopId::Family, 200, 5),
        c(LoopId::Whois, 200, 5),
    ];
    let mut b = vec![
        c(LoopId::Whois, 200, 5),
        c(LoopId::Family, 200, 5),
        c(LoopId::Ics, 200, 5),
    ];
    attention_rank(&mut a);
    attention_rank(&mut b);
    assert_eq!(a, b);
}

#[test]
fn the_floor_excludes_a_candidate_that_scores_nothing() {
    let c = AttentionCandidate {
        loop_id: LoopId::Ask,
        score: 0,
        last_ms: 0,
    };
    assert!(!c.ranked());
    assert!(AttentionCandidate { score: 1, ..c }.ranked());
}

#[test]
fn the_policy_version_is_named_so_a_changed_constant_cannot_pass_as_the_same_policy() {
    assert_eq!(ATTENTION_POLICY, "attention-policy-v1");
}

// ── L2-B: the wake identity, before anything writes one ──────────────────────────────────────

#[test]
fn a_cycle_label_round_trips_and_rejects_everything_else() {
    use crate::{CycleId, LOOP_LEDGER_V6};

    let c = CycleId::new(1_788_000_000_000, 7);
    assert_eq!(c.render(), "cycle:1788000000000:7");
    assert_eq!(CycleId::parse(&c.render()), Some(c));
    assert_eq!(CycleId::parse("cycle:0:0"), Some(CycleId::new(0, 0)));

    // Everything a reader might be handed instead. A v6 row whose label does not parse is
    // MALFORMED: pairing a shadow against a row of unknown wake would be evidence about nothing.
    for bad in [
        "",
        "cycle:",
        "cycle:1",
        "cycle:1:",
        "cycle::1",
        "cycle:1:2:3",
        "cycle:-1:2",
        "cycle:+1:2",
        "cycle: 1:2",
        "cycle:1 :2",
        "cycle:01:2", // one wake, two renderings, is not an identity
        "cycle:1:007",
        "cycle:1.0:2",
        "cycle:abc:2",
        "cycle:1:abc",
        "wake:1:2",
        "1:2",
        "cycle:99999999999999999999:1", // wider than u64
    ] {
        assert_eq!(CycleId::parse(bad), None, "accepted {bad:?}");
    }

    // The version string exists and is distinct from the one v5 rows carry.
    assert_eq!(LOOP_LEDGER_V6, "loop-ledger-v6");
    assert_ne!(LOOP_LEDGER_V6, crate::LOOP_LEDGER_VERSION);
}

#[test]
fn wake_identity_is_ordered_within_a_process_and_unique_across_restarts() {
    use crate::CycleId;
    let boot_a = 1_788_000_000_000u64;
    let boot_b = 1_788_000_005_000u64;
    // Monotone within a process...
    assert!(CycleId::new(boot_a, 1) < CycleId::new(boot_a, 2));
    // ...and a restart never reuses an identity, which is why the process start is in the label:
    // a bare wake number restarts at zero on every boot and would pair two different wakes.
    assert_ne!(CycleId::new(boot_a, 0), CycleId::new(boot_b, 0));
    assert!(CycleId::new(boot_a, 9_999) < CycleId::new(boot_b, 0));
}
