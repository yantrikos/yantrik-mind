//! Model-free continuity for goals that outlive one process or one planning horizon.
//!
//! A long-running goal needs more than a timer. Its checkpoint must carry the exact goal identity,
//! assumptions, plan revision, action ledger, and remaining budget; a changed assumption must block
//! further execution until a bounded replan; and completion must emit a verifiable outcome receipt.
//! This module owns those invariants without inference, I/O, or outward capabilities.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Millis;

const MAX_ID_BYTES: usize = 128;
const MAX_TEXT_BYTES: usize = 4_096;
const MAX_PLAN_STEPS: usize = 16;
const MAX_ASSUMPTIONS: usize = 64;
const MAX_ASSUMPTION_CHANGES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorizonBudget {
    pub max_actions: u32,
    pub max_replans: u32,
    pub max_cost_units: u64,
    /// Total elapsed time from the original start, including every interruption.
    pub max_elapsed_ms: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assumption {
    pub value: String,
    pub checked_at_ms: Millis,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssumptionChange {
    pub key: String,
    pub previous: String,
    pub observed: String,
    pub observed_at_ms: Millis,
    /// The plan revision that explicitly absorbed this change. `None` blocks more actions.
    pub addressed_by_revision: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionTrace {
    pub action_id: String,
    pub summary: String,
    pub at_ms: Millis,
    pub cost_units: u64,
    pub reversible: bool,
    /// Required for an irreversible action. This is an opaque receipt reference, never a secret.
    pub authorization_receipt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HorizonStatus {
    Active,
    AwaitingReplan,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorizonRun {
    pub goal_id: String,
    pub objective: String,
    pub status: HorizonStatus,
    pub started_at_ms: Millis,
    pub last_checkpoint_ms: Millis,
    pub completed_at_ms: Option<Millis>,
    pub plan_revision: u32,
    pub plan: Vec<String>,
    pub assumptions: BTreeMap<String, Assumption>,
    pub assumption_changes: Vec<AssumptionChange>,
    pub actions: Vec<ActionTrace>,
    pub spent_cost_units: u64,
    pub budget: HorizonBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalCheckpoint {
    pub goal_id: String,
    pub created_at_ms: Millis,
    /// A SNAPSHOT at `created_at_ms`, never current state.
    ///
    /// It carries the goal's own serialized fields, and those include a `status` that ages: a
    /// checkpoint on staging reads `active` for a goal the lifecycle has since recorded as failed
    /// and then expired. That is not a defect — the authority is `mind_horizon_jobs`, `list_horizons`
    /// joins its `status` from there, and this blob is read only by `HorizonRun::resume`, which the
    /// requeue path guards against terminal goals. But a field called `status` inside a durable blob
    /// reads as current to whoever opens it next, which is exactly how a stale value becomes a
    /// decision. Read state from the jobs table; read this only to resume.
    pub state_json: String,
    pub state_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeReceipt {
    pub goal_id: String,
    pub status: HorizonStatus,
    pub started_at_ms: Millis,
    pub finished_at_ms: Millis,
    pub actions: u32,
    pub replans: u32,
    pub spent_cost_units: u64,
    pub final_state_sha256: String,
    pub receipt_sha256: String,
}

/// An explicit operator intervention against a durable horizon goal.
///
/// These actions never rewrite the goal checkpoint. The receipt binds the exact checkpoint digest
/// and scheduler transition so a pause, resume, retry, or cancellation remains independently
/// auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HorizonControlAction {
    Pause,
    Resume,
    Retry,
    Cancel,
}

impl HorizonControlAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Retry => "retry",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorizonControlReceipt {
    pub goal_id: String,
    pub action: HorizonControlAction,
    pub occurred_at_ms: Millis,
    pub checkpoint_sha256: String,
    pub previous_queue_status: Option<String>,
    pub next_queue_status: Option<String>,
    pub receipt_sha256: String,
}

/// A scheduler-owned lifecycle transition for a durable horizon goal.
///
/// Unlike [`HorizonControlReceipt`], these transitions are not operator requests. They are emitted
/// by the durable scheduler itself and hash-chain to the preceding lifecycle receipt so a history
/// reader can detect tampering, deletion from the middle, or reordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HorizonLifecycleEvent {
    Scheduled,
    Recovered,
    WakeStarted,
    Failed,
    Completed,
    /// E.F2: a segment observed a declared assumption changing; the goal parked and left a
    /// claimable replan carrier in the queue.
    AwaitingReplan,
    /// E.F2: the replan branch took the claimed carrier for one numbered attempt.
    ReplanStarted,
    /// E.F2: the revised plan was validated and applied; a `scheduled` receipt follows in the
    /// same transaction.
    Replanned,
    /// E.F2: the lifecycle chain had a shape the reducer cannot authorise; terminal.
    ReplanIntegrityFailed,
    /// E.F3: the goal's elapsed-time budget ran out before its next segment; terminal. The
    /// checkpoint stays as history; the queue row is gone; nothing may claim or control it again.
    Expired,
}

impl HorizonLifecycleEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Recovered => "recovered",
            Self::WakeStarted => "wake_started",
            Self::Failed => "failed",
            Self::Completed => "completed",
            Self::AwaitingReplan => "awaiting_replan",
            Self::ReplanStarted => "replan_started",
            Self::Replanned => "replanned",
            Self::ReplanIntegrityFailed => "replan_integrity_failed",
            Self::Expired => "expired",
        }
    }
}

/// E.F2: what a replan-related lifecycle receipt carries beyond the queue transition. Never a
/// key or a value: the assumption is an opaque digest, the attempt a receipt ordinal, the chain
/// digest the malformed prefix an integrity failure was detected on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplanDetail {
    /// First 16 hex characters of SHA-256 over the assumption key; joins to the local run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assumption_id: Option<String>,
    /// The attempt ordinal (count of prior `replan_started` receipts + 1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// The plan revision this attempt produces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_revision: Option<u32>,
    /// Digest of the malformed chain prefix (integrity failures only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_digest: Option<String>,
}

impl ReplanDetail {
    pub fn awaiting(assumption_id: String) -> Self {
        Self {
            assumption_id: Some(assumption_id),
            attempt: None,
            target_revision: None,
            chain_digest: None,
        }
    }
    pub fn started(assumption_id: String, attempt: u32, target_revision: u32) -> Self {
        Self {
            assumption_id: Some(assumption_id),
            attempt: Some(attempt),
            target_revision: Some(target_revision),
            chain_digest: None,
        }
    }
    pub fn attempt_only(attempt: u32) -> Self {
        Self {
            assumption_id: None,
            attempt: Some(attempt),
            target_revision: None,
            chain_digest: None,
        }
    }
    pub fn integrity(chain_digest: String) -> Self {
        Self {
            assumption_id: None,
            attempt: None,
            target_revision: None,
            chain_digest: Some(chain_digest),
        }
    }
    fn valid_assumption_id(&self) -> bool {
        self.assumption_id
            .as_deref()
            .is_some_and(|id| id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()))
    }
}

/// E.F2: the opaque, bounded identity a receipt carries for an assumption key.
pub fn assumption_id(key: &str) -> String {
    sha256(key.as_bytes())[..16].to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HorizonLifecycleReceipt {
    pub goal_id: String,
    pub event: HorizonLifecycleEvent,
    pub occurred_at_ms: Millis,
    /// The exact active checkpoint digest, or the final-state digest for `completed`. Absent only
    /// when the lifecycle event itself records that the checkpoint was already missing.
    pub state_sha256: Option<String>,
    pub previous_queue_status: Option<String>,
    pub next_queue_status: Option<String>,
    /// Present only for `failed`; callers expose only their typed, bounded reason vocabulary.
    pub failure_reason: Option<String>,
    pub previous_receipt_sha256: Option<String>,
    /// E.F2: present only on replan-related events (and on an attempt-scoped `failed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replan: Option<ReplanDetail>,
    pub receipt_sha256: String,
}

impl HorizonLifecycleReceipt {
    #[allow(clippy::too_many_arguments)]
    pub fn issue(
        goal_id: impl Into<String>,
        event: HorizonLifecycleEvent,
        occurred_at_ms: Millis,
        state_sha256: Option<String>,
        previous_queue_status: Option<String>,
        next_queue_status: Option<String>,
        failure_reason: Option<String>,
        previous_receipt_sha256: Option<String>,
    ) -> Result<Self, HorizonError> {
        Self::issue_with_replan(
            goal_id,
            event,
            occurred_at_ms,
            state_sha256,
            previous_queue_status,
            next_queue_status,
            failure_reason,
            previous_receipt_sha256,
            None,
        )
    }

    /// E.F2: issue a receipt that carries replan detail. Receipts without detail digest exactly
    /// as before the amendment, so existing chains verify unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn issue_with_replan(
        goal_id: impl Into<String>,
        event: HorizonLifecycleEvent,
        occurred_at_ms: Millis,
        state_sha256: Option<String>,
        previous_queue_status: Option<String>,
        next_queue_status: Option<String>,
        failure_reason: Option<String>,
        previous_receipt_sha256: Option<String>,
        replan: Option<ReplanDetail>,
    ) -> Result<Self, HorizonError> {
        let mut receipt = Self {
            goal_id: goal_id.into(),
            event,
            occurred_at_ms,
            state_sha256,
            previous_queue_status,
            next_queue_status,
            failure_reason,
            previous_receipt_sha256,
            replan,
            receipt_sha256: String::new(),
        };
        if !receipt.valid_transition() {
            return Err(HorizonError::InvalidInput);
        }
        receipt.receipt_sha256 = lifecycle_digest(&receipt);
        Ok(receipt)
    }

    pub fn verify(&self) -> bool {
        self.valid_transition()
            && valid_sha256(&self.receipt_sha256)
            && self.receipt_sha256 == lifecycle_digest(self)
    }

    fn valid_transition(&self) -> bool {
        if !valid_id(&self.goal_id)
            || self
                .state_sha256
                .as_deref()
                .is_some_and(|digest| !valid_sha256(digest))
            || self
                .previous_queue_status
                .as_deref()
                .is_some_and(|status| !valid_queue_status(status))
            || self
                .next_queue_status
                .as_deref()
                .is_some_and(|status| !valid_queue_status(status))
            || self
                .failure_reason
                .as_deref()
                .is_some_and(|reason| !valid_id(reason))
            || self
                .previous_receipt_sha256
                .as_deref()
                .is_some_and(|digest| !valid_sha256(digest))
        {
            return false;
        }
        let replan = self.replan.as_ref();
        let no_replan = replan.is_none();
        match self.event {
            HorizonLifecycleEvent::Scheduled => {
                no_replan
                    && self.state_sha256.is_some()
                    && self.previous_queue_status.is_none()
                    && self.next_queue_status.as_deref() == Some("pending")
                    && self.failure_reason.is_none()
            }
            // E.F2 — parking: the running segment job becomes a pending replan carrier.
            HorizonLifecycleEvent::AwaitingReplan => {
                self.state_sha256.is_some()
                    && self.previous_queue_status.as_deref() == Some("running")
                    && self.next_queue_status.as_deref() == Some("pending")
                    && self.failure_reason.is_none()
                    && replan.is_some_and(|d| {
                        d.valid_assumption_id()
                            && d.attempt.is_none()
                            && d.target_revision.is_none()
                            && d.chain_digest.is_none()
                    })
            }
            // E.F2 — acquisition: the claimed carrier stays running under one numbered attempt.
            HorizonLifecycleEvent::ReplanStarted => {
                self.state_sha256.is_some()
                    && self.previous_queue_status.as_deref() == Some("running")
                    && self.next_queue_status.as_deref() == Some("running")
                    && self.failure_reason.is_none()
                    && replan.is_some_and(|d| {
                        d.valid_assumption_id()
                            && d.attempt.is_some_and(|a| a >= 1)
                            && d.target_revision.is_some_and(|r| r >= 1)
                            && d.chain_digest.is_none()
                    })
            }
            // E.F2 — success: the carrier is consumed; `scheduled` follows in the same transaction.
            HorizonLifecycleEvent::Replanned => {
                self.state_sha256.is_some()
                    && self.previous_queue_status.as_deref() == Some("running")
                    && self.next_queue_status.is_none()
                    && self.failure_reason.is_none()
                    && replan.is_some_and(|d| {
                        d.valid_assumption_id()
                            && d.attempt.is_some_and(|a| a >= 1)
                            && d.target_revision.is_some_and(|r| r >= 1)
                            && d.chain_digest.is_none()
                    })
            }
            // E.F3 — expiry: the last checkpoint's digest, the actual prior queue status (a
            // legacy goal may have none) to no queue row, the one bounded reason, terminal.
            HorizonLifecycleEvent::Expired => {
                no_replan
                    && self.state_sha256.is_some()
                    && matches!(
                        self.previous_queue_status.as_deref(),
                        None | Some("pending") | Some("failed") | Some("paused")
                    )
                    && self.next_queue_status.is_none()
                    && self.failure_reason.as_deref() == Some("budget_elapsed")
            }
            // E.F2 — integrity: terminal, no attempt, the malformed prefix's digest.
            HorizonLifecycleEvent::ReplanIntegrityFailed => {
                self.previous_queue_status.as_deref() == Some("running")
                    && self.next_queue_status.as_deref() == Some("failed")
                    && self.failure_reason.as_deref() == Some("replan_lifecycle_mismatch")
                    && replan.is_some_and(|d| {
                        d.assumption_id.is_none()
                            && d.attempt.is_none()
                            && d.target_revision.is_none()
                            && d.chain_digest.as_deref().is_some_and(valid_sha256)
                    })
            }
            HorizonLifecycleEvent::Recovered => {
                no_replan
                    && self.previous_queue_status.as_deref() == Some("running")
                    && self.next_queue_status.as_deref() == Some("pending")
                    && self.failure_reason.is_none()
            }
            HorizonLifecycleEvent::WakeStarted => {
                no_replan
                    && self.previous_queue_status.as_deref() == Some("pending")
                    && self.next_queue_status.as_deref() == Some("running")
                    && self.failure_reason.is_none()
            }
            HorizonLifecycleEvent::Failed => {
                // An attempt-scoped failure (E.F2) carries only the attempt it closes.
                replan.is_none_or(|d| {
                    d.attempt.is_some_and(|a| a >= 1)
                        && d.assumption_id.is_none()
                        && d.target_revision.is_none()
                        && d.chain_digest.is_none()
                }) && self.previous_queue_status.as_deref() == Some("running")
                    && self.next_queue_status.as_deref() == Some("failed")
                    && self.failure_reason.is_some()
                    && (self.state_sha256.is_some()
                        || self.failure_reason.as_deref() == Some("checkpoint_validation_failed"))
            }
            HorizonLifecycleEvent::Completed => {
                no_replan
                    && self.state_sha256.is_some()
                    && self.previous_queue_status.as_deref() == Some("running")
                    && self.next_queue_status.is_none()
                    && self.failure_reason.is_none()
            }
        }
    }
}

impl HorizonControlReceipt {
    pub fn issue(
        goal_id: impl Into<String>,
        action: HorizonControlAction,
        occurred_at_ms: Millis,
        checkpoint_sha256: impl Into<String>,
        previous_queue_status: Option<String>,
        next_queue_status: Option<String>,
    ) -> Result<Self, HorizonError> {
        let mut receipt = Self {
            goal_id: goal_id.into(),
            action,
            occurred_at_ms,
            checkpoint_sha256: checkpoint_sha256.into(),
            previous_queue_status,
            next_queue_status,
            receipt_sha256: String::new(),
        };
        if !receipt.valid_transition() {
            return Err(HorizonError::InvalidInput);
        }
        receipt.receipt_sha256 = control_digest(&receipt);
        Ok(receipt)
    }

    pub fn verify(&self) -> bool {
        self.valid_transition()
            && valid_sha256(&self.receipt_sha256)
            && self.receipt_sha256 == control_digest(self)
    }

    fn valid_transition(&self) -> bool {
        if !valid_id(&self.goal_id)
            || !valid_sha256(&self.checkpoint_sha256)
            || self
                .previous_queue_status
                .as_deref()
                .is_some_and(|status| !valid_queue_status(status))
            || self
                .next_queue_status
                .as_deref()
                .is_some_and(|status| !valid_queue_status(status))
        {
            return false;
        }
        match self.action {
            HorizonControlAction::Pause => {
                self.previous_queue_status.as_deref() == Some("pending")
                    && self.next_queue_status.as_deref() == Some("paused")
            }
            HorizonControlAction::Resume => {
                self.previous_queue_status.as_deref() == Some("paused")
                    && self.next_queue_status.as_deref() == Some("pending")
            }
            HorizonControlAction::Retry => {
                self.previous_queue_status.as_deref() == Some("failed")
                    && self.next_queue_status.as_deref() == Some("pending")
            }
            HorizonControlAction::Cancel => {
                self.previous_queue_status.as_deref() != Some("running")
                    && self.next_queue_status.is_none()
            }
        }
    }
}

impl OutcomeReceipt {
    pub fn verify(&self) -> bool {
        self.status == HorizonStatus::Completed
            && self.receipt_sha256
                == outcome_digest(
                    &self.goal_id,
                    self.started_at_ms,
                    self.finished_at_ms,
                    self.actions,
                    self.replans,
                    self.spent_cost_units,
                    &self.final_state_sha256,
                )
    }

    /// Verify both the receipt envelope and the exact completed state it claims to summarize.
    pub fn verify_state(&self, run: &HorizonRun) -> bool {
        if !self.verify()
            || run.goal_id != self.goal_id
            || run.status != HorizonStatus::Completed
            || run.started_at_ms != self.started_at_ms
            || run.completed_at_ms != Some(self.finished_at_ms)
            || run.actions.len() as u32 != self.actions
            || run.plan_revision != self.replans
            || run.spent_cost_units != self.spent_cost_units
        {
            return false;
        }
        serde_json::to_vec(run)
            .map(|state| sha256(&state) == self.final_state_sha256)
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HorizonError {
    InvalidInput,
    InvalidCheckpoint,
    ClockWentBackwards,
    BudgetExceeded,
    ReplanRequired,
    NoAssumptionChange,
    UnauthorizedIrreversibleAction,
    DuplicateAction,
    Terminal,
}

impl HorizonRun {
    pub fn start(
        goal_id: impl Into<String>,
        objective: impl Into<String>,
        plan: Vec<String>,
        assumptions: BTreeMap<String, String>,
        budget: HorizonBudget,
        now_ms: Millis,
    ) -> Result<Self, HorizonError> {
        let goal_id = goal_id.into();
        let objective = objective.into();
        if !valid_id(&goal_id)
            || !valid_text(&objective)
            || !valid_plan(&plan)
            || assumptions.len() > MAX_ASSUMPTIONS
            || budget.max_actions == 0
            || budget.max_elapsed_ms == 0
        {
            return Err(HorizonError::InvalidInput);
        }
        let mut checked_assumptions = BTreeMap::new();
        for (key, value) in assumptions {
            if !valid_id(&key) || !valid_text(&value) {
                return Err(HorizonError::InvalidInput);
            }
            checked_assumptions.insert(
                key,
                Assumption {
                    value,
                    checked_at_ms: now_ms,
                },
            );
        }
        Ok(Self {
            goal_id,
            objective,
            status: HorizonStatus::Active,
            started_at_ms: now_ms,
            last_checkpoint_ms: now_ms,
            completed_at_ms: None,
            plan_revision: 0,
            plan,
            assumptions: checked_assumptions,
            assumption_changes: Vec::new(),
            actions: Vec::new(),
            spent_cost_units: 0,
            budget,
        })
    }

    /// Record one already-governed action in the durable goal ledger.
    ///
    /// This does not execute the action. An irreversible action is representable only when its
    /// transaction-specific authorization receipt is already present.
    pub fn record_action(&mut self, action: ActionTrace) -> Result<(), HorizonError> {
        self.ensure_active()?;
        self.check_time(action.at_ms)?;
        if !valid_id(&action.action_id)
            || !valid_text(&action.summary)
            || action
                .authorization_receipt
                .as_deref()
                .is_some_and(|receipt| !valid_id(receipt))
        {
            return Err(HorizonError::InvalidInput);
        }
        if !action.reversible && action.authorization_receipt.is_none() {
            return Err(HorizonError::UnauthorizedIrreversibleAction);
        }
        if self
            .actions
            .iter()
            .any(|existing| existing.action_id == action.action_id)
        {
            return Err(HorizonError::DuplicateAction);
        }
        if self.actions.len() >= self.budget.max_actions as usize {
            return Err(HorizonError::BudgetExceeded);
        }
        let spent = self
            .spent_cost_units
            .checked_add(action.cost_units)
            .ok_or(HorizonError::BudgetExceeded)?;
        if spent > self.budget.max_cost_units {
            return Err(HorizonError::BudgetExceeded);
        }
        self.spent_cost_units = spent;
        self.actions.push(action);
        Ok(())
    }

    /// Recheck one declared assumption. A changed value immediately parks the run for replanning.
    pub fn observe_assumption(
        &mut self,
        key: &str,
        observed: impl Into<String>,
        now_ms: Millis,
    ) -> Result<bool, HorizonError> {
        if self.status == HorizonStatus::Completed {
            return Err(HorizonError::Terminal);
        }
        self.check_time(now_ms)?;
        let observed = observed.into();
        if !valid_id(key) || !valid_text(&observed) {
            return Err(HorizonError::InvalidInput);
        }
        let Some(assumption) = self.assumptions.get_mut(key) else {
            return Err(HorizonError::InvalidInput);
        };
        assumption.checked_at_ms = now_ms;
        if assumption.value == observed {
            return Ok(false);
        }
        if self.assumption_changes.len() >= MAX_ASSUMPTION_CHANGES {
            return Err(HorizonError::BudgetExceeded);
        }
        let previous = std::mem::replace(&mut assumption.value, observed.clone());
        self.assumption_changes.push(AssumptionChange {
            key: key.to_string(),
            previous,
            observed,
            observed_at_ms: now_ms,
            addressed_by_revision: None,
        });
        self.status = HorizonStatus::AwaitingReplan;
        Ok(true)
    }

    /// Replace the rolling plan after one or more assumption changes.
    pub fn replan(&mut self, plan: Vec<String>) -> Result<(), HorizonError> {
        if self.status == HorizonStatus::Completed {
            return Err(HorizonError::Terminal);
        }
        if self.status != HorizonStatus::AwaitingReplan
            || !self
                .assumption_changes
                .iter()
                .any(|change| change.addressed_by_revision.is_none())
        {
            return Err(HorizonError::NoAssumptionChange);
        }
        if !valid_plan(&plan) || self.plan_revision >= self.budget.max_replans {
            return Err(HorizonError::BudgetExceeded);
        }
        self.plan_revision += 1;
        for change in &mut self.assumption_changes {
            if change.addressed_by_revision.is_none() {
                change.addressed_by_revision = Some(self.plan_revision);
            }
        }
        self.plan = plan;
        self.status = HorizonStatus::Active;
        Ok(())
    }

    /// Produce a self-verifying, restart-safe snapshot.
    pub fn checkpoint(&mut self, now_ms: Millis) -> Result<GoalCheckpoint, HorizonError> {
        if self.status == HorizonStatus::Completed {
            return Err(HorizonError::Terminal);
        }
        self.check_time(now_ms)?;
        self.last_checkpoint_ms = now_ms;
        self.validate_state()?;
        let state_json =
            serde_json::to_string(self).map_err(|_| HorizonError::InvalidCheckpoint)?;
        Ok(GoalCheckpoint {
            goal_id: self.goal_id.clone(),
            created_at_ms: now_ms,
            state_sha256: sha256(state_json.as_bytes()),
            state_json,
        })
    }

    /// Restore only a checkpoint whose bytes, goal identity, and timestamp agree.
    pub fn resume(checkpoint: &GoalCheckpoint, now_ms: Millis) -> Result<Self, HorizonError> {
        if !valid_id(&checkpoint.goal_id)
            || !valid_sha256(&checkpoint.state_sha256)
            || sha256(checkpoint.state_json.as_bytes()) != checkpoint.state_sha256
        {
            return Err(HorizonError::InvalidCheckpoint);
        }
        let run: Self = serde_json::from_str(&checkpoint.state_json)
            .map_err(|_| HorizonError::InvalidCheckpoint)?;
        if run.goal_id != checkpoint.goal_id
            || run.last_checkpoint_ms != checkpoint.created_at_ms
            || run.completed_at_ms.is_some()
            || run.status == HorizonStatus::Completed
        {
            return Err(HorizonError::InvalidCheckpoint);
        }
        run.validate_state()
            .map_err(|_| HorizonError::InvalidCheckpoint)?;
        run.check_time(now_ms)?;
        Ok(run)
    }

    /// Terminate a satisfied run and return a receipt bound to its exact final state.
    pub fn complete(&mut self, now_ms: Millis) -> Result<OutcomeReceipt, HorizonError> {
        self.ensure_active()?;
        self.check_time(now_ms)?;
        self.status = HorizonStatus::Completed;
        self.completed_at_ms = Some(now_ms);
        let final_state = serde_json::to_vec(self).map_err(|_| HorizonError::InvalidCheckpoint)?;
        let final_state_sha256 = sha256(&final_state);
        let actions = self.actions.len() as u32;
        let replans = self.plan_revision;
        let receipt_sha256 = outcome_digest(
            &self.goal_id,
            self.started_at_ms,
            now_ms,
            actions,
            replans,
            self.spent_cost_units,
            &final_state_sha256,
        );
        Ok(OutcomeReceipt {
            goal_id: self.goal_id.clone(),
            status: self.status,
            started_at_ms: self.started_at_ms,
            finished_at_ms: now_ms,
            actions,
            replans,
            spent_cost_units: self.spent_cost_units,
            final_state_sha256,
            receipt_sha256,
        })
    }

    fn ensure_active(&self) -> Result<(), HorizonError> {
        match self.status {
            HorizonStatus::Active => Ok(()),
            HorizonStatus::AwaitingReplan => Err(HorizonError::ReplanRequired),
            HorizonStatus::Completed => Err(HorizonError::Terminal),
        }
    }

    fn check_time(&self, now_ms: Millis) -> Result<(), HorizonError> {
        if now_ms < self.started_at_ms || now_ms < self.latest_observed_ms() {
            return Err(HorizonError::ClockWentBackwards);
        }
        if now_ms.saturating_sub(self.started_at_ms) > self.budget.max_elapsed_ms {
            return Err(HorizonError::BudgetExceeded);
        }
        Ok(())
    }

    fn latest_observed_ms(&self) -> Millis {
        let action_ms = self
            .actions
            .last()
            .map(|action| action.at_ms)
            .unwrap_or(self.started_at_ms);
        let assumption_ms = self
            .assumptions
            .values()
            .map(|assumption| assumption.checked_at_ms)
            .max()
            .unwrap_or(self.started_at_ms);
        self.last_checkpoint_ms.max(action_ms).max(assumption_ms)
    }

    /// Reject a correctly hashed but structurally impossible snapshot.
    fn validate_state(&self) -> Result<(), HorizonError> {
        if !valid_id(&self.goal_id)
            || !valid_text(&self.objective)
            || !valid_plan(&self.plan)
            || self.assumptions.len() > MAX_ASSUMPTIONS
            || self.assumption_changes.len() > MAX_ASSUMPTION_CHANGES
            || self.budget.max_actions == 0
            || self.budget.max_elapsed_ms == 0
            || self.plan_revision > self.budget.max_replans
            || self.last_checkpoint_ms < self.started_at_ms
            || self.last_checkpoint_ms.saturating_sub(self.started_at_ms)
                > self.budget.max_elapsed_ms
        {
            return Err(HorizonError::InvalidCheckpoint);
        }
        if self.status == HorizonStatus::Completed || self.completed_at_ms.is_some() {
            return Err(HorizonError::InvalidCheckpoint);
        }

        for (key, assumption) in &self.assumptions {
            if !valid_id(key)
                || !valid_text(&assumption.value)
                || assumption.checked_at_ms < self.started_at_ms
                || assumption.checked_at_ms > self.last_checkpoint_ms
            {
                return Err(HorizonError::InvalidCheckpoint);
            }
        }

        let pending_changes = self
            .assumption_changes
            .iter()
            .filter(|change| change.addressed_by_revision.is_none())
            .count();
        if (pending_changes > 0) != (self.status == HorizonStatus::AwaitingReplan) {
            return Err(HorizonError::InvalidCheckpoint);
        }
        for change in &self.assumption_changes {
            if !valid_id(&change.key)
                || !valid_text(&change.previous)
                || !valid_text(&change.observed)
                || change.observed_at_ms < self.started_at_ms
                || change.observed_at_ms > self.last_checkpoint_ms
                || change
                    .addressed_by_revision
                    .is_some_and(|revision| revision == 0 || revision > self.plan_revision)
            {
                return Err(HorizonError::InvalidCheckpoint);
            }
        }

        if self.actions.len() > self.budget.max_actions as usize {
            return Err(HorizonError::InvalidCheckpoint);
        }
        let mut action_ids = BTreeSet::new();
        let mut cost = 0_u64;
        let mut previous_at_ms = self.started_at_ms;
        for action in &self.actions {
            if !valid_id(&action.action_id)
                || !action_ids.insert(action.action_id.as_str())
                || !valid_text(&action.summary)
                || action.at_ms < previous_at_ms
                || action.at_ms > self.last_checkpoint_ms
                || (!action.reversible && action.authorization_receipt.is_none())
                || action
                    .authorization_receipt
                    .as_deref()
                    .is_some_and(|receipt| !valid_id(receipt))
            {
                return Err(HorizonError::InvalidCheckpoint);
            }
            cost = cost
                .checked_add(action.cost_units)
                .ok_or(HorizonError::InvalidCheckpoint)?;
            previous_at_ms = action.at_ms;
        }
        if cost != self.spent_cost_units || cost > self.budget.max_cost_units {
            return Err(HorizonError::InvalidCheckpoint);
        }
        Ok(())
    }
}

fn outcome_digest(
    goal_id: &str,
    started_at_ms: Millis,
    finished_at_ms: Millis,
    actions: u32,
    replans: u32,
    spent_cost_units: u64,
    final_state_sha256: &str,
) -> String {
    let payload = serde_json::to_vec(&(
        goal_id,
        started_at_ms,
        finished_at_ms,
        actions,
        replans,
        spent_cost_units,
        final_state_sha256,
    ))
    .expect("outcome receipt tuple is serializable");
    sha256(&payload)
}

fn control_digest(receipt: &HorizonControlReceipt) -> String {
    let payload = serde_json::to_vec(&(
        &receipt.goal_id,
        receipt.action,
        receipt.occurred_at_ms,
        &receipt.checkpoint_sha256,
        &receipt.previous_queue_status,
        &receipt.next_queue_status,
    ))
    .expect("horizon control receipt tuple is serializable");
    sha256(&payload)
}

fn lifecycle_digest(receipt: &HorizonLifecycleReceipt) -> String {
    let payload = serde_json::to_vec(&(
        &receipt.goal_id,
        receipt.event,
        receipt.occurred_at_ms,
        &receipt.state_sha256,
        &receipt.previous_queue_status,
        &receipt.next_queue_status,
        &receipt.failure_reason,
        &receipt.previous_receipt_sha256,
    ))
    .expect("horizon lifecycle receipt tuple is serializable");
    match &receipt.replan {
        // Receipts without replan detail digest exactly as before E.F2.
        None => sha256(&payload),
        Some(detail) => {
            let mut bytes = payload;
            bytes.extend_from_slice(
                &serde_json::to_vec(detail).expect("replan detail is serializable"),
            );
            sha256(&bytes)
        }
    }
}

fn valid_queue_status(status: &str) -> bool {
    matches!(status, "pending" | "running" | "failed" | "paused")
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_TEXT_BYTES
}

fn valid_plan(plan: &[String]) -> bool {
    !plan.is_empty()
        && plan.len() <= MAX_PLAN_STEPS
        && plan.iter().all(|step| valid_text(step))
        && plan.iter().collect::<BTreeSet<_>>().len() == plan.len()
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: u64 = 60_000;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;

    fn action(id: &str, at_ms: u64, cost_units: u64) -> ActionTrace {
        ActionTrace {
            action_id: id.into(),
            summary: format!("completed {id}"),
            at_ms,
            cost_units,
            reversible: true,
            authorization_receipt: None,
        }
    }

    fn budget() -> HorizonBudget {
        HorizonBudget {
            max_actions: 4,
            max_replans: 1,
            max_cost_units: 10,
            max_elapsed_ms: 4 * DAY,
        }
    }

    #[test]
    fn simulated_goal_survives_three_horizons_replans_and_emits_an_outcome_receipt() {
        let start = 1_900_000_000_000;
        let mut assumptions = BTreeMap::new();
        assumptions.insert("source-schema".into(), "v1".into());
        let mut run = HorizonRun::start(
            "goal:three-horizons",
            "Produce a verified comparison under a fixed budget",
            vec!["collect".into(), "verify".into(), "synthesize".into()],
            assumptions,
            budget(),
            start,
        )
        .unwrap();

        run.record_action(action("collect-a", start + MINUTE, 2))
            .unwrap();
        let at_fifteen_minutes = run.checkpoint(start + 15 * MINUTE).unwrap();
        let wire = serde_json::to_string(&at_fifteen_minutes).unwrap();
        let decoded: GoalCheckpoint = serde_json::from_str(&wire).unwrap();
        let mut run = HorizonRun::resume(&decoded, start + 15 * MINUTE).unwrap();

        run.record_action(action("verify-a", start + HOUR, 2))
            .unwrap();
        let at_two_hours = run.checkpoint(start + 2 * HOUR).unwrap();
        let mut run = HorizonRun::resume(&at_two_hours, start + 2 * HOUR).unwrap();
        assert!(run
            .observe_assumption("source-schema", "v2", start + 2 * HOUR + 1)
            .unwrap());
        assert_eq!(run.status, HorizonStatus::AwaitingReplan);
        assert_eq!(
            run.record_action(action("must-not-run", start + 2 * HOUR + 2, 1)),
            Err(HorizonError::ReplanRequired)
        );

        // The pending drift itself survives a multi-day process interruption.
        let drift_checkpoint = run.checkpoint(start + 2 * HOUR + 3).unwrap();
        let mut run = HorizonRun::resume(&drift_checkpoint, start + 3 * DAY).unwrap();
        assert_eq!(run.status, HorizonStatus::AwaitingReplan);
        run.replan(vec![
            "validate-v2-schema".into(),
            "re-run-verification".into(),
            "synthesize".into(),
        ])
        .unwrap();
        run.record_action(action("verify-v2", start + 3 * DAY + 1, 3))
            .unwrap();
        let receipt = run.complete(start + 3 * DAY + 2).unwrap();

        assert_eq!(run.status, HorizonStatus::Completed);
        assert_eq!(receipt.actions, 3);
        assert_eq!(receipt.replans, 1);
        assert_eq!(receipt.spent_cost_units, 7);
        assert!(
            receipt.verify(),
            "the outcome envelope is internally consistent"
        );
        assert!(
            receipt.verify_state(&run),
            "the outcome is bound to the exact final state"
        );
        assert_eq!(run.assumption_changes.len(), 1);
        assert_eq!(run.assumption_changes[0].addressed_by_revision, Some(1));
        assert!(run.actions.iter().all(|trace| trace.reversible));
    }

    #[test]
    fn operator_control_receipts_bind_exact_scheduler_transitions() {
        let checkpoint = "a".repeat(64);
        let pause = HorizonControlReceipt::issue(
            "goal:controlled",
            HorizonControlAction::Pause,
            1_900_000_000_000,
            checkpoint.clone(),
            Some("pending".into()),
            Some("paused".into()),
        )
        .unwrap();
        assert!(pause.verify());

        let retry = HorizonControlReceipt::issue(
            "goal:controlled",
            HorizonControlAction::Retry,
            1_900_000_000_001,
            checkpoint.clone(),
            Some("failed".into()),
            Some("pending".into()),
        )
        .unwrap();
        assert!(retry.verify());
        assert!(HorizonControlReceipt::issue(
            "goal:controlled",
            HorizonControlAction::Retry,
            1_900_000_000_002,
            checkpoint.clone(),
            Some("paused".into()),
            Some("pending".into()),
        )
        .is_err());

        let mut tampered = pause.clone();
        tampered.next_queue_status = Some("pending".into());
        assert!(!tampered.verify());
        assert!(HorizonControlReceipt::issue(
            "goal:controlled",
            HorizonControlAction::Cancel,
            1_900_000_000_003,
            checkpoint,
            Some("running".into()),
            None,
        )
        .is_err());
    }

    #[test]
    fn scheduler_lifecycle_receipts_bind_transitions_and_chain_order() {
        let state = "b".repeat(64);
        let scheduled = HorizonLifecycleReceipt::issue(
            "goal:lifecycle",
            HorizonLifecycleEvent::Scheduled,
            1_900_000_000_000,
            Some(state.clone()),
            None,
            Some("pending".into()),
            None,
            None,
        )
        .unwrap();
        let failed = HorizonLifecycleReceipt::issue(
            "goal:lifecycle",
            HorizonLifecycleEvent::Failed,
            1_900_000_000_001,
            Some(state),
            Some("running".into()),
            Some("failed".into()),
            Some("segment_contract_failed".into()),
            Some(scheduled.receipt_sha256.clone()),
        )
        .unwrap();
        assert!(scheduled.verify());
        assert!(failed.verify());
        assert_eq!(
            failed.previous_receipt_sha256.as_deref(),
            Some(scheduled.receipt_sha256.as_str())
        );

        let mut tampered = failed.clone();
        tampered.failure_reason = Some("action_ledger_failed".into());
        assert!(!tampered.verify());
        assert!(HorizonLifecycleReceipt::issue(
            "goal:lifecycle",
            HorizonLifecycleEvent::Recovered,
            1_900_000_000_002,
            Some("c".repeat(64)),
            Some("failed".into()),
            Some("pending".into()),
            None,
            Some(failed.receipt_sha256),
        )
        .is_err());
    }

    #[test]
    fn checkpoints_and_irreversible_actions_fail_closed() {
        let start = 1_900_000_000_000;
        let mut run = HorizonRun::start(
            "goal:safety",
            "Keep authority outside the resumed state machine",
            vec!["prepare".into()],
            BTreeMap::new(),
            budget(),
            start,
        )
        .unwrap();
        let mut irreversible = action("broadcast", start + 1, 1);
        irreversible.reversible = false;
        assert_eq!(
            run.record_action(irreversible),
            Err(HorizonError::UnauthorizedIrreversibleAction)
        );

        let mut checkpoint = run.checkpoint(start + 2).unwrap();
        checkpoint.state_json.push(' ');
        assert_eq!(
            HorizonRun::resume(&checkpoint, start + 3),
            Err(HorizonError::InvalidCheckpoint)
        );

        // A digest is an integrity check, not permission to smuggle an impossible ledger. Even a
        // caller that recomputes it cannot resume a state whose accounting is false.
        let mut forged = run.clone();
        forged.spent_cost_units = 9;
        let state_json = serde_json::to_string(&forged).unwrap();
        let forged_checkpoint = GoalCheckpoint {
            goal_id: forged.goal_id.clone(),
            created_at_ms: forged.last_checkpoint_ms,
            state_sha256: sha256(state_json.as_bytes()),
            state_json,
        };
        assert_eq!(
            HorizonRun::resume(&forged_checkpoint, start + 3),
            Err(HorizonError::InvalidCheckpoint)
        );
    }
}
