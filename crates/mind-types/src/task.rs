//! `Task` — the CHEAP tier. Operational todos are plain typed nodes (stored/queried with bare
//! INSERT/SELECT, zero cognitive cost — no embedding, no revision, no contradiction scan). They
//! live in the SAME store as the cognitive graph (one identity chain, no second brain), and can be
//! *promoted* to cognitive reasoning (related to goals/beliefs, conflict-checked) only when an
//! item actually needs it. Cost is paid where reasoning happens, not for bookkeeping.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: String,   // pending | in_progress | completed | cancelled | blocked
    pub priority: String, // low | medium | high | critical
    pub due_ms: Option<u64>,
}

impl Task {
    pub fn is_open(&self) -> bool {
        !matches!(self.status.as_str(), "completed" | "cancelled")
    }
}
