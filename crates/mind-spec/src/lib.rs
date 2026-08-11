//! mind-spec — the MODEL-FREE half of the cognitive loop.
//!
//! # Why this crate exists at all
//!
//! The runtime remembers the execution; the model does not. That one sentence is the architecture.
//! A classic agent loop threads its own transcript back through the model on every step, so by
//! iteration fifteen it is paying to re-read its own autobiography — and getting worse answers,
//! because the signal is buried in the replay. This crate is the alternative: a compact state
//! capsule the runtime owns and REPLACES each step, plus the decisions a computer should be making
//! anyway.
//!
//! # The boundary is the point
//!
//! **This crate has no inference dependency, and must never acquire one.** Counters, budgets,
//! deduplication, timeouts, thresholds, retry policy, and completion tests do not need a language
//! model, and every one of them is cheaper, faster, and more trustworthy as code. Putting them
//! behind a crate that *cannot* call a model turns "only invoke the LLM when semantic judgment is
//! actually needed" from a discipline into a property the compiler enforces.
//!
//! What genuinely needs a model — choosing the next action, interpreting an observation, resolving a
//! contradiction, synthesizing the answer — lives in `mind-agents`, which depends on this.
//!
//! # What is here
//!
//! - [`goal`] — a compiled goal and its CONTRACT. Completion is a test, not a feeling.
//! - [`capsule`] — the rolling state capsule and its reducer. Replaces, never appends.
//! - [`control`] — the deterministic controller: the decisions code should own.
//!
//! # An honesty rule this crate holds
//!
//! No field here holds a number a model guessed about the future. A model asked for the "expected
//! information gain" of an action it has not taken will return a plausible decimal, and a plausible
//! decimal is indistinguishable from a measured one once it is in a struct. So the model supplies
//! reason codes and observations; the runtime supplies arithmetic. Where a prior is unavoidable it is
//! named [`Prior`] and carries how it was obtained.

pub mod capsule;
pub mod control;
pub mod goal;

pub use capsule::{Capsule, Evidence, EvidenceRef, Observation, Progress, Uncertainty};
pub use control::{Controller, Decision, Limits, ReasonCode, StepOutcome};
pub use goal::{Budget, CompletionCriteria, Contract, GoalSpec, OutputContract, Verdict};

/// A number the runtime did not measure.
///
/// Some quantities genuinely have to start as estimates — how useful a tool is likely to be before
/// this mind has ever used it. Wrapping them keeps the estimate honest at every read: a caller can
/// see that `0.6` is a default someone chose rather than a rate observed over 40 runs, and the
/// observability layer can show it that way instead of implying measurement.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Prior {
    pub value: f64,
    pub basis: Basis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// A hardcoded starting point. Honest, but it has told you nothing yet.
    Declared,
    /// Computed from this mind's own recorded outcomes.
    Measured { runs: u32 },
    /// A model's estimate. Kept distinguishable precisely because it is the least trustworthy kind.
    Estimated,
}

impl Prior {
    pub fn declared(value: f64) -> Self {
        Self { value: value.clamp(0.0, 1.0), basis: Basis::Declared }
    }
    pub fn measured(value: f64, runs: u32) -> Self {
        Self { value: value.clamp(0.0, 1.0), basis: Basis::Measured { runs } }
    }
    pub fn estimated(value: f64) -> Self {
        Self { value: value.clamp(0.0, 1.0), basis: Basis::Estimated }
    }
    /// Is this backed by enough observation to act on without hedging?
    pub fn is_trustworthy(&self) -> bool {
        matches!(self.basis, Basis::Measured { runs } if runs >= 5)
    }
}

/// Milliseconds since the epoch. Passed in rather than read, so every decision in this crate is a
/// pure function of its inputs and a test can drive time forward without sleeping.
pub type Millis = u64;
