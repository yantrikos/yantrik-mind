//! The tools, over one `MemoryHandle`.
//!
//! Two halves, one engine instance:
//!
//! - **Beliefs** — what a mind has come to hold true, with evidence and confidence. These go
//!   through this mind's own belief code, so scope, sensitivity, versioning and tombstones apply.
//! - **Memories** — flat records a mind writes and recalls by meaning, in a namespace.
//!
//! `recall` returns both at once, labelled, because a mind asking what it knows about something
//! should not have to know which half the answer was filed under.

use mind_memory::{MemoryHandle, MemoryHit, MemoryWrite};
use mind_types::{AccessContext, Belief, BeliefAssertion, MemoryFacade, RecallQuery};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::*;
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler};
use serde_json::{json, Value};

/// Every client is the machine's owner — see the module doc in main.rs for why.
fn owner() -> AccessContext {
    AccessContext::operator_audit()
}

fn ok(v: Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![Content::text(
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    )]))
}

fn fail(what: &str, e: impl std::fmt::Debug) -> McpError {
    McpError::internal_error(format!("{what} failed: {e:?}"), None)
}

fn belief_json(b: &Belief) -> Value {
    json!({
        "kind": "belief",
        "id": b.id,
        "statement": b.statement,
        "confidence": (b.confidence * 1000.0).round() / 1000.0,
        "evidence_count": b.evidence_count,
        "provenance": b.provenance,
        "status": b.status,
        "updated_ms": b.updated_ms,
    })
}

fn memory_json(m: &MemoryHit) -> Value {
    json!({
        "kind": "memory",
        "rid": m.rid,
        "text": m.text,
        "score": (m.score * 10000.0).round() / 10000.0,
        "memory_type": m.memory_type,
        "namespace": m.namespace,
        "domain": m.domain,
        "source": m.source,
        "created_at": m.created_at,
        "importance": m.importance,
        "metadata": m.metadata,
        "why_retrieved": m.why_retrieved,
    })
}

// ── Inputs ────────────────────────────────────────────────────────────────────────────

fn d_memory_type() -> String { "semantic".into() }
fn d_importance() -> f64 { 0.5 }
fn d_namespace() -> String { "default".into() }
fn d_domain() -> String { "general".into() }
fn d_source() -> String { "mcp".into() }
fn d_top_k() -> usize { 8 }
fn d_include() -> String { "all".into() }
fn d_direction() -> String { "supports".into() }
fn d_strength() -> f64 { 1.0 }
fn d_provenance() -> String { "told".into() }
fn d_limit() -> usize { 10 }
fn d_weight() -> f64 { 1.0 }

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RememberInput {
    /// What to remember, as a self-contained sentence.
    pub text: String,
    /// semantic (a fact), episodic (something that happened), procedural (how to do something).
    #[serde(default = "d_memory_type")]
    pub memory_type: String,
    /// 0.0 trivial to 1.0 critical.
    #[serde(default = "d_importance")]
    pub importance: f64,
    /// Whose memory this is. "default" is shared by every mind on this machine.
    #[serde(default = "d_namespace")]
    pub namespace: String,
    /// e.g. general, work, health, preference.
    #[serde(default = "d_domain")]
    pub domain: String,
    /// Who is writing: the mind's name, "user", "system".
    #[serde(default = "d_source")]
    pub source: String,
    /// Your own fields, returned with the memory on recall — e.g. `{"source": "extracted"}` to mark
    /// an unconfirmed guess. At most 16 KB.
    pub metadata: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RecallInput {
    /// A short natural-language question or topic.
    pub query: String,
    /// How many results from each half.
    #[serde(default = "d_top_k")]
    pub top_k: usize,
    /// Narrow flat memories to one namespace. Beliefs are not namespaced.
    pub namespace: Option<String>,
    /// all, beliefs, or memories.
    #[serde(default = "d_include")]
    pub include: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BelieveInput {
    /// The proposition, as a plain sentence.
    pub statement: String,
    /// supports or contradicts.
    #[serde(default = "d_direction")]
    pub direction: String,
    /// How strong this piece of evidence is, roughly 0.5 weak to 3.0 very strong.
    #[serde(default = "d_strength")]
    pub strength: f64,
    /// Where the evidence came from, e.g. "user said so on 2026-09-16".
    pub source: Option<String>,
    /// told, observed, inferred, extracted.
    #[serde(default = "d_provenance")]
    pub provenance: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BeliefsInput {
    /// Words the belief should contain.
    pub query: String,
    #[serde(default = "d_limit")]
    pub limit: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct StatementInput {
    /// The belief's statement, exactly as returned by `beliefs` or `recall`.
    pub statement: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ForgetInput {
    /// memory (by rid) or belief (by statement).
    pub kind: String,
    /// The memory's rid, or the belief's statement.
    pub target: String,
    /// Why — kept with the tombstone. Beliefs only.
    pub reason: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RelateInput {
    pub src: String,
    pub dst: String,
    /// e.g. works_on, lives_in, prefers.
    pub rel: String,
    #[serde(default = "d_weight")]
    pub weight: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReflectInput {
    /// What to reflect on.
    pub question: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NoInput {}

// ── Server ────────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct MemoryServer {
    mem: MemoryHandle,
    tool_router: ToolRouter<MemoryServer>,
}

impl MemoryServer {
    pub fn new(mem: MemoryHandle) -> Self {
        Self { mem, tool_router: Self::tool_router() }
    }
}

#[tool_router]
impl MemoryServer {
    #[tool(description = "Store a flat memory — a fact, an event, or a how-to — recalled later by meaning. Returns its rid.")]
    async fn remember(&self, Parameters(i): Parameters<RememberInput>) -> Result<CallToolResult, McpError> {
        let rid = self
            .mem
            .remember_memory(MemoryWrite {
                text: i.text,
                memory_type: i.memory_type,
                importance: i.importance.clamp(0.0, 1.0),
                namespace: i.namespace,
                domain: i.domain,
                source: i.source,
                metadata: i.metadata.map(Value::Object).unwrap_or(Value::Null),
            })
            .await
            .map_err(|e| {
                // The caller's to fix, so said as invalid params: an agent that reads every
                // internal error as "memory is down" would stop saving anything over one bad write.
                if e.is_memory_write_gate_refusal() || matches!(e, mind_types::MindError::Invalid(_)) {
                    McpError::invalid_params(format!("remember refused: {e}"), None)
                } else {
                    fail("remember", e)
                }
            })?;
        ok(json!({ "rid": rid, "status": "stored" }))
    }

    #[tool(description = "Recall what this machine knows about something: beliefs and memories together, each labelled with its kind. Call this before answering anything that depends on the person, their work, or earlier conversations.")]
    async fn recall(&self, Parameters(i): Parameters<RecallInput>) -> Result<CallToolResult, McpError> {
        let top_k = i.top_k.clamp(1, 50);
        let want_beliefs = matches!(i.include.as_str(), "all" | "beliefs");
        let want_memories = matches!(i.include.as_str(), "all" | "memories");

        let beliefs = if want_beliefs {
            self.mem
                .recall_typed(RecallQuery { text: i.query.clone(), top_k, kind: None }, &owner())
                .await
                .map_err(|e| fail("recall beliefs", e))?
        } else {
            Vec::new()
        };
        let memories = if want_memories {
            self.mem
                .recall_memories(&i.query, top_k, i.namespace.as_deref(), &owner())
                .await
                .map_err(|e| fail("recall memories", e))?
        } else {
            Vec::new()
        };

        // Interleaved by rank, not merged by score. The two halves are scored by different
        // rankers on different scales, and sorting one number against the other would present a
        // comparison that means nothing. Each half keeps its own order; each result carries its
        // own score and says which half it came from.
        let belief_rows: Vec<Value> = beliefs
            .iter()
            .map(|r| {
                json!({
                    "kind": "belief",
                    "id": r.item.id,
                    "statement": r.item.text,
                    "confidence": (r.item.confidence * 1000.0).round() / 1000.0,
                    "evidence_count": r.item.evidence_count,
                    "score": (r.score * 10000.0).round() / 10000.0,
                    "why": r.why,
                })
            })
            .collect();
        let memory_rows: Vec<Value> = memories.iter().map(memory_json).collect();
        let mut results = Vec::with_capacity(belief_rows.len() + memory_rows.len());
        let (mut b, mut m) = (belief_rows.into_iter(), memory_rows.into_iter());
        loop {
            let (nb, nm) = (b.next(), m.next());
            if nb.is_none() && nm.is_none() {
                break;
            }
            results.extend(nb);
            results.extend(nm);
        }
        ok(json!({ "query": i.query, "count": results.len(), "results": results }))
    }

    #[tool(description = "Add evidence for or against a belief. Creates the belief if it is new; otherwise updates its confidence. Use for things the person has told you or you have established, not for passing remarks.")]
    async fn believe(&self, Parameters(i): Parameters<BelieveInput>) -> Result<CallToolResult, McpError> {
        let polarity = match i.direction.as_str() {
            "supports" => 1.0,
            "contradicts" => -1.0,
            other => {
                return Err(McpError::invalid_params(
                    format!("direction must be supports or contradicts, got `{other}`"),
                    None,
                ))
            }
        };
        let belief = self
            .mem
            .remember_as_belief(BeliefAssertion {
                statement: i.statement,
                polarity,
                weight: i.strength.clamp(0.0, 6.0),
                source_event: i.source,
                provenance: i.provenance,
            })
            .await
            .map_err(|e| fail("believe", e))?;
        ok(belief_json(&belief))
    }

    #[tool(description = "List beliefs whose statement contains these words, most confident first.")]
    async fn beliefs(&self, Parameters(i): Parameters<BeliefsInput>) -> Result<CallToolResult, McpError> {
        let found = self
            .mem
            .beliefs_matching_n(&i.query, i.limit.clamp(1, 100), &owner())
            .await
            .map_err(|e| fail("beliefs", e))?;
        ok(json!({ "count": found.len(), "beliefs": found.iter().map(belief_json).collect::<Vec<_>>() }))
    }

    #[tool(description = "Show a belief with every piece of evidence behind it.")]
    async fn explain(&self, Parameters(i): Parameters<StatementInput>) -> Result<CallToolResult, McpError> {
        match self.mem.explain_belief(&i.statement, &owner()).await.map_err(|e| fail("explain", e))? {
            Some((belief, evidence)) => ok(json!({
                "belief": belief_json(&belief),
                "evidence": evidence.iter().map(|e| json!({
                    "weight": e.weight,
                    "polarity": e.polarity,
                    "source": e.source_event,
                    "excerpt": e.excerpt,
                })).collect::<Vec<_>>(),
            })),
            None => ok(json!({ "found": false, "statement": i.statement })),
        }
    }

    #[tool(description = "Forget a memory (by rid) or a belief (by statement). Nothing is erased: it is tombstoned and stops being recalled.")]
    async fn forget(&self, Parameters(i): Parameters<ForgetInput>) -> Result<CallToolResult, McpError> {
        let done = match i.kind.as_str() {
            "memory" => self.mem.forget_memory(&i.target).await.map_err(|e| fail("forget memory", e))?,
            "belief" => match i.reason.as_deref() {
                Some(r) => self.mem.forget_with_reason(&i.target, r).await,
                None => self.mem.forget(&i.target).await,
            }
            .map_err(|e| fail("forget belief", e))?,
            other => {
                return Err(McpError::invalid_params(
                    format!("kind must be memory or belief, got `{other}`"),
                    None,
                ))
            }
        };
        ok(json!({ "forgotten": done, "kind": i.kind, "target": i.target }))
    }

    #[tool(description = "Open contradictions between beliefs, most severe first.")]
    async fn conflicts(&self, Parameters(_): Parameters<NoInput>) -> Result<CallToolResult, McpError> {
        let found = self.mem.conflicts(&owner()).await.map_err(|e| fail("conflicts", e))?;
        ok(json!({ "count": found.len(), "conflicts": found }))
    }

    #[tool(description = "Record a relationship between two things, e.g. Pranab --lives_in--> Bentonville.")]
    async fn relate(&self, Parameters(i): Parameters<RelateInput>) -> Result<CallToolResult, McpError> {
        self.mem
            .relate(&i.src, &i.dst, &i.rel, i.weight)
            .await
            .map_err(|e| fail("relate", e))?;
        ok(json!({ "related": true, "src": i.src, "rel": i.rel, "dst": i.dst }))
    }

    #[tool(description = "Reflect on a question: the relevant beliefs, open conflicts, goals and preferences, summarised.")]
    async fn reflect(&self, Parameters(i): Parameters<ReflectInput>) -> Result<CallToolResult, McpError> {
        let r = self.mem.reflect(&i.question, &owner()).await.map_err(|e| fail("reflect", e))?;
        ok(json!({
            "summary": r.summary,
            "beliefs": r.beliefs.iter().map(belief_json).collect::<Vec<_>>(),
            "open_conflicts": r.open_conflicts,
            "goals": r.goals.iter().map(|g| &g.text).collect::<Vec<_>>(),
            "preferences": r.preferences.iter().map(|p| &p.text).collect::<Vec<_>>(),
        }))
    }
}

#[tool_handler]
impl ServerHandler for MemoryServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("yantrik-memory", env!("CARGO_PKG_VERSION")))
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "This is the machine's memory, shared by every mind that runs on it. What you \
                 store here, the next mind will know.\n\n\
                 - Before answering anything that depends on the person, their work, or an earlier \
                 conversation, call `recall`. It returns beliefs and memories together.\n\
                 - When the person tells you something durable about themselves, their work or \
                 their preferences, call `believe` with direction supports.\n\
                 - When something happens or you learn a fact worth keeping, call `remember`.\n\
                 - When the person corrects you, call `believe` with direction contradicts on the \
                 old statement, then `believe` the new one.",
            )
    }
}
