//! `TurnContext` — the per-turn value threaded perceive→cognize→decide→act→learn. This replaces
//! a long-lived god-state: the loop carries a fresh `TurnContext`; state mutation happens inside
//! subsystems via their facades, never by the core reaching into fields.
use crate::candidate::Candidate;
use crate::clock::UnixMillis;
use crate::event::Event;
use crate::memory::WorkingSet;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnContext {
    pub trace_id: String,
    pub event: Event,
    pub working_set: WorkingSet,
    pub candidates: Vec<Candidate>,
    pub learnings: Vec<String>,
    pub started_ms: UnixMillis,
}

impl TurnContext {
    pub fn new(event: Event, started_ms: UnixMillis) -> Self {
        Self {
            trace_id: event.trace_id.clone(),
            event,
            working_set: WorkingSet::default(),
            candidates: Vec::new(),
            learnings: Vec::new(),
            started_ms,
        }
    }
}
