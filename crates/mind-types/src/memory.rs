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

/// Why a belief landed in the uncertain bucket — the specific epistemic cause, not a generic hedge.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum UncertaintyReason {
    /// Confidence fell over time via exponential half-life decay.
    Decayed,
    /// Belief has an active contradiction with another stored belief.
    Contradicted,
    /// Fewer than two pieces of evidence — not enough to anchor confidently.
    Sparse,
    /// The asserted prior was already below the stable threshold; no single cause dominates.
    LowPrior,
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
    /// Set when this belief lives in `WorkingSet::uncertain_beliefs`; None for all other uses.
    #[serde(default)]
    pub uncertainty_reason: Option<UncertaintyReason>,
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
    #[serde(default)]
    pub evidence_count: u32,
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

/// Who can see a memory / who is reading it. The household read-isolation primitive: a private fact
/// from one person must NEVER surface to another. (See the surprise-gift adversarial test.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// Visible to all household members (a shared/group fact).
    Shared,
    /// Visible ONLY to this person id (a private-DM fact).
    Private(String),
}

/// The primary household member (the companion's owner). Legacy/untagged memory is private to them,
/// so pre-multi-user facts never leak to a later-added member.
pub const PRIMARY: &str = "primary";

impl Scope {
    /// The primary member's private scope.
    pub fn primary() -> Scope {
        Scope::Private(PRIMARY.to_string())
    }
    /// Storage form: "shared" or "private:<owner>".
    pub fn as_tag(&self) -> String {
        match self {
            Scope::Shared => "shared".into(),
            Scope::Private(o) => format!("private:{o}"),
        }
    }
    pub fn parse(tag: &str) -> Scope {
        match tag.strip_prefix("private:") {
            Some(o) => Scope::Private(o.to_string()),
            None => Scope::Shared,
        }
    }
    /// Can `viewer` see an item stored with `stored` scope tag? Shared → everyone; Private → only the
    /// owner. An untagged/legacy item (stored=None) is private to the PRIMARY (so old single-user facts
    /// never leak to a later-added member). `None` viewer = unrestricted (system/single-user).
    pub fn visible_to(stored: Option<&str>, viewer: Option<&Scope>) -> bool {
        let viewer = match viewer {
            None => return true, // unrestricted
            Some(v) => v,
        };
        match stored.map(Scope::parse) {
            None => matches!(viewer, Scope::Private(v) if v == PRIMARY), // legacy → primary only
            Some(Scope::Shared) => true,
            Some(Scope::Private(owner)) => matches!(viewer, Scope::Private(v) if *v == owner),
        }
    }
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
    pub goals: Vec<MemoryItem>,
    pub preferences: Vec<MemoryItem>,
}

/// A typed URGE in the tension economy — a substrate-grounded pressure that a DRIVE emits when it
/// meets a gap (an open contradiction, a stale-but-important belief, a curiosity gap). Persisted in
/// yantrikdb; accrues while the mind is idle; the proactive layer later arbitrates which (if any)
/// clears the bar to surface. Deliberately NOT a free-floating "urge" — it is grounded in measurable
/// substrate state (so it is ablatable/falsifiable), per the locked salience design.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tension {
    pub id: String,
    pub kind: TensionKind,
    pub pressure: f64, // [0,1] salience/urgency
    pub about: String, // what it concerns (human-readable)
    pub created_ms: UnixMillis,
    pub status: String, // "open" | "discharged"
}

/// Which DRIVE produced a tension.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TensionKind {
    Contradiction,    // coherence drive — two beliefs conflict
    Staleness,        // vigilance drive — an important belief is decaying/unrefreshed
    Curiosity,        // curiosity drive — a knowledge gap worth exploring
    VerificationDebt, // rigor drive — believed but unverified
    Operational,      // self-vigilance drive — the mind's OWN functioning needs attention (self-healing)
}

impl TensionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TensionKind::Contradiction => "contradiction",
            TensionKind::Staleness => "staleness",
            TensionKind::Curiosity => "curiosity",
            TensionKind::VerificationDebt => "verification_debt",
            TensionKind::Operational => "operational",
        }
    }
    pub fn parse(s: &str) -> TensionKind {
        match s {
            "staleness" => TensionKind::Staleness,
            "curiosity" => TensionKind::Curiosity,
            "verification_debt" => TensionKind::VerificationDebt,
            "operational" => TensionKind::Operational,
            _ => TensionKind::Contradiction,
        }
    }
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

    // ── group-chat read-isolation (scoped variants; the unscoped methods above = unrestricted) ──
    /// Recall, FILTERED to what `viewer` may see (shared facts + their own private). Default: ignores
    /// scope (delegates to recall_typed) so non-isolating impls need no change.
    async fn recall_typed_as(&self, q: RecallQuery, _viewer: Scope) -> Result<Vec<Recalled>> {
        self.recall_typed(q).await
    }
    /// Assert a belief tagged with a visibility `scope`. Default: ignores scope.
    async fn remember_as_belief_scoped(&self, a: BeliefAssertion, _scope: Scope) -> Result<Belief> {
        self.remember_as_belief(a).await
    }
    /// Working-set hydration, FILTERED to what `viewer` may see. Default: unrestricted.
    async fn hydrate_working_set_as(&self, focus: &str, _viewer: Scope) -> Result<WorkingSet> {
        self.hydrate_working_set(focus).await
    }
    /// Append a transcript line tagged with a visibility `scope`. Default: ignores scope.
    async fn append_message_scoped(&self, role: &str, text: &str, _scope: Scope) -> Result<()> {
        self.append_message(role, text).await
    }
    /// Recent transcript lines FILTERED to what `viewer` may see. Default: unrestricted.
    async fn recent_messages_as(&self, limit: usize, _viewer: Scope) -> Result<Vec<(String, String)>> {
        self.recent_messages(limit).await
    }
    /// Write a machine-derived OBSERVATION (skill/tool/sub-agent/web output) — provenance-tagged,
    /// secret-scanned, NEVER a naked Belief. This is the gated inward boundary for the moat.
    async fn remember_observation(&self, text: &str, source: crate::safety::ProvenanceCategory) -> Result<String>;
    /// Create/strengthen a graph edge between entities.
    async fn relate(&self, src: &str, dst: &str, rel: &str, weight: f64) -> Result<()>;
    /// Compose typed recalls + open conflicts into a structured reflection.
    async fn reflect(&self, question: &str) -> Result<Reflection>;
    /// Currently-open contradictions across stored beliefs.
    async fn conflicts(&self) -> Result<Vec<Contradiction>>;

    // ── tiny profile KV (name/purpose/onboarding) — durable, isolated from the cognitive graph ──
    /// Set a profile value (latest write wins on read).
    async fn profile_set(&self, key: &str, value: &str) -> Result<()>;
    /// Read the latest profile value for a key, or None.
    async fn profile_get(&self, key: &str) -> Result<Option<String>>;

    // ── tension economy (the "urges": drives emit substrate-grounded pressures; proactive arbitrates) ──
    /// Record a typed urge emitted by a drive (deduped on (kind, about) so it accrues, not floods).
    async fn record_tension(&self, kind: TensionKind, pressure: f64, about: &str) -> Result<()>;
    /// Open tensions, highest pressure first.
    async fn open_tensions(&self, limit: usize) -> Result<Vec<Tension>>;
    /// Mark a tension discharged (resolved, or surfaced to the user).
    async fn discharge_tension(&self, id: &str) -> Result<bool>;
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

    // ── goals + preferences (named capture; surfaced by reflect) ──
    async fn store_goal(&self, text: &str) -> Result<()>;
    async fn store_preference(&self, text: &str) -> Result<()>;

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

    // ── engine learning/metacognition (calibration + self-assessment; defaults = inert for fakes) ──
    /// Feed a graded prediction outcome into the engine's learning layer: the per-action-kind
    /// bandit + isotonic confidence calibration + per-SUBJECT source reliability. This is how
    /// foresight EARNS calibrated confidence instead of asserting raw model numbers.
    async fn record_prediction_outcome(&self, _domain: &str, _subject: &str, _raw_confidence: f64, _hit: bool) -> Result<()> {
        Ok(())
    }
    /// (subject_track_record ∈ [0,1], calibrated_confidence) from the engine's learned state.
    /// Track record defaults to 0.5 (no data); calibrated falls back to the raw value.
    async fn foresight_reliability(&self, _subject: &str, raw_confidence: f64) -> Result<(f64, f64)> {
        Ok((0.5, raw_confidence))
    }
    /// A short metacognitive self-check line when reasoning health is DEGRADED (thin evidence /
    /// high contradiction density). None while healthy — a sound mind doesn't narrate its health.
    async fn metacog_note(&self) -> Result<Option<String>> {
        Ok(None)
    }
    /// Record a life-event Episode (feeds the engine's temporal layer: periodicity/bursts/rhythm).
    async fn record_episode(&self, _label: &str) -> Result<()> {
        Ok(())
    }
    /// One human line about the user's activity rhythm (None until enough episodes accrue).
    async fn activity_rhythm(&self, _local_offset_hours: i32) -> Result<Option<String>> {
        Ok(None)
    }
    /// Record a tool call's outcome into the engine's bandit ("tool:<name>") — the mind learning
    /// which of its OWN tools are reliable.
    async fn record_tool_outcome(&self, _tool: &str, _ok: bool) -> Result<()> {
        Ok(())
    }
    /// Measured per-tool reliability: Vec<(tool, success_rate, observations)>, worst first.
    async fn tool_track_record(&self) -> Result<Vec<(String, f64, u64)>> {
        Ok(vec![])
    }
    /// Feed a proactive send's fate (engaged vs ignored) into the engine's WORLD MODEL (per-time-bin
    /// engagement learning), personality feedback, and bond progression.
    async fn record_proactive_outcome(&self, _sent_ms: i64, _engaged: bool) -> Result<()> {
        Ok(())
    }
    /// Predicted engagement rate for a proactive send RIGHT NOW (None until the world model has
    /// enough transitions to say anything real).
    async fn proactive_receptivity(&self) -> Result<Option<f64>> {
        Ok(None)
    }
}
