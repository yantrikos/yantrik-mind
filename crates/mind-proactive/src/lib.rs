//! mind-proactive - Detect->Generate->Score->Deliver pipeline + commitment ledger
//!
//! PHASE 3B EX1 (E.EX1): the posture vocabulary and ONE typed arbitration seam.
//! Deterministic rules only - no learned weights, no LLM, no expected-value formula.
//!
//! SEMANTICS (formal):
//!   IGNORE  = examined; no currently justified future cognition obligation
//!   MONITOR = no action now, BUT a future cognition obligation exists (wake condition REQUIRED)
//!   ACT     = a useful intervention is justified now, inside the current window
//!
//! ISOLATION RULE: the executive consumes OBSERVABLE candidate variables only. Oracle
//! outcome tables are evaluation ground truth and must never cross this boundary.
//!
//! Commitment/goal inputs arrive later as a normalized VIEW over authoritative organs;
//! this crate creates no fifth registry of "things Yantrik owes".

/// Posture vocabulary. NOT three levels of urgency (#formal semantics above).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Posture {
    Ignore,
    Monitor,
    Act,
}

/// What would cause reconsideration? Every MONITOR must answer this (#wake condition).
#[derive(Clone, Debug, PartialEq)]
pub enum WakeCondition {
    /// Reconsider when within N milliseconds of the deadline.
    DeadlineWithin(i64),
    /// Reconsider when this world-entity's state transitions.
    StateChangeOf(String),
    /// Reconsider when this source publishes fresher evidence.
    SourceFresh(String),
}

#[derive(Clone, Debug)]
pub struct MonitorPlan {
    pub review_at_ms: Option<i64>,
    pub wake_when: Vec<WakeCondition>,
}

/// The waist input: everything judgment may look at. All fields observable/warranted.
#[derive(Clone, Debug)]
pub struct ExecutiveCandidate {
    pub candidate_id: String,
    pub source_ref: String,
    pub now_ms: i64,
    /// Consequence severity if unhandled (0..=3), observable from facts.
    pub urgency: u8,
    pub deadline_at_ms: Option<i64>,
    pub already_resolved: bool,
    /// A safe/useful intervention EXISTS and Yantrik could execute it itself.
    pub useful_action_available: bool,
    pub internal_capability: bool,
    pub blocked: bool,
    pub waiting_on_someone: bool,
    pub intervention_window_open: bool,
    pub execution_cost: u8,
    pub interruption_cost: u8,
    pub risk: u8,
    pub confidence: f32,
    /// EX2: when useful action BECOMES justified. None = unknown (never invented).
    pub intervention_not_before_ms: Option<i64>,
    /// EX2: last moment the action is still preventative. None falls back to deadline.
    pub intervention_by_ms: Option<i64>,
    /// EX2: remaining-time threshold under which the user must be interrupted.
    pub interrupt_lead_ms: Option<i64>,
    /// EX3: this candidate IS an obligation, seen through a VIEW over authoritative organs.
    pub commitment: Option<CommitmentView>,
    /// EX3: observable environment fact - the nearest OTHER obligation converging now.
    pub converging_obligation_due_ms: Option<i64>,
    /// EX3: waiting on someone; until this instant their grace is respected.
    pub wait_grace_until_ms: Option<i64>,
    /// EX4: normalized VIEW over authoritative resource/receptivity state.
    pub resources: Option<ResourceContextView>,
}

/// EX4 doctrine: resource failure changes EXECUTABILITY, not importance;
/// receptivity changes DELIVERY STRATEGY, not world truth. View only - no registry.
#[derive(Clone, Debug)]
pub struct ResourceContextView {
    pub network_available: bool,
    pub capability_available: bool,
    pub budget_available: bool,
    /// None = receptivity unknown / not needed.
    pub user_receptive: Option<bool>,
    pub quiet_hours: bool,
    /// When quiet hours END, as an ABSOLUTE epoch-millisecond timestamp — not a duration.
    ///
    /// `arbitrate` copies this straight into `MonitorPlan.review_at_ms`, whose sibling construction
    /// is `Some(cm.due_at_ms - DAY_MS)` — plainly an instant. The unit was never written down here,
    /// and the live caller passed "milliseconds until quiet hours end" for as long as EX4-LIVE-A
    /// has been recording: every quiet-hours decision carried a review time a few hours after the
    /// 1970 epoch. Nothing acted on it (the executive is shadow-only) and nothing rendered it until
    /// `posture_json` did, which is how it was finally seen. Stated now so the next caller cannot
    /// make the same guess.
    pub quiet_hours_end_ms: Option<i64>,
}

/// Normalized view referencing an authoritative organ (task store / promise ledger).
/// The executive NEVER owns obligation truth - it only reads through this lens.
#[derive(Clone, Debug)]
pub struct CommitmentView {
    pub ref_id: String,
    pub source_organ: &'static str,
    pub made_at_ms: i64,
    pub due_at_ms: i64,
    pub fulfilled: bool,
}

#[derive(Clone, Debug)]
pub struct ExecutiveDecision {
    pub candidate_id: String,
    pub posture: Posture,
    pub requires_user_interrupt: bool,
    pub reason_code: &'static str,
    pub monitor: Option<MonitorPlan>,
    pub evidence_refs: Vec<String>,
}

const DAY_MS: i64 = 86_400_000;

/// THE SEAM. Total, deterministic, side-effect-free. Rules ordered; failures earn nuance.
pub fn arbitrate(c: &ExecutiveCandidate) -> ExecutiveDecision {
    let ev_refs = vec![c.source_ref.clone()];
    let dec = |posture, interrupt, reason, monitor| {
        let mut d = ExecutiveDecision {
            candidate_id: c.candidate_id.clone(),
            posture,
            requires_user_interrupt: interrupt,
            reason_code: reason,
            monitor,
            evidence_refs: ev_refs.clone(),
        };
        // EX4 conditioning pass: condition the JUSTIFIED posture on executability/delivery.
        if d.posture == Posture::Act {
            if let Some(r) = &c.resources {
                let block = if !r.capability_available {
                    Some(("capability_unavailable", WakeCondition::StateChangeOf("capability".into())))
                } else if !r.budget_available {
                    Some(("budget_unavailable", WakeCondition::StateChangeOf("budget".into())))
                } else if !r.network_available {
                    Some(("resource_unavailable", WakeCondition::StateChangeOf("network".into())))
                } else {
                    None
                };
                if let Some((rc, wake)) = block {
                    d.posture = Posture::Monitor;
                    d.requires_user_interrupt = false;
                    d.reason_code = rc;
                    d.monitor = Some(MonitorPlan { review_at_ms: None, wake_when: vec![wake] });
                    return d;
                }
                if d.requires_user_interrupt {
                    let imminent = c.deadline_at_ms
                        .map(|dl| c.urgency >= 3 && dl - c.now_ms <= c.interrupt_lead_ms.unwrap_or(4 * 3_600_000))
                        .unwrap_or(false);
                    if !imminent {
                        if r.quiet_hours {
                            d.posture = Posture::Monitor;
                            d.requires_user_interrupt = false;
                            d.reason_code = "quiet_hours";
                            d.monitor = Some(MonitorPlan {
                                review_at_ms: r.quiet_hours_end_ms,
                                wake_when: vec![WakeCondition::StateChangeOf("receptivity".into())],
                            });
                        } else if r.user_receptive == Some(false) {
                            d.posture = Posture::Monitor;
                            d.requires_user_interrupt = false;
                            d.reason_code = "user_unavailable";
                            d.monitor = Some(MonitorPlan {
                                review_at_ms: None,
                                wake_when: vec![WakeCondition::StateChangeOf("receptivity".into())],
                            });
                        }
                    } else {
                        d.reason_code = "critical_window_override";
                    }
                }
            }
        }
        d
    };

    // 1. Examined, nothing warrants revisiting: silence IS the decision.
    if c.already_resolved {
        return dec(Posture::Ignore, false, "already_resolved", None);
    }
    // ── EX3: obligations and waits (view-based; no registry owned here) ──
    if let Some(cm) = &c.commitment {
        if cm.fulfilled {
            return dec(Posture::Ignore, false, "commitment_fulfilled", None);
        }
        if c.useful_action_available && c.intervention_window_open && cm.due_at_ms - c.now_ms <= DAY_MS {
            return dec(Posture::Act, !c.internal_capability, "obligation_deadline_converging", None);
        }
        return dec(
            Posture::Monitor,
            false,
            "commitment_tracked",
            Some(MonitorPlan {
                review_at_ms: Some(cm.due_at_ms - DAY_MS),
                wake_when: vec![WakeCondition::DeadlineWithin(DAY_MS)],
            }),
        );
    }
    if c.waiting_on_someone {
        if let Some(grace) = c.wait_grace_until_ms {
            if c.now_ms < grace {
                // Their grace is respected even if a window technically exists - no nagging.
                return dec(
                    Posture::Monitor,
                    false,
                    "waiting_grace_open",
                    Some(MonitorPlan {
                        review_at_ms: Some(grace),
                        wake_when: vec![WakeCondition::StateChangeOf(c.source_ref.clone()), WakeCondition::DeadlineWithin(DAY_MS)],
                    }),
                );
            }
            if c.useful_action_available && c.intervention_window_open {
                return dec(Posture::Act, !c.internal_capability, "dependency_wait_elapsed", None);
            }
        }
    }
    if c.converging_obligation_due_ms.is_some() && c.urgency < 2 && c.deadline_at_ms.is_none() {
        return dec(Posture::Ignore, false, "yields_to_commitment", None);
    }
    if c.blocked {
        // Need persists; safe execution isn't justified NOW. Wake when resources return.
        return dec(
            Posture::Monitor,
            false,
            "execution_blocked",
            Some(MonitorPlan {
                review_at_ms: None,
                wake_when: vec![WakeCondition::SourceFresh(format!("resources:{}", c.candidate_id))],
            }),
        );
    }

    let horizon = c.deadline_at_ms.map(|d| d - c.now_ms);
    let near_deadline = horizon.map(|h| h <= 2 * DAY_MS).unwrap_or(false);

    // ── EX2 TEMPORAL ESCALATION (E.EX2): explicit window times govern when present.
    // Posture monotonicity is NOT guaranteed; justification at each evaluation time is.
    if let Some(d) = c.deadline_at_ms {
        if c.intervention_not_before_ms.is_none() && c.intervention_by_ms.is_none() && !c.intervention_window_open {
            // We know WHEN it must be done, not WHEN acting helps: do not invent a window.
            return dec(
                Posture::Monitor,
                false,
                "insufficient_timing_basis",
                Some(MonitorPlan {
                    review_at_ms: None,
                    wake_when: vec![
                        WakeCondition::StateChangeOf(format!("intervention_window:{}", c.candidate_id)),
                        WakeCondition::DeadlineWithin(DAY_MS),
                    ],
                }),
            );
        }
        if let Some(nb) = c.intervention_not_before_ms {
            if c.now_ms < nb {
                return dec(
                    Posture::Monitor,
                    false,
                    "too_early_wake_scheduled",
                    Some(MonitorPlan {
                        review_at_ms: Some(nb),
                        wake_when: vec![WakeCondition::DeadlineWithin(DAY_MS), WakeCondition::StateChangeOf(c.source_ref.clone())],
                    }),
                );
            }
        }
        if let Some(by) = c.intervention_by_ms {
            if c.now_ms > by {
                let reason = if c.now_ms > d { "deadline_missed_recovery" } else { "preventative_window_missed" };
                return dec(Posture::Act, !c.internal_capability || c.now_ms > d, reason, None);
            }
        }
        if c.useful_action_available && (c.intervention_window_open || c.intervention_not_before_ms.is_some() || c.intervention_by_ms.is_some()) {
            let interrupt = !c.internal_capability
                || c.interrupt_lead_ms.map(|lead| d - c.now_ms <= lead).unwrap_or(false);
            let reason = if interrupt { "user_action_required" } else { "prepare_internally" };
            return dec(Posture::Act, interrupt, reason, None);
        }
    }

    // 2. No justified intervention YET + a future trigger exists: intentional waiting.
    if !c.intervention_window_open || !c.useful_action_available {
        let mut wakes = vec![WakeCondition::DeadlineWithin(DAY_MS)];
        wakes.push(WakeCondition::StateChangeOf(c.source_ref.clone()));
        return dec(
            Posture::Monitor,
            false,
            "too_early_wake_scheduled",
            Some(MonitorPlan { review_at_ms: c.deadline_at_ms.map(|d| d - DAY_MS), wake_when: wakes }),
        );
    }

    // 3. Window open, useful action available: act; escalate to the user only if we cannot.
    let _ = near_deadline;
    dec(
        Posture::Act,
        !c.internal_capability,
        "intervention_window_open",
        None,
    )
}

#[cfg(test)]
mod ex1_tests {
    use super::*;

    fn base() -> ExecutiveCandidate {
        ExecutiveCandidate {
            candidate_id: "t".into(), source_ref: "world:t".into(), now_ms: 0,
            urgency: 2, deadline_at_ms: None, already_resolved: false,
            useful_action_available: false, internal_capability: false, blocked: false,
            waiting_on_someone: false, intervention_window_open: false,
            execution_cost: 1, interruption_cost: 2, risk: 1, confidence: 0.9,
            commitment: None, converging_obligation_due_ms: None, wait_grace_until_ms: None,
            resources: None,
            intervention_not_before_ms: None, intervention_by_ms: None, interrupt_lead_ms: None,
        }
    }

    #[test]
    fn resolved_is_silence_not_low_priority() {
        let mut c = base();
        c.already_resolved = true;
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Ignore);
        assert_eq!(d.reason_code, "already_resolved");
        assert!(d.monitor.is_none());
    }

    #[test]
    fn too_early_monitors_WITH_wake_condition() {
        let mut c = base();
        c.deadline_at_ms = Some(14 * DAY_MS);
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Monitor);
        let m = d.monitor.expect("MONITOR without a wake condition is procrastination");
        assert!(!m.wake_when.is_empty());
        assert!(m.wake_when.contains(&WakeCondition::DeadlineWithin(DAY_MS)));
    }

    #[test]
    fn open_window_with_useful_action_acts() {
        let mut c = base();
        c.useful_action_available = true;
        c.intervention_window_open = true;
        c.internal_capability = true;
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Act);
        assert!(!d.requires_user_interrupt);
    }

    #[test]
    fn act_without_internal_capability_interrupts() {
        let mut c = base();
        c.useful_action_available = true;
        c.intervention_window_open = true;
        c.internal_capability = false;
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Act);
        assert!(d.requires_user_interrupt);
    }

    #[test]
    fn blocked_demotes_to_monitor_and_keeps_the_need() {
        let mut c = base();
        c.useful_action_available = true;
        c.intervention_window_open = true;
        c.blocked = true;
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Monitor);
        assert!(d.monitor.unwrap().wake_when.iter().any(|w| matches!(w, WakeCondition::SourceFresh(_))));
    }
}

#[cfg(test)]
mod ex2_temporal_tests {
    use super::*;

    /// PHASE 3B EX2 RED SPEC - same unchanged situation; ONLY evaluation time moves.
    /// deadline=D; useful window opens D-14d... wait: opens D-9d, closes D; user-interrupt
    /// lead 4h. Posture must be independently justified at every instant (#justification,
    /// not monotonicity).
    fn cand(now_ms: i64) -> ExecutiveCandidate {
        let d = 30 * DAY_MS;
        ExecutiveCandidate {
            candidate_id: "curve".into(), source_ref: "world:event".into(), now_ms,
            urgency: 2, deadline_at_ms: Some(d), already_resolved: false,
            useful_action_available: true, internal_capability: true, blocked: false,
            waiting_on_someone: false, intervention_window_open: false,
            execution_cost: 1, interruption_cost: 2, risk: 1, confidence: 0.95,
            intervention_not_before_ms: Some(d - 9 * DAY_MS),
            intervention_by_ms: Some(d),
            interrupt_lead_ms: Some(4 * 3_600_000),
            commitment: None, converging_obligation_due_ms: None, wait_grace_until_ms: None,
            resources: None,
        }
    }

    #[test]
    fn boundaries_are_exact() {
        let d = 30 * DAY_MS;
        let nb = d - 9 * DAY_MS;
        // 1ms before window opens -> still waiting
        assert_eq!(arbitrate(&cand(nb - 1)).posture, Posture::Monitor);
        // at the exact opening ms -> justified intervention begins
        assert_eq!(arbitrate(&cand(nb)).posture, Posture::Act);
        // 1ms before interrupt lead -> internal
        let di = arbitrate(&cand(d - 4 * 3_600_000 - 1));
        assert_eq!(di.posture, Posture::Act);
        assert!(!di.requires_user_interrupt);
        // at the interrupt lead -> user needed
        let dii = arbitrate(&cand(d - 4 * 3_600_000));
        assert_eq!(dii.posture, Posture::Act);
        assert!(dii.requires_user_interrupt);
        // at deadline: window edge, recovery not yet claimed
        let dd = arbitrate(&cand(d));
        assert_eq!(dd.posture, Posture::Act);
        // 1ms past deadline: recovery, never ordinary act
        let dr = arbitrate(&cand(d + 1));
        assert_eq!(dr.posture, Posture::Act);
        assert_eq!(dr.reason_code, "deadline_missed_recovery");
        assert!(dr.requires_user_interrupt);
    }

    #[test]
    fn preventative_window_missed_but_deadline_alive() {
        let d = 30 * DAY_MS;
        let mut c = cand(d - 2 * 3_600_000);
        c.intervention_by_ms = Some(d - 6 * 3_600_000); // preventative prep closed earlier today
        let r = arbitrate(&c);
        assert_eq!(r.reason_code, "preventative_window_missed");
    }

    #[test]
    fn missing_window_data_is_never_invented() {
        let mut c = cand(0);
        c.intervention_not_before_ms = None;
        c.intervention_by_ms = None;
        c.intervention_window_open = false;
        let r = arbitrate(&c);
        assert_eq!(r.posture, Posture::Monitor);
        assert_eq!(r.reason_code, "insufficient_timing_basis");
        assert!(r.monitor.unwrap().wake_when.iter()
            .any(|w| matches!(w, WakeCondition::StateChangeOf(s) if s.contains("intervention_window"))));
    }

    #[test]
    fn justification_over_monotonicity() {
        // MONITOR early, then the issue resolves: posture legitimately REVERSES to IGNORE.
        assert_eq!(arbitrate(&cand(0)).posture, Posture::Monitor);
        let mut done = cand(5 * DAY_MS);
        done.already_resolved = true;
        assert_eq!(arbitrate(&done).posture, Posture::Ignore);
    }
}


#[cfg(test)]
mod ex3_commitment_tests {
    use super::*;

    /// PHASE 3B EX3 RED SPEC - obligations outrank opportunities; waits honor grace.
    /// CommitmentView is a NORMALIZED VIEW over authoritative organs, never a registry.
    fn cand() -> ExecutiveCandidate {
        ExecutiveCandidate {
            candidate_id: "c".into(), source_ref: "world:c".into(), now_ms: 0,
            urgency: 1, deadline_at_ms: None, already_resolved: false,
            useful_action_available: false, internal_capability: true, blocked: false,
            waiting_on_someone: false, intervention_window_open: false,
            execution_cost: 1, interruption_cost: 2, risk: 1, confidence: 0.95,
            intervention_not_before_ms: None, intervention_by_ms: None, interrupt_lead_ms: None,
            commitment: None, converging_obligation_due_ms: None, wait_grace_until_ms: None,
            resources: None,
        }
    }

    #[test]
    fn converging_promise_acts_internally() {
        let mut c = cand();
        c.commitment = Some(CommitmentView {
            ref_id: "task:form-42".into(), source_organ: "mind-tasks",
            made_at_ms: 0, due_at_ms: 90 * 60_000, fulfilled: false,
        });
        c.useful_action_available = true;
        c.intervention_window_open = true;
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Act);
        assert_eq!(d.reason_code, "obligation_deadline_converging");
        assert!(!d.requires_user_interrupt);
    }

    #[test]
    fn distant_promise_is_tracked_not_forgotten() {
        let mut c = cand();
        c.commitment = Some(CommitmentView {
            ref_id: "promise:call-mom".into(), source_organ: "promise-ledger",
            made_at_ms: 0, due_at_ms: 10 * 3_600_000, fulfilled: false,
        });
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Monitor);
        assert_eq!(d.reason_code, "commitment_tracked");
    }

    #[test]
    fn optional_opportunity_yields_to_converging_obligation() {
        let mut c = cand();
        c.converging_obligation_due_ms = Some(90 * 60_000);
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Ignore);
        assert_eq!(d.reason_code, "yields_to_commitment");
    }

    #[test]
    fn wait_grace_monitors_then_dependency_failure_acts() {
        let mut c = cand();
        c.waiting_on_someone = true;
        c.wait_grace_until_ms = Some(48 * 3_600_000);
        c.deadline_at_ms = Some(72 * 3_600_000);
        c.useful_action_available = true;
        c.intervention_window_open = true;
        c.internal_capability = false;
        assert_eq!(arbitrate(&c).posture, Posture::Monitor);
        let mut late = cand();
        late.waiting_on_someone = true;
        late.wait_grace_until_ms = Some(48 * 3_600_000);
        late.now_ms = 50 * 3_600_000;
        late.deadline_at_ms = Some(72 * 3_600_000);
        late.useful_action_available = true;
        late.intervention_window_open = true;
        late.internal_capability = false;
        let d = arbitrate(&late);
        assert_eq!(d.posture, Posture::Act);
        assert_eq!(d.reason_code, "dependency_wait_elapsed");
        assert!(d.requires_user_interrupt);
    }
}




#[cfg(test)]
mod ex4_resource_tests {
    use super::*;

    /// PHASE 3B EX4 RED SPEC - resource failure changes EXECUTABILITY, not importance;
    /// receptivity changes DELIVERY STRATEGY, not world truth. No monotonic assumption.
    fn res(net: bool, cap: bool, budget: bool, receptive: Option<bool>, quiet: bool) -> ResourceContextView {
        ResourceContextView {
            network_available: net, capability_available: cap, budget_available: budget,
            user_receptive: receptive, quiet_hours: quiet, quiet_hours_end_ms: None,
        }
    }
    fn cand() -> ExecutiveCandidate {
        ExecutiveCandidate {
            candidate_id: "r".into(), source_ref: "world:r".into(), now_ms: 0,
            urgency: 2, deadline_at_ms: Some(48 * 3_600_000), already_resolved: false,
            useful_action_available: true, internal_capability: true, blocked: false,
            waiting_on_someone: false, intervention_window_open: true,
            execution_cost: 1, interruption_cost: 2, risk: 1, confidence: 0.95,
            intervention_not_before_ms: None, intervention_by_ms: None, interrupt_lead_ms: None,
            commitment: None, converging_obligation_due_ms: None, wait_grace_until_ms: None,
            resources: None,
        }
    }

    #[test]
    fn capability_loss_demotes_act_to_monitor_with_wake() {
        let mut c = cand();
        c.resources = Some(res(false, false, true, None, false));
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Monitor);
        assert_eq!(d.reason_code, "capability_unavailable");
        assert!(d.monitor.unwrap().wake_when.iter().any(|w| matches!(w, WakeCondition::StateChangeOf(s) if s.contains("capability"))));
    }

    #[test]
    fn budget_loss_demotes_but_need_persists() {
        let mut c = cand();
        c.resources = Some(res(true, true, false, None, false));
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Monitor);
        assert_eq!(d.reason_code, "budget_unavailable");
    }

    #[test]
    fn network_return_restores_act_no_monotonicity() {
        let mut down = cand();
        down.resources = Some(res(false, true, true, None, false));
        assert_eq!(arbitrate(&down).posture, Posture::Monitor);
        let mut up = cand();
        up.resources = Some(res(true, true, true, None, false));
        up.internal_capability = false;
        let d = arbitrate(&up);
        assert_eq!(d.posture, Posture::Act);
        assert!(d.requires_user_interrupt);
    }

    #[test]
    fn unreceptive_user_leaves_internal_action_alone() {
        let mut c = cand();
        c.resources = Some(res(true, true, true, Some(false), true));
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Act);
        assert!(!d.requires_user_interrupt);
    }

    #[test]
    fn user_only_action_with_unreceptive_user_defers() {
        let mut c = cand();
        c.internal_capability = false;
        c.urgency = 1;
        c.resources = Some(res(true, true, true, Some(false), false));
        let d = arbitrate(&c);
        assert_eq!(d.posture, Posture::Monitor);
        assert_eq!(d.reason_code, "user_unavailable");
        // delivery strategy must actually move: a deferred interrupt is not pending
        assert!(!d.requires_user_interrupt);
    }

    #[test]
    fn quiet_hours_defer_non_urgent_but_critical_window_overrides() {
        let mut soft = cand();
        soft.internal_capability = false;
        soft.urgency = 1;
        soft.deadline_at_ms = Some(20 * 3_600_000);
        // ISOLATED pin: quiet hours only - receptivity left unknown
        soft.resources = Some(res(true, true, true, None, true));
        let dsoft = arbitrate(&soft);
        assert_eq!(dsoft.posture, Posture::Monitor);
        assert_eq!(dsoft.reason_code, "quiet_hours");
        assert!(!dsoft.requires_user_interrupt);
        let mut crit = cand();
        crit.internal_capability = false;
        crit.urgency = 3;
        crit.deadline_at_ms = Some(20 * 60_000);
        crit.interrupt_lead_ms = Some(4 * 3_600_000);
        crit.resources = Some(res(true, true, true, Some(false), true));
        let d = arbitrate(&crit);
        assert_eq!(d.posture, Posture::Act);
        assert_eq!(d.reason_code, "critical_window_override");
        assert!(d.requires_user_interrupt);
    }

    #[test]
    fn wake_causes_reconsideration_not_automatic_action() {
        // While blocked: Monitor every time (pure function, no hidden state).
        let mut blocked = cand();
        blocked.internal_capability = false;
        blocked.resources = Some(res(false, true, true, None, false));
        let m1 = arbitrate(&blocked);
        let m2 = arbitrate(&blocked);
        assert_eq!(m1.reason_code, "resource_unavailable");
        assert_eq!(m1.posture, Posture::Monitor);
        assert_eq!(m2.posture, Posture::Monitor);
        assert!(!m1.requires_user_interrupt && !m2.requires_user_interrupt);
        // The wake firing means RE-EVALUATION against current facts - which then
        // independently justifies ACT because the resource is now available.
        let mut restored = blocked.clone();
        restored.resources.as_mut().unwrap().network_available = true;
        let d = arbitrate(&restored);
        assert_eq!(d.posture, Posture::Act);
        // re-evaluation independently re-derives the reason - nothing was remembered
        assert_eq!(d.reason_code, "user_action_required");
        assert!(d.requires_user_interrupt);
    }
}


