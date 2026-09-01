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
    GoalCheckpoint, HorizonControlAction, HorizonControlReceipt, HorizonRun, OutcomeReceipt,
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
    SegmentContract,
    ActionLedger,
    AssumptionObservation,
    StatePersistence,
}

impl HorizonFailureReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::CheckpointValidation => "checkpoint_validation_failed",
            Self::SegmentContract => "segment_contract_failed",
            Self::ActionLedger => "action_ledger_failed",
            Self::AssumptionObservation => "assumption_observation_failed",
            Self::StatePersistence => "state_persistence_failed",
        }
    }
}

const HORIZON_FAILURE_CODES: &[&str] = &[
    HorizonFailureReason::CheckpointValidation.as_str(),
    HorizonFailureReason::SegmentContract.as_str(),
    HorizonFailureReason::ActionLedger.as_str(),
    HorizonFailureReason::AssumptionObservation.as_str(),
    HorizonFailureReason::StatePersistence.as_str(),
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
}

pub struct RecipeStore {
    conn: Mutex<Connection>,
}

pub struct ActiveHorizonRecord {
    pub run: HorizonRun,
    pub wake_at_ms: Option<u64>,
    pub queue_status: Option<String>,
    /// A bounded code owned by the scheduler. Raw tool/backend errors never reach operator views.
    pub failure_reason: Option<String>,
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
            )",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn save(&self, r: &RunRecord, now_ms: u64) -> anyhow::Result<()> {
        let steps = serde_json::to_string(&r.steps)?;
        let vars = serde_json::to_string(&r.vars)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO mind_recipe_runs (id,name,status,current_step,steps_json,vars_json,error,updated_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET
                status=excluded.status, current_step=excluded.current_step, steps_json=excluded.steps_json,
                vars_json=excluded.vars_json, error=excluded.error, updated_ms=excluded.updated_ms",
            rusqlite::params![r.id, r.name, r.status, r.current_step as i64, steps, vars, r.error, now_ms as i64],
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
    pub fn load(&self, id: &str) -> Option<RunRecord> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id,name,status,current_step,steps_json,vars_json,error FROM mind_recipe_runs WHERE id=?1",
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
            "SELECT id,name,status,current_step,steps_json,vars_json,error FROM mind_recipe_runs WHERE status='sleeping'",
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
            "SELECT id,name,status,current_step,steps_json,vars_json,error FROM mind_recipe_runs WHERE status=?1",
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
        HorizonRun::resume(checkpoint, checkpoint.created_at_ms)
            .map_err(|error| anyhow::anyhow!("invalid horizon checkpoint: {error:?}"))?;
        let checkpoint_json = serde_json::to_string(checkpoint)?;
        let created_ms = i64::try_from(checkpoint.created_at_ms)
            .map_err(|_| anyhow::anyhow!("horizon checkpoint timestamp is out of range"))?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
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
        tx.commit()?;
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
            active.push(ActiveHorizonRecord {
                run,
                wake_at_ms,
                queue_status,
                failure_reason,
            });
        }
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

    /// Atomically replace an active checkpoint with an immutable, verified completion receipt.
    /// A different receipt for the same goal can never overwrite the first terminal outcome.
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
        let has_checkpoint: bool = tx
            .query_row(
                "SELECT 1 FROM mind_horizon_checkpoints WHERE goal_id=?1",
                [&job.goal_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        let has_outcome: bool = tx
            .query_row(
                "SELECT 1 FROM mind_horizon_outcomes WHERE goal_id=?1",
                [&job.goal_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !has_checkpoint || has_outcome {
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
        tx.commit()?;
        Ok(())
    }

    /// Atomically claim a small batch so overlapping scheduler ticks cannot run the same segment.
    pub fn claim_due_horizon_jobs(&self, now_ms: u64) -> anyhow::Result<Vec<HorizonJob>> {
        let now_ms = i64::try_from(now_ms)
            .map_err(|_| anyhow::anyhow!("horizon tick timestamp is out of range"))?;
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let mut stmt = tx.prepare(
            "SELECT goal_id,job_json FROM mind_horizon_jobs
             WHERE status='pending' AND wake_ms<=?1 ORDER BY wake_ms,goal_id LIMIT 8",
        )?;
        let rows: Vec<(String, String)> = stmt
            .query_map([now_ms], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);
        let mut jobs = Vec::with_capacity(rows.len());
        for (goal_id, job_json) in rows {
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
                jobs.push(job);
            }
        }
        tx.commit()?;
        Ok(jobs)
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
    ) -> anyhow::Result<()> {
        // This field is operator-visible, so callers can provide only this typed, code-owned
        // vocabulary. Free-text backend errors have no type-correct route into persistence.
        let reason_code = reason.as_str();
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute(
            "UPDATE mind_horizon_jobs SET status='failed',error=?2
             WHERE goal_id=?1 AND status='running'",
            rusqlite::params![goal_id, reason_code],
        )?;
        if changed != 1 {
            anyhow::bail!("horizon failure status did not match one running job");
        }
        Ok(())
    }

    /// All scheduler jobs are validated read-only segments, so a process crash may safely return a
    /// claimed-but-unfinished row to the pending queue. The HorizonRun action id deduplicates the
    /// case where the checkpoint committed but the job deletion did not.
    pub fn recover_horizon_jobs(&self) -> usize {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE mind_horizon_jobs SET status='pending',error=NULL WHERE status='running'",
            [],
        )
        .unwrap_or(0)
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
            .fail_horizon_job(&original.goal_id, HorizonFailureReason::SegmentContract)
            .is_err());
        assert_eq!(store.claim_due_horizon_jobs(start + 20).unwrap().len(), 1);
        store
            .fail_horizon_job(&original.goal_id, HorizonFailureReason::SegmentContract)
            .unwrap();
        assert!(store
            .fail_horizon_job(&original.goal_id, HorizonFailureReason::ActionLedger)
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
            .fail_horizon_job("goal:missing", HorizonFailureReason::SegmentContract)
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
            .fail_horizon_job(&original.goal_id, HorizonFailureReason::SegmentContract)
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
}
