//! mind-types — the narrow-waist contracts every module agrees on, and nothing else.
//!
//! No logic, no heavy deps: this crate exists so the rest of the system stays decoupled.
//! The six waist contracts are `Event`, `MemoryFacade`, `Candidate` (+`ActionIntent`),
//! `HarmGate`, `TurnContext`, `ActionRuntime`; plus the `Clock` seam for deterministic time.
//! See BUILD.md.

pub mod error;
pub mod clock;
pub mod event;
pub mod memory;
pub mod candidate;
pub mod action;
pub mod harm;
pub mod turn;

pub use error::{MindError, Result};
pub use clock::{Clock, SystemClock, TestClock, UnixMillis};
pub use event::{Event, EventBody, EventSource};
pub use memory::{
    Belief, BeliefAssertion, Contradiction, Evidence, MemoryFacade, MemoryItem, MemoryKind,
    Recalled, RecallQuery, Reflection, WorkingSet,
};
pub use candidate::{Candidate, CandidateKind, ScoreAxes};
pub use action::{
    ActionDecision, ActionIntent, ActionReceipt, ActionRequest, ActionRuntime, Capability,
    RiskLevel,
};
pub use harm::{Decision, HarmGate};
pub use turn::TurnContext;
