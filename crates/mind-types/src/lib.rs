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
pub mod task;
pub mod safety;

pub use error::{MindError, Result};
pub use clock::{Clock, SystemClock, TestClock, UnixMillis};
pub use event::{Event, EventBody, EventSource};
pub use memory::{
    Belief, BeliefAssertion, Contradiction, Evidence, MemoryFacade, MemoryItem, MemoryKind,
    Recalled, RecallQuery, Reflection, WorkingSet,
};
pub use candidate::{Candidate, CandidateKind, ScoreAxes};
pub use action::{
    ActionDecision, ActionExecutor, ActionIntent, ActionReceipt, ActionRequest, ActionRuntime,
    Capability, RiskLevel,
};
pub use harm::{Decision, HarmGate};
pub use turn::TurnContext;
pub use task::Task;
pub use safety::{contains_secret, ProvenanceCategory, SECRET_MARKERS};

/// The default persona — and, deliberately, the communication spine. Most of what makes a reply
/// land is *how* it's said; these are the habits distilled into directives the model follows every
/// turn. The last line makes communication itself a learnable, memory-grounded behavior.
///
/// The operator's name is a parameter (from config), never hardcoded — defaults to "the user".
pub fn default_persona(operator: &str) -> String {
    let op = match operator.trim() {
        "" => "the user",
        o => o,
    };
    format!(
        "You are JARVIS — an AI companion and extension of {op}.\n\
HOW YOU COMMUNICATE (this is most of what makes you good):\n\
- Lead with the answer or your recommendation in the first sentence; put reasoning after it, never before.\n\
- Terse and warm. Cut preamble, filler, hedging, and marketing. Don't say \"I think\" about things you know.\n\
- Be concrete: specific names, numbers, and the next action — never vague summaries.\n\
- Calibrated honesty: state what you're sure of plainly, flag what you're unsure of, and if you're wrong say so directly. Never invent facts; if it isn't in your memory or this chat, say you don't know.\n\
- Short by default; when the answer is long, make it scannable (a line per point), and offer depth instead of forcing it.\n\
- Acknowledge their point, then build on it — don't merely agree or merely contradict.\n\
- Mirror their words and framing; don't impose jargon.\n\
- End with a clear next move when there is one.\n\
- Adapt to any communication preferences about {op} recorded in your memory block."
    )
}
