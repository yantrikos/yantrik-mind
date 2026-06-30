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
    MemoryKind, MindError, RecallQuery, Recalled, Reflection, Result, Skill, Task, WorkingSet,
};

use yantrikdb_core::belief::{BeliefRevisionConfig, Evidence as YEvidence};
use yantrikdb_core::belief_query::BeliefPattern;
use yantrikdb_core::contradiction::ContradictionConfig;
use yantrikdb_core::state::{
    sigmoid, BeliefPayload, CognitiveEdge, CognitiveEdgeKind, CognitiveNode, NodeId,
    NodeIdAllocator, NodeKind, NodePayload, Priority, Provenance, TaskPayload, TaskStatus,
};
use yantrikdb_core::YantrikDB;

type Reply<T> = oneshot::Sender<std::result::Result<T, String>>;

enum Cmd {
    Record { text: String, reply: Reply<String> },
    RememberObservation { text: String, source: String, reply: Reply<String> },
    GetText { rid: String, reply: Reply<Option<String>> },
    AssertBelief { statement: String, signed_weight: f64, source: String, provenance: String, reply: Reply<Belief> },
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
    AppendMessage { role: String, text: String, reply: Reply<()> },
    RecentMessages { limit: usize, reply: Reply<Vec<(String, String)>> },
    MessagesSince { after_id: i64, limit: usize, reply: Reply<Vec<(i64, String, String)>> },
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
    }
}

fn find_belief(db: &YantrikDB, statement: &str) -> Option<CognitiveNode> {
    all_beliefs(db).into_iter().find(|n| node_prop(n) == Some(statement))
}

fn assert_belief(
    db: &YantrikDB,
    alloc: &mut NodeIdAllocator,
    statement: &str,
    signed_weight: f64,
    source: &str,
    provenance: &str,
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

/// Belief recall. Beliefs live in `cognitive_nodes` (not the flat HNSW index), so when an embedder
/// is attached we rank by cosine similarity of the query vs each proposition (model2vec is in-process
/// and fast), blended with a small confidence prior so a confident near-match outranks a vague exact
/// one. With no embedder (test builds) we fall back to keyword overlap + confidence — the prior shape.
fn recall_beliefs(db: &YantrikDB, text: &str, top_k: usize) -> Vec<Recalled> {
    let beliefs = all_beliefs(db);

    if db.has_embedder() {
        if let Ok(q) = db.embed(text) {
            let mut scored: Vec<(f64, f64, CognitiveNode)> = beliefs
                .into_iter()
                .map(|n| {
                    let prop = node_prop(&n).unwrap_or("");
                    let sim = db.embed(prop).ok().map(|v| cosine(&q, &v)).unwrap_or(0.0);
                    let blended = sim + 0.1 * n.attrs.confidence;
                    (blended, sim, n)
                })
                .collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            return scored
                .into_iter()
                .take(top_k.max(1))
                .map(|(blended, sim, n)| Recalled {
                    score: blended,
                    why: vec![format!("semantic {:.2}, confidence {:.2}", sim, n.attrs.confidence)],
                    item: belief_item(&n),
                })
                .collect();
        }
    }

    // keyword fallback (no embedder)
    let qwords: Vec<String> = text.to_ascii_lowercase().split_whitespace().map(|w| w.to_string()).collect();
    let mut scored: Vec<(f64, CognitiveNode)> = beliefs
        .into_iter()
        .map(|n| {
            let p = node_prop(&n).unwrap_or("").to_ascii_lowercase();
            let overlap = qwords.iter().filter(|w| p.contains(w.as_str())).count() as f64;
            (overlap + n.attrs.confidence, n)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(top_k.max(1))
        .map(|(score, n)| Recalled {
            score,
            why: vec![format!("confidence {:.2}", n.attrs.confidence)],
            item: belief_item(&n),
        })
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
    let new_sig = task_word_set(description);
    if !new_sig.is_empty() {
        for n in all_task_nodes(db) {
            if let NodePayload::Task(ref t) = n.payload {
                if matches!(t.status, TaskStatus::Completed | TaskStatus::Cancelled) {
                    continue;
                }
                if jaccard(&new_sig, &task_word_set(&t.description)) >= 0.6 {
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
    let _ = db.conn().execute(
        "CREATE TABLE IF NOT EXISTS mind_transcript \
         (id INTEGER PRIMARY KEY AUTOINCREMENT, role TEXT NOT NULL, text TEXT NOT NULL, ts REAL NOT NULL)",
        [],
    );
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

fn append_message(db: &YantrikDB, role: &str, text: &str) -> std::result::Result<(), String> {
    db.conn()
        .execute(
            "INSERT INTO mind_transcript (role, text, ts) VALUES (?1, ?2, ?3)",
            rusqlite::params![role, text, now_secs()],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn recent_messages(db: &YantrikDB, limit: usize) -> std::result::Result<Vec<(String, String)>, String> {
    // 0.9.0's `conn()` returns a temporary guard (was `&Connection`); bind it so the prepared
    // statement doesn't outlive a dropped temporary.
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT role, text FROM mind_transcript ORDER BY id DESC LIMIT ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([limit as i64], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut v: Vec<(String, String)> = rows.filter_map(|r| r.ok()).collect();
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
                        Cmd::AssertBelief { statement, signed_weight, source, provenance, reply } => {
                            let _ = reply.send(assert_belief(&db, &mut alloc, &statement, signed_weight, &source, &provenance));
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
                        Cmd::AppendMessage { role, text, reply } => {
                            let _ = reply.send(append_message(&db, &role, &text));
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
                        Cmd::RecentMessages { limit, reply } => {
                            let _ = reply.send(recent_messages(&db, limit));
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
        self.call(|reply| Cmd::AssertBelief { statement, signed_weight, source, provenance, reply }).await
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
                evidence_count: 0,
                updated_ms: r.item.updated_ms,
                status: "active".into(),
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
            let eff = decay_confidence(r.item.confidence, age_ms, halflife_days);
            if eff >= 0.7 {
                ws.stable_facts.push(MemoryItem { confidence: eff, ..r.item });
            } else {
                ws.uncertain_beliefs.push(Belief {
                    id: r.item.id.clone(),
                    statement: r.item.text.clone(),
                    confidence: eff,
                    certainty: r.item.certainty,
                    provenance: "recalled".into(),
                    evidence_count: 0,
                    updated_ms: r.item.updated_ms,
                    status: "active".into(),
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
        let (role, text) = (role.to_string(), text.to_string());
        self.call(|reply| Cmd::AppendMessage { role, text, reply }).await
    }
    async fn messages_since(&self, after_id: i64, limit: usize) -> Result<Vec<(i64, String, String)>> {
        self.call(|reply| Cmd::MessagesSince { after_id, limit, reply }).await
    }
    async fn recent_messages(&self, limit: usize) -> Result<Vec<(String, String)>> {
        self.call(|reply| Cmd::RecentMessages { limit, reply }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn store_goal_is_idempotent() {
        let mem = MemoryHandle::spawn(":memory:", 4).unwrap();
        mem.store_goal("become more self-aware").await.unwrap();
        mem.store_goal("become more self-aware").await.unwrap();
        mem.store_goal("become more self-aware").await.unwrap();
        let goals = mem.list_goals().await.unwrap();
        assert_eq!(goals.len(), 1, "duplicate store_goal calls must not multiply entries");
    }
}
