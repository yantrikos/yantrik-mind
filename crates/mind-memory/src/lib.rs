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

pub mod receipts;

use async_trait::async_trait;
use tokio::sync::mpsc;
use rusqlite::OptionalExtension;
use tokio::sync::oneshot;

use mind_types::{
    AuthError, Belief, BeliefAssertion, Contradiction, Evidence as MEvidence, MemoryFacade,
    MemoryItem, MemoryKind, MindError, RecallQuery, Recalled, Reflection, Result, Skill, Task,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAuthorization {
    Authorized,
    Unauthorized,
    /// Authorization state could not be determined (e.g. the auth check itself failed).
    /// The handle can be constructed, but restricted operations (e.g. retro-dedup) will
    /// return `MindError::NotAuthorized` rather than proceeding with unusable state.
    Unknown,
}

enum Cmd {
    Record { text: String, reply: Reply<String> },
    RememberObservation { text: String, source: String, reply: Reply<String> },
    GetText { rid: String, reply: Reply<Option<String>> },
    AssertBelief { statement: String, signed_weight: f64, source: String, provenance: String, evidence_version: Option<u64>, reply: Reply<Belief> },
    RecallTyped { text: String, top_k: usize, reply: Reply<Vec<Recalled>> },
    BeliefsMatching { needle: String, limit: usize, reply: Reply<Vec<Belief>> },
    Conflicts { reply: Reply<Vec<Contradiction>> },
    Explain { statement: String, reply: Reply<Option<(Belief, Vec<MEvidence>)>> },
    Relate { src: String, dst: String, rel: String, weight: f64, reply: Reply<()> },
    // Belief lifecycle: every tombstone carries a reason ("user-deleted" must stay
    // distinguishable from hygiene forever); None = legacy caller → "unspecified".
    Forget { statement: String, reason: Option<String>, reply: Reply<bool> },
    Tombstones { reply: Reply<Vec<(String, String, u64)>> },
    Export { reply: Reply<String> },
    // cheap task tier (plain node CRUD — no cognitive ops)
    AddTask { description: String, priority: String, due_ms: Option<u64>, reply: Reply<Task> },
    ListTasks { include_done: bool, reply: Reply<Vec<Task>> },
    CompleteTask { id: String, reply: Reply<bool> },
    // cheap raw transcript (immediate context; isolated table, not the cognitive graph)
    AppendMessage { role: String, text: String, scope: String, reply: Reply<()> },
    RecentMessages { limit: usize, viewer: Option<String>, reply: Reply<Vec<(String, String)>> },
    MessagesSince { after_id: i64, limit: usize, reply: Reply<Vec<(i64, String, String)>> },
    UserTurnTimes { since_ms: i64, reply: Reply<Vec<i64>> },
    ProactiveBaselineRate { reply: Reply<Option<f64>> },
    RecordProactiveOutcomeBackfill { sent_ms: i64, engaged: bool, reply: Reply<()> },
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
    // Attachable expertise. Mount/unmount are process-local; install copies the pack beside the db
    // so it comes back on every open.
    MountPack { path: String, reply: Reply<String> },
    InstallPack { path: String, reply: Reply<String> },
    UnmountPack { id: String, reply: Reply<()> },
    UninstallPack { id: String, reply: Reply<bool> },
    ListApproaches { limit: usize, reply: Reply<Vec<String>> },
    MountedPacks { reply: Reply<Vec<mind_types::memory::PackBrief>> },
    /// Seal the given craft texts into a pack file: stage them in a dedicated namespace, seal THAT
    /// namespace only, then remove the staging rows win or lose. The texts arrive pre-gathered and
    /// pre-filtered (see `seal_learned_pack`) — the actor only does the parts that need the db.
    SealCraftPack { dest: String, name: String, version: String, texts: Vec<String>, reply: Reply<u64> },
    PackContext { reply: Reply<Option<String>> },
    RecallFromPacks { query: String, top_k: usize, reply: Reply<Vec<mind_types::memory::PackHit>> },
    ProbePacks { query: String, top_k: usize, reply: Reply<Vec<mind_types::memory::PackProbe>> },
    RecordPackEvent { pack_id: String, event: mind_types::memory::PackEvent, reply: Reply<()> },
    PackStats { reply: Reply<Vec<mind_types::memory::PackStats>> },
    // goals / preferences (plain text CRUD; no Bayesian revision)
    StoreGoalPref { kind: String, text: String, reply: Reply<()> },
    ListGoalPrefs { kind: String, reply: Reply<Vec<MemoryItem>> },
    // profile KV (single value per key, latest-wins — distinct from append-distinct goals/prefs)
    SetProfile { key: String, value: String, reply: Reply<()> },
    // group-chat read-isolation: per-belief visibility scope (keyed by proposition)
    SetBeliefScope { proposition: String, scope: String, reply: Reply<()> },
    BeliefScopeMap { reply: Reply<std::collections::HashMap<String, String>> },
    // Purpose Gate v1: explicit per-belief sensitivity overrides + standing purpose grants
    SetBeliefSensitivity { proposition: String, class: String, reply: Reply<()> },
    BeliefSensitivityMap { reply: Reply<std::collections::HashMap<String, String>> },
    GrantPurpose { spec: mind_types::PurposeGrantSpec, reply: Reply<i64> },
    RevokePurposeGrant { id: i64, reply: Reply<bool> },
    ListPurposeGrants { reply: Reply<Vec<mind_types::PurposeGrant>> },
    // tension economy (the "urges" drives emit; plain CRUD ledger)
    RecordTension { kind: String, pressure: f64, about: String, reply: Reply<()> },
    OpenTensions { limit: usize, reply: Reply<Vec<mind_types::Tension>> },
    DischargeTension { id: String, reply: Reply<bool> },
    ExpireStaleTensions { curiosity_days: i64, other_days: i64, reply: Reply<usize> },
    TensionOutcomeCounts { reply: Reply<(usize, usize)> },
    RecallDemandFor { about: String, reply: Reply<f64> },
    // retro-dedup: collapse norm_prop/Jaccard near-duplicates written before the write-path dedup existed
    RetroDedupStore { reply: Reply<(usize, usize)> },
    // immune harness: point-in-time snapshot of the live DB (seeded-belief trials run on the COPY)
    SnapshotTo { dest: String, reply: Reply<()> },
    // test-only: insert a goal/pref row bypassing all dedup checks (simulates pre-PR#19 legacy data)
    #[cfg(test)]
    ForceInsertGoalPref { kind: String, text: String, reply: Reply<()> },
}

// ── actor scheduling doctrine (measured 2026-08-24) ──────────────────────────
//
// The actor owns ONE YantrikDB on ONE thread (`!Sync` substrate), so it cannot serve two
// commands at once, and NO queue arrangement changes that. Priority lanes were built and
// measured here: an in-flight bulk command stalled an interactive read for 65ms of its 70ms
// duration WITH lanes in place — lanes reorder QUEUED work, nothing preempts a RUNNING
// command, and forcing every command back through one FIFO changed nothing once the real fix
// landed. The real fix is the one that measured out:
//
//   1. A command that does not need actor state must not occupy the actor. SnapshotTo opens
//      its own read-only connection and runs off-thread; live reads now complete in <10% of
//      a running snapshot's wall time.
//   2. Everything else stays FIFO — simple, causally transparent (same-caller ordering by
//      await; read-your-writes by construction).
//
// If a future command stalls turns the way VACUUM INTO did, the pattern to copy is #1:
// make it self-contained or split it into an off-thread detect phase plus a small
// actor-applied commit phase. Do NOT reintroduce lanes for it — that was the measured dead
// end. Candidates if ever needed (all currently fast enough on production-scale data):
// Export (~whole-store read), RetroDedupStore (~pairwise scan), SealCraftPack.

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

/// Consistent point-in-time snapshot of the live database into `dest`.
///
/// Immune-harness support (co-designed with gpt-5.6-sol, 2026-07-10): seeded
/// false-belief trials must run on a COPY the critic sandbox owns — never the
/// live namespace. Runs on the actor thread, so no mind-side write can
/// interleave; `VACUUM INTO` takes its own read transaction and is WAL-safe
/// (a plain file copy is not). `dest` must not already exist — refusing to
/// overwrite is the cheap guard against a reversed argument order ever
/// pointing this at the live file.
pub fn snapshot_db_to(live_path: &str, dest: &str) -> std::result::Result<(), String> {
    if live_path == ":memory:" {
        return Err("cannot snapshot a :memory: mind — no durable file to copy".into());
    }
    if std::path::Path::new(dest).exists() {
        return Err(format!("snapshot destination already exists: {dest}"));
    }
    let conn = rusqlite::Connection::open_with_flags(
        live_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("open live db read-only: {e}"))?;
    conn.execute("VACUUM INTO ?1", rusqlite::params![dest])
        .map_err(|e| format!("VACUUM INTO {dest}: {e}"))?;
    Ok(())
}

fn all_beliefs(db: &YantrikDB) -> Vec<CognitiveNode> {
    // NOT query_beliefs: its loader (load_cognitive_nodes_by_kind) silently caps at 1,000 nodes —
    // with 2,400+ beliefs, everything taught recently fell past the cap and became unrecallable.
    // Query the graph directly with a real limit.
    db.query_cognitive_nodes(&yantrikdb_core::CognitiveNodeFilter {
        kinds: vec![yantrikdb_core::NodeKind::Belief],
        limit: 100_000,
        ..Default::default()
    })
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

/// Normalize a proposition before storage while preserving meaningful case.
fn normalize_belief_text(s: &str) -> String {
    s.trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '!' | '?' | ','))
        .to_string()
}

/// Normalize a proposition for comparison: lowercase and collapse whitespace.
/// Merges trivial formatting/case restatements ("July 23" / "july 23.") WITHOUT touching content —
/// "…Rust is 1.70" and "…Rust is 1.96" stay DISTINCT, so contradictions remain separate nodes.
/// (Word-overlap dedup is unsafe here: it strips the very tokens — numbers/versions — that
/// distinguish contradicting claims.)
fn norm_prop(s: &str) -> String {
    normalize_belief_text(s).to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
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
    let statement = normalize_belief_text(statement);
    let node = match find_belief(db, &statement) {
        Some(n) => n,
        None => {
            let id = alloc.alloc(NodeKind::Belief);
            let mut n = CognitiveNode::new(
                id,
                statement.clone(),
                NodePayload::Belief(BeliefPayload {
                    proposition: statement.clone(),
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
    let canonical = node_prop(&node).unwrap_or(&statement).to_string();
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

/// Every banked approach, newest first — a deterministic LIKE scan, not similarity search.
///
/// This exists because the approaches were WRITE-ONLY: `bank_procedure` stores them as episodic
/// memories via `remember_observation`, while `recall_typed` (the path the loop read them back
/// through) scores only Belief-kind cognitive nodes — so every banked approach was unreachable
/// from the moment it was saved, and the roundtrip test never noticed because its assertion sat
/// behind an `if let`. The prefix contract is the same one `split_routine` parses and banking
/// writes; enumeration is capped and newest-first so the freshest craft wins a tie.
fn list_approaches(db: &YantrikDB, limit: usize) -> std::result::Result<Vec<String>, String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(
            "SELECT text FROM memories \
             WHERE (text LIKE 'APPROACH:%' OR text LIKE 'PROCEDURE:%') \
               AND consolidation_status != 'tombstoned' \
             ORDER BY rowid DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([limit as i64], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Tables a sealed craft pack may carry. Everything else in the file is DROPPED after sealing.
///
/// The engine's seal scrubs the tables IT knows about — but the mind bolts its own tables onto
/// the same database file (transcript, skills, belief scopes, tensions), and the engine's list
/// cannot know them. The first live seal proved it: a 26 MB pack carrying 1,944 rows of the
/// household's conversation transcript beside its 9 craft rows. A denylist loses that race
/// forever — every table added later starts out leaked. The allowlist inverts it: a pack IS
/// its corpus, the corpus's search index, its chunk vectors, and its manifest; anything else
/// in the file is a leak by definition.
const PACK_TABLE_ALLOWLIST: &[&str] = &["memories", "memory_chunks", "meta"];

/// Drop every non-allowlisted table from a freshly sealed pack, then VERIFY. Fail closed: the
/// caller deletes the file on any error — a pack that cannot be proven clean must not exist.
fn scrub_sealed_pack(dest: &str) -> std::result::Result<(), String> {
    let conn = rusqlite::Connection::open(dest).map_err(|e| e.to_string())?;
    // FK enforcement is per-connection; with it on, drop order matters and a referenced table
    // fails mid-scrub. The tables are being deleted wholesale — referential order is meaningless.
    conn.execute_batch("PRAGMA foreign_keys=OFF").map_err(|e| e.to_string())?;
    let names: Vec<(String, String)> = conn
        .prepare("SELECT name, type FROM sqlite_master WHERE type IN ('table','view')")
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
        })
        .map_err(|e| e.to_string())?;
    for (n, kind) in &names {
        let keep = PACK_TABLE_ALLOWLIST.contains(&n.as_str())
            || n.starts_with("memories_fts") // the kept corpus's FTS shadow family
            || n.starts_with("sqlite_"); // sqlite internals are not droppable
        if !keep {
            // The statement must match the object: DROP TABLE on a view errors even with IF
            // EXISTS ("use DROP VIEW to delete view edges" — the engine keeps `edges` as a view).
            let stmt = if kind == "view" { "DROP VIEW" } else { "DROP TABLE" };
            conn.execute_batch(&format!("{stmt} IF EXISTS \"{}\"", n.replace('"', "\"\"")))
                .map_err(|e| format!("dropping {n}: {e}"))?;
        }
    }
    conn.execute_batch("VACUUM").map_err(|e| e.to_string())?;
    // The verification is the point: enumerate what SURVIVED and refuse anything off-list.
    let survivors: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(0)).map(|rows| rows.filter_map(|r| r.ok()).collect()))
        .map_err(|e| e.to_string())?;
    for n in &survivors {
        if !PACK_TABLE_ALLOWLIST.contains(&n.as_str()) && !n.starts_with("memories_fts") {
            return Err(format!("table {n} survived the pack scrub — refusing to leave this file on disk"));
        }
    }
    Ok(())
}

/// Seal pre-gathered craft texts into a pack file (the LEARNING → PACKS direction).
///
/// The rows are STAGED into a dedicated namespace and the seal is scoped to exactly that
/// namespace — never `None`, which would export every private household row in the database.
/// The staging rows are removed afterward win or lose: they exist only to be exported, and
/// leaving them behind on a failed seal would make the next attempt double-export.
fn seal_craft_pack(
    db: &YantrikDB,
    dest: &str,
    name: &str,
    version: &str,
    texts: &[String],
) -> std::result::Result<u64, String> {
    if texts.is_empty() {
        return Err("nothing to seal — no banked approaches or skills survived the export filter".into());
    }
    const NS: &str = "learned-craft";
    let meta = serde_json::json!({ "source": "yantrik-mind self-learning" });
    let mut rids: Vec<String> = Vec::new();
    let mut stage_err: Option<String> = None;
    for t in texts {
        let r = if db.has_embedder() {
            db.record_text(t, "procedural", 0.7, 0.0, 604_800.0, &meta, NS, 0.8, "general", "system", None)
        } else {
            let zero = vec![0.0f32; db.embedding_dim()];
            db.record(t, "procedural", 0.7, 0.0, 604_800.0, &meta, &zero, NS, 0.8, "general", "system", None)
        };
        match r {
            Ok(rid) => rids.push(rid),
            Err(e) => {
                stage_err = Some(e.to_string());
                break;
            }
        }
    }
    let sealed = match stage_err {
        Some(e) => Err(format!("staging craft rows failed: {e}")),
        None => {
            let embedder = match db.embedder_identity() {
                Ok(Some((ename, digest, dim))) => serde_json::json!({ "name": ename, "digest": digest, "dim": dim }),
                _ => serde_json::json!({ "name": null, "digest": null, "dim": db.embedding_dim() }),
            };
            let coverage: Vec<String> = texts
                .iter()
                .filter_map(|t| t.lines().next().map(|l| l.chars().take(60).collect::<String>()))
                .take(8)
                .collect();
            // Built through serde rather than a struct literal ON PURPOSE: every optional
            // manifest field is #[serde(default)], so this compiles against any engine version
            // that has the required fields — a literal broke the build the moment the local
            // engine grew fields the deployment box's checkout did not have yet.
            let manifest: yantrikdb_core::PackManifest = match serde_json::from_value(serde_json::json!({
                "name": name,
                "version": version,
                "origin": format!("yantrik-mind/{name}"),
                "description": "Craft this mind learned by doing: banked approaches and measured skills.",
                "embedder": embedder,
                // The constitution frames the corpus honestly: these are ONE mind's local
                // measurements, and a mounting host must not read them as universal claims.
                "constitution": [
                    "These approaches were banked by a household mind from its own successful runs. \
                     Reliability notes are that mind's local measurements, not universal claims — \
                     prefer your own measured procedures where they exist."
                ],
                "coverage": coverage,
            })) {
                Ok(m) => m,
                Err(e) => {
                    for rid in &rids {
                        let _ = db.forget(rid);
                    }
                    return Err(format!("building the pack manifest failed: {e}"));
                }
            };
            db.seal_pack(dest, &manifest, Some(NS))
                .map_err(|e| e.to_string())
                .and_then(|m| {
                    // The engine sealed ITS tables clean; now drop the mind's own bolt-ons and
                    // verify. A pack that cannot be proven clean is deleted, not shipped.
                    match scrub_sealed_pack(dest) {
                        Ok(()) => Ok(m.corpus_rows),
                        Err(e) => {
                            let _ = std::fs::remove_file(dest);
                            Err(format!("pack scrub failed ({e}) — the file was removed"))
                        }
                    }
                })
        }
    };
    // Staging rows out, regardless of outcome.
    for rid in &rids {
        let _ = db.forget(rid);
    }
    sealed
}

/// The manifest fields the mind reads, lifted out of the engine's `PackManifest` by NAME through
/// serde rather than as struct fields ON PURPOSE: every optional manifest field is
/// `#[serde(default)]`, so this compiles against any engine version that has the required fields
/// — the deployment box's engine copy lags the local one, and a struct-field read of a field the
/// box's engine does not have yet is a build break there and nowhere else (the seal path learned
/// this first; see `seal_craft_pack`). A field the engine lacks reads as None, never as a lie.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
struct ManifestView {
    #[serde(default)]
    content_digest: Option<String>,
    #[serde(default)]
    coverage: Vec<String>,
    #[serde(default)]
    recommended_top_k: Option<u32>,
    #[serde(default)]
    recommended_min_similarity: Option<f64>,
    #[serde(default)]
    publisher_pubkey: Option<String>,
}

impl ManifestView {
    fn of(m: &yantrikdb_core::PackManifest) -> Self {
        serde_json::to_value(m).ok().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
    }
}

/// A mounted pack's manifest, read from its file on first use and cached by path — the FAILURE
/// too: an unreadable manifest is warned about once and remembered as `None` until the next
/// mount/unmount clears the cache, so a pack with a bad manifest costs one line, not one per recall.
///
/// An unreadable manifest makes the pack get the host wall rather than no floor. Never a reason to
/// skip the pack: the engine already vetted the file at mount, so an unreadable manifest here is a
/// race with a replaced file, not a bad pack.
fn cached_manifest<'a>(
    cache: &'a mut std::collections::HashMap<String, Option<ManifestView>>,
    path: &str,
) -> Option<&'a ManifestView> {
    if !cache.contains_key(path) {
        let read = match YantrikDB::read_manifest(path) {
            Ok(m) => Some(ManifestView::of(&m)),
            Err(e) => {
                tracing::warn!(path, error = %e, "pack manifest unreadable — the host wall applies until it is remounted");
                None
            }
        };
        cache.insert(path.to_string(), read);
    }
    cache.get(path).and_then(|m| m.as_ref())
}

/// Where one mounted pack's rows live and how its publisher said to retrieve them.
#[derive(Debug, Clone, PartialEq)]
struct PackRoute {
    pack_id: String,
    name: String,
    namespace: String,
    /// Similarity below this is withheld — the publisher's measured floor, else the default.
    floor: f64,
    /// At most this many rows from this pack per query, when the publisher measured one.
    cap: Option<usize>,
}

/// One engine recall result reduced to what the floor and the identity resolution need.
#[derive(Debug, Clone, PartialEq)]
struct PackCandidate {
    rid: String,
    text: String,
    score: f64,
    similarity: f64,
    namespace: String,
    /// The pack NAME the engine stamped on the hit (`why_retrieved: "pack:<name>"`), or None for
    /// a row the engine did not attribute to any pack — i.e. a host row that happens to share a
    /// namespace. Those are refused here regardless of namespace.
    pack_name: Option<String>,
}

impl PackCandidate {
    fn from_engine(r: yantrikdb_core::RecallResult) -> Self {
        let pack_name = r
            .why_retrieved
            .iter()
            .find_map(|w| w.strip_prefix("pack:").map(str::to_string));
        Self { rid: r.rid, text: r.text, score: r.score, similarity: r.scores.similarity, namespace: r.namespace, pack_name }
    }
}

/// The pure half of pack recall: floor on SIMILARITY, resolve each row to its pack, honour the
/// publisher's per-pack cap, rank by the engine's composite, return at most `want` — plus the
/// number of rows DROPPED AS AMBIGUOUS, which the caller must surface.
///
/// Identity resolution: a row is attributed only to the route that matches BOTH its namespace and
/// the engine's `pack:<name>` stamp — in every case, not only when a namespace is shared, because a
/// row stamped with a name no route carries is a pack this host did not route (a pack sealed without
/// a namespace, say, whose rows landed in another pack's) and must not be credited to the pack that
/// happens to own the namespace. When the stamp selects more than one route — two versions or two
/// re-seals of one pack mounted at once — the row is ABSTAINED from and counted, never handed to
/// whichever manifest was iterated first: a hit attributed to the wrong version would key that
/// version's evidence and floor on another's rows. (The engine's structured provenance in core 0.18
/// retires this; the lifecycle registry, ARCH-6 P.7, will refuse the second mount outright.) Rows
/// without any pack stamp are host rows and never pass, whatever namespace they sit in.
fn floor_pack_hits(candidates: Vec<PackCandidate>, routes: &[PackRoute], want: usize) -> (Vec<mind_types::memory::PackHit>, usize) {
    let (judged, ambiguous) = judge_pack_candidates(candidates, routes, want);
    let hits = judged
        .into_iter()
        .filter(|j| j.disposition == mind_types::memory::PackDisposition::Cleared)
        .map(|j| mind_types::memory::PackHit {
            pack_id: j.route.pack_id.clone(),
            rid: j.candidate.rid,
            text: j.candidate.text,
            score: j.candidate.score,
            similarity: j.candidate.similarity,
            namespace: j.candidate.namespace,
        })
        .collect();
    (hits, ambiguous)
}

/// A candidate after judgement: the route it belongs to and what recall did with it.
struct Judged<'r> {
    route: &'r PackRoute,
    candidate: PackCandidate,
    disposition: mind_types::memory::PackDisposition,
}

/// The ONE judgement, in rank order: floor first (a row under its pack's floor is withheld by the
/// floor whatever else is true), then the publisher's per-pack cap, then the turn's overall limit.
/// Recall keeps the `Cleared` rows; the probe reports every row with its disposition, so what the
/// operator reads as "would reach a turn" is exactly what a turn would have received.
fn judge_pack_candidates<'r>(candidates: Vec<PackCandidate>, routes: &'r [PackRoute], want: usize) -> (Vec<Judged<'r>>, usize) {
    use mind_types::memory::PackDisposition as D;
    let (resolved, ambiguous) = resolve_pack_candidates(candidates, routes);
    let mut taken: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut cleared = 0usize;
    let mut out = Vec::with_capacity(resolved.len());
    for (route, candidate) in resolved {
        let disposition = if !(candidate.similarity >= route.floor) {
            D::WithheldFloor
        } else if route.cap.is_some_and(|cap| taken.get(&route.pack_id).copied().unwrap_or(0) >= cap) {
            D::WithheldPackCap
        } else if cleared >= want {
            D::WithheldLimit
        } else {
            *taken.entry(route.pack_id.clone()).or_insert(0) += 1;
            cleared += 1;
            D::Cleared
        };
        out.push(Judged { route, candidate, disposition });
    }
    (out, ambiguous)
}

/// Attribute each candidate to the ONE route matching its namespace and stamp, ranked by the
/// engine's composite; returns the attributed pairs and the count abstained as ambiguous. The
/// identity half of `floor_pack_hits`, shared with the probe so the operator sees exactly the rows
/// recall would have judged.
fn resolve_pack_candidates<'r>(candidates: Vec<PackCandidate>, routes: &'r [PackRoute]) -> (Vec<(&'r PackRoute, PackCandidate)>, usize) {
    let mut ranked = candidates;
    ranked.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut out = Vec::new();
    let mut ambiguous = 0usize;
    for c in ranked {
        let Some(stamp) = c.pack_name.as_deref() else { continue };
        let named: Vec<&'r PackRoute> =
            routes.iter().filter(|r| r.namespace == c.namespace && r.name == stamp).collect();
        match named.len() {
            1 => out.push((named[0], c)),
            0 => continue,
            _ => ambiguous += 1,
        }
    }
    (out, ambiguous)
}

/// How many candidates to pull per namespace, as a multiple of what the caller wants, and the cap
/// on that. The engine's namespace filter admits every row in the namespace — host rows that share
/// it and every pack sharing it — and they compete for the same slots before the stamp filter can
/// drop them. Overfetching bounds how many such rows it takes to crowd a pack row out; it does NOT
/// make crowding impossible, and this code does not claim it does. The proof arrives with the
/// engine's allowlist recall (`recall_from_packs_for`, core 0.18), which searches only the packs
/// asked for.
const PACK_OVERFETCH: usize = 4;
const PACK_FETCH_MAX: usize = 64;
/// The most rows a probe renders, however large the ask — an operator instrument, not a dump.
const PACK_PROBE_MAX: usize = 48;

/// Pack evidence for a query: one NAMESPACE-SCOPED recall per distinct pack namespace, overfetched,
/// then `floor_pack_hits`.
///
/// Per namespace rather than one mixed recall post-filtered: in a fully mixed pool the household's
/// own rows consume the candidate slots before the floor ever sees a pack row. The namespace filter
/// removes the household's rows from the pool unless they share the pack's namespace; for those
/// and for packs sharing a namespace, `PACK_OVERFETCH` sized by the number of routes in the
/// namespace mitigates crowding (see its doc for what it cannot prove). Reinforcement is skipped —
/// reading a publisher's corpus must not teach the host's learned weights anything about the
/// household.
fn recall_from_mounted_packs(
    db: &YantrikDB,
    manifests: &mut std::collections::HashMap<String, Option<ManifestView>>,
    query: &str,
    top_k: usize,
) -> std::result::Result<Vec<mind_types::memory::PackHit>, String> {
    // Bound the PUBLIC ask before any arithmetic touches it: `clamp(want, MAX)` with want > MAX
    // panics, and a panic here takes the memory actor thread with it (Codex's review of 9aea6a6).
    let want = top_k.clamp(1, PACK_FETCH_MAX);
    let (candidates, routes) = collect_pack_candidates(db, manifests, query, want)?;
    let (hits, ambiguous) = floor_pack_hits(candidates, &routes, want);
    if ambiguous > 0 {
        tracing::warn!(
            ambiguous,
            "pack rows withheld: more than one mounted pack claims their namespace AND name (two versions mounted at once?) — unmount one"
        );
    }
    Ok(hits)
}

/// `ym pack probe`: the same candidates recall judges, every one attributed, with the floor it was
/// measured against and the disposition recall gave it. Ranked by the engine's composite; at most
/// 3× `top_k` rows, bounded by `PACK_PROBE_MAX`, so a withheld pack still shows its best few.
fn probe_mounted_packs(
    db: &YantrikDB,
    manifests: &mut std::collections::HashMap<String, Option<ManifestView>>,
    query: &str,
    top_k: usize,
) -> std::result::Result<Vec<mind_types::memory::PackProbe>, String> {
    let want = top_k.clamp(1, PACK_FETCH_MAX);
    let limit = want.saturating_mul(3).min(PACK_PROBE_MAX);
    let (candidates, routes) = collect_pack_candidates(db, manifests, query, want)?;
    let (judged, _ambiguous) = judge_pack_candidates(candidates, &routes, want);
    Ok(judged
        .into_iter()
        .take(limit)
        .map(|j| mind_types::memory::PackProbe {
            pack_id: j.route.pack_id.clone(),
            rid: j.candidate.rid,
            text: j.candidate.text,
            score: j.candidate.score,
            similarity: j.candidate.similarity,
            floor: j.route.floor,
            disposition: j.disposition,
        })
        .collect())
}

/// The shared front half of recall and probe: routes for every mounted pack with a namespace, and
/// one overfetched, namespace-scoped, reinforcement-free engine recall per distinct namespace.
fn collect_pack_candidates(
    db: &YantrikDB,
    manifests: &mut std::collections::HashMap<String, Option<ManifestView>>,
    query: &str,
    want: usize,
) -> std::result::Result<(Vec<PackCandidate>, Vec<PackRoute>), String> {
    debug_assert!((1..=PACK_FETCH_MAX).contains(&want), "callers bound want before collecting");
    let routes: Vec<PackRoute> = db
        .mounted_packs()
        .into_iter()
        .filter_map(|p| {
            let namespace = p.namespace.clone()?;
            let m = cached_manifest(manifests, &p.path);
            Some(PackRoute {
                pack_id: p.pack_id,
                name: p.name,
                namespace,
                floor: mind_types::memory::effective_pack_floor(m.and_then(|m| m.recommended_min_similarity)),
                cap: m.and_then(|m| m.recommended_top_k).map(|k| k.max(1) as usize),
            })
        })
        .collect();
    if routes.is_empty() {
        return Ok((Vec::new(), routes));
    }
    let embedding = db.embed(query).map_err(|e| e.to_string())?;
    let mut candidates: Vec<PackCandidate> = Vec::new();
    let mut routes_in_ns: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for r in &routes {
        *routes_in_ns.entry(r.namespace.as_str()).or_insert(0) += 1;
    }
    for (namespace, n_routes) in routes_in_ns {
        // `want` is already within [1, PACK_FETCH_MAX], so the bound below can never fall under
        // it; written without `clamp` so no future change to the bound can reintroduce the panic.
        let k = want
            .saturating_mul(PACK_OVERFETCH)
            .saturating_mul(n_routes)
            .min(PACK_FETCH_MAX)
            .max(want);
        let rs = db
            .recall(
                &embedding,
                k,
                None,        // time_window
                None,        // memory_type
                false,       // include_consolidated
                false,       // expand_entities
                Some(query), // query_text (keyword lanes)
                true,        // skip_reinforce — a publisher's corpus teaches the host nothing
                Some(namespace),
                None,        // domain
                None,        // source
                None,        // certainty_min
                None,        // order — relevance
                false,       // include_superseded
            )
            .map_err(|e| e.to_string())?;
        candidates.extend(rs.into_iter().map(PackCandidate::from_engine));
    }
    Ok((candidates, routes))
}

/// Would exporting this text carry a distinctive personal value out of the household?
///
/// A deliberately narrow mirror of `egress_planning::distinctive_pii` (which lives in
/// mind-conversation and cannot be reached from here): emails, contiguous 7–15-digit numbers, and
/// long mixed alphanumeric tokens. High precision over recall — a false positive silently drops a
/// real approach from the export, so only value-shaped tokens qualify.
fn looks_private(text: &str) -> bool {
    for raw in text.split(|c: char| c.is_whitespace() || matches!(c, '"' | ',' | '{' | '}' | '[' | ']' | '(' | ')' | '<' | '>' | ';' | '/' | '\\' | ':' | '=' | '&' | '?' | '|' | '`')) {
        let tok = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.' && c != '-' && c != '_' && c != '+');
        if tok.len() < 7 {
            continue;
        }
        let is_email = tok
            .find('@')
            .map(|at| at > 0 && tok[at + 1..].contains('.') && !tok[at + 1..].ends_with('.'))
            .unwrap_or(false);
        let is_phone_like = tok.chars().all(|c| c.is_ascii_digit()) && (7..=15).contains(&tok.len());
        let digits = tok.chars().filter(|c| c.is_ascii_digit()).count();
        let is_long_id = tok.len() >= 16
            && tok.chars().all(|c| c.is_ascii_alphanumeric())
            && digits > 0
            && tok.chars().any(|c| c.is_ascii_alphabetic());
        if is_email || is_phone_like || is_long_id {
            return true;
        }
    }
    false
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

/// Build a `ContradictionConfig` from `YM_CONTRADICTION_SENSITIVITY` (float in [0.0, 1.0],
/// default 0.5). Higher values lower the confidence/severity thresholds so more conflicts are
/// surfaced; lower values raise them to suppress noisy, low-confidence conflicts.
/// Tradeoff: high sensitivity catches real contradictions earlier but risks false positives in
/// ambiguous domains; low sensitivity is quieter but may miss genuine belief conflicts.
fn contradiction_config_from_env() -> ContradictionConfig {
    let s: f64 = std::env::var("YM_CONTRADICTION_SENSITIVITY")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.5)
        .clamp(0.0, 1.0);
    // At s=0.5 these reproduce the library defaults (min_confidence=0.6, min_severity=0.2).
    ContradictionConfig {
        min_confidence_for_conflict: 0.6 + (0.5 - s) * 0.4,
        min_severity: 0.2 + (0.5 - s) * 0.2,
        ..ContradictionConfig::default()
    }
}

/// Minimum topical overlap required before two beliefs can be surfaced as contradictory.
/// Semantic cosine is normalized so the embedder's ordinary background similarity does not make
/// unrelated subjects appear related; significant-word overlap provides the no-embedder fallback.
fn contradiction_relatedness_threshold() -> f64 {
    let value = std::env::var("YM_CONTRADICTION_RELATEDNESS_THRESHOLD").ok();
    parse_relatedness_threshold(value.as_deref())
}

fn parse_relatedness_threshold(value: Option<&str>) -> f64 {
    value
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.25)
        // This is a correctness gate, not just a sensitivity knob: allowing zero would make every
        // pair related and could surface unrelated beliefs as an open contradiction.
        .clamp(0.25, 1.0)
}

fn topical_relatedness(a: &str, b: &str, semantic_cosine: Option<f64>) -> f64 {
    let a_words = task_word_set(a);
    let b_words = task_word_set(b);
    let word_overlap = jaccard(&a_words, &b_words);
    let leading_subject_matches = |text: &str, words: &std::collections::HashSet<String>| {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .find(|word| words.contains(*word))
            .map(str::to_string)
    };
    let subject_overlap = match (
        leading_subject_matches(a, &a_words),
        leading_subject_matches(b, &b_words),
    ) {
        (Some(a_subject), Some(b_subject)) if a_subject == b_subject => 1.0,
        _ => 0.0,
    };
    // Cosines around 0.5 are common even for unrelated natural-language sentences. Map the useful
    // 0.5..1.0 range onto 0..1 so only meaningful semantic similarity contributes to this gate.
    let semantic = semantic_cosine
        .map(|s| ((s - 0.5) * 2.0).clamp(0.0, 1.0))
        .unwrap_or(0.0);
    // KNOWN DEFECT, deliberately left in place — see `the_real_world_false_contradictions_are_ignored`.
    // A shared leading word saturates this to 1.0, which opens the gate for every pair of beliefs
    // about the same person. Attempting to score the PREDICATE instead fixed the false positives
    // and broke the true ones ("Pranab sleeps early" vs "Pranab stays up late" contradict while
    // sharing no predicate words), so the correct fix needs the semantic signal to carry
    // antonym-shaped conflicts, and probably belongs in the detector rather than this filter.
    word_overlap.max(subject_overlap).max(semantic)
}

fn beliefs_are_topically_related(db: &YantrikDB, a: &str, b: &str, threshold: f64) -> bool {
    let semantic = if db.has_embedder() {
        db.embed(a)
            .ok()
            .zip(db.embed(b).ok())
            .map(|(a_vec, b_vec)| cosine(&a_vec, &b_vec))
    } else {
        None
    };
    topical_relatedness(a, b, semantic) >= threshold
}

fn detect_conflicts(db: &YantrikDB) -> Vec<Contradiction> {
    let res = match db.detect_belief_contradictions(&contradiction_config_from_env()) {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let id_to_prop: HashMap<NodeId, String> = all_beliefs(db)
        .iter()
        .filter_map(|n| node_prop(n).map(|p| (n.id, p.to_string())))
        .collect();
    let relatedness_threshold = contradiction_relatedness_threshold();
    res.conflicts
        .iter()
        .filter_map(|c| {
            let belief_a = id_to_prop.get(&c.belief_a)?;
            let belief_b = id_to_prop.get(&c.belief_b)?;
            beliefs_are_topically_related(db, belief_a, belief_b, relatedness_threshold).then(
                || Contradiction {
                    id: format!("{}~{}", c.belief_a, c.belief_b),
                    belief_a: belief_a.clone(),
                    belief_b: belief_b.clone(),
                    severity: c.severity,
                    status: "open".into(),
                },
            )
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

/// A knowledge pack's LOCAL track record (ARCH-6 P.2): the SQL witness beside the flight
/// recorder's events. Counts only — every rate needs its denominator said aloud at render time.
fn ensure_pack_stats_table(db: &YantrikDB) {
    let _ = db.conn().execute(
        "CREATE TABLE IF NOT EXISTS mind_pack_stats \
         (pack_id TEXT PRIMARY KEY, content_digest TEXT, \
          surfaced INTEGER NOT NULL DEFAULT 0, used INTEGER NOT NULL DEFAULT 0, \
          graded INTEGER NOT NULL DEFAULT 0, good INTEGER NOT NULL DEFAULT 0, \
          first_ms INTEGER NOT NULL, last_ms INTEGER NOT NULL)",
        [],
    );
}

fn now_ms_i64() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Count one rung for a pack. The row is keyed by pack id but OWNED by the content digest: a pack
/// re-sealed under the same id (same version, new rows) must not inherit the old rows' record, so
/// a digest change resets the counters before counting. A pack whose digest is unknown (manifest
/// unreadable, or not mounted right now) is counted under its id without a reset — better a
/// record that says "digest unknown" than a rung lost.
fn record_pack_event(
    db: &YantrikDB,
    manifests: &mut std::collections::HashMap<String, Option<ManifestView>>,
    pack_id: &str,
    event: mind_types::memory::PackEvent,
) -> std::result::Result<(), String> {
    use mind_types::memory::PackEvent;
    use rusqlite::OptionalExtension;
    let now = now_ms_i64();
    let digest: Option<String> = db
        .mounted_packs()
        .into_iter()
        .find(|p| p.pack_id == pack_id)
        .and_then(|p| cached_manifest(manifests, &p.path).and_then(|m| m.content_digest.clone()));
    let conn = db.conn();
    let existing: Option<Option<String>> = conn
        .query_row("SELECT content_digest FROM mind_pack_stats WHERE pack_id = ?1", [pack_id], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()
        .map_err(|e| e.to_string())?;
    match existing {
        None => {
            conn.execute(
                "INSERT INTO mind_pack_stats (pack_id, content_digest, surfaced, used, graded, good, first_ms, last_ms) \
                 VALUES (?1, ?2, 0, 0, 0, 0, ?3, ?3)",
                rusqlite::params![pack_id, digest, now],
            )
            .map_err(|e| e.to_string())?;
        }
        Some(old) if digest.is_some() && old != digest => {
            conn.execute(
                "UPDATE mind_pack_stats SET content_digest = ?2, surfaced = 0, used = 0, graded = 0, good = 0, \
                 first_ms = ?3, last_ms = ?3 WHERE pack_id = ?1",
                rusqlite::params![pack_id, digest, now],
            )
            .map_err(|e| e.to_string())?;
        }
        _ => {}
    }
    let (column, good) = match event {
        PackEvent::Surfaced => ("surfaced", false),
        PackEvent::Used => ("used", false),
        PackEvent::Graded { good } => ("graded", good),
    };
    conn.execute(
        &format!("UPDATE mind_pack_stats SET {column} = {column} + 1, last_ms = ?2 WHERE pack_id = ?1"),
        rusqlite::params![pack_id, now],
    )
    .map_err(|e| e.to_string())?;
    if good {
        conn.execute("UPDATE mind_pack_stats SET good = good + 1 WHERE pack_id = ?1", [pack_id])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn pack_stats(db: &YantrikDB) -> std::result::Result<Vec<mind_types::memory::PackStats>, String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare(
            "SELECT pack_id, content_digest, surfaced, used, graded, good, first_ms, last_ms \
             FROM mind_pack_stats ORDER BY surfaced DESC, pack_id ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(mind_types::memory::PackStats {
                pack_id: r.get(0)?,
                content_digest: r.get(1)?,
                surfaced: r.get::<_, i64>(2)?.max(0) as u64,
                used: r.get::<_, i64>(3)?.max(0) as u64,
                graded: r.get::<_, i64>(4)?.max(0) as u64,
                good: r.get::<_, i64>(5)?.max(0) as u64,
                first_ms: r.get(6)?,
                last_ms: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
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

/// Purpose Gate v1 storage: an explicit per-belief sensitivity override (keyed by canonical
/// proposition, like `mind_belief_scope`) and the standing purpose-grant ledger. A belief with
/// no sensitivity row is classified deterministically at read time (`Sensitivity::classify`);
/// an explicit row wins in either direction — it is the correction path.
fn ensure_purpose_tables(db: &YantrikDB) {
    let _ = db.conn().execute(
        "CREATE TABLE IF NOT EXISTS mind_belief_sensitivity (proposition TEXT PRIMARY KEY, class TEXT NOT NULL)",
        [],
    );
    // Grants are never deleted — revocation flips a flag, so the audit story survives.
    let _ = db.conn().execute(
        "CREATE TABLE IF NOT EXISTS mind_purpose_grants (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            owner TEXT NOT NULL,
            beneficiary TEXT NOT NULL,
            class TEXT NOT NULL,
            activity TEXT NOT NULL,
            expires_ms INTEGER NOT NULL,
            note TEXT NOT NULL,
            revoked INTEGER NOT NULL DEFAULT 0,
            created_ms INTEGER NOT NULL
        )",
        [],
    );
}

fn set_belief_sensitivity(db: &YantrikDB, proposition: &str, class: &str) -> std::result::Result<(), String> {
    db.conn()
        .execute(
            "INSERT INTO mind_belief_sensitivity (proposition, class) VALUES (?1, ?2) \
             ON CONFLICT(proposition) DO UPDATE SET class=excluded.class",
            [proposition, class],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn belief_sensitivity_map(db: &YantrikDB) -> std::result::Result<std::collections::HashMap<String, String>, String> {
    let conn = db.conn();
    let mut stmt = conn.prepare("SELECT proposition, class FROM mind_belief_sensitivity").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn grant_purpose(db: &YantrikDB, spec: &mind_types::PurposeGrantSpec) -> std::result::Result<i64, String> {
    let now_ms = (now_secs() * 1000.0) as i64;
    db.conn()
        .execute(
            "INSERT INTO mind_purpose_grants (owner, beneficiary, class, activity, expires_ms, note, revoked, created_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            rusqlite::params![
                spec.owner.as_tag(),
                spec.beneficiary.as_tag(),
                spec.class.map(|c| c.as_tag().to_string()).unwrap_or_else(|| "*".into()),
                spec.activity.map(|a| a.as_tag().to_string()).unwrap_or_else(|| "*".into()),
                spec.expires_ms as i64,
                spec.note,
                now_ms,
            ],
        )
        .map_err(|e| e.to_string())?;
    Ok(db.conn().last_insert_rowid())
}

fn revoke_purpose_grant(db: &YantrikDB, id: i64) -> std::result::Result<bool, String> {
    db.conn()
        .execute("UPDATE mind_purpose_grants SET revoked = 1 WHERE id = ?1 AND revoked = 0", [id])
        .map(|n| n > 0)
        .map_err(|e| e.to_string())
}

fn list_purpose_grants(db: &YantrikDB) -> std::result::Result<Vec<mind_types::PurposeGrant>, String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT id, owner, beneficiary, class, activity, expires_ms, note, revoked, created_ms FROM mind_purpose_grants ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(mind_types::PurposeGrant {
                id: r.get::<_, i64>(0)?,
                owner: mind_types::Subject::parse(&r.get::<_, String>(1)?),
                beneficiary: mind_types::Subject::parse(&r.get::<_, String>(2)?),
                class: match r.get::<_, String>(3)?.as_str() {
                    "*" => None,
                    c => Some(mind_types::Sensitivity::parse(c)),
                },
                activity: match r.get::<_, String>(4)?.as_str() {
                    "*" => None,
                    a => mind_types::Activity::parse(a),
                },
                expires_ms: r.get::<_, i64>(5)? as u64,
                note: r.get::<_, String>(6)?,
                revoked: r.get::<_, i64>(7)? != 0,
                created_ms: r.get::<_, i64>(8)? as u64,
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// Belief-lifecycle storage: the tombstone ledger. One row per forgotten
/// proposition, carrying WHY — readable after the fact, unlike the row it
/// marks, so "user-deleted" stays forever distinguishable from dedup/hygiene.
fn ensure_tombstone_table(db: &YantrikDB) {
    let _ = db.conn().execute(
        "CREATE TABLE IF NOT EXISTS mind_belief_tombstone (proposition TEXT PRIMARY KEY, reason TEXT NOT NULL, ts_ms INTEGER NOT NULL)",
        [],
    );
}

fn record_tombstone(db: &YantrikDB, proposition: &str, reason: &str) -> std::result::Result<(), String> {
    let ts = (now_secs() * 1000.0) as i64;
    db.conn()
        .execute(
            "INSERT INTO mind_belief_tombstone (proposition, reason, ts_ms) VALUES (?1, ?2, ?3) \
             ON CONFLICT(proposition) DO UPDATE SET reason=excluded.reason, ts_ms=excluded.ts_ms",
            rusqlite::params![proposition, reason, ts],
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn list_tombstones(db: &YantrikDB) -> std::result::Result<Vec<(String, String, u64)>, String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT proposition, reason, ts_ms FROM mind_belief_tombstone ORDER BY ts_ms DESC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? as u64)))
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

/// The per-read purpose lens (Purpose Gate v1): resolves each item's data OWNER
/// (from its scope tag — a fact that entered through X's private channel is X's)
/// and SENSITIVITY (explicit override row, else the deterministic classifier),
/// then asks the pure policy whether the declared purpose may hydrate it.
/// Grants can open the operator's background lanes; they never widen a
/// principal's viewing scope — the scope wall runs first and stays supreme.
struct PurposeLens {
    purpose: mind_types::Purpose,
    scopes: std::collections::HashMap<String, String>,
    sensitivity: std::collections::HashMap<String, String>,
    grants: Vec<mind_types::PurposeGrant>,
    now_ms: u64,
}

impl PurposeLens {
    fn allows(&self, proposition: &str) -> bool {
        let owner = mind_types::Subject::owner_of_scope_tag(self.scopes.get(proposition).map(|s| s.as_str()));
        let sens = self
            .sensitivity
            .get(proposition)
            .map(|s| mind_types::Sensitivity::parse(s))
            .unwrap_or_else(|| mind_types::Sensitivity::classify(proposition));
        let granted = self.grants.iter().any(|g| g.covers(&self.purpose, &owner, sens, self.now_ms));
        mind_types::purpose_allows(&self.purpose, &owner, sens, granted)
    }
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

/// Retro-deduplication for the goals/prefs table: applies the same norm_prop + Jaccard logic as
/// the write path to EXISTING rows, removing any near-duplicates that were written before PR #19
/// added write-path dedup. Returns the count of rows deleted.
fn retro_dedup_goals_prefs(db: &YantrikDB) -> usize {
    let mut removed = 0usize;
    for kind in ["goal", "preference"] {
        let items = list_goal_prefs(db, kind).unwrap_or_default();
        // Walk items in insertion order (id ASC — the same order stored by list_goal_prefs).
        // Keep the first occurrence of each canonical / Jaccard-similar group; delete the rest.
        let mut survivors: Vec<MemoryItem> = Vec::new();
        for item in items {
            let canon = norm_prop(&item.text);
            let sig = task_word_set(&item.text);
            let is_dup = survivors.iter().any(|s| norm_prop(&s.text) == canon)
                || (sig.len() >= 2
                    && survivors.iter().any(|s| jaccard(&task_word_set(&s.text), &sig) >= 0.6));
            if is_dup {
                if let Ok(id) = item.id.parse::<i64>() {
                    let _ = db.conn().execute("DELETE FROM mind_goals_prefs WHERE id = ?1", [id]);
                    removed += 1;
                }
            } else {
                survivors.push(item);
            }
        }
    }
    removed
}

/// Retro-deduplication for beliefs: tombstones any CognitiveNode whose norm_prop(proposition)
/// collides with an earlier node, first folding the duplicate's accumulated log_odds into the
/// survivor as a single synthetic evidence event so no information is lost. Word-overlap dedup is
/// intentionally NOT applied — "Rust is 1.70" vs "Rust is 1.96" differ only in significant tokens
/// and must remain distinct contradicting nodes. Returns the count of nodes tombstoned.
fn retro_dedup_beliefs(db: &YantrikDB) -> usize {
    let beliefs = all_beliefs(db);
    let mut seen: HashMap<String, NodeId> = HashMap::new();
    let mut merged = 0usize;
    for node in &beliefs {
        let Some(prop) = node_prop(node) else { continue };
        let canon = norm_prop(prop);
        if let Some(&survivor_id) = seen.get(&canon) {
            if let NodePayload::Belief(b) = &node.payload {
                if b.log_odds != 0.0 {
                    let ev = YEvidence {
                        target_belief: survivor_id,
                        weight: b.log_odds,
                        source: "retro-dedup".to_string(),
                        provenance: prov("system"),
                        propagate: false,
                        timestamp: now_secs(),
                    };
                    let _ = db.assert_belief_evidence(&ev, &BeliefRevisionConfig::default());
                }
            }
            let _ = db.tombstone_cognitive_node(node.id);
            merged += 1;
        } else {
            seen.insert(canon, node.id);
        }
    }
    merged
}

/// Run retro-dedup over both the belief graph and the goals/prefs table. Safe to call on any live
/// DB — idempotent; a second pass on an already-clean store is always a no-op. Returns
/// `(beliefs_tombstoned, goals_prefs_deleted)`.
fn retro_dedup_store(db: &YantrikDB) -> (usize, usize) {
    (retro_dedup_beliefs(db), retro_dedup_goals_prefs(db))
}

fn ensure_tensions_table(db: &YantrikDB) {
    let _ = db.conn().execute(
        "CREATE TABLE IF NOT EXISTS mind_tensions \
         (id INTEGER PRIMARY KEY AUTOINCREMENT, kind TEXT NOT NULL, pressure REAL NOT NULL, \
          about TEXT NOT NULL, created_ms INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'open')",
        [],
    );
}

/// The identity of a contradiction, independent of how it was spelled.
///
/// Deduping on the raw `about` string looked right and was not: ONE contradiction reaches this
/// function under several different strings, so every variant inserted its own row. Measured live —
/// a single dead fact ("Pranab owns a Rosefield watch intended as a gift") held **54 rows covering
/// 12 real pairs**, and across the whole table 55% of conflict rows were duplicates.
///
/// Three things varied while the meaning did not:
///   * **Two writers, two formats.** The assert-belief path emits `conflict: A vs B` (32 of the 54)
///     and the DMN reconciliation path emits `"A" vs "B"` (the other 22). Neither knew about the
///     other, so an exact-string match could never collapse them.
///   * **Both directions.** `A vs B` and `B vs A` are the same contradiction; 24 of 24 ordered
///     variants had their mirror stored as a separate row.
///   * **Punctuation.** `pranab.co.in` and `pranab.co.in.` are the same claim to a reader and two
///     different strings to SQLite.
///
/// So the key strips the format prefix and quotes, lowercases, drops non-alphanumerics, and SORTS
/// the two sides — making it a property of the pair rather than of the sentence that carried it.
/// Non-conflict tensions (urges, which have no `vs`) fall through to their normalised text, which
/// preserves the original accrue-don't-flood behaviour for them.
pub fn tension_key(about: &str) -> String {
    let s = about.strip_prefix("conflict:").unwrap_or(about).trim();
    let norm = |x: &str| -> String {
        x.trim()
            .trim_matches('"')
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    match s.split_once(" vs ") {
        Some((a, b)) => {
            let (a, b) = (norm(a), norm(b));
            // Sorted, so direction cannot create a second row.
            if a <= b { format!("{a} vs {b}") } else { format!("{b} vs {a}") }
        }
        None => norm(s),
    }
}

/// Record a tension, deduped on (kind, tension_key(about)) among OPEN rows so a recurring urge
/// accrues (keeps the max pressure + refreshes created_ms) rather than flooding the ledger.
fn record_tension_db(db: &YantrikDB, kind: &str, pressure: f64, about: &str, now_ms: i64) -> std::result::Result<(), String> {
    let conn = db.conn();
    let key = tension_key(about);
    // Compare on the KEY, not the stored string. Scanning open rows of this kind and normalising in
    // Rust keeps the matching logic in exactly one place — a SQL expression would have to reproduce
    // it and the two would drift.
    let existing: Option<(i64, f64)> = conn
        .prepare("SELECT id, pressure, about FROM mind_tensions WHERE kind=?1 AND status='open'")
        .and_then(|mut st| {
            let rows = st.query_map(rusqlite::params![kind], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, String>(2)?))
            })?;
            Ok(rows.flatten().find(|(_, _, a)| tension_key(a) == key).map(|(i, p, _)| (i, p)))
        })
        .unwrap_or(None);
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

/// Half-life (days) of a tension's URGENCY. An urge nobody acted on for a week is genuinely less
/// pressing than a fresh one of equal nominal pressure — and, critically, decay is what stops a
/// fixed-pressure class from owning the digest forever (see `open_tensions_db`).
pub const TENSION_HALF_LIFE_DAYS: f64 = 7.0;

/// Effective urgency = nominal pressure decayed by age. Pure so the ranking is testable.
pub fn effective_pressure(pressure: f64, age_ms: i64) -> f64 {
    let days = (age_ms.max(0) as f64) / 86_400_000.0;
    pressure * 0.5f64.powf(days / TENSION_HALF_LIFE_DAYS)
}

/// The open tensions the proactive drive may surface, ranked by AGE-DECAYED pressure.
///
/// The previous ordering (`pressure DESC, created_ms DESC`) starved the drive dead. Measured on the
/// live box 2026-07-25: 2,602 open tensions, 17 discharged EVER (0.6%), and all 12 digest slots
/// permanently held by `operational` self-build alarms at a fixed 0.85 — so 2,257 curiosity urges
/// (0.4) and 329 contradictions were structurally unreachable, some for a month. With a fixed
/// pressure per class, the highest class wins forever and the newest-first tiebreak means an item
/// that loses once can never win later: not a backlog, a graveyard.
///
/// Decaying by age makes the ranking a genuine competition again — a stale 0.85 alarm falls below a
/// fresh 0.4 curiosity after ~two half-lives — and pairs with `expire_stale_tensions_db` to keep the
/// live set bounded. Candidates are drawn by recency, then re-ranked, so the scan stays cheap.
fn open_tensions_db(db: &YantrikDB, limit: usize) -> std::result::Result<Vec<mind_types::Tension>, String> {
    const CANDIDATES: i64 = 500;
    let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT id, kind, pressure, about, created_ms FROM mind_tensions WHERE status='open' ORDER BY created_ms DESC LIMIT ?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([CANDIDATES], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut all: Vec<(f64, mind_types::Tension)> = rows
        .filter_map(|r| r.ok())
        .map(|(id, kind, pressure, about, created_ms)| {
            let eff = effective_pressure(pressure, now_ms - created_ms);
            (
                eff,
                mind_types::Tension {
                    id: id.to_string(),
                    kind: mind_types::TensionKind::parse(&kind),
                    pressure,
                    about,
                    created_ms: created_ms as u64,
                    status: "open".into(),
                },
            )
        })
        .collect();
    all.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    Ok(all.into_iter().take(limit).map(|(_, t)| t).collect())
}

/// Bound the tension ledger: an OPEN urge older than its kind's shelf life is closed as `expired`.
///
/// Without this the table only grows (measured: ~90 new curiosity urges/day, 4 weeks, 0.6% ever
/// discharged). Contradictions get a much longer life than curiosity because unresolved contradictory
/// beliefs are real epistemic debt the mind should keep chewing on; a hunch nobody pursued in two
/// weeks is noise. Expiry is a distinct status from `discharged` so "we surfaced it" and "it aged
/// out unseen" stay distinguishable in the record.
fn expire_stale_tensions_db(
    db: &YantrikDB,
    now_ms: i64,
    curiosity_days: i64,
    other_days: i64,
) -> std::result::Result<usize, String> {
    let cur_cut = now_ms - curiosity_days * 86_400_000;
    let oth_cut = now_ms - other_days * 86_400_000;
    let n = db
        .conn()
        .execute(
            "UPDATE mind_tensions SET status='expired' WHERE status='open' AND \
             ((kind='curiosity' AND created_ms < ?1) OR (kind<>'curiosity' AND created_ms < ?2))",
            [cur_cut, oth_cut],
        )
        .map_err(|e| e.to_string())?;
    Ok(n)
}

fn discharge_tension_db(db: &YantrikDB, id: &str) -> std::result::Result<bool, String> {
    let n = db
        .conn()
        .execute("UPDATE mind_tensions SET status='discharged' WHERE id=?1 AND status='open'", [id])
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

/// Engine demand for a topic: aggregate confidence-deficit of beliefs whose text overlaps with the
/// `about` string. Uses the same word-match logic as BeliefsMatching (words ≥4 chars, case-insensitive,
/// matched at word-start). Result is normalised to [0,1] via sum/(1+sum) so it saturates smoothly.
fn recall_demand_for_db(db: &YantrikDB, about: &str) -> f64 {
    let words: Vec<String> = about
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .map(|w| w.to_lowercase())
        .collect();
    if words.is_empty() {
        return 0.0;
    }
    let uncertainty_sum: f64 = all_beliefs(db)
        .iter()
        .filter_map(|n| {
            let stmt = node_prop(n)?.to_lowercase();
            let toks: Vec<&str> = stmt.split(|c: char| !c.is_alphanumeric()).collect();
            if words.iter().any(|w| toks.iter().any(|x| x.starts_with(w.as_str()))) {
                Some(1.0_f64 - n.attrs.confidence.clamp(0.0, 1.0))
            } else {
                None
            }
        })
        .sum();
    uncertainty_sum / (1.0 + uncertainty_sum)
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
    // A CONTEXT BREAK ends the conversational window: everything before the newest break row is
    // invisible to prompt assembly and to the restored chat pane, while memory and consolidation
    // (which read by id, not through here) keep the full record. The scan is newest-first, so
    // truncate at the FIRST break met and drop the marker itself — it is punctuation, not content.
    if let Some(pos) = v.iter().position(|(role, _)| role == "break") {
        v.truncate(pos);
    }
    v.reverse(); // newest-first SQL -> chronological for the prompt
    Ok(v)
}

fn user_turn_times(db: &YantrikDB, since_ms: i64) -> std::result::Result<Vec<i64>, String> {
    let conn = db.conn();
    let mut stmt = conn
        .prepare("SELECT CAST(ts * 1000 AS INTEGER) FROM mind_transcript WHERE role = 'user' AND ts * 1000 >= ?1 ORDER BY ts ASC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params![since_ms], |r| r.get::<_, i64>(0))
        .map_err(|e| e.to_string())?;
    Ok(rows.filter_map(|r| r.ok()).collect())
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

/// Backlog gauge for the single command queue: current depth (queued + running) and the
/// high-water mark since spawn. The hwm is the tripwire that says "something outran the actor"
/// without needing to catch it in the act — the signal that would justify splitting a heavy
/// command off-thread per the scheduling doctrine at the top of this file.
#[derive(Default)]
struct BacklogGauge {
    depth: std::sync::atomic::AtomicUsize,
    high_water: std::sync::atomic::AtomicUsize,
}

impl BacklogGauge {
    fn on_send(&self) {
        let d = self.depth.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.high_water.fetch_max(d, std::sync::atomic::Ordering::SeqCst);
    }
    fn on_done(&self) {
        self.depth.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn snapshot(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::SeqCst;
        (self.depth.load(SeqCst), self.high_water.load(SeqCst))
    }
}

/// Public snapshot of the actor's queue state, for operator surfaces and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BacklogDepth {
    pub queued_or_running: usize,
    /// Worst backlog since spawn — the number that says "consolidation outran the actor".
    pub high_water: usize,
}

#[derive(Clone)]
pub struct MemoryHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    /// Where the store lives (":memory:" for scratch minds) — surfaced so wiring like the
    /// flight recorder can sit beside the same DB without re-reading env.
    db_path: String,
    gauge: std::sync::Arc<BacklogGauge>,
    /// ARCH-1 slice 2: every principal read is receipted into a hash-chained ledger.
    receipts: std::sync::Arc<receipts::ReadReceiptLedger>,
    /// Authorization state recorded at spawn time; checked by restricted operations.
    device_auth: DeviceAuthorization,
}

impl MemoryHandle {
    pub fn spawn(db_path: &str, dim: usize) -> Result<Self> {
        Self::spawn_for_device(db_path, dim, DeviceAuthorization::Authorized)
    }

    /// Open memory on behalf of a device-authenticated caller.
    pub fn spawn_for_device(
        db_path: &str,
        dim: usize,
        device_authorization: DeviceAuthorization,
    ) -> Result<Self> {
        if device_authorization == DeviceAuthorization::Unauthorized {
            return Err(AuthError::DeviceNotAuthorized.into());
        }

        let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
        let gauge = std::sync::Arc::new(BacklogGauge::default());
        // The actor thread keeps its own gauge handle; the outer one goes on the returned Self.
        let thread_gauge = std::sync::Arc::clone(&gauge);
        let path = db_path.to_string();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();

        std::thread::Builder::new()
            .name("mind-memory".into())
            .spawn(move || {
                let gauge = thread_gauge;
                let db = match YantrikDB::new(&path, dim) {
                    Ok(d) => { let _ = ready_tx.send(Ok(())); d }
                    Err(e) => { let _ = ready_tx.send(Err(e.to_string())); return; }
                };
                // yantrikdb 0.12: records longer than the embedder's input window used to be
                // embedded head-only — a long briefing was findable by its opening lines and
                // invisible from everything after (silent retrieval loss). The backfill chunks
                // pre-0.12 records; it is idempotent and skips already-chunked rows, so running it
                // at every boot costs one scan and converges to a no-op.
                match db.rechunk_long_records() {
                    Ok((0, _)) => {}
                    Ok((n, v)) => eprintln!("[memory] chunk backfill: {n} long record(s) → {v} chunk vector(s) now findable past their head"),
                    Err(e) => eprintln!("[memory] chunk backfill skipped: {e}"),
                }
                ensure_transcript_table(&db);
                ensure_skills_table(&db);
                ensure_pack_stats_table(&db);
                ensure_goals_prefs_table(&db);
                ensure_tensions_table(&db);
                ensure_belief_scope_table(&db);
                ensure_belief_evidence_version_table(&db);
                ensure_purpose_tables(&db);
                ensure_tombstone_table(&db);
                let mut alloc = db.load_node_id_allocator().unwrap_or_else(|_| NodeIdAllocator::new());
                let zero = vec![0.0f32; dim];
                let meta = serde_json::json!({});
                // Mounted-pack manifests, read from the pack files and cached by path. The engine's
                // `PackInfo` does not yet carry the signed retrieval settings or the digest
                // (substrate ask ARCH-6 §C.2.1, queued for core 0.18); until it does, the manifest is
                // the only place the floor lives. Cleared on every mount/unmount so a replaced file
                // is re-read rather than served stale.
                let mut pack_manifests: std::collections::HashMap<String, Option<ManifestView>> =
                    std::collections::HashMap::new();
                // THE PUMP: one FIFO, drained in arrival order. Causally transparent by
                // construction (see the scheduling doctrine at the top of this file for why
                // this stayed a single queue, and which escape hatch exists for heavy commands).
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
                        Cmd::BeliefsMatching { needle, limit, reply } => {
                            // Classify each needle token: SHORT ALL-CAPS ACRONYMS (SDF, ML, API — the
                            // exact shape of work subjects) match WHOLE-WORD to avoid noise ("AI" inside
                            // "domain"); ordinary words (len>=4) keep substring match ("adopt"->"adoption").
                            // The old flat len>=4 gate silently dropped 3-char acronyms = the SDF bug.
                            let words: Vec<(String, bool)> = needle
                                .split(|c: char| !c.is_alphanumeric())
                                .filter_map(|w| {
                                    let acronym = (2..=3).contains(&w.len())
                                        && w.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                                        && w.chars().any(|c| c.is_ascii_uppercase());
                                    if acronym {
                                        Some((w.to_lowercase(), true))
                                    } else if w.len() >= 4 {
                                        Some((w.to_lowercase(), false))
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                            let hits: Vec<Belief> = if words.is_empty() {
                                Vec::new()
                            } else {
                                all_beliefs(&db)
                                    .iter()
                                    .map(to_belief_dto)
                                    .filter(|b| {
                                        let t = b.statement.to_lowercase();
                                        let toks: Vec<&str> =
                                            t.split(|c: char| !c.is_alphanumeric()).collect();
                                        words.iter().any(|(w, whole)| {
                                            if *whole || w.len() <= 4 {
                                                // short words whole-word: "rath" must not hit "RATHer"
                                                toks.iter().any(|x| *x == w.as_str())
                                            } else {
                                                // longer words match at word START: "adopt"->"adoption",
                                                // but never mid-word accidents.
                                                toks.iter().any(|x| x.starts_with(w.as_str()))
                                            }
                                        })
                                    })
                                    .take(limit.max(1))
                                    .collect()
                            };
                            let _ = reply.send(Ok(hits));
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
                        Cmd::Forget { statement, reason, reply } => {
                            let r = match find_belief(&db, &statement) {
                                Some(n) => {
                                    let r = db.tombstone_cognitive_node(n.id).map_err(|e| e.to_string());
                                    if matches!(r, Ok(true)) {
                                        // The reason survives the row it marks — that is the point.
                                        let _ = record_tombstone(&db, &statement, reason.as_deref().unwrap_or("unspecified"));
                                    }
                                    r
                                }
                                None => Ok(false),
                            };
                            let _ = reply.send(r);
                        }
                        Cmd::Tombstones { reply } => {
                            let _ = reply.send(list_tombstones(&db));
                        }
                        Cmd::Export { reply } => {
                            let beliefs: Vec<Belief> = all_beliefs(&db).iter().map(to_belief_dto).collect();
                            let _ = reply.send(serde_json::to_string(&beliefs).map_err(|e| e.to_string()));
                        }
                        Cmd::SnapshotTo { dest, reply } => {
                            // Self-contained BY CONSTRUCTION: snapshot_db_to opens its OWN
                            // read-only connection and touches no actor state, so running it
                            // here would stall every lane behind a whole-file VACUUM (measured:
                            // a live read waited 65ms of a 70ms copy). Off-thread it goes; the
                            // actor keeps serving commands while the copy runs.
                            type SnapshotReply = tokio::sync::oneshot::Sender<std::result::Result<(), String>>;
                            let reply_cell: std::sync::Arc<std::sync::Mutex<Option<SnapshotReply>>> =
                                std::sync::Arc::new(std::sync::Mutex::new(Some(reply)));
                            let live = path.clone();
                            let dest2 = dest.clone();
                            let cell = std::sync::Arc::clone(&reply_cell);
                            let spawned = std::thread::Builder::new()
                                .name("mind-memory-snapshot".into())
                                .spawn(move || {
                                    if let Some(r) = cell.lock().unwrap().take() {
                                        let _ = r.send(snapshot_db_to(&live, &dest2));
                                    }
                                });
                            if spawned.is_err() {
                                // Thread spawn itself failed (process pathology): run inline
                                // rather than dropping the caller's reply.
                                if let Some(r) = reply_cell.lock().unwrap().take() {
                                    let _ = r.send(snapshot_db_to(&path, &dest));
                                }
                            }
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
                        Cmd::MountPack { path, reply } => {
                            // The engine REFUSES a pack built against a different embedder
                            // (PackEmbedderMismatch), because the query is encoded once and searched
                            // against both indexes — mounting across embedding spaces returns
                            // plausible-looking, meaningless results. That refusal is surfaced, never
                            // forced: `allow_unverified_embedder` is deliberately not exposed here.
                            pack_manifests.clear();
                            let _ = reply.send(
                                db.mount_pack(&path).map_err(|e| e.to_string()),
                            );
                        }
                        Cmd::InstallPack { path, reply } => {
                            pack_manifests.clear();
                            let _ = reply.send(
                                db.install_pack(&path).map_err(|e| e.to_string()),
                            );
                        }
                        Cmd::UnmountPack { id, reply } => {
                            pack_manifests.clear();
                            let _ = reply.send(
                                db.unmount_pack(&id).map(|_| ()).map_err(|e| e.to_string()),
                            );
                        }
                        Cmd::ListApproaches { limit, reply } => {
                            let _ = reply.send(list_approaches(&db, limit));
                        }
                        Cmd::UninstallPack { id, reply } => {
                            // The engine removes the durably-installed file AND unmounts; a plain
                            // unmount is process-local and the pack silently returns on restart —
                            // the A/B-contamination bug this verb exists to end.
                            pack_manifests.clear();
                            let _ = reply.send(db.uninstall_pack(&id).map_err(|e| e.to_string()));
                        }
                        Cmd::SealCraftPack { dest, name, version, texts, reply } => {
                            let _ = reply.send(seal_craft_pack(&db, &dest, &name, &version, &texts));
                        }
                        Cmd::MountedPacks { reply } => {
                            let pack_dir = db.pack_dir().map(|d| d.to_string_lossy().to_string());
                            let packs = db
                                .mounted_packs()
                                .into_iter()
                                .map(|p| {
                                    let m = cached_manifest(&mut pack_manifests, &p.path);
                                    mind_types::memory::PackBrief {
                                        id: p.pack_id,
                                        name: p.name,
                                        version: p.version,
                                        origin: p.origin,
                                        trust: format!("{:?}", p.trust),
                                        rows: p.rows as u64,
                                        namespace: p.namespace.clone(),
                                        // A file living in the engine's install dir comes back on
                                        // every open; anything else is this process's transient mount.
                                        installed: pack_dir.as_deref().map(|d| p.path.starts_with(d)).unwrap_or(false),
                                        content_digest: m.and_then(|m| m.content_digest.clone()),
                                        coverage: m.map(|m| m.coverage.clone()).unwrap_or_default(),
                                        recommended_top_k: m.and_then(|m| m.recommended_top_k),
                                        recommended_min_similarity: m.and_then(|m| m.recommended_min_similarity),
                                        signer: m.and_then(|m| m.publisher_pubkey.clone()),
                                    }
                                })
                                .collect();
                            let _ = reply.send(Ok(packs));
                        }
                        Cmd::PackContext { reply } => {
                            let _ = reply.send(Ok(db.pack_context()));
                        }
                        Cmd::RecallFromPacks { query, top_k, reply } => {
                            let _ = reply.send(recall_from_mounted_packs(&db, &mut pack_manifests, &query, top_k));
                        }
                        Cmd::ProbePacks { query, top_k, reply } => {
                            let _ = reply.send(probe_mounted_packs(&db, &mut pack_manifests, &query, top_k));
                        }
                        Cmd::RecordPackEvent { pack_id, event, reply } => {
                            let _ = reply.send(record_pack_event(&db, &mut pack_manifests, &pack_id, event));
                        }
                        Cmd::PackStats { reply } => {
                            let _ = reply.send(pack_stats(&db));
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
                        Cmd::SetBeliefSensitivity { proposition, class, reply } => {
                            let _ = reply.send(set_belief_sensitivity(&db, &proposition, &class));
                        }
                        Cmd::BeliefSensitivityMap { reply } => {
                            let _ = reply.send(belief_sensitivity_map(&db));
                        }
                        Cmd::GrantPurpose { spec, reply } => {
                            let _ = reply.send(grant_purpose(&db, &spec));
                        }
                        Cmd::RevokePurposeGrant { id, reply } => {
                            let _ = reply.send(revoke_purpose_grant(&db, id));
                        }
                        Cmd::ListPurposeGrants { reply } => {
                            let _ = reply.send(list_purpose_grants(&db));
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
                        Cmd::UserTurnTimes { since_ms, reply } => {
                            let _ = reply.send(user_turn_times(&db, since_ms));
                        }
                        Cmd::ProactiveBaselineRate { reply } => {
                            let r = (|| {
                                let sum = db.world_model_summary().ok()?;
                                (sum.total_transitions >= 20).then_some(sum.global_positive_rate)
                            })();
                            let _ = reply.send(Ok(r));
                        }
                        Cmd::RecordProactiveOutcomeBackfill { sent_ms, engaged, reply } => {
                            // World model ONLY. Deliberately no personality/bond feedback: see the
                            // trait doc — those are live relationship steps, not replayable history.
                            let feats = StateFeatures::discretize(sent_ms as f64 / 1000.0, 0.5, 0.0, 0.0, 0);
                            let outcome = if engaged { WmOutcome::Accepted } else { WmOutcome::Ignored };
                            let r = db.record_transition(feats, WmAction::SendNotification, outcome).map_err(|e| e.to_string());
                            let _ = reply.send(r);
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
                        Cmd::TensionOutcomeCounts { reply } => {
                            let q = |st: &str| -> usize {
                                db.conn()
                                    .query_row("SELECT COUNT(*) FROM mind_tensions WHERE status=?1", [st], |r| r.get::<_, i64>(0))
                                    .unwrap_or(0) as usize
                            };
                            let _ = reply.send(Ok((q("discharged"), q("expired"))));
                        }
                        Cmd::ExpireStaleTensions { curiosity_days, other_days, reply } => {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0);
                            let _ = reply.send(expire_stale_tensions_db(&db, now, curiosity_days, other_days));
                        }
                        Cmd::RecallDemandFor { about, reply } => {
                            let _ = reply.send(Ok(recall_demand_for_db(&db, &about)));
                        }
                        Cmd::RetroDedupStore { reply } => {
                            let _ = reply.send(Ok(retro_dedup_store(&db)));
                        }
                        #[cfg(test)]
                        Cmd::ForceInsertGoalPref { kind, text, reply } => {
                            ensure_goals_prefs_table(&db);
                            let r = db.conn()
                                .execute(
                                    "INSERT OR IGNORE INTO mind_goals_prefs (kind, text) VALUES (?1, ?2)",
                                    [kind.as_str(), text.as_str()],
                                )
                                .map(|_| ())
                                .map_err(|e| e.to_string());
                            let _ = reply.send(r);
                        }
                    }
                    // The command is finished — its queue slot frees only now, so depth counts
                    // queued + running work.
                    gauge.on_done();
                }
            })
            .map_err(|e| MindError::Memory(format!("spawn actor: {e}")))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                tx,
                db_path: db_path.to_string(),
                gauge,
                receipts: std::sync::Arc::new(receipts::ReadReceiptLedger::for_db(db_path)),
                device_auth: device_authorization,
            }),
            Ok(Err(e)) => Err(MindError::Memory(format!("init YantrikDB: {e}"))),
            Err(_) => Err(MindError::Memory("actor thread died during init".into())),
        }
    }

    /// Receipt a boundary-crossing read — EVERY context, operator included
    /// (Purpose Gate v1: the background lanes are exactly the reads a purpose
    /// audit exists to catch; a ledger blind to them would be theater).
    fn receipt_read(&self, ctx: &mind_types::AccessContext, method: &str, detail: &str, results: usize, suppressed: usize) {
        let detail: String = detail.chars().take(120).collect();
        self.receipts.append(receipts::ReadReceipt {
            ts_ms: receipts::now_ms(),
            principal: ctx.principal_label(),
            method: method.to_string(),
            detail,
            results,
            purpose: Some(ctx.purpose().label()),
            suppressed: if suppressed == 0 { None } else { Some(suppressed) },
        });
    }

    /// Scope-filter helper: the belief-scope map applied to anything belief-shaped.
    async fn belief_scopes(&self) -> HashMap<String, String> {
        self.call(|reply| Cmd::BeliefScopeMap { reply }).await.unwrap_or_default()
    }

    /// Build the per-read purpose lens (Purpose Gate v1), or None for the
    /// unrestricted lanes (Audit/Maintenance) so hygiene reads cost nothing.
    async fn purpose_lens(&self, ctx: &mind_types::AccessContext) -> Option<PurposeLens> {
        let purpose = ctx.purpose().clone();
        if purpose.is_unrestricted_lane() {
            return None;
        }
        let scopes = self.belief_scopes().await;
        let sensitivity = self.call(|reply| Cmd::BeliefSensitivityMap { reply }).await.unwrap_or_default();
        let grants = self.call(|reply| Cmd::ListPurposeGrants { reply }).await.unwrap_or_default();
        Some(PurposeLens { purpose, scopes, sensitivity, grants, now_ms: (now_secs() * 1000.0) as u64 })
    }

    /// Both read walls in one pass, in their fixed order: scope visibility
    /// (who may VIEW — supreme, never widened by a grant), then the purpose
    /// lens (what this work may USE). Returns the surviving items and how many
    /// scope-visible items the purpose gate suppressed (for the receipt).
    async fn wall<T>(&self, ctx: &mind_types::AccessContext, items: Vec<T>, key: impl Fn(&T) -> &str) -> (Vec<T>, usize) {
        let lens = self.purpose_lens(ctx).await;
        let viewer = ctx.viewer();
        let scopes_owned;
        let scopes: &HashMap<String, String> = match &lens {
            Some(l) => &l.scopes,
            None => {
                scopes_owned = if viewer.is_some() { self.belief_scopes().await } else { HashMap::new() };
                &scopes_owned
            }
        };
        let scoped: Vec<T> = match &viewer {
            None => items,
            Some(v) => items
                .into_iter()
                .filter(|t| mind_types::Scope::visible_to(scopes.get(key(t)).map(|s| s.as_str()), Some(v)))
                .collect(),
        };
        let before = scoped.len();
        let kept: Vec<T> = match &lens {
            None => scoped,
            Some(l) => scoped.into_iter().filter(|t| l.allows(key(t))).collect(),
        };
        let suppressed = before - kept.len();
        (kept, suppressed)
    }

    async fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Cmd) -> Result<T> {
        let (reply, rx) = oneshot::channel();
        self.gauge.on_send();
        self.tx.send(make(reply)).map_err(|_| MindError::Memory("memory actor is gone".into()))?;
        rx.await
            .map_err(|_| MindError::Memory("memory actor dropped the reply".into()))?
            .map_err(MindError::Memory)
    }

    /// Current backlog as (queued_or_running, high_water_since_spawn). A climbing high-water
    /// mark is the alarm that the mind is asking more of memory than one thread can serve — the
    /// signal that would justify moving a heavy command off-thread per the scheduling doctrine.
    pub fn backlog_depth(&self) -> BacklogDepth {
        let (queued_or_running, high_water) = self.gauge.snapshot();
        BacklogDepth { queued_or_running, high_water }
    }

    /// Where this handle's store lives (":memory:" for scratch minds).
    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    /// Point-in-time snapshot of the live database into `dest` (a path that
    /// must not yet exist). The copy opens with `MemoryHandle::spawn(dest, dim)`;
    /// the immune harness injects seed beliefs THERE — the live mind is opened
    /// read-only for the duration of the copy and never written.
    pub async fn snapshot_to(&self, dest: impl Into<String>) -> Result<()> {
        let dest = dest.into();
        self.call(|reply| Cmd::SnapshotTo { dest, reply }).await
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

    /// Retro-dedup: collapse norm_prop / Jaccard near-duplicates in the belief graph and
    /// goals/prefs table that existed before the write-path dedup was introduced (PR #19).
    /// Safe to call repeatedly — idempotent on an already-clean store.
    /// Returns `(beliefs_tombstoned, goals_prefs_deleted)`.
    ///
    /// Precondition: the handle must have been opened with `DeviceAuthorization::Authorized`.
    /// Returns `MemoryError::NotAuthorized` when the authorization state is unavailable or
    /// invalid, short-circuiting before touching the store.
    pub async fn retro_dedup_store(&self) -> Result<(usize, usize)> {
        if self.device_auth != DeviceAuthorization::Authorized {
            return Err(MindError::NotAuthorized);
        }
        self.call(|reply| Cmd::RetroDedupStore { reply }).await
    }

    #[cfg(test)]
    async fn force_insert_goal_pref_raw(&self, kind: &str, text: &str) -> Result<()> {
        let (kind, text) = (kind.to_string(), text.to_string());
        self.call(move |reply| Cmd::ForceInsertGoalPref { kind, text, reply }).await
    }
}

#[async_trait]
impl MemoryFacade for MemoryHandle {
    // ── ARCH-1 slice 2 + Purpose Gate v1: reads are authorized AT THIS BOUNDARY.
    // Two walls, fixed order: the scope wall (who may VIEW — principal contexts
    // only, never widened by anything), then the purpose lens (what the declared
    // work may USE — every context outside Audit/Maintenance, operator included).
    // Every read is receipted into the hash-chained ledger with its purpose. ──
    async fn recall_typed(&self, q: RecallQuery, ctx: &mind_types::AccessContext) -> Result<Vec<Recalled>> {
        let (text, top_k) = (q.text.clone(), q.top_k);
        let recalled = self.call(|reply| Cmd::RecallTyped { text, top_k, reply }).await?;
        let (out, suppressed) = self.wall(ctx, recalled, |r: &Recalled| r.item.text.as_str()).await;
        self.receipt_read(ctx, "recall_typed", &q.text, out.len(), suppressed);
        Ok(out)
    }

    async fn beliefs_matching(&self, needle: &str, ctx: &mind_types::AccessContext) -> Result<Vec<Belief>> {
        self.beliefs_matching_n(needle, 20, ctx).await
    }

    async fn beliefs_matching_n(&self, needle: &str, limit: usize, ctx: &mind_types::AccessContext) -> Result<Vec<Belief>> {
        let needle_owned = needle.to_string();
        let hits = self.call(move |reply| Cmd::BeliefsMatching { needle: needle_owned, limit, reply }).await?;
        let (out, suppressed) = self.wall(ctx, hits, |b: &Belief| b.statement.as_str()).await;
        self.receipt_read(ctx, "beliefs_matching", needle, out.len(), suppressed);
        Ok(out)
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

    // ── scoped writes (visibility tagged at ingest) ──
    async fn remember_as_belief_scoped(&self, a: BeliefAssertion, scope: mind_types::Scope) -> Result<Belief> {
        let belief = self.remember_as_belief(a).await?;
        // Tag by the CANONICAL proposition (find_belief may have merged a paraphrase into an existing node).
        let (proposition, tag) = (belief.statement.clone(), scope.as_tag());
        let _ = self.call(|reply| Cmd::SetBeliefScope { proposition, scope: tag, reply }).await;
        Ok(belief)
    }

    // ── Purpose Gate v1: sensitivity overrides + the standing-grant ledger ──
    async fn set_belief_sensitivity(&self, proposition: &str, class: mind_types::Sensitivity) -> Result<()> {
        let (proposition, class) = (proposition.to_string(), class.as_tag().to_string());
        self.call(|reply| Cmd::SetBeliefSensitivity { proposition, class, reply }).await
    }
    async fn grant_purpose(&self, spec: mind_types::PurposeGrantSpec) -> Result<i64> {
        self.call(|reply| Cmd::GrantPurpose { spec, reply }).await
    }
    async fn revoke_purpose_grant(&self, id: i64) -> Result<bool> {
        self.call(|reply| Cmd::RevokePurposeGrant { id, reply }).await
    }
    async fn list_purpose_grants(&self) -> Result<Vec<mind_types::PurposeGrant>> {
        self.call(|reply| Cmd::ListPurposeGrants { reply }).await
    }

    async fn relate(&self, src: &str, dst: &str, rel: &str, weight: f64) -> Result<()> {
        let (src, dst, rel) = (src.to_string(), dst.to_string(), rel.to_string());
        self.call(|reply| Cmd::Relate { src, dst, rel, weight, reply }).await
    }

    async fn reflect(&self, question: &str, ctx: &mind_types::AccessContext) -> Result<Reflection> {
        let recalled = self.recall_typed(RecallQuery { text: question.to_string(), top_k: 5, kind: None }, ctx).await?;
        let open_conflicts = self.conflicts(ctx).await?;
        // Goals/preferences are untagged personal state → legacy semantics: primary-private.
        // The operator and the primary see them; any other principal reflects without them —
        // and the declared purpose must be allowed to USE the primary's ordinary facts.
        let owner_view = ctx.viewer().map(|v| matches!(&v, mind_types::Scope::Private(p) if p == mind_types::PRIMARY)).unwrap_or(true)
            && mind_types::purpose_allows(ctx.purpose(), &mind_types::Subject::primary(), mind_types::Sensitivity::Ordinary, false);
        let goals = if owner_view { self.list_goals().await.unwrap_or_default() } else { vec![] };
        let preferences = if owner_view { self.list_preferences().await.unwrap_or_default() } else { vec![] };
        let beliefs: Vec<Belief> = recalled
            .iter()
            .map(|r| {
                // Lifecycle: a belief an open conflict names is CONTRADICTED,
                // and a reflection that hides that is reflecting a fantasy.
                let contradicted = open_conflicts.iter().any(|c| c.belief_a == r.item.text || c.belief_b == r.item.text);
                let status = if contradicted { mind_types::BeliefStatus::Contradicted } else { mind_types::BeliefStatus::Active };
                Belief {
                    id: r.item.id.clone(),
                    statement: r.item.text.clone(),
                    confidence: r.item.confidence,
                    certainty: r.item.certainty,
                    provenance: "recalled".into(),
                    evidence_count: r.item.evidence_count,
                    updated_ms: r.item.updated_ms,
                    status: status.as_tag().into(),
                    uncertainty_reason: None,
                }
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

    async fn conflicts(&self, ctx: &mind_types::AccessContext) -> Result<Vec<Contradiction>> {
        let all: Vec<Contradiction> = self.call(|reply| Cmd::Conflicts { reply }).await?;
        // A conflict is usable only when BOTH sides pass both walls — otherwise listing it
        // would leak the text of a belief outside the principal's scope, or hydrate a
        // purpose-denied belief through its contradiction partner.
        let lens = self.purpose_lens(ctx).await;
        let viewer = ctx.viewer();
        let scopes_owned;
        let scopes: &HashMap<String, String> = match &lens {
            Some(l) => &l.scopes,
            None => {
                scopes_owned = if viewer.is_some() { self.belief_scopes().await } else { HashMap::new() };
                &scopes_owned
            }
        };
        let scoped: Vec<Contradiction> = match &viewer {
            None => all,
            Some(v) => all
                .into_iter()
                .filter(|c| {
                    mind_types::Scope::visible_to(scopes.get(&c.belief_a).map(|s| s.as_str()), Some(v))
                        && mind_types::Scope::visible_to(scopes.get(&c.belief_b).map(|s| s.as_str()), Some(v))
                })
                .collect(),
        };
        let before = scoped.len();
        let out: Vec<Contradiction> = match &lens {
            None => scoped,
            Some(l) => scoped.into_iter().filter(|c| l.allows(&c.belief_a) && l.allows(&c.belief_b)).collect(),
        };
        self.receipt_read(ctx, "conflicts", "", out.len(), before - out.len());
        Ok(out)
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
    async fn expire_stale_tensions(&self, curiosity_days: i64, other_days: i64) -> Result<usize> {
        self.call(|reply| Cmd::ExpireStaleTensions { curiosity_days, other_days, reply }).await
    }
    async fn tension_outcome_counts(&self) -> Result<(usize, usize)> {
        self.call(|reply| Cmd::TensionOutcomeCounts { reply }).await
    }
    async fn recall_demand_for(&self, about: &str) -> Result<f64> {
        let about = about.to_string();
        self.call(|reply| Cmd::RecallDemandFor { about, reply }).await
    }

    async fn explain_belief(&self, belief_id: &str, ctx: &mind_types::AccessContext) -> Result<Option<(Belief, Vec<MEvidence>)>> {
        let statement = belief_id.to_string();
        let found: Option<(Belief, Vec<MEvidence>)> = self.call(|reply| Cmd::Explain { statement, reply }).await?;
        // Out-of-scope OR purpose-denied belief → None, indistinguishable from
        // "no such belief" (an existence oracle would itself be a leak).
        let items: Vec<(Belief, Vec<MEvidence>)> = found.into_iter().collect();
        let (mut kept, suppressed) = self.wall(ctx, items, |(b, _): &(Belief, Vec<MEvidence>)| b.statement.as_str()).await;
        let out = kept.pop();
        self.receipt_read(ctx, "explain_belief", belief_id, usize::from(out.is_some()), suppressed);
        Ok(out)
    }

    async fn hydrate_working_set(&self, focus: &str, ctx: &mind_types::AccessContext) -> Result<WorkingSet> {
        // The belief recall + conflict list are ctx-filtered at their own boundary; task
        // commitments are untagged personal state → operator + primary only (legacy semantics).
        let recalled = self.recall_typed(RecallQuery { text: focus.to_string(), top_k: 8, kind: None }, ctx).await?;
        let open = self.conflicts(ctx).await?;
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
                // Lifecycle: the status is DERIVED here, where the context to derive
                // it exists — the same rows that produced the uncertainty reason.
                let status = match reason {
                    UncertaintyReason::Contradicted => mind_types::BeliefStatus::Contradicted,
                    UncertaintyReason::Decayed => mind_types::BeliefStatus::Stale,
                    UncertaintyReason::Sparse | UncertaintyReason::LowPrior => mind_types::BeliefStatus::Active,
                };
                ws.uncertain_beliefs.push(Belief {
                    id: r.item.id.clone(),
                    statement: r.item.text.clone(),
                    confidence: eff,
                    certainty: r.item.certainty,
                    provenance: "recalled".into(),
                    evidence_count: r.item.evidence_count,
                    updated_ms: r.item.updated_ms,
                    status: status.as_tag().into(),
                    uncertainty_reason: Some(reason),
                });
            }
        }
        ws.active_contradictions = open;
        // open tasks ride along as commitments (cheap tier surfaced for grounding) — tasks are
        // untagged personal state: only the operator and the primary VIEW them, and the declared
        // purpose must be allowed to USE the primary's ordinary facts (a background lane serving
        // someone else hydrates no commitments).
        let owner_view = ctx.viewer().map(|v| matches!(&v, mind_types::Scope::Private(p) if p == mind_types::PRIMARY)).unwrap_or(true)
            && mind_types::purpose_allows(ctx.purpose(), &mind_types::Subject::primary(), mind_types::Sensitivity::Ordinary, false);
        if owner_view {
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

    async fn consolidate(&self) -> Result<usize> {
        // Real consolidation (clustering aging turns -> typed nodes) lands in Phase 2 with the
        // embedder wired. v1: no-op.
        Ok(0)
    }

    async fn forget(&self, id: &str) -> Result<bool> {
        let statement = id.to_string();
        self.call(|reply| Cmd::Forget { statement, reason: None, reply }).await
    }

    async fn forget_with_reason(&self, id: &str, reason: &str) -> Result<bool> {
        let (statement, reason) = (id.to_string(), Some(reason.to_string()));
        self.call(|reply| Cmd::Forget { statement, reason, reply }).await
    }

    async fn belief_tombstones(&self) -> Result<Vec<(String, String, u64)>> {
        self.call(|reply| Cmd::Tombstones { reply }).await
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
    async fn uninstall_pack(&self, id: &str) -> Result<bool> {
        let id = id.to_string();
        self.call(|reply| Cmd::UninstallPack { id, reply }).await
    }

    async fn list_approaches(&self, limit: usize) -> Result<Vec<String>> {
        self.call(move |reply| Cmd::ListApproaches { limit, reply }).await
    }

    async fn seal_learned_pack(&self, dest: &str, name: &str, version: &str) -> Result<String> {
        // GATHER the craft through the same surfaces the loop itself reads, then filter.
        // Skills carry their measured ledger; banked approaches are recognized by the same
        // prefix contract `split_routine` parses. The PII gate errs toward withholding: a
        // dropped approach costs a pack row, a leaked personal value costs trust.
        let mut texts: Vec<String> = Vec::new();
        for s in MemoryFacade::list_skills(self).await.unwrap_or_default() {
            if s.status == "quarantined" {
                continue; // measured-bad craft is not craft
            }
            let measured = if s.runs > 0 {
                format!("worked {} of {} runs", s.successes, s.runs)
            } else {
                "not yet run".to_string()
            };
            texts.push(format!(
                "SKILL: {}\nWHEN: {}\nMEASURED: {} (status {})\n```{}\n{}\n```",
                s.name, s.summary, measured, s.status, s.lang, s.code
            ));
        }
        for t in MemoryFacade::list_approaches(self, 200).await.unwrap_or_default() {
            if !texts.contains(&t) {
                texts.push(t);
            }
        }
        let before = texts.len();
        texts.retain(|t| !looks_private(t));
        let withheld = before - texts.len();

        let (dest_o, name_o, version_o) = (dest.to_string(), name.to_string(), version.to_string());
        let rows = self
            .call(move |reply| Cmd::SealCraftPack { dest: dest_o, name: name_o, version: version_o, texts, reply })
            .await?;
        Ok(format!(
            "sealed {rows} craft row(s) into {dest}{}",
            if withheld > 0 {
                format!(" — {withheld} withheld (carried a personal value; the pack must not)")
            } else {
                String::new()
            }
        ))
    }

    async fn mount_pack(&self, path: &str) -> Result<String> {
        let path = path.to_string();
        self.call(|reply| Cmd::MountPack { path, reply }).await
    }
    async fn install_pack(&self, path: &str) -> Result<String> {
        let path = path.to_string();
        self.call(|reply| Cmd::InstallPack { path, reply }).await
    }
    async fn unmount_pack(&self, id_or_name: &str) -> Result<()> {
        let id = id_or_name.to_string();
        self.call(|reply| Cmd::UnmountPack { id, reply }).await
    }
    async fn mounted_packs(&self) -> Result<Vec<mind_types::memory::PackBrief>> {
        self.call(|reply| Cmd::MountedPacks { reply }).await
    }
    async fn pack_context(&self) -> Result<Option<String>> {
        self.call(|reply| Cmd::PackContext { reply }).await
    }
    async fn recall_from_packs(&self, query: &str, top_k: usize) -> Result<Vec<mind_types::memory::PackHit>> {
        let query = query.to_string();
        self.call(|reply| Cmd::RecallFromPacks { query, top_k, reply }).await
    }
    async fn probe_packs(&self, query: &str, top_k: usize) -> Result<Vec<mind_types::memory::PackProbe>> {
        let query = query.to_string();
        self.call(|reply| Cmd::ProbePacks { query, top_k, reply }).await
    }
    async fn record_pack_event(&self, pack_id: &str, event: mind_types::memory::PackEvent) -> Result<()> {
        let pack_id = pack_id.to_string();
        self.call(|reply| Cmd::RecordPackEvent { pack_id, event, reply }).await
    }
    async fn pack_stats(&self) -> Result<Vec<mind_types::memory::PackStats>> {
        self.call(|reply| Cmd::PackStats { reply }).await
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
    async fn recent_messages(&self, limit: usize, ctx: &mind_types::AccessContext) -> Result<Vec<(String, String)>> {
        // Purpose Gate v1 on the transcript: a principal keeps its own scope; an
        // operator-lane read outside Audit/Maintenance is downgraded to the scope
        // its BENEFICIARY could see — dream/proactive/code reads serving the
        // primary see the primary's window, not every member's private lines.
        let viewer = match (ctx.viewer(), ctx.purpose()) {
            (Some(v), _) => Some(v.as_tag()),
            (None, p) if p.is_unrestricted_lane() => None,
            (None, p) => Some(p.serves.as_viewer_scope().as_tag()),
        };
        let out: Vec<(String, String)> = self.call(|reply| Cmd::RecentMessages { limit, viewer, reply }).await?;
        self.receipt_read(ctx, "recent_messages", "", out.len(), 0);
        Ok(out)
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
    fn backlog_depth(&self) -> (usize, usize) {
        // Inherent method wins resolution over this trait method, so this reads the live gauge.
        let d = MemoryHandle::backlog_depth(self);
        (d.queued_or_running, d.high_water)
    }
    async fn record_proactive_outcome(&self, sent_ms: i64, engaged: bool) -> Result<()> {
        self.call(move |reply| Cmd::RecordProactiveOutcome { sent_ms, engaged, reply }).await
    }
    async fn record_proactive_outcome_backfill(&self, sent_ms: i64, engaged: bool) -> Result<()> {
        self.call(move |reply| Cmd::RecordProactiveOutcomeBackfill { sent_ms, engaged, reply }).await
    }
    async fn user_turn_times(&self, since_ms: i64) -> Result<Vec<i64>> {
        self.call(move |reply| Cmd::UserTurnTimes { since_ms, reply }).await
    }
    async fn proactive_baseline_rate(&self) -> Result<Option<f64>> {
        self.call(|reply| Cmd::ProactiveBaselineRate { reply }).await
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

/// Sealed-pack fixtures shared with downstream crates' tests (`features = ["fixtures"]`), built the
/// way `packs/build.py` builds a real pack so pack behaviour is tested against real artifacts rather
/// than mocks. Not part of the runtime surface.
#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures {
    use super::*;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Seal `rows` into `dest` as pack `name` (origin `yantrik-mind-test/<name>`, version 0.1.0)
    /// under `namespace`, carrying the given publisher retrieval settings. Returns the pack id.
    /// Built with the same bundled embedder a 64-dim host uses, so it mounts on
    /// `MemoryHandle::spawn(_, 64)`; the staging database is removed win or lose.
    pub fn seal_fixture_pack(
        dest: &str,
        name: &str,
        namespace: &str,
        rows: &[&str],
        recommended_min_similarity: Option<f64>,
        recommended_top_k: Option<u32>,
    ) -> std::result::Result<String, String> {
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let staging = std::env::temp_dir().join(format!("ym_fixture_{}_{name}_{n}.db", std::process::id()));
        let staging_s = staging.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&staging);
        let sealed = (|| {
            let db = YantrikDB::new(&staging_s, 64).map_err(|e| e.to_string())?;
            if !db.has_embedder() {
                return Err("fixture host has no embedder — the engine's bundled-embedder feature is off".to_string());
            }
            let meta = serde_json::json!({ "source": "fixture" });
            for r in rows {
                db.record_text(r, "semantic", 0.6, 0.0, 604_800.0, &meta, namespace, 0.9, "general", "document", None)
                    .map_err(|e| e.to_string())?;
            }
            let embedder = match db.embedder_identity() {
                Ok(Some((ename, digest, dim))) => serde_json::json!({ "name": ename, "digest": digest, "dim": dim }),
                _ => serde_json::json!({ "name": null, "digest": null, "dim": db.embedding_dim() }),
            };
            let manifest: yantrikdb_core::PackManifest = serde_json::from_value(serde_json::json!({
                "name": name,
                "version": "0.1.0",
                "origin": format!("yantrik-mind-test/{name}"),
                "description": "test fixture",
                "embedder": embedder,
                "namespace": namespace,
                "constitution": ["Fixture rule: say which fixture row you used."],
                "coverage": rows.iter().map(|r| r.chars().take(60).collect::<String>()).collect::<Vec<_>>(),
                "recommended_top_k": recommended_top_k,
                "recommended_min_similarity": recommended_min_similarity,
            }))
            .map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(dest);
            let m = db.seal_pack(dest, &manifest, Some(namespace)).map_err(|e| e.to_string())?;
            Ok(m.pack_id())
        })();
        let _ = std::fs::remove_file(&staging);
        let _ = std::fs::remove_file(format!("{staging_s}-wal"));
        let _ = std::fs::remove_file(format!("{staging_s}-shm"));
        sealed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(pack_id: &str, name: &str, ns: &str, floor: f64, cap: Option<usize>) -> PackRoute {
        PackRoute { pack_id: pack_id.into(), name: name.into(), namespace: ns.into(), floor, cap }
    }
    fn cand(rid: &str, ns: &str, stamp: Option<&str>, similarity: f64, score: f64) -> PackCandidate {
        PackCandidate {
            rid: rid.into(),
            text: format!("row {rid}"),
            score,
            similarity,
            namespace: ns.into(),
            pack_name: stamp.map(str::to_string),
        }
    }

    /// The floor is on SIMILARITY, never on the composite: a row the engine ranks first on
    /// importance/trust still stays out when its similarity is under the pack's floor.
    #[test]
    fn pack_floor_gates_on_similarity_not_on_the_composite_score() {
        let routes = vec![route("yantrik/a@1.0.0", "a", "ns_a", 0.6, None)];
        let (hits, ambiguous) = floor_pack_hits(
            vec![
                cand("confident-noise", "ns_a", Some("a"), 0.41, 0.95), // best composite, weak similarity
                cand("relevant", "ns_a", Some("a"), 0.72, 0.60),
            ],
            &routes,
            5,
        );
        assert_eq!(ambiguous, 0);
        assert_eq!(hits.iter().map(|h| h.rid.as_str()).collect::<Vec<_>>(), vec!["relevant"]);
        assert_eq!(hits[0].pack_id, "yantrik/a@1.0.0");
        assert_eq!(hits[0].similarity, 0.72);
    }

    /// Host rows never pass, even from inside a pack's namespace; an unclaimed namespace never passes.
    #[test]
    fn pack_recall_refuses_host_rows_and_unknown_namespaces() {
        let routes = vec![route("yantrik/a@1.0.0", "a", "ns_a", 0.0, None)];
        let (hits, _) = floor_pack_hits(
            vec![
                cand("host-row-in-pack-ns", "ns_a", None, 0.99, 0.99),
                cand("row-in-private-ns", "private:asha", Some("a"), 0.99, 0.99),
                cand("pack-row", "ns_a", Some("a"), 0.70, 0.70),
            ],
            &routes,
            5,
        );
        assert_eq!(hits.iter().map(|h| h.rid.as_str()).collect::<Vec<_>>(), vec!["pack-row"]);
    }

    /// The floor is a wall the publisher may raise and never lower (Codex's review): a declared 0.0
    /// — sloppy or hostile — and a non-finite or out-of-range declaration all land on the host wall;
    /// a declared 0.7 is honoured because it is stricter.
    #[test]
    fn a_declared_floor_raises_the_wall_and_never_lowers_it() {
        use mind_types::memory::{effective_pack_floor as eff, DEFAULT_PACK_SIMILARITY_FLOOR as WALL};
        assert_eq!(eff(None), WALL);
        assert_eq!(eff(Some(0.0)), WALL);
        assert_eq!(eff(Some(0.30)), WALL);
        assert_eq!(eff(Some(0.70)), 0.70);
        assert_eq!(eff(Some(f64::NAN)), WALL);
        assert_eq!(eff(Some(1.5)), WALL);
        assert_eq!(eff(Some(-0.2)), WALL);
    }

    /// The probe's dispositions are recall's own judgement in recall's own order: floor, then the
    /// publisher's per-pack cap, then the turn's limit — so a row the probe calls cleared is a row
    /// a turn would have received, and a withheld row says why (Codex's review of 9aea6a6).
    #[test]
    fn probe_dispositions_mirror_recall_selection() {
        use mind_types::memory::PackDisposition as D;
        let routes = vec![
            route("yantrik/a@1.0.0", "a", "ns_a", 0.5, Some(1)),
            route("yantrik/b@1.0.0", "b", "ns_b", 0.5, None),
        ];
        let cands = vec![
            cand("a1", "ns_a", Some("a"), 0.9, 0.9),
            cand("a2", "ns_a", Some("a"), 0.9, 0.8), // over a's cap of 1
            cand("b1", "ns_b", Some("b"), 0.9, 0.7),
            cand("b2", "ns_b", Some("b"), 0.9, 0.6), // cleared everything, but the turn takes 2
            cand("b3", "ns_b", Some("b"), 0.1, 0.5), // under the floor — reported as such, not as "limit"
        ];
        let (judged, ambiguous) = judge_pack_candidates(cands.clone(), &routes, 2);
        assert_eq!(ambiguous, 0);
        let got: Vec<(&str, D)> = judged.iter().map(|j| (j.candidate.rid.as_str(), j.disposition)).collect();
        assert_eq!(
            got,
            vec![("a1", D::Cleared), ("a2", D::WithheldPackCap), ("b1", D::Cleared), ("b2", D::WithheldLimit), ("b3", D::WithheldFloor)]
        );
        // And recall returns exactly the Cleared rows, in the same order.
        let (hits, _) = floor_pack_hits(cands, &routes, 2);
        assert_eq!(hits.iter().map(|h| h.rid.as_str()).collect::<Vec<_>>(), vec!["a1", "b1"]);
    }

    /// A row stamped with a name no route carries is never credited to the route that owns its
    /// namespace — even when that route is the only one there.
    #[test]
    fn a_wrong_stamp_is_never_credited_to_the_only_route_in_its_namespace() {
        let routes = vec![route("yantrik/a@1.0.0", "a", "ns_a", 0.0, None)];
        let (hits, ambiguous) = floor_pack_hits(
            vec![
                cand("stamped-for-someone-else", "ns_a", Some("zzz"), 0.99, 0.99),
                cand("stamped-for-a", "ns_a", Some("a"), 0.70, 0.70),
            ],
            &routes,
            5,
        );
        assert_eq!(ambiguous, 0, "unclaimed is not ambiguous");
        assert_eq!(hits.iter().map(|h| h.rid.as_str()).collect::<Vec<_>>(), vec!["stamped-for-a"]);
    }

    /// The collision the name stamp cannot break: two VERSIONS (or two re-seals) of one pack
    /// mounted at once share namespace and name. Their rows are abstained from and counted —
    /// never handed to whichever manifest came first — while a differently-named pack in the same
    /// namespace still resolves.
    #[test]
    fn two_mounted_versions_of_one_pack_make_their_rows_ambiguous_not_misattributed() {
        let routes = vec![
            route("yantrik/a@1.0.0", "a", "ns_a", 0.0, None),
            route("yantrik/a@2.0.0", "a", "ns_a", 0.0, None),
            route("yantrik/b@1.0.0", "b", "ns_a", 0.0, None),
        ];
        let (hits, ambiguous) = floor_pack_hits(
            vec![
                cand("a-row-1", "ns_a", Some("a"), 0.9, 0.9),
                cand("a-row-2", "ns_a", Some("a"), 0.9, 0.8),
                cand("b-row", "ns_a", Some("b"), 0.9, 0.7),
            ],
            &routes,
            5,
        );
        assert_eq!(ambiguous, 2, "both of pack a's rows are unattributable");
        assert_eq!(
            hits.iter().map(|h| (h.rid.as_str(), h.pack_id.as_str())).collect::<Vec<_>>(),
            vec![("b-row", "yantrik/b@1.0.0")]
        );
    }

    /// Two versions of one pack share a namespace: the engine's name stamp breaks the tie, a row
    /// nothing claims is dropped rather than guessed, and the publisher's cap holds per pack.
    #[test]
    fn pack_identity_is_resolved_by_stamp_and_capped_per_pack() {
        let routes = vec![
            route("yantrik/a@1.0.0", "a", "ns_a", 0.0, Some(1)),
            route("yantrik/a-next@2.0.0", "a-next", "ns_a", 0.0, Some(1)),
        ];
        let (hits, ambiguous) = floor_pack_hits(
            vec![
                cand("a1", "ns_a", Some("a"), 0.9, 0.9),
                cand("a2", "ns_a", Some("a"), 0.9, 0.8), // over a's cap of 1
                cand("n1", "ns_a", Some("a-next"), 0.9, 0.7),
                cand("orphan", "ns_a", Some("someone-else"), 0.9, 0.6),
            ],
            &routes,
            5,
        );
        assert_eq!(ambiguous, 0, "an orphan is unclaimed, not ambiguous");
        let ids: Vec<(&str, &str)> = hits.iter().map(|h| (h.rid.as_str(), h.pack_id.as_str())).collect();
        assert_eq!(ids, vec![("a1", "yantrik/a@1.0.0"), ("n1", "yantrik/a-next@2.0.0")]);
    }

    /// P.1 on a REAL sealed pack: a verbatim query clears a strict floor and carries its identity;
    /// an unrelated question is withheld — the 12/12 → 5/12 attach-harm case, closed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_mounted_pack_is_floored_on_similarity_and_names_itself() {
        use mind_types::MemoryFacade;
        let dir = std::env::temp_dir().join(format!("ym_p1_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let strict = dir.join("strict.ydbpack");
        let rows = [
            "Typography — set body text on a modular scale with a measure of 45 to 75 characters per line.",
            "Spacing — derive every gap in a layout from one base unit so the page reads as a system.",
        ];
        let id = fixtures::seal_fixture_pack(strict.to_str().unwrap(), "strict-craft", "strict_craft", &rows, Some(0.9), Some(4)).unwrap();
        assert_eq!(id, "yantrik-mind-test/strict-craft@0.1.0");
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        mem.mount_pack(strict.to_str().unwrap()).await.unwrap();

        let hits = mem.recall_from_packs(rows[0], 5).await.unwrap();
        assert_eq!(hits.len(), 1, "the verbatim row and not its sibling: {hits:?}");
        assert_eq!(hits[0].pack_id, id);
        assert!(!hits[0].rid.is_empty());
        assert!(hits[0].similarity >= 0.9, "verbatim similarity {}", hits[0].similarity);
        assert_eq!(hits[0].namespace, "strict_craft");
        assert!(hits[0].text.starts_with("Typography"));

        let none = mem.recall_from_packs("what is seventeen multiplied by twenty three", 5).await.unwrap();
        assert!(none.is_empty(), "noise cleared a 0.9 floor: {none:?}");

        // The probe shows BOTH rows: the verbatim one cleared, its sibling withheld with the
        // similarity it reached and the floor it was measured against — the operator can tell
        // "off-coverage" from "too strict" without guessing.
        let probe = mem.probe_packs(rows[0], 5).await.unwrap();
        assert_eq!(probe.len(), 2, "every attributed candidate, cleared or not: {probe:?}");
        assert!(probe[0].cleared() && probe[0].text.starts_with("Typography"), "{probe:?}");
        assert_eq!(probe[1].disposition, mind_types::memory::PackDisposition::WithheldFloor, "{probe:?}");
        assert!(probe[1].similarity < 0.9 && probe[1].floor == 0.9, "{probe:?}");

        // An absurd ask must not panic the actor (usize::MAX once reached `clamp(want, 64)` with
        // want > 64, which panics) and must come back bounded.
        let huge = mem.recall_from_packs(rows[0], usize::MAX).await.unwrap();
        assert_eq!(huge.len(), 1, "bounded, and still just the verbatim row: {huge:?}");
        let huge_probe = mem.probe_packs(rows[0], usize::MAX).await.unwrap();
        assert!(huge_probe.len() <= 48 && huge_probe.len() == 2, "{huge_probe:?}");
        assert!(mem.mounted_packs().await.is_ok(), "the actor is still alive after the absurd asks");

        // The brief shows the operator the floor in force and the identity to key evidence on.
        let briefs = mem.mounted_packs().await.unwrap();
        assert_eq!(briefs.len(), 1);
        assert_eq!(briefs[0].id, id);
        assert_eq!(briefs[0].recommended_min_similarity, Some(0.9));
        assert_eq!(briefs[0].recommended_top_k, Some(4));
        assert!(briefs[0].content_digest.as_deref().unwrap_or("").starts_with("blake3:"), "{:?}", briefs[0].content_digest);
        assert_eq!(briefs[0].coverage.len(), 2);
        assert_eq!(briefs[0].signer, None, "an unsigned fixture has no signer");
        let _ = std::fs::remove_file(&strict);
    }

    /// A REAL sealed pack declaring a 0.0 floor cannot lower the host wall: the brief shows what it
    /// declared, and arithmetic noise is still withheld.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pack_declaring_a_zero_floor_is_still_held_to_the_host_wall() {
        use mind_types::MemoryFacade;
        let dir = std::env::temp_dir().join(format!("ym_p1_zero_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pack = dir.join("zero.ydbpack");
        let row = "Motion — animate only transform and opacity so the compositor does the work.";
        fixtures::seal_fixture_pack(pack.to_str().unwrap(), "zero-floor", "zero_floor_ns", &[row], Some(0.0), None).unwrap();
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        mem.mount_pack(pack.to_str().unwrap()).await.unwrap();
        let briefs = mem.mounted_packs().await.unwrap();
        assert_eq!(briefs[0].recommended_min_similarity, Some(0.0), "the declaration is shown, not hidden…");
        let hits = mem.recall_from_packs(row, 5).await.unwrap();
        assert_eq!(hits.len(), 1, "…a verbatim query still lands: {hits:?}");
        let none = mem.recall_from_packs("what is seventeen multiplied by twenty three", 5).await.unwrap();
        assert!(none.is_empty(), "…and the host wall still withholds noise despite the declared 0.0: {none:?}");
        let _ = std::fs::remove_file(&pack);
    }

    /// Crowding, mitigated: HOST rows planted in the pack's own namespace compete in the same
    /// engine pool. With six near-verbatim host rows and want=3, the pack row must still surface
    /// (the pure filter then drops the host rows, which carry no pack stamp). This proves the
    /// overfetch bounds crowding at this scale; it does not prove crowding impossible — see
    /// `PACK_OVERFETCH`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn host_rows_sharing_the_namespace_do_not_starve_pack_evidence_at_this_scale() {
        use mind_types::MemoryFacade;
        let dir = std::env::temp_dir().join(format!("ym_p1_crowd_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pack = dir.join("crowd.ydbpack");
        let host_db = dir.join("crowd_host.db");
        let _ = std::fs::remove_file(&host_db);
        let row = "Focus — every interactive control needs a visible focus ring that is not the browser default.";
        fixtures::seal_fixture_pack(pack.to_str().unwrap(), "crowd-craft", "crowd_ns", &[row], None, None).unwrap();
        {
            // Six host rows in the PACK'S namespace, each a near-verbatim copy of the query.
            let db = YantrikDB::new(host_db.to_str().unwrap(), 64).unwrap();
            let meta = serde_json::json!({});
            for i in 0..6 {
                db.record_text(
                    &format!("{row} (household note {i})"),
                    "semantic", 0.9, 0.0, 604_800.0, &meta, "crowd_ns", 0.9, "general", "user", None,
                )
                .unwrap();
            }
        }
        let mem = MemoryHandle::spawn(host_db.to_str().unwrap(), 64).unwrap();
        mem.mount_pack(pack.to_str().unwrap()).await.unwrap();
        let hits = mem.recall_from_packs(row, 3).await.unwrap();
        assert_eq!(hits.len(), 1, "the pack row must survive six crowding host rows: {hits:?}");
        assert_eq!(hits[0].pack_id, "yantrik-mind-test/crowd-craft@0.1.0");
        assert!(!hits[0].text.contains("household note"), "a host row can never be a pack hit");
        drop(mem);
        let _ = std::fs::remove_file(&pack);
        let _ = std::fs::remove_file(&host_db);
    }

    /// P.2's SQL witness: rungs count, and a pack re-sealed under the SAME id starts from zero —
    /// evidence belongs to the rows that earned it, which the content digest names.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pack_stats_count_rungs_and_reset_when_the_pack_is_resealed() {
        use mind_types::memory::PackEvent as E;
        use mind_types::MemoryFacade;
        let dir = std::env::temp_dir().join(format!("ym_p2_stats_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pack = dir.join("stats.ydbpack");
        let id = fixtures::seal_fixture_pack(pack.to_str().unwrap(), "stats-craft", "stats_ns", &["Row one — the first sealing."], None, None).unwrap();
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        mem.mount_pack(pack.to_str().unwrap()).await.unwrap();
        let digest1 = mem.mounted_packs().await.unwrap()[0].content_digest.clone();
        assert!(digest1.is_some());
        for e in [E::Surfaced, E::Surfaced, E::Used, E::Graded { good: true }, E::Graded { good: false }] {
            mem.record_pack_event(&id, e).await.unwrap();
        }
        let s = mem.pack_stats().await.unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].surfaced, s[0].used, s[0].graded, s[0].good), (2, 1, 2, 1), "{s:?}");
        assert_eq!(s[0].content_digest, digest1);
        assert!(s[0].last_ms >= s[0].first_ms);

        // Re-seal under the same id with different rows, remount, count once more.
        mem.unmount_pack(&id).await.unwrap();
        let id2 = fixtures::seal_fixture_pack(pack.to_str().unwrap(), "stats-craft", "stats_ns", &["Row two — a different corpus, same id."], None, None).unwrap();
        assert_eq!(id, id2, "same origin@version");
        mem.mount_pack(pack.to_str().unwrap()).await.unwrap();
        let digest2 = mem.mounted_packs().await.unwrap()[0].content_digest.clone();
        assert_ne!(digest1, digest2, "different rows, different digest");
        mem.record_pack_event(&id, E::Surfaced).await.unwrap();
        let s = mem.pack_stats().await.unwrap();
        assert_eq!((s[0].surfaced, s[0].used, s[0].graded, s[0].good), (1, 0, 0, 0), "the old rows' record did not carry over: {s:?}");
        assert_eq!(s[0].content_digest, digest2);
        let _ = std::fs::remove_file(&pack);
    }

    /// A pack that declares no floor gets the host wall — never no floor.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_pack_without_a_declared_floor_is_still_floored() {
        use mind_types::MemoryFacade;
        let dir = std::env::temp_dir().join(format!("ym_p1_default_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pack = dir.join("nofloor.ydbpack");
        let row = "Contrast — body text needs at least 4.5 to 1 against its background to be readable.";
        fixtures::seal_fixture_pack(pack.to_str().unwrap(), "nofloor", "nofloor_ns", &[row], None, None).unwrap();
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();
        mem.mount_pack(pack.to_str().unwrap()).await.unwrap();
        let briefs = mem.mounted_packs().await.unwrap();
        assert_eq!(briefs[0].recommended_min_similarity, None, "the pack declares none…");
        let hits = mem.recall_from_packs(row, 5).await.unwrap();
        assert_eq!(hits.len(), 1, "…yet a verbatim query still lands: {hits:?}");
        let none = mem.recall_from_packs("remind me to call the plumber on tuesday", 5).await.unwrap();
        assert!(none.is_empty(), "…and the default floor still withholds noise: {none:?}");
        let _ = std::fs::remove_file(&pack);
    }

    /// Test context: a member speaking for themselves in a live conversation —
    /// the standard channel shape (Purpose Gate v1 requires every read to say
    /// what it serves; a member's turn serves that member).
    fn member_ctx(scope: mind_types::Scope) -> mind_types::AccessContext {
        let purpose = match &scope {
            mind_types::Scope::Private(o) => mind_types::Purpose::conversation(o),
            mind_types::Scope::Shared => mind_types::Purpose::new(mind_types::Subject::Household, mind_types::Activity::Conversation),
        };
        mind_types::AccessContext::principal(scope, purpose)
    }

    /// A context break ends the conversational WINDOW, never the RECORD: recent_messages stops at
    /// the newest break (the marker itself invisible), while the id-ordered reader consolidation
    /// uses still sees everything — a fresh start must not starve what memory learns from.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_context_break_ends_the_window_not_the_record() {
        use mind_types::MemoryFacade;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        mem.append_message("user", "the old topic").await.unwrap();
        mem.append_message("assistant", "the old answer").await.unwrap();
        mem.append_message("break", "— context break (operator) —").await.unwrap();
        mem.append_message("user", "a brand new topic").await.unwrap();

        let window = mem.recent_messages(10, &mind_types::AccessContext::operator_audit()).await.unwrap();
        let texts: Vec<&str> = window.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(texts, vec!["a brand new topic"], "the window starts after the break: {texts:?}");
        assert!(!texts.iter().any(|t| t.contains("context break")), "the marker is punctuation, not content");

        let all = mem.messages_since(0, 50).await.unwrap();
        assert!(all.iter().any(|(_, _, t)| t.contains("the old topic")), "the full record survives for consolidation");
    }

    /// The self-learning loop, closed: banked approaches are ENUMERABLE (the library was
    /// write-only once — banking wrote episodic memories, recall read only beliefs), and the
    /// mind's craft seals into a pack with personal values withheld and the staging rows gone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn learned_craft_is_enumerable_and_seals_into_a_pack() {
        use mind_types::MemoryFacade;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();

        // A measured skill, a clean banked approach, and an approach carrying a personal value.
        mem.save_skill(mind_types::Skill {
            name: "fetch-then-cite".into(),
            lang: "python".into(),
            code: "print('fetch then cite')".into(),
            summary: "fetch a page then cite it".into(),
            tags: vec![],
            status: "active".into(),
            runs: 4,
            successes: 4,
            created_ms: 0,
        })
        .await
        .unwrap();
        mem.remember_observation(
            "APPROACH: repo review\nWHEN: evaluating a repository\n1. read the README\n2. read the commits",
            mind_types::safety::ProvenanceCategory::SubAgent,
        )
        .await
        .unwrap();
        mem.remember_observation(
            "APPROACH: mail check\nWHEN: checking the inbox\n1. open secret.owner@example.com\n2. read the top thread",
            mind_types::safety::ProvenanceCategory::SubAgent,
        )
        .await
        .unwrap();

        // Deterministic enumeration sees BOTH approaches, newest first.
        let approaches = mem.list_approaches(50).await.unwrap();
        assert_eq!(approaches.len(), 2, "banked craft must be enumerable: {approaches:?}");
        assert!(approaches[0].starts_with("APPROACH: mail check"), "newest first");

        // Sealing exports the skill + the clean approach; the personal value is withheld.
        let dir = std::env::temp_dir().join(format!("ym_seal_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("craft.ydbpack");
        let _ = std::fs::remove_file(&dest);
        // A transcript line stands in for everything the household file carries that a pack
        // must not: the first live seal shipped 1,944 of these before the scrub existed.
        mem.append_message("user", "a private household line that must never enter a pack").await.unwrap();

        let summary = mem.seal_learned_pack(dest.to_str().unwrap(), "learned-craft", "0.1.0").await.unwrap();
        assert!(dest.exists(), "the pack file must exist: {summary}");
        assert!(summary.contains("sealed 2"), "skill + clean approach, not the private one: {summary}");
        assert!(summary.contains("withheld"), "the withholding must be SAID, not silent: {summary}");

        // THE SCRUB, proven on the artifact itself: the pack carries its corpus and nothing of
        // the household's — no transcript table, no belief graph, no skills ledger, and no
        // off-allowlist table of any name.
        {
            let conn = rusqlite::Connection::open(&dest).unwrap();
            let tables: Vec<String> = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            for t in &tables {
                assert!(
                    PACK_TABLE_ALLOWLIST.contains(&t.as_str()) || t.starts_with("memories_fts"),
                    "off-allowlist table {t} inside a sealed pack — the household leaks: {tables:?}"
                );
            }
            let corpus: i64 = conn.query_row("SELECT count(*) FROM memories", [], |r| r.get(0)).unwrap();
            assert_eq!(corpus, 2, "exactly the exported craft, nothing else");
        }
        // The staging rows must not linger: a second seal exports the same 2, not 4.
        let dest2 = dir.join("craft2.ydbpack");
        let _ = std::fs::remove_file(&dest2);
        let summary2 = mem.seal_learned_pack(dest2.to_str().unwrap(), "learned-craft", "0.1.0").await.unwrap();
        assert!(summary2.contains("sealed 2"), "staging rows leaked into a re-seal: {summary2}");
        let _ = std::fs::remove_file(&dest);
        let _ = std::fs::remove_file(&dest2);
    }

    /// One contradiction must be ONE row however it was spelled.
    ///
    /// Built from the real 54-row Rosefield pile-up: 12 genuine pairs stored 54 times, because the
    /// dedup matched the raw `about` string and three things varied while the meaning did not —
    /// two writer formats, both orderings, and stray punctuation.
    #[test]
    fn one_contradiction_is_one_key_however_it_was_written() {
        let assert_path = "conflict: Pranab owns a Rosefield watch vs Pranab lives in Bentonville";
        let dmn_path = "\"Pranab owns a Rosefield watch\" vs \"Pranab lives in Bentonville\"";
        let reversed = "\"Pranab lives in Bentonville\" vs \"Pranab owns a Rosefield watch\"";
        let punctuated = "conflict: Pranab lives in Bentonville. vs Pranab owns a Rosefield watch.";

        let k = tension_key(assert_path);
        assert_eq!(tension_key(dmn_path), k, "the two writer formats must collapse to one key");
        assert_eq!(tension_key(reversed), k, "A vs B and B vs A are the same contradiction");
        assert_eq!(tension_key(punctuated), k, "a trailing period is not a different claim");

        // Genuinely different pairs must NOT collapse — a dedup that over-merges hides real conflict.
        assert_ne!(
            tension_key("conflict: Pranab owns a Rosefield watch vs Pranab has an iPhone"),
            k,
            "different second operands are different contradictions"
        );

        // Non-conflict tensions (urges) have no " vs " and still normalise to something stable,
        // preserving the accrue-don't-flood behaviour they always had.
        assert_eq!(tension_key("  Curiosity:  unread   papers "), tension_key("curiosity unread papers"));
    }

    /// The relatedness gate must reject the pairs that actually polluted the live table.
    ///
    /// These are verbatim from the 54-row pile-up: one belief about a gift paired against the
    /// user's city, phone, website and daughter. Nothing about them is contradictory — they are a
    /// cross-product of unrelated facts, and every one was stored at the floor pressure of 0.30,
    /// which is the tell that nothing ever scored them.
    #[test]
    fn the_real_world_false_contradictions_are_ignored() {
        let watch = "Pranab owns a Rosefield watch intended as a birthday gift for Brishti";
        let threshold = contradiction_relatedness_threshold();
        // CHARACTERISATION, NOT A SPEC: this asserts what the gate currently DOES, so the day
        // someone fixes it this test fails loudly and gets inverted rather than silently rotting.
        for other in [
            "Pranab lives in Bentonville, United States",
            "Pranab Sarkar has an iPhone",
            "Pranab's personal website is https://pranab.co.in",
            "Pranab has a daughter named Aadrisha",
            "Pranab is most active around midnight and busiest on Wednesdays",
        ] {
            let score = topical_relatedness(watch, other, None);
            assert!(
                score >= threshold,
                "the gate is documented as letting these through on the shared subject alone;                  if '{other}' now scores {score} below the {threshold} gate, the defect is FIXED —                  invert this assertion and delete the KNOWN DEFECT comment in topical_relatedness"
            );
        }

        // The gate must still ADMIT a genuine conflict about the same subject, or it is just off.
        let real = topical_relatedness(watch, "Pranab gave Brishti a handbag instead of the Rosefield watch", None);
        assert!(real >= threshold, "a real same-subject conflict must survive the gate, scored {real}");
    }

    /// The dedup must actually collapse the variants at the storage layer, not just in the key fn.
    #[test]
    fn recording_the_same_contradiction_twice_keeps_one_row() {
        let db = YantrikDB::new(":memory:", 64).expect("in-memory db");
        ensure_tensions_table(&db);
        let now = 1_786_000_000_000i64;

        record_tension_db(&db, "contradiction", 0.30, "conflict: A vs B", now).unwrap();
        record_tension_db(&db, "contradiction", 0.45, "\"B\" vs \"A\"", now + 1_000).unwrap();
        record_tension_db(&db, "contradiction", 0.20, "conflict: B vs A.", now + 2_000).unwrap();

        let n: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM mind_tensions WHERE status='open'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "three spellings of one contradiction must be one row, not three");

        // Accrual still works: the row keeps the HIGHEST pressure seen, not the latest.
        let p: f64 = db
            .conn()
            .query_row("SELECT pressure FROM mind_tensions WHERE status='open'", [], |r| r.get(0))
            .unwrap();
        assert!((p - 0.45).abs() < 1e-9, "a recurring tension keeps its max pressure, got {p}");
    }

    #[test]
    fn unauthorized_device_cannot_load_memory() {
        let result = MemoryHandle::spawn_for_device(
            ":memory:",
            64,
            DeviceAuthorization::Unauthorized,
        );

        assert!(matches!(
            result,
            Err(MindError::Auth(AuthError::DeviceNotAuthorized))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_auth_state_short_circuits_retro_dedup() {
        let mem = MemoryHandle::spawn_for_device(":memory:", 64, DeviceAuthorization::Unknown)
            .expect("Unknown auth should construct the handle");
        let result = mem.retro_dedup_store().await;
        assert!(
            matches!(result, Err(MindError::NotAuthorized)),
            "retro_dedup_store must return NotAuthorized for non-Authorized handles, got: {result:?}"
        );
    }

    /// ARCH-1 acceptance test — the authorization-kernel deliverable.
    /// Plant a PRIMARY-ONLY secret and a SHARED fact, then prove a non-primary
    /// household member (a `Principal`) recovers the shared fact but NEVER the
    /// secret — through BOTH read paths (semantic recall and deterministic
    /// exact match) — while the primary/operator sees both. This is the
    /// invariant every second channel depends on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn arch1_member_cannot_recover_primary_secret_via_any_read_path() {
        use mind_types::{AccessContext, Scope};
        let mem = MemoryHandle::spawn(":memory:", 64).unwrap();

        // A secret only the primary should ever see, and a genuinely shared fact.
        let secret = "The safe combination is 47-12-33";
        let shared = "Dinner on Friday is at seven";
        mem.remember_as_belief_scoped(
            BeliefAssertion { statement: secret.into(), polarity: 1.0, weight: 2.0, source_event: Some("test".into()), provenance: "told".into() },
            Scope::primary(),
        ).await.unwrap();
        mem.remember_as_belief_scoped(
            BeliefAssertion { statement: shared.into(), polarity: 1.0, weight: 2.0, source_event: Some("test".into()), provenance: "told".into() },
            Scope::Shared,
        ).await.unwrap();

        let member = member_ctx(Scope::Private("asha".into()));
        let owner = mind_types::AccessContext::operator_audit();

        // ── Path 1: deterministic exact match ──────────────────────────────
        let m_secret = mem.beliefs_matching("safe combination", &member_ctx(member.viewer().unwrap())).await.unwrap();
        assert!(!m_secret.iter().any(|b| b.statement == secret), "MEMBER recovered the primary secret via exact match — isolation breached");
        let m_shared = mem.beliefs_matching("dinner friday", &member_ctx(member.viewer().unwrap())).await.unwrap();
        assert!(m_shared.iter().any(|b| b.statement == shared), "member must still see genuinely shared facts");

        // ── Path 2: semantic recall ────────────────────────────────────────
        let r_secret = mem.recall_typed(RecallQuery { text: "safe combination".into(), top_k: 10, kind: None }, &member_ctx(member.viewer().unwrap())).await.unwrap();
        assert!(!r_secret.iter().any(|r| r.item.text == secret), "MEMBER recovered the primary secret via semantic recall — isolation breached");

        // ── The owner (operator) sees everything ───────────────────────────
        assert!(owner.is_operator());
        let o_secret = mem.beliefs_matching("safe combination", &mind_types::AccessContext::operator_audit()).await.unwrap();
        assert!(o_secret.iter().any(|b| b.statement == secret), "operator must retain full access");
        let o_secret_scoped = mem.beliefs_matching("safe combination", &member_ctx(Scope::primary())).await.unwrap();
        assert!(o_secret_scoped.iter().any(|b| b.statement == secret), "primary viewer must see their own private belief");

        // ── Path 3: explain_belief — out-of-scope belief is indistinguishable from absent ──
        assert!(mem.explain_belief(secret, &member).await.unwrap().is_none(), "MEMBER explained the primary secret — isolation breached");
        assert!(mem.explain_belief(shared, &member).await.unwrap().is_some(), "member must still explain shared beliefs");
        assert!(mem.explain_belief(secret, &owner).await.unwrap().is_some(), "operator must retain explain access");

        // ── Path 4: conflicts — a contradiction is visible only when BOTH sides are ──
        let secret_b = "The safe combination is 51-09-27";
        mem.remember_as_belief_scoped(
            BeliefAssertion { statement: secret_b.into(), polarity: 1.0, weight: 2.0, source_event: Some("test".into()), provenance: "told".into() },
            Scope::primary(),
        ).await.unwrap();
        mem.relate(secret, secret_b, "contradicts", 0.9).await.unwrap();
        let o_conflicts = mem.conflicts(&owner).await.unwrap();
        assert!(o_conflicts.iter().any(|c| c.belief_a.contains("safe combination")), "operator must see the private-belief conflict");
        let m_conflicts = mem.conflicts(&member).await.unwrap();
        assert!(
            !m_conflicts.iter().any(|c| c.belief_a.contains("safe combination") || c.belief_b.contains("safe combination")),
            "MEMBER saw the primary secret via the conflicts list — isolation breached"
        );

        // ── Path 5: reflect — beliefs, conflicts, goals, prefs all filtered ──
        mem.store_goal("buy the anniversary surprise").await.unwrap();
        let m_reflect = mem.reflect("safe combination", &member).await.unwrap();
        assert!(!m_reflect.beliefs.iter().any(|b| b.statement.contains("safe combination")), "MEMBER reflect surfaced the secret");
        assert!(!m_reflect.open_conflicts.iter().any(|c| c.belief_a.contains("safe combination")), "MEMBER reflect surfaced the secret conflict");
        assert!(m_reflect.goals.is_empty(), "goals are primary-private state — a member reflect must not carry them");
        let o_reflect = mem.reflect("anniversary", &owner).await.unwrap();
        assert!(!o_reflect.goals.is_empty(), "operator reflect must retain goals");

        // ── Path 6: hydrate_working_set — grounding + commitments filtered ──
        mem.add_task("wrap the safe-combination note", "high", None).await.unwrap();
        let m_ws = mem.hydrate_working_set("safe combination", &member).await.unwrap();
        assert!(!m_ws.stable_facts.iter().any(|f| f.text.contains("safe combination")), "MEMBER working set carried the secret");
        assert!(!m_ws.uncertain_beliefs.iter().any(|b| b.statement.contains("safe combination")), "MEMBER working set carried the secret (uncertain lane)");
        assert!(!m_ws.active_contradictions.iter().any(|c| c.belief_a.contains("safe combination")), "MEMBER working set carried the secret conflict");
        assert!(m_ws.commitments.is_empty(), "tasks are primary state — a member working set must not carry them");
        let o_ws = mem.hydrate_working_set("safe combination", &owner).await.unwrap();
        assert!(!o_ws.commitments.is_empty(), "operator working set must retain task commitments");

        // ── Path 7: transcript — a primary DM line never reaches a member view ──
        mem.append_message_scoped("user", "the gift is hidden in the garage", Scope::primary()).await.unwrap();
        mem.append_message_scoped("user", "dinner moved to eight", Scope::Shared).await.unwrap();
        let m_recent = mem.recent_messages(10, &member).await.unwrap();
        assert!(!m_recent.iter().any(|(_, t)| t.contains("garage")), "MEMBER read the primary transcript — isolation breached");
        assert!(m_recent.iter().any(|(_, t)| t.contains("dinner moved")), "member must still see shared-channel lines");
        let o_recent = mem.recent_messages(10, &owner).await.unwrap();
        assert!(o_recent.iter().any(|(_, t)| t.contains("garage")), "operator must retain the full transcript");
    }

    /// ARCH-1 slice 2 (d) + Purpose Gate v1: EVERY read — operator included — is
    /// receipted into a hash-chained, append-only ledger next to the DB, carrying
    /// its declared purpose; the chain verifies end-to-end. (The operator
    /// exemption is gone: the background lanes are exactly the reads a purpose
    /// audit exists to catch.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn every_read_leaves_a_hash_chained_receipt_with_its_purpose() {
        use mind_types::Scope;
        let db_path = scratch_db_path("receipts");
        let ledger_path = std::path::PathBuf::from(format!("{db_path}.read_receipts.jsonl"));
        let mem = MemoryHandle::spawn(&db_path, 8).unwrap();
        mem.remember_as_belief_scoped(
            BeliefAssertion { statement: "Dinner on Friday is at seven".into(), polarity: 1.0, weight: 2.0, source_event: None, provenance: "told".into() },
            Scope::Shared,
        ).await.unwrap();

        // Operator reads ARE receipted, named "operator", carrying their declared purpose.
        let op = mind_types::AccessContext::operator_audit();
        let _ = mem.beliefs_matching("dinner", &op).await.unwrap();
        let rs = receipts::read_ledger(&ledger_path);
        assert_eq!(rs.len(), 1, "an operator read must leave a receipt");
        assert_eq!(rs[0].principal, "operator");
        assert_eq!(rs[0].purpose.as_deref(), Some("audit→member:primary"), "the receipt carries the declared purpose");

        // Principal reads are receipted with their purpose — one per boundary crossing, chain intact.
        let member = member_ctx(Scope::Private("asha".into()));
        let _ = mem.beliefs_matching("dinner", &member).await.unwrap();
        let _ = mem.recall_typed(RecallQuery { text: "dinner".into(), top_k: 5, kind: None }, &member).await.unwrap();
        let _ = mem.conflicts(&member).await.unwrap();
        let rs = receipts::read_ledger(&ledger_path);
        assert!(rs.len() >= 4, "each read must leave a receipt (got {})", rs.len());
        assert!(rs[1..].iter().all(|r| r.principal == "private:asha"), "receipts must name the principal");
        assert!(rs[1..].iter().all(|r| r.purpose.as_deref() == Some("conversation→member:asha")), "every receipt carries its purpose");
        assert!(rs.iter().any(|r| r.method == "beliefs_matching" && r.detail.contains("dinner")));
        assert_eq!(receipts::verify_ledger(&ledger_path), Ok(rs.len()), "the receipt chain must verify");
        let _ = std::fs::remove_file(&ledger_path);
    }

    /// Belief lifecycle (organ #5): a tombstone carries its reason and the reason
    /// outlives the row — "user-deleted" stays forever distinguishable from
    /// hygiene — and statuses are DERIVED where the deriving context exists
    /// (a contradicted belief says so in hydration and reflection).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lifecycle_tombstones_carry_reasons_and_statuses_are_derived() {
        use mind_types::MemoryFacade;
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let a = "The dentist moved to Elm Street";
        let b = "The dentist stayed on Oak Avenue";
        for s in [a, b] {
            mem.remember_as_belief(BeliefAssertion {
                statement: s.into(), polarity: 1.0, weight: 2.0, source_event: Some("t".into()), provenance: "told".into(),
            }).await.unwrap();
        }
        mem.relate(a, b, "contradicts", 0.9).await.unwrap();

        // Derived status: reflection marks the conflicted side "contradicted".
        let ctx = mind_types::AccessContext::operator_audit();
        let refl = mem.reflect("dentist street", &ctx).await.unwrap();
        let conflicted: Vec<&Belief> = refl.beliefs.iter().filter(|x| x.statement == a || x.statement == b).collect();
        assert!(!conflicted.is_empty(), "the conflicted beliefs must reflect");
        assert!(conflicted.iter().all(|x| x.status == "contradicted"), "reflection must not report a conflicted belief as active: {conflicted:?}");

        // Tombstone with reason: the privacy path records "user-deleted"; a plain
        // forget records "unspecified" — and both survive as readable rows.
        assert!(mem.forget_with_reason(a, "user-deleted").await.unwrap());
        assert!(mem.forget(b).await.unwrap());
        let ts = mem.belief_tombstones().await.unwrap();
        assert!(ts.iter().any(|(p, r, _)| p == a && r == "user-deleted"), "{ts:?}");
        assert!(ts.iter().any(|(p, r, _)| p == b && r == "unspecified"), "{ts:?}");
        // The rows themselves are gone from recall.
        let hits = mem.beliefs_matching("dentist", &ctx).await.unwrap();
        assert!(hits.is_empty(), "tombstoned beliefs must not recall: {hits:?}");
    }

    /// Purpose Gate v1 red-team corpus — the vision's own acceptance metric:
    /// purpose-incompatible facts that are PRESENT, VISIBLE (to the operator
    /// capability), and HIGHLY RELEVANT (deterministic word-match queries) must
    /// produce ZERO unauthorized hydrations across the cross-owner and
    /// sensitive-class corpora — on every read path, in every background lane.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn purpose_gate_redteam_zero_unauthorized_hydrations() {
        use mind_types::{Activity, MemoryFacade, Purpose, PurposeGrantSpec, Scope, Sensitivity, Subject};
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let tell = |statement: &str, scope: Scope| {
            let a = BeliefAssertion { statement: statement.into(), polarity: 1.0, weight: 2.0, source_event: Some("corpus".into()), provenance: "told".into() };
            (a, scope)
        };
        // The corpus. Every fact is stored, scope-tagged, and reachable by exact word match.
        let asha_health = "Asha's oncology appointment is on July 18"; // cross-owner AND sensitive
        let primary_health = "Pranab's therapy session is on Tuesday evenings"; // sensitive, own
        let primary_finance = "Pranab's mortgage payment is 2400 a month"; // sensitive, own
        let primary_cred = "the garage code is 4921"; // credentials, own
        let household = "dinner on Friday is at seven"; // household ordinary
        let primary_ordinary = "Pranab prefers terse replies"; // own ordinary
        for (a, s) in [
            tell(asha_health, Scope::Private("asha".into())),
            tell(primary_health, Scope::primary()),
            tell(primary_finance, Scope::primary()),
            tell(primary_cred, Scope::primary()),
            tell(household, Scope::Shared),
            tell(primary_ordinary, Scope::primary()),
        ] {
            mem.remember_as_belief_scoped(a, s).await.unwrap();
        }
        let queries = ["oncology appointment July", "therapy session Tuesday", "mortgage payment month", "garage code 4921", "dinner Friday seven", "terse replies"];
        let forbidden_for_background = [asha_health, primary_health, primary_finance, primary_cred];

        // 1) EVERY background lane serving the primary: ordinary own/household facts hydrate,
        //    cross-owner and sensitive-class facts NEVER do — zero, on every read path.
        for activity in [Activity::Proactive, Activity::Research, Activity::Dream, Activity::Foresight, Activity::CodeWork, Activity::Recipe] {
            let lane = mind_types::AccessContext::operator(Purpose::serving_primary(activity));
            for q in queries {
                let hits = mem.beliefs_matching_n(q, 50, &lane).await.unwrap();
                for f in forbidden_for_background {
                    assert!(!hits.iter().any(|b| b.statement == f), "{activity:?} hydrated a purpose-incompatible fact via beliefs_matching({q}): {f}");
                }
                let recalled = mem.recall_typed(RecallQuery { text: q.into(), top_k: 50, kind: None }, &lane).await.unwrap();
                for f in forbidden_for_background {
                    assert!(!recalled.iter().any(|r| r.item.text == f), "{activity:?} hydrated a purpose-incompatible fact via recall_typed({q}): {f}");
                }
                let ws = mem.hydrate_working_set(q, &lane).await.unwrap();
                for f in forbidden_for_background {
                    assert!(!ws.stable_facts.iter().any(|i| i.text == f) && !ws.uncertain_beliefs.iter().any(|b| b.statement == f),
                        "{activity:?} hydrated a purpose-incompatible fact via working set({q}): {f}");
                }
            }
            // No existence oracle through explain either.
            for f in forbidden_for_background {
                assert!(mem.explain_belief(f, &lane).await.unwrap().is_none(), "{activity:?} explained a purpose-denied fact: {f}");
            }
            // The lane still WORKS: ordinary own + household facts hydrate.
            let ok = mem.beliefs_matching_n("terse replies", 50, &lane).await.unwrap();
            assert!(ok.iter().any(|b| b.statement == primary_ordinary), "{activity:?} must still hydrate the primary's ordinary facts");
            let ok2 = mem.beliefs_matching_n("dinner Friday", 50, &lane).await.unwrap();
            assert!(ok2.iter().any(|b| b.statement == household), "{activity:?} must still hydrate household facts");
        }

        // 2) Direct conversation with the owner: sensitive OWN facts answer ("what's my
        //    garage code?" is the product) — another member's private fact still never appears.
        let primary_convo = member_ctx(Scope::primary());
        for own in [primary_health, primary_finance, primary_cred, primary_ordinary] {
            let hits = mem.beliefs_matching_n(own.split_whitespace().take(3).collect::<Vec<_>>().join(" ").as_str(), 50, &primary_convo).await.unwrap();
            assert!(hits.iter().any(|b| b.statement == own), "the primary's own conversation must hydrate their own fact: {own}");
        }
        let leak = mem.beliefs_matching_n("oncology appointment July", 50, &primary_convo).await.unwrap();
        assert!(!leak.iter().any(|b| b.statement == asha_health), "Asha's private fact leaked into the primary's conversation");
        // And Asha's own conversation still answers Asha about her own health.
        let asha_convo = member_ctx(Scope::Private("asha".into()));
        let asha_hits = mem.beliefs_matching_n("oncology appointment July", 50, &asha_convo).await.unwrap();
        assert!(asha_hits.iter().any(|b| b.statement == asha_health), "Asha must be answered about her own health fact");

        // 3) A standing grant opens EXACTLY its crossing — and expiry/revocation close it.
        //    Grant: Asha's facts may serve the primary's Proactive work (gift planning).
        let gid = mem.grant_purpose(PurposeGrantSpec {
            owner: Subject::Member("asha".into()),
            beneficiary: Subject::primary(),
            class: None, // wildcard — which deliberately still excludes credentials
            activity: Some(Activity::Proactive),
            expires_ms: (receipts::now_ms()) + 60_000,
            note: "gift planning for Asha's birthday".into(),
        }).await.unwrap();
        let proactive = mind_types::AccessContext::operator(Purpose::serving_primary(Activity::Proactive));
        let granted = mem.beliefs_matching_n("oncology appointment July", 50, &proactive).await.unwrap();
        assert!(granted.iter().any(|b| b.statement == asha_health), "the standing grant must open the granted crossing");
        // The grant does NOT leak into other activities…
        let dream = mind_types::AccessContext::operator(Purpose::serving_primary(Activity::Dream));
        let dream_hits = mem.beliefs_matching_n("oncology appointment July", 50, &dream).await.unwrap();
        assert!(!dream_hits.iter().any(|b| b.statement == asha_health), "a Proactive grant must not open the Dream lane");
        // …and NEVER widens a principal's viewing scope (viewer isolation stays supreme).
        let still_walled = mem.beliefs_matching_n("oncology appointment July", 50, &primary_convo).await.unwrap();
        assert!(!still_walled.iter().any(|b| b.statement == asha_health), "a grant must never widen a principal's scope");
        // Revocation closes it again — zero hydrations, immediately.
        assert!(mem.revoke_purpose_grant(gid).await.unwrap());
        let revoked = mem.beliefs_matching_n("oncology appointment July", 50, &proactive).await.unwrap();
        assert!(!revoked.iter().any(|b| b.statement == asha_health), "a revoked grant must stop hydrating");
        let ledger = mem.list_purpose_grants().await.unwrap();
        assert!(ledger.iter().any(|g| g.id == gid && g.revoked), "the revoked grant must survive on the ledger (the audit story)");

        // 4) An explicit sensitivity correction beats the classifier, both directions.
        mem.set_belief_sensitivity(primary_ordinary, Sensitivity::Finance).await.unwrap();
        let now_denied = mem.beliefs_matching_n("terse replies", 50, &proactive).await.unwrap();
        assert!(!now_denied.iter().any(|b| b.statement == primary_ordinary), "an explicit Finance tag must deny the background lane");
        mem.set_belief_sensitivity(primary_health, Sensitivity::Ordinary).await.unwrap();
        let now_allowed = mem.beliefs_matching_n("therapy session Tuesday", 50, &proactive).await.unwrap();
        assert!(now_allowed.iter().any(|b| b.statement == primary_health), "an explicit Ordinary tag must override the classifier");

        // 5) Audit retains full visibility (and is receipted elsewhere).
        let audit_hits = mem.beliefs_matching_n("oncology appointment July", 50, &mind_types::AccessContext::operator_audit()).await.unwrap();
        assert!(audit_hits.iter().any(|b| b.statement == asha_health), "the audit lane must retain full visibility");
    }

    fn scratch_db_path(tag: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "ym_snap_{tag}_{}_{}.db",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        p.to_string_lossy().into_owned()
    }

    /// The immune-harness invariant: a snapshot is a faithful, independently
    /// openable copy, and seeding the COPY leaves the live mind untouched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn snapshot_to_copies_beliefs_and_seeding_copy_never_touches_live() {
        let live_path = scratch_db_path("live");
        let snap_path = scratch_db_path("copy");
        {
            let live = MemoryHandle::spawn(&live_path, 8).unwrap();
            live.remember_as_belief(BeliefAssertion {
                statement: "Asha's birthday is March 3".into(),
                polarity: 1.0,
                weight: 1.5,
                source_event: Some("test".into()),
                provenance: "told".into(),
            })
            .await
            .unwrap();
            live.snapshot_to(&snap_path).await.unwrap();

            // Seed a false belief into the COPY only.
            let copy = MemoryHandle::spawn(&snap_path, 8).unwrap();
            let seeded = copy
                .remember_as_belief(BeliefAssertion {
                    statement: "Asha's birthday is July 9".into(),
                    polarity: 1.0,
                    weight: 1.5,
                    source_event: Some("seed".into()),
                    provenance: "told".into(),
                })
                .await;
            assert!(seeded.is_ok(), "copy must accept writes");
            // Copy carried the genuine belief over.
            assert!(copy.explain_belief("Asha's birthday is March 3", &mind_types::AccessContext::operator_audit()).await.unwrap().is_some());
            // Live mind never saw the seed.
            assert!(live.explain_belief("Asha's birthday is July 9", &mind_types::AccessContext::operator_audit()).await.unwrap().is_none());
            assert!(live.explain_belief("Asha's birthday is March 3", &mind_types::AccessContext::operator_audit()).await.unwrap().is_some());
        }
        let _ = std::fs::remove_file(&live_path);
        let _ = std::fs::remove_file(&snap_path);
    }

    /// Guards: never overwrite an existing file (that's how a reversed argument
    /// would hit the live db), and :memory: minds have nothing to snapshot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn snapshot_to_refuses_existing_dest_and_memory_source() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        assert!(mem.snapshot_to(scratch_db_path("nomem")).await.is_err());

        let live_path = scratch_db_path("live2");
        let live = MemoryHandle::spawn(&live_path, 8).unwrap();
        let dest = scratch_db_path("exists");
        std::fs::write(&dest, b"occupied").unwrap();
        assert!(live.snapshot_to(&dest).await.is_err());
        let _ = std::fs::remove_file(&live_path);
        let _ = std::fs::remove_file(&dest);
    }

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
    async fn belief_surface_variants_are_normalized_and_deduplicated() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let assertion = |statement: &str| BeliefAssertion {
            statement: statement.into(),
            polarity: 1.0,
            weight: 1.0,
            source_event: None,
            provenance: "told".into(),
        };

        let first = mem.remember_as_belief(assertion("Exercise improves mood.  ")).await.unwrap();
        let second = mem.remember_as_belief(assertion("exercise improves mood")).await.unwrap();

        assert_eq!(first.statement, "Exercise improves mood");
        assert_eq!(second.id, first.id, "case and trailing punctuation must not create another belief");
        assert_eq!(mem.belief_count().await.unwrap(), 1);
        assert!(
            mem.conflicts(&mind_types::AccessContext::operator_audit()).await.unwrap().is_empty(),
            "surface variants are not contradictions"
        );
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

    /// Retro-dedup collapses trailing-punctuation and Jaccard paraphrases that were written BEFORE
    /// the write-path dedup existed (simulated with force_insert_goal_pref_raw bypass).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn retro_dedup_store_collapses_legacy_near_duplicates() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();

        // Trailing-punctuation / case variant (norm_prop dedup)
        mem.force_insert_goal_pref_raw("preference", "Exercise daily").await.unwrap();
        mem.force_insert_goal_pref_raw("preference", "exercise daily.").await.unwrap();

        // Jaccard paraphrase (≈ 0.71 word overlap — caught by the ≥0.6 threshold)
        mem.force_insert_goal_pref_raw("preference", "Prefers terse one-line summaries").await.unwrap();
        mem.force_insert_goal_pref_raw("preference", "Prefers terse, one-line summaries when possible").await.unwrap();

        // Distinct entry — must survive unchanged
        mem.force_insert_goal_pref_raw("preference", "Drink more water").await.unwrap();

        let before = mem.list_preferences().await.unwrap();
        assert_eq!(before.len(), 5, "force-insert bypassed dedup; all 5 rows present before retro pass");

        let (beliefs_merged, goals_prefs_removed) = mem.retro_dedup_store().await.unwrap();
        assert_eq!(beliefs_merged, 0, "no belief duplicates in a fresh DB");
        assert_eq!(goals_prefs_removed, 2, "one norm_prop dup + one Jaccard dup removed");

        let after = mem.list_preferences().await.unwrap();
        assert_eq!(after.len(), 3, "first occurrences and the distinct entry survive");
        let texts: Vec<&str> = after.iter().map(|p| p.text.as_str()).collect();
        assert!(texts.contains(&"Exercise daily"), "norm_prop survivor kept: {texts:?}");
        assert!(texts.contains(&"Prefers terse one-line summaries"), "Jaccard survivor kept: {texts:?}");
        assert!(texts.contains(&"Drink more water"), "distinct entry unchanged: {texts:?}");

        // Idempotency: a second pass on the already-clean store removes nothing more.
        let (b2, gp2) = mem.retro_dedup_store().await.unwrap();
        assert_eq!((b2, gp2), (0, 0), "second retro-dedup pass on clean store is a no-op");
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
        let hits = mem.recall_typed(RecallQuery { text: "latest stable Rust release".into(), top_k: 10, kind: None }, &mind_types::AccessContext::operator_audit()).await.unwrap();
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
        let wife_view = mem.recall_typed(q("birthday gift watch"), &member_ctx(wife.clone())).await.unwrap();
        assert!(!wife_view.iter().any(|r| r.item.text.contains("gold watch")), "LEAK: wife saw the surprise: {:?}", wife_view.iter().map(|r| &r.item.text).collect::<Vec<_>>());
        // Pranab MUST see his own private belief.
        let p_view = mem.recall_typed(q("birthday gift watch"), &member_ctx(primary.clone())).await.unwrap();
        assert!(p_view.iter().any(|r| r.item.text.contains("gold watch")), "primary must see his own private belief");
        // BOTH see the shared milk fact.
        assert!(mem.recall_typed(q("out of milk"), &member_ctx(wife.clone())).await.unwrap().iter().any(|r| r.item.text.contains("milk")), "wife sees shared");
        assert!(mem.recall_typed(q("out of milk"), &member_ctx(primary)).await.unwrap().iter().any(|r| r.item.text.contains("milk")), "primary sees shared");
        // The wife's GROUNDING (working set) must also exclude the gift — the LLM never even sees it.
        let ws = mem.hydrate_working_set("birthday gift watch", &member_ctx(wife)).await.unwrap();
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
            .recall_typed(RecallQuery { text: "reply style terse".into(), top_k: 5, kind: None }, &mind_types::AccessContext::operator_audit())
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

        let conflicts = mem.conflicts(&mind_types::AccessContext::operator_audit()).await.unwrap();
        assert!(!conflicts.is_empty(), "the contradiction should be detected");
        assert!(conflicts.iter().any(|c| c.belief_a.contains("terse") || c.belief_b.contains("terse")));

        // Explanation returns the belief with its evidence trail.
        let (belief, _ev) = mem
            .explain_belief("Pranab prefers terse replies", &mind_types::AccessContext::operator_audit())
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

    #[test]
    fn topical_relatedness_rejects_unrelated_subjects() {
        let threshold = 0.25;
        assert_eq!(parse_relatedness_threshold(None), threshold);
        assert_eq!(parse_relatedness_threshold(Some("0.7")), 0.7);
        assert_eq!(parse_relatedness_threshold(Some("2")), 1.0);
        let unrelated = topical_relatedness(
            "The Pacific Ocean is deep",
            "Rust 1.96 has improved diagnostics",
            Some(0.55),
        );
        assert!(
            unrelated < threshold,
            "background semantic similarity must not pass the topical gate: {unrelated}"
        );

        let same_subject = topical_relatedness(
            "Pranab prefers terse replies",
            "Pranab prefers long detailed replies",
            None,
        );
        assert!(
            same_subject >= threshold,
            "contradictory claims about the same subject must pass the topical gate: {same_subject}"
        );
    }

    #[test]
    fn topical_relatedness_threshold_cannot_disable_the_gate() {
        let threshold = parse_relatedness_threshold(Some("0"));
        assert_eq!(threshold, 0.25);
        assert!(
            topical_relatedness("The Pacific Ocean is deep", "Rust has improved diagnostics", None)
                < threshold
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn contradiction_scan_skips_unrelated_belief_pairs() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        for statement in [
            "The Pacific Ocean is deep",
            "Rust 1.96 has improved diagnostics",
            "Pranab sleeps early",
            "Pranab stays up late",
        ] {
            mem.remember_as_belief(BeliefAssertion {
                statement: statement.into(),
                polarity: 1.0,
                weight: 2.0,
                source_event: None,
                provenance: "told".into(),
            })
            .await
            .unwrap();
        }
        mem.relate(
            "The Pacific Ocean is deep",
            "Rust 1.96 has improved diagnostics",
            "contradicts",
            0.9,
        )
        .await
        .unwrap();
        mem.relate(
            "Pranab sleeps early",
            "Pranab stays up late",
            "contradicts",
            0.9,
        )
        .await
        .unwrap();

        let conflicts = mem.conflicts(&mind_types::AccessContext::operator_audit()).await.unwrap();
        assert_eq!(conflicts.len(), 1, "only the related pair should survive: {conflicts:?}");
        assert!(conflicts[0].belief_a.contains("Pranab"));
        assert!(conflicts[0].belief_b.contains("Pranab"));
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
        let ws = mem.hydrate_working_set("what's on my plate", &mind_types::AccessContext::operator_audit()).await.unwrap();
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
            .recall_typed(RecallQuery { text: "he likes short responses".into(), top_k: 1, kind: None }, &mind_types::AccessContext::operator_audit())
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
            .recall_typed(RecallQuery { text: "earth sun orbit".into(), top_k: 5, kind: None }, &mind_types::AccessContext::operator_audit())
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
            .recall_typed(RecallQuery { text: "earth sun orbit".into(), top_k: 5, kind: None }, &mind_types::AccessContext::operator_audit())
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
        let reflection = mem.reflect("sky colour", &mind_types::AccessContext::operator_audit()).await.unwrap();
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
        let reflection2 = mem.reflect("sky colour", &mind_types::AccessContext::operator_audit()).await.unwrap();
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
            .recall_typed(RecallQuery { text: "red flower".into(), top_k: 10, kind: None }, &mind_types::AccessContext::operator_audit())
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

    #[test]
    fn contradiction_config_sensitivity_knob() {
        // Default (s=0.5) must reproduce library defaults.
        let default = contradiction_config_from_env();
        assert!((default.min_confidence_for_conflict - 0.6).abs() < 1e-9);
        assert!((default.min_severity - 0.2).abs() < 1e-9);

        // High sensitivity (s=1.0): lower thresholds → more conflicts surfaced.
        std::env::set_var("YM_CONTRADICTION_SENSITIVITY", "1.0");
        let high = contradiction_config_from_env();
        assert!(high.min_confidence_for_conflict < default.min_confidence_for_conflict,
            "high sensitivity must lower min_confidence");
        assert!(high.min_severity < default.min_severity,
            "high sensitivity must lower min_severity");

        // Low sensitivity (s=0.0): higher thresholds → fewer conflicts surfaced.
        std::env::set_var("YM_CONTRADICTION_SENSITIVITY", "0.0");
        let low = contradiction_config_from_env();
        assert!(low.min_confidence_for_conflict > default.min_confidence_for_conflict,
            "low sensitivity must raise min_confidence");
        assert!(low.min_severity > default.min_severity,
            "low sensitivity must raise min_severity");

        // Out-of-range values are clamped, not panicked.
        std::env::set_var("YM_CONTRADICTION_SENSITIVITY", "9999.0");
        let clamped = contradiction_config_from_env();
        assert!((clamped.min_confidence_for_conflict - high.min_confidence_for_conflict).abs() < 1e-9,
            "values >1.0 must clamp to 1.0 behaviour");

        // Garbage value falls back to default.
        std::env::set_var("YM_CONTRADICTION_SENSITIVITY", "not_a_number");
        let fallback = contradiction_config_from_env();
        assert!((fallback.min_confidence_for_conflict - 0.6).abs() < 1e-9,
            "unparseable env var must fall back to default");

        // Clean up so other tests aren't affected.
        std::env::remove_var("YM_CONTRADICTION_SENSITIVITY");
    }

}
 
#[cfg(test)]
mod actor_ordering_tests {
    use super::*;

    /// READ-YOUR-WRITES THROUGH THE SINGLE QUEUE: a transcript write followed by a bulk read of
    /// the same table must observe the write. This is the causality the consolidation cursor
    /// depends on, and the property a naive two-lane design could have broken (kept from the
    /// lane experiment as the permanent regression lock for whatever scheduling comes later).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_bulk_read_sees_every_prior_write() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        mem.append_message_scoped("user", "the garage code is 4417", mind_types::Scope::Private("primary".into())).await.unwrap();
        let rows = mem.messages_since(0, 50).await.unwrap();
        assert!(
            rows.iter().any(|(_, _, t)| t.contains("4417")),
            "a bulk read must observe a prior write: {rows:?}"
        );
    }

    /// Same-caller ordering: writes issued before a bulk window are visible inside it; writes
    /// issued after are not required to be. Deterministic per caller — await-per-command is
    /// the ordering guarantee, independent of any future queue arrangement.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn same_caller_ordering_is_deterministic() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        for i in 0..5 {
            mem.append_message_scoped("user", &format!("msg {i}"), mind_types::Scope::Private("primary".into())).await.unwrap();
        }
        let rows = mem.messages_since(0, 50).await.unwrap();
        let ids: Vec<i64> = rows.iter().map(|(id, _, _)| *id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "bulk window returns ascending ids");
        assert_eq!(rows.len(), 5);
        // A write AFTER the consumed window must not retro-appear in it.
        mem.append_message_scoped("user", "later msg", mind_types::Scope::Private("primary".into())).await.unwrap();
        assert!(!rows.iter().any(|(_, _, t)| t.contains("later msg")));
    }

    /// Under concurrent load nothing is lost, every reply arrives exactly once, and the backlog
    /// gauge drains back to zero — the actor's termination and accounting discipline.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_load_loses_nothing_and_drains_to_zero() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let mut joins = Vec::new();
        for w in 0..8isize {
            let m = mem.clone();
            joins.push(tokio::spawn(async move {
                for i in 0..25 {
                    let txt = format!("worker {w} message {i}");
                    m.append_message_scoped("user", &txt, mind_types::Scope::Private("primary".into())).await.unwrap();
                }
            }));
        }
        for _ in 0..10 {
            let _ = mem.messages_since(0, 100).await.unwrap();
            let _ = mem.recent_messages(5, &mind_types::AccessContext::operator_audit()).await.unwrap();
        }
        for j in joins {
            j.await.unwrap();
        }
        let all = mem.messages_since(0, 10_000).await.unwrap();
        assert_eq!(all.len(), 200, "every write must land exactly once");
        let d = mem.backlog_depth();
        assert!(d.high_water >= 1, "the queue was exercised: {d:?}");
        assert_eq!(d.queued_or_running, 0, "backlog must drain to zero: {d:?}");
    }
}



 
#[cfg(test)]
mod lane_experiment {
    use super::*;

    /// LATENCY EXPERIMENT (#[ignore]: seeds a large store; run with `cargo test -p mind-memory
    /// experiment_bulk -- --ignored --nocapture`). Demonstrates the property the lanes exist
    /// for: while a bulk command (snapshot copy) is IN FLIGHT, an interactive read issued
    /// mid-flight completes in a fraction of the bulk op's duration. Run once against the
    /// two-lane pump and once against a forced single-FIFO classification (Cmd::lane() ->
    /// always Interactive) to record both sides of the comparison in the experiment ledger.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore]
    async fn experiment_bulk_op_does_not_delay_live_reads() {
        let dir = std::env::temp_dir().join(format!("ym_lane_exp_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("exp.db").to_string_lossy().to_string();
        let mem = MemoryHandle::spawn(&db_path, 8).unwrap();

        // Seed transcript rows until a snapshot copy clears ~80ms (bounded calibration).
        let body = "x".repeat(300);
        let mut batches = 0u32;
        loop {
            for i in 0..500 {
                mem.append_message_scoped("user", &format!("{batches}:{i} {body}"), mind_types::Scope::Private("primary".into())).await.unwrap();
            }
            batches += 1;
            let t0 = std::time::Instant::now();
            let probe = dir.join(format!("probe_{batches}.db"));
            mem.snapshot_to(probe.to_string_lossy().to_string()).await.unwrap();
            if t0.elapsed() >= std::time::Duration::from_millis(80) || batches >= 40 {
                break;
            }
        }

        // Start the bulk op; wait until its destination file appears (= copy genuinely running).
        let dest_path = dir.join("final.db");
        let m2 = mem.clone();
        let bg = tokio::spawn(async move {
            let t = std::time::Instant::now();
            m2.snapshot_to(dest_path.to_string_lossy().to_string()).await.unwrap();
            t.elapsed()
        });
        let dest = dir.join("final.db");
        let mut waited = std::time::Duration::ZERO;
        while !dest.exists() && waited < std::time::Duration::from_secs(5) {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            waited += std::time::Duration::from_millis(2);
        }
        assert!(dest.exists(), "bulk op never started");

        let t0 = std::time::Instant::now();
        let _ = mem.recent_messages(10, &mind_types::AccessContext::operator_audit()).await.unwrap();
        let interactive_dt = t0.elapsed();
        let bg_dt = bg.await.unwrap();

        println!(
            "EXPERIMENT seed_batches={batches} interactive_read={interactive_dt:?} background_total={bg_dt:?}"
        );
        assert!(
            interactive_dt < bg_dt / 2,
            "live read serialized behind bulk op: interactive {interactive_dt:?} vs bg {bg_dt:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}



