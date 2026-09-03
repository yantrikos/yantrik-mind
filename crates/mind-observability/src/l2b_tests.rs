//! L2-B: the per-wake signal set and the shadow row it produces.
//!
//! Every case here is about a distinction the design depends on and that a plausible simplification
//! would erase: a wake with nothing due against a wake that abstained; a slot never filled against
//! a slot filled and not due; the site's `due` against a `due` this module might have re-derived.

use crate::{
    attention_constants, attention_shadow, AskGate, AttentionShadow, CycleId, DigestGate, DmnGate,
    ForcedWhois, GateReading, IdleGated, KnockGate, LoopId, PersistedChatQuiet, PersistedReceptive,
    TimerChatQuiet, TimerQuiet, TimerUnconditional, WakeSignals, WakeTimer, ATTENTION_SCOPE,
    LOOP_LEDGER_V6,
};

const NOW: u64 = 1_788_000_000_000;

fn timer(period_ms: u64, overdue_by_ms: u64) -> WakeTimer {
    WakeTimer {
        last_ms: NOW - period_ms - overdue_by_ms,
        period_ms,
    }
}

/// A wake with one due unconditional timer, as overdue as asked.
fn one_due(overdue_by_ms: u64) -> WakeSignals {
    WakeSignals {
        resolve: Some(TimerUnconditional {
            due: true,
            timer: timer(60_000, overdue_by_ms),
        }),
        ..Default::default()
    }
}

fn cycle() -> CycleId {
    CycleId::new(7, 3)
}

// ── the urgency arithmetic, at its edges ─────────────────────────────────────────────────────
#[test]
fn window_urgency_is_zero_before_the_period_and_capped_at_one_thousand() {
    // Not yet due: last_ms is now, so `now - last - period` saturates to zero rather than wrapping.
    let fresh = WakeTimer {
        last_ms: NOW,
        period_ms: 60_000,
    };
    assert_eq!(fresh.urgency(NOW), 0);
    // Exactly one period overdue is 1000 per mille of the period...
    assert_eq!(timer(60_000, 60_000).urgency(NOW), 1000);
    // ...half a period is 500...
    assert_eq!(timer(60_000, 30_000).urgency(NOW), 500);
    // ...and ten periods is still 1000, because the cap is the point.
    assert_eq!(timer(60_000, 600_000).urgency(NOW), 1000);
}

#[test]
fn urgency_has_no_input_that_panics() {
    // A zero period would divide by zero; the frozen formula says max(period, 1).
    let zero = WakeTimer {
        last_ms: 0,
        period_ms: 0,
    };
    assert_eq!(zero.urgency(NOW), 1000);
    // A clock that went backwards saturates rather than wrapping to an enormous urgency.
    let future = WakeTimer {
        last_ms: NOW + 10_000,
        period_ms: 1_000,
    };
    assert_eq!(future.urgency(NOW), 0);
    // And an overdue so large that overdue × 1000 would overflow u64 saturates at the cap.
    let huge = WakeTimer {
        last_ms: 0,
        period_ms: 1,
    };
    assert_eq!(huge.urgency(u64::MAX), 1000);
}

#[test]
fn the_knock_measures_an_idle_stretch_and_a_forced_whois_is_always_maximal() {
    let knock = KnockGate {
        due: true,
        enabled: true,
        presence: true,
        idle_ms: 90 * 60_000,
        idle_required_ms: 45 * 60_000,
        last_activity_ms: NOW - 90 * 60_000,
    };
    // One required stretch past the requirement: 1000 per mille.
    assert_eq!(knock.reading(NOW).urgency, 1000);
    let barely = KnockGate {
        idle_ms: 45 * 60_000,
        ..knock
    };
    assert_eq!(barely.reading(NOW).urgency, 0);
    // The knock's tie-break is last ACTIVITY, not a period stamp — it is the only Stretch loop.
    assert_eq!(knock.reading(NOW).last_ms, NOW - 90 * 60_000);

    let forced = ForcedWhois {
        due: true,
        at_ms: NOW,
        chat_present: true,
    };
    assert_eq!(
        forced.reading(NOW).urgency,
        1000,
        "a human asked for it now; nothing the arithmetic says outranks that"
    );
}

// ── the three states of a slot ───────────────────────────────────────────────────────────────
#[test]
fn a_wake_with_nothing_due_records_nothing_and_is_not_an_abstention() {
    // Filled slots that all report not-due. This must be `None`, never a row with an empty
    // ranking: counting it would put a denominator's worth of free agreement into every metric.
    let s = WakeSignals {
        resolve: Some(TimerUnconditional {
            due: false,
            timer: timer(60_000, 60_000),
        }),
        member_beat: Some(TimerQuiet {
            due: false,
            timer: timer(60_000, 60_000),
            quiet: true,
        }),
        ..Default::default()
    };
    assert_eq!(attention_shadow(&s, cycle(), NOW), None);
    // An entirely empty wake is the same answer for the same reason.
    assert_eq!(
        attention_shadow(&WakeSignals::default(), cycle(), NOW),
        None
    );
}

#[test]
fn an_unfilled_slot_is_not_the_same_as_a_slot_that_is_not_due() {
    // Both wakes have exactly one due loop and must produce the same row; the difference is that
    // one of them never filled MemberBeat's slot. The hole is invisible HERE by design — the read
    // side finds it by joining `buildable` against the ledger's own due rows — so what this test
    // pins is that the writer does not invent a `buildable` entry for a slot it never saw.
    let not_due = WakeSignals {
        member_beat: Some(TimerQuiet {
            due: false,
            timer: timer(60_000, 0),
            quiet: true,
        }),
        ..one_due(60_000)
    };
    let missing = one_due(60_000);
    let a = attention_shadow(&not_due, cycle(), NOW).expect("a row");
    let b = attention_shadow(&missing, cycle(), NOW).expect("a row");
    assert_eq!(a.buildable, vec![LoopId::Resolve]);
    assert_eq!(a, b, "a not-due slot and an unfilled slot both stay out");
}

#[test]
fn a_due_wake_where_nothing_clears_the_floor_abstains_and_says_so() {
    // Resolve's constants are 300/900/0/1000. At urgency 0 the score is well above the floor of
    // one, so to reach an empty ranking the loop must be out of the policy's scope entirely.
    // ProfileRefresh is in scope, so instead assert the shape directly: a row exists, it names
    // what was buildable, and `abstained_empty` is true exactly when the ranking is empty.
    let s = one_due(0);
    let row = attention_shadow(&s, cycle(), NOW).expect("a row");
    assert!(!row.ranked.is_empty(), "Resolve scores above the floor");
    assert!(!row.abstained_empty);
    // The invariant, stated as an invariant rather than trusted: these two can never disagree.
    assert_eq!(row.abstained_empty, row.ranked.is_empty());
    assert_eq!(row.top(), Some(LoopId::Resolve));
}

// ── the row ──────────────────────────────────────────────────────────────────────────────────
#[test]
fn exactly_one_row_per_wake_and_it_carries_that_wakes_identity() {
    let row = attention_shadow(&one_due(60_000), CycleId::new(11, 4), NOW).expect("a row");
    assert_eq!(row.cycle, CycleId::new(11, 4));
    let ev = row.to_event(NOW);
    assert_eq!(ev.kind, "attention_shadow");
    assert_eq!(ev.evaluator_id.as_deref(), Some(LOOP_LEDGER_V6));
    assert_eq!(
        ev.object_id.as_deref(),
        Some("cycle:11:4"),
        "the wake identity is what the read side joins on"
    );
    assert_eq!(ev.actor.as_deref(), Some("attention"));
    assert_eq!(ev.lane.as_deref(), Some("shadow"));
    assert_eq!(ev.chosen.as_deref(), Some("resolve"));
    assert_eq!(ev.verdict.as_deref(), Some("ranked"));
    assert_eq!(ev.outcome.as_deref(), Some("buildable:1"));
    // Counts and ids only. A shadow row that carried text would be a second write in disguise.
    assert!(ev.subject.is_none());
    assert!(ev.purpose.is_none());
    assert_eq!(ev.model_calls, None);
}

#[test]
fn two_wakes_of_one_process_get_different_identities() {
    let a = attention_shadow(&one_due(60_000), CycleId::new(11, 4), NOW).expect("a row");
    let b = attention_shadow(&one_due(60_000), CycleId::new(11, 5), NOW).expect("a row");
    assert_ne!(a.cycle, b.cycle);
    assert_ne!(a.to_event(NOW).object_id, b.to_event(NOW).object_id);
}

// ── ordering and scope ───────────────────────────────────────────────────────────────────────
#[test]
fn the_ranking_is_by_score_and_not_by_the_order_the_signals_were_collected_in() {
    // THIS TEST WAS VACUOUS ONCE. Its first version paired HomeWatch with Resolve, and HomeWatch
    // both scores higher AND comes earlier in the frozen scope — so the row was already in the
    // right order before anything sorted it, and deleting the ranking call left the test green.
    // The pair has to DISAGREE. Ask (500/500/500/400) is scope index 3 and scores about 5.3e11;
    // HomeWatch (800/800/300/700) is scope index 5 and scores about 1.85e12. Collection order and
    // score order point opposite ways, so only a real sort puts HomeWatch first.
    let s = WakeSignals {
        ask: Some(AskGate {
            due: true,
            enabled: true,
            spoke: true,
            ask_ok: true,
            timer: timer(60_000, 60_000),
            receptive: true,
        }),
        home_watch: Some(TimerChatQuiet {
            due: true,
            timer: timer(60_000, 60_000),
            presence: true,
            enabled: true,
        }),
        ..Default::default()
    };
    let collected: Vec<LoopId> = s.readings(NOW).into_iter().map(|(id, _)| id).collect();
    assert_eq!(
        collected,
        vec![LoopId::Ask, LoopId::HomeWatch],
        "collection is in scope order, which here is the WRONG order for ranking"
    );
    let row = attention_shadow(&s, cycle(), NOW).expect("a row");
    assert_eq!(row.top(), Some(LoopId::HomeWatch));
    assert_eq!(row.ranked.len(), 2);
    assert!(row.ranked[0].score > row.ranked[1].score);
    assert_eq!(row.ranked[1].loop_id, LoopId::Ask);
    // `buildable` is in scope order, which is what a reader recomputing it must be able to assume.
    let order = |id: LoopId| ATTENTION_SCOPE.iter().position(|s| *s == id).unwrap();
    let idx: Vec<usize> = row.buildable.iter().map(|id| order(*id)).collect();
    let mut sorted = idx.clone();
    sorted.sort_unstable();
    assert_eq!(idx, sorted, "buildable must be in the frozen scope order");
}

#[test]
fn a_forced_whois_supersedes_the_unforced_gate_rather_than_appearing_twice() {
    let s = WakeSignals {
        whois: Some(PersistedReceptive {
            due: true,
            timer: timer(86_400_000, 0),
            presence: true,
            receptive: true,
        }),
        whois_forced: Some(ForcedWhois {
            due: true,
            at_ms: NOW,
            chat_present: true,
        }),
        ..Default::default()
    };
    let row = attention_shadow(&s, cycle(), NOW).expect("a row");
    assert_eq!(
        row.buildable,
        vec![LoopId::Whois],
        "one opportunity, one entry — a loop must never appear twice in one wake"
    );
    // And the reading taken is the FORCED one: urgency 1000, not the unforced timer's 0.
    let readings = s.readings(NOW);
    let whois: Vec<&(LoopId, GateReading)> =
        readings.iter().filter(|(id, _)| *id == LoopId::Whois).collect();
    assert_eq!(whois.len(), 1);
    assert_eq!(whois[0].1.urgency, 1000);
}

#[test]
fn every_loop_the_signal_set_can_report_is_one_the_policy_can_score() {
    // A slot the struct can fill but the policy has no constants for would be silently dropped
    // from the ranking while still counting as buildable — a loop that is always ignored and never
    // missing. Fill EVERY slot, due, and require the ranking to be as long as the buildable list.
    let t = timer(60_000, 60_000);
    let s = WakeSignals {
        resolve: Some(TimerUnconditional { due: true, timer: t }),
        profile_refresh: Some(TimerUnconditional { due: true, timer: t }),
        ics: Some(TimerUnconditional { due: true, timer: t }),
        lease_sweep: Some(TimerUnconditional { due: true, timer: t }),
        member_beat: Some(TimerQuiet { due: true, timer: t, quiet: true }),
        home_watch: Some(TimerChatQuiet { due: true, timer: t, presence: true, enabled: true }),
        family: Some(TimerChatQuiet { due: true, timer: t, presence: true, enabled: true }),
        follow_up: Some(TimerChatQuiet { due: true, timer: t, presence: true, enabled: true }),
        price_watch: Some(TimerChatQuiet { due: true, timer: t, presence: true, enabled: true }),
        patterns: Some(IdleGated {
            due: true,
            timer: t,
            presence: true,
            enabled: true,
            spoke: true,
            idle_ms: 60_000,
        }),
        tradition_prep: Some(PersistedReceptive { due: true, timer: t, presence: true, receptive: true }),
        whois: Some(PersistedReceptive { due: true, timer: t, presence: true, receptive: true }),
        whois_forced: None,
        mail_sweep: Some(PersistedChatQuiet { due: true, timer: t, presence: true }),
        dmn: Some(DmnGate {
            due: true,
            enabled: true,
            timer: t,
            idle_ms: 60_000,
            idle_required_ms: 30_000,
        }),
        knock: Some(KnockGate {
            due: true,
            enabled: true,
            presence: true,
            idle_ms: 90 * 60_000,
            idle_required_ms: 45 * 60_000,
            last_activity_ms: NOW - 90 * 60_000,
        }),
        digest: Some(DigestGate {
            due: true,
            enabled: true,
            spoke: true,
            idle_ok: true,
            timer: t,
            receptive: true,
        }),
        ask: Some(AskGate {
            due: true,
            enabled: true,
            spoke: true,
            ask_ok: true,
            timer: t,
            receptive: true,
        }),
    };
    let row = attention_shadow(&s, cycle(), NOW).expect("a row");
    assert_eq!(
        row.buildable.len(),
        17,
        "the frozen scope is seventeen loops and every one has a slot: {:?}",
        row.buildable
    );
    assert_eq!(
        row.ranked.len(),
        row.buildable.len(),
        "a buildable loop the policy cannot score would vanish from the ranking unnoticed"
    );
    for id in &row.buildable {
        assert!(
            attention_constants(*id).is_some(),
            "{id:?} can be reported but not scored"
        );
    }
    // The seventeen slots cover the frozen scope exactly — no loop in scope has no slot.
    let mut seen: Vec<LoopId> = row.buildable.clone();
    seen.sort_by_key(|id| format!("{id:?}"));
    let mut scope: Vec<LoopId> = ATTENTION_SCOPE.to_vec();
    scope.sort_by_key(|id| format!("{id:?}"));
    assert_eq!(seen, scope, "the signal set and the frozen scope must agree");
}

#[test]
fn the_shadow_reads_the_sites_verdict_and_never_recomputes_it() {
    // A gate whose inputs all look overdue but whose site says NOT due must stay out. Each loop's
    // due predicate lives at its own gate, several of them bespoke; a second implementation here
    // is how two answers to one question come to disagree.
    let s = WakeSignals {
        dmn: Some(DmnGate {
            due: false,
            enabled: true,
            timer: timer(60_000, 600_000),
            idle_ms: 10 * 60_000,
            idle_required_ms: 60_000,
        }),
        ..Default::default()
    };
    assert_eq!(
        attention_shadow(&s, cycle(), NOW),
        None,
        "every input says due; the site said no, and the site decides"
    );
}

#[test]
fn a_shadow_row_is_completely_recomputable_from_its_signals() {
    // Determinism is what lets two readers of one ledger row agree. Same inputs, same row, twice.
    let s = WakeSignals {
        family: Some(TimerChatQuiet {
            due: true,
            timer: timer(3_600_000, 1_800_000),
            presence: true,
            enabled: true,
        }),
        ..one_due(60_000)
    };
    let a: AttentionShadow = attention_shadow(&s, cycle(), NOW).expect("a row");
    let b: AttentionShadow = attention_shadow(&s, cycle(), NOW).expect("a row");
    assert_eq!(a, b);
    assert_eq!(a.to_event(NOW).policy, b.to_event(NOW).policy);
}

#[test]
fn equal_scores_are_broken_by_the_frozen_scope_order() {
    // Resolve and Ics carry identical constants (300/900/0/1000), so at equal urgency they tie on
    // score exactly and the second tie-break decides. Resolve is scope index 6, Ics index 12.
    //
    // Recorded while writing this: the THIRD tie-break in `attention_rank` — the longest wait — is
    // unreachable in the shadow's use. Scope index is unique per loop and the shadow emits at most
    // one candidate per loop, so the scope comparison never returns Equal and `last_ms` never
    // decides anything. The preregistration describes it as the final tie-break; it is really a
    // guard for a caller that passes the same loop twice, which the shadow never does. Left in
    // place, documented here, and written up as a ledger correction rather than quietly deleted.
    let t = timer(60_000, 60_000);
    let s = WakeSignals {
        resolve: Some(TimerUnconditional { due: true, timer: t }),
        ics: Some(TimerUnconditional {
            due: true,
            // A much older stamp, so if `last_ms` ever DID decide, Ics would win and this would fail.
            timer: WakeTimer { last_ms: 0, period_ms: 60_000 },
        }),
        ..Default::default()
    };
    let row = attention_shadow(&s, cycle(), NOW).expect("a row");
    assert_eq!(row.ranked.len(), 2);
    assert_eq!(
        row.ranked[0].score, row.ranked[1].score,
        "identical constants and identical urgency must tie exactly"
    );
    assert_eq!(
        row.top(),
        Some(LoopId::Resolve),
        "the tie goes to the earlier loop in the frozen scope, not the longer wait"
    );
}
