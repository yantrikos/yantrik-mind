//! The harm-gate — the ONE inviolable wall. Deterministic, no-LLM (an LLM-evaluated gate is
//! rewritable by prompt injection), deny-by-default for the classes it governs, un-rewritable
//! (a compiled function with a frozen signature, not config). Both tools and proactive gate
//! through it. "No rope as long as it's not harming anyone."
use crate::action::ActionIntent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { reason: String },
    Modify { reason: String },
}

impl Decision {
    pub fn is_allow(&self) -> bool {
        matches!(self, Decision::Allow)
    }
}

pub trait HarmGate: Send + Sync {
    fn evaluate(&self, intent: &ActionIntent) -> Decision;
}
