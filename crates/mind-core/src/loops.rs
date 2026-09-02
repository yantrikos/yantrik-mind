//! L3a (ARCH7 §4 L3, first slice): the process-hosted loop runner.
//!
//! Three loops that speak to nobody — the external-calendar refresh, the standing-lease sweep,
//! and the default-mode (offline-cognition) tick — run here on EVERY box, Telegram or headless,
//! instead of inside the Telegram poll loop where they only ran when a phone was configured.
//! Their bodies are moved from the poll loop, not rewritten: same gate kind, same considered set,
//! same policy lines, same loop-ledger recording, same timer transition. What changed is the
//! executor (`LoopHost::Process`) and, for DMN, the idle source: the engine's own turn exclusion
//! (`TurnExclusion::try_admit_dmn`), which is atomic against a turn starting on any surface.
//!
//! Frozen shape (the L3a prereg): one non-reentrant task per process behind a start latch; the
//! sole owner of the three timer states; a 5 s interval with `MissedTickBehavior::Delay`; the three
//! bodies awaited serially in the poll loop's order (Ics → LeaseSweep → DMN); no per-body spawn;
//! nothing in this module sends, and no model call happens here outside DMN's existing pass.
use crate::telegram::now_ms;
use mind_conversation::ConversationEngine;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

static RUNNER_STARTED: AtomicBool = AtomicBool::new(false);

/// Start the runner exactly once per process. A second call is refused and reported.
pub(crate) fn spawn_loop_runner(conv: Arc<ConversationEngine>) -> bool {
    if !claim_runner_start() {
        eprintln!("[loops] runner already started in this process; second start refused");
        return false;
    }
    tokio::spawn(run_loops(conv));
    true
}

/// The one claim on the latch: true for the first caller in this process, false after.
fn claim_runner_start() -> bool {
    !RUNNER_STARTED.swap(true, Ordering::AcqRel)
}

/// Test-only: forget the latch so a fixture can prove the second start is refused.
#[cfg(test)]
pub(crate) fn reset_runner_latch_for_test() {
    RUNNER_STARTED.store(false, Ordering::Release);
}

fn runner_period_secs() -> u64 {
    std::env::var("YM_LOOP_RUNNER_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|s: &u64| *s >= 1)
        .unwrap_or(5)
}

async fn run_loops(conv: Arc<ConversationEngine>) {
    let process_start_ms = now_ms();
    let mut state = RunnerState::default();
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(runner_period_secs()));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        // Serial, legacy order; no spawn.
        state.last_ics = run_ics(&conv, process_start_ms, state.last_ics).await;
        state.last_lease_sweep =
            run_lease_sweep(&conv, process_start_ms, state.last_lease_sweep).await;
        run_dmn(&conv, process_start_ms, &mut state).await;
    }
}

/// The three timer states, owned by the runner task alone.
#[derive(Default)]
pub(crate) struct RunnerState {
    pub(crate) last_ics: u64,
    pub(crate) last_lease_sweep: u64,
    pub(crate) last_dmn: u64,
    pub(crate) gate_dmn: mind_observability::OpportunityGate,
}

/// External-calendar refresh: re-pull the read-only ICS feed if one is connected. Paced
/// (YM_ICS_SECS, default 6h); no chat gating — it only updates stored events, sends nothing.
pub(crate) async fn run_ics(
    conv: &ConversationEngine,
    process_start_ms: u64,
    last_ics: u64,
) -> u64 {
    let period: u64 = std::env::var("YM_ICS_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(21_600);
    let now = now_ms();
    let ics_gate = mind_observability::Gated::timer(mind_observability::Timer {
        now_ms: now,
        last_ms: last_ics,
        period_ms: period * 1000,
    });
    let ics_decision = ics_gate.decide();
    if ics_decision == mind_observability::GateDecision::Act {
        let ics_t0 = now_ms();
        let n = conv.refresh_ics().await;
        if n > 0 {
            eprintln!("[calendar] refreshed {n} external event(s)");
        }
        conv.record_loop_tick(
            mind_observability::LoopTick::acted(
                mind_observability::LoopOpportunity::Window {
                    loop_id: mind_observability::LoopId::Ics,
                    process_start_ms,
                    key: last_ics,
                },
                mind_observability::LoopHost::Process,
                mind_observability::LoopOutcome::Ran,
            )
            .considered(&[mind_observability::ConsideredSignal::DueDelegations])
            .policy(&[mind_observability::LoopPolicy::Cadence(period)])
            .count(n as u32)
            .wall_ms(now_ms().saturating_sub(ics_t0)),
        );
        return ics_gate.advance(ics_decision);
    }
    last_ics
}

/// Standing-lease expiry sweep (ARCH-6 P.4): its own cursor, no chat gating — it only ends
/// leases whose time has passed, records each, and logs only when it did.
pub(crate) async fn run_lease_sweep(
    conv: &ConversationEngine,
    process_start_ms: u64,
    last_lease_sweep: u64,
) -> u64 {
    let period: u64 = std::env::var("YM_LEASE_SWEEP_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let now = now_ms();
    let ls_gate = mind_observability::Gated::timer(mind_observability::Timer {
        now_ms: now,
        last_ms: last_lease_sweep,
        period_ms: period * 1000,
    });
    let ls_decision = ls_gate.decide();
    if ls_decision == mind_observability::GateDecision::Act {
        let ls_t0 = now_ms();
        let mut swept: u32 = 0;
        for line in conv.sweep_leases().await {
            swept += 1;
            eprintln!("{line}");
        }
        conv.record_loop_tick(
            mind_observability::LoopTick::acted(
                mind_observability::LoopOpportunity::Window {
                    loop_id: mind_observability::LoopId::LeaseSweep,
                    process_start_ms,
                    key: last_lease_sweep,
                },
                mind_observability::LoopHost::Process,
                mind_observability::LoopOutcome::Ran,
            )
            .considered(&[mind_observability::ConsideredSignal::DueDelegations])
            .policy(&[mind_observability::LoopPolicy::Cadence(period)])
            .count(swept)
            .wall_ms(now_ms().saturating_sub(ls_t0)),
        );
        return ls_gate.advance(ls_decision);
    }
    last_lease_sweep
}

/// Default-mode ("sleep") tick: when the user has been idle past the threshold on every surface,
/// run ONE bounded offline-cognition pass (rehearse → reconcile → associate over the typed
/// substrate). Paced so it fires at most every YM_DMN_SECS, and admitted only while idle — the
/// admission is atomic against a turn starting, so it never STARTS while a turn is in flight.
pub(crate) async fn run_dmn(
    conv: &ConversationEngine,
    process_start_ms: u64,
    st: &mut RunnerState,
) {
    // L1 v3: the due window is computed OUTSIDE the enable switch so a disabled DMN still
    // records `held:disabled` once per window; the switch itself is unchanged.
    let dmn_on = std::env::var("YM_DMN").map(|v| v != "off").unwrap_or(true);
    let idle_secs: u64 = std::env::var("YM_DMN_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let period: u64 = std::env::var("YM_DMN_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    let now = now_ms();
    let dmn_due = now.saturating_sub(st.last_dmn) >= period * 1000;
    let dmn_considered = [
        mind_observability::ConsideredSignal::Tensions,
        mind_observability::ConsideredSignal::Beliefs,
        mind_observability::ConsideredSignal::PaperDesk,
    ];
    let dmn_policy = [
        mind_observability::LoopPolicy::Cadence(period),
        mind_observability::LoopPolicy::Idle(idle_secs),
        mind_observability::LoopPolicy::Budget(mind_observability::BudgetKind::DmnOneCall),
    ];
    // Admission is the one place the idle source changed: the engine's turn exclusion, not the
    // poll loop's private activity stamp. The permit lives for the pass and is dropped after it.
    let permit = if dmn_on && dmn_due {
        conv.turns().try_admit_dmn(now, idle_secs * 1000)
    } else {
        None
    };
    if let Some(_permit) = permit {
        let t0 = now_ms();
        let lines = conv.dmn_tick().await;
        for line in &lines {
            eprintln!("{line}");
        }
        // The act records under the due window it closes; model calls stay unknown until
        // the DMN reports its own count (never inferred from its budget).
        st.gate_dmn.mark(st.last_dmn);
        conv.record_loop_tick(
            mind_observability::LoopTick::acted(
                mind_observability::LoopOpportunity::Window {
                    loop_id: mind_observability::LoopId::Dmn,
                    process_start_ms,
                    key: st.last_dmn,
                },
                mind_observability::LoopHost::Process,
                mind_observability::LoopOutcome::Dreamed,
            )
            .considered(&dmn_considered)
            .policy(&dmn_policy)
            .count(lines.len() as u32)
            .wall_ms(now_ms().saturating_sub(t0)),
        );
        st.last_dmn = now;
    } else if dmn_due {
        if let Some(window) = st.gate_dmn.take_window(
            mind_observability::LoopId::Dmn,
            process_start_ms,
            st.last_dmn,
        ) {
            conv.record_loop_tick(
                mind_observability::LoopTick::held(
                    window,
                    mind_observability::LoopHost::Process,
                    if dmn_on {
                        mind_observability::HeldReason::IdleGate
                    } else {
                        mind_observability::HeldReason::Disabled
                    },
                )
                .considered(&dmn_considered)
                .policy(&dmn_policy),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runner module sends nothing and calls no model outside DMN's own pass; the three
    /// bodies keep their gate kinds, considered sets, policy lines, ledger recording and timer
    /// transitions; the runner is serial, single-owner, latched, and delay-on-miss.
    #[test]
    fn the_runner_is_frozen_as_preregistered() {
        let src = include_str!("loops.rs");
        let body = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        assert!(!body.contains("tg_send"), "the runner never sends");
        assert!(
            !body.contains("inference"),
            "no model call from the runner itself"
        );
        assert!(
            !body.contains("chat_grounded"),
            "no model call from the runner itself"
        );
        assert_eq!(
            body.matches("tokio::spawn(").count(),
            1,
            "one spawn: the runner task"
        );
        assert!(body.contains("MissedTickBehavior::Delay"));
        assert!(body.contains("RUNNER_STARTED.swap(true"));
        // Serial legacy order, no per-body spawn.
        let i = body.find("run_ics(").unwrap();
        let l = body.find("run_lease_sweep(").unwrap();
        let d = body.find("run_dmn(").unwrap();
        assert!(i < l && l < d);
        // Each body records under the process host with its legacy kind and lines.
        for needle in [
            "mind_observability::Gated::timer(",
            "LoopId::Ics,",
            "LoopId::LeaseSweep,",
            "LoopId::Dmn,",
            "BudgetKind::DmnOneCall",
            "ConsideredSignal::DueDelegations",
            "ics_gate.advance(ics_decision)",
            "ls_gate.advance(ls_decision)",
            "try_admit_dmn(now, idle_secs * 1000)",
        ] {
            assert!(body.contains(needle), "{needle}");
        }
        assert!(!body.contains("LoopHost::Telegram") && !body.contains("LoopHost::Headless"));
        assert_eq!(
            body.matches("mind_observability::LoopHost::Process")
                .count(),
            4
        );
    }

    /// Every frontend reply callsite in mind-core reaches the engine through one of its three
    /// registering entries — `turn`, `fast_reply`, `cli_dispatch` — and never through
    /// `handle_turn` / `handle_turn_as` directly, so the turn exclusion covers every surface by
    /// construction. Enumerated here so a new surface must be added consciously.
    #[test]
    fn every_frontend_reply_callsite_routes_through_a_registering_entry() {
        let sources = [
            include_str!("telegram.rs"),
            include_str!("web.rs"),
            include_str!("lib.rs"),
        ];
        let mut turn = 0;
        let mut fast = 0;
        let mut cli = 0;
        for src in sources {
            // Production text only: the guard tests quote these names inside string literals.
            let prod = &src[..src.find("#[cfg(test)]").unwrap_or(src.len())];
            assert!(
                !prod.contains(".handle_turn(") && !prod.contains(".handle_turn_as("),
                "a frontend must not call the unregistered inner turn"
            );
            turn += prod.matches("conv.turn(").count() + prod.matches("conv2.turn(").count();
            fast += prod.matches("conv.fast_reply(").count();
            cli += prod.matches("conv.cli_dispatch(").count();
        }
        assert!(
            turn >= 5,
            "the agentic surfaces: telegram, control, chat, openai, frame, repl"
        );
        assert_eq!(fast, 1, "the voice fast path is one surface");
        assert!(cli >= 3, "the operator console surfaces");
    }

    /// One runner per process: the second start is refused, through the same claim helper
    /// `spawn_loop_runner` uses.
    #[test]
    fn the_start_latch_admits_exactly_one_runner() {
        reset_runner_latch_for_test();
        assert!(claim_runner_start(), "the first start claims the latch");
        assert!(!claim_runner_start(), "a second start is refused");
        assert!(!claim_runner_start(), "and stays refused");
        reset_runner_latch_for_test();
    }

    /// The amended timing gate. `advance` resets a timer to its actual fire time, so pacing
    /// depends on the host. The legacy poll loop BLOCKS in its long poll (up to 25 s) and then
    /// wakes (≈1.5 s), so with no updates it evaluates its gates about every 26.5 s; the runner
    /// evaluates every 5 s. Modelled exactly that way: each host's evaluation clock advances by
    /// its own step. The gate is per act, against its OWN due boundary (the previous act plus
    /// the period): every runner act lands within [0, +5 s] after due; the poll loop's landed
    /// within [0, +26.5 s]; the higher sweep frequency that follows is the declared L3a
    /// behaviour change, not a deviation.
    #[test]
    fn every_runner_act_lands_within_one_tick_after_its_own_due_boundary() {
        /// Replay one cadence for a day with an evaluation every `eval_step_ms`. Returns
        /// (act time, due boundary) pairs, the boundary being the previous act plus the period.
        fn replay(eval_step_ms: u64, period_ms: u64) -> Vec<(u64, u64)> {
            let start = 1_788_300_000_000u64;
            let end = start + 24 * 60 * 60 * 1000;
            let mut eval = start;
            let mut last = 0u64;
            let mut acts = Vec::new();
            while eval < end {
                let gate = mind_observability::Gated::timer(mind_observability::Timer {
                    now_ms: eval,
                    last_ms: last,
                    period_ms,
                });
                let d = gate.decide();
                if d == mind_observability::GateDecision::Act {
                    let due_at = if last == 0 { start } else { last + period_ms };
                    acts.push((eval, due_at));
                    last = gate.advance(d);
                }
                eval += eval_step_ms;
            }
            acts
        }
        for period_ms in [60_000u64, 300_000, 21_600_000] {
            let runner = replay(5_000, period_ms);
            let poll = replay(26_500, period_ms);
            assert!(!runner.is_empty() && !poll.is_empty());
            for (act, due) in &runner {
                let late = *act as i64 - *due as i64;
                assert!(
                    (0..=5_000).contains(&late),
                    "period {period_ms}: runner act {act} is {late} ms after due {due}"
                );
            }
            let mut worst_poll = 0i64;
            for (act, due) in &poll {
                let late = *act as i64 - *due as i64;
                assert!((0..=26_500).contains(&late), "poll loop lateness {late}");
                worst_poll = worst_poll.max(late);
            }
            // The declared consequence, made visible: the blocking poll loop really was late on
            // short cadences, and the runner never acts fewer times than it did.
            if period_ms == 60_000 {
                assert!(
                    worst_poll > 5_000,
                    "a 60 s sweep under a blocking poll ran late"
                );
                assert!(runner.len() > poll.len(), "the runner sweeps more often");
            }
            assert!(runner.len() >= poll.len(), "period {period_ms}");
        }
    }
}
