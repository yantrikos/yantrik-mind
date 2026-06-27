//! Recipe persistence — durable run state in SQLite, lifted in spirit from the original engine's
//! `RecipeStore`. One row per run (status + current_step + steps + vars), so a run that was mid-
//! flight when the process died can be recovered. Its own connection to the DB file (separate from
//! the memory actor) keeps this a leaf — the recipe tables live alongside the cognitive ones.
//!
//! Crash discipline (carried over): on recovery, an interrupted step is re-run ONLY if it's
//! idempotent; a non-idempotent step (an `Act`/send) is failed-visibly, never blind-replayed.

use std::collections::HashMap;
use std::sync::Mutex;

use rusqlite::Connection;
use serde_json::Value;

use crate::RecipeStep;

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
            )",
        )?;
        Ok(Self { conn: Mutex::new(conn) })
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

    /// Runs that were `running` when the process stopped — candidates for recovery.
    pub fn resumable(&self) -> Vec<RunRecord> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match conn.prepare(
            "SELECT id,name,status,current_step,steps_json,vars_json,error FROM mind_recipe_runs WHERE status='running'",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
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
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }
}
