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
    pub target: String,
    pub summary: String,
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
