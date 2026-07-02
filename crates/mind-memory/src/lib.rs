//! mind-memory — the typed-memory MOAT over YantrikDB; the **sole writer** to the cognitive graph.
//!
//! A single-owner actor on a dedicated thread owns the `!Sync` `YantrikDB`; the async, Clone
//! `MemoryHandle` talks to it over mpsc + oneshot and implements `mind_types::MemoryFacade`. This
//! cashes in what flat-RAG assistants structurally cannot have: typed **beliefs** with Bayesian
//! revision, **contradiction detection**, and **explanations** (evidence trails). Beliefs are
//! keyed by their proposition text — a belief *is* its proposition.
//!
//! Phase 1 surfaces the belief moat + recall + working-set. Semantic (embedding) recall and real
//! consolidation land in Phase 2 once the embedder is wired.

use std::collections::HashMap;

use async_trait::async_trait;
use rusqlite::OptionalExtension;
use tokio::sync::{mpsc, oneshot};

use mind_types::{
    Belief, BeliefAssertion, Contradiction, Evidence as MEvidence, MemoryFacade, MemoryItem,
    MemoryKind, MindError, RecallQuery, Recalled, Reflection, Result, Skill, Task,
    UncertaintyReason, WorkingSet,
};

use yantrikdb_core::belief::{BeliefRevisionConfig, Evidence as YEvidence};
use yantrikdb_core::belief_query::BeliefPattern;
use yantrikdb_core::contradiction::ContradictionConfig;
use yantrikdb_core::state::{
    sigmoid, BeliefPayload, CognitiveEdge, CognitiveEdgeKind, CognitiveNode, EpisodePayload,
    NodeId, NodeIdAllocator, NodeKind, NodePayload, Priority, Provenance, TaskPayload, TaskStatus,
};
use yantrikdb_core::intent::IntentConfig;
use yantrikdb_core::personality_bias::BondLevel;
use yantrikdb_core::temporal::BurstConfig;
use yantrikdb_core::world_model::{ActionKind as WmAction, ActionOutcome as WmOutcome, StateFeatures};
use yantrikdb_core::{InteractionOutcome, YantrikDB};

type Reply<T> = oneshot::Sender<std::result::Result<T, String>>;

enum Cmd {
    Record { text: String, reply: Reply<String> },
    RememberObservation { text: String, source: String, reply: Reply<String> },
    GetText { rid: String, reply: Reply<Option<String>> },
    AssertBelief { statement: String, signed_weight: f64, source: String, provenance: String, evidence_version: Option<u64>, reply: Reply<Belief> },
    RecallTyped { text: String, top_k: usize, reply: Reply<Vec<Recalled>> },
    Conflicts { reply: Reply<Vec<Contradiction>> },
    Explain { statement: String, reply: Reply<Option<(Belief, Vec<MEvidence>)>> },
    Relate { src: String, dst: String, rel: String, weight: f64, reply: Reply<()> },
    Forget { statement: String, reply: Reply<bool> },
    Export { reply: Reply<String> },
    // cheap task tier (plain node CRUD — no cognitive ops)
    AddTask { description: String, priority: String, due_ms: Option<u64>, reply: Reply<Task> },
    ListTasks { include_done: bool, reply: Reply<Vec<Task>> },
    CompleteTask { id: String, reply: Reply<bool> },
    // cheap raw transcript (immediate context; isolated table, not the cognitive graph)
    AppendMessage { role: String, text: String, scope: String, reply: Reply<()> },
    RecentMessages { limit: usize, viewer: Option<String>, reply: Reply<Vec<(String, String)>> },
    MessagesSince { after_id: i64, limit: usize, reply: Reply<Vec<(i64, String, String)>> },
    RecordPredictionOutcome { domain: String, subject: String, raw: f64, hit: bool, reply: Reply<()> },
    RecordEpisode { label: String, reply: Reply<()> },
    RecordToolOutcome { tool: String, ok: bool, reply: Reply<()> },
    RecordProactiveOutcome { sent_ms: i64, engaged: bool, reply: Reply<()> },
    ProactiveReceptivity { reply: Reply<Option<f64>> },
    RelationshipLens { reply: Reply<Option<String>> },
    BeliefCount { reply: Reply<u64> },
    ToolTrackRecord { reply: Reply<Vec<(String, f64, u64)>> },
    ActivityRhythm { local_offset_hours: i32, reply: Reply<Option<String>> },
    ForesightReliability { subject: String, raw: f64, reply: Reply<(f64, f64)> },
    MetacogNote { reply: Reply<Option<String>> },
    // skill library
    SaveSkill { skill: Skill, reply: Reply<()> },
    GetSkill { name: String, reply: Reply<Option<Skill>> },
    ListSkills { reply: Reply<Vec<Skill>> },
    RecallSkills { query: String, limit: usize, reply: Reply<Vec<Skill>> },
    RecordSkillOutcome { name: String, success: bool, reply: Reply<()> },
    // goals / preferences (plain text CRUD; no Bayesian revision)
    StoreGoalPref { kind: String, text: String, reply: Reply<()> },
    ListGoalPrefs { kind: String, reply: Reply<Vec<MemoryItem>> },
    // profile KV (single value per key, latest-wins — distinct from append-distinct goals/prefs)
    SetProfile { key: String, value: String, reply: Reply<()> },
    // group-chat read-isolation: per-belief visibility scope (keyed by proposition)
    SetBeliefScope { proposition: String, scope: String, reply: Reply<()> },
    BeliefScopeMap { reply: Reply<std::collections::HashMap<String, String>> },
    // tension economy (the "urges" drives emit; plain CRUD ledger)
    RecordTension { kind: String, pressure: f64, about: String, reply: Reply<()> },
    OpenTensions { limit: usize, reply: Reply<Vec<mind_types::Tension>> },
    DischargeTension { id: String, reply: Reply<bool> },
}

// ── pure helpers (run on the actor thread, with &YantrikDB) ──────────────────

/// THE write gate: nothing secret-shaped may enter the cognitive moat (beliefs/observations).
/// Deterministic, shared with the harm-gate (one source of truth). Raw transcript is exempt
/// (verbatim ephemeral context, never reasoned over as knowledge).
fn gate_write(text: &str) -> std::result::Result<(), String> {
    if mind_types::contains_secret(text) {
        return Err("refused: write contains a secret/credential marker (write-gate)".into());
    }
    Ok(())
}

fn now_secs() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Exponential half-life decay toward the 0.5 uninformed prior.
///
/// `asserted` — the stored [0,1] posterior; `age_ms` — milliseconds since last update;
/// `halflife_days` — time at which the delta from 0.5 halves (env: `YM_BELIEF_HALFLIFE_DAYS`).
///
/// Formula: `0.5 + (asserted − 0.5) × 0.5^(age_days / halflife_days)`
fn decay_confidence(asserted: f64, age_ms: u64, halflife_days: f64) -> f64 {
    if halflife_days <= 0.0 {
        return asserted;
    }
    let age_days = age_ms as f64 / 86_400_000.0;
    0.5 + (asserted - 0.5) * 0.5f64.powf(age_days / halflife_days)
}

/// Classify why a belief is uncertain, in precedence order.
fn classify_uncertainty(
    original_conf: f64,
    decayed_conf: f64,
    evidence_count: u32,
    statement: &str,
    open: &[Contradiction],
) -> UncertaintyReason {
    if open.iter().any(|c| c.belief_a == statement || c.belief_b == statement) {
        return UncertaintyReason::Contradicted;
    }
    if original_conf - decayed_conf > 0.05 {
        return UncertaintyReason::Decayed;
    }
    if evidence_count < 2 {
        return UncertaintyReason::Sparse;
    }
    UncertaintyReason::LowPrior
}

fn prov(s: &str) -> Provenance {
    match s.to_ascii_lowercase().as_str() {
        "told" => Provenance::Told,
        "inferred" => Provenance::Inferred,
        "extracted" => Provenance::Extracted,
        "experimented" => Provenance::Experimented,
        "consolidated" => Provenance::Consolidated,
        _ => Provenance::Observed,
    }
}

fn edge_kind(s: &str) -> CognitiveEdgeKind {
    match s.to_ascii_lowercase().as_str() {
        "contradicts" => CognitiveEdgeKind::Contradicts,
        "supports" => CognitiveEdgeKind::Supports,
        _ => CognitiveEdgeKind::AssociatedWith,
    }
}

fn all_beliefs(db: &YantrikDB) -> Vec<CognitiveNode> {
    db.query_beliefs(&BeliefPattern { limit: 100_000, ..Default::default() })
        .unwrap_or_default()
}

fn node_prop(n: &CognitiveNode) -> Option<&str> {
    match &n.payload {
        NodePayload::Belief(b) => Some(b.proposition.as_str()),
        _ => None,
    }
}

fn evidence_count(n: &CognitiveNode) -> u32 {
    match &n.payload {
        NodePayload::Belief(b) => b.evidence_trail.len() as u32,
        _ => 0,
    }
}

fn to_belief_dto(n: &CognitiveNode) -> Belief {
    let statement = node_prop(n).map(|s| s.to_string()).unwrap_or_else(|| n.label.clone());
    Belief {
        id: statement.clone(),
        statement,
        confidence: n.attrs.confidence,
        certainty: n.attrs.confidence,
        provenance: format!("{:?}", n.attrs.provenance),
        evidence_count: evidence_count(n),
        updated_ms: n.attrs.last_updated_ms,
        status: "active".into(),
        uncertainty_reason: None,
    }
}

/// Normalize a proposition for dedup: lowercase, collapse whitespace, drop trailing punctuation.
/// Merges trivial formatting/case restatements ("July 23" / "july 23.") WITHOUT touching content —
/// "…Rust is 1.70" and "…Rust is 1.96" stay DISTINCT, so contradictions remain separate nodes.
/// (Word-overlap dedup is unsafe here: it strips the very tokens — numbers/versions — that
/// distinguish contradicting claims.)
fn norm_prop(s: &str) -> String {
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ").trim_end_matches(['.', '!', '?', ',']).to_string()
}

fn find_belief(db: &YantrikDB, statement: &str) -> Option<CognitiveNode> {
    let target = norm_prop(statement);
    all_beliefs(db).into_iter().find(|n| node_prop(n).map(|p| norm_prop(p) == target).unwrap_or(false))
}

fn assert_belief(
    db: &YantrikDB,
    alloc: &mut NodeIdAllocator,
    statement: &str,
    signed_weight: f64,
    source: &str,
    provenance: &str,
    evidence_version: Option<u64>,
) -> std::result::Result<Belief, String> {
    gate_write(statement)?;
    let node = match find_belief(db, statement) {
        Some(n) => n,
        None => {
            let id = alloc.alloc(NodeKind::Belief);
            let mut n = CognitiveNode::new(
                id,
                statement.to_string(),
                NodePayload::Belief(BeliefPayload {
                    proposition: statement.to_string(),
                    log_odds: 0.0,
                    domain: "general".into(),
                    evidence_trail: vec![],
                    user_confirmed: false,
                }),
            );
            n.attrs.confidence = sigmoid(0.0);
            n.attrs.provenance = prov(provenance);
            db.persist_cognitive_node(&n).map_err(|e| e.to_string())?;
            db.persist_node_id_allocator(alloc).map_err(|e| e.to_string())?;
            n
        }
    };
    // Monotonic evidence-version guard. Key by the CANONICAL proposition (find_belief may have merged
    // a paraphrase into an existing node). An explicit version that isn't strictly greater than the
    // stored one is an out-of-order or replayed update: drop it and return the current (fresher)
    // belief unchanged so its confidence is never overwritten. The unversioned (None) legacy path
    // always advances the counter by one and is never rejected.
    let canonical = node_prop(&node).unwrap_or(statement).to_string();
    let stored_version = get_belief_evidence_version(db, &canonical);
    if let (Some(incoming), Some(current)) = (evidence_version, stored_version) {
        if incoming <= current {
            return Ok(to_belief_dto(&node));
        }
    }

    let ev = YEvidence {
        target_belief: node.id,
        weight: signed_weight,
        source: source.to_string(),
        provenance: prov(provenance),
        propagate: false,
        timestamp: now_secs(),
    };
    db.assert_belief_evidence(&ev, &BeliefRevisionConfig::default())
        .map_err(|e| e.to_string())?;
    let next_version = match evidence_version {
        Some(incoming) => incoming,
        None => stored_version.unwrap_or(0) + 1,
    };
    set_belief_evidence_version(db, &canonical, next_version)?;
    let updated = db
        .load_cognitive_node(node.id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "belief vanished after assert".to_string())?;
    Ok(to_belief_dto(&updated))
}

/// Record into the flat vector store. Uses native `record_text` (auto-embed) when an embedder is
/// attached — 0.9.0 bundles one at dim 64, so this is the live path — giving real semantic recall.
/// Falls back to a zero-vector `record` only on no-embedder builds (the dim-8 test path), where
/// recall degrades to keyword rather than erroring with `NoEmbedder`.
fn record_memory(
    db: &YantrikDB,
    text: &str,
    zero: &[f32],
    mtype: &str,
    importance: f64,
    certainty: f64,
    source: &str,
    meta: &serde_json::Value,
) -> std::result::Result<String, String> {
    if db.has_embedder() {
        db.record_text(text, mtype, importance, 0.0, 604_800.0, meta, "default", certainty, "general", source, None)
            .map_err(|e| e.to_string())
    } else {
        db.record(text, mtype, importance, 0.0, 604_800.0, meta, zero, "default", certainty, "general", source, None)
            .map_err(|e| e.to_string())
    }
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0f64, 0f64, 0f64);
    for i in 0..a.len() {
        let (x, y) = (a[i] as f64, b[i] as f64);
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn belief_item(n: &CognitiveNode) -> MemoryItem {
    let prop = node_prop(n).unwrap_or("").to_string();
    MemoryItem {
        id: prop.clone(),
        kind: MemoryKind::Belief,
        text: prop,
        confidence: n.attrs.confidence,
        certainty: n.attrs.confidence,
        updated_ms: n.attrs.last_updated_ms,
        evidence_count: evidence_count(n),
    }
}

// ── BeliefScorer — pluggable ranking strategy for recall_beliefs ──────────────

struct BeliefScore {
    score: f64,
    why:   Vec<String>,
    node:  CognitiveNode,
}

trait BeliefScorer {
    fn score(&self, query: &str, beliefs: Vec<CognitiveNode>) -> Vec<BeliefScore>;
}

struct EmbedderScorer<'a> {
    db: &'a YantrikDB,
}

impl<'a> BeliefScorer for EmbedderScorer<'a> {
    fn score(&self, query: &str, beliefs: Vec<CognitiveNode>) -> Vec<BeliefScore> {
        let Ok(q) = self.db.embed(query) else {
            return KeywordScorer.score(query, beliefs);
        };
        beliefs
            .into_iter()
            .map(|n| {
                let prop = node_prop(&n).unwrap_or("");
                let sim = self.db.embed(prop).ok().map(|v| cosine(&q, &v)).unwrap_or(0.0);
                let score = sim + 0.1 * n.attrs.confidence;
                BeliefScore {
                    score,
                    why: vec![format!("semantic {:.2}, confidence {:.2}", sim, n.attrs.confidence)],
                    node: n,
                }
            })
            .collect()
    }
}

struct KeywordScorer;

impl BeliefScorer for KeywordScorer {
    fn score(&self, query: &str, beliefs: Vec<CognitiveNode>) -> Vec<BeliefScore> {
        let qwords: Vec<String> =
            query.to_ascii_lowercase().split_whitespace().map(|w| w.to_string()).collect();
        beliefs
            .into_iter()
            .map(|n| {
                let p = node_prop(&n).unwrap_or("").to_ascii_lowercase();
                let overlap = qwords.iter().filter(|w| p.contains(w.as_str())).count() as f64;
                let score = overlap + n.attrs.confidence;
                BeliefScore {
                    score,
                    why: vec![format!("confidence {:.2}", n.attrs.confidence)],
                    node: n,
                }
            })
            .collect()
    }
}

/// Belief recall. Beliefs live in `cognitive_nodes` (not the flat HNSW index), so when an embedder
/// is attached we rank by cosine similarity of the query vs each proposition (model2vec is in-process
/// and fast), blended with a small confidence prior so a confident near-match outranks a vague exact
/// one. With no embedder (test builds) we fall back to keyword overlap + confidence — the prior shape.
fn recall_beliefs(db: &YantrikDB, text: &str, top_k: usize) -> Vec<Recalled> {
    let beliefs = all_beliefs(db);
    let scorer: Box<dyn BeliefScorer + '_> = if db.has_embedder() {
        Box::new(EmbedderScorer { db })
    } else {
        Box::new(KeywordScorer)
    };
    let mut scored = scorer.score(text, beliefs);
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(top_k.max(1))
        .map(|s| Recalled { score: s.score, why: s.why, item: belief_item(&s.node) })
        .collect()
}

fn detect_conflicts(db: &YantrikDB) -> Vec<Contradiction> {
    let res = match db.detect_belief_contradictions(&ContradictionConfig::default()) {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let id_to_prop: HashMap<NodeId, String> = all_beliefs(db)
        .iter()
        .filter_map(|n| node_prop(n).map(|p| (n.id, p.to_string())))
        .collect();
    res.conflicts
        .iter()
        .map(|c| Contradiction {
            id: format!("{}~{}", c.belief_a, c.belief_b),
            belief_a: id_to_prop.get(&c.belief_a).cloned().unwrap_or_default(),
            belief_b: id_to_prop.get(&c.belief_b).cloned().unwrap_or_default(),
            severity: c.severity,
            status: "open".into(),
        })
        .collect()
}

fn explain(db: &YantrikDB, statement: &str) -> std::result::Result<Option<(Belief, Vec<MEvidence>)>, String> {
    let node = match find_belief(db, statement) {
        Some(n) => n,
        None => return Ok(None),
    };
    let belief = to_belief_dto(&node);
    let mut evs = Vec::new();
    if let Ok(Some(exp)) = db.explain_belief(node.id) {
        for (i, e) in exp.supporting_evidence.iter().enumerate() {
            evs.push(MEvidence {
                id: format!("{}#{i}", belief.id),
                belief_id: belief.id.clone(),
                source_event: None,
                weight: e.weight.abs(),
                polarity: if e.weight >= 0.0 { 1.0 } else { -1.0 },
                excerpt: e.source.clone(),
            });
        }
    }
    Ok(Some((belief, evs)))
}

// ── cheap task tier (plain cognitive-node CRUD; no embedding/revision/scan) ──

fn prio(s: &str) -> Priority {
    match s.to_ascii_lowercase().as_str() {
        "critical" => Priority::Critical,
        "high" => Priority::High,
        "low" => Priority::Low,
        _ => Priority::Medium,
    }
}

fn task_dto(n: &CognitiveNode) -> Option<Task> {
    if let NodePayload::Task(t) = &n.payload {
        Some(Task {
            id: format!("{}", n.id),
            description: t.description.clone(),
            status: t.status.as_str().to_string(),
            priority: t.priority.as_str().to_string(),
            due_ms: t.deadline.map(|s| (s * 1000.0) as u64),
        })
    } else {
        None
    }
}

fn all_task_nodes(db: &YantrikDB) -> Vec<CognitiveNode> {
    db.load_cognitive_nodes_by_kind(NodeKind::Task).unwrap_or_default()
}

/// Content-word set of a task description (lowercased, stopwords + short tokens dropped) — the basis
/// for de-duplicating paraphrased tasks (commitment-extraction re-creates the same task as slightly
/// different wording every consolidation pass; this caused ~40 duplicate gift/page reminders).
fn task_word_set(s: &str) -> std::collections::HashSet<String> {
    // Generic stopwords ONLY — domain words (gift/order/build/page…) carry the meaning that keeps
    // distinct intents apart, so they must stay in the signature.
    const STOP: &[&str] = &[
        "the", "and", "for", "his", "her", "with", "under", "are", "was", "you", "your",
        "into", "from", "that", "this", "ensure", "possibly", "within",
    ];
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 2 && !STOP.contains(w))
        .map(|w| w.to_string())
        .collect()
}

fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.union(b).count() as f64
}

fn add_task(
    db: &YantrikDB,
    alloc: &mut NodeIdAllocator,
    description: &str,
    priority: &str,
    due_ms: Option<u64>,
) -> std::result::Result<Task, String> {
    // Dedup: if an OPEN task is a close paraphrase of this one, reuse it instead of piling up.
    // Two complementary signals — mirrors the belief store's word-overlap + embedder moat:
    //   • word-overlap (jaccard ≥ 0.6) catches shared-vocabulary restatements, and is the only
    //     signal on the no-embedder test path (dim 8);
    //   • semantic (cosine ≥ 0.85) fires when the bundled embedder (dim 64) is attached, so a
    //     paraphrase that shares almost NO words ("buy groceries for the week" / "do the weekly
    //     grocery shopping", jaccard 0 yet cosine 0.89) still merges instead of piling up a third
    //     near-identical entry in the morning briefing.
    let new_sig = task_word_set(description);
    let new_vec = if db.has_embedder() { db.embed(description).ok() } else { None };
    if !new_sig.is_empty() || new_vec.is_some() {
        for n in all_task_nodes(db) {
            if let NodePayload::Task(ref t) = n.payload {
                if matches!(t.status, TaskStatus::Completed | TaskStatus::Cancelled) {
                    continue;
                }
                let word_dup =
                    !new_sig.is_empty() && jaccard(&new_sig, &task_word_set(&t.description)) >= 0.6;
                let semantic_dup = new_vec
                    .as_ref()
                    .map(|q| db.embed(&t.description).ok().map(|v| cosine(q, &v)).unwrap_or(0.0) >= 0.85)
                    .unwrap_or(false);
                if word_dup || semantic_dup {
                    return task_dto(&n).ok_or_else(|| "task build failed".to_string());
                }
            }
        }
    }
    let id = alloc.alloc(NodeKind::Task);
    let node = CognitiveNode::new(
        id,
        description.to_string(),
        NodePayload::Task(TaskPayload {
            description: description.to_string(),
            status: TaskStatus::Pending,
            goal_id: None,
            deadline: due_ms.map(|m| m as f64 / 1000.0),
            priority: prio(priority),
            estimated_minutes: None,
            prerequisites: vec![],
        }),
    );
    db.persist_cognitive_node(&node).map_err(|e| e.to_string())?;
    db.persist_node_id_allocator(alloc).map_err(|e| e.to_string())?;
    task_dto(&node).ok_or_else(|| "task build failed".to_string())
}

fn complete_task(db: &YantrikDB, id: &str) -> std::result::Result<bool, String> {
    let mut node = match all_task_nodes(db).into_iter().find(|n| format!("{}", n.id) == id) {
        Some(n) => n,
        None => return Ok(false),
    };
    if let NodePayload::Task(ref mut t) = node.payload {
        t.status = TaskStatus::Completed;
        db.persist_cognitive_node(&node).map_err(|e| e.to_string())?;
        Ok(true)
    } else {
        Ok(false)
    }
}

// ── cheap raw transcript (dedicated isolated table; plain SQL, no cognitive ops) ──

fn ensure_transcript_table(db: &YantrikDB) {
    let c = db.conn();
    let _ = c.execute(
        "CREATE TABLE IF NOT EXISTS mind_transcript \
         (id INTEGER PRIMARY KEY AUTOINCREMENT, role TEXT NOT NULL, text TEXT NOT NULL, ts REAL NOT NULL, \
          scope TEXT NOT NULL DEFAULT 'private:primary')",
        [],
    );
    // Migrate pre-existing tables: add the scope column; existing rows default to primary-private so a
    // later-added household member never sees the prior single-user transcript. (Errors if column exists.)
    let _ = c.execute("ALTER TABLE mind_transcript ADD COLUMN scope TEXT NOT NULL DEFAULT 'private:primary'", []);
}

// ── skill library (code-tools; same store, plain SQL; reuse always runs in the sandbox) ──

fn ensure_skills_table(db: &YantrikDB) {
    let _ = db.conn().execute(
        "CREATE TABLE IF NOT EXISTS mind_skills \
         (name TEXT PRIMARY KEY, lang TEXT NOT NULL, code TEXT NOT NULL, summary TEXT NOT NULL, \
          tags TEXT NOT NULL, status TEXT NOT NULL, runs INTEGER NOT NULL, successes INTEGER NOT NULL, created_ms INTEGER NOT NULL)",
        [],
    );
}

fn ensure_goals_prefs_table(db: &YantrikDB) {
    let conn = db.conn();
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS mind_goals_prefs \
         (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, text TEXT NOT NULL, \
          UNIQUE(kind, text))",
        [],
    );
    // Idempotent migration: add the unique index on existing databases that predate this constraint.
    let _ = conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_goals_prefs_kind_text \
         ON mind_goals_prefs(kind, text)",
        [],
    );
}

fn store_goal_pref(db: &YantrikDB, kind: &str, text: &str) -> std::result::Result<(), String> {
    let existing = list_goal_prefs(db, kind).unwrap_or_default();
    // Normalization dedup: canonicalize (lowercase, collapse whitespace, trim trailing punctuation —
    // same `norm_prop` the belief path uses) and collapse pure formatting/case variants ("Exercise" /
    // "exercise.") into the FIRST entry, whatever the word count. This catches short goals the jaccard
    // check below skips (it needs ≥2 significant words), so single-word restatements no longer duplicate.
    let canon = norm_prop(text);
    if existing.iter().any(|m| norm_prop(&m.text) == canon) {
        return Ok(()); // a canonical-form match already on file — no-op
    }
    // Dedup paraphrases: consolidation re-extracts the same goal/preference with slightly different
    // wording every pass (this flooded the store with ~280 near-dup goals/prefs). Goals/prefs have NO
    // contradiction semantics, so a moderate 0.6 word-overlap safely collapses re-phrasings of the same
    // intent while keeping distinct intents (gift vs repo-tracking) apart. Keeps the FIRST phrasing.
    let sig = task_word_set(text);
    if sig.len() >= 2 && existing.iter().any(|m| jaccard(&task_word_set(&m.text), &sig) >= 0.6) {
        return Ok(()); // a paraphrase already on file — no-op
    }
    db.conn()
        .execute("INSERT OR IGNORE INTO mind_goals_prefs (kind, text) VALUES (?1, ?2)", [kind, text])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Profile KV write: ONE value per key, latest wins. Distinct from `store_goal_pref` (append-distinct):
/// a profile key (holdings/subscriptions/bills/name/…) must overwrite. The old code reused the
/// INSERT-OR-IGNORE goals path, so re-storing any previously-seen value was silently dropped and the
/// reader returned a stale older row. Delete-then-insert guarantees a single fresh row per key.
fn set_profile(db: &YantrikDB, key: &str, value: &str) -> std::result::Result<(), String> {
    let conn = db.conn();
    conn.execute("DELETE FROM mind_goals_prefs WHERE kind = ?1", [key]).map_err(|e| e.to_string())?;
    conn.execute("INSERT INTO mind_goals_prefs (kind, text) VALUES (?1, ?2)", [key, value]).map_err(|e| e.to_string())?;
    Ok(())
}

/// Per-belief visibility scope (group-chat read-isolation), keyed by the belief's canonical
/// proposition. "shared" | "private:<owner>". A belief with no row = legacy (primary-only).
fn ensure_belief_scope_table(db: &YantrikDB) {
    let _ = db.conn().execute(
        "CREATE TABLE IF NOT EXISTS mind_belief_scope (proposition TEXT PRIMARY KEY, scope TEXT NOT NULL)",
        [],
    );
}

fn set_belief_scope(db: &YantrikDB, proposition: &str, scope: &str) -> std::result::Result<(), String> {
    db.conn()
        .execute(
            "INSERT INTO mind_belief_scope (proposition, scope) VALUES (?1, ?2) \
             ON CONFLICT(proposition) DO UPDATE SET scope=excluded.scope",
            [proposition, scope],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn belief_scope_map(db: &YantrikDB) -> std::result::Result<std::collections::HashMap<String, String>, String> {
    let conn = db.conn();
    let mut stmt = conn.prepare("SELECT proposition, scope FROM mind_belief_scope").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Per-belief monotonic evidence version — an optimistic-concurrency guard, keyed by the belief's
/// canonical proposition. A confidence write must carry a version STRICTLY GREATER than the one last
/// applied; anything ≤ it is an out-of-order or replayed evidence update and is dropped, so a stale
/// evidence packet can never silently overwrite a fresher confidence score. A belief with no row has
/// never taken a versioned write.
fn ensure_belief_evidence_version_table(db: &YantrikDB) {
    let _ = db.conn().execute(
        "CREATE TABLE IF NOT EXISTS mind_belief_evidence_version (proposition TEXT PRIMARY KEY, version INTEGER NOT NULL)",
        [],
    );
}

fn get_belief_evidence_version(db: &YantrikDB, proposition: &str) -> Option<u64> {
    let conn = db.conn();
    conn.query_row(
        "SELECT version FROM mind_belief_evidence_version WHERE proposition = ?1",
        [proposition],
        |r| r.get::<_, i64>(0),
    )
    .ok()
    .map(|v| v as u64)
}

fn set_belief_evidence_version(db: &YantrikDB, proposition: &str, version: u64) -> std::result::Result<(), String> {
    db.conn()
        .execute(
            "INSERT INTO mind_belief_evidence_version (proposition, version) VALUES (?1, ?2) \
             ON CONFLICT(proposition) DO UPDATE SET version=excluded.version",
            rusqlite::params![proposition, version as i64],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn list_goal_prefs(db: &YantrikDB, kind: &str) -> std::result::Result<Vec<MemoryItem>, String> {
    let kind_enum = if kind == "goal" { MemoryKind::Goal } else { MemoryKind::Preference };
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT id, text FROM mind_goals_prefs WHERE kind = ?1 ORDER BY id ASC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([kind], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    Ok(rows
        .filter_map(|r| r.ok())
        .map(|(id, text)| MemoryItem {
            id: id.to_string(),
            kind: kind_enum,
            text,
            confidence: 1.0,
            certainty: 1.0,
            updated_ms: 0,
            evidence_count: 0,
        })
        .collect())
}

fn ensure_tensions_table(db: &YantrikDB) {
    let _ = db.conn().execute(
        "CREATE TABLE IF NOT EXISTS mind_tensions \
         (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, pressure REAL NOT NULL, \
          about TEXT NOT NULL, created_ms INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'open')",
        [],
    );
}

/// Record a tension, deduped on (kind, about) among OPEN rows so a recurring urge accrues (keeps the
/// max pressure + refreshes created_ms) rather than flooding the ledger with duplicates.
fn record_tension_db(db: &YantrikDB, kind: &str, pressure: f64, about: &str, now_ms: i64) -> std::result::Result<(), String> {
    let conn = db.conn();
    let existing: Option<(i64, f64)> = conn
        .query_row(
            "SELECT id, pressure FROM mind_tensions WHERE kind=?1 AND about=?2 AND status='open' LIMIT 1",
            rusqlite::params![kind, about],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok();
    match existing {
        Some((id, prev)) => conn
            .execute(
                "UPDATE mind_tensions SET pressure=?1, created_ms=?2 WHERE id=?3",
                rusqlite::params![prev.max(pressure), now_ms, id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string()),
        None => conn
            .execute(
                "INSERT INTO mind_tensions (kind, pressure, about, created_ms, status) VALUES (?1,?2,?3,?4,'open')",
                rusqlite::params![kind, pressure, about, now_ms],
            )
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

fn open_tensions_db(db: &YantrikDB, limit: usize) -> std::result::Result<Vec<mind_types::Tension>, String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT id, kind, pressure, about, created_ms FROM mind_tensions WHERE status='open' ORDER BY pressure DESC, created_ms DESC LIMIT ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows
        .filter_map(|r| r.ok())
        .map(|(id, kind, pressure, about, created_ms)| mind_types::Tension {
            id: id.to_string(),
            kind: mind_types::TensionKind::parse(&kind),
            pressure,
            about,
            created_ms: created_ms as u64,
            status: "open".into(),
        })
        .collect())
}

fn discharge_tension_db(db: &YantrikDB, id: &str) -> std::result::Result<bool, String> {
    let n = db
        .conn()
        .execute("UPDATE mind_tensions SET status='discharged' WHERE id=?1 AND status='open'", [id])
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

fn skill_row(r: &rusqlite::Row) -> rusqlite::Result<Skill> {
    let tags_json: String = r.get(4)?;
    Ok(Skill {
        name: r.get(0)?,
        lang: r.get(1)?,
        code: r.get(2)?,
        summary: r.get(3)?,
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        status: r.get(5)?,
        runs: r.get::<_, i64>(6)? as u64,
        successes: r.get::<_, i64>(7)? as u64,
        created_ms: r.get::<_, i64>(8)? as u64,
    })
}

fn save_skill(db: &YantrikDB, s: &Skill) -> std::result::Result<(), String> {
    // The write-gate applies to skill CODE too — no hardcoded secrets bank into the library.
    gate_write(&s.code)?;
    let tags = serde_json::to_string(&s.tags).unwrap_or_else(|_| "[]".into());
    db.conn()
        .execute(
            "INSERT INTO mind_skills (name,lang,code,summary,tags,status,runs,successes,created_ms) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) \
             ON CONFLICT(name) DO UPDATE SET lang=?2,code=?3,summary=?4,tags=?5,status=?6",
            rusqlite::params![s.name, s.lang, s.code, s.summary, tags, s.status, s.runs as i64, s.successes as i64, s.created_ms as i64],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn get_skill(db: &YantrikDB, name: &str) -> std::result::Result<Option<Skill>, String> {
    db.conn()
        .query_row("SELECT name,lang,code,summary,tags,status,runs,successes,created_ms FROM mind_skills WHERE name=?1", [name], skill_row)
        .optional()
        .map_err(|e| e.to_string())
}

fn list_skills(db: &YantrikDB) -> std::result::Result<Vec<Skill>, String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT name,lang,code,summary,tags,status,runs,successes,created_ms FROM mind_skills ORDER BY created_ms DESC")
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], skill_row).map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn recall_skills(db: &YantrikDB, query: &str, limit: usize) -> std::result::Result<Vec<Skill>, String> {
    // Quarantined skills are never recalled.
    let skills: Vec<Skill> = list_skills(db)?.into_iter().filter(|s| s.status != "quarantined").collect();

    // SEMANTIC when an embedder is attached (0.9.0 bundles one) — the earned upgrade now that the
    // moat embeds. Rank by cosine of the query vs each skill's "name. summary. tags", blended with a
    // small reliability prior so a proven skill edges out an equally-relevant flaky one. A similarity
    // floor keeps "no matching skill" first-class (don't surface an unrelated skill). Falls back to
    // substring overlap on no-embedder builds.
    if db.has_embedder() {
        if let Ok(q) = db.embed(query) {
            let mut scored: Vec<(f64, f64, Skill)> = skills
                .into_iter()
                .map(|s| {
                    let text = format!("{}. {}. {}", s.name, s.summary, s.tags.join(" "));
                    let sim = db.embed(&text).ok().map(|v| cosine(&q, &v)).unwrap_or(0.0);
                    (sim + 0.1 * s.success_rate(), sim, s)
                })
                .collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            return Ok(scored
                .into_iter()
                .filter(|(_, sim, _)| *sim >= 0.30)
                .take(limit)
                .map(|(_, _, s)| s)
                .collect());
        }
    }

    let q = query.to_lowercase();
    let words: Vec<&str> = q.split_whitespace().filter(|w| w.len() >= 3).collect();
    let mut scored: Vec<(i32, Skill)> = skills
        .into_iter()
        .map(|s| {
            let hay = format!("{} {} {}", s.name, s.summary, s.tags.join(" ")).to_lowercase();
            let score = words.iter().filter(|w| hay.contains(**w)).count() as i32;
            (score, s)
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(scored.into_iter().take(limit).map(|(_, s)| s).collect())
}

fn record_skill_outcome(db: &YantrikDB, name: &str, success: bool) -> std::result::Result<(), String> {
    let conn = db.conn();
    conn.execute(
        "UPDATE mind_skills SET runs = runs + 1, successes = successes + ?2 WHERE name = ?1",
        rusqlite::params![name, if success { 1i64 } else { 0 }],
    )
    .map_err(|e| e.to_string())?;
    // Auto-quarantine a flaky skill: <50% success over >=4 runs (DeepSeek's rule).
    conn.execute(
        "UPDATE mind_skills SET status='quarantined' WHERE name=?1 AND runs>=4 AND (successes*2) < runs",
        [name],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn append_message(db: &YantrikDB, role: &str, text: &str, scope: &str) -> std::result::Result<(), String> {
    db.conn()
        .execute(
            "INSERT INTO mind_transcript (role, text, ts, scope) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![role, text, now_secs(), scope],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn recent_messages(db: &YantrikDB, limit: usize, viewer: Option<&str>) -> std::result::Result<Vec<(String, String)>, String> {
    // 0.9.0's `conn()` returns a temporary guard (was `&Connection`); bind it so the prepared
    // statement doesn't outlive a dropped temporary. When a `viewer` tag is given, read-ISOLATE the
    // transcript to shared lines + that viewer's own private lines (group-chat privacy).
    let conn = db.conn();
    let mut v: Vec<(String, String)> = match viewer {
        Some(tag) => {
            let mut stmt = conn
                .prepare("SELECT role, text FROM mind_transcript WHERE scope='shared' OR scope=?1 ORDER BY id DESC LIMIT ?2")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map(rusqlite::params![tag, limit as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            rows.filter_map(|r| r.ok()).collect()
        }
        None => {
            let mut stmt = conn
                .prepare("SELECT role, text FROM mind_transcript ORDER BY id DESC LIMIT ?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([limit as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(|e| e.to_string())?;
            rows.filter_map(|r| r.ok()).collect()
        }
    };
    v.reverse(); // newest-first SQL -> chronological for the prompt
    Ok(v)
}

fn messages_since(db: &YantrikDB, after_id: i64, limit: usize) -> std::result::Result<Vec<(i64, String, String)>, String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT id, role, text FROM mind_transcript WHERE id > ?1 ORDER BY id ASC LIMIT ?2")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![after_id, limit as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn relate(db: &YantrikDB, src: &str, dst: &str, rel: &str, weight: f64) -> std::result::Result<(), String> {
    let a = find_belief(db, src).ok_or_else(|| format!("no belief: {src}"))?;
    let b = find_belief(db, dst).ok_or_else(|| format!("no belief: {dst}"))?;
    let edge = CognitiveEdge::new(a.id, b.id, edge_kind(rel), weight);
    db.persist_cognitive_edge(&edge).map_err(|e| e.to_string())
}

// ── the actor + handle ───────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MemoryHandle {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl MemoryHandle {
    pub fn spawn(db_path: &str, dim: usize) -> Result<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
        let path = db_path.to_string();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();

        std::thread::Builder::new()
            .name("mind-memory".into())
            .spawn(move || {
                let db = match YantrikDB::new(&path, dim) {
                    Ok(d) => { let _ = ready_tx.send(Ok(())); d }
                    Err(e) => { let _ = ready_tx.send(Err(e.to_string())); return; }
                };
                ensure_transcript_table(&db);
                ensure_skills_table(&db);
                ensure_goals_prefs_table(&db);
                ensure_tensions_table(&db);
                ensure_belief_scope_table(&db);
                ensure_belief_evidence_version_table(&db);
                let mut alloc = db.load_node_id_allocator().unwrap_or_else(|_| NodeIdAllocator::new());
                let zero = vec![0.0f32; dim];
                let meta = serde_json::json!({});
                while let Some(cmd) = rx.blocking_recv() {
                    match cmd {
                        Cmd::Record { text, reply } => {
                            let r = gate_write(&text).and_then(|_| record_memory(&db, &text, &zero, "episodic", 0.5, 0.8, "user", &meta));
                            let _ = reply.send(r);
                        }
                        Cmd::RememberObservation { text, source, reply } => {
                            // Provenance-tagged, secret-scanned, low-certainty: an Observation, never a Belief.
                            let r = gate_write(&text).and_then(|_| {
                                let obs_meta = serde_json::json!({ "provenance": source, "observed_at": now_secs(), "kind": "observation" });
                                record_memory(&db, &text, &zero, "episodic", 0.4, 0.6, &source, &obs_meta)
                            });
                            let _ = reply.send(r);
                        }
                        Cmd::GetText { rid, reply } => {
                            let r = db.get(&rid).map(|o| o.map(|m| m.text)).map_err(|e| e.to_string());
                            let _ = reply.send(r);
                        }
                        Cmd::AssertBelief { statement, signed_weight, source, provenance, evidence_version, reply } => {
                            let result = assert_belief(&db, &mut alloc, &statement, signed_weight, &source, &provenance, evidence_version);
                            if result.is_ok() {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as i64)
                                    .unwrap_or(0);
                                for c in detect_conflicts(&db) {
                                    let _ = record_tension_db(
                                        &db,
                                        "contradiction",
                                        c.severity.clamp(0.3, 1.0),
                                        &format!("conflict: {} vs {}", c.belief_a, c.belief_b),
                                        now,
                                    );
                                }
                            }
                            let _ = reply.send(result);
                        }
                        Cmd::RecallTyped { text, top_k, reply } => {
                            let _ = reply.send(Ok(recall_beliefs(&db, &text, top_k)));
                        }
                        Cmd::Conflicts { reply } => {
                            let _ = reply.send(Ok(detect_conflicts(&db)));
                        }
                        Cmd::Explain { statement, reply } => {
                            let _ = reply.send(explain(&db, &statement));
                        }
                        Cmd::Relate { src, dst, rel, weight, reply } => {
                            let _ = reply.send(relate(&db, &src, &dst, &rel, weight));
                        }
                        Cmd::Forget { statement, reply } => {
                            let r = match find_belief(&db, &statement) {
                                Some(n) => db.tombstone_cognitive_node(n.id).map_err(|e| e.to_string()),
                                None => Ok(false),
                            };
                            let _ = reply.send(r);
                        }
                        Cmd::Export { reply } => {
                            let beliefs: Vec<Belief> = all_beliefs(&db).iter().map(to_belief_dto).collect();
                            let _ = reply.send(serde_json::to_string(&beliefs).map_err(|e| e.to_string()));
                        }
                        Cmd::AddTask { description, priority, due_ms, reply } => {
                            let _ = reply.send(add_task(&db, &mut alloc, &description, &priority, due_ms));
                        }
                        Cmd::ListTasks { include_done, reply } => {
                            let tasks: Vec<Task> = all_task_nodes(&db)
                                .iter()
                                .filter_map(task_dto)
                                .filter(|t| include_done || t.is_open())
                                .collect();
                            let _ = reply.send(Ok(tasks));
                        }
                        Cmd::CompleteTask { id, reply } => {
                            let _ = reply.send(complete_task(&db, &id));
                        }
                        Cmd::AppendMessage { role, text, scope, reply } => {
                            let _ = reply.send(append_message(&db, &role, &text, &scope));
                        }
                        Cmd::SaveSkill { skill, reply } => {
                            let _ = reply.send(save_skill(&db, &skill));
                        }
                        Cmd::GetSkill { name, reply } => {
                            let _ = reply.send(get_skill(&db, &name));
                        }
                        Cmd::ListSkills { reply } => {
                            let _ = reply.send(list_skills(&db));
                        }
                        Cmd::RecallSkills { query, limit, reply } => {
                            let _ = reply.send(recall_skills(&db, &query, limit));
                        }
                        Cmd::RecordSkillOutcome { name, success, reply } => {
                            let _ = reply.send(record_skill_outcome(&db, &name, success));
                        }
                        Cmd::StoreGoalPref { kind, text, reply } => {
                            let _ = reply.send(store_goal_pref(&db, &kind, &text));
                        }
                        Cmd::ListGoalPrefs { kind, reply } => {
                            let _ = reply.send(list_goal_prefs(&db, &kind));
                        }
                        Cmd::SetProfile { key, value, reply } => {
                            let _ = reply.send(set_profile(&db, &key, &value));
                        }
                        Cmd::SetBeliefScope { proposition, scope, reply } => {
                            let _ = reply.send(set_belief_scope(&db, &proposition, &scope));
                        }
                        Cmd::BeliefScopeMap { reply } => {
                            let _ = reply.send(belief_scope_map(&db));
                        }
                        Cmd::RecentMessages { limit, viewer, reply } => {
                            let _ = reply.send(recent_messages(&db, limit, viewer.as_deref()));
                        }
                        Cmd::RecordProactiveOutcome { sent_ms, engaged, reply } => {
                            // World model: engagement per time-bin (the state at SEND time). This is
                            // how proactivity learns WHEN the user is receptive instead of assuming.
                            let feats = StateFeatures::discretize(sent_ms as f64 / 1000.0, 0.5, 0.0, 0.0, 0);
                            let outcome = if engaged { WmOutcome::Accepted } else { WmOutcome::Ignored };
                            let r = db.record_transition(feats, WmAction::SendNotification, outcome).map_err(|e| e.to_string());
                            // Personality: engagement nudges proactivity (and a little warmth) up;
                            // being ignored nudges proactivity down. Small steps — a relationship, not a switch.
                            let _ = db.record_personality_feedback(1, if engaged { 0.05 } else { -0.03 });
                            if engaged {
                                let _ = db.record_personality_feedback(3, 0.02);
                            }
                            // Bond level follows cumulative accepted engagements.
                            if let Ok(sum) = db.world_model_summary() {
                                let accepted = (sum.global_positive_rate * sum.total_transitions as f64) as u64;
                                let bond = match accepted {
                                    0..=4 => BondLevel::Stranger,
                                    5..=14 => BondLevel::Acquaintance,
                                    15..=39 => BondLevel::Familiar,
                                    40..=99 => BondLevel::Bonded,
                                    _ => BondLevel::Trusted,
                                };
                                let _ = db.set_bond_level(bond);
                            }
                            let _ = reply.send(r);
                        }
                        Cmd::ProactiveReceptivity { reply } => {
                            let r = (|| {
                                let sum = db.world_model_summary().ok()?;
                                if sum.total_transitions < 20 {
                                    return None; // not enough relationship data to gate on
                                }
                                let feats = StateFeatures::discretize(now_secs(), 0.5, 0.0, 0.0, 0);
                                db.predict_outcome(&feats, WmAction::SendNotification).ok()
                            })();
                            let _ = reply.send(Ok(r));
                        }
                        Cmd::BeliefCount { reply } => {
                            let n: u64 = db
                                .conn()
                                .query_row("SELECT COUNT(*) FROM cognitive_nodes WHERE kind='belief'", [], |r| r.get(0))
                                .unwrap_or(0);
                            let _ = reply.send(Ok(n));
                        }
                        Cmd::RelationshipLens { reply } => {
                            let mut parts: Vec<String> = Vec::new();
                            // Bond + leading trait -> how to speak. The APPLY side of personality:
                            // the earned relationship visibly shapes the voice.
                            if let Ok(store) = db.load_personality_bias_store() {
                                let v = &store.current;
                                let mut dims: Vec<(&str, f64)> = vec![
                                    ("curiosity", v.curiosity),
                                    ("proactivity", v.proactivity),
                                    ("caution", v.caution),
                                    ("warmth", v.warmth),
                                    ("efficiency", v.efficiency),
                                ];
                                dims.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                                let style = match store.bond_level {
                                    BondLevel::Stranger => "warm but not presumptuous",
                                    BondLevel::Acquaintance => "friendly, still earning trust",
                                    BondLevel::Familiar => "relaxed and personal",
                                    BondLevel::Bonded => "close-friend candor",
                                    BondLevel::Trusted => "full candor — finish their thoughts",
                                };
                                if let Some((lead, x)) = dims.first() {
                                    parts.push(format!(
                                        "bond {:?} ({style}); leading trait {lead} {:.2}",
                                        store.bond_level, x
                                    ));
                                }
                            }
                            // Inferred current mode -> what to match (execute vs explore vs rest).
                            if let Ok(Some(t)) = db.top_intent(&IntentConfig::default()) {
                                let d: String = t.description.chars().take(90).collect();
                                parts.push(format!("their current mode: {d}"));
                            }
                            // A same-day activity burst -> be extra concise; maybe check in.
                            if let Ok(b) = db.detect_episode_bursts(&BurstConfig::default()) {
                                let now = now_secs();
                                if let Some(x) = b.bursts.iter().rev().find(|x| now - x.window_end < 86_400.0 && x.z_score > 2.0) {
                                    parts.push(format!(
                                        "activity burst today ({} events, z={:.1}) — they may be slammed; be extra concise",
                                        x.event_count, x.z_score
                                    ));
                                }
                            }
                            let _ = reply.send(Ok(if parts.is_empty() { None } else { Some(parts.join("; ")) }));
                        }
                        Cmd::RecordToolOutcome { tool, ok, reply } => {
                            let outcome = if ok { InteractionOutcome::Accepted } else { InteractionOutcome::Rejected };
                            let r = db
                                .record_learning_interaction(format!("tool:{tool}"), 0.5, outcome, [0.0; 4])
                                .map_err(|e| e.to_string());
                            let _ = reply.send(r);
                        }
                        Cmd::ToolTrackRecord { reply } => {
                            // Per-tool Beta posteriors from the bandit registry, worst first — the
                            // mind's measured self-knowledge about its own tools.
                            let v = db
                                .load_learning_state()
                                .map(|st| {
                                    let mut v: Vec<(String, f64, u64)> = st
                                        .bandits
                                        .bandits
                                        .into_iter()
                                        .filter_map(|(k, b)| {
                                            k.strip_prefix("tool:").map(|t| {
                                                (t.to_string(), b.alpha / (b.alpha + b.beta), b.total)
                                            })
                                        })
                                        .collect();
                                    v.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                                    v
                                })
                                .unwrap_or_default();
                            let _ = reply.send(Ok(v));
                        }
                        Cmd::RecordEpisode { label, reply } => {
                            // Life-events feed the engine's TEMPORAL layer (periodicity, bursts,
                            // hour/day histograms). Without episodes that whole layer starves.
                            let r = (|| -> std::result::Result<(), String> {
                                let id = alloc.alloc(NodeKind::Episode);
                                let n = CognitiveNode::new(
                                    id,
                                    label.clone(),
                                    NodePayload::Episode(EpisodePayload {
                                        memory_rid: String::new(),
                                        summary: label.clone(),
                                        occurred_at: now_secs(),
                                        participants: vec!["user".into()],
                                    }),
                                );
                                db.persist_cognitive_node(&n).map_err(|e| e.to_string())?;
                                db.persist_node_id_allocator(&alloc).map_err(|e| e.to_string())
                            })();
                            let _ = reply.send(r);
                        }
                        Cmd::ActivityRhythm { local_offset_hours, reply } => {
                            // Render the engine's activity histograms into one human line. Silent
                            // until enough life is recorded (>= 30 episodes) — no fake rhythm.
                            let note = (|| {
                                let hour = db.episode_hour_histogram().ok()?;
                                if hour.total < 30 {
                                    return None;
                                }
                                let dow = db.episode_dow_histogram().ok()?;
                                let ph_utc = hour.counts.iter().enumerate().max_by_key(|(_, c)| **c).map(|(h, _)| h as i32)?;
                                let ph = (ph_utc + local_offset_hours).rem_euclid(24);
                                const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
                                let pd = dow.counts.iter().enumerate().max_by_key(|(_, c)| **c).and_then(|(d, _)| DAYS.get(d).copied())?;
                                Some(format!("most active around {ph}:00, busiest on {pd}s ({} moments tracked)", hour.total))
                            })();
                            let _ = reply.send(Ok(note));
                        }
                        Cmd::RecordPredictionOutcome { domain, subject, raw, hit, reply } => {
                            // Two learners per graded call: the action-kind bandit + isotonic
                            // calibration (foresight:<domain>), and per-SUBJECT source reliability.
                            let outcome = if hit { InteractionOutcome::Accepted } else { InteractionOutcome::Rejected };
                            let r1 = db.record_learning_interaction(format!("foresight:{domain}"), raw, outcome, [0.0; 4]);
                            let r2 = if hit { db.learning_belief_confirmed(&subject) } else { db.learning_belief_contradicted(&subject) };
                            let _ = reply.send(r1.and(r2).map_err(|e| e.to_string()));
                        }
                        Cmd::ForesightReliability { subject, raw, reply } => {
                            let rel = db.source_reliability(&subject).unwrap_or(0.5);
                            let cal = db.calibrated_confidence(raw).unwrap_or(raw);
                            let _ = reply.send(Ok((rel, cal)));
                        }
                        Cmd::MetacogNote { reply } => {
                            // Only speak up when degraded — a healthy mind doesn't narrate its health.
                            let note = db.metacognitive_assessment().ok().and_then(|r| {
                                if r.evidence_sparsity > 0.7 || r.contradiction_density > 0.5 {
                                    Some(format!(
                                        "evidence sparsity {:.0}%, contradiction density {:.0}%",
                                        r.evidence_sparsity * 100.0,
                                        r.contradiction_density * 100.0
                                    ))
                                } else {
                                    None
                                }
                            });
                            let _ = reply.send(Ok(note));
                        }
                        Cmd::MessagesSince { after_id, limit, reply } => {
                            let _ = reply.send(messages_since(&db, after_id, limit));
                        }
                        Cmd::RecordTension { kind, pressure, about, reply } => {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0);
                            let _ = reply.send(record_tension_db(&db, &kind, pressure, &about, now));
                        }
                        Cmd::OpenTensions { limit, reply } => {
                            let _ = reply.send(open_tensions_db(&db, limit));
                        }
                        Cmd::DischargeTension { id, reply } => {
                            let _ = reply.send(discharge_tension_db(&db, &id));
                        }
                    }
                }
            })
            .map_err(|e| MindError::Memory(format!("spawn actor: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { tx }),
            Ok(Err(e)) => Err(MindError::Memory(format!("init YantrikDB: {e}"))),
            Err(_) => Err(MindError::Memory("actor thread died during init".into())),
        }
    }

    async fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Cmd) -> Result<T> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(make(reply)).map_err(|_| MindError::Memory("memory actor is gone".into()))?;
        rx.await
            .map_err(|_| MindError::Memory("memory actor dropped the reply".into()))?
            .map_err(MindError::Memory)
    }

    // flat-path helpers retained from Spike A
    pub async fn record(&self, text: impl Into<String>) -> Result<String> {
        let text = text.into();
        self.call(|reply| Cmd::Record { text, reply }).await
    }
    pub async fn get_text(&self, rid: &str) -> Result<Option<String>> {
        let rid = rid.to_string();
        self.call(|reply| Cmd::GetText { rid, reply }).await
    }

    pub async fn store_goal(&self, text: &str) -> Result<()> {
        let (kind, text) = ("goal".to_string(), text.to_string());
        self.call(|reply| Cmd::StoreGoalPref { kind, text, reply }).await
    }
    pub async fn store_preference(&self, text: &str) -> Result<()> {
        let (kind, text) = ("preference".to_string(), text.to_string());
        self.call(|reply| Cmd::StoreGoalPref { kind, text, reply }).await
    }
    pub async fn list_goals(&self) -> Result<Vec<MemoryItem>> {
        self.call(|reply| Cmd::ListGoalPrefs { kind: "goal".to_string(), reply }).await
    }
    pub async fn list_preferences(&self) -> Result<Vec<MemoryItem>> {
        self.call(|reply| Cmd::ListGoalPrefs { kind: "preference".to_string(), reply }).await
    }
}

#[async_trait]
impl MemoryFacade for MemoryHandle {
    async fn recall_typed(&self, q: RecallQuery) -> Result<Vec<Recalled>> {
        let (text, top_k) = (q.text, q.top_k);
        self.call(|reply| Cmd::RecallTyped { text, top_k, reply }).await
    }

    async fn remember_observation(&self, text: &str, source: mind_types::ProvenanceCategory) -> Result<String> {
        let text = text.to_string();
        let source = source.as_str().to_string();
        self.call(|reply| Cmd::RememberObservation { text, source, reply }).await
    }

    async fn remember_as_belief(&self, a: BeliefAssertion) -> Result<Belief> {
        let signed_weight = a.polarity * a.weight.abs();
        let (statement, source, provenance) = (a.statement, a.source_event.unwrap_or_default(), a.provenance);
        self.call(|reply| Cmd::AssertBelief { statement, signed_weight, source, provenance, evidence_version: None, reply }).await
    }

    async fn remember_as_belief_versioned(&self, a: BeliefAssertion, evidence_version: u64) -> Result<Belief> {
        let signed_weight = a.polarity * a.weight.abs();
        let (statement, source, provenance) = (a.statement, a.source_event.unwrap_or_default(), a.provenance);
        self.call(|reply| Cmd::AssertBelief { statement, signed_weight, source, provenance, evidence_version: Some(evidence_version), reply }).await
    }

    // ── group-chat read-isolation (real impls; default trait methods are unrestricted) ──
    async fn remember_as_belief_scoped(&self, a: BeliefAssertion, scope: mind_types::Scope) -> Result<Belief> {
        let belief = self.remember_as_belief(a).await?;
        // Tag by the CANONICAL proposition (find_belief may have merged a paraphrase into an existing node).
        let (proposition, tag) = (belief.statement.clone(), scope.as_tag());
        let _ = self.call(|reply| Cmd::SetBeliefScope { proposition, scope: tag, reply }).await;
        Ok(belief)
    }

    async fn recall_typed_as(&self, q: RecallQuery, viewer: mind_types::Scope) -> Result<Vec<Recalled>> {
        let recalled = self.recall_typed(q).await?;
        let scopes = self.call(|reply| Cmd::BeliefScopeMap { reply }).await.unwrap_or_default();
        Ok(recalled
            .into_iter()
            .filter(|r| mind_types::Scope::visible_to(scopes.get(&r.item.text).map(|s| s.as_str()), Some(&viewer)))
            .collect())
    }

    async fn hydrate_working_set_as(&self, focus: &str, viewer: mind_types::Scope) -> Result<WorkingSet> {
        // Same shape as hydrate_working_set, but the belief recall is read-ISOLATED to the viewer, and
        // task commitments surface only to the primary (tasks aren't per-task scoped yet).
        let recalled = self.recall_typed_as(RecallQuery { text: focus.to_string(), top_k: 8, kind: None }, viewer.clone()).await?;
        let open = self.conflicts().await?;
        let mut ws = WorkingSet::default();
        let halflife_days: f64 = std::env::var("YM_BELIEF_HALFLIFE_DAYS").ok().and_then(|s| s.parse().ok()).unwrap_or(90.0);
        let now_ms = (now_secs() * 1000.0) as u64;
        for r in recalled {
            let age_ms = now_ms.saturating_sub(r.item.updated_ms);
            let eff = decay_confidence(r.item.confidence, age_ms, halflife_days);
            if eff >= 0.7 {
                ws.stable_facts.push(MemoryItem { confidence: eff, ..r.item });
            } else {
                let reason = classify_uncertainty(r.item.confidence, eff, r.item.evidence_count, &r.item.text, &open);
                ws.uncertain_beliefs.push(Belief {
                    id: r.item.id.clone(),
                    statement: r.item.text.clone(),
                    confidence: eff,
                    certainty: r.item.certainty,
                    provenance: "recalled".into(),
                    evidence_count: 0,
                    updated_ms: r.item.updated_ms,
                    status: "active".into(),
                    uncertainty_reason: Some(reason),
                });
            }
        }
        ws.active_contradictions = open;
        if matches!(&viewer, mind_types::Scope::Private(v) if v == mind_types::PRIMARY) {
            for t in self.list_tasks(false).await.unwrap_or_default() {
                ws.commitments.push(MemoryItem {
                    id: t.id,
                    kind: MemoryKind::Task,
                    text: t.description,
                    confidence: 1.0,
                    certainty: 1.0,
                    updated_ms: t.due_ms.unwrap_or(0),
                    evidence_count: 0,
                });
            }
        }
        Ok(ws)
    }

    async fn relate(&self, src: &str, dst: &str, rel: &str, weight: f64) -> Result<()> {
        let (src, dst, rel) = (src.to_string(), dst.to_string(), rel.to_string());
        self.call(|reply| Cmd::Relate { src, dst, rel, weight, reply }).await
    }

    async fn reflect(&self, question: &str) -> Result<Reflection> {
        let recalled = self.recall_typed(RecallQuery { text: question.to_string(), top_k: 5, kind: None }).await?;
        let open_conflicts = self.conflicts().await?;
        let goals = self.list_goals().await.unwrap_or_default();
        let preferences = self.list_preferences().await.unwrap_or_default();
        let beliefs: Vec<Belief> = recalled
            .iter()
            .map(|r| Belief {
                id: r.item.id.clone(),
                statement: r.item.text.clone(),
                confidence: r.item.confidence,
                certainty: r.item.certainty,
                provenance: "recalled".into(),
                evidence_count: r.item.evidence_count,
                updated_ms: r.item.updated_ms,
                status: "active".into(),
                uncertainty_reason: None,
            })
            .collect();
        Ok(Reflection {
            summary: format!(
                "{} relevant beliefs, {} open conflicts, {} goals, {} preferences",
                beliefs.len(), open_conflicts.len(), goals.len(), preferences.len()
            ),
            beliefs,
            open_conflicts,
            goals,
            preferences,
        })
    }

    async fn conflicts(&self) -> Result<Vec<Contradiction>> {
        self.call(|reply| Cmd::Conflicts { reply }).await
    }

    async fn profile_set(&self, key: &str, value: &str) -> Result<()> {
        let (key, value) = (key.to_string(), value.to_string());
        self.call(|reply| Cmd::SetProfile { key, value, reply }).await
    }
    async fn profile_get(&self, key: &str) -> Result<Option<String>> {
        let kind = key.to_string();
        let items = self.call(|reply| Cmd::ListGoalPrefs { kind, reply }).await?;
        Ok(items.last().map(|i| i.text.clone()))
    }

    async fn record_tension(&self, kind: mind_types::TensionKind, pressure: f64, about: &str) -> Result<()> {
        let (kind, about) = (kind.as_str().to_string(), about.to_string());
        self.call(|reply| Cmd::RecordTension { kind, pressure: pressure.clamp(0.0, 1.0), about, reply }).await
    }
    async fn open_tensions(&self, limit: usize) -> Result<Vec<mind_types::Tension>> {
        self.call(|reply| Cmd::OpenTensions { limit, reply }).await
    }
    async fn discharge_tension(&self, id: &str) -> Result<bool> {
        let id = id.to_string();
        self.call(|reply| Cmd::DischargeTension { id, reply }).await
    }

    async fn explain_belief(&self, belief_id: &str) -> Result<Option<(Belief, Vec<MEvidence>)>> {
        let statement = belief_id.to_string();
        self.call(|reply| Cmd::Explain { statement, reply }).await
    }

    async fn hydrate_working_set(&self, focus: &str) -> Result<WorkingSet> {
        let recalled = self.recall_typed(RecallQuery { text: focus.to_string(), top_k: 8, kind: None }).await?;
        let open = self.conflicts().await?;
        let mut ws = WorkingSet::default();
        let halflife_days: f64 = std::env::var("YM_BELIEF_HALFLIFE_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(90.0);
        let now_ms = (now_secs() * 1000.0) as u64;
        for r in recalled {
            let age_ms = now_ms.saturating_sub(r.item.updated_ms);
            let original_conf = r.item.confidence;
            let eff = decay_confidence(original_conf, age_ms, halflife_days);
            if eff >= 0.7 {
                ws.stable_facts.push(MemoryItem { confidence: eff, ..r.item });
            } else {
                let reason = classify_uncertainty(original_conf, eff, r.item.evidence_count, &r.item.text, &open);
                ws.uncertain_beliefs.push(Belief {
                    id: r.item.id.clone(),
                    statement: r.item.text.clone(),
                    confidence: eff,
                    certainty: r.item.certainty,
                    provenance: "recalled".into(),
                    evidence_count: r.item.evidence_count,
                    updated_ms: r.item.updated_ms,
                    status: "active".into(),
                    uncertainty_reason: Some(reason),
                });
            }
        }
        ws.active_contradictions = open;
        // open tasks ride along as commitments (cheap tier surfaced for grounding)
        for t in self.list_tasks(false).await.unwrap_or_default() {
            ws.commitments.push(MemoryItem {
                id: t.id,
                kind: MemoryKind::Task,
                text: t.description,
                confidence: 1.0,
                certainty: 1.0,
                updated_ms: t.due_ms.unwrap_or(0),
                evidence_count: 0,
            });
        }
        Ok(ws)
    }

    async fn consolidate(&self) -> Result<usize> {
        // Real consolidation (clustering aging turns -> typed nodes) lands in Phase 2 with the
        // embedder wired. v1: no-op.
        Ok(0)
    }

    async fn forget(&self, id: &str) -> Result<bool> {
        let statement = id.to_string();
        self.call(|reply| Cmd::Forget { statement, reply }).await
    }

    async fn export(&self) -> Result<String> {
        self.call(|reply| Cmd::Export { reply }).await
    }

    async fn store_goal(&self, text: &str) -> Result<()> {
        let (kind, text) = ("goal".to_string(), text.to_string());
        self.call(|reply| Cmd::StoreGoalPref { kind, text, reply }).await
    }
    async fn store_preference(&self, text: &str) -> Result<()> {
        let (kind, text) = ("preference".to_string(), text.to_string());
        self.call(|reply| Cmd::StoreGoalPref { kind, text, reply }).await
    }

    async fn add_task(&self, description: &str, priority: &str, due_ms: Option<u64>) -> Result<Task> {
        let (description, priority) = (description.to_string(), priority.to_string());
        self.call(|reply| Cmd::AddTask { description, priority, due_ms, reply }).await
    }
    async fn list_tasks(&self, include_done: bool) -> Result<Vec<Task>> {
        self.call(|reply| Cmd::ListTasks { include_done, reply }).await
    }
    async fn complete_task(&self, id: &str) -> Result<bool> {
        let id = id.to_string();
        self.call(|reply| Cmd::CompleteTask { id, reply }).await
    }

    async fn save_skill(&self, skill: Skill) -> Result<()> {
        self.call(|reply| Cmd::SaveSkill { skill, reply }).await
    }
    async fn get_skill(&self, name: &str) -> Result<Option<Skill>> {
        let name = name.to_string();
        self.call(|reply| Cmd::GetSkill { name, reply }).await
    }
    async fn list_skills(&self) -> Result<Vec<Skill>> {
        self.call(|reply| Cmd::ListSkills { reply }).await
    }
    async fn recall_skills(&self, query: &str, limit: usize) -> Result<Vec<Skill>> {
        let query = query.to_string();
        self.call(|reply| Cmd::RecallSkills { query, limit, reply }).await
    }
    async fn record_skill_outcome(&self, name: &str, success: bool) -> Result<()> {
        let name = name.to_string();
        self.call(|reply| Cmd::RecordSkillOutcome { name, success, reply }).await
    }

    async fn append_message(&self, role: &str, text: &str) -> Result<()> {
        // Unscoped append = primary's private context (single-user default; never leaks to a member).
        self.append_message_scoped(role, text, mind_types::Scope::primary()).await
    }
    async fn append_message_scoped(&self, role: &str, text: &str, scope: mind_types::Scope) -> Result<()> {
        let (role, text, scope) = (role.to_string(), text.to_string(), scope.as_tag());
        self.call(|reply| Cmd::AppendMessage { role, text, scope, reply }).await
    }
    async fn messages_since(&self, after_id: i64, limit: usize) -> Result<Vec<(i64, String, String)>> {
        self.call(|reply| Cmd::MessagesSince { after_id, limit, reply }).await
    }
    async fn recent_messages(&self, limit: usize) -> Result<Vec<(String, String)>> {
        self.call(|reply| Cmd::RecentMessages { limit, viewer: None, reply }).await
    }
    async fn recent_messages_as(&self, limit: usize, viewer: mind_types::Scope) -> Result<Vec<(String, String)>> {
        self.call(|reply| Cmd::RecentMessages { limit, viewer: Some(viewer.as_tag()), reply }).await
    }
    async fn record_prediction_outcome(&self, domain: &str, subject: &str, raw_confidence: f64, hit: bool) -> Result<()> {
        let (domain, subject) = (domain.to_string(), subject.to_lowercase());
        self.call(move |reply| Cmd::RecordPredictionOutcome { domain, subject, raw: raw_confidence, hit, reply }).await
    }
    async fn foresight_reliability(&self, subject: &str, raw_confidence: f64) -> Result<(f64, f64)> {
        let subject = subject.to_lowercase();
        self.call(move |reply| Cmd::ForesightReliability { subject, raw: raw_confidence, reply }).await
    }
    async fn metacog_note(&self) -> Result<Option<String>> {
        self.call(|reply| Cmd::MetacogNote { reply }).await
    }
    async fn record_episode(&self, label: &str) -> Result<()> {
        let label = label.to_string();
        self.call(move |reply| Cmd::RecordEpisode { label, reply }).await
    }
    async fn activity_rhythm(&self, local_offset_hours: i32) -> Result<Option<String>> {
        self.call(move |reply| Cmd::ActivityRhythm { local_offset_hours, reply }).await
    }
    async fn record_tool_outcome(&self, tool: &str, ok: bool) -> Result<()> {
        let tool = tool.to_string();
        self.call(move |reply| Cmd::RecordToolOutcome { tool, ok, reply }).await
    }
    async fn tool_track_record(&self) -> Result<Vec<(String, f64, u64)>> {
        self.call(|reply| Cmd::ToolTrackRecord { reply }).await
    }
    async fn record_proactive_outcome(&self, sent_ms: i64, engaged: bool) -> Result<()> {
        self.call(move |reply| Cmd::RecordProactiveOutcome { sent_ms, engaged, reply }).await
    }
    async fn proactive_receptivity(&self) -> Result<Option<f64>> {
        self.call(|reply| Cmd::ProactiveReceptivity { reply }).await
    }
    async fn relationship_lens(&self) -> Result<Option<String>> {
        self.call(|reply| Cmd::RelationshipLens { reply }).await
    }
    async fn belief_count(&self) -> Result<u64> {
        self.call(|reply| Cmd::BeliefCount { reply }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE EMBEDDER MOAT, applied to task dedup: with the bundled embedder attached (dim 64) a
    /// paraphrase that shares NO significant words with an open task — so word-overlap jaccard is
    /// 0 — still collapses into it because their embeddings are ≥ 0.85 cosine. A genuinely
    /// unrelated task stays separate. This is the case the morning briefing kept showing thrice.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn add_task_semantic_dedup_merges_paraphrase_without_shared_words() {
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        mem.add_task("Buy groceries for the week", "medium", None).await.unwrap();
        // shares no ≥3-char content token with the above (jaccard 0), but cosine ≈ 0.89 → merges
        mem.add_task("Do the weekly grocery shopping", "medium", None).await.unwrap();
        assert_eq!(
            mem.list_tasks(false).await.unwrap().len(),
            1,
            "semantic paraphrase with no shared words must collapse via the embedder path"
        );
        // an unrelated task (cosine ≈ 0.01) is NOT swallowed
        mem.add_task("Fix the leaking kitchen faucet", "medium", None).await.unwrap();
        assert_eq!(
            mem.list_tasks(false).await.unwrap().len(),
            2,
            "an unrelated task stays a distinct entry"
        );
    }

    #[test]
    fn decay_confidence_halves_toward_prior() {
        // fresh belief — no decay
        assert!((decay_confidence(0.9, 0, 90.0) - 0.9).abs() < 1e-9);
        // exactly one halflife: delta from 0.5 halves → (0.9-0.5)*0.5 + 0.5 = 0.7
        let one_hl_ms = (90.0_f64 * 86_400_000.0) as u64;
        assert!((decay_confidence(0.9, one_hl_ms, 90.0) - 0.7).abs() < 1e-6);
        // confidence below 0.5 also decays toward 0.5: (0.2-0.5)*0.5 + 0.5 = 0.35
        assert!((decay_confidence(0.2, one_hl_ms, 90.0) - 0.35).abs() < 1e-6);
        // many halflives → asymptotically approaches 0.5
        let many_hl_ms = (900.0_f64 * 86_400_000.0) as u64;
        assert!((decay_confidence(0.99, many_hl_ms, 90.0) - 0.5).abs() < 0.001);
        // zero halflife disables decay
        assert!((decay_confidence(0.9, one_hl_ms, 0.0) - 0.9).abs() < 1e-9);
    }

    /// Monotonic evidence-version guard: once a belief has taken a versioned confidence write, a
    /// LATER-ARRIVING evidence packet carrying an OLDER (or replayed, equal) version must be dropped
    /// — it can never overwrite the fresher confidence a higher version already established.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn stale_evidence_version_cannot_overwrite_fresher_confidence() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let claim = "the client prefers morning meetings";
        let assertion = |polarity: f64, weight: f64| BeliefAssertion {
            statement: claim.into(),
            polarity,
            weight,
            source_event: None,
            provenance: "told".into(),
        };

        // v1: strong POSITIVE evidence → confidence rises well above the 0.5 prior.
        let v1 = mem.remember_as_belief_versioned(assertion(1.0, 2.0), 1).await.unwrap();
        assert!(v1.confidence > 0.5, "positive evidence should raise confidence: {}", v1.confidence);

        // v3: even stronger POSITIVE evidence → the freshest, highest-confidence state.
        let fresh = mem.remember_as_belief_versioned(assertion(1.0, 3.0), 3).await.unwrap();
        assert!(fresh.confidence > v1.confidence, "newer evidence should raise confidence further");
        let fresh_conf = fresh.confidence;

        // v2 arrives LATE and is strongly NEGATIVE. It is older than the stored v3, so it must be
        // dropped — the fresher confidence survives untouched, not silently overwritten downward.
        let stale = mem.remember_as_belief_versioned(assertion(-1.0, 5.0), 2).await.unwrap();
        assert_eq!(stale.confidence, fresh_conf, "stale (older) evidence version must be rejected");

        // A replay of the current version (v3) is likewise a no-op — equal is not strictly greater.
        let replay = mem.remember_as_belief_versioned(assertion(-1.0, 5.0), 3).await.unwrap();
        assert_eq!(replay.confidence, fresh_conf, "replayed (equal) evidence version must be rejected");

        // A genuinely newer version (v4) is applied — the guard only blocks stale/replayed writes.
        let advanced = mem.remember_as_belief_versioned(assertion(-1.0, 5.0), 4).await.unwrap();
        assert!(advanced.confidence < fresh_conf, "a strictly-newer version must still apply: {}", advanced.confidence);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn actor_round_trips_a_write_then_read() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let rid = mem.record("the sky is blue").await.unwrap();
        assert_eq!(mem.get_text(&rid).await.unwrap().as_deref(), Some("the sky is blue"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn write_gate_blocks_secrets_into_the_moat() {
        use mind_types::ProvenanceCategory;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        // A secret can't enter as a belief…
        let belief = mem
            .remember_as_belief(BeliefAssertion {
                statement: "the deploy token is ghp_ABCDEFGH1234567890".into(),
                polarity: 1.0,
                weight: 1.5,
                source_event: None,
                provenance: "told".into(),
            })
            .await;
        assert!(belief.is_err(), "secret-bearing belief must be refused by the write-gate");
        // …nor as an observation…
        let obs_secret = mem.remember_observation("here is the key: ghp_SECRET1234567890ab", ProvenanceCategory::SandboxedSkill).await;
        assert!(obs_secret.is_err(), "secret-bearing observation must be refused");
        // …but a clean observation is stored (provenance-tagged), never a belief.
        let ok = mem.remember_observation("the CSV had 412 rows", ProvenanceCategory::SandboxedSkill).await;
        assert!(ok.is_ok(), "clean observation should store: {ok:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_concurrent_tasks_no_lost_writes_no_deadlock() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let mut handles = Vec::new();
        for i in 0..50u32 {
            let m = mem.clone();
            handles.push(tokio::spawn(async move { m.record(format!("fact number {i}")).await }));
        }
        let mut rids = Vec::new();
        for h in handles {
            rids.push(h.await.unwrap().unwrap());
        }
        for rid in &rids {
            assert!(mem.get_text(rid).await.unwrap().is_some());
        }
        let unique: std::collections::HashSet<_> = rids.iter().collect();
        assert_eq!(unique.len(), 50);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn profile_kv_is_single_value_latest_wins() {
        // Regression: profile_set must OVERWRITE (one value per key) — including re-storing a value
        // seen before. The old INSERT-OR-IGNORE goals path silently dropped repeat (kind,text) writes,
        // so the reader returned a STALE older row, breaking holdings/subs/bills on any repeated value.
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        mem.profile_set("holdings", "[A]").await.unwrap();
        assert_eq!(mem.profile_get("holdings").await.unwrap().as_deref(), Some("[A]"));
        mem.profile_set("holdings", "[A,B]").await.unwrap();
        assert_eq!(mem.profile_get("holdings").await.unwrap().as_deref(), Some("[A,B]"));
        // re-store a value seen earlier — MUST read back, not the stale "[A,B]"
        mem.profile_set("holdings", "[A]").await.unwrap();
        assert_eq!(mem.profile_get("holdings").await.unwrap().as_deref(), Some("[A]"), "re-stored prior value must win");
        // a different key stays independent
        mem.profile_set("name", "Pranab").await.unwrap();
        assert_eq!(mem.profile_get("name").await.unwrap().as_deref(), Some("Pranab"));
        assert_eq!(mem.profile_get("holdings").await.unwrap().as_deref(), Some("[A]"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn goals_prefs_dedup_paraphrases_keep_distinct() {
        // Regression: consolidation re-phrases the same goal/pref every pass — paraphrases must collapse
        // (they flooded the store with ~280 near-dups), while distinct intents stay separate.
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        mem.store_preference("Prefers terse one-line summaries").await.unwrap();
        mem.store_preference("Prefers terse, one-line summaries when possible").await.unwrap(); // paraphrase
        mem.store_preference("Likes dark mode in the editor").await.unwrap(); // distinct
        let prefs = mem.list_preferences().await.unwrap();
        assert_eq!(prefs.len(), 2, "paraphrase collapses, distinct stays: {:?}", prefs.iter().map(|p| &p.text).collect::<Vec<_>>());

        mem.store_goal("Buy a handbag and watch combo for wife by July 23").await.unwrap();
        mem.store_goal("buy a handbag + watch combo under $200 for wife before July 23, 2026").await.unwrap(); // paraphrase
        mem.store_goal("Track GitHub repositories for new issues").await.unwrap(); // distinct
        let goals = mem.list_goals().await.unwrap();
        assert_eq!(goals.len(), 2, "gift paraphrase collapses, repo-tracking stays: {:?}", goals.iter().map(|g| &g.text).collect::<Vec<_>>());

        // Normalization dedup: a short goal whose only difference is case/punctuation must collapse even
        // though jaccard skips it (<2 significant words). "Exercise" / "exercise." → one entry.
        mem.store_preference("Exercise").await.unwrap();
        mem.store_preference("exercise.").await.unwrap(); // pure formatting variant → SAME entry
        let ex: Vec<_> = mem.list_preferences().await.unwrap().into_iter().filter(|p| p.text.to_lowercase().starts_with("exercise")).collect();
        assert_eq!(ex.len(), 1, "case/punctuation variant of a short goal collapses: {:?}", ex.iter().map(|p| &p.text).collect::<Vec<_>>());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn beliefs_reinforce_restatement_keep_contradiction_separate() {
        // A near-identical restatement reinforces the SAME node; a contradicting version (low overlap)
        // stays a SEPARATE node so contradiction detection survives.
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let assert = |s: &str| {
            let s = s.to_string();
            let m = mem.clone();
            async move {
                m.remember_as_belief(BeliefAssertion { statement: s, polarity: 1.0, weight: 0.8, source_event: None, provenance: "test".into() }).await.unwrap()
            }
        };
        assert("The latest stable Rust release is 1.70").await;
        assert("the latest stable Rust release is 1.70.").await; // formatting/case variant → SAME node
        assert("The latest stable Rust release is 1.96").await; // different content → SEPARATE node
        let hits = mem.recall_typed(RecallQuery { text: "latest stable Rust release".into(), top_k: 10, kind: None }).await.unwrap();
        let rust: Vec<_> = hits.iter().filter(|r| r.item.text.contains("Rust release")).collect();
        assert_eq!(rust.len(), 2, "formatting variant merges, contradiction (1.70 vs 1.96) stays separate: {:?}", rust.iter().map(|r| &r.item.text).collect::<Vec<_>>());
    }

    /// THE GROUP-CHAT MOAT: a private fact from one member must NEVER surface to another. The
    /// surprise-gift guarantee — cannot be prompt-engineered open because it's filtered at recall.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn read_isolation_keeps_a_private_belief_from_another_member() {
        use mind_types::Scope;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let (primary, wife) = (Scope::Private("primary".into()), Scope::Private("wife".into()));
        // Pranab (primary), in a private DM, tells the bot his surprise gift plan.
        mem.remember_as_belief_scoped(BeliefAssertion { statement: "I am getting my wife a gold watch for her birthday".into(), polarity: 1.0, weight: 0.9, source_event: None, provenance: "told".into() }, primary.clone()).await.unwrap();
        // A SHARED household fact (told in the group).
        mem.remember_as_belief_scoped(BeliefAssertion { statement: "The household is out of milk".into(), polarity: 1.0, weight: 0.8, source_event: None, provenance: "told".into() }, Scope::Shared).await.unwrap();
        let q = |t: &str| RecallQuery { text: t.into(), top_k: 10, kind: None };

        // The WIFE must NOT see the private gift belief.
        let wife_view = mem.recall_typed_as(q("birthday gift watch"), wife.clone()).await.unwrap();
        assert!(!wife_view.iter().any(|r| r.item.text.contains("gold watch")), "LEAK: wife saw the surprise: {:?}", wife_view.iter().map(|r| &r.item.text).collect::<Vec<_>>());
        // Pranab MUST see his own private belief.
        let p_view = mem.recall_typed_as(q("birthday gift watch"), primary.clone()).await.unwrap();
        assert!(p_view.iter().any(|r| r.item.text.contains("gold watch")), "primary must see his own private belief");
        // BOTH see the shared milk fact.
        assert!(mem.recall_typed_as(q("out of milk"), wife.clone()).await.unwrap().iter().any(|r| r.item.text.contains("milk")), "wife sees shared");
        assert!(mem.recall_typed_as(q("out of milk"), primary).await.unwrap().iter().any(|r| r.item.text.contains("milk")), "primary sees shared");
        // The wife's GROUNDING (working set) must also exclude the gift — the LLM never even sees it.
        let ws = mem.hydrate_working_set_as("birthday gift watch", wife).await.unwrap();
        let grounded: Vec<String> = ws.stable_facts.iter().map(|m| m.text.clone()).chain(ws.uncertain_beliefs.iter().map(|b| b.statement.clone())).collect();
        assert!(!grounded.iter().any(|t| t.contains("gold watch")), "LEAK in grounding: {grounded:?}");
    }

    /// THE MOAT: typed belief + Bayesian revision + contradiction detection + explanation,
    /// all through the clean async facade. This is what flat-RAG assistants cannot do.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn belief_revision_contradiction_and_explanation() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();

        // Positive evidence raises confidence above the 0.5 prior.
        let b = mem
            .remember_as_belief(BeliefAssertion {
                statement: "Pranab prefers terse replies".into(),
                polarity: 1.0,
                weight: 2.0,
                source_event: Some("he told me".into()),
                provenance: "told".into(),
            })
            .await
            .unwrap();
        assert!(b.confidence > 0.5, "positive evidence should raise confidence, got {}", b.confidence);
        assert_eq!(b.id, "Pranab prefers terse replies");

        // Recall finds it by overlapping words.
        let r = mem
            .recall_typed(RecallQuery { text: "reply style terse".into(), top_k: 5, kind: None })
            .await
            .unwrap();
        assert!(r.iter().any(|x| x.item.text.contains("terse")), "recall should surface the belief");

        // A contradicting belief + an explicit contradiction link.
        mem.remember_as_belief(BeliefAssertion {
            statement: "Pranab prefers long detailed replies".into(),
            polarity: 1.0,
            weight: 2.0,
            source_event: None,
            provenance: "inferred".into(),
        })
        .await
        .unwrap();
        mem.relate(
            "Pranab prefers terse replies",
            "Pranab prefers long detailed replies",
            "contradicts",
            0.9,
        )
        .await
        .unwrap();

        let conflicts = mem.conflicts().await.unwrap();
        assert!(!conflicts.is_empty(), "the contradiction should be detected");
        assert!(conflicts.iter().any(|c| c.belief_a.contains("terse") || c.belief_b.contains("terse")));

        // Explanation returns the belief with its evidence trail.
        let (belief, _ev) = mem
            .explain_belief("Pranab prefers terse replies")
            .await
            .unwrap()
            .expect("belief exists");
        assert!(belief.confidence > 0.5);
        assert!(belief.evidence_count >= 1, "belief should carry its evidence trail");

        // Negative evidence pushes a belief's confidence down.
        let down = mem
            .remember_as_belief(BeliefAssertion {
                statement: "Pranab is in Tokyo".into(),
                polarity: -1.0,
                weight: 2.0,
                source_event: None,
                provenance: "inferred".into(),
            })
            .await
            .unwrap();
        assert!(down.confidence < 0.5, "negative evidence should lower confidence, got {}", down.confidence);
    }

    /// The CHEAP task tier: plain CRUD, no cognitive ops, in the same store.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cheap_task_crud_and_completion() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let t = mem.add_task("finish the Q3 report", "high", None).await.unwrap();
        assert_eq!(t.status, "pending");
        assert_eq!(t.priority, "high");

        let open = mem.list_tasks(false).await.unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].description, "finish the Q3 report");

        assert!(mem.complete_task(&t.id).await.unwrap());
        assert!(mem.list_tasks(false).await.unwrap().is_empty(), "completed task should drop off the open list");
        assert_eq!(mem.list_tasks(true).await.unwrap().len(), 1, "but still present when including done");

        // tasks ride into the working-set as commitments (for grounding)
        mem.add_task("call the dentist", "medium", None).await.unwrap();
        let ws = mem.hydrate_working_set("what's on my plate").await.unwrap();
        assert!(ws.commitments.iter().any(|c| c.text.contains("dentist")), "open task should surface in working-set");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn add_task_dedups_paraphrases_keeps_distinct_intents() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        // commitment-extraction re-creates the SAME task as different wording each pass — these must collapse
        mem.add_task("Build a live-updating web page tracking top 10 handbag + watch combos under $200", "medium", None).await.unwrap();
        mem.add_task("Build and deliver a live-updating web page tracking the top 10 handbag and watch combos under $200", "medium", None).await.unwrap();
        mem.add_task("Build a live-updating web page with the top 10 handbag and watch combos under $200", "medium", None).await.unwrap();
        assert_eq!(mem.list_tasks(false).await.unwrap().len(), 1, "paraphrased page tasks collapse to one");
        // a genuinely different intent is NOT swallowed
        mem.add_task("Order wife's birthday gift by July 17th to ensure delivery by July 23rd", "high", None).await.unwrap();
        assert_eq!(mem.list_tasks(false).await.unwrap().len(), 2, "distinct intent stays separate");
        // and its own paraphrase dedups against it
        mem.add_task("Order wife's birthday gift (handbag + watch combo) by July 17th to ensure delivery by July 23rd", "high", None).await.unwrap();
        assert_eq!(mem.list_tasks(false).await.unwrap().len(), 2, "gift paraphrase collapses too");
    }

    /// THE EMBEDDER MOAT (yantrikdb 0.9.0): at dim 64 the engine auto-attaches its bundled
    /// model2vec embedder, so recall is genuinely SEMANTIC — a paraphrase that shares *no words*
    /// with the stored belief still surfaces it. This is what keyword recall structurally cannot do.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn semantic_recall_with_bundled_embedder() {
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        for s in [
            "the cat sat quietly on the mat",
            "Pranab prefers concise answers",
            "the stock market fell sharply today",
        ] {
            mem.remember_as_belief(BeliefAssertion {
                statement: s.into(),
                polarity: 1.0,
                weight: 2.0,
                source_event: None,
                provenance: "told".into(),
            })
            .await
            .unwrap();
        }
        // "he likes short responses" shares no keywords with "Pranab prefers concise answers".
        let r = mem
            .recall_typed(RecallQuery { text: "he likes short responses".into(), top_k: 1, kind: None })
            .await
            .unwrap();
        assert!(!r.is_empty(), "semantic recall returned nothing");
        assert!(
            r[0].item.text.contains("concise"),
            "semantic recall should rank the paraphrase first, got: {:?} (why: {:?})",
            r[0].item.text,
            r[0].why
        );
    }

    /// SEMANTIC SKILL RECALL (earned by the bundled embedder): a paraphrased need finds the right
    /// banked skill even with no shared keywords, and ranks it above an unrelated skill.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn semantic_skill_recall_ranks_paraphrase_first() {
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        for (name, summary, tags) in [
            ("csv_row_counter", "counts the number of rows in a CSV file", vec!["csv", "data"]),
            ("greeter", "prints a friendly hello greeting", vec!["text"]),
        ] {
            mem.save_skill(Skill {
                name: name.into(),
                lang: "python".into(),
                code: "print(1)".into(),
                summary: summary.into(),
                tags: tags.into_iter().map(String::from).collect(),
                status: "active".into(),
                runs: 3,
                successes: 3,
                created_ms: 0,
            })
            .await
            .unwrap();
        }
        // "how many lines are in a spreadsheet" shares no keywords with "counts rows in a CSV file".
        let hits = mem.recall_skills("how many lines are in a spreadsheet", 3).await.unwrap();
        assert!(!hits.is_empty(), "semantic skill recall returned nothing");
        assert_eq!(
            hits[0].name, "csv_row_counter",
            "the CSV skill should rank first for the paraphrase, got: {:?}",
            hits.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    /// recall_typed must carry evidence_count so the rehearse phase can detect fragile
    /// single-source certainty and emit a VerificationDebt tension.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recall_typed_item_carries_evidence_count() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        mem.remember_as_belief(BeliefAssertion {
            statement: "the earth orbits the sun".into(),
            polarity: 1.0,
            weight: 2.0,
            source_event: Some("astronomy class".into()),
            provenance: "told".into(),
        })
        .await
        .unwrap();
        let recalled = mem
            .recall_typed(RecallQuery { text: "earth sun orbit".into(), top_k: 5, kind: None })
            .await
            .unwrap();
        let hit = recalled.iter().find(|r| r.item.text.contains("earth")).expect("belief not recalled");
        assert_eq!(hit.item.evidence_count, 1, "one assertion → evidence_count must be 1");

        // A second assertion on the same belief increments the count.
        mem.remember_as_belief(BeliefAssertion {
            statement: "the earth orbits the sun".into(),
            polarity: 1.0,
            weight: 1.5,
            source_event: None,
            provenance: "inferred".into(),
        })
        .await
        .unwrap();
        let recalled2 = mem
            .recall_typed(RecallQuery { text: "earth sun orbit".into(), top_k: 5, kind: None })
            .await
            .unwrap();
        let hit2 = recalled2.iter().find(|r| r.item.text.contains("earth")).expect("belief not recalled");
        assert_eq!(hit2.item.evidence_count, 2, "two assertions → evidence_count must be 2");
    }

    /// reflect() must surface each belief's true evidence_count so that single-source
    /// fragility is visible to the DMN's VerificationDebt logic.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reflect_belief_carries_evidence_count() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        mem.remember_as_belief(BeliefAssertion {
            statement: "the sky is blue".into(),
            polarity: 1.0,
            weight: 2.0,
            source_event: Some("observation".into()),
            provenance: "told".into(),
        })
        .await
        .unwrap();
        // Single-source: reflect must report evidence_count == 1 (not 0).
        let reflection = mem.reflect("sky colour").await.unwrap();
        let belief = reflection.beliefs.iter().find(|b| b.statement.contains("sky")).expect("belief missing from reflection");
        assert_eq!(belief.evidence_count, 1, "reflect must propagate evidence_count from recalled item, got 0");

        // A second assertion increments to 2 — reflect tracks it too.
        mem.remember_as_belief(BeliefAssertion {
            statement: "the sky is blue".into(),
            polarity: 1.0,
            weight: 1.0,
            source_event: None,
            provenance: "inferred".into(),
        })
        .await
        .unwrap();
        let reflection2 = mem.reflect("sky colour").await.unwrap();
        let belief2 = reflection2.beliefs.iter().find(|b| b.statement.contains("sky")).expect("belief missing from second reflection");
        assert_eq!(belief2.evidence_count, 2, "reflect must track accumulated evidence_count");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn store_goal_is_idempotent() {
        let mem = MemoryHandle::spawn(":memory:", 4).unwrap();
        mem.store_goal("become more self-aware").await.unwrap();
        mem.store_goal("become more self-aware").await.unwrap();
        mem.store_goal("become more self-aware").await.unwrap();
        let goals = mem.list_goals().await.unwrap();
        assert_eq!(goals.len(), 1, "duplicate store_goal calls must not multiply entries");
    }

    /// assert_belief must immediately emit a Contradiction tension for any conflict that exists
    /// at the time the belief is persisted — no explicit rehearsal sweep required.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn assert_belief_emits_contradiction_tension() {
        let mem = MemoryHandle::spawn(":memory:", 4).unwrap();

        // Establish two contradicting beliefs and link them.
        mem.remember_as_belief(BeliefAssertion {
            statement: "Pranab sleeps early".into(),
            polarity: 1.0,
            weight: 2.0,
            source_event: None,
            provenance: "told".into(),
        })
        .await
        .unwrap();
        mem.remember_as_belief(BeliefAssertion {
            statement: "Pranab stays up late".into(),
            polarity: 1.0,
            weight: 2.0,
            source_event: None,
            provenance: "observed".into(),
        })
        .await
        .unwrap();
        mem.relate("Pranab sleeps early", "Pranab stays up late", "contradicts", 0.8)
            .await
            .unwrap();

        // Before the next assert_belief there should be no tension yet.
        let before = mem.open_tensions(20).await.unwrap();
        assert!(
            before.iter().all(|t| !matches!(t.kind, mind_types::TensionKind::Contradiction)),
            "no contradiction tension expected before any assert_belief triggers the scan"
        );

        // A new assert_belief triggers the scan — the pre-existing conflict must now appear.
        mem.remember_as_belief(BeliefAssertion {
            statement: "Pranab sleeps early".into(),
            polarity: 1.0,
            weight: 0.5,
            source_event: Some("second observation".into()),
            provenance: "inferred".into(),
        })
        .await
        .unwrap();

        let after = mem.open_tensions(20).await.unwrap();
        assert!(
            after.iter().any(|t| matches!(t.kind, mind_types::TensionKind::Contradiction)),
            "assert_belief should have emitted a Contradiction tension for the known conflict"
        );
        let tension = after.iter().find(|t| matches!(t.kind, mind_types::TensionKind::Contradiction)).unwrap();
        assert!(
            tension.about.contains("sleeps early") || tension.about.contains("stays up late"),
            "tension description should name the conflicting beliefs, got: {}",
            tension.about
        );
        assert!(tension.pressure >= 0.3, "pressure should be clamped to at least 0.3, got {}", tension.pressure);
    }

    /// KeywordScorer: confidence breaks the tie when two beliefs have identical keyword overlap.
    /// Both "flower" and "red car" have exactly 1 match with query "red flower"; "flower" earns
    /// higher confidence via 5 Bayesian updates vs 1, so it must rank first. Exercises the same
    /// BeliefScorer → sort → truncate → Recalled pipeline as EmbedderScorer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn keyword_scorer_confidence_breaks_overlap_tie() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();

        // 5 positive assertions → high posterior confidence.
        for _ in 0..5 {
            mem.remember_as_belief(BeliefAssertion {
                statement: "a lovely flower grows here".into(),
                polarity: 1.0,
                weight: 2.0,
                source_event: None,
                provenance: "told".into(),
            })
            .await
            .unwrap();
        }
        // 1 assertion → lower confidence (same prior, same weight, fewer updates).
        mem.remember_as_belief(BeliefAssertion {
            statement: "the red car is fast".into(),
            polarity: 1.0,
            weight: 2.0,
            source_event: None,
            provenance: "told".into(),
        })
        .await
        .unwrap();
        // Zero overlap with "red flower" — must rank last regardless of confidence.
        mem.remember_as_belief(BeliefAssertion {
            statement: "the sky is completely blue".into(),
            polarity: 1.0,
            weight: 2.0,
            source_event: None,
            provenance: "told".into(),
        })
        .await
        .unwrap();

        let hits = mem
            .recall_typed(RecallQuery { text: "red flower".into(), top_k: 10, kind: None })
            .await
            .unwrap();

        let flower_pos = hits.iter().position(|r| r.item.text.contains("flower")).expect("flower belief missing");
        let red_pos = hits.iter().position(|r| r.item.text.contains("red car")).expect("red car belief missing");
        let sky_pos = hits.iter().position(|r| r.item.text.contains("blue")).expect("sky belief missing");

        assert!(
            flower_pos < red_pos,
            "higher-confidence belief (5 assertions) must outrank equal-overlap lower-confidence one (1 assertion); got: {:?}",
            hits.iter().map(|r| (&r.item.text, r.score)).collect::<Vec<_>>()
        );
        assert!(
            sky_pos > red_pos,
            "zero-overlap belief must rank below any one-overlap belief; got: {:?}",
            hits.iter().map(|r| (&r.item.text, r.score)).collect::<Vec<_>>()
        );
        // KeywordScorer always emits "confidence" in the why — distinguishes it from embedder path.
        assert!(
            hits.iter().all(|r| r.why.iter().any(|w| w.contains("confidence"))),
            "KeywordScorer why strings must contain 'confidence'; got: {:?}",
            hits.iter().map(|r| &r.why).collect::<Vec<_>>()
        );
    }
}
