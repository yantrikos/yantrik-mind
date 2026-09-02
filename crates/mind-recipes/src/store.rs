//! Recipe persistence — durable run state in SQLite, lifted in spirit from the original engine's
//! `RecipeStore`. One row per run (status + current_step + steps + vars), so a run that was mid-
//! flight when the process died can be recovered. Its own connection to the DB file (separate from
//! the memory actor) keeps this a leaf — the recipe tables live alongside the cognitive ones.
//!
//! Crash discipline (carried over): on recovery, an interrupted step is re-run ONLY if it's
//! idempotent; a non-idempotent step (an `Act`/send) is failed-visibly, never blind-replayed.

use std::collections::HashMap;
use std::sync::Mutex;

use mind_spec::{
    bounded_notice_text, sha256_hex, EngagementMarker, NoticeEvent, NoticeKind, NoticeReceipt,
};
use mind_spec::{
    reduce_replan, GoalCheckpoint, HorizonControlAction, HorizonControlReceipt,
    HorizonLifecycleEvent, HorizonLifecycleReceipt, HorizonRun, OutcomeReceipt, ReplanAcquisition,
    ReplanBlock, ReplanDetail, ReplanIdentity,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::{HorizonJob, RecipeStep};

type HorizonStatusRow = (
    String,
    String,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
);

const LEGACY_HORIZON_FAILURE: &str = "legacy_unclassified_failure";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HorizonFailureReason {
    CheckpointValidation,
    SegmentToolExecution,
    SegmentReasoning,
    SegmentExecution,
    SegmentContract,
    ActionLedger,
    AssumptionObservation,
    StatePersistence,
    /// E.F2: the revised plan failed `HorizonJob::validate` (retryable).
    ReplanValidation,
    /// E.F2: `plan_revision >= max_replans` at acquisition (terminal).
    ReplanBudgetExhausted,
    /// E.F2: the planner call was lost, errored or produced nothing (retryable).
    ReplanPlanner,
    /// E.F2: the lifecycle chain had a shape the reducer cannot authorise (terminal).
    ReplanLifecycleMismatch,
    /// E.F3: the elapsed-time budget ran out before the next segment (terminal).
    BudgetElapsed,
}

impl HorizonFailureReason {
    /// E.F2: failures the operator retry control refuses. A retry cannot change the plan
    /// revision or the budget, and cannot repair a malformed chain; only a future preregistered
    /// control could.
    pub(crate) const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::ReplanBudgetExhausted | Self::ReplanLifecycleMismatch | Self::BudgetElapsed
        )
    }
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CheckpointValidation => "checkpoint_validation_failed",
            Self::SegmentToolExecution => "segment_tool_execution_failed",
            Self::SegmentReasoning => "segment_reasoning_failed",
            Self::SegmentExecution => "segment_execution_failed",
            Self::SegmentContract => "segment_contract_failed",
            Self::ActionLedger => "action_ledger_failed",
            Self::AssumptionObservation => "assumption_observation_failed",
            Self::StatePersistence => "state_persistence_failed",
            Self::ReplanValidation => "replan_validation_failed",
            Self::ReplanBudgetExhausted => "replan_budget_exhausted",
            Self::ReplanPlanner => "replan_planner_failed",
            Self::ReplanLifecycleMismatch => mind_spec::REPLAN_LIFECYCLE_MISMATCH,
            Self::BudgetElapsed => "budget_elapsed",
        }
    }

    /// Convert the durable recipe runner's failed-step pointer into a bounded diagnosis.
    ///
    /// The free-text backend error deliberately remains private. The step kind is code-owned and
    /// lets lifecycle receipts distinguish a connector/tool failure from local reasoning failure
    /// without persisting provider details, prompts, or tool output.
    pub(crate) fn from_failed_step(step: Option<&RecipeStep>) -> Self {
        match step {
            Some(RecipeStep::Tool { .. }) => Self::SegmentToolExecution,
            Some(RecipeStep::Think { .. } | RecipeStep::ThinkCited { .. }) => {
                Self::SegmentReasoning
            }
            Some(_) => Self::SegmentExecution,
            None => Self::SegmentExecution,
        }
    }
}

const HORIZON_FAILURE_CODES: &[&str] = &[
    HorizonFailureReason::CheckpointValidation.as_str(),
    HorizonFailureReason::SegmentToolExecution.as_str(),
    HorizonFailureReason::SegmentReasoning.as_str(),
    HorizonFailureReason::SegmentExecution.as_str(),
    HorizonFailureReason::SegmentContract.as_str(),
    HorizonFailureReason::ActionLedger.as_str(),
    HorizonFailureReason::AssumptionObservation.as_str(),
    HorizonFailureReason::StatePersistence.as_str(),
    HorizonFailureReason::ReplanValidation.as_str(),
    HorizonFailureReason::ReplanBudgetExhausted.as_str(),
    HorizonFailureReason::ReplanPlanner.as_str(),
    HorizonFailureReason::ReplanLifecycleMismatch.as_str(),
    HorizonFailureReason::BudgetElapsed.as_str(),
];

/// E.F2: the codes the operator retry control refuses (see `HorizonFailureReason::is_terminal`).
const TERMINAL_HORIZON_FAILURE_CODES: &[&str] = &[
    HorizonFailureReason::ReplanBudgetExhausted.as_str(),
    HorizonFailureReason::ReplanLifecycleMismatch.as_str(),
    HorizonFailureReason::BudgetElapsed.as_str(),
];

fn bounded_failure_reason(raw: Option<&str>) -> String {
    raw.filter(|reason| HORIZON_FAILURE_CODES.contains(reason))
        .unwrap_or(LEGACY_HORIZON_FAILURE)
        .to_string()
}

#[derive(Debug, Clone)]
pub struct RunRecord {
    pub id: String,
    pub name: String,
    pub status: String, // running | waiting | done | failed
    pub current_step: usize,
    pub steps: Vec<RecipeStep>,
    pub vars: HashMap<String, Value>,
    pub error: Option<String>,
    /// E.WEB19: typed run identity, persisted — never inferred from `name` at read time.
    /// `origin` ∈ imported_agent | scheduled_goal | other; `None` on legacy rows the backfill
    /// could not classify. `canonical_agent` is the exact agent name an imported order belongs to.
    pub origin: Option<String>,
    pub canonical_agent: Option<String>,
}

pub struct RecipeStore {
    conn: Mutex<Connection>,
}

pub struct ActiveHorizonRecord {
    pub run: HorizonRun,
    pub wake_at_ms: Option<u64>,
    pub queue_status: Option<String>,
    /// E.F3: a verified `expired` lifecycle event exists — terminal; listed outside the active
    /// heading and first, never scheduled, claimed or controlled again.
    pub expired: bool,
    /// A bounded code owned by the scheduler. Raw tool/backend errors never reach operator views.
    pub failure_reason: Option<String>,
}

/// L3b: a queued console notice as accounting — never the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedNotice {
    /// False when the dedupe key already named this notice: nothing was written this call.
    pub fresh: bool,
    pub notice_id: String,
    pub operator_id: String,
    pub kind: NoticeKind,
    pub created_ms: u64,
    pub chars: usize,
    pub text_sha256: String,
    /// L3c: the engagement marker for an engaging notice.
    pub marker: Option<EngagementMarker>,
    /// L3c: the show-by bound for an engaging notice.
    pub show_by_ms: Option<u64>,
}

/// L3c: what one acknowledgement established — the durable outbox record the engine commits
/// from. `shown_now` is false when this lease had already acknowledged (idempotent), in which
/// case `shown_ms` and `marker` are the ORIGINAL ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeAck {
    pub shown_now: bool,
    pub shown_ms: u64,
    pub kind: NoticeKind,
    pub marker: Option<EngagementMarker>,
}

/// L3c: a shown engaging notice, for the reconciler that finishes commits after a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShownEngagement {
    pub notice_id: String,
    pub shown_ms: u64,
    pub marker: EngagementMarker,
}

/// L3b: a notice handed to one cockpit under a lease; the text travels here and only here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeasedNotice {
    pub notice_id: String,
    pub lease_id: String,
    pub lease_until_ms: u64,
    pub kind: NoticeKind,
    pub text: String,
    pub created_ms: u64,
}

/// L3b: one notice with its verified receipt chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeHistoryEntry {
    pub notice: QueuedNotice,
    pub receipts: Vec<NoticeReceipt>,
}

/// E.F3: what one scheduler tick's atomic sweep-and-claim produced.
#[derive(Debug, Clone, Default)]
pub struct HorizonSweep {
    /// Goals expired on this tick, in id order; each has exactly one `expired` receipt.
    pub expired: Vec<String>,
    pub jobs: Vec<HorizonJob>,
}

impl RecipeStore {
    pub fn open(path: &str) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        let _ = conn.pragma_update(None, "journal_mode", "WAL");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mind_recipe_runs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                status TEXT NOT NULL,
                current_step INTEGER NOT NULL,
                steps_json TEXT NOT NULL,
                vars_json TEXT NOT NULL,
                error TEXT,
                updated_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS mind_horizon_checkpoints (
                goal_id TEXT PRIMARY KEY,
                checkpoint_json TEXT NOT NULL,
                state_sha256 TEXT NOT NULL,
                created_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS mind_horizon_outcomes (
                goal_id TEXT PRIMARY KEY,
                receipt_json TEXT NOT NULL,
                receipt_sha256 TEXT NOT NULL,
                completed_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS mind_horizon_jobs (
                goal_id TEXT PRIMARY KEY,
                job_json TEXT NOT NULL,
                wake_ms INTEGER NOT NULL,
                status TEXT NOT NULL,
                error TEXT
            );
            CREATE TABLE IF NOT EXISTS mind_horizon_controls (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                goal_id TEXT NOT NULL,
                action TEXT NOT NULL,
                receipt_json TEXT NOT NULL,
                receipt_sha256 TEXT NOT NULL UNIQUE,
                occurred_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS mind_horizon_lifecycle (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                goal_id TEXT NOT NULL,
                event TEXT NOT NULL,
                receipt_json TEXT NOT NULL,
                receipt_sha256 TEXT NOT NULL UNIQUE,
                occurred_ms INTEGER NOT NULL
            )",
        )?;
        // E.F3: one `expired` receipt per goal, enforced by the store itself.
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS mind_horizon_lifecycle_expired_once
                ON mind_horizon_lifecycle(goal_id) WHERE event='expired'",
        )?;
        // L3b: the console notice queue — notices plus their append-only receipts; one `shown`
        // per notice is the store's own rule.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS mind_notices (
                notice_id TEXT PRIMARY KEY,
                operator_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                created_ms INTEGER NOT NULL,
                text TEXT NOT NULL,
                text_sha256 TEXT NOT NULL,
                chars INTEGER NOT NULL,
                dedupe_key TEXT NOT NULL,
                UNIQUE (operator_id, dedupe_key)
            );
            CREATE TABLE IF NOT EXISTS mind_notice_receipts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                notice_id TEXT NOT NULL,
                operator_id TEXT NOT NULL,
                event TEXT NOT NULL,
                receipt_json TEXT NOT NULL,
                receipt_sha256 TEXT NOT NULL UNIQUE,
                occurred_ms INTEGER NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS mind_notice_shown_once
                ON mind_notice_receipts(notice_id) WHERE event='shown';",
        )?;
        // L3c: the engagement marker and the show-by bound, added to the L3b table in place.
        // Rows queued before this column exist keep validating under the L3b identity formula.
        let has_marker: bool = conn
            .prepare("PRAGMA table_info(mind_notices)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .iter()
            .any(|c| c == "engagement_json");
        if !has_marker {
            conn.execute_batch(
                "ALTER TABLE mind_notices ADD COLUMN engagement_json TEXT;
                 ALTER TABLE mind_notices ADD COLUMN show_by_ms INTEGER;",
            )?;
        }
        Self::migrate_run_identity(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn save(&self, r: &RunRecord, now_ms: u64) -> anyhow::Result<()> {
        let steps = serde_json::to_string(&r.steps)?;
        let vars = serde_json::to_string(&r.vars)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO mind_recipe_runs (id,name,status,current_step,steps_json,vars_json,error,updated_ms,origin,canonical_agent)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(id) DO UPDATE SET
                status=excluded.status, current_step=excluded.current_step, steps_json=excluded.steps_json,
                vars_json=excluded.vars_json, error=excluded.error, updated_ms=excluded.updated_ms,
                origin=COALESCE(excluded.origin, mind_recipe_runs.origin),
                canonical_agent=COALESCE(excluded.canonical_agent, mind_recipe_runs.canonical_agent)",
            rusqlite::params![r.id, r.name, r.status, r.current_step as i64, steps, vars, r.error, now_ms as i64, r.origin, r.canonical_agent],
        )?;
        Ok(())
    }

    pub fn set_status(&self, id: &str, status: &str, error: Option<&str>, now_ms: u64) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "UPDATE mind_recipe_runs SET status=?2, error=?3, updated_ms=?4 WHERE id=?1",
            rusqlite::params![id, status, error, now_ms as i64],
        );
    }

    /// Load a single run by id (any status) — used to resume a recipe waiting on an AskUser answer.
    /// E.WEB19: typed run identity, added as two nullable columns and backfilled ONCE.
    /// Idempotent by construction: the ALTERs run only when the column is absent, and the
    /// backfill touches only rows whose `origin` is still NULL — a second open changes nothing.
    /// Classification happens here and never again: an `import:` id with a `standing: ` name is
    /// an imported agent (canonical = the name after the prefix, taken now); a `sched:` id is a
    /// scheduled goal with no agent; anything else stays legacy/NULL. No rename, no id change.
    fn migrate_run_identity(conn: &Connection) -> anyhow::Result<()> {
        let mut have_origin = false;
        let mut have_agent = false;
        {
            let mut stmt = conn.prepare("PRAGMA table_info(mind_recipe_runs)")?;
            let cols = stmt.query_map([], |row| row.get::<_, String>(1))?;
            for c in cols.flatten() {
                if c == "origin" {
                    have_origin = true;
                }
                if c == "canonical_agent" {
                    have_agent = true;
                }
            }
        }
        if !have_origin {
            conn.execute("ALTER TABLE mind_recipe_runs ADD COLUMN origin TEXT", [])?;
        }
        if !have_agent {
            conn.execute(
                "ALTER TABLE mind_recipe_runs ADD COLUMN canonical_agent TEXT",
                [],
            )?;
        }
        conn.execute(
            "UPDATE mind_recipe_runs
                SET origin = 'imported_agent', canonical_agent = substr(name, 11)
              WHERE origin IS NULL AND id LIKE 'import:%' AND name LIKE 'standing: %'",
            [],
        )?;
        conn.execute(
            "UPDATE mind_recipe_runs SET origin = 'scheduled_goal'
              WHERE origin IS NULL AND id LIKE 'sched:%'",
            [],
        )?;
        Ok(())
    }

    pub fn load(&self, id: &str) -> Option<RunRecord> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id,name,status,current_step,steps_json,vars_json,error,origin,canonical_agent FROM mind_recipe_runs WHERE id=?1",
            [id],
            |row| {
                let steps_json: String = row.get(4)?;
                let vars_json: String = row.get(5)?;
                Ok(RunRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    status: row.get(2)?,
                    current_step: row.get::<_, i64>(3)? as usize,
                    steps: serde_json::from_str(&steps_json).unwrap_or_default(),
                    vars: serde_json::from_str(&vars_json).unwrap_or_default(),
                    error: row.get::<_, Option<String>>(6)?,
                    origin: row.get::<_, Option<String>>(7)?,
                    canonical_agent: row.get::<_, Option<String>>(8)?,
                })
            },
        )
        .ok()
    }

    /// Sleeping (persistent-delegation) runs whose wake time (`vars.__wake_at`) is due as of `now_ms`.
    /// The tick (`RecipeEngine::resume_due`) wakes these.
    pub fn due_sleeping(&self, now_ms: u64) -> Vec<RunRecord> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT id,name,status,current_step,steps_json,vars_json,error,origin,canonical_agent FROM mind_recipe_runs WHERE status='sleeping'",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map([], |row| {
            let steps_json: String = row.get(4)?;
            let vars_json: String = row.get(5)?;
            Ok(RunRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                status: row.get(2)?,
                current_step: row.get::<_, i64>(3)? as usize,
                steps: serde_json::from_str(&steps_json).unwrap_or_default(),
                vars: serde_json::from_str(&vars_json).unwrap_or_default(),
                error: row.get::<_, Option<String>>(6)?,
                origin: row.get::<_, Option<String>>(7)?,
                canonical_agent: row.get::<_, Option<String>>(8)?,
            })
        });
        match rows {
            Ok(it) => it
                .filter_map(|r| r.ok())
                .filter(|r| {
                    r.vars
                        .get("__wake_at")
                        .and_then(|v| v.as_u64())
                        .is_some_and(|w| w <= now_ms)
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Runs that were `running` when the process stopped — candidates for recovery.
    pub fn resumable(&self) -> Vec<RunRecord> {
        self.by_status("running")
    }

    /// Every run in one status. The single place the row→RunRecord mapping lives, so a schema change
    /// touches one query instead of three near-identical ones.
    pub fn by_status(&self, status: &str) -> Vec<RunRecord> {
        let conn = self.conn.lock().unwrap();
        let Ok(mut stmt) = conn.prepare(
            "SELECT id,name,status,current_step,steps_json,vars_json,error,origin,canonical_agent FROM mind_recipe_runs WHERE status=?1",
        ) else {
            return Vec::new();
        };
        let rows = stmt.query_map([status], |row| {
            let steps_json: String = row.get(4)?;
            let vars_json: String = row.get(5)?;
            Ok(RunRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                status: row.get(2)?,
                current_step: row.get::<_, i64>(3)? as usize,
                steps: serde_json::from_str(&steps_json).unwrap_or_default(),
                vars: serde_json::from_str(&vars_json).unwrap_or_default(),
                error: row.get::<_, Option<String>>(6)?,
                origin: row.get::<_, Option<String>>(7)?,
                canonical_agent: row.get::<_, Option<String>>(8)?,
            })
        });
        match rows {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Atomically insert or advance one verified long-horizon checkpoint.
    ///
    /// Older snapshots can arrive after a restart or retry; they are rejected instead of rolling
    /// the durable goal backwards. `HorizonRun::resume` validates both the digest and the state
    /// invariants before any bytes reach SQLite.
    pub fn save_horizon_checkpoint(&self, checkpoint: &GoalCheckpoint) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        Self::save_checkpoint_tx(&tx, checkpoint)?;
        tx.commit()?;
        Ok(())
    }

    /// The checkpoint upsert inside a caller-owned transaction (E.F2 parks and replans in one
    /// transaction with their queue and lifecycle writes).
    fn save_checkpoint_tx(
        tx: &rusqlite::Transaction<'_>,
        checkpoint: &GoalCheckpoint,
    ) -> anyhow::Result<()> {
        HorizonRun::resume(checkpoint, checkpoint.created_at_ms)
            .map_err(|error| anyhow::anyhow!("invalid horizon checkpoint: {error:?}"))?;
        if Self::verified_expired_on(tx, &checkpoint.goal_id)? {
            anyhow::bail!("an expired horizon goal is terminal: its checkpoint is history");
        }
        let checkpoint_json = serde_json::to_string(checkpoint)?;
        let created_ms = i64::try_from(checkpoint.created_at_ms)
            .map_err(|_| anyhow::anyhow!("horizon checkpoint timestamp is out of range"))?;
        let existing: Option<(i64, String)> = tx
            .query_row(
                "SELECT created_ms,state_sha256 FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [&checkpoint.goal_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((existing_ms, existing_sha256)) = existing {
            if existing_ms > created_ms
                || (existing_ms == created_ms && existing_sha256 != checkpoint.state_sha256)
            {
                anyhow::bail!(
                    "refused stale or conflicting horizon checkpoint for {}",
                    checkpoint.goal_id
                );
            }
        }
        tx.execute(
            "INSERT INTO mind_horizon_checkpoints
                (goal_id,checkpoint_json,state_sha256,created_ms)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(goal_id) DO UPDATE SET
                checkpoint_json=excluded.checkpoint_json,
                state_sha256=excluded.state_sha256,
                created_ms=excluded.created_ms",
            rusqlite::params![
                checkpoint.goal_id,
                checkpoint_json,
                checkpoint.state_sha256,
                created_ms
            ],
        )?;
        Ok(())
    }

    /// Load and independently verify the latest active state. Corruption is an error, never an
    /// empty/default run that could silently restart work with reset budgets.
    pub fn load_horizon(&self, goal_id: &str, now_ms: u64) -> anyhow::Result<Option<HorizonRun>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<(String, String)> = conn
            .query_row(
                "SELECT checkpoint_json,state_sha256
                 FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [goal_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((checkpoint_json, stored_sha256)) = raw else {
            return Ok(None);
        };
        let checkpoint: GoalCheckpoint = serde_json::from_str(&checkpoint_json)?;
        if checkpoint.goal_id != goal_id || checkpoint.state_sha256 != stored_sha256 {
            anyhow::bail!("horizon checkpoint identity or digest mismatch");
        }
        HorizonRun::resume(&checkpoint, now_ms)
            .map(Some)
            .map_err(|error| anyhow::anyhow!("horizon checkpoint failed validation: {error:?}"))
    }

    /// Strictly load every active goal plus its scheduler state for the operator control plane.
    /// Corruption is surfaced as an error; the status screen never silently hides a malformed row.
    pub fn list_horizons(&self, _now_ms: u64) -> anyhow::Result<Vec<ActiveHorizonRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT c.goal_id,c.checkpoint_json,c.state_sha256,j.wake_ms,j.status,j.error
             FROM mind_horizon_checkpoints c
             LEFT JOIN mind_horizon_jobs j ON j.goal_id=c.goal_id
             ORDER BY COALESCE(j.wake_ms,9223372036854775807),c.goal_id",
        )?;
        let rows: Vec<HorizonStatusRow> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut active = Vec::with_capacity(rows.len());
        for (goal_id, checkpoint_json, stored_sha256, wake_ms, queue_status, queue_error) in rows {
            let checkpoint: GoalCheckpoint = serde_json::from_str(&checkpoint_json)?;
            if checkpoint.goal_id != goal_id || checkpoint.state_sha256 != stored_sha256 {
                anyhow::bail!("horizon checkpoint identity or digest mismatch");
            }
            // Status inspection must remain possible after the elapsed-time budget expires. Resume
            // at the checkpoint's own timestamp for full structural/digest validation; the engine
            // separately marks an expired budget in its operator view.
            let run =
                HorizonRun::resume(&checkpoint, checkpoint.created_at_ms).map_err(|error| {
                    anyhow::anyhow!("horizon checkpoint failed validation: {error:?}")
                })?;
            if queue_status.as_deref().is_some_and(|status| {
                !matches!(status, "pending" | "running" | "failed" | "paused")
            }) {
                anyhow::bail!("horizon scheduler status failed validation");
            }
            let wake_at_ms = wake_ms
                .map(|wake| {
                    u64::try_from(wake)
                        .map_err(|_| anyhow::anyhow!("horizon wake timestamp is out of range"))
                })
                .transpose()?;
            let failure_reason = match queue_status.as_deref() {
                Some("failed") => Some(bounded_failure_reason(queue_error.as_deref())),
                _ if queue_error.is_none() => None,
                _ => anyhow::bail!("a non-failed horizon job carried a failure reason"),
            };
            let expired = Self::verified_expired_on(&conn, &goal_id)?;
            active.push(ActiveHorizonRecord {
                run,
                wake_at_ms,
                queue_status,
                expired,
                failure_reason,
            });
        }
        // E.F3: expired goals first (verified lifecycle), the wake order kept within each group.
        active.sort_by_key(|record| !record.expired);
        Ok(active)
    }

    /// Apply one exact operator control transition and append a receipt bound to the current
    /// checkpoint. Scheduler claims and controls share the same SQLite transaction boundary, so a
    /// running segment is never ambiguously paused or cancelled underneath its executor.
    pub fn control_horizon(
        &self,
        goal_id: &str,
        action: HorizonControlAction,
        now_ms: u64,
    ) -> anyhow::Result<HorizonControlReceipt> {
        let occurred_ms = i64::try_from(now_ms)
            .map_err(|_| anyhow::anyhow!("horizon control timestamp is out of range"))?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let raw_checkpoint: Option<(String, String)> = tx
            .query_row(
                "SELECT checkpoint_json,state_sha256
                 FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [goal_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((checkpoint_json, checkpoint_sha256)) = raw_checkpoint else {
            anyhow::bail!("no active horizon goal matches that exact id");
        };
        let checkpoint: GoalCheckpoint = serde_json::from_str(&checkpoint_json)?;
        if checkpoint.goal_id != goal_id || checkpoint.state_sha256 != checkpoint_sha256 {
            anyhow::bail!("horizon checkpoint identity or digest mismatch");
        }
        if Self::verified_expired_on(&tx, goal_id)? {
            anyhow::bail!("an expired horizon goal is terminal: no control applies");
        }
        HorizonRun::resume(&checkpoint, checkpoint.created_at_ms)
            .map_err(|error| anyhow::anyhow!("horizon checkpoint failed validation: {error:?}"))?;
        if now_ms < checkpoint.created_at_ms {
            anyhow::bail!("horizon control clock went backwards");
        }

        let previous_status: Option<String> = tx
            .query_row(
                "SELECT status FROM mind_horizon_jobs WHERE goal_id=?1",
                [goal_id],
                |row| row.get(0),
            )
            .optional()?;
        if previous_status
            .as_deref()
            .is_some_and(|status| !matches!(status, "pending" | "running" | "failed" | "paused"))
        {
            anyhow::bail!("horizon scheduler status failed validation");
        }

        let next_status = match action {
            HorizonControlAction::Pause => {
                if previous_status.as_deref() != Some("pending") {
                    anyhow::bail!("only a pending horizon goal can be paused");
                }
                Some("paused".to_string())
            }
            HorizonControlAction::Resume => {
                if previous_status.as_deref() != Some("paused") {
                    anyhow::bail!("only a paused horizon goal can be resumed");
                }
                // A resume is the only control that can restore execution authority. Revalidate
                // elapsed time at the operator's current clock before making the job claimable.
                HorizonRun::resume(&checkpoint, now_ms)
                    .map_err(|error| anyhow::anyhow!("horizon goal cannot resume: {error:?}"))?;
                Some("pending".to_string())
            }
            HorizonControlAction::Retry => {
                if previous_status.as_deref() != Some("failed") {
                    anyhow::bail!("only a failed horizon goal can be retried");
                }
                // E.F2: a terminal replan failure cannot be retried — a retry can change
                // neither the plan revision nor the budget, and cannot repair a malformed
                // lifecycle chain. The goal needs a preregistered reconciliation, not a retry.
                let failure_code: Option<String> = tx
                    .query_row(
                        "SELECT error FROM mind_horizon_jobs WHERE goal_id=?1",
                        [goal_id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .flatten();
                if failure_code
                    .as_deref()
                    .is_some_and(|code| TERMINAL_HORIZON_FAILURE_CODES.contains(&code))
                {
                    anyhow::bail!("terminal horizon failure is not retryable");
                }
                // Retry restores only scheduler eligibility. Revalidate the signed checkpoint and
                // elapsed-time budget at the operator's current clock; never rewrite either one.
                HorizonRun::resume(&checkpoint, now_ms)
                    .map_err(|error| anyhow::anyhow!("horizon goal cannot retry: {error:?}"))?;
                Some("pending".to_string())
            }
            HorizonControlAction::Cancel => {
                if previous_status.as_deref() == Some("running") {
                    anyhow::bail!("a running horizon segment cannot be cancelled mid-execution");
                }
                None
            }
        };
        let receipt = HorizonControlReceipt::issue(
            goal_id,
            action,
            now_ms,
            checkpoint_sha256,
            previous_status.clone(),
            next_status.clone(),
        )
        .map_err(|error| anyhow::anyhow!("horizon control rejected: {error:?}"))?;

        match action {
            HorizonControlAction::Pause
            | HorizonControlAction::Resume
            | HorizonControlAction::Retry => {
                let changed = tx.execute(
                    "UPDATE mind_horizon_jobs SET status=?2,error=NULL
                     WHERE goal_id=?1 AND status=?3",
                    rusqlite::params![goal_id, next_status, previous_status],
                )?;
                if changed != 1 {
                    anyhow::bail!("horizon control lost a scheduler race");
                }
            }
            HorizonControlAction::Cancel => {
                tx.execute("DELETE FROM mind_horizon_jobs WHERE goal_id=?1", [goal_id])?;
                let changed = tx.execute(
                    "DELETE FROM mind_horizon_checkpoints WHERE goal_id=?1",
                    [goal_id],
                )?;
                if changed != 1 {
                    anyhow::bail!("horizon cancellation lost its active checkpoint");
                }
            }
        }

        let receipt_json = serde_json::to_string(&receipt)?;
        tx.execute(
            "INSERT INTO mind_horizon_controls
                (goal_id,action,receipt_json,receipt_sha256,occurred_ms)
             VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                goal_id,
                action.as_str(),
                receipt_json,
                receipt.receipt_sha256,
                occurred_ms
            ],
        )?;
        tx.commit()?;
        Ok(receipt)
    }

    /// Read and verify the append-only control history for one goal.
    pub fn load_horizon_controls(
        &self,
        goal_id: &str,
    ) -> anyhow::Result<Vec<HorizonControlReceipt>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT action,receipt_json,receipt_sha256
             FROM mind_horizon_controls WHERE goal_id=?1 ORDER BY id",
        )?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([goal_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut receipts = Vec::with_capacity(rows.len());
        for (action, receipt_json, stored_sha256) in rows {
            let receipt: HorizonControlReceipt = serde_json::from_str(&receipt_json)?;
            if receipt.goal_id != goal_id
                || receipt.action.as_str() != action
                || receipt.receipt_sha256 != stored_sha256
                || !receipt.verify()
            {
                anyhow::bail!("horizon control receipt failed validation");
            }
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    /// Read and verify the scheduler-owned, hash-chained lifecycle for one goal.
    pub fn load_horizon_lifecycle(
        &self,
        goal_id: &str,
    ) -> anyhow::Result<Vec<HorizonLifecycleReceipt>> {
        let conn = self.conn.lock().unwrap();
        Self::load_lifecycle_on(&conn, goal_id)
    }

    fn load_lifecycle_on(
        conn: &rusqlite::Connection,
        goal_id: &str,
    ) -> anyhow::Result<Vec<HorizonLifecycleReceipt>> {
        let mut stmt = conn.prepare(
            "SELECT event,receipt_json,receipt_sha256
             FROM mind_horizon_lifecycle WHERE goal_id=?1 ORDER BY id",
        )?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([goal_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut receipts = Vec::with_capacity(rows.len());
        let mut previous_sha256: Option<String> = None;
        let mut expired_seen = false;
        for (event, receipt_json, stored_sha256) in rows {
            let receipt: HorizonLifecycleReceipt = serde_json::from_str(&receipt_json)?;
            // E.F3: the expiry receipt is terminal; a row after it is corruption, not history.
            if expired_seen {
                anyhow::bail!("horizon lifecycle receipt follows a terminal expiry");
            }
            if receipt.goal_id != goal_id
                || receipt.event.as_str() != event
                || receipt.receipt_sha256 != stored_sha256
                || receipt.previous_receipt_sha256 != previous_sha256
                || !receipt.verify()
                || (matches!(
                    receipt.event,
                    HorizonLifecycleEvent::Failed
                        | HorizonLifecycleEvent::ReplanIntegrityFailed
                        | HorizonLifecycleEvent::Expired
                ) && !receipt
                    .failure_reason
                    .as_deref()
                    .is_some_and(|reason| HORIZON_FAILURE_CODES.contains(&reason)))
            {
                anyhow::bail!("horizon lifecycle receipt chain failed validation");
            }
            previous_sha256 = Some(receipt.receipt_sha256.clone());
            expired_seen = receipt.event == HorizonLifecycleEvent::Expired;
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    #[allow(clippy::too_many_arguments)]
    fn append_horizon_lifecycle(
        tx: &rusqlite::Transaction<'_>,
        goal_id: &str,
        event: HorizonLifecycleEvent,
        occurred_at_ms: u64,
        state_sha256: Option<&str>,
        previous_queue_status: Option<&str>,
        next_queue_status: Option<&str>,
        failure_reason: Option<&str>,
    ) -> anyhow::Result<HorizonLifecycleReceipt> {
        Self::append_horizon_lifecycle_replan(
            tx,
            goal_id,
            event,
            occurred_at_ms,
            state_sha256,
            previous_queue_status,
            next_queue_status,
            failure_reason,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn append_horizon_lifecycle_replan(
        tx: &rusqlite::Transaction<'_>,
        goal_id: &str,
        event: HorizonLifecycleEvent,
        occurred_at_ms: u64,
        state_sha256: Option<&str>,
        previous_queue_status: Option<&str>,
        next_queue_status: Option<&str>,
        failure_reason: Option<&str>,
        replan: Option<ReplanDetail>,
    ) -> anyhow::Result<HorizonLifecycleReceipt> {
        if matches!(
            event,
            HorizonLifecycleEvent::Failed
                | HorizonLifecycleEvent::ReplanIntegrityFailed
                | HorizonLifecycleEvent::Expired
        ) && !failure_reason.is_some_and(|reason| HORIZON_FAILURE_CODES.contains(&reason))
        {
            anyhow::bail!("unbounded horizon lifecycle failure reason");
        }
        let previous: Option<(String, String)> = tx
            .query_row(
                "SELECT receipt_json,receipt_sha256 FROM mind_horizon_lifecycle
                 WHERE goal_id=?1 ORDER BY id DESC LIMIT 1",
                [goal_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let previous_receipt_sha256 = if let Some((receipt_json, stored_sha256)) = previous {
            let receipt: HorizonLifecycleReceipt = serde_json::from_str(&receipt_json)?;
            // E.F3: nothing is appended after the terminal expiry receipt.
            if receipt.event == HorizonLifecycleEvent::Expired {
                anyhow::bail!("an expired horizon goal is terminal: no lifecycle receipt follows");
            }
            if receipt.goal_id != goal_id
                || receipt.receipt_sha256 != stored_sha256
                || !receipt.verify()
            {
                anyhow::bail!("previous horizon lifecycle receipt failed validation");
            }
            Some(stored_sha256)
        } else {
            None
        };
        let receipt = HorizonLifecycleReceipt::issue_with_replan(
            goal_id,
            event,
            occurred_at_ms,
            state_sha256.map(str::to_string),
            previous_queue_status.map(str::to_string),
            next_queue_status.map(str::to_string),
            failure_reason.map(str::to_string),
            previous_receipt_sha256,
            replan,
        )
        .map_err(|error| anyhow::anyhow!("horizon lifecycle receipt rejected: {error:?}"))?;
        let receipt_json = serde_json::to_string(&receipt)?;
        let occurred_ms = i64::try_from(occurred_at_ms)
            .map_err(|_| anyhow::anyhow!("horizon lifecycle timestamp is out of range"))?;
        tx.execute(
            "INSERT INTO mind_horizon_lifecycle
                (goal_id,event,receipt_json,receipt_sha256,occurred_ms)
             VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![
                goal_id,
                event.as_str(),
                receipt_json,
                receipt.receipt_sha256,
                occurred_ms
            ],
        )?;
        Ok(receipt)
    }

    /// Atomically replace an active checkpoint with an immutable, verified completion receipt.
    /// A different receipt for the same goal can never overwrite the first terminal outcome.
    /// E.F2 parking, one transaction: the AwaitingReplan checkpoint, the running segment job
    /// replaced by a pending replan carrier, and the `awaiting_replan` receipt. Nothing here can
    /// land without the rest.
    pub(crate) fn park_horizon_for_replan(
        &self,
        checkpoint: &GoalCheckpoint,
        carrier: &HorizonJob,
        now_ms: u64,
        assumption_id: &str,
    ) -> anyhow::Result<()> {
        carrier.validate()?;
        if carrier.goal_id != checkpoint.goal_id {
            anyhow::bail!("replan carrier identity mismatch");
        }
        let carrier_json = serde_json::to_string(carrier)?;
        let wake_ms = i64::try_from(carrier.wake_at_ms)
            .map_err(|_| anyhow::anyhow!("horizon wake timestamp is out of range"))?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        Self::save_checkpoint_tx(&tx, checkpoint)?;
        let changed = tx.execute(
            "UPDATE mind_horizon_jobs SET job_json=?2,wake_ms=?3,status='pending',error=NULL
             WHERE goal_id=?1 AND status='running'",
            rusqlite::params![checkpoint.goal_id, carrier_json, wake_ms],
        )?;
        if changed != 1 {
            anyhow::bail!("horizon parking did not match one running job");
        }
        Self::append_horizon_lifecycle_replan(
            &tx,
            &checkpoint.goal_id,
            HorizonLifecycleEvent::AwaitingReplan,
            now_ms,
            Some(&checkpoint.state_sha256),
            Some("running"),
            Some("pending"),
            None,
            Some(ReplanDetail::awaiting(assumption_id.to_string())),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The queued job for a goal with its scheduler status and bounded failure code, if any.
    pub fn queued_horizon_job(
        &self,
        goal_id: &str,
    ) -> anyhow::Result<Option<(HorizonJob, String, Option<String>)>> {
        let conn = self.conn.lock().unwrap();
        let row: Option<(String, String, Option<String>)> = conn
            .query_row(
                "SELECT job_json,status,error FROM mind_horizon_jobs WHERE goal_id=?1",
                [goal_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        match row {
            Some((job_json, status, error)) => {
                let job: HorizonJob = serde_json::from_str(&job_json)?;
                Ok(Some((
                    job,
                    status,
                    error.map(|e| bounded_failure_reason(Some(&e))),
                )))
            }
            None => Ok(None),
        }
    }

    /// Test-only: put a queued job back to `running`, modelling a stale in-flight claimant that
    /// re-enters after the integrity transaction committed. Production has no such path.
    #[cfg(test)]
    pub(crate) fn force_job_running_for_test(&self, goal_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE mind_horizon_jobs SET status='running' WHERE goal_id=?1",
            [goal_id],
        )?;
        Ok(())
    }

    /// The active checkpoint's digest, the value every receipt on this goal must carry.
    pub(crate) fn active_checkpoint_sha256(&self, goal_id: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        Ok(conn
            .query_row(
                "SELECT state_sha256 FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [goal_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// E.F2 acquisition, one transaction: reduce the verified chain, decide the branch, and write
    /// exactly what that branch writes — `replan_started(attempt)` for an initial or bound retry
    /// acquisition, nothing for a crash resumption, and for any other shape the terminal
    /// integrity failure (receipt + failed job together; idempotent on re-entry).
    pub(crate) fn acquire_replan(
        &self,
        goal_id: &str,
        now_ms: u64,
        checkpoint_sha256: &str,
        assumption_id: &str,
        target_revision: u32,
    ) -> anyhow::Result<ReplanAcquisition> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        // The frozen acquisition reads its own evidence inside the transaction: the active
        // checkpoint must be AwaitingReplan with this digest, and the running job must be a
        // replan carrier with exactly the identity the caller claims. Nothing here is trusted
        // from the caller.
        let stored: Option<(String, String)> = tx
            .query_row(
                "SELECT checkpoint_json,state_sha256 FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [goal_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((checkpoint_json, stored_sha256)) = stored else {
            anyhow::bail!("replan acquisition found no active checkpoint");
        };
        if stored_sha256 != checkpoint_sha256 {
            anyhow::bail!("replan acquisition checkpoint digest mismatch");
        }
        let checkpoint: GoalCheckpoint = serde_json::from_str(&checkpoint_json)?;
        let run = HorizonRun::resume(&checkpoint, checkpoint.created_at_ms)
            .map_err(|error| anyhow::anyhow!("replan checkpoint failed validation: {error:?}"))?;
        let awaiting = run.status == mind_spec::HorizonStatus::AwaitingReplan;
        let running: Option<(String, String)> = tx
            .query_row(
                "SELECT job_json,status FROM mind_horizon_jobs WHERE goal_id=?1",
                [goal_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((job_json, job_status)) = running else {
            anyhow::bail!("replan acquisition found no queued job");
        };
        if job_status != "running" {
            anyhow::bail!("replan acquisition requires the claimed carrier to be running");
        }
        let job: HorizonJob = serde_json::from_str(&job_json)?;
        let carrier_matches = matches!(
            &job.kind,
            crate::HorizonJobKind::Replan {
                assumption_id: stored_id,
                target_revision: stored_revision,
            } if stored_id == assumption_id && *stored_revision == target_revision
        );
        let receipts = Self::load_lifecycle_on(&tx, goal_id)?;
        let chain = reduce_replan(&receipts);
        // Branch C's binding: the latest control receipt is a retry against THIS checkpoint
        // digest, failed -> pending, issued at or after the latest attempt-scoped failure.
        let last_failed_ms = receipts
            .iter()
            .rev()
            .find(|r| r.event == HorizonLifecycleEvent::Failed)
            .map(|r| r.occurred_at_ms);
        let latest_control: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT action,receipt_json,occurred_ms FROM mind_horizon_controls
                 WHERE goal_id=?1 ORDER BY id DESC LIMIT 1",
                [goal_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let retry_bound = match (latest_control, last_failed_ms) {
            (Some((action, receipt_json, occurred_ms)), Some(failed_ms)) if action == "retry" => {
                let receipt: HorizonControlReceipt = serde_json::from_str(&receipt_json)?;
                receipt.verify()
                    && receipt.goal_id == goal_id
                    && receipt.checkpoint_sha256 == checkpoint_sha256
                    && receipt.previous_queue_status.as_deref() == Some("failed")
                    && receipt.next_queue_status.as_deref() == Some("pending")
                    && u64::try_from(occurred_ms).unwrap_or(0) >= failed_ms
            }
            _ => false,
        };
        let expected = ReplanIdentity {
            assumption_id: assumption_id.to_string(),
            target_revision,
        };
        let acquisition = if chain.integrity_failed.is_some() {
            // Terminal already: nothing about the carrier can reopen it.
            ReplanAcquisition::Blocked(ReplanBlock::IntegrityAlreadyFailed)
        } else if carrier_matches {
            chain.acquire(awaiting, &expected, retry_bound)
        } else {
            // The claimed carrier is not the one the caller names: a shape the chain cannot
            // authorise, handled exactly like any other mismatch.
            ReplanAcquisition::Blocked(ReplanBlock::Mismatch {
                chain_digest: chain.prefix_digest.clone(),
            })
        };
        match &acquisition {
            ReplanAcquisition::Initial { attempt } | ReplanAcquisition::Retry { attempt } => {
                Self::append_horizon_lifecycle_replan(
                    &tx,
                    goal_id,
                    HorizonLifecycleEvent::ReplanStarted,
                    now_ms,
                    Some(checkpoint_sha256),
                    Some("running"),
                    Some("running"),
                    None,
                    Some(ReplanDetail::started(
                        assumption_id.to_string(),
                        *attempt,
                        target_revision,
                    )),
                )?;
            }
            ReplanAcquisition::Resume { .. } => {}
            ReplanAcquisition::Blocked(ReplanBlock::IntegrityAlreadyFailed) => {
                // Re-entry after the integrity receipt exists, recognised ONLY by the stored
                // digest equalling the digest of the prefix before it: the job goes back to
                // failed with the same code and no second receipt is written. A stored digest
                // that does not match is not a re-entry — still terminal, still nothing more
                // written; the caller sees a distinct block and never a retryable failure.
                let acquisition =
                    if chain.integrity_failed.as_deref() == Some(chain.prefix_digest.as_str()) {
                        acquisition
                    } else {
                        ReplanAcquisition::Blocked(ReplanBlock::IntegrityRecordMismatch)
                    };
                tx.execute(
                    "UPDATE mind_horizon_jobs SET status='failed',error=?2
                     WHERE goal_id=?1 AND status='running'",
                    rusqlite::params![goal_id, mind_spec::REPLAN_LIFECYCLE_MISMATCH],
                )?;
                tx.commit()?;
                return Ok(acquisition);
            }
            ReplanAcquisition::Blocked(block) => {
                // Mismatch, an unbound retry, or a claim the caller should never have made:
                // terminal integrity failure, receipt and failed job in this one transaction.
                let chain_digest = match block {
                    ReplanBlock::Mismatch { chain_digest } => chain_digest.clone(),
                    _ => chain.prefix_digest.clone(),
                };
                Self::write_integrity_failure_tx(
                    &tx,
                    goal_id,
                    now_ms,
                    checkpoint_sha256,
                    chain_digest,
                )?;
            }
        }
        tx.commit()?;
        Ok(acquisition)
    }

    /// The terminal integrity outcome, inside a caller-owned transaction: the running job goes
    /// to failed with the mismatch code and exactly one `replan_integrity_failed` receipt
    /// carrying the malformed prefix's digest is appended. Never called when the chain already
    /// carries an integrity receipt.
    fn write_integrity_failure_tx(
        tx: &rusqlite::Transaction<'_>,
        goal_id: &str,
        now_ms: u64,
        checkpoint_sha256: &str,
        chain_digest: String,
    ) -> anyhow::Result<()> {
        let changed = tx.execute(
            "UPDATE mind_horizon_jobs SET status='failed',error=?2
             WHERE goal_id=?1 AND status='running'",
            rusqlite::params![goal_id, mind_spec::REPLAN_LIFECYCLE_MISMATCH],
        )?;
        if changed != 1 {
            anyhow::bail!("replan integrity failure did not match one running job");
        }
        Self::append_horizon_lifecycle_replan(
            tx,
            goal_id,
            HorizonLifecycleEvent::ReplanIntegrityFailed,
            now_ms,
            Some(checkpoint_sha256),
            Some("running"),
            Some("failed"),
            Some(mind_spec::REPLAN_LIFECYCLE_MISMATCH),
            Some(ReplanDetail::integrity(chain_digest)),
        )?;
        Ok(())
    }

    /// Test-only: put a queued job to `failed` with a code, modelling the state the scheduler
    /// leaves after a closed attempt when a fixture drove the store directly.
    #[cfg(test)]
    pub(crate) fn force_job_failed_for_test(
        &self,
        goal_id: &str,
        code: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE mind_horizon_jobs SET status='failed',error=?2 WHERE goal_id=?1",
            rusqlite::params![goal_id, code],
        )?;
        Ok(())
    }

    /// Test-only (E.F3): break the stored digest so the checkpoint is no longer provably the goal's.
    #[cfg(test)]
    pub(crate) fn corrupt_checkpoint_digest_for_test(&self, goal_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE mind_horizon_checkpoints SET state_sha256='0000' WHERE goal_id=?1",
            [goal_id],
        )?;
        Ok(())
    }

    /// Test-only (E.F3): smuggle a raw row after the chain's end by copying the FIRST receipt
    /// (its digest column altered past the store's uniqueness), modelling tampering after expiry.
    #[cfg(test)]
    pub(crate) fn duplicate_first_receipt_row_for_test(&self, goal_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO mind_horizon_lifecycle(goal_id,event,receipt_json,receipt_sha256,occurred_ms)
             SELECT goal_id,event,receipt_json,receipt_sha256||'x',occurred_ms
             FROM mind_horizon_lifecycle WHERE goal_id=?1 ORDER BY id ASC LIMIT 1",
            [goal_id],
        )?;
        Ok(())
    }

    /// Test-only: append a `replan_started` marker with an arbitrary attempt, to model a chain
    /// that is malformed (a skipped ordinal) or has a newer open attempt.
    #[cfg(test)]
    pub(crate) fn append_started_for_test(
        &self,
        goal_id: &str,
        now_ms: u64,
        assumption_id: &str,
        attempt: u32,
        target_revision: u32,
    ) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let sha: String = tx.query_row(
            "SELECT state_sha256 FROM mind_horizon_checkpoints WHERE goal_id=?1",
            [goal_id],
            |row| row.get(0),
        )?;
        Self::append_horizon_lifecycle_replan(
            &tx,
            goal_id,
            HorizonLifecycleEvent::ReplanStarted,
            now_ms,
            Some(&sha),
            Some("running"),
            Some("running"),
            None,
            Some(ReplanDetail::started(
                assumption_id.to_string(),
                attempt,
                target_revision,
            )),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Test-only: append an integrity receipt with an arbitrary digest, to model a chain whose
    /// stored integrity record disagrees with its prefix.
    #[cfg(test)]
    pub(crate) fn append_integrity_for_test(
        &self,
        goal_id: &str,
        now_ms: u64,
        chain_digest: String,
    ) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let sha: String = tx.query_row(
            "SELECT state_sha256 FROM mind_horizon_checkpoints WHERE goal_id=?1",
            [goal_id],
            |row| row.get(0),
        )?;
        Self::write_integrity_failure_tx(&tx, goal_id, now_ms, &sha, chain_digest)?;
        tx.commit()?;
        Ok(())
    }

    /// E.F2 success, one transaction: the replanned (Active) checkpoint, the carrier row rewritten
    /// into the validated next segment, then `replanned` and `scheduled`.
    pub(crate) fn commit_replan(
        &self,
        checkpoint: &GoalCheckpoint,
        next: &HorizonJob,
        assumption_id: &str,
        attempt: u32,
        target_revision: u32,
    ) -> anyhow::Result<bool> {
        next.validate()?;
        if next.goal_id != checkpoint.goal_id {
            anyhow::bail!("replanned segment identity mismatch");
        }
        let job_json = serde_json::to_string(next)?;
        let wake_ms = i64::try_from(next.wake_at_ms)
            .map_err(|_| anyhow::anyhow!("horizon wake timestamp is out of range"))?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        // A success closure may close only the currently open marker with this attempt and
        // this identity. The chain is reduced HERE, in the transaction, before anything is
        // written; any other shape is the terminal integrity outcome, and the replanned
        // checkpoint never lands.
        let receipts = Self::load_lifecycle_on(&tx, &checkpoint.goal_id)?;
        let chain = reduce_replan(&receipts);
        let expected = ReplanIdentity {
            assumption_id: assumption_id.to_string(),
            target_revision,
        };
        let open = chain.open_markers();
        let closes_the_open_marker = chain.integrity_failed.is_none()
            && !chain.malformed
            && matches!(open.as_slice(), [m] if m.attempt == attempt && m.identity == expected);
        if !closes_the_open_marker {
            if chain.integrity_failed.is_none() {
                let stored_sha256: String = tx.query_row(
                    "SELECT state_sha256 FROM mind_horizon_checkpoints WHERE goal_id=?1",
                    [&checkpoint.goal_id],
                    |row| row.get(0),
                )?;
                Self::write_integrity_failure_tx(
                    &tx,
                    &checkpoint.goal_id,
                    checkpoint.created_at_ms,
                    &stored_sha256,
                    chain.prefix_digest.clone(),
                )?;
            } else {
                tx.execute(
                    "UPDATE mind_horizon_jobs SET status='failed',error=?2
                     WHERE goal_id=?1 AND status='running'",
                    rusqlite::params![checkpoint.goal_id, mind_spec::REPLAN_LIFECYCLE_MISMATCH],
                )?;
            }
            tx.commit()?;
            return Ok(false);
        }
        Self::save_checkpoint_tx(&tx, checkpoint)?;
        let changed = tx.execute(
            "UPDATE mind_horizon_jobs SET job_json=?2,wake_ms=?3,status='pending',error=NULL
             WHERE goal_id=?1 AND status='running'",
            rusqlite::params![checkpoint.goal_id, job_json, wake_ms],
        )?;
        if changed != 1 {
            anyhow::bail!("horizon replan did not match one running carrier");
        }
        let detail = ReplanDetail::started(assumption_id.to_string(), attempt, target_revision);
        Self::append_horizon_lifecycle_replan(
            &tx,
            &checkpoint.goal_id,
            HorizonLifecycleEvent::Replanned,
            checkpoint.created_at_ms,
            Some(&checkpoint.state_sha256),
            Some("running"),
            None,
            None,
            Some(detail),
        )?;
        Self::append_horizon_lifecycle(
            &tx,
            &checkpoint.goal_id,
            HorizonLifecycleEvent::Scheduled,
            checkpoint.created_at_ms,
            Some(&checkpoint.state_sha256),
            None,
            Some("pending"),
            None,
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// E.F2: close one attempt with a bounded failure — receipt and failed job in one transaction,
    /// idempotent when the chain already closed that attempt.
    pub(crate) fn fail_replan_attempt(
        &self,
        goal_id: &str,
        reason: HorizonFailureReason,
        attempt: u32,
        occurred_at_ms: u64,
    ) -> anyhow::Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let receipts = Self::load_lifecycle_on(&tx, goal_id)?;
        let chain = reduce_replan(&receipts);
        if chain.integrity_failed.is_some() {
            // Terminal already: nothing is appended after an integrity receipt.
            tx.execute(
                "UPDATE mind_horizon_jobs SET status='failed',error=?2
                 WHERE goal_id=?1 AND status='running'",
                rusqlite::params![goal_id, mind_spec::REPLAN_LIFECYCLE_MISMATCH],
            )?;
            tx.commit()?;
            return Ok(false);
        }
        // Idempotent re-entry ONLY on a well-formed chain whose latest marker is this very
        // attempt, already closed, with nothing open after it. A stale closure arriving after a
        // newer attempt opened, or any malformed chain, is not a re-entry.
        let open = chain.open_markers();
        let already_closed_latest = !chain.malformed
            && open.is_empty()
            && chain
                .markers
                .last()
                .is_some_and(|m| m.attempt == attempt && m.closed_by_success.is_some());
        if already_closed_latest {
            tx.commit()?;
            return Ok(true);
        }
        let checkpoint_sha256: Option<String> = tx
            .query_row(
                "SELECT state_sha256 FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [goal_id],
                |row| row.get(0),
            )
            .optional()?;
        // A failure closure may close only the currently open marker with this attempt.
        if chain.malformed || !matches!(open.as_slice(), [m] if m.attempt == attempt) {
            let Some(sha) = checkpoint_sha256.as_deref() else {
                anyhow::bail!("replan attempt failure found no active checkpoint");
            };
            Self::write_integrity_failure_tx(
                &tx,
                goal_id,
                occurred_at_ms,
                sha,
                chain.prefix_digest.clone(),
            )?;
            tx.commit()?;
            return Ok(false);
        }
        let changed = tx.execute(
            "UPDATE mind_horizon_jobs SET status='failed',error=?2
             WHERE goal_id=?1 AND status='running'",
            rusqlite::params![goal_id, reason.as_str()],
        )?;
        if changed != 1 {
            anyhow::bail!("replan attempt failure did not match one running job");
        }
        Self::append_horizon_lifecycle_replan(
            &tx,
            goal_id,
            HorizonLifecycleEvent::Failed,
            occurred_at_ms,
            checkpoint_sha256.as_deref(),
            Some("running"),
            Some("failed"),
            Some(reason.as_str()),
            Some(ReplanDetail::attempt_only(attempt)),
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn finish_horizon(&self, run: &HorizonRun, receipt: &OutcomeReceipt) -> anyhow::Result<()> {
        if !receipt.verify_state(run) {
            anyhow::bail!("outcome receipt does not match the completed horizon state");
        }
        let receipt_json = serde_json::to_string(receipt)?;
        let completed_ms = i64::try_from(receipt.finished_at_ms)
            .map_err(|_| anyhow::anyhow!("horizon completion timestamp is out of range"))?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT receipt_sha256 FROM mind_horizon_outcomes WHERE goal_id=?1",
                [&receipt.goal_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != receipt.receipt_sha256 {
                anyhow::bail!("refused to overwrite a different horizon outcome");
            }
            tx.execute(
                "DELETE FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [&receipt.goal_id],
            )?;
            tx.execute(
                "DELETE FROM mind_horizon_jobs WHERE goal_id=?1",
                [&receipt.goal_id],
            )?;
            tx.commit()?;
            return Ok(());
        }
        let has_active: bool = tx
            .query_row(
                "SELECT 1 FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [&receipt.goal_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_active {
            anyhow::bail!("cannot finish a horizon goal without an active checkpoint");
        }
        let inserted = tx.execute(
            "INSERT INTO mind_horizon_outcomes
                (goal_id,receipt_json,receipt_sha256,completed_ms)
             VALUES (?1,?2,?3,?4)
             ON CONFLICT(goal_id) DO NOTHING",
            rusqlite::params![
                receipt.goal_id,
                receipt_json,
                receipt.receipt_sha256,
                completed_ms
            ],
        )?;
        if inserted != 1 {
            anyhow::bail!("horizon outcome was not inserted");
        }
        Self::append_horizon_lifecycle(
            &tx,
            &receipt.goal_id,
            HorizonLifecycleEvent::Completed,
            receipt.finished_at_ms,
            Some(&receipt.final_state_sha256),
            Some("running"),
            None,
            None,
        )?;
        tx.execute(
            "DELETE FROM mind_horizon_checkpoints WHERE goal_id=?1",
            [&receipt.goal_id],
        )?;
        tx.execute(
            "DELETE FROM mind_horizon_jobs WHERE goal_id=?1",
            [&receipt.goal_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn schedule_horizon_job(&self, job: &HorizonJob) -> anyhow::Result<()> {
        job.validate()?;
        let wake_ms = i64::try_from(job.wake_at_ms)
            .map_err(|_| anyhow::anyhow!("horizon wake timestamp is out of range"))?;
        let job_json = serde_json::to_string(job)?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let checkpoint: Option<(String, i64)> = tx
            .query_row(
                "SELECT state_sha256,created_ms FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [&job.goal_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let has_outcome: bool = tx
            .query_row(
                "SELECT 1 FROM mind_horizon_outcomes WHERE goal_id=?1",
                [&job.goal_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if checkpoint.is_none() || has_outcome || Self::verified_expired_on(&tx, &job.goal_id)? {
            anyhow::bail!("horizon job requires one active, non-terminal goal");
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT job_json FROM mind_horizon_jobs WHERE goal_id=?1",
                [&job.goal_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing) = existing {
            if existing != job_json {
                anyhow::bail!("a different horizon segment is already scheduled");
            }
            tx.commit()?;
            return Ok(());
        }
        tx.execute(
            "INSERT INTO mind_horizon_jobs (goal_id,job_json,wake_ms,status,error)
             VALUES (?1,?2,?3,'pending',NULL)",
            rusqlite::params![job.goal_id, job_json, wake_ms],
        )?;
        let (checkpoint_sha256, checkpoint_ms) =
            checkpoint.expect("checked above: scheduled jobs require a checkpoint");
        Self::append_horizon_lifecycle(
            &tx,
            &job.goal_id,
            HorizonLifecycleEvent::Scheduled,
            u64::try_from(checkpoint_ms)
                .map_err(|_| anyhow::anyhow!("horizon checkpoint timestamp is out of range"))?,
            Some(&checkpoint_sha256),
            None,
            Some("pending"),
            None,
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Atomically claim a small batch so overlapping scheduler ticks cannot run the same segment.
    pub fn claim_due_horizon_jobs(&self, now_ms: u64) -> anyhow::Result<Vec<HorizonJob>> {
        Ok(self.sweep_and_claim(now_ms)?.jobs)
    }

    /// E.F3: whether the VERIFIED lifecycle chain carries the terminal `expired` receipt. A chain
    /// that fails verification is an error here, so every consumer fails closed on corruption.
    fn verified_expired_on(conn: &rusqlite::Connection, goal_id: &str) -> anyhow::Result<bool> {
        Ok(Self::load_lifecycle_on(conn, goal_id)?
            .iter()
            .any(|receipt| receipt.event == HorizonLifecycleEvent::Expired))
    }

    /// E.F3: expire every goal whose elapsed budget ran out, then claim what is due — ONE
    /// transaction, so no other scheduler can claim between the sweep and the claim. The sweep
    /// covers queue rows `pending`, `failed`, `paused` and checkpoints with no row; it never
    /// touches `running`. Each expiry appends one `expired` receipt (prior status → none,
    /// `budget_elapsed`) and deletes the queue row; the checkpoint stays as history.
    pub fn sweep_and_claim(&self, now_ms: u64) -> anyhow::Result<HorizonSweep> {
        let now_i64 = i64::try_from(now_ms)
            .map_err(|_| anyhow::anyhow!("horizon tick timestamp is out of range"))?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let candidates: Vec<(String, String, String, Option<String>, Option<String>)> = {
            let mut stmt = tx.prepare(
                "SELECT c.goal_id,c.checkpoint_json,c.state_sha256,j.status,j.error
                 FROM mind_horizon_checkpoints c
                 LEFT JOIN mind_horizon_jobs j ON j.goal_id=c.goal_id
                 WHERE (j.status IS NULL OR j.status IN ('pending','failed','paused'))
                   AND NOT EXISTS (SELECT 1 FROM mind_horizon_outcomes o WHERE o.goal_id=c.goal_id)
                 ORDER BY c.goal_id",
            )?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        let mut expired = Vec::new();
        for (goal_id, checkpoint_json, state_sha256, queue_status, queue_error) in candidates {
            // Fail closed on identity: a receipt is signed only over the goal's own checkpoint,
            // whose digest must match the column it is stored under.
            let checkpoint: GoalCheckpoint = serde_json::from_str(&checkpoint_json)?;
            if checkpoint.goal_id != goal_id || checkpoint.state_sha256 != state_sha256 {
                anyhow::bail!("horizon checkpoint identity or digest mismatch");
            }
            let run =
                HorizonRun::resume(&checkpoint, checkpoint.created_at_ms).map_err(|error| {
                    anyhow::anyhow!("horizon checkpoint failed validation: {error:?}")
                })?;
            // The VERIFIED chain decides terminality (a corrupt chain fails the tick): an expired
            // or integrity-failed chain, or a failed row carrying a terminal E.F2 code, is already
            // terminal and gets no second terminal receipt.
            let receipts = Self::load_lifecycle_on(&tx, &goal_id)?;
            let already_terminal = receipts
                .iter()
                .any(|receipt| receipt.event == HorizonLifecycleEvent::Expired)
                || reduce_replan(&receipts).integrity_failed.is_some()
                || (queue_status.as_deref() == Some("failed")
                    && queue_error
                        .as_deref()
                        .is_some_and(|code| TERMINAL_HORIZON_FAILURE_CODES.contains(&code)));
            if already_terminal {
                continue;
            }
            // Exactly `HorizonRun::check_time`'s boundary: equality is inside the budget.
            if now_ms.saturating_sub(run.started_at_ms) <= run.budget.max_elapsed_ms {
                continue;
            }
            tx.execute("DELETE FROM mind_horizon_jobs WHERE goal_id=?1", [&goal_id])?;
            Self::append_horizon_lifecycle(
                &tx,
                &goal_id,
                HorizonLifecycleEvent::Expired,
                now_ms,
                Some(&state_sha256),
                queue_status.as_deref(),
                None,
                Some(HorizonFailureReason::BudgetElapsed.as_str()),
            )?;
            expired.push(goal_id);
        }
        let mut stmt = tx.prepare(
            "SELECT j.goal_id,j.job_json,c.state_sha256 FROM mind_horizon_jobs j
             LEFT JOIN mind_horizon_checkpoints c ON c.goal_id=j.goal_id
             WHERE j.status='pending' AND j.wake_ms<=?1 ORDER BY j.wake_ms,j.goal_id LIMIT 8",
        )?;
        let rows: Vec<(String, String, Option<String>)> = stmt
            .query_map([now_i64], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        let mut jobs = Vec::with_capacity(rows.len());
        for (goal_id, job_json, checkpoint_sha256) in rows {
            let job: HorizonJob = serde_json::from_str(&job_json)?;
            job.validate()?;
            if job.goal_id != goal_id {
                anyhow::bail!("horizon job identity mismatch");
            }
            let changed = tx.execute(
                "UPDATE mind_horizon_jobs SET status='running',error=NULL
                 WHERE goal_id=?1 AND status='pending'",
                [&goal_id],
            )?;
            if changed == 1 {
                Self::append_horizon_lifecycle(
                    &tx,
                    &goal_id,
                    HorizonLifecycleEvent::WakeStarted,
                    now_ms,
                    checkpoint_sha256.as_deref(),
                    Some("pending"),
                    Some("running"),
                    None,
                )?;
                jobs.push(job);
            }
        }
        tx.commit()?;
        Ok(HorizonSweep { expired, jobs })
    }

    pub fn finish_horizon_job(&self, goal_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM mind_horizon_jobs WHERE goal_id=?1", [goal_id])?;
        Ok(())
    }

    pub(crate) fn fail_horizon_job(
        &self,
        goal_id: &str,
        reason: HorizonFailureReason,
        occurred_at_ms: u64,
    ) -> anyhow::Result<()> {
        // This field is operator-visible, so callers can provide only this typed, code-owned
        // vocabulary. Free-text backend errors have no type-correct route into persistence.
        let reason_code = reason.as_str();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let checkpoint_sha256: Option<String> = tx
            .query_row(
                "SELECT state_sha256 FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [goal_id],
                |row| row.get(0),
            )
            .optional()?;
        let changed = tx.execute(
            "UPDATE mind_horizon_jobs SET status='failed',error=?2
             WHERE goal_id=?1 AND status='running'",
            rusqlite::params![goal_id, reason_code],
        )?;
        if changed != 1 {
            anyhow::bail!("horizon failure status did not match one running job");
        }
        Self::append_horizon_lifecycle(
            &tx,
            goal_id,
            HorizonLifecycleEvent::Failed,
            occurred_at_ms,
            checkpoint_sha256.as_deref(),
            Some("running"),
            Some("failed"),
            Some(reason_code),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// All scheduler jobs are validated read-only segments, so a process crash may safely return a
    /// claimed-but-unfinished row to the pending queue. The HorizonRun action id deduplicates the
    /// case where the checkpoint committed but the job deletion did not.
    pub fn recover_horizon_jobs(&self, now_ms: u64) -> usize {
        let recovered = (|| -> anyhow::Result<usize> {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction()?;
            let rows: Vec<(String, Option<String>)> = {
                let mut stmt = tx.prepare(
                    "SELECT j.goal_id,c.state_sha256 FROM mind_horizon_jobs j
                     LEFT JOIN mind_horizon_checkpoints c ON c.goal_id=j.goal_id
                     WHERE j.status='running' ORDER BY j.goal_id",
                )?;
                let rows = stmt
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                rows
            };
            for (goal_id, checkpoint_sha256) in &rows {
                let changed = tx.execute(
                    "UPDATE mind_horizon_jobs SET status='pending',error=NULL
                     WHERE goal_id=?1 AND status='running'",
                    [goal_id],
                )?;
                if changed != 1 {
                    anyhow::bail!("horizon recovery lost a scheduler race");
                }
                Self::append_horizon_lifecycle(
                    &tx,
                    goal_id,
                    HorizonLifecycleEvent::Recovered,
                    now_ms,
                    checkpoint_sha256.as_deref(),
                    Some("running"),
                    Some("pending"),
                    None,
                )?;
            }
            tx.commit()?;
            Ok(rows.len())
        })();
        recovered.unwrap_or(0)
    }

    /// Read a terminal receipt without reviving the completed goal.
    pub fn load_horizon_outcome(&self, goal_id: &str) -> anyhow::Result<Option<OutcomeReceipt>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<(String, String)> = conn
            .query_row(
                "SELECT receipt_json,receipt_sha256
                 FROM mind_horizon_outcomes WHERE goal_id=?1",
                [goal_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((receipt_json, stored_sha256)) = raw else {
            return Ok(None);
        };
        let receipt: OutcomeReceipt = serde_json::from_str(&receipt_json)?;
        if receipt.goal_id != goal_id
            || receipt.receipt_sha256 != stored_sha256
            || !receipt.verify()
        {
            anyhow::bail!("horizon outcome failed validation");
        }
        Ok(Some(receipt))
    }
}

#[cfg(test)]
mod horizon_tests {
    use std::collections::BTreeMap;

    use mind_spec::{ActionTrace, HorizonBudget, HorizonRun};

    use super::*;
    use crate::HorizonJobKind;

    fn run(start: u64) -> HorizonRun {
        HorizonRun::start(
            "goal:persisted",
            "Survive a real store restart without resetting authority or budget",
            vec!["checkpoint".into(), "finish".into()],
            BTreeMap::new(),
            HorizonBudget {
                max_actions: 2,
                max_replans: 1,
                max_cost_units: 5,
                max_elapsed_ms: 10_000,
            },
            start,
        )
        .unwrap()
    }

    #[test]
    fn horizon_checkpoint_and_outcome_survive_store_restart() {
        let scratch = mind_types::scratch::file("horizon-store", "db");
        let db = scratch.as_str();
        let start = 1_900_000_000_000;
        let mut original = run(start);
        let checkpoint = original.checkpoint(start + 10).unwrap();
        RecipeStore::open(&db)
            .unwrap()
            .save_horizon_checkpoint(&checkpoint)
            .unwrap();

        let store = RecipeStore::open(&db).unwrap();
        let mut resumed = store
            .load_horizon("goal:persisted", start + 20)
            .unwrap()
            .expect("active goal survives restart");
        resumed
            .record_action(ActionTrace {
                action_id: "verified-step".into(),
                summary: "verified the resumed state".into(),
                at_ms: start + 30,
                cost_units: 2,
                reversible: true,
                authorization_receipt: None,
            })
            .unwrap();
        let receipt = resumed.complete(start + 40).unwrap();
        store.finish_horizon(&resumed, &receipt).unwrap();
        store
            .finish_horizon(&resumed, &receipt)
            .expect("a lost acknowledgement can retry the same terminal receipt");
        drop(store);

        let reopened = RecipeStore::open(&db).unwrap();
        assert!(reopened
            .load_horizon("goal:persisted", start + 50)
            .unwrap()
            .is_none());
        let stored = reopened
            .load_horizon_outcome("goal:persisted")
            .unwrap()
            .expect("terminal receipt survives restart");
        assert_eq!(stored, receipt);
        assert!(stored.verify_state(&resumed));
    }

    #[test]
    fn failed_horizon_diagnosis_and_retry_receipt_survive_store_restart() {
        let scratch = mind_types::scratch::file("horizon-failed-retry", "db");
        let db = scratch.as_str();
        let start = 1_900_000_000_000;
        let mut original = run(start);
        let original_budget = original.budget;
        let checkpoint = original.checkpoint(start + 10).unwrap();
        let job = HorizonJob {
            kind: HorizonJobKind::Segment,
            goal_id: original.goal_id.clone(),
            segment_id: "observe-once".into(),
            recipe: crate::Recipe {
                id: "observe-inbox".into(),
                name: "Observe the inbox once".into(),
                steps: vec![RecipeStep::Tool {
                    tool_name: "inbox".into(),
                    args: serde_json::json!({"limit": 1}),
                    store_as: "fresh".into(),
                    on_error: crate::ErrorAction::Fail,
                }],
            },
            assumption_vars: BTreeMap::new(),
            wake_at_ms: start + 20,
            cost_units: 1,
            complete_on_success: false,
        };

        let store = RecipeStore::open(&db).unwrap();
        store.save_horizon_checkpoint(&checkpoint).unwrap();
        store.schedule_horizon_job(&job).unwrap();
        assert!(store
            .fail_horizon_job(
                &original.goal_id,
                HorizonFailureReason::SegmentContract,
                start + 20,
            )
            .is_err());
        assert_eq!(store.claim_due_horizon_jobs(start + 20).unwrap().len(), 1);
        store
            .fail_horizon_job(
                &original.goal_id,
                HorizonFailureReason::SegmentContract,
                start + 20,
            )
            .unwrap();
        assert!(store
            .fail_horizon_job(
                &original.goal_id,
                HorizonFailureReason::ActionLedger,
                start + 21,
            )
            .is_err());
        drop(store);

        let reopened = RecipeStore::open(&db).unwrap();
        let failed = reopened.list_horizons(start + 30).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].queue_status.as_deref(), Some("failed"));
        assert_eq!(
            failed[0].failure_reason.as_deref(),
            Some("segment_contract_failed")
        );
        let lifecycle = reopened.load_horizon_lifecycle(&original.goal_id).unwrap();
        assert_eq!(
            lifecycle
                .iter()
                .map(|receipt| receipt.event)
                .collect::<Vec<_>>(),
            vec![
                HorizonLifecycleEvent::Scheduled,
                HorizonLifecycleEvent::WakeStarted,
                HorizonLifecycleEvent::Failed,
            ]
        );
        assert!(lifecycle.iter().all(HorizonLifecycleReceipt::verify));
        assert_eq!(
            lifecycle[2].previous_receipt_sha256.as_deref(),
            Some(lifecycle[1].receipt_sha256.as_str())
        );
        assert_eq!(
            lifecycle[2].failure_reason.as_deref(),
            Some("segment_contract_failed")
        );
        let receipt = reopened
            .control_horizon(&original.goal_id, HorizonControlAction::Retry, start + 30)
            .unwrap();
        assert!(receipt.verify());
        drop(reopened);

        let after_retry_restart = RecipeStore::open(&db).unwrap();
        let pending = after_retry_restart.list_horizons(start + 40).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].queue_status.as_deref(), Some("pending"));
        assert_eq!(pending[0].failure_reason, None);
        assert_eq!(pending[0].run.budget, original_budget);
        let controls = after_retry_restart
            .load_horizon_controls(&original.goal_id)
            .unwrap();
        assert_eq!(controls, vec![receipt]);
        assert!(after_retry_restart
            .fail_horizon_job(
                "goal:missing",
                HorizonFailureReason::SegmentContract,
                start + 40,
            )
            .is_err());
        after_retry_restart
            .control_horizon(&original.goal_id, HorizonControlAction::Pause, start + 40)
            .unwrap();
        assert!(after_retry_restart
            .control_horizon(&original.goal_id, HorizonControlAction::Retry, start + 41,)
            .is_err());
        after_retry_restart
            .control_horizon(&original.goal_id, HorizonControlAction::Resume, start + 42)
            .unwrap();
        assert_eq!(
            after_retry_restart
                .claim_due_horizon_jobs(start + 42)
                .unwrap()
                .len(),
            1
        );
        assert!(after_retry_restart
            .control_horizon(&original.goal_id, HorizonControlAction::Retry, start + 43,)
            .is_err());
    }

    #[test]
    fn legacy_free_text_horizon_failure_is_never_exposed() {
        const PRIVATE_LEGACY_ERROR: &str =
            "provider rejected credential sk-test-private: this must stay in SQLite";
        let scratch = mind_types::scratch::file("horizon-legacy-failure", "db");
        let db = scratch.as_str();
        let start = 1_900_000_000_000;
        let mut original = run(start);
        let checkpoint = original.checkpoint(start + 10).unwrap();
        let job = HorizonJob {
            kind: HorizonJobKind::Segment,
            goal_id: original.goal_id.clone(),
            segment_id: "observe-once".into(),
            recipe: crate::Recipe {
                id: "observe-inbox".into(),
                name: "Observe the inbox once".into(),
                steps: vec![RecipeStep::Tool {
                    tool_name: "inbox".into(),
                    args: serde_json::json!({"limit": 1}),
                    store_as: "fresh".into(),
                    on_error: crate::ErrorAction::Fail,
                }],
            },
            assumption_vars: BTreeMap::new(),
            wake_at_ms: start + 20,
            cost_units: 1,
            complete_on_success: false,
        };

        let store = RecipeStore::open(&db).unwrap();
        store.save_horizon_checkpoint(&checkpoint).unwrap();
        store.schedule_horizon_job(&job).unwrap();
        assert_eq!(store.claim_due_horizon_jobs(start + 20).unwrap().len(), 1);
        store
            .fail_horizon_job(
                &original.goal_id,
                HorizonFailureReason::SegmentContract,
                start + 20,
            )
            .unwrap();
        let bounded_current_error: String = store
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT error FROM mind_horizon_jobs WHERE goal_id=?1",
                [&original.goal_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            bounded_current_error,
            HorizonFailureReason::SegmentContract.as_str()
        );
        assert!(!bounded_current_error.contains(PRIVATE_LEGACY_ERROR));

        // Simulate a pre-E.HOR1 row that already persisted free text; reads still redact it.
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE mind_horizon_jobs SET status='failed',error=?2 WHERE goal_id=?1",
                rusqlite::params![original.goal_id, PRIVATE_LEGACY_ERROR],
            )
            .unwrap();
        drop(store);

        let reopened = RecipeStore::open(&db).unwrap();
        let failed = reopened.list_horizons(start + 30).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(
            failed[0].failure_reason.as_deref(),
            Some(LEGACY_HORIZON_FAILURE)
        );
        assert!(!failed[0]
            .failure_reason
            .as_deref()
            .unwrap()
            .contains(PRIVATE_LEGACY_ERROR));
        reopened
            .control_horizon(&original.goal_id, HorizonControlAction::Retry, start + 30)
            .unwrap();
        let persisted_error: Option<String> = reopened
            .conn
            .lock()
            .unwrap()
            .query_row(
                "SELECT error FROM mind_horizon_jobs WHERE goal_id=?1",
                [&original.goal_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted_error, None);
    }

    #[test]
    fn horizon_store_rejects_stale_and_corrupt_checkpoints() {
        let scratch = mind_types::scratch::file("horizon-store-corrupt", "db");
        let db = scratch.as_str();
        let start = 1_900_000_000_000;
        let mut state = run(start);
        let older = state.checkpoint(start + 10).unwrap();
        let newer = state.checkpoint(start + 20).unwrap();
        let store = RecipeStore::open(&db).unwrap();
        store.save_horizon_checkpoint(&newer).unwrap();
        assert!(store.save_horizon_checkpoint(&older).is_err());
        state
            .record_action(ActionTrace {
                action_id: "same-timestamp".into(),
                summary: "must not replace a different state at the same logical time".into(),
                at_ms: start + 20,
                cost_units: 0,
                reversible: true,
                authorization_receipt: None,
            })
            .unwrap();
        let conflicting = state.checkpoint(start + 20).unwrap();
        assert!(store.save_horizon_checkpoint(&conflicting).is_err());

        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE mind_horizon_checkpoints SET checkpoint_json='{}'
                 WHERE goal_id='goal:persisted'",
                [],
            )
            .unwrap();
        assert!(store.load_horizon("goal:persisted", start + 30).is_err());
        assert!(
            store.list_horizons(start + 30).is_err(),
            "the operator view must not hide or default a corrupt checkpoint"
        );
        assert!(store
            .control_horizon("goal:persisted", HorizonControlAction::Retry, start + 30,)
            .is_err());
    }

    #[test]
    fn missing_checkpoint_is_still_a_receipted_scheduler_failure() {
        let start = 1_900_000_000_000;
        let mut original = run(start);
        let checkpoint = original.checkpoint(start + 10).unwrap();
        let job = HorizonJob {
            kind: HorizonJobKind::Segment,
            goal_id: original.goal_id.clone(),
            segment_id: "observe-once".into(),
            recipe: crate::Recipe {
                id: "observe-inbox".into(),
                name: "Observe the inbox once".into(),
                steps: vec![RecipeStep::Tool {
                    tool_name: "inbox".into(),
                    args: serde_json::json!({"limit": 1}),
                    store_as: "fresh".into(),
                    on_error: crate::ErrorAction::Fail,
                }],
            },
            assumption_vars: BTreeMap::new(),
            wake_at_ms: start + 20,
            cost_units: 1,
            complete_on_success: false,
        };
        let store = RecipeStore::open(":memory:").unwrap();
        store.save_horizon_checkpoint(&checkpoint).unwrap();
        store.schedule_horizon_job(&job).unwrap();
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [&original.goal_id],
            )
            .unwrap();

        assert_eq!(store.claim_due_horizon_jobs(start + 20).unwrap().len(), 1);
        store
            .fail_horizon_job(
                &original.goal_id,
                HorizonFailureReason::CheckpointValidation,
                start + 20,
            )
            .unwrap();
        let lifecycle = store.load_horizon_lifecycle(&original.goal_id).unwrap();
        assert_eq!(lifecycle.len(), 3);
        assert_eq!(lifecycle[0].event, HorizonLifecycleEvent::Scheduled);
        assert!(lifecycle[0].state_sha256.is_some());
        assert_eq!(lifecycle[1].event, HorizonLifecycleEvent::WakeStarted);
        assert!(lifecycle[1].state_sha256.is_none());
        assert_eq!(lifecycle[2].event, HorizonLifecycleEvent::Failed);
        assert!(lifecycle[2].state_sha256.is_none());
        assert_eq!(
            lifecycle[2].failure_reason.as_deref(),
            Some("checkpoint_validation_failed")
        );
        assert!(lifecycle.iter().all(HorizonLifecycleReceipt::verify));
    }
}

/// E.WEB19: the identity migration is idempotent and classifies exactly once.
#[cfg(test)]
mod run_identity_migration_tests {
    use super::*;

    fn pre_migration_db(path: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE mind_recipe_runs (
                id TEXT PRIMARY KEY, name TEXT NOT NULL, status TEXT NOT NULL,
                current_step INTEGER NOT NULL, steps_json TEXT NOT NULL, vars_json TEXT NOT NULL,
                error TEXT, updated_ms INTEGER NOT NULL);",
        )
        .unwrap();
        for (id, name) in [
            ("import:market-check-1", "standing: market-check"),
            ("sched:abc123-1", "standing: check the weather"),
            ("delegate:legacy-1", "standing: market-check"),
        ] {
            conn.execute(
                "INSERT INTO mind_recipe_runs VALUES (?1,?2,'sleeping',0,'[]','{\"__wake_at\":123}',NULL,1)",
                [id, name],
            )
            .unwrap();
        }
    }

    type Row = (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    );

    /// Reads rows with their identity when the columns exist, and as `None` before the migration
    /// has run — so the same helper can compare the two states.
    fn rows(path: &str) -> Vec<Row> {
        let conn = Connection::open(path).unwrap();
        let has_identity = conn
            .prepare("SELECT origin FROM mind_recipe_runs LIMIT 1")
            .is_ok();
        let sql = if has_identity {
            "SELECT id,name,status,origin,canonical_agent,vars_json FROM mind_recipe_runs ORDER BY id"
        } else {
            "SELECT id,name,status,NULL,NULL,vars_json FROM mind_recipe_runs ORDER BY id"
        };
        let mut stmt = conn.prepare(sql).unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
    }

    #[test]
    fn opening_a_pre_migration_store_twice_classifies_once_and_changes_nothing_else() {
        let dir = std::env::temp_dir().join(format!("ym-web19-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recipes.db").to_str().unwrap().to_string();
        pre_migration_db(&path);
        let before = rows(&path);
        assert!(
            before.iter().all(|r| r.3.is_none()),
            "pre-migration rows carry no identity"
        );

        let first = RecipeStore::open(&path).unwrap();
        let after = rows(&path);
        let by_id = |id: &str| after.iter().find(|r| r.0 == id).cloned().unwrap();
        // The imported order is classified with its canonical agent, taken once from the name.
        let imported = by_id("import:market-check-1");
        assert_eq!(imported.3.as_deref(), Some("imported_agent"));
        assert_eq!(imported.4.as_deref(), Some("market-check"));
        // The scheduled goal is typed and carries NO agent: it must never join one.
        let sched = by_id("sched:abc123-1");
        assert_eq!(sched.3.as_deref(), Some("scheduled_goal"));
        assert_eq!(sched.4, None);
        // The ambiguous legacy row is not promoted, whatever its name says.
        let legacy = by_id("delegate:legacy-1");
        assert_eq!(legacy.3, None);
        assert_eq!(legacy.4, None);
        // Nothing else moved: ids, names, status and vars are byte-identical.
        for (b, a) in before.iter().zip(after.iter()) {
            assert_eq!((&b.0, &b.1, &b.2, &b.5), (&a.0, &a.1, &a.2, &a.5));
        }
        drop(first);

        // A second open is a no-op: same rows, same values, no error.
        let second = RecipeStore::open(&path).unwrap();
        assert_eq!(rows(&path), after, "idempotent");
        // The typed reader sees the same classification; wake and state survive.
        let sleeping = second.due_sleeping(u64::MAX);
        assert_eq!(sleeping.len(), 3);
        let imp = sleeping
            .iter()
            .find(|r| r.id == "import:market-check-1")
            .unwrap();
        assert_eq!(imp.canonical_agent.as_deref(), Some("market-check"));
        assert_eq!(
            imp.vars.get("__wake_at").and_then(|v| v.as_u64()),
            Some(123)
        );
        // Saving a legacy row back (as a resume would) keeps its NULL identity, and saving an
        // identified row without identity keeps what the backfill set (COALESCE, not overwrite).
        let mut leg = second.load("delegate:legacy-1").unwrap();
        leg.status = "paused".into();
        second.save(&leg, 2).unwrap();
        let mut imp2 = second.load("import:market-check-1").unwrap();
        imp2.origin = None;
        imp2.canonical_agent = None;
        second.save(&imp2, 2).unwrap();
        let again = rows(&path);
        assert_eq!(
            again.iter().find(|r| r.0 == "delegate:legacy-1").unwrap().3,
            None
        );
        assert_eq!(
            again
                .iter()
                .find(|r| r.0 == "import:market-check-1")
                .unwrap()
                .4
                .as_deref(),
            Some("market-check")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// L3b: one stored notice row, read whole so it can be verified whole.
struct NoticeRow {
    notice_id: String,
    operator_id: String,
    kind: String,
    created_ms: i64,
    text: String,
    text_sha256: String,
    chars: i64,
    dedupe_key: String,
    /// L3c: the canonical engagement marker for an engaging notice; None for the L3b kinds.
    engagement_json: Option<String>,
    /// L3c: the instant after which an unshown engaging notice expires; None for the L3b kinds.
    show_by_ms: Option<i64>,
}

const NOTICE_COLUMNS: &str =
    "notice_id,operator_id,kind,created_ms,text,text_sha256,chars,dedupe_key,engagement_json,show_by_ms";

fn notice_row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoticeRow> {
    Ok(NoticeRow {
        notice_id: row.get(0)?,
        operator_id: row.get(1)?,
        kind: row.get(2)?,
        created_ms: row.get(3)?,
        text: row.get(4)?,
        text_sha256: row.get(5)?,
        chars: row.get(6)?,
        dedupe_key: row.get(7)?,
        engagement_json: row.get(8)?,
        show_by_ms: row.get(9)?,
    })
}

/// L3c: the identity of an engaging notice binds the marker's digest and the show-by bound.
fn engaging_notice_id_for(
    operator_id: &str,
    kind: NoticeKind,
    created_ms: u64,
    text_sha256: &str,
    dedupe_key: &str,
    marker_digest: &str,
    show_by_ms: u64,
) -> String {
    format!(
        "notice:{}",
        sha256_hex(
            format!(
                "v2\\n{operator_id}\\n{}\\n{created_ms}\\n{text_sha256}\\n{dedupe_key}\\n{marker_digest}\\n{show_by_ms}",
                kind.as_str()
            )
            .as_bytes()
        )
    )
}

/// The notice id is the full digest of the row's identity: operator, kind, creation instant,
/// text digest and dedupe key. A row whose id does not recompute is not the row that was queued.
fn notice_id_for(
    operator_id: &str,
    kind: NoticeKind,
    created_ms: u64,
    text_sha256: &str,
    dedupe_key: &str,
) -> String {
    format!(
        "notice:{}",
        sha256_hex(
            format!(
                "{operator_id}\n{}\n{created_ms}\n{text_sha256}\n{dedupe_key}",
                kind.as_str()
            )
            .as_bytes()
        )
    )
}

/// A verified notice row's typed facts.
struct VerifiedNotice {
    kind: NoticeKind,
    created_ms: u64,
    marker: Option<EngagementMarker>,
    show_by_ms: Option<u64>,
}

/// Bind the row to itself: bounded text, digest, length, kind and the recomputed id — under the
/// L3b formula for a plain notice, under the v2 formula (marker digest + show-by) for an engaging
/// one. Every read that hands text to a renderer goes through here, so a mutated row, a doctored
/// marker or a moved show-by bound is an error, never a line.
fn verify_notice_row(row: &NoticeRow, operator_id: &str) -> anyhow::Result<VerifiedNotice> {
    let kind = NoticeKind::parse(&row.kind)
        .ok_or_else(|| anyhow::anyhow!("notice kind failed validation"))?;
    let created_ms = u64::try_from(row.created_ms)
        .map_err(|_| anyhow::anyhow!("notice timestamp failed validation"))?;
    let chars = usize::try_from(row.chars).unwrap_or(usize::MAX);
    if row.operator_id != operator_id
        || bounded_notice_text(&row.text) != row.text
        || row.text.is_empty()
        || sha256_hex(row.text.as_bytes()) != row.text_sha256
        || row.text.chars().count() != chars
    {
        anyhow::bail!("notice row failed validation");
    }
    let (marker, show_by_ms) = match (&row.engagement_json, row.show_by_ms) {
        (None, None) => {
            if kind.is_engaging()
                || row.notice_id
                    != notice_id_for(
                        operator_id,
                        kind,
                        created_ms,
                        &row.text_sha256,
                        &row.dedupe_key,
                    )
            {
                anyhow::bail!("notice row failed validation");
            }
            (None, None)
        }
        (Some(stored), Some(show_by)) => {
            let marker = EngagementMarker::parse(stored)
                .ok_or_else(|| anyhow::anyhow!("engagement marker failed validation"))?;
            let show_by_ms = u64::try_from(show_by)
                .map_err(|_| anyhow::anyhow!("notice show-by failed validation"))?;
            if marker.kind != kind
                || show_by_ms <= created_ms
                || row.notice_id
                    != engaging_notice_id_for(
                        operator_id,
                        kind,
                        created_ms,
                        &row.text_sha256,
                        &row.dedupe_key,
                        &marker.digest(),
                        show_by_ms,
                    )
            {
                anyhow::bail!("engaging notice row failed validation");
            }
            (Some(marker), Some(show_by_ms))
        }
        _ => anyhow::bail!("engaging notice row failed validation"),
    };
    Ok(VerifiedNotice {
        kind,
        created_ms,
        marker,
        show_by_ms,
    })
}

/// L3b: the console notice queue.
impl RecipeStore {
    /// Every notice row for an operator, oldest first (`newest_first` flips it), unverified —
    /// callers verify each row before using anything from it.
    fn load_notice_rows(
        conn: &rusqlite::Connection,
        operator_id: &str,
        newest_first: bool,
        limit: Option<usize>,
    ) -> anyhow::Result<Vec<NoticeRow>> {
        let order = if newest_first {
            "ORDER BY created_ms DESC,notice_id DESC"
        } else {
            "ORDER BY created_ms,notice_id"
        };
        let sql = format!(
            "SELECT {NOTICE_COLUMNS} FROM mind_notices WHERE operator_id=?1 {order} LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;
        let limit = limit.map(|l| l as i64).unwrap_or(-1);
        let rows = stmt
            .query_map(rusqlite::params![operator_id, limit], notice_row_from)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Queue one bounded notice for an operator with its `queued` receipt in ONE transaction.
    /// A repeated dedupe key lands nothing and returns `Ok(None)`; empty text is refused.
    pub fn queue_notice(
        &self,
        operator_id: &str,
        kind: NoticeKind,
        text: &str,
        dedupe_key: &str,
        now_ms: u64,
    ) -> anyhow::Result<QueuedNotice> {
        if operator_id.is_empty() || dedupe_key.is_empty() || dedupe_key.len() > 200 {
            anyhow::bail!("notice requires an operator and a bounded dedupe key");
        }
        let text = bounded_notice_text(text);
        if text.is_empty() {
            anyhow::bail!("notice text is empty after bounding");
        }
        let chars = text.chars().count();
        let text_sha256 = sha256_hex(text.as_bytes());
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let duplicate: Option<NoticeRow> = tx
            .query_row(
                &format!(
                    "SELECT {NOTICE_COLUMNS} FROM mind_notices WHERE dedupe_key=?1 AND operator_id=?2"
                ),
                rusqlite::params![dedupe_key, operator_id],
                notice_row_from,
            )
            .optional()?;
        if let Some(row) = duplicate {
            let verified = verify_notice_row(&row, operator_id)?;
            return Ok(QueuedNotice {
                fresh: false,
                notice_id: row.notice_id,
                operator_id: operator_id.to_string(),
                kind: verified.kind,
                created_ms: verified.created_ms,
                chars: row.text.chars().count(),
                text_sha256: row.text_sha256,
                marker: verified.marker,
                show_by_ms: verified.show_by_ms,
            });
        }
        let notice_id = notice_id_for(operator_id, kind, now_ms, &text_sha256, dedupe_key);
        let created_ms = i64::try_from(now_ms)
            .map_err(|_| anyhow::anyhow!("notice timestamp is out of range"))?;
        tx.execute(
            "INSERT INTO mind_notices (notice_id,operator_id,kind,created_ms,text,text_sha256,chars,dedupe_key)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            rusqlite::params![
                notice_id,
                operator_id,
                kind.as_str(),
                created_ms,
                text,
                text_sha256,
                chars as i64,
                dedupe_key
            ],
        )?;
        let receipt = NoticeReceipt::issue(
            notice_id.clone(),
            operator_id,
            NoticeEvent::Queued,
            now_ms,
            None,
            None,
            None,
        )
        .ok_or_else(|| anyhow::anyhow!("queued notice receipt has an invalid shape"))?;
        Self::insert_notice_receipt(&tx, &receipt)?;
        tx.commit()?;
        Ok(QueuedNotice {
            fresh: true,
            notice_id,
            operator_id: operator_id.to_string(),
            kind,
            created_ms: now_ms,
            chars,
            text_sha256,
            marker: None,
            show_by_ms: None,
        })
    }

    /// L3c: queue an ENGAGING notice — one that predicts engagement — with its canonical marker
    /// and a show-by bound, in one transaction with its `queued` receipt. Refused: a kind that is
    /// not engaging, a marker of another kind, a bound at or before now, and (for a knock) any
    /// other knock for this operator on the same UTC day whose verified chain is not terminal —
    /// at most ONE outstanding knock per day, decided over receipts, not text. A repeated dedupe
    /// key names the existing notice (`fresh == false`) and writes nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn queue_engaging_notice(
        &self,
        operator_id: &str,
        kind: NoticeKind,
        text: &str,
        dedupe_key: &str,
        marker: &EngagementMarker,
        show_by_ms: u64,
        now_ms: u64,
    ) -> anyhow::Result<QueuedNotice> {
        if !kind.is_engaging() || marker.kind != kind || !marker.validate() {
            anyhow::bail!("an engaging notice needs a marker of its own kind");
        }
        if show_by_ms <= now_ms {
            anyhow::bail!("an engaging notice needs a show-by bound after now");
        }
        if operator_id.is_empty() || dedupe_key.is_empty() || dedupe_key.len() > 200 {
            anyhow::bail!("notice requires an operator and a bounded dedupe key");
        }
        let text = bounded_notice_text(text);
        if text.is_empty() {
            anyhow::bail!("notice text is empty after bounding");
        }
        let chars = text.chars().count();
        let text_sha256 = sha256_hex(text.as_bytes());
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        // Dedupe is per OPPORTUNITY, not forever: an existing row for this key is returned only
        // while its chain is non-terminal (queued or leased). An expired, shown or completed row
        // is history, and a fresh attempt is inserted under an attempt-suffixed key, so a line
        // that expired unshown can queue again on its next opportunity.
        // Attempts of this key: the exact base, or the base plus ":" and a numeric attempt —
        // matched in Rust, never with LIKE (`_` is a valid ref character and a LIKE wildcard).
        let prior: Vec<NoticeRow> = Self::load_notice_rows(&tx, operator_id, false, None)?
            .into_iter()
            .filter(|row| {
                row.dedupe_key == dedupe_key
                    || row
                        .dedupe_key
                        .strip_prefix(dedupe_key)
                        .and_then(|rest| rest.strip_prefix(':'))
                        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            })
            .collect();
        let mut attempts = 0usize;
        for row in prior {
            let verified = verify_notice_row(&row, operator_id)?;
            let receipts = Self::load_notice_chain(&tx, &row.notice_id, operator_id)?;
            let terminal = receipts.last().is_some_and(|r| {
                matches!(
                    r.event,
                    NoticeEvent::Shown | NoticeEvent::Expired | NoticeEvent::Committed
                )
            });
            if !terminal {
                return Ok(QueuedNotice {
                    fresh: false,
                    notice_id: row.notice_id,
                    operator_id: operator_id.to_string(),
                    kind: verified.kind,
                    created_ms: verified.created_ms,
                    chars: row.text.chars().count(),
                    text_sha256: row.text_sha256,
                    marker: verified.marker,
                    show_by_ms: verified.show_by_ms,
                });
            }
            attempts += 1;
        }
        let dedupe_key = if attempts == 0 {
            dedupe_key.to_string()
        } else {
            format!("{dedupe_key}:{attempts}")
        };
        let dedupe_key = dedupe_key.as_str();
        if kind == NoticeKind::Knock {
            let day = now_ms / 86_400_000;
            for row in Self::load_notice_rows(&tx, operator_id, false, None)? {
                let verified = verify_notice_row(&row, operator_id)?;
                if verified.kind != NoticeKind::Knock || verified.created_ms / 86_400_000 != day {
                    continue;
                }
                let receipts = Self::load_notice_chain(&tx, &row.notice_id, operator_id)?;
                let terminal = receipts.last().is_some_and(|r| {
                    matches!(
                        r.event,
                        NoticeEvent::Shown | NoticeEvent::Expired | NoticeEvent::Committed
                    )
                });
                if !terminal {
                    anyhow::bail!("one outstanding knock per operator per day");
                }
            }
        }
        let notice_id = engaging_notice_id_for(
            operator_id,
            kind,
            now_ms,
            &text_sha256,
            dedupe_key,
            &marker.digest(),
            show_by_ms,
        );
        let created_ms = i64::try_from(now_ms)
            .map_err(|_| anyhow::anyhow!("notice timestamp is out of range"))?;
        let show_by = i64::try_from(show_by_ms)
            .map_err(|_| anyhow::anyhow!("notice show-by is out of range"))?;
        tx.execute(
            &format!(
                "INSERT INTO mind_notices ({NOTICE_COLUMNS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)"
            ),
            rusqlite::params![
                notice_id,
                operator_id,
                kind.as_str(),
                created_ms,
                text,
                text_sha256,
                chars as i64,
                dedupe_key,
                marker.canonical_json(),
                show_by
            ],
        )?;
        let receipt = NoticeReceipt::issue(
            notice_id.clone(),
            operator_id,
            NoticeEvent::Queued,
            now_ms,
            None,
            None,
            None,
        )
        .ok_or_else(|| anyhow::anyhow!("queued notice receipt has an invalid shape"))?;
        Self::insert_notice_receipt(&tx, &receipt)?;
        tx.commit()?;
        Ok(QueuedNotice {
            fresh: true,
            notice_id,
            operator_id: operator_id.to_string(),
            kind,
            created_ms: now_ms,
            chars,
            text_sha256,
            marker: Some(marker.clone()),
            show_by_ms: Some(show_by_ms),
        })
    }

    /// L3c: write the terminal `expired` receipt for every engaging notice of the operator whose
    /// show-by bound has passed without a `shown` and without a live lease. Idempotent. Called by
    /// the lease path and by the runner's housekeeping beat, so a cockpit that never returns
    /// still lets the day's knock slot free up.
    pub fn sweep_engaging_expiry(&self, operator_id: &str, now_ms: u64) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let expired = Self::sweep_expiry_on(&tx, operator_id, now_ms)?;
        tx.commit()?;
        Ok(expired)
    }

    fn sweep_expiry_on(
        tx: &rusqlite::Transaction<'_>,
        operator_id: &str,
        now_ms: u64,
    ) -> anyhow::Result<usize> {
        let mut expired = 0usize;
        for row in Self::load_notice_rows(tx, operator_id, false, None)? {
            let verified = verify_notice_row(&row, operator_id)?;
            let Some(show_by) = verified.show_by_ms else {
                continue;
            };
            if now_ms <= show_by {
                continue;
            }
            let receipts = Self::load_notice_chain(tx, &row.notice_id, operator_id)?;
            let Some(last) = receipts.last() else {
                anyhow::bail!("notice without a queued receipt");
            };
            let terminal = matches!(
                last.event,
                NoticeEvent::Shown | NoticeEvent::Expired | NoticeEvent::Committed
            );
            let live_lease = last.event == NoticeEvent::Leased
                && last.lease_until_ms.is_some_and(|until| until > now_ms);
            if terminal || live_lease {
                continue;
            }
            let receipt = NoticeReceipt::issue(
                row.notice_id.clone(),
                operator_id,
                NoticeEvent::Expired,
                now_ms,
                None,
                None,
                Some(last.receipt_sha256.clone()),
            )
            .ok_or_else(|| anyhow::anyhow!("expired notice receipt has an invalid shape"))?;
            Self::insert_notice_receipt(tx, &receipt)?;
            expired += 1;
        }
        Ok(expired)
    }

    /// L3c: the durable outbox — every engaging notice that was SHOWN and whose prediction has
    /// not yet been marked committed. A completed item (last receipt `committed`) never comes
    /// back, however old, so the reconciler can neither replay nor cycle.
    pub fn shown_engagements(&self, operator_id: &str) -> anyhow::Result<Vec<ShownEngagement>> {
        let conn = self.conn.lock().unwrap();
        let mut out = Vec::new();
        for row in Self::load_notice_rows(&conn, operator_id, false, None)? {
            let verified = verify_notice_row(&row, operator_id)?;
            let Some(marker) = verified.marker else {
                continue;
            };
            let receipts = Self::load_notice_chain(&conn, &row.notice_id, operator_id)?;
            if let Some(last) = receipts.last() {
                if last.event == NoticeEvent::Shown {
                    out.push(ShownEngagement {
                        notice_id: row.notice_id,
                        shown_ms: last.occurred_at_ms,
                        marker,
                    });
                }
            }
        }
        Ok(out)
    }

    /// L3c: durable outbox completion — append the `committed` receipt after `shown`. Idempotent:
    /// an item already completed returns `Ok(false)`; anything else than a shown engaging notice
    /// is an error, so completion can never be written for a line nobody saw.
    pub fn mark_engagement_committed(&self, notice_id: &str, now_ms: u64) -> anyhow::Result<bool> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let row: Option<NoticeRow> = tx
            .query_row(
                &format!("SELECT {NOTICE_COLUMNS} FROM mind_notices WHERE notice_id=?1"),
                [notice_id],
                notice_row_from,
            )
            .optional()?;
        let Some(row) = row else {
            anyhow::bail!("no notice matches that id");
        };
        let operator_id = row.operator_id.clone();
        let verified = verify_notice_row(&row, &operator_id)?;
        if verified.marker.is_none() {
            anyhow::bail!("only an engaging notice completes an engagement");
        }
        let receipts = Self::load_notice_chain(&tx, notice_id, &operator_id)?;
        let Some(last) = receipts.last() else {
            anyhow::bail!("notice without a queued receipt");
        };
        match last.event {
            NoticeEvent::Committed => return Ok(false),
            NoticeEvent::Shown => {}
            _ => anyhow::bail!("an engagement completes only after it was shown"),
        }
        let receipt = NoticeReceipt::issue(
            notice_id,
            operator_id.as_str(),
            NoticeEvent::Committed,
            now_ms.max(last.occurred_at_ms),
            None,
            None,
            Some(last.receipt_sha256.clone()),
        )
        .ok_or_else(|| anyhow::anyhow!("committed notice receipt has an invalid shape"))?;
        Self::insert_notice_receipt(&tx, &receipt)?;
        tx.commit()?;
        Ok(true)
    }

    /// Lease up to `limit` unseen notices for one operator, oldest first, each under a fresh
    /// lease that expires at `now + lease_ms`. Rows under a live lease or already shown are
    /// skipped; a corrupt chain fails the read closed. ONE transaction.
    pub fn lease_notices(
        &self,
        operator_id: &str,
        now_ms: u64,
        lease_ms: u64,
        limit: usize,
    ) -> anyhow::Result<Vec<LeasedNotice>> {
        if lease_ms == 0 {
            anyhow::bail!("a notice lease needs a positive duration");
        }
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        // L3c: an engaging notice past its show-by bound expires before anything is leased.
        Self::sweep_expiry_on(&tx, operator_id, now_ms)?;
        let rows = Self::load_notice_rows(&tx, operator_id, false, None)?;
        let mut leased = Vec::new();
        for row in rows {
            if leased.len() >= limit {
                break;
            }
            // The row and its chain are verified whole before anything is read from them; a
            // forged `shown` row cannot hide a notice, and a mutated text cannot be rendered.
            let verified = verify_notice_row(&row, operator_id)?;
            let receipts = Self::load_notice_chain(&tx, &row.notice_id, operator_id)?;
            let Some(last) = receipts.last() else {
                anyhow::bail!("notice without a queued receipt");
            };
            if matches!(
                last.event,
                NoticeEvent::Shown | NoticeEvent::Expired | NoticeEvent::Committed
            ) {
                continue;
            }
            if last.event == NoticeEvent::Leased
                && last.lease_until_ms.is_some_and(|until| until > now_ms)
            {
                continue;
            }
            if verified.show_by_ms.is_some_and(|show_by| now_ms > show_by) {
                continue;
            }
            let lease_id = sha256_hex(
                format!(
                    "lease\n{}\n{now_ms}\n{}",
                    row.notice_id, last.receipt_sha256
                )
                .as_bytes(),
            )[..16]
                .to_string();
            let lease_until_ms = now_ms.saturating_add(lease_ms);
            let receipt = NoticeReceipt::issue(
                row.notice_id.clone(),
                operator_id,
                NoticeEvent::Leased,
                now_ms,
                Some(lease_id.clone()),
                Some(lease_until_ms),
                Some(last.receipt_sha256.clone()),
            )
            .ok_or_else(|| anyhow::anyhow!("leased notice receipt has an invalid shape"))?;
            Self::insert_notice_receipt(&tx, &receipt)?;
            leased.push(LeasedNotice {
                notice_id: row.notice_id,
                lease_id,
                lease_until_ms,
                kind: verified.kind,
                text: row.text,
                created_ms: verified.created_ms,
            });
        }
        tx.commit()?;
        Ok(leased)
    }

    /// Acknowledge a paint: the chain's last receipt must be THIS live lease. `shown_now` is true
    /// when this call wrote the one `shown` receipt; false when this same lease had already
    /// acknowledged — then the ORIGINAL `shown_ms` and marker come back, so an interrupted commit
    /// can be finished without moving the clock. Any other shape — unknown notice, not leased,
    /// foreign or expired lease, an expired notice — is an error.
    pub fn ack_notice_shown(
        &self,
        notice_id: &str,
        lease_id: &str,
        now_ms: u64,
    ) -> anyhow::Result<NoticeAck> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let row: Option<NoticeRow> = tx
            .query_row(
                &format!("SELECT {NOTICE_COLUMNS} FROM mind_notices WHERE notice_id=?1"),
                [notice_id],
                notice_row_from,
            )
            .optional()?;
        let Some(row) = row else {
            anyhow::bail!("no notice matches that id");
        };
        let operator_id = row.operator_id.clone();
        let verified = verify_notice_row(&row, &operator_id)?;
        let receipts = Self::load_notice_chain(&tx, notice_id, &operator_id)?;
        let Some(last) = receipts.last() else {
            anyhow::bail!("notice without a queued receipt");
        };
        match last.event {
            NoticeEvent::Shown | NoticeEvent::Committed => {
                let shown = receipts
                    .iter()
                    .find(|r| r.event == NoticeEvent::Shown)
                    .unwrap_or(last);
                if shown.lease_id.as_deref() == Some(lease_id) {
                    return Ok(NoticeAck {
                        shown_now: false,
                        shown_ms: shown.occurred_at_ms,
                        kind: verified.kind,
                        marker: verified.marker,
                    });
                }
                anyhow::bail!("notice already shown under another lease");
            }
            NoticeEvent::Expired => anyhow::bail!("notice expired unshown"),
            NoticeEvent::Queued => anyhow::bail!("notice is not leased"),
            NoticeEvent::Leased => {
                if last.lease_id.as_deref() != Some(lease_id) {
                    anyhow::bail!("lease does not match the live lease");
                }
                if !last.lease_until_ms.is_some_and(|until| until > now_ms) {
                    anyhow::bail!("lease expired: lease the notice again");
                }
            }
        }
        let receipt = NoticeReceipt::issue(
            notice_id,
            operator_id.as_str(),
            NoticeEvent::Shown,
            now_ms,
            Some(lease_id.to_string()),
            None,
            Some(last.receipt_sha256.clone()),
        )
        .ok_or_else(|| anyhow::anyhow!("shown notice receipt has an invalid shape"))?;
        Self::insert_notice_receipt(&tx, &receipt)?;
        tx.commit()?;
        Ok(NoticeAck {
            shown_now: true,
            shown_ms: now_ms,
            kind: verified.kind,
            marker: verified.marker,
        })
    }

    /// Every notice for an operator, newest first, each with its verified chain. A corrupt chain
    /// is an error, never a hidden row.
    pub fn notice_history(
        &self,
        operator_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<NoticeHistoryEntry>> {
        let conn = self.conn.lock().unwrap();
        let rows = Self::load_notice_rows(&conn, operator_id, true, Some(limit))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let verified = verify_notice_row(&row, operator_id)?;
            let receipts = Self::load_notice_chain(&conn, &row.notice_id, operator_id)?;
            out.push(NoticeHistoryEntry {
                notice: QueuedNotice {
                    fresh: false,
                    notice_id: row.notice_id,
                    operator_id: operator_id.to_string(),
                    kind: verified.kind,
                    created_ms: verified.created_ms,
                    chars: row.text.chars().count(),
                    text_sha256: row.text_sha256,
                    marker: verified.marker,
                    show_by_ms: verified.show_by_ms,
                },
                receipts,
            });
        }
        Ok(out)
    }

    /// `(unseen, under a live lease)` for one operator — the `why deliveries` numbers.
    pub fn notice_queue_depth(
        &self,
        operator_id: &str,
        now_ms: u64,
    ) -> anyhow::Result<(usize, usize)> {
        let conn = self.conn.lock().unwrap();
        let rows = Self::load_notice_rows(&conn, operator_id, false, None)?;
        let mut unseen = 0usize;
        let mut live = 0usize;
        for row in rows {
            verify_notice_row(&row, operator_id)?;
            let receipts = Self::load_notice_chain(&conn, &row.notice_id, operator_id)?;
            let Some(last) = receipts.last() else {
                anyhow::bail!("notice without a queued receipt");
            };
            if matches!(
                last.event,
                NoticeEvent::Shown | NoticeEvent::Expired | NoticeEvent::Committed
            ) {
                continue;
            }
            unseen += 1;
            if last.event == NoticeEvent::Leased && last.lease_until_ms.is_some_and(|u| u > now_ms)
            {
                live += 1;
            }
        }
        Ok((unseen, live))
    }

    fn insert_notice_receipt(
        tx: &rusqlite::Transaction<'_>,
        receipt: &NoticeReceipt,
    ) -> anyhow::Result<()> {
        let occurred_ms = i64::try_from(receipt.occurred_at_ms)
            .map_err(|_| anyhow::anyhow!("notice receipt timestamp is out of range"))?;
        tx.execute(
            "INSERT INTO mind_notice_receipts (notice_id,operator_id,event,receipt_json,receipt_sha256,occurred_ms)
             VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![
                receipt.notice_id,
                receipt.operator_id,
                receipt.event.as_str(),
                serde_json::to_string(receipt)?,
                receipt.receipt_sha256,
                occurred_ms
            ],
        )?;
        Ok(())
    }

    /// Read and verify one notice's chain: opens with `queued`, links by digest, and nothing
    /// follows `shown`.
    fn load_notice_chain(
        conn: &rusqlite::Connection,
        notice_id: &str,
        operator_id: &str,
    ) -> anyhow::Result<Vec<NoticeReceipt>> {
        let mut stmt = conn.prepare(
            "SELECT event,receipt_json,receipt_sha256 FROM mind_notice_receipts
             WHERE notice_id=?1 ORDER BY id",
        )?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([notice_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut receipts: Vec<NoticeReceipt> = Vec::with_capacity(rows.len());
        for (event, receipt_json, stored_sha256) in rows {
            let receipt: NoticeReceipt = serde_json::from_str(&receipt_json)?;
            let previous = receipts.last();
            let opens = receipts.is_empty();
            if receipt.notice_id != notice_id
                || receipt.operator_id != operator_id
                || receipt.event.as_str() != event
                || receipt.receipt_sha256 != stored_sha256
                || !receipt.verify()
                || (opens != (receipt.event == NoticeEvent::Queued))
                || receipt.previous_receipt_sha256.as_deref()
                    != previous.map(|p| p.receipt_sha256.as_str())
                || previous.is_some_and(|p| {
                    matches!(p.event, NoticeEvent::Expired | NoticeEvent::Committed)
                })
                || (previous.is_some_and(|p| p.event == NoticeEvent::Shown)
                    && receipt.event != NoticeEvent::Committed)
            {
                anyhow::bail!("notice receipt chain failed validation");
            }
            // Transition semantics, not only shape: time never moves backward; a lease may be
            // taken again only at or after the previous lease's expiry; `shown` closes the SAME
            // live lease it follows and nothing else.
            if let Some(prev) = previous {
                let semantic_ok = receipt.occurred_at_ms >= prev.occurred_at_ms
                    && match receipt.event {
                        NoticeEvent::Queued => false,
                        NoticeEvent::Leased => match prev.event {
                            NoticeEvent::Queued => true,
                            NoticeEvent::Leased => prev
                                .lease_until_ms
                                .is_some_and(|until| receipt.occurred_at_ms >= until),
                            NoticeEvent::Shown | NoticeEvent::Expired | NoticeEvent::Committed => {
                                false
                            }
                        },
                        NoticeEvent::Shown => {
                            prev.event == NoticeEvent::Leased
                                && prev.lease_id == receipt.lease_id
                                && prev
                                    .lease_until_ms
                                    .is_some_and(|until| receipt.occurred_at_ms < until)
                        }
                        // Expired follows queued, or a lease that had already run out.
                        NoticeEvent::Expired => match prev.event {
                            NoticeEvent::Queued => true,
                            NoticeEvent::Leased => prev
                                .lease_until_ms
                                .is_some_and(|until| receipt.occurred_at_ms >= until),
                            _ => false,
                        },
                        // Committed follows shown and nothing else.
                        NoticeEvent::Committed => prev.event == NoticeEvent::Shown,
                    };
                if !semantic_ok {
                    anyhow::bail!("notice receipt chain failed transition validation");
                }
            }
            receipts.push(receipt);
        }
        Ok(receipts)
    }

    /// Test-only (L3b): insert a receipt row exactly as given, bypassing every semantic check.
    #[cfg(test)]
    pub(crate) fn insert_receipt_for_test(&self, receipt: &NoticeReceipt) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        Self::insert_notice_receipt(&tx, receipt)?;
        tx.commit()?;
        Ok(())
    }

    /// Test-only (L3b): mutate a stored notice's text under an otherwise valid chain.
    #[cfg(test)]
    pub(crate) fn mutate_notice_text_for_test(&self, notice_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE mind_notices SET text=text||'!' WHERE notice_id=?1",
            [notice_id],
        )?;
        Ok(())
    }

    /// Test-only (L3b): the last receipt's digest, to forge a successor from.
    #[cfg(test)]
    pub(crate) fn last_receipt_sha_for_test(&self, notice_id: &str) -> anyhow::Result<String> {
        let conn = self.conn.lock().unwrap();
        Ok(conn.query_row(
            "SELECT receipt_sha256 FROM mind_notice_receipts WHERE notice_id=?1 ORDER BY id DESC LIMIT 1",
            [notice_id],
            |row| row.get(0),
        )?)
    }

    /// Test-only (L3c): move an engaging notice's show-by bound under a still-valid chain.
    #[cfg(test)]
    pub(crate) fn shift_show_by_for_test(&self, notice_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE mind_notices SET show_by_ms=show_by_ms+1 WHERE notice_id=?1",
            [notice_id],
        )?;
        Ok(())
    }

    /// Test-only (L3c): smuggle a well-formed marker onto a plain (L3b-identity) row.
    #[cfg(test)]
    pub(crate) fn smuggle_marker_for_test(&self, notice_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        let marker = EngagementMarker::ask("name", 300).expect("valid marker");
        conn.execute(
            "UPDATE mind_notices SET engagement_json=?2, show_by_ms=9999999999 WHERE notice_id=?1",
            rusqlite::params![notice_id, marker.canonical_json()],
        )?;
        Ok(())
    }

    /// Test-only (L3b): a second `shown` row by raw write must be refused by the store's index.
    #[cfg(test)]
    pub(crate) fn duplicate_shown_row_for_test(&self, notice_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO mind_notice_receipts(notice_id,operator_id,event,receipt_json,receipt_sha256,occurred_ms)
             SELECT notice_id,operator_id,event,receipt_json,receipt_sha256||'x',occurred_ms
             FROM mind_notice_receipts WHERE notice_id=?1 AND event='shown'",
            [notice_id],
        )?;
        Ok(())
    }

    /// Test-only (L3b): alter a stored receipt's digest column so the chain no longer verifies.
    #[cfg(test)]
    pub(crate) fn tamper_notice_receipt_for_test(&self, notice_id: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE mind_notice_receipts SET receipt_sha256=substr(receipt_sha256,2)||'0'
             WHERE notice_id=?1",
            [notice_id],
        )?;
        Ok(())
    }
}
