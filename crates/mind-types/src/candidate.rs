//! `Candidate` — the unit of "something the system might do or say" (a reply, a proactive
//! message, a tool action, a cortex proposal). One scoring path (7 axes), one delivery path.
//! Carries an optional `ActionIntent` the harm-gate can inspect.
use crate::action::ActionIntent;
use crate::clock::UnixMillis;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CandidateKind {
    Reply,
    Proactive,
    ToolAction,
    CortexProposal,
}

/// The 7 scoring axes (from the proven companion pipeline). `priority` is value×confidence,
/// urgency-bumped, penalised by annoyance risk — the only thing the governor needs to rank.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreAxes {
    pub urgency: f64,
    pub confidence: f64,
    pub interruptibility: f64,
    pub novelty: f64,
    pub expected_value: f64,
    pub annoyance_risk: f64,
    pub acceptance_rate: f64,
}

impl Default for ScoreAxes {
    fn default() -> Self {
        Self {
            urgency: 0.3,
            confidence: 0.7,
            interruptibility: 0.5,
            novelty: 0.5,
            expected_value: 0.5,
            annoyance_risk: 0.2,
            acceptance_rate: 0.5,
        }
    }
}

impl ScoreAxes {
    pub fn priority(&self) -> f64 {
        let base = self.expected_value * self.confidence * (1.0 + self.urgency);
        (base * (1.0 - 0.5 * self.annoyance_risk) * (0.5 + 0.5 * self.acceptance_rate)).max(0.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub kind: CandidateKind,
    pub why_now: String,
    pub content: String,
    pub intent: Option<ActionIntent>,
    pub axes: ScoreAxes,
    pub dedupe_key: String,
    pub created_ms: UnixMillis,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn priority_rewards_value_and_penalises_annoyance() {
        let high = ScoreAxes {
            expected_value: 0.9,
            confidence: 0.9,
            urgency: 0.5,
            annoyance_risk: 0.0,
            acceptance_rate: 1.0,
            ..Default::default()
        };
        let annoying = ScoreAxes {
            annoyance_risk: 0.9,
            ..high.clone()
        };
        assert!(high.priority() > annoying.priority());
        assert!(high.priority() > 0.0);
    }
}
