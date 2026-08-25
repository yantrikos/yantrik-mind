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
    let dec = |posture, interrupt, reason, monitor| ExecutiveDecision {
        candidate_id: c.candidate_id.clone(),
        posture,
        requires_user_interrupt: interrupt,
        reason_code: reason,
        monitor,
        evidence_refs: ev_refs.clone(),
    };

    // 1. Examined, nothing warrants revisiting: silence IS the decision.
    if c.already_resolved {
        return dec(Posture::Ignore, false, "already_resolved", None);
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
