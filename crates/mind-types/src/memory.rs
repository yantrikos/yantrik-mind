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

#[async_trait]
pub trait MemoryFacade: Send + Sync {
    /// Typed + semantic + temporal recall (multi-signal).
    async fn recall_typed(&self, q: RecallQuery) -> Result<Vec<Recalled>>;
    /// Assert evidence for/against a belief; runs Bayesian revision under the hood.
    async fn remember_as_belief(&self, a: BeliefAssertion) -> Result<Belief>;
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
}
