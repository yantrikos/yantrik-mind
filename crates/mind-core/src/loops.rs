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
//! sole owner of the timer states; a 5 s interval with `MissedTickBehavior::Delay`; the bodies
//! awaited serially in the poll loop's order; no per-body spawn; nothing in this module sends —
//! every line goes through the delivery seam — and the only model calls are the four named
//! passes' own (Resolve grading, ProfileRefresh's one learn call, Patterns' one grounded call,
//! DMN's one call).
//!
//! L3b (second slice) moved the three judge-calling loops that speak — Resolve, ProfileRefresh,
//! Patterns — here, behind `crate::delivery::Delivery`, so they run on a box with no phone. Their
//! gates, considered sets, cadences, ledger recording and timer transitions are unchanged; each
//! gained the `Budget` line naming its one-call bound. What changed is stated: a line lands on
//! Telegram when reachable outside quiet hours, else in the console notice queue (quiet hours
//! now queue instead of dropping); Patterns' presence input means "a surface exists", its idle
//! input is the engine's turn exclusion, and its `spoke` input is "a proactive line was sent in
//! the last ten minutes" rather than the poll loop's per-tick flag.
use crate::delivery::{Delivered, Delivery, EngagingRoute};
use crate::telegram::{in_quiet_hours_now, now_ms, quiet_hours_end_at_ms};
use mind_conversation::turn_exclusion::BackgroundPass;
use mind_conversation::{ConversationEngine, EngagementMarker, LegacyOutcome};
use mind_observability::{DeliveryKind, HeldReason, LoopId, LoopOutcome};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

static RUNNER_STARTED: AtomicBool = AtomicBool::new(false);

/// Start the runner exactly once per process. A second call is refused and reported.
pub(crate) fn spawn_loop_runner(conv: Arc<ConversationEngine>, delivery: Arc<Delivery>) -> bool {
    if !claim_runner_start() {
        eprintln!("[loops] runner already started in this process; second start refused");
        return false;
    }
    tokio::spawn(run_loops(conv, delivery));
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

async fn run_loops(conv: Arc<ConversationEngine>, delivery: Arc<Delivery>) {
    let process_start_ms = now_ms();
    // Legacy boot stamps: profile refresh and patterns do not fire right after boot.
    let mut state = RunnerState {
        last_profile: process_start_ms,
        last_patterns: process_start_ms,
        // Legacy: no proactive digest right after boot; the ask may pose its first question.
        last_digest: process_start_ms,
        ..Default::default()
    };
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(runner_period_secs()));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        // L3c: engagement housekeeping first — the stale resolvers and the shown-marker outbox —
        // so an unanswered claim closes on every box within a minute of its deadline.
        state.last_housekeeping = run_housekeeping(&conv, state.last_housekeeping).await;
        // Serial, legacy order; no spawn.
        state.last_ics = run_ics(&conv, process_start_ms, state.last_ics).await;
        state.last_lease_sweep =
            run_lease_sweep(&conv, process_start_ms, state.last_lease_sweep).await;
        state.last_resolve =
            run_resolve(&conv, &delivery, process_start_ms, state.last_resolve).await;
        state.last_profile =
            run_profile_refresh(&conv, &delivery, process_start_ms, state.last_profile).await;
        run_engagement(&conv, &delivery, process_start_ms, &mut state).await;
        run_patterns(&conv, &delivery, process_start_ms, &mut state).await;
        run_dmn(&conv, process_start_ms, &mut state).await;
    }
}

/// The timer states, owned by the runner task alone.
#[derive(Default)]
pub(crate) struct RunnerState {
    /// L3c: the housekeeping step's own clock (≤ 60 s).
    pub(crate) last_housekeeping: u64,
    pub(crate) last_ics: u64,
    pub(crate) last_lease_sweep: u64,
    pub(crate) last_resolve: u64,
    pub(crate) last_profile: u64,
    pub(crate) last_patterns: u64,
    pub(crate) gate_patterns: mind_observability::OpportunityGate,
    /// L3c-2: the engagement loops' own clocks and gates (knock has no cadence: a stretch gate).
    pub(crate) last_digest: u64,
    pub(crate) last_ask: u64,
    pub(crate) gate_knock: mind_observability::OpportunityGate,
    pub(crate) gate_digest: mind_observability::OpportunityGate,
    pub(crate) gate_ask: mind_observability::OpportunityGate,
    pub(crate) last_dmn: u64,
    pub(crate) gate_dmn: mind_observability::OpportunityGate,
}

/// L3b: "spoke" for the process host — a proactive line was SENT within this window.
const SPOKE_WINDOW_MS: i64 = 10 * 60 * 1000;

/// L3c: the housekeeping cadence — the one process-hosted owner of the stale resolvers.
const HOUSEKEEPING_PERIOD_MS: u64 = 60_000;

/// L3c: engagement housekeeping, timer-only, speaks to nobody, calls no model. (1) The stale
/// resolvers: an unanswered proactive claim resolves as ignored no later than its deadline plus
/// this period; the pace ledger's rows the same way (E.P3's corrected rule). (2) The durable
/// outbox: any shown-but-uncommitted engagement marker is committed at its shown instant.
/// (3) Engaging notices past their show-by bound get their terminal receipt so the day's knock
/// slot frees even when no cockpit returns. Not a loop opportunity: it has no gate to hold.
pub(crate) async fn run_housekeeping(conv: &ConversationEngine, last: u64) -> u64 {
    let now = now_ms();
    if now.saturating_sub(last) < HOUSEKEEPING_PERIOD_MS {
        return last;
    }
    conv.resolve_proactive(false).await;
    conv.ledger_resolve(false).await;
    let _ = conv.sweep_engaging_expiry();
    let committed = conv.reconcile_shown_engagements().await;
    if committed > 0 {
        eprintln!(
            "[housekeeping] committed {committed} shown engagement marker(s) from the outbox"
        );
    }
    now
}

/// Prediction-resolver tick: grade any predictions whose deadline has passed against the current
/// understanding, write the hit/miss into per-domain calibration, and surface each verdict
/// through the seam. Paced (YM_RESOLVE_SECS, default 1h); this is the self-scoring half of the
/// learning curve running on its own — no user prompt needed for tracked subjects.
pub(crate) async fn run_resolve(
    conv: &ConversationEngine,
    delivery: &Delivery,
    process_start_ms: u64,
    last_resolve: u64,
) -> u64 {
    let period: u64 = std::env::var("YM_RESOLVE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600);
    let now = now_ms();
    let rs_gate = mind_observability::Gated::timer(mind_observability::Timer {
        now_ms: now,
        last_ms: last_resolve,
        period_ms: period * 1000,
    });
    let rs_decision = rs_gate.decide();
    if rs_decision != mind_observability::GateDecision::Act {
        return last_resolve;
    }
    let rs_t0 = now_ms();
    let mut verdicts: u32 = 0;
    for verdict in conv.resolve_predictions(false).await {
        verdicts += 1;
        delivery
            .deliver(mind_observability::DeliveryKind::Verdict, &verdict)
            .await;
    }
    conv.record_loop_tick(
        mind_observability::LoopTick::acted(
            mind_observability::LoopOpportunity::Window {
                loop_id: mind_observability::LoopId::Resolve,
                process_start_ms,
                key: last_resolve,
            },
            mind_observability::LoopHost::Process,
            mind_observability::LoopOutcome::Ran,
        )
        .considered(&[mind_observability::ConsideredSignal::Beliefs])
        .policy(&[
            mind_observability::LoopPolicy::Cadence(period),
            mind_observability::LoopPolicy::Budget(mind_observability::BudgetKind::ResolveGrade),
        ])
        .count(verdicts)
        .wall_ms(now_ms().saturating_sub(rs_t0)),
    );
    rs_gate.advance(rs_decision)
}

/// Periodic profile refresh: re-crawl the registered personal seed so personal facts stay
/// current. Paced (YM_PROFILE_REFRESH_SECS, default ~3 days); one `learn_profile` model call
/// per period; beliefs dedupe/reinforce. A re-learn summary goes through the seam.
pub(crate) async fn run_profile_refresh(
    conv: &ConversationEngine,
    delivery: &Delivery,
    process_start_ms: u64,
    last_profile: u64,
) -> u64 {
    let period: u64 = std::env::var("YM_PROFILE_REFRESH_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(259_200);
    let now = now_ms();
    let pr_gate = mind_observability::Gated::timer(mind_observability::Timer {
        now_ms: now,
        last_ms: last_profile,
        period_ms: period * 1000,
    });
    let pr_decision = pr_gate.decide();
    if pr_decision != mind_observability::GateDecision::Act {
        return last_profile;
    }
    let pr_t0 = now_ms();
    let mut refreshed: u32 = 0;
    if let Some(update) = conv.refresh_profile().await {
        refreshed = 1;
        delivery
            .deliver(
                mind_observability::DeliveryKind::ProfileRefresh,
                &format!("🧭 Refreshed what I know about you:\n\n{update}"),
            )
            .await;
    }
    conv.record_loop_tick(
        mind_observability::LoopTick::acted(
            mind_observability::LoopOpportunity::Window {
                loop_id: mind_observability::LoopId::ProfileRefresh,
                process_start_ms,
                key: last_profile,
            },
            mind_observability::LoopHost::Process,
            mind_observability::LoopOutcome::Ran,
        )
        .considered(&[mind_observability::ConsideredSignal::Beliefs])
        .policy(&[
            mind_observability::LoopPolicy::Cadence(period),
            mind_observability::LoopPolicy::Budget(
                mind_observability::BudgetKind::ProfileLearnOneCall,
            ),
        ])
        .count(refreshed)
        .wall_ms(now_ms().saturating_sub(pr_t0)),
    );
    pr_gate.advance(pr_decision)
}

/// L3c-2: how long an engaging line waits for the cockpit before it expires unshown.
const ENGAGING_SHOW_BY_MS: u64 = 10 * 60 * 1000;

/// L3c-2: the engagement loops — the calibrated knock, the proactive digest, the get-to-know-you
/// ask — moved from the Telegram poll loop with their gates, considered sets, policy lines,
/// opportunity gates and cadence resets. What changed is stated: `chat_present` is
/// `has_presence()` (a pinned chat or an open cockpit); the idle input is the engine's turn
/// exclusion; `spoke` is "a proactive line was sent in the last ten minutes", set by this beat's
/// knock too; every line goes through the engaging door, so a Telegram send commits after the
/// API accepted it, a console line commits at `shown`, and no presence means nothing queued.
/// The knock's paired world-shadow sample records at its decision moment on EVERY box now.
pub(crate) async fn run_engagement(
    conv: &ConversationEngine,
    delivery: &Delivery,
    process_start_ms: u64,
    st: &mut RunnerState,
) {
    let proactive_on = std::env::var("YM_PROACTIVE")
        .map(|v| v != "off")
        .unwrap_or(true);
    let pd_secs: u64 = std::env::var("YM_PROACTIVE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(86_400);
    let ask_secs: u64 = std::env::var("YM_ASK_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(7_200);
    let now = now_ms();
    if !proactive_on {
        // L1 v3: disabled loops are observable — one held:disabled per due window.
        if now.saturating_sub(st.last_digest) >= pd_secs * 1000 {
            if let Some(window) =
                st.gate_digest
                    .take_window(LoopId::Digest, process_start_ms, st.last_digest)
            {
                conv.record_loop_tick(
                    mind_observability::LoopTick::held(
                        window,
                        mind_observability::LoopHost::Process,
                        HeldReason::Disabled,
                    )
                    .considered(&[
                        mind_observability::ConsideredSignal::Urges,
                        mind_observability::ConsideredSignal::Receptivity,
                    ])
                    .policy(&[mind_observability::LoopPolicy::Cadence(pd_secs)]),
                );
            }
        }
        if now.saturating_sub(st.last_ask) >= ask_secs * 1000 {
            if let Some(window) =
                st.gate_ask
                    .take_window(LoopId::Ask, process_start_ms, st.last_ask)
            {
                conv.record_loop_tick(
                    mind_observability::LoopTick::held(
                        window,
                        mind_observability::LoopHost::Process,
                        HeldReason::Disabled,
                    )
                    .considered(&[
                        mind_observability::ConsideredSignal::Name,
                        mind_observability::ConsideredSignal::Purpose,
                    ])
                    .policy(&[mind_observability::LoopPolicy::Cadence(ask_secs)]),
                );
            }
        }
        return;
    }
    let idle_secs: u64 = std::env::var("YM_DMN_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    // ONE reading of the user's clock per beat, used by the gates below AND handed to the
    // engine, so the Executive pane cannot show a different night from the one the arbiter
    // was given.
    let quiet_now = in_quiet_hours_now();
    conv.note_observed_quiet(quiet_now, quiet_hours_end_at_ms());
    let last_activity = conv.turns().last_user_activity_ms();
    let idle_stretch = now.saturating_sub(last_activity) >= idle_secs * 1000;
    let present = delivery.has_presence();
    let idle_ok = present && !quiet_now && idle_stretch;
    let mut spoke = conv.spoke_recently(SPOKE_WINDOW_MS).await;
    // The route is decided ONCE for this beat: every band, probability and marker below belongs
    // to the surface that will show the line; the door never falls back from a rejected send.
    let route = delivery.engaging_route(quiet_now);
    let telegram_now = route == EngagingRoute::Telegram;

    // THE CALIBRATED KNOCK goes FIRST — the highest-value thing the mind can say unprompted, and
    // capped at one per day inside its own evaluation. If it is delivered, nothing else speaks
    // this beat. Its opportunity is ONE IDLE STRETCH (keyed by the activity that opened it).
    let digest_considered = [
        mind_observability::ConsideredSignal::Urges,
        mind_observability::ConsideredSignal::Receptivity,
        mind_observability::ConsideredSignal::ExecutiveShadow,
    ];
    let digest_policy = [
        mind_observability::LoopPolicy::Cadence(pd_secs),
        mind_observability::LoopPolicy::Idle(idle_secs),
        mind_observability::LoopPolicy::Budget(mind_observability::BudgetKind::ReceptivityGate),
    ];
    // Atomic admission (the DMN/Patterns contract): the stateful work — a knock's evaluation,
    // the digest's generation, their deliveries — runs only when no turn is active, decided
    // under the exclusion's lock; a refusal is one held opportunity and moves no cadence.
    let permit = if idle_ok {
        conv.turns()
            .try_admit_background(now, idle_secs * 1000, BackgroundPass::Engagement)
    } else {
        None
    };
    let admitted = permit.is_some();
    if idle_ok && !admitted {
        if let Some(window) = st.gate_knock.take_stretch(LoopId::Knock, last_activity) {
            conv.record_loop_tick(
                mind_observability::LoopTick::held(
                    window,
                    mind_observability::LoopHost::Process,
                    HeldReason::IdleGate,
                )
                .considered(&[
                    mind_observability::ConsideredSignal::Packets,
                    mind_observability::ConsideredSignal::Receptivity,
                    mind_observability::ConsideredSignal::DailyCap,
                ])
                .policy(&[mind_observability::LoopPolicy::Idle(idle_secs)]),
            );
        }
    }
    if admitted {
        let t0 = now_ms();
        let mut knocked = false;
        let mut knock_held_no_presence = false;
        if let Some(candidate) = conv.prepare_knock().await {
            // The line is banded by the surface that will show it: Telegram's calibration when
            // a chat is reachable now, else the console's own (which may hold it below band).
            let ready = if telegram_now {
                Some((candidate.render(), candidate.clone()))
            } else {
                let console_p = conv.console_engagement_p().await;
                candidate.for_console(console_p).map(|c| (c.render(), c))
            };
            if let Some((text, banded)) = ready {
                if let Some(marker) = EngagementMarker::knock(
                    &banded.pkt_id,
                    mind_conversation::p_units(banded.p),
                    banded.band,
                    &banded.eval_id,
                ) {
                    match delivery
                        .deliver_engaging(
                            route,
                            DeliveryKind::Knock,
                            &text,
                            &marker,
                            now + ENGAGING_SHOW_BY_MS,
                            now_ms(),
                        )
                        .await
                    {
                        Delivered::TelegramAccepted { chars } => {
                            eprintln!("[knock] calibrated knock delivered ({chars} chars)");
                            conv.commit_knock(
                                &banded,
                                i64::try_from(now_ms()).unwrap_or(i64::MAX),
                                "telegram",
                            )
                            .await;
                            spoke = true;
                            knocked = true;
                        }
                        // Queued for the cockpit: it commits at `shown`, and marks nothing spoken.
                        Delivered::ConsoleQueued { .. } => knocked = true,
                        // The surface went stale between the gate and the door: the same
                        // opportunity is rendered held, never as an act.
                        Delivered::HeldNoPresence => knock_held_no_presence = true,
                        Delivered::Undelivered => {}
                    }
                }
            }
        }
        if knock_held_no_presence {
            if let Some(window) = st.gate_knock.take_stretch(LoopId::Knock, last_activity) {
                conv.record_loop_tick(
                    mind_observability::LoopTick::held(
                        window,
                        mind_observability::LoopHost::Process,
                        HeldReason::NoPresence,
                    )
                    .considered(&[
                        mind_observability::ConsideredSignal::Packets,
                        mind_observability::ConsideredSignal::Receptivity,
                        mind_observability::ConsideredSignal::DailyCap,
                    ])
                    .policy(&[mind_observability::LoopPolicy::Idle(idle_secs)]),
                );
            }
        }
        let first = !knock_held_no_presence
            && st
                .gate_knock
                .take_stretch(LoopId::Knock, last_activity)
                .is_some();
        if knocked || first {
            conv.record_loop_tick(
                mind_observability::LoopTick::acted(
                    mind_observability::LoopOpportunity::Stretch {
                        loop_id: LoopId::Knock,
                        start_ms: last_activity,
                    },
                    mind_observability::LoopHost::Process,
                    if knocked {
                        LoopOutcome::Knocked
                    } else {
                        LoopOutcome::Evaluated
                    },
                )
                .considered(&[
                    mind_observability::ConsideredSignal::Packets,
                    mind_observability::ConsideredSignal::Receptivity,
                    mind_observability::ConsideredSignal::DailyCap,
                ])
                .policy(&[
                    mind_observability::LoopPolicy::Idle(idle_secs),
                    mind_observability::LoopPolicy::Cap(mind_observability::CapKind::OnePerDay),
                ])
                .wall_ms(now_ms().saturating_sub(t0)),
            );
        }
    }

    // The proactive digest: its opportunity is its due window (keyed by the legacy timer).
    let digest_due = now.saturating_sub(st.last_digest) >= pd_secs * 1000;
    if digest_due && (spoke || !idle_ok || !admitted) {
        if let Some(window) =
            st.gate_digest
                .take_window(LoopId::Digest, process_start_ms, st.last_digest)
        {
            conv.record_loop_tick(
                mind_observability::LoopTick::held(
                    window,
                    mind_observability::LoopHost::Process,
                    if spoke {
                        HeldReason::SpokeAlready
                    } else if !present {
                        HeldReason::NoPresence
                    } else if quiet_now {
                        HeldReason::QuietHours
                    } else {
                        HeldReason::IdleGate
                    },
                )
                .considered(&digest_considered)
                .policy(&digest_policy),
            );
        }
    }
    if !spoke && idle_ok && admitted && digest_due {
        let t0 = now_ms();
        let digest_window = st.last_digest;
        // SHADOW ONLY. The return value must never reach control flow: the legacy gate below
        // stays authoritative for every send. Keyed on the window so re-evaluations collapse.
        let _shadow = conv
            .ex4_shadow_decide(digest_window as i64, quiet_now, quiet_hours_end_at_ms())
            .await;
        if conv.proactive_receptivity_ok().await {
            let mut outcome = LoopOutcome::NothingToSay;
            let mut digest_held_no_presence = false;
            if let Some(msg) = conv.proactive_digest().await {
                // The marker's probability is the console domain's; it is read only when the line is
                // queued for the cockpit (a Telegram send commits under its own domain).
                let p = conv.console_engagement_p().await;
                // The ref is this opportunity's: the text AND the window it was due in.
                let r#ref = mind_conversation::digest_ref_for(&msg, digest_window);
                let marker = EngagementMarker::digest_line(
                    r#ref.strip_prefix("digest:").unwrap_or(""),
                    mind_conversation::p_units(p),
                )
                .expect("a digest marker from a bounded ref");
                match delivery
                    .deliver_engaging(
                        route,
                        DeliveryKind::Digest,
                        &msg,
                        &marker,
                        now + ENGAGING_SHOW_BY_MS,
                        now_ms(),
                    )
                    .await
                {
                    Delivered::TelegramAccepted { chars } => {
                        eprintln!("[proactive] surfaced a digest ({chars} chars)");
                        let claim = conv.note_proactive_sent().await;
                        conv.ex4_shadow_note_legacy(
                            digest_window as i64,
                            LegacyOutcome::Sent,
                            Some(claim),
                        )
                        .await;
                        spoke = true;
                        outcome = LoopOutcome::DigestSent;
                    }
                    Delivered::ConsoleQueued { .. } => {
                        // Queued: the claim exists only once shown; the shadow's join key is the
                        // marker's ref, which the shown commit logs under.
                        conv.ex4_shadow_note_legacy(
                            digest_window as i64,
                            LegacyOutcome::Sent,
                            Some(marker.r#ref.clone()),
                        )
                        .await;
                        outcome = LoopOutcome::FoundQueued;
                    }
                    Delivered::HeldNoPresence => {
                        // Presence went stale between the gate and the door: the line is lost
                        // (the digest discharges what it renders) and the shadow cannot be
                        // compared against a display that never happened. The opportunity is
                        // rendered HELD, not acted; the cadence still resets (never hammer).
                        conv.ex4_shadow_note_legacy(
                            digest_window as i64,
                            LegacyOutcome::Undetermined,
                            None,
                        )
                        .await;
                        digest_held_no_presence = true;
                    }
                    Delivered::Undelivered => {
                        conv.ex4_shadow_note_legacy(
                            digest_window as i64,
                            LegacyOutcome::Undetermined,
                            None,
                        )
                        .await;
                        outcome = LoopOutcome::FoundUndelivered;
                    }
                }
            } else {
                // Gate passed and there was nothing to say — a real third case.
                conv.ex4_shadow_note_legacy(
                    digest_window as i64,
                    LegacyOutcome::NothingToSay,
                    None,
                )
                .await;
            }
            st.last_digest = now; // reset cadence whether or not we spoke (never hammer)
            st.gate_digest.mark(digest_window);
            let window = mind_observability::LoopOpportunity::Window {
                loop_id: LoopId::Digest,
                process_start_ms,
                key: digest_window,
            };
            conv.record_loop_tick(if digest_held_no_presence {
                mind_observability::LoopTick::held(
                    window,
                    mind_observability::LoopHost::Process,
                    HeldReason::NoPresence,
                )
                .considered(&digest_considered)
                .policy(&digest_policy)
                .wall_ms(now_ms().saturating_sub(t0))
            } else {
                mind_observability::LoopTick::acted(
                    window,
                    mind_observability::LoopHost::Process,
                    outcome,
                )
                .considered(&digest_considered)
                .policy(&digest_policy)
                .wall_ms(now_ms().saturating_sub(t0))
            });
        } else {
            // Declined. `proactive_digest()` never ran, so whether there was anything to say is
            // unknown by construction. Outcome stays CENSORED.
            conv.ex4_shadow_note_legacy(
                digest_window as i64,
                LegacyOutcome::DeclinedByReceptivity,
                None,
            )
            .await;
            if let Some(window) =
                st.gate_digest
                    .take_window(LoopId::Digest, process_start_ms, digest_window)
            {
                conv.record_loop_tick(
                    mind_observability::LoopTick::held(
                        window,
                        mind_observability::LoopHost::Process,
                        HeldReason::Receptivity,
                    )
                    .considered(&digest_considered)
                    .policy(&digest_policy)
                    .wall_ms(now_ms().saturating_sub(t0)),
                );
            }
        }
    }

    drop(permit);

    // The ask-drive: one get-to-know-you question per cadence while idle and receptive.
    let ask_idle: u64 = std::env::var("YM_ASK_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(120);
    let ask_ok = present && !quiet_now && now.saturating_sub(last_activity) >= ask_idle * 1000;
    let ask_on = std::env::var("YM_ASK").map(|v| v != "off").unwrap_or(true);
    let ask_due = now.saturating_sub(st.last_ask) >= ask_secs * 1000;
    let ask_considered = [
        mind_observability::ConsideredSignal::Name,
        mind_observability::ConsideredSignal::Purpose,
        mind_observability::ConsideredSignal::FollowUps,
        mind_observability::ConsideredSignal::Receptivity,
    ];
    let ask_policy = [
        mind_observability::LoopPolicy::Cadence(ask_secs),
        mind_observability::LoopPolicy::Idle(ask_idle),
        mind_observability::LoopPolicy::Budget(mind_observability::BudgetKind::ReceptivityGate),
        mind_observability::LoopPolicy::Cap(mind_observability::CapKind::OneOutstanding),
    ];
    // The ask admits on its own, under its own idle bound (the knock/digest permit is gone).
    let ask_permit = if !spoke && ask_on && ask_ok && ask_due {
        conv.turns()
            .try_admit_background(now, ask_idle * 1000, BackgroundPass::Engagement)
    } else {
        None
    };
    let ask_admitted = ask_permit.is_some();
    if ask_admitted && conv.proactive_receptivity_ok().await {
        let t0 = now_ms();
        let ask_window = st.last_ask;
        let mut outcome = LoopOutcome::NothingToAsk;
        let mut ask_held_no_presence = false;
        if let Some(candidate) = conv.prepare_ask().await {
            // The marker's probability is the console domain's; it is read only when the line is
            // queued for the cockpit (a Telegram send commits under its own domain).
            let p = conv.console_engagement_p().await;
            // The ref is this opportunity's: the slot (or `open`) and the window it was due in.
            let r#ref = mind_conversation::ask_ref_for(candidate.slot.as_deref(), ask_window);
            if let Some(marker) = EngagementMarker::ask(
                r#ref.strip_prefix("ask:").unwrap_or(""),
                mind_conversation::p_units(p),
            ) {
                match delivery
                    .deliver_engaging(
                        route,
                        DeliveryKind::Ask,
                        &candidate.text,
                        &marker,
                        now + ENGAGING_SHOW_BY_MS,
                        now_ms(),
                    )
                    .await
                {
                    Delivered::TelegramAccepted { .. } => {
                        eprintln!("[ask] posed a get-to-know-you question");
                        // The question is armed only after the send was accepted.
                        conv.commit_ask(&candidate).await;
                        conv.note_proactive_sent().await;
                        outcome = LoopOutcome::Asked;
                    }
                    Delivered::ConsoleQueued { .. } => outcome = LoopOutcome::FoundQueued,
                    Delivered::HeldNoPresence => ask_held_no_presence = true,
                    Delivered::Undelivered => outcome = LoopOutcome::FoundUndelivered,
                }
            }
        }
        st.last_ask = now; // reset cadence whether or not it asked
        st.gate_ask.mark(ask_window);
        let window = mind_observability::LoopOpportunity::Window {
            loop_id: LoopId::Ask,
            process_start_ms,
            key: ask_window,
        };
        conv.record_loop_tick(if ask_held_no_presence {
            mind_observability::LoopTick::held(
                window,
                mind_observability::LoopHost::Process,
                HeldReason::NoPresence,
            )
            .considered(&ask_considered)
            .policy(&ask_policy)
            .wall_ms(now_ms().saturating_sub(t0))
        } else {
            mind_observability::LoopTick::acted(
                window,
                mind_observability::LoopHost::Process,
                outcome,
            )
            .considered(&ask_considered)
            .policy(&ask_policy)
            .wall_ms(now_ms().saturating_sub(t0))
        });
    } else if ask_due {
        if let Some(window) = st
            .gate_ask
            .take_window(LoopId::Ask, process_start_ms, st.last_ask)
        {
            conv.record_loop_tick(
                mind_observability::LoopTick::held(
                    window,
                    mind_observability::LoopHost::Process,
                    if !ask_on {
                        HeldReason::Disabled
                    } else if spoke {
                        HeldReason::SpokeAlready
                    } else if !present {
                        HeldReason::NoPresence
                    } else if !ask_ok || !ask_admitted {
                        HeldReason::IdleGate
                    } else {
                        HeldReason::Receptivity
                    },
                )
                .considered(&ask_considered)
                .policy(&ask_policy),
            );
        }
    }
}

/// Pattern-finder surface — the "learn from memory" loop turned outward. On its own slow cadence
/// (default ~2 days), while idle, awake and with a surface to land on, run the cross-domain
/// pattern analysis (one grounded model call); it SAVES survivors as learned beliefs regardless,
/// but only SAYS something when it found a real, grounded one (the 💡 marker). Delivered to
/// Telegram it counts as spoken; queued for the console it does not.
pub(crate) async fn run_patterns(
    conv: &ConversationEngine,
    delivery: &Delivery,
    process_start_ms: u64,
    st: &mut RunnerState,
) {
    let pat_secs: u64 = std::env::var("YM_PATTERNS_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(172_800);
    let patterns_on = std::env::var("YM_PATTERNS")
        .map(|v| v != "off")
        .unwrap_or(true);
    let idle_secs: u64 = std::env::var("YM_DMN_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    let now = now_ms();
    let quiet_now = in_quiet_hours_now();
    let idle_stretch = now.saturating_sub(conv.turns().last_user_activity_ms()) >= idle_secs * 1000;
    let spoke = conv.spoke_recently(SPOKE_WINDOW_MS).await;
    let pat_gate = mind_observability::Gated::idle_gated(
        mind_observability::Timer {
            now_ms: now,
            last_ms: st.last_patterns,
            period_ms: pat_secs * 1000,
        },
        mind_observability::Presence {
            chat_present: delivery.has_surface(),
            quiet: quiet_now,
        },
        mind_observability::IdleInputs {
            enabled: patterns_on,
            spoke,
            idle: idle_stretch,
        },
    );
    let pat_decision = pat_gate.decide();
    let pat_considered = [
        mind_observability::ConsideredSignal::Beliefs,
        mind_observability::ConsideredSignal::Receptivity,
    ];
    let pat_policy = [
        mind_observability::LoopPolicy::Cadence(pat_secs),
        mind_observability::LoopPolicy::Idle(idle_secs),
        mind_observability::LoopPolicy::Budget(mind_observability::BudgetKind::PatternsOneCall),
    ];
    // The gate's idle input is a reading; admission is the decision. The permit is taken under
    // the engine's exclusive lock BEFORE the model call and held across the call and the
    // delivery, so a turn that registers first — a person's or a machine view's — always wins.
    let permit = if pat_decision == mind_observability::GateDecision::Act {
        conv.turns().try_admit_background(
            now,
            idle_secs * 1000,
            mind_conversation::turn_exclusion::BackgroundPass::Patterns,
        )
    } else {
        None
    };
    if let Some(_permit) = permit {
        let pat_t0 = now_ms();
        let pat_window = st.last_patterns;
        let msg = conv.find_patterns().await;
        let found = msg.starts_with('\u{1f4a1}');
        let outcome = if found {
            match delivery
                .deliver(mind_observability::DeliveryKind::Pattern, &msg)
                .await
            {
                Delivered::TelegramAccepted { chars } => {
                    eprintln!("[patterns] surfaced a learned pattern ({chars} chars)");
                    conv.note_proactive_sent().await;
                    mind_observability::LoopOutcome::Surfaced
                }
                Delivered::ConsoleQueued { .. } => mind_observability::LoopOutcome::FoundQueued,
                Delivered::Undelivered | Delivered::HeldNoPresence => {
                    mind_observability::LoopOutcome::FoundUndelivered
                }
            }
        } else {
            mind_observability::LoopOutcome::NothingFound
        };
        st.last_patterns = pat_gate.advance(pat_decision);
        st.gate_patterns.mark(pat_window);
        conv.record_loop_tick(
            mind_observability::LoopTick::acted(
                mind_observability::LoopOpportunity::Window {
                    loop_id: mind_observability::LoopId::Patterns,
                    process_start_ms,
                    key: pat_window,
                },
                mind_observability::LoopHost::Process,
                outcome,
            )
            .considered(&pat_considered)
            .policy(&pat_policy)
            .count(u32::from(found))
            .wall_ms(now_ms().saturating_sub(pat_t0)),
        );
    } else {
        // Held: the gate said so, or admission lost to a registered turn (idle-gate). Once per
        // window; the timer does not advance and the act window is not marked.
        let pat_reason = match pat_decision {
            mind_observability::GateDecision::Hold(reason) => reason,
            mind_observability::GateDecision::Act => mind_observability::HeldReason::IdleGate,
            // Not due: no opportunity exists, so nothing is recorded (legacy behaviour).
            mind_observability::GateDecision::NotDue => return,
        };
        if let Some(window) = st.gate_patterns.take_window(
            mind_observability::LoopId::Patterns,
            process_start_ms,
            st.last_patterns,
        ) {
            conv.record_loop_tick(
                mind_observability::LoopTick::held(
                    window,
                    mind_observability::LoopHost::Process,
                    pat_reason,
                )
                .considered(&pat_considered)
                .policy(&pat_policy),
            );
        }
    }
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

    /// The runner module sends nothing itself (every line goes through the delivery seam) and
    /// calls no model outside the four named passes; the bodies keep their gate kinds,
    /// considered sets, policy lines, ledger recording and timer transitions; the runner is
    /// serial, single-owner, latched, and delay-on-miss.
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
        let h = body.find("run_housekeeping(").unwrap();
        let i = body.find("run_ics(").unwrap();
        assert!(h < i, "housekeeping runs ahead of the loops");
        let l = body.find("run_lease_sweep(").unwrap();
        let r = body.find("run_resolve(").unwrap();
        let p = body.find("run_profile_refresh(").unwrap();
        let e = body.find("run_engagement(").unwrap();
        let pt = body.find("run_patterns(").unwrap();
        let d = body.find("run_dmn(").unwrap();
        assert!(i < l && l < r && r < p && p < e && e < pt && pt < d);
        // Each body records under the process host with its legacy kind and lines.
        for needle in [
            "mind_observability::Gated::timer(",
            "mind_observability::Gated::idle_gated(",
            "LoopId::Ics,",
            "LoopId::LeaseSweep,",
            "LoopId::Resolve,",
            "LoopId::ProfileRefresh,",
            "LoopId::Patterns,",
            "LoopId::Dmn,",
            "BudgetKind::DmnOneCall",
            "BudgetKind::ResolveGrade",
            "BudgetKind::ProfileLearnOneCall",
            "BudgetKind::PatternsOneCall",
            "ConsideredSignal::DueDelegations",
            "ics_gate.advance(ics_decision)",
            "ls_gate.advance(ls_decision)",
            "rs_gate.advance(rs_decision)",
            "pr_gate.advance(pr_decision)",
            "pat_gate.advance(pat_decision)",
            "try_admit_dmn(now, idle_secs * 1000)",
        ] {
            assert!(body.contains(needle), "{needle}");
        }
        assert!(!body.contains("LoopHost::Telegram") && !body.contains("LoopHost::Headless"));
        assert_eq!(
            body.matches("mind_observability::LoopHost::Process")
                .count(),
            20
        );
        // L3b: the model-call kill is exactly the four passes' own calls, each once; every
        // outward line is a `deliver`, and only a Telegram acceptance marks anything spoken.
        for call in [
            "conv.resolve_predictions(",
            "conv.refresh_profile(",
            "conv.find_patterns(",
            "conv.dmn_tick(",
            "conv.prepare_knock(",
            "conv.proactive_digest(",
            "conv.prepare_ask(",
        ] {
            assert_eq!(body.matches(call).count(), 1, "{call}");
        }
        assert_eq!(
            body.matches(".deliver(").count(),
            3,
            "three plain sends, all through the seam"
        );
        assert_eq!(
            body.matches(".deliver_engaging(").count(),
            3,
            "three engaging sends, all through the engaging door"
        );
        // Every "spoken" mark sits inside a TelegramAccepted arm: patterns, digest, ask.
        let mut marks = 0;
        let mut from = 0;
        while let Some(i) = body[from..].find("note_proactive_sent") {
            let at = from + i;
            assert!(
                body[at.saturating_sub(700)..at].contains("Delivered::TelegramAccepted"),
                "a spoken mark outside a TelegramAccepted arm at {at}"
            );
            marks += 1;
            from = at + 1;
        }
        assert_eq!(marks, 3, "patterns, digest, ask");
        assert!(
            !body.contains("commit_knock(&candidate") && body.contains("commit_knock(\n"),
            "the knock commits only from its accepted arm"
        );
        // L3c-2 (Codex's amend): the engagement loops admit atomically — the knock/digest work
        // and the ask each behind `try_admit_background(.., BackgroundPass::Engagement)` — the
        // route is decided once per beat, and the digest's and ask's refs carry their window.
        let eng = &body[body.find("pub(crate) async fn run_engagement(").unwrap()
            ..body.find("/// Pattern-finder surface").unwrap()];
        assert_eq!(eng.matches("BackgroundPass::Engagement").count(), 2);
        let admit1 = eng.find("BackgroundPass::Engagement").unwrap();
        let knock_call = eng.find("conv.prepare_knock(").unwrap();
        let digest_call = eng.find("conv.proactive_digest(").unwrap();
        assert!(admit1 < knock_call && knock_call < digest_call);
        let admit2 = eng.rfind("BackgroundPass::Engagement").unwrap();
        let ask_call = eng.find("conv.prepare_ask(").unwrap();
        assert!(digest_call < admit2 && admit2 < ask_call);
        assert!(
            eng.contains("drop(permit);"),
            "the first permit is released before the ask"
        );
        assert_eq!(
            eng.matches("delivery.engaging_route(").count(),
            1,
            "one route per beat"
        );
        assert!(
            !eng.contains("telegram_reachable()"),
            "the route, not the raw fact, decides"
        );
        assert!(eng.contains("digest_ref_for(&msg, digest_window)"));
        assert!(eng.contains("ask_ref_for(candidate.slot.as_deref(), ask_window)"));
        // Codex's addendum D: a surface gone stale at the door is ONE held:no-presence opportunity
        // for each of the three, never an act; `Undelivered` stays an act with its own outcome.
        for flag in [
            "knock_held_no_presence = true",
            "digest_held_no_presence = true",
            "ask_held_no_presence = true",
        ] {
            assert_eq!(eng.matches(flag).count(), 1, "{flag}");
        }
        assert_eq!(
            eng.matches("HeldReason::NoPresence").count(),
            5,
            "knock/digest/ask stale + the two gate holds"
        );
        assert!(!eng.contains("Delivered::Undelivered | Delivered::HeldNoPresence"));
        assert!(!eng.contains("Delivered::HeldNoPresence => {}"));
        // A refused admission holds and moves no cadence: the resets sit inside the admitted arms.
        let held_knock = &eng
            [eng.find("if idle_ok && !admitted {").unwrap()..eng.find("if admitted {").unwrap()];
        assert!(held_knock.contains("HeldReason::IdleGate") && !held_knock.contains("st.last_"));
        assert!(body.contains("chat_present: delivery.has_surface()"));
        // L3c: the stale resolvers have exactly one owner and it is timer-bound to a minute.
        assert_eq!(body.matches("conv.resolve_proactive(false)").count(), 1);
        assert_eq!(body.matches("conv.ledger_resolve(false)").count(), 1);
        assert!(body.contains("const HOUSEKEEPING_PERIOD_MS: u64 = 60_000;"));
        assert_eq!(body.matches("conv.reconcile_shown_engagements(").count(), 1);
        // L3b (Codex's second pass): Patterns admits atomically under the turn exclusion, takes
        // the permit BEFORE its model call, holds it across the call and the delivery, and on
        // refusal records one held:idle-gate without advancing or marking.
        let pat_start = body.find("pub(crate) async fn run_patterns(").unwrap();
        let pat_end = body.find("pub(crate) async fn run_dmn(").unwrap();
        let pat = &body[pat_start..pat_end];
        let admit = pat
            .find("try_admit_background(")
            .expect("patterns reaches the admission seam");
        let call = pat.find("conv.find_patterns(").unwrap();
        let deliver = pat.find(".deliver(").unwrap();
        assert!(admit < call && call < deliver);
        assert!(pat[admit..call].contains("if let Some(_permit) = permit {"));
        assert!(pat.contains("BackgroundPass::Patterns"));
        let held = &pat[pat.rfind("} else {").unwrap()..];
        assert!(held.contains("HeldReason::IdleGate"));
        assert!(!held.contains("pat_gate.advance(") && !held.contains("gate_patterns.mark("));
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
        let mut views = 0;
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
            views += prod.matches("conv.cli_dispatch_view(").count();
        }
        // The cockpit's automatic JSON refreshes are views: turns that do not move the clock.
        // The table is exact; a new polled route must be added here and to the engine's
        // allowlist together.
        let web = include_str!("web.rs");
        let web_prod = &web[..web.find("#[cfg(test)]").unwrap_or(web.len())];
        for view in [
            "\"jobs json\"",
            "\"horizons_json\"",
            "\"skills_json\"",
            "\"claims_json\"",
            "\"loops_json\"",
            "\"orders\",",
            "\"orders json\"",
            "&format!(\"horizon_history_json {id}\")",
        ] {
            let at = web_prod.find(view).unwrap_or_else(|| panic!("{view}"));
            let before = &web_prod[at.saturating_sub(120)..at];
            assert!(
                before.contains("cli_dispatch_view("),
                "{view} is a machine view"
            );
        }
        // The chains view builds its line into `verb` (an auditor-selected window) first.
        assert!(
            web_prod.contains("conv.cli_dispatch_view(&verb, "),
            "chains_json is a machine view"
        );
        assert_eq!(views, 9, "the nine read-only GET views, and no mutation");
        for mutation in [
            "import {doc}",
            "jobs {verb} {id}",
            "orders {verb} {id}",
            "plugin {verb} {id}",
        ] {
            let at = web_prod
                .find(mutation)
                .unwrap_or_else(|| panic!("{mutation}"));
            let before = &web_prod[at.saturating_sub(120)..at];
            assert!(
                !before.contains("cli_dispatch_view("),
                "{mutation} is a person's action"
            );
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
