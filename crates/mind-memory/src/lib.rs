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
use tokio::sync::{mpsc, oneshot};

use mind_types::{
    Belief, BeliefAssertion, Contradiction, Evidence as MEvidence, MemoryFacade, MemoryItem,
    MemoryKind, MindError, RecallQuery, Recalled, Reflection, Result, WorkingSet,
};

use yantrikdb_core::belief::{BeliefRevisionConfig, Evidence as YEvidence};
use yantrikdb_core::belief_query::BeliefPattern;
use yantrikdb_core::contradiction::ContradictionConfig;
use yantrikdb_core::state::{
    sigmoid, BeliefPayload, CognitiveEdge, CognitiveEdgeKind, CognitiveNode, NodeId,
    NodeIdAllocator, NodeKind, NodePayload, Provenance,
};
use yantrikdb_core::YantrikDB;

type Reply<T> = oneshot::Sender<std::result::Result<T, String>>;

enum Cmd {
    Record { text: String, reply: Reply<String> },
    GetText { rid: String, reply: Reply<Option<String>> },
    AssertBelief { statement: String, signed_weight: f64, source: String, provenance: String, reply: Reply<Belief> },
    RecallTyped { text: String, top_k: usize, reply: Reply<Vec<Recalled>> },
    Conflicts { reply: Reply<Vec<Contradiction>> },
    Explain { statement: String, reply: Reply<Option<(Belief, Vec<MEvidence>)>> },
    Relate { src: String, dst: String, rel: String, weight: f64, reply: Reply<()> },
    Forget { statement: String, reply: Reply<bool> },
    Export { reply: Reply<String> },
}

// ── pure helpers (run on the actor thread, with &YantrikDB) ──────────────────

fn now_secs() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
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

fn recall_beliefs(db: &YantrikDB, text: &str, top_k: usize) -> Vec<Recalled> {
    let qwords: Vec<String> = text.to_ascii_lowercase().split_whitespace().map(|w| w.to_string()).collect();
    let mut scored: Vec<(f64, CognitiveNode)> = all_beliefs(db)
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
            item: MemoryItem {
                id: node_prop(&n).unwrap_or("").to_string(),
                kind: MemoryKind::Belief,
                text: node_prop(&n).unwrap_or("").to_string(),
                confidence: n.attrs.confidence,
                certainty: n.attrs.confidence,
                updated_ms: n.attrs.last_updated_ms,
            },
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
                let mut alloc = db.load_node_id_allocator().unwrap_or_else(|_| NodeIdAllocator::new());
                let zero = vec![0.0f32; dim];
                let meta = serde_json::json!({});
                while let Some(cmd) = rx.blocking_recv() {
                    match cmd {
                        Cmd::Record { text, reply } => {
                            let r = db.record(&text, "episodic", 0.5, 0.0, 604_800.0, &meta, &zero, "default", 0.8, "general", "user", None).map_err(|e| e.to_string());
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
}

#[async_trait]
impl MemoryFacade for MemoryHandle {
    async fn recall_typed(&self, q: RecallQuery) -> Result<Vec<Recalled>> {
        let (text, top_k) = (q.text, q.top_k);
        self.call(|reply| Cmd::RecallTyped { text, top_k, reply }).await
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
            summary: format!("{} relevant beliefs, {} open conflicts", beliefs.len(), open_conflicts.len()),
            beliefs,
            open_conflicts,
        })
    }

    async fn conflicts(&self) -> Result<Vec<Contradiction>> {
        self.call(|reply| Cmd::Conflicts { reply }).await
    }

    async fn explain_belief(&self, belief_id: &str) -> Result<Option<(Belief, Vec<MEvidence>)>> {
        let statement = belief_id.to_string();
        self.call(|reply| Cmd::Explain { statement, reply }).await
    }

    async fn hydrate_working_set(&self, focus: &str) -> Result<WorkingSet> {
        let recalled = self.recall_typed(RecallQuery { text: focus.to_string(), top_k: 8, kind: None }).await?;
        let open = self.conflicts().await?;
        let mut ws = WorkingSet::default();
        for r in recalled {
            if r.item.confidence >= 0.7 {
                ws.stable_facts.push(r.item);
            } else {
                ws.uncertain_beliefs.push(Belief {
                    id: r.item.id.clone(),
                    statement: r.item.text.clone(),
                    confidence: r.item.confidence,
                    certainty: r.item.certainty,
                    provenance: "recalled".into(),
                    evidence_count: 0,
                    updated_ms: r.item.updated_ms,
                    status: "active".into(),
                });
            }
        }
        ws.active_contradictions = open;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn actor_round_trips_a_write_then_read() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let rid = mem.record("the sky is blue").await.unwrap();
        assert_eq!(mem.get_text(&rid).await.unwrap().as_deref(), Some("the sky is blue"));
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
}
