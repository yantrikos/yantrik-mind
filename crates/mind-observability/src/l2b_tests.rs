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

/// Someone is in the chat and it is not quiet hours. The WHOLE presence, because five entry types
/// used to collapse it to one boolean and drop `quiet` — including the two named "ChatQuiet".
fn here() -> crate::Presence {
    crate::Presence { chat_present: true, quiet: false }
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
        presence: here(),
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
    // THIS TEST ONCE ASSERTED THE OPPOSITE OF ITS OWN NAME. It ended `assert_eq!(a, b)` — the two
    // rows were identical, because the only record of a slot was `buildable`, which filters on due.
    // A slot never filled and a slot filled-and-not-due produced byte-identical rows, while the
    // struct's doc called telling them apart "the whole point of being total over the scope".
    // `filled` makes the hole visible in the row itself.
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
    // Neither is proposed: only a due gate is buildable.
    assert_eq!(a.buildable, vec![LoopId::Resolve]);
    assert_eq!(b.buildable, vec![LoopId::Resolve]);
    // But the coverage differs, and the row says so.
    assert_eq!(a.filled, vec![LoopId::Resolve, LoopId::MemberBeat]);
    assert_eq!(b.filled, vec![LoopId::Resolve]);
    assert_ne!(
        a, b,
        "a wake that evaluated two gates is not a wake that evaluated one"
    );
    assert_ne!(a.to_event(NOW).outcome, b.to_event(NOW).outcome);
    // ...and the EVENT names which slots were filled, so a reader finds the hole without the join.
    assert_eq!(
        a.to_event(NOW).outcome.as_deref(),
        Some("buildable:1 filled:2 filled_ids:resolve,member-beat")
    );
    assert_eq!(
        b.to_event(NOW).outcome.as_deref(),
        Some("buildable:1 filled:1 filled_ids:resolve")
    );
}

#[test]
fn the_floor_cannot_bind_under_policy_v1_so_the_shadow_never_abstains() {
    // The test that stood here was named for an abstention it could not produce, and its central
    // assertion was `abstained_empty == ranked.is_empty()` — true by construction, one line after
    // the field is assigned from exactly that expression.
    //
    // The real fact: the floor is `score >= 1`, and the LOWEST score any in-scope loop can take at
    // any urgency is far above it, so `ranked` is never empty when `due` is not. Compute it rather
    // than recall it, so a constants change that made abstention possible fails here and gets a
    // policy version instead of passing unnoticed.
    let mut worst = u64::MAX;
    let mut worst_id = None;
    for id in ATTENTION_SCOPE {
        let c = attention_constants(id).expect("every scope member has constants");
        for urgency in [0u64, 1, 500, 999, 1000] {
            let sc = crate::attention_score(&c, urgency);
            if sc < worst {
                worst = sc;
                worst_id = Some(id);
            }
        }
    }
    assert!(
        worst >= 1,
        "the floor CAN bind now ({worst_id:?} scores {worst}): abstention is live, and that is a new policy version with its own ledger row, not a silent change"
    );
    // The consequence, recorded where a reader of the metric will find it: the preregistered
    // "shadow false negative" metric — legacy acted while the shadow abstained — is degenerate at
    // zero under attention-policy-v1, because the shadow proposes something on every due wake.
    let row = attention_shadow(&one_due(0), cycle(), NOW).expect("a row");
    assert!(!row.abstained_empty);
    assert_eq!(row.top(), Some(LoopId::Resolve));
}

// ── the row ──────────────────────────────────────────────────────────────────────────────────
#[test]
fn exactly_one_row_per_wake_and_it_carries_that_wakes_identity() {
    let row = attention_shadow(&one_due(60_000), CycleId::new(11, 4), NOW).expect("a row");
    assert_eq!(row.cycle, CycleId::new(11, 4));
    let ev = row.to_event(NOW);
    assert_eq!(ev.kind, "attention_shadow");
    assert_eq!(
        ev.evaluator_id.as_deref(),
        Some(crate::ATTENTION_POLICY),
        "the row is stamped with the POLICY that produced its numbers; a v2 with different constants must not be indistinguishable from v1"
    );
    assert_eq!(
        ev.policy.first().map(String::as_str),
        Some(LOOP_LEDGER_V6),
        "and the wire shape rides alongside it"
    );
    assert_eq!(
        ev.object_id.as_deref(),
        Some("cycle:11:4"),
        "the wake identity is what the read side joins on"
    );
    assert_eq!(ev.actor.as_deref(), Some("attention"));
    assert_eq!(ev.lane.as_deref(), Some("shadow"));
    assert_eq!(ev.chosen.as_deref(), Some("resolve"));
    assert_eq!(ev.verdict.as_deref(), Some("ranked"));
    assert_eq!(
        ev.outcome.as_deref(),
        Some("buildable:1 filled:1 filled_ids:resolve")
    );
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
    // The pair has to DISAGREE. Ask (500/500/500/400) is scope index 3 and scores 262 at urgency
    // 1000; HomeWatch (800/800/300/700) is scope index 5 and scores 924. (An earlier version of
    // this comment quoted 5.3e11 and 1.85e12 — the raw numerator before the divisor, a number the
    // code never produces.) Collection order and score order point opposite ways, so only a real
    // sort puts HomeWatch first.
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
            presence: here(),
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
            presence: here(),
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
        home_watch: Some(TimerChatQuiet { due: true, timer: t, presence: here(), enabled: true }),
        family: Some(TimerChatQuiet { due: true, timer: t, presence: here(), enabled: true }),
        follow_up: Some(TimerChatQuiet { due: true, timer: t, presence: here(), enabled: true }),
        price_watch: Some(TimerChatQuiet { due: true, timer: t, presence: here(), enabled: true }),
        patterns: Some(IdleGated {
            due: true,
            timer: t,
            presence: here(),
            idle: crate::IdleInputs { enabled: true, spoke: true, idle: true },
        }),
        tradition_prep: Some(PersistedReceptive { due: true, timer: t, presence: here(), receptive: true }),
        whois: Some(PersistedReceptive { due: true, timer: t, presence: here(), receptive: true }),
        whois_forced: None,
        mail_sweep: Some(PersistedChatQuiet { due: true, timer: t, presence: here() }),
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
            presence: here(),
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
fn the_stored_row_carries_enough_to_rebuild_the_ranking() {
    // The test here called one pure function twice on the same input and asserted the results were
    // equal — guaranteed by construction, and it would have passed with the whole ranking feature
    // deleted. What matters is that the ROW a reader finds in the ledger contains the ranking, so a
    // second reader can rebuild it without ever seeing the signals.
    // The PAIR MATTERS. The first version used Resolve and Family, whose scope order and score
    // order already agree, so the closing "the stored order must be the ranked order" assertion
    // held with the ranking deleted — a vacuous assertion inside the test written to replace a
    // vacuous test. Ask (scope index 3, score 262) and HomeWatch (index 5, score 924) disagree.
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
            presence: here(),
            enabled: true,
        }),
        ..Default::default()
    };
    let row: AttentionShadow = attention_shadow(&s, cycle(), NOW).expect("a row");
    let ev = row.to_event(NOW);
    // Rebuild from the event alone: skip the leading schema token, then read "<loop>:<score>".
    let rebuilt: Vec<(String, u64)> = ev
        .policy
        .iter()
        .skip(1)
        .filter_map(|p| {
            let (name, score) = p.rsplit_once(':')?;
            Some((name.to_string(), score.parse().ok()?))
        })
        .collect();
    assert_eq!(
        rebuilt.len(),
        row.ranked.len(),
        "every ranked loop must be in the row: {:?}",
        ev.policy
    );
    for (i, c) in row.ranked.iter().enumerate() {
        assert_eq!(rebuilt[i].0, c.loop_id.as_str());
        assert_eq!(rebuilt[i].1, c.score);
    }
    // ...and the order survives the round trip, which is the part a reader depends on.
    let scores: Vec<u64> = rebuilt.iter().map(|(_, s)| *s).collect();
    let mut sorted = scores.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(scores, sorted, "the stored order must be the ranked order");
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

// ── E.L2B-R: the reader ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod l2b_reader_tests {
    use crate::*;

    fn wake(n: u64) -> CycleId {
        CycleId::new(100, n)
    }

    fn tick(c: Option<CycleId>, id: LoopId, held: Option<HeldReason>, ts: u64) -> DecisionEvent {
        let opp = LoopOpportunity::Window {
            loop_id: id,
            process_start_ms: 100,
            key: ts,
        };
        let t = match held {
            None => LoopTick::acted(opp, LoopHost::Process, LoopOutcome::Ran),
            Some(h) => LoopTick::held(opp, LoopHost::Process, h),
        };
        match c {
            Some(c) => t.in_wake(c).to_event(ts),
            None => t.to_event(ts),
        }
    }

    fn shadow(c: CycleId, considered: &[&str], chose: &str, ts: u64) -> DecisionEvent {
        let mut ev = DecisionEvent::new(&format!("attn-{}", c.render()), "attention_shadow");
        ev.ts_ms = ts;
        ev.object_id = Some(c.render());
        ev.candidates = considered.iter().map(|s| s.to_string()).collect();
        ev.chosen = Some(chose.into());
        ev.verdict = Some("ranked".into());
        ev
    }

    /// KILL 1: it names WHICH loops. A reader that says "3 buildable" is the feed's defect with
    /// extra steps — the feed already shows counts, and that is exactly why the shadow was mute.
    #[test]
    fn it_names_the_loops_not_just_how_many() {
        let evs = vec![
            shadow(wake(1), &["ics", "lease-sweep"], "ics", 10),
            tick(Some(wake(1)), LoopId::Ics, None, 11),
            tick(Some(wake(1)), LoopId::LeaseSweep, None, 12),
        ];
        let out = render_attention_shadow_at(&evs, 20);
        assert!(out.contains("ics"), "the loop is named: {out}");
        assert!(out.contains("lease-sweep"), "and so is the other one: {out}");
        assert!(out.contains(&wake(1).render()), "the wake is identified: {out}");
    }

    /// KILL 2, AND THE DEFINITION THAT MATTERS MOST: a HELD row is a DUE row.
    ///
    /// Comparing against acted rows alone reports a flattering zero and silently forgives every
    /// gate whose loops are usually blocked — which on an idle box is most of them. The real first
    /// reading on staging was exactly this shape: `ask` held on presence and `dmn` held by the idle
    /// gate, both due, neither considered.
    #[test]
    fn a_held_loop_is_due_and_counts_against_the_shadow() {
        let evs = vec![
            shadow(wake(1), &["ics"], "ics", 10),
            tick(Some(wake(1)), LoopId::Ics, None, 11),
            tick(Some(wake(1)), LoopId::Dmn, Some(HeldReason::IdleGate), 12),
        ];
        let out = render_attention_shadow_at(&evs, 20);
        assert!(
            out.contains("UNSEEN     : dmn"),
            "a held-but-due loop the shadow never considered is a shortfall: {out}"
        );
        assert!(
            out.contains("1 carried a due loop the shadow never considered"),
            "and it is counted: {out}"
        );
    }

    /// KILL 2 again, the OTHER flattering zero: the denominator is every wake with loop activity,
    /// not every wake that produced a shadow row. A wake where only unwired loops act writes NO
    /// shadow row, so scoping to shadow rows hides the strongest case there is.
    #[test]
    fn a_wake_with_no_shadow_row_is_still_counted() {
        let evs = vec![
            shadow(wake(1), &["ics"], "ics", 10),
            tick(Some(wake(1)), LoopId::Ics, None, 11),
            // Wake 2: a loop acted and the shadow said nothing at all.
            tick(Some(wake(2)), LoopId::Dmn, None, 20),
        ];
        let out = render_attention_shadow_at(&evs, 30);
        assert!(
            out.contains("NO SHADOW ROW"),
            "the wake the shadow never saw must appear: {out}"
        );
        assert!(
            out.contains("2 wake(s)"),
            "and it must be in the denominator: {out}"
        );
        assert!(out.contains("UNSEEN     : dmn"), "{out}");
    }

    /// KILL 3: with nothing recorded it must SAY so — an empty table reads as "nothing was due",
    /// which is the one thing the operator must not conclude from a shadow that is switched off.
    #[test]
    fn silence_is_reported_as_silence_not_as_an_empty_table() {
        let out = render_attention_shadow_at(&[], 99);
        assert!(out.contains("No attention shadow rows"), "{out}");
        assert!(
            out.contains("YM_ATTENTION_SHADOW"),
            "it must say the shadow is off by default, or silence looks like evidence: {out}"
        );
    }

    /// Rows that cannot be paired are REPORTED, never dropped. A denominator that quietly shrinks
    /// to the rows that happen to pair is the censoring pattern that cost two earlier readings.
    #[test]
    fn rows_without_a_wake_are_reported_rather_than_dropped() {
        let evs = vec![
            shadow(wake(1), &["ics"], "ics", 10),
            tick(Some(wake(1)), LoopId::Ics, None, 11),
            tick(None, LoopId::Dmn, None, 12),
            tick(None, LoopId::Knock, None, 13),
        ];
        let out = render_attention_shadow_at(&evs, 20);
        assert!(
            out.contains("2 loop row(s) carry no wake"),
            "the unpairable rows are named in the report: {out}"
        );
        // And they must NOT be silently treated as a shortfall against a wake they never belonged to.
        assert!(!out.contains("UNSEEN     : knock"), "{out}");
    }

    /// The one separator the aligned label columns use.
    fn american_colon() -> &'static str {
        ": "
    }

    /// The report is READ BY A PERSON. A Rust string continuation that does not collapse leaves a
    /// run of spaces mid-sentence; that shipped to staging in E.CFG2's notice this morning, was
    /// fixed and guarded THERE, and I reintroduced it here the same day. So the guard travels with
    /// the habit rather than with the one string that taught it.
    #[test]
    fn no_line_carries_a_run_of_whitespace_mid_sentence() {
        let evs = vec![
            shadow(wake(1), &["ics"], "ics", 10),
            tick(Some(wake(1)), LoopId::Ics, None, 11),
            tick(None, LoopId::Dmn, None, 12),
        ];
        for out in [
            render_attention_shadow_at(&evs, 20),
            render_attention_shadow_at(&[], 20),
        ] {
            for line in out.lines() {
                // A padded label column ("scores     : ...") is deliberate alignment, so only the
                // VALUE after the colon is prose. Everything else is checked whole.
                let body = line.trim_start();
                let prose = match body.split_once(american_colon()) {
                    Some((label, value)) if label.chars().all(|c| c.is_ascii_alphabetic() || c == ' ') => value,
                    _ => body,
                };
                assert!(
                    !prose.contains("  "),
                    "a run of spaces mid-sentence: {line:?}"
                );
                assert_eq!(line.trim_end(), line, "trailing whitespace: {line:?}");
            }
        }
    }

    /// A window with no misses must not be reported as completeness. On an idle box a gate whose
    /// loops never came due cannot be missed, and saying "0 unseen" without that caveat would let
    /// a quiet night stand in for evidence the shadow is complete.
    #[test]
    fn a_clean_window_is_not_called_completeness() {
        let evs = vec![
            shadow(wake(1), &["ics"], "ics", 10),
            tick(Some(wake(1)), LoopId::Ics, None, 11),
        ];
        let out = render_attention_shadow_at(&evs, 20);
        assert!(out.contains("weak evidence, not completeness"), "{out}");
    }
}

// ── E.G2-R: the world shadow's reader ─────────────────────────────────────────────────────────

#[cfg(test)]
mod g2r_reader_tests {
    use crate::*;

    fn shadow(sample: &str, outcome: &str, ts: u64, id: &str) -> DecisionEvent {
        let mut ev = DecisionEvent::new(id, "world_shadow");
        ev.ts_ms = ts;
        ev.goal_id = Some(format!("worldshadow:{sample}"));
        ev.outcome = Some(outcome.into());
        ev.chosen = Some("shadow-only".into());
        ev.verdict = Some("shadowed".into());
        ev
    }

    fn disposition(parent: Option<&str>, chosen: &str, verdict: &str, ts: u64) -> DecisionEvent {
        let mut ev = DecisionEvent::new(&format!("disp-{ts}"), "knock_disposition");
        ev.ts_ms = ts;
        ev.parent_event_id = parent.map(str::to_string);
        ev.chosen = Some(chosen.into());
        ev.verdict = Some(verdict.into());
        ev
    }

    /// KILL 1: the two samples are never pooled. One measures agreement with a decision, the other
    /// only that the pipeline is alive; an average of them describes neither.
    #[test]
    fn the_two_samples_are_reported_separately() {
        let evs = vec![
            shadow("knock-receptivity", "unknown", 10, "s1"),
            shadow("knock-receptivity", "unknown", 11, "s2"),
            shadow("headless-cadence", "known:home", 12, "s3"),
        ];
        let out = render_world_shadow_at(&evs, 20);
        assert!(out.contains("sample knock-receptivity"), "{out}");
        assert!(out.contains("sample headless-cadence"), "{out}");
        // Each carries its OWN denominator; a pooled 3 would be the bug.
        assert!(out.contains("2 consult(s)"), "the paired sample counts alone: {out}");
        assert!(out.contains("1 consult(s)"), "and so does the unpaired one: {out}");
    }

    /// KILL 2: the disposition breakdown sits beside the split. "known 1.5%" alone reads as a
    /// failing model; the truth on the canary was that every evaluation exited at no_packets and
    /// the model was answering about an idle box.
    #[test]
    fn the_reason_evaluations_ended_is_shown_beside_what_the_model_knew() {
        let evs = vec![
            shadow("knock-receptivity", "unknown", 10, "s1"),
            disposition(Some("s1"), "no_packets", "before-gate", 11),
        ];
        let out = render_world_shadow_at(&evs, 20);
        assert!(
            out.contains("ended at no_packets"),
            "why the evaluation ended must appear: {out}"
        );
    }

    /// KILL 3: join health. A silently broken join makes any agreement number meaningless while
    /// the report still looks healthy.
    #[test]
    fn join_health_is_reported() {
        let evs = vec![
            shadow("knock-receptivity", "unknown", 10, "s1"),
            disposition(Some("s1"), "no_packets", "before-gate", 11),
            disposition(Some("missing"), "no_packets", "before-gate", 12),
            disposition(None, "no_packets", "before-gate", 13),
        ];
        let out = render_world_shadow_at(&evs, 20);
        assert!(
            out.contains("joined to a shadow row 1, orphaned 2"),
            "the join must be counted both ways: {out}"
        );
    }

    /// KILL 4, THE ONE THAT MATTERS: with nothing reaching the gate, the agreement is UNCOMPUTABLE
    /// and must be named as such. A zero standing where "uncomputable" belongs looks like a healthy
    /// result — no disagreement — and is the single way this report could quietly mislead.
    #[test]
    fn an_agreement_that_cannot_be_computed_is_never_reported_as_zero() {
        let evs = vec![
            shadow("knock-receptivity", "unknown", 10, "s1"),
            disposition(Some("s1"), "no_packets", "before-gate", 11),
        ];
        let out = render_world_shadow_at(&evs, 20);
        assert!(out.contains("UNCOMPUTABLE"), "{out}");
        assert!(
            out.contains("not zero disagreement"),
            "it must say what the absence is NOT, or a reader supplies the wrong meaning: {out}"
        );
    }

    /// And when the gate DOES run, it says so instead — the report must not be permanently pessimistic.
    #[test]
    fn a_reached_gate_is_reported_as_comparable() {
        let evs = vec![
            shadow("knock-receptivity", "known:home", 10, "s1"),
            disposition(Some("s1"), "sent", "receptive", 11),
        ];
        let out = render_world_shadow_at(&evs, 20);
        assert!(!out.contains("UNCOMPUTABLE"), "{out}");
        assert!(out.contains("1 evaluation(s) reached the gate"), "{out}");
    }

    /// A window with shadow rows but NO evaluations at all must not say "0 evaluations exited
    /// before the gate" — that phrasing implies evaluations happened and none passed, which is a
    /// different and more damning claim than "nothing ran". The staging deploy printed exactly that
    /// sentence over zero evaluations, and a mutation showed this branch had no test at all.
    #[test]
    fn no_evaluations_is_worded_differently_from_all_evaluations_exiting_early() {
        let evs = vec![shadow("headless-cadence", "unknown", 10, "s1")];
        let out = render_world_shadow_at(&evs, 20);
        assert!(out.contains("UNCOMPUTABLE"), "{out}");
        assert!(
            out.contains("No knock evaluation ran"),
            "with nothing to compare it must say nothing ran: {out}"
        );
        assert!(
            !out.contains("0 evaluation(s) exited"),
            "a count of zero must never be phrased as evaluations that exited early: {out}"
        );
    }

    /// Silence is silence, not an empty table.
    #[test]
    fn no_rows_says_so() {
        let out = render_world_shadow_at(&[], 99);
        assert!(out.contains("No world-shadow rows"), "{out}");
    }
}

