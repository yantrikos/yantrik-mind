//! The `MemoryFacade` — the async, Send+Sync firewall over the `!Sync` YantrikDB. Every module
//! reaches memory ONLY through this and gets owned DTOs back, never a `&YantrikDB`. `mind-memory`
//! is the sole implementor and the sole writer to the cognitive graph.
use crate::clock::UnixMillis;
use crate::error::Result;
use crate::task::Task;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The typed cognitive kinds we surface (subset/projection of yantrikdb-core NodeKinds).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MemoryKind {
    Belief,
    Goal,
    Constraint,
    Preference,
    Risk,
    Task,
    Opportunity,
    Need,
    Episode,
    Entity,
    Routine,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Belief {
    pub id: String,
    pub statement: String,
    pub confidence: f64, // [0,1] posterior
    pub certainty: f64,
    pub provenance: String, // observed/inferred/told/...
    pub evidence_count: u32,
    pub updated_ms: UnixMillis,
    pub status: String, // active/contradicted/...
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: String,
    pub belief_id: String,
    pub source_event: Option<String>,
    pub weight: f64,
    pub polarity: f64, // -1..1 (against..for)
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contradiction {
    pub id: String,
    pub belief_a: String,
    pub belief_b: String,
    pub severity: f64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub id: String,
    pub kind: MemoryKind,
    pub text: String,
    pub confidence: f64,
    pub certainty: f64,
    pub updated_ms: UnixMillis,
}

/// The retrieval/ranking moat bundle hydrated for a turn — this is where the moat lives in
/// conversation. Built by `WorkingSetHydrator` in `mind-memory`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkingSet {
    pub stable_facts: Vec<MemoryItem>,
    pub uncertain_beliefs: Vec<Belief>,
    pub active_contradictions: Vec<Contradiction>,
    pub recent_events: Vec<MemoryItem>,
    pub preferences: Vec<MemoryItem>,
    pub commitments: Vec<MemoryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallQuery {
    pub text: String,
    pub top_k: usize,
    pub kind: Option<MemoryKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recalled {
    pub item: MemoryItem,
    pub score: f64,
    pub why: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeliefAssertion {
    pub statement: String,
    pub polarity: f64, // evidence direction
    pub weight: f64,   // evidence strength (likelihood ratio-ish)
    pub source_event: Option<String>,
    pub provenance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reflection {
    pub summary: String,
    pub beliefs: Vec<Belief>,
    pub open_conflicts: Vec<Contradiction>,
}

/// A reusable code-tool the mind authored, vetted in the sandbox, and banked for recall. Stored in
/// YantrikDB. Reuse ALWAYS runs through the sandbox — promotion grants recallability, never authority.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Skill {
    pub name: String,
    pub lang: String, // "python" | "shell" | "rust"
    pub code: String,
    /// What it does (used for recall) — should be system/verifier-generated, not raw author prose.
    pub summary: String,
    pub tags: Vec<String>,
    pub status: String, // "candidate" | "active" | "quarantined"
    pub runs: u64,
    pub successes: u64,
    pub created_ms: u64,
}

impl Skill {
    pub fn success_rate(&self) -> f64 {
        if self.runs == 0 { 1.0 } else { self.successes as f64 / self.runs as f64 }
    }
}

#[async_trait]
pub trait MemoryFacade: Send + Sync {
    /// Typed + semantic + temporal recall (multi-signal).
    async fn recall_typed(&self, q: RecallQuery) -> Result<Vec<Recalled>>;
    /// Assert evidence for/against a belief; runs Bayesian revision under the hood.
    async fn remember_as_belief(&self, a: BeliefAssertion) -> Result<Belief>;
    /// Write a machine-derived OBSERVATION (skill/tool/sub-agent/web output) — provenance-tagged,
    /// secret-scanned, NEVER a naked Belief. This is the gated inward boundary for the moat.
    async fn remember_observation(&self, text: &str, source: crate::safety::ProvenanceCategory) -> Result<String>;
    /// Create/strengthen a graph edge between entities.
    async fn relate(&self, src: &str, dst: &str, rel: &str, weight: f64) -> Result<()>;
    /// Compose typed recalls + open conflicts into a structured reflection.
    async fn reflect(&self, question: &str) -> Result<Reflection>;
    /// Currently-open contradictions across stored beliefs.
    async fn conflicts(&self) -> Result<Vec<Contradiction>>;
    /// A belief plus its evidence trail (provenance).
    async fn explain_belief(&self, belief_id: &str) -> Result<Option<(Belief, Vec<Evidence>)>>;
    /// Build the typed working-set for a focus/turn.
    async fn hydrate_working_set(&self, focus: &str) -> Result<WorkingSet>;
    /// Consolidate aging turns into typed memory (provenance-preserving). Returns #created.
    async fn consolidate(&self) -> Result<usize>;
    /// Privacy: forget a memory by id.
    async fn forget(&self, id: &str) -> Result<bool>;
    /// Privacy: export everything (JSON).
    async fn export(&self) -> Result<String>;

    // ── cheap task tier (plain CRUD, no cognitive cost) ──
    async fn add_task(&self, description: &str, priority: &str, due_ms: Option<u64>) -> Result<Task>;
    async fn list_tasks(&self, include_done: bool) -> Result<Vec<Task>>;
    async fn complete_task(&self, id: &str) -> Result<bool>;

    // ── skill library (code-tools the mind banks + reuses; reuse always runs in the sandbox) ──
    /// Save/replace a vetted skill (code is secret-scanned by the write-gate). Returns Err if gated.
    async fn save_skill(&self, skill: Skill) -> Result<()>;
    /// Fetch a skill by exact name.
    async fn get_skill(&self, name: &str) -> Result<Option<Skill>>;
    /// All skills (for "what can you do?").
    async fn list_skills(&self) -> Result<Vec<Skill>>;
    /// Recall skills relevant to a task (ranked by name/summary/tag match).
    async fn recall_skills(&self, query: &str, limit: usize) -> Result<Vec<Skill>>;
    /// Record a run outcome → updates runs/successes; auto-quarantines a flaky skill.
    async fn record_skill_outcome(&self, name: &str, success: bool) -> Result<()>;

    // ── cheap raw transcript (immediate conversational context; NOT knowledge) ──
    /// Append a raw chat line (role = "user" | "assistant").
    async fn append_message(&self, role: &str, text: &str) -> Result<()>;
    /// The most recent chat lines in chronological order: Vec<(role, text)>.
    async fn recent_messages(&self, limit: usize) -> Result<Vec<(String, String)>>;
    /// Transcript lines with id > `after_id`, ascending: Vec<(id, role, text)>. For the consolidation
    /// pass, which advances a cursor over what it has already distilled into typed memory.
    async fn messages_since(&self, after_id: i64, limit: usize) -> Result<Vec<(i64, String, String)>>;
}
