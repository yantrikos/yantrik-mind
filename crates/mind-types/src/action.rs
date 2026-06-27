//! The Effect boundary. `ActionIntent` (carried on a `Candidate`) is only a *proposal*; the
//! `ActionRuntime` is the hardened *doing* boundary — capabilities, confirmation, idempotency,
//! audit receipts, rollback-where-possible. Tools and proactive both execute through it, and it
//! consults the `HarmGate` before doing anything.
use crate::error::Result;
use crate::turn::TurnContext;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskLevel {
    None,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Capability {
    ReadFs,
    WriteFs,
    Network,
    Exec,
    SendMessage,
    Memory,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionIntent {
    pub kind: String,
    /// The thing acted on (e.g. an email recipient, a repo, a path).
    pub target: String,
    /// Human-readable one-liner describing the action (shown when asking for confirmation).
    pub summary: String,
    /// The concrete content to act with (e.g. the email body), distinct from the human `summary`.
    pub payload: Option<String>,
    pub capabilities: Vec<Capability>,
    pub risk: RiskLevel,
    pub reversible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionRequest {
    pub id: String,
    pub actor: String,
    pub intent: ActionIntent,
    pub justification: String,
    pub created_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ActionDecision {
    Execute,
    RequireConfirmation { reason: String },
    Deny { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionReceipt {
    pub request_id: String,
    pub ok: bool,
    pub output: String,
    pub idempotency_key: String,
}

#[async_trait]
pub trait ActionRuntime: Send + Sync {
    async fn decide(&self, req: &ActionRequest, ctx: &TurnContext) -> ActionDecision;
    async fn execute(&self, req: ActionRequest) -> Result<ActionReceipt>;
}

/// The thing that actually performs an effect (send the email, post the comment). Injectable so the
/// runtime stays a leaf and tests use a scripted executor instead of touching the world. The runtime
/// only calls this AFTER the harm-gate + decision have passed — an executor never re-decides policy.
#[async_trait]
pub trait ActionExecutor: Send + Sync {
    /// Perform the effect for this request, returning a human-readable result string.
    async fn perform(&self, req: &ActionRequest) -> Result<String>;
}
