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

/// The belief lifecycle (One Mind vision, organ #5) — the states a belief can
/// occupy, replacing "deliberate forgetting" with typed transitions. Doctrine:
/// forgetting is a privacy right first, epistemic hygiene second, character
/// optimization never — and "acted-against" is NOT evidence of falsehood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BeliefStatus {
    Active,
    /// Confidence has decayed materially since last confirmation.
    Stale,
    /// An open contradiction names this belief on either side.
    Contradicted,
    /// A newer revision replaced it (the old row keeps its history).
    Superseded,
    /// Flagged out of active use pending human or new-evidence resolution —
    /// never set by the immune critic alone (its flags stay advisory).
    Quarantined,
    /// The user asked for it to be gone. Tombstoned with that reason.
    UserDeleted,
}

impl BeliefStatus {
    pub fn as_tag(&self) -> &'static str {
        match self {
            BeliefStatus::Active => "active",
            BeliefStatus::Stale => "stale",
            BeliefStatus::Contradicted => "contradicted",
            BeliefStatus::Superseded => "superseded",
            BeliefStatus::Quarantined => "quarantined",
            BeliefStatus::UserDeleted => "user-deleted",
        }
    }
    pub fn parse(tag: &str) -> BeliefStatus {
        match tag {
            "stale" => BeliefStatus::Stale,
            "contradicted" => BeliefStatus::Contradicted,
            "superseded" => BeliefStatus::Superseded,
            "quarantined" => BeliefStatus::Quarantined,
            "user-deleted" => BeliefStatus::UserDeleted,
            _ => BeliefStatus::Active,
        }
    }
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
    /// A `BeliefStatus` tag. Derived where the context to derive it exists
    /// (hydration and reflection set stale/contradicted); "active" elsewhere.
    pub status: String,
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

/// The authorization context a read/egress is performed under (ARCH-1, the
/// authorization kernel). Every personal-data read should carry one, so the
/// resource layer — not the channel — decides what is visible. `Operator`
/// (unscoped) is the privileged capability that only the trusted owner path
/// may mint; a `Principal` is filtered at the resource boundary and can
/// never see beyond its scope, whatever channel/command/tool/recipe it arrives
/// through.
///
/// Purpose Gate v1: BOTH variants carry a declared `Purpose` — there is
/// deliberately no way to construct a context without saying what the read
/// serves, so every call site's purpose is greppable and every receipt carries
/// it. For a `Principal`, scope stays supreme and the purpose can only narrow
/// further (sensitivity classes). For the `Operator`, the purpose is the ONLY
/// filter background lanes get — which is exactly the point: dream/proactive/
/// research reads used to be unfiltered and unreceipted; now they are
/// owner-locked to who they serve unless a standing grant opens a crossing.
#[derive(Debug, Clone)]
pub enum AccessContext {
    /// The explicit operator capability — reserved for the trusted owner path;
    /// never derive this from an untrusted channel. Sees past scope walls, but
    /// its reads are purpose-filtered (unless the purpose is Audit/Maintenance)
    /// and ALWAYS receipted.
    Operator { purpose: crate::purpose::Purpose },
    /// Access limited to what `scope` may see, then purpose-filtered on top.
    /// Enforced by the memory layer.
    Principal { scope: Scope, purpose: crate::purpose::Purpose },
}

impl AccessContext {
    /// A principal filtered to `scope`, reading for `purpose` — the standard
    /// context for any channel turn.
    pub fn principal(scope: Scope, purpose: crate::purpose::Purpose) -> AccessContext {
        AccessContext::Principal { scope, purpose }
    }
    /// The operator capability, reading for `purpose`.
    pub fn operator(purpose: crate::purpose::Purpose) -> AccessContext {
        AccessContext::Operator { purpose }
    }
    /// The operator's console/eval/verification lane — full visibility, always receipted.
    pub fn operator_audit() -> AccessContext {
        AccessContext::Operator { purpose: crate::purpose::Purpose::audit() }
    }
    /// The viewer scope for filtering: None for the operator (unfiltered),
    /// Some(scope) for a principal. Feeds `Scope::visible_to`.
    pub fn viewer(&self) -> Option<Scope> {
        match self {
            AccessContext::Operator { .. } => None,
            AccessContext::Principal { scope, .. } => Some(scope.clone()),
        }
    }
    /// True when this context is the privileged, unfiltered operator.
    pub fn is_operator(&self) -> bool {
        matches!(self, AccessContext::Operator { .. })
    }
    /// The declared purpose of this context's reads.
    pub fn purpose(&self) -> &crate::purpose::Purpose {
        match self {
            AccessContext::Operator { purpose } => purpose,
            AccessContext::Principal { purpose, .. } => purpose,
        }
    }
    /// A short label for sensitive-read receipts.
    pub fn principal_label(&self) -> String {
        match self {
            AccessContext::Operator { .. } => "operator".into(),
            AccessContext::Principal { scope: Scope::Shared, .. } => "shared".into(),
            AccessContext::Principal { scope: Scope::Private(o), .. } => format!("private:{o}"),
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

/// One mounted knowledge pack, as the operator needs to see it.
///
/// `trust` is carried verbatim from the engine rather than reduced to a boolean: "Signed" and
/// "Unsigned" mean integrity-proven-and-identity-known versus integrity-proven-only, and collapsing
/// that distinction is what would let a re-signed pack borrow someone else's reputation.
#[derive(Debug, Clone, PartialEq)]
pub struct PackBrief {
    pub id: String,
    pub name: String,
    pub version: String,
    pub origin: String,
    pub trust: String,
    pub rows: u64,
    /// The namespace the pack's rows live under — what scoped recall needs to reach them.
    pub namespace: Option<String>,
    /// Durably installed beside the database (returns on every restart), as opposed to a
    /// process-local mount that vanishes with the process. The distinction an operator needs
    /// before trusting that `unmount` actually removed anything.
    pub installed: bool,
    /// The sealed corpus digest (`blake3:…`): the identity that survives a rename or a re-sign,
    /// and the key any local evidence about this pack must be stored under — a re-sealed pack
    /// must never inherit its predecessor's track record.
    pub content_digest: Option<String>,
    /// What the publisher says the pack covers, verbatim from the manifest. Routing input, and
    /// the only thing a host can match a need against without mounting.
    pub coverage: Vec<String>,
    /// Retrieval settings the publisher MEASURED for this corpus (`sweep_retrieval.py`), or None
    /// when the pack predates them. The floor is the one the consumer must apply — see
    /// `recall_from_packs`; the engine signs it and does not apply it.
    pub recommended_top_k: Option<u32>,
    pub recommended_min_similarity: Option<f64>,
    /// The publisher's Ed25519 public key (hex) when the pack is signed. Identity, not trust:
    /// `trust` says whether THIS host recognises the key.
    pub signer: Option<String>,
}

/// The similarity floor applied to a pack whose manifest declares none.
///
/// `sweep_retrieval.py`'s default, and the value three published packs settled near (0.55–0.65).
/// The floor is not cosmetic: the substrate measured unconditional top-5 injection taking a
/// control set from 12/12 to 5/12 (`yantrikdb/docs/PACKS.md` §5b), and one pack's notes record
/// records at 0.565–0.569 being injected into "what is 17 multiplied by 23?" until the floor
/// moved to 0.6. A pack that declares its own measured floor overrides this.
pub const DEFAULT_PACK_SIMILARITY_FLOOR: f64 = 0.55;

/// The floor in force for a pack: the HOST WALL, raised by a valid publisher-measured floor and
/// never lowered by one.
///
/// A manifest is publisher data. It may make the host stricter about its own corpus (a pack that
/// measured 0.65 gets 0.65), but a declared 0.0 — sloppy or hostile — must not reopen the
/// attach-harm the wall exists to close: pack rules never outrank host policy, and this is the
/// first place that hierarchy is a number rather than a sentence. Non-finite or out-of-range
/// declarations are ignored, with the same result. (Codex's review of P.1.)
pub fn effective_pack_floor(declared: Option<f64>) -> f64 {
    match declared {
        Some(f) if f.is_finite() && (0.0..=1.0).contains(&f) => f.max(DEFAULT_PACK_SIMILARITY_FLOOR),
        _ => DEFAULT_PACK_SIMILARITY_FLOOR,
    }
}

/// One row recalled from a mounted knowledge pack, with the identity lineage needs: WHICH pack
/// (`pack_id` = `origin@version`) and WHICH record (`rid`) said it.
///
/// `similarity` is the raw cosine the floor is applied to. `score` is the engine's composite
/// (importance, recency, trust tier folded in) — it ranks hits and must never gate them: a
/// high-importance row with weak similarity is exactly the attach-harm case the floor exists for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackHit {
    pub pack_id: String,
    pub rid: String,
    pub text: String,
    pub score: f64,
    pub similarity: f64,
    pub namespace: String,
}

/// What pack recall DID with a candidate row — the same judgement, in the same order, that
/// decides what reaches a turn, so the probe never claims a reachability recall would not grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackDisposition {
    /// Reached the turn.
    Cleared,
    /// Similarity under the pack's floor in force.
    WithheldFloor,
    /// Over the publisher's per-pack `recommended_top_k`.
    WithheldPackCap,
    /// Cleared everything but arrived after the turn's overall limit was filled.
    WithheldLimit,
}

/// One candidate row as pack recall SAW it, with what happened to it: what an operator needs to
/// tell "off-coverage" from "the wall is too strict for this embedder" — a withheld row at 0.53
/// and a withheld row at 0.12 are different findings — and a floor-withheld row from a capped one.
/// An instrument for `ym pack probe`; never a prompt input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackProbe {
    pub pack_id: String,
    pub rid: String,
    pub text: String,
    pub score: f64,
    pub similarity: f64,
    /// The floor in force for this row's pack.
    pub floor: f64,
    pub disposition: PackDisposition,
}

impl PackProbe {
    pub fn cleared(&self) -> bool {
        self.disposition == PackDisposition::Cleared
    }
}

/// One pack the mind COULD lease: mounted, or sitting in the pack library as a file whose manifest
/// was read without mounting. The catalog the coverage router ranks over (ARCH-6 P.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackCatalogEntry {
    pub pack_id: String,
    pub path: String,
    pub content_digest: Option<String>,
    pub coverage: Vec<String>,
    /// The floor in force (host wall, raised by a valid declaration).
    pub floor: f64,
    pub mounted: bool,
    pub signer: Option<String>,
}

/// Why the coverage router did not lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbstainReason {
    /// Nothing in the catalog.
    NoPacks,
    /// The best pack's best phrase did not reach the coverage floor.
    BelowFloor,
    /// Two packs were too close to call.
    Tie,
}

/// The coverage router's answer for one query. SHADOWED in P.3: recorded, never acted on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PackRoute {
    Lease { pack_id: String, sim: f64, margin: f64 },
    Abstain { reason: AbstainReason, best: Option<(String, f64)> },
}

impl PackRoute {
    pub fn leased(&self) -> Option<&str> {
        match self {
            PackRoute::Lease { pack_id, .. } => Some(pack_id),
            PackRoute::Abstain { .. } => None,
        }
    }
    /// A short stable label for events and reports: `lease` / `abstain:below_floor` / …
    pub fn label(&self) -> &'static str {
        match self {
            PackRoute::Lease { .. } => "lease",
            PackRoute::Abstain { reason: AbstainReason::NoPacks, .. } => "abstain:no_packs",
            PackRoute::Abstain { reason: AbstainReason::BelowFloor, .. } => "abstain:below_floor",
            PackRoute::Abstain { reason: AbstainReason::Tie, .. } => "abstain:tie",
        }
    }
}

/// One pack's best coverage match for a query, with the phrase that earned it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoverageMatch {
    pub pack_id: String,
    pub sim: f64,
    pub phrase: String,
}

/// One thing that happened to a pack's evidence in a turn — the three rungs of the only ladder a
/// knowledge pack can climb locally: it was SURFACED into a turn, the reply USED it (a proxy, see
/// `pack_evidence_used`), and the person's next message GRADED the reply that used it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PackEvent {
    Surfaced,
    Used,
    Graded { good: bool },
}

/// A pack's local track record — counts, never a rate, because every rate here needs its own
/// denominator said aloud: `used` is out of `surfaced`, `graded` is out of `used`, and `good` is
/// out of `graded`. `graded < used` is CENSORING (no next message, or not the primary's turn), not
/// failure. Keyed by the pack's content digest underneath: a re-sealed pack starts from zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackStats {
    pub pack_id: String,
    pub content_digest: Option<String>,
    pub surfaced: u64,
    pub used: u64,
    pub graded: u64,
    pub good: u64,
    pub first_ms: i64,
    pub last_ms: i64,
}

#[async_trait]
pub trait MemoryFacade: Send + Sync {
    // ── ARCH-1 (slice 2) + Purpose Gate v1: EVERY personal-data read carries an
    // AccessContext, and every AccessContext carries a declared Purpose — there
    // is deliberately NO unscoped read API and NO purposeless context. A caller
    // that needs unfiltered access must write `AccessContext::operator_audit()`
    // (or `operator(purpose)` for a background lane) at the call site, so
    // unrestricted reads are greppable and can never happen by default. A
    // principal read is scope-filtered at the resource boundary (mind-memory),
    // then purpose-filtered (sensitivity classes / owner crossings), whatever
    // channel, command, model, recipe, or tool it arrives through. The old
    // fail-open `_as` defaults (scoped variant silently delegating to the
    // unscoped read) are gone: that inversion is the point of this slice.

    /// Typed + semantic + temporal recall (multi-signal), filtered to what `ctx` may see.
    async fn recall_typed(&self, q: RecallQuery, ctx: &AccessContext) -> Result<Vec<Recalled>>;

    /// Deterministic belief lookup: every belief whose statement contains any word (len>=4) of
    /// `needle`, case-insensitive. No semantic ranking — complete and exact, filtered to what
    /// `ctx` may see. Default: empty (fail-closed for impls that hold no beliefs).
    async fn beliefs_matching(&self, needle: &str, ctx: &AccessContext) -> Result<Vec<Belief>> {
        let _ = (needle, ctx);
        Ok(vec![])
    }

    /// Same as `beliefs_matching` with an explicit result cap — for namespaced knowledge bases
    /// (studied repos) where the default 20 would silently truncate. Default: empty.
    async fn beliefs_matching_n(&self, needle: &str, limit: usize, ctx: &AccessContext) -> Result<Vec<Belief>> {
        let _ = (needle, limit, ctx);
        Ok(vec![])
    }

    /// Assert evidence for/against a belief; runs Bayesian revision under the hood.
    async fn remember_as_belief(&self, a: BeliefAssertion) -> Result<Belief>;

    /// Assert belief evidence stamped with a monotonic `evidence_version`. A write whose version is
    /// not strictly greater than the last one applied to this belief is an out-of-order or replayed
    /// update and is dropped, so a stale evidence packet can never silently overwrite a fresher
    /// confidence score. Default: ignores the version (delegates to the unversioned path).
    async fn remember_as_belief_versioned(&self, a: BeliefAssertion, _evidence_version: u64) -> Result<Belief> {
        self.remember_as_belief(a).await
    }

    // ── scoped WRITES (reads are ctx-filtered above; writes tag visibility at ingest) ──
    /// Assert a belief tagged with a visibility `scope`. Default: ignores scope.
    async fn remember_as_belief_scoped(&self, a: BeliefAssertion, _scope: Scope) -> Result<Belief> {
        self.remember_as_belief(a).await
    }
    /// Append a transcript line tagged with a visibility `scope`. Default: ignores scope.
    async fn append_message_scoped(&self, role: &str, text: &str, _scope: Scope) -> Result<()> {
        self.append_message(role, text).await
    }
    /// Write a machine-derived OBSERVATION (skill/tool/sub-agent/web output) — provenance-tagged,
    /// secret-scanned, NEVER a naked Belief. This is the gated inward boundary for the moat.
    async fn remember_observation(&self, text: &str, source: crate::safety::ProvenanceCategory) -> Result<String>;
    /// Create/strengthen a graph edge between entities.
    async fn relate(&self, src: &str, dst: &str, rel: &str, weight: f64) -> Result<()>;
    /// Compose typed recalls + open conflicts into a structured reflection, filtered to `ctx`.
    async fn reflect(&self, question: &str, ctx: &AccessContext) -> Result<Reflection>;
    /// Currently-open contradictions across stored beliefs, filtered to `ctx` (a principal sees a
    /// conflict only when BOTH sides are visible to them — a contradiction that references another
    /// member's private belief would otherwise leak its text).
    async fn conflicts(&self, ctx: &AccessContext) -> Result<Vec<Contradiction>>;

    // ── tiny profile KV (name/purpose/onboarding) — durable, isolated from the cognitive graph ──
    /// Set a profile value (latest write wins on read).
    async fn profile_set(&self, key: &str, value: &str) -> Result<()>;
    /// Read the latest profile value for a key, or None.
    async fn profile_get(&self, key: &str) -> Result<Option<String>>;

    // ── tension economy (the "urges": drives emit substrate-grounded pressures; proactive arbitrates) ──
    /// Record a typed urge emitted by a drive (deduped on (kind, about) so it accrues, not floods).
    async fn record_tension(&self, kind: TensionKind, pressure: f64, about: &str) -> Result<()>;
    /// Open tensions, most URGENT first — nominal pressure decayed by age, so no fixed-pressure
    /// class can occupy the window forever (see `open_tensions_db` for the measured starvation
    /// this ordering fixes).
    async fn open_tensions(&self, limit: usize) -> Result<Vec<Tension>>;
    /// Close open urges older than their kind's shelf life as `expired`, bounding the ledger.
    /// Returns how many were expired. Curiosity ages out fast; contradictions are kept far longer.
    async fn expire_stale_tensions(&self, curiosity_days: i64, other_days: i64) -> Result<usize>;
    /// (discharged, expired) tension counts — the outcome ratio that says whether what the drives
    /// noticed was ever actually SEEN by anyone, or just aged out unread.
    async fn tension_outcome_counts(&self) -> Result<(usize, usize)>;
    /// Mark a tension discharged (resolved, or surfaced to the user).
    async fn discharge_tension(&self, id: &str) -> Result<bool>;
    /// A belief plus its evidence trail (provenance). A principal gets None for a belief outside
    /// their scope — indistinguishable from "no such belief" (no existence oracle).
    async fn explain_belief(&self, belief_id: &str, ctx: &AccessContext) -> Result<Option<(Belief, Vec<Evidence>)>>;
    /// Build the typed working-set for a focus/turn, filtered to what `ctx` may see.
    async fn hydrate_working_set(&self, focus: &str, ctx: &AccessContext) -> Result<WorkingSet>;
    /// Consolidate aging turns into typed memory (provenance-preserving). Returns #created.
    async fn consolidate(&self) -> Result<usize>;
    /// Privacy: forget a memory by id.
    async fn forget(&self, id: &str) -> Result<bool>;
    /// Forget WITH the reason on the tombstone — the lifecycle-honest path.
    /// A deletion whose reason is lost is indistinguishable from a dedup;
    /// "user-deleted" must stay distinguishable forever. Default: delegates
    /// to `forget` (reason dropped — real backends override).
    async fn forget_with_reason(&self, id: &str, _reason: &str) -> Result<bool> {
        self.forget(id).await
    }
    /// Every tombstone on record: (proposition, reason, ts_ms). The audit
    /// story for deletions — readable after the fact, unlike the row it marks.
    async fn belief_tombstones(&self) -> Result<Vec<(String, String, u64)>> {
        Ok(vec![])
    }
    /// Privacy: export everything (JSON). OPERATOR-INTERNAL: only the owner's eval/backup paths
    /// call this — it must never be wired to a channel/command/tool without an operator check.
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

    // ── attachable expertise: YantrikDB knowledge packs ────────────────────────────────────────
    //
    // Distinct from the mind's own capability packs (`ym pack install <json>`, which bundle banked
    // SKILLS and their evals). A `.ydbpack` is a sealed corpus plus a constitution: mounting one
    // gives the mind knowledge it can recall and rules it must follow, and unmounting gives them
    // back leaving the host byte-for-byte unchanged.
    //
    // Defaulted so a memory implementation without pack support — and every test double — keeps
    // compiling and honestly reports "no packs" rather than pretending.

    /// Mount a sealed pack for this process. Returns the pack id.
    async fn mount_pack(&self, _path: &str) -> Result<String> {
        Err(crate::MindError::Invalid("this memory backend has no pack support".into()))
    }
    /// Every banked approach (APPROACH:/PROCEDURE:-prefixed craft), newest first — a DETERMINISTIC
    /// enumeration, not similarity search. Exists because the loop's banked craft was write-only:
    /// stored as episodic memories, read back through a belief-only recall that could never see it.
    async fn list_approaches(&self, _limit: usize) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    /// Remove a durably-installed pack: unmount AND delete the installed file, so it does not
    /// silently return on the next restart (which is exactly what a plain unmount does).
    async fn uninstall_pack(&self, _id: &str) -> Result<bool> {
        Err(crate::MindError::Invalid("this memory backend has no pack support".into()))
    }
    /// Seal the mind's own banked craft — approaches it learned by doing, skills with their
    /// measured track records — into a mountable pack file. The self-improvement loop's EXPORT:
    /// what one mind earned becomes attachable expertise for another. Returns a one-line summary.
    async fn seal_learned_pack(&self, _dest: &str, _name: &str, _version: &str) -> Result<String> {
        Err(crate::MindError::Invalid("this memory backend has no pack support".into()))
    }
    /// Copy a pack beside the database and mount it on every open from now on.
    async fn install_pack(&self, _path: &str) -> Result<String> {
        Err(crate::MindError::Invalid("this memory backend has no pack support".into()))
    }
    /// Unmount by pack id or name.
    async fn unmount_pack(&self, _id_or_name: &str) -> Result<()> {
        Err(crate::MindError::Invalid("this memory backend has no pack support".into()))
    }
    /// What is mounted right now: (name, version, origin, trust, rows).
    async fn mounted_packs(&self) -> Result<Vec<PackBrief>> {
        Ok(Vec::new())
    }
    /// Recall from MOUNTED PACKS ONLY — never the host's own memories — floored and identified.
    ///
    /// Scoped deliberately. The engine's text recall is unscoped and would return every namespace in
    /// the database, including other household members' private facts; it would "work" for packs
    /// while quietly defeating read-isolation.
    ///
    /// Every hit has cleared its pack's own similarity floor (`recommended_min_similarity`, else
    /// [`DEFAULT_PACK_SIMILARITY_FLOOR`]) and carries the pack id and record id it came from. The
    /// floor is on SIMILARITY, not on the composite score — the composite folds in importance and
    /// trust, which is how a confident, irrelevant row gets injected into an arithmetic question.
    async fn recall_from_packs(&self, _query: &str, _top_k: usize) -> Result<Vec<PackHit>> {
        Ok(Vec::new())
    }
    /// The operator's view of the same recall: every attributed candidate, cleared or withheld, with
    /// the floor it was measured against. Read-only instrument; nothing here reaches a prompt.
    async fn probe_packs(&self, _query: &str, _top_k: usize) -> Result<Vec<PackProbe>> {
        Ok(Vec::new())
    }
    /// Count one rung of a pack's local ladder (the SQL witness beside the flight recorder's).
    async fn record_pack_event(&self, _pack_id: &str, _event: PackEvent) -> Result<()> {
        Ok(())
    }
    /// Every pack's local track record, most-surfaced first.
    async fn pack_stats(&self) -> Result<Vec<PackStats>> {
        Ok(Vec::new())
    }
    /// Point the pack LIBRARY at a directory of `.ydbpack` files whose manifests are read without
    /// mounting. Production sets it once from the environment; tests point it at a scratch dir.
    async fn set_pack_library(&self, _dir: &str) -> Result<()> {
        Ok(())
    }
    /// The catalog the router ranks over: every mounted pack plus every library file, one entry per
    /// pack id (a mounted pack wins over its library copy).
    async fn available_packs(&self) -> Result<Vec<PackCatalogEntry>> {
        Ok(Vec::new())
    }
    /// The coverage router's verdict for a query, with every pack's best match. Read-only: nothing
    /// is leased or mounted by asking.
    async fn route_packs(&self, _query: &str) -> Result<(Vec<CoverageMatch>, PackRoute)> {
        Ok((Vec::new(), PackRoute::Abstain { reason: AbstainReason::NoPacks, best: None }))
    }
    /// The constitution + coverage block the engine assembles for the system prompt, or None when
    /// nothing is mounted. The ENGINE owns this text so every consumer injects an identical block
    /// rather than five divergent hand-written versions.
    async fn pack_context(&self) -> Result<Option<String>> {
        Ok(None)
    }

    // ── cheap raw transcript (immediate conversational context; NOT knowledge) ──
    /// Append a raw chat line (role = "user" | "assistant").
    async fn append_message(&self, role: &str, text: &str) -> Result<()>;
    /// The most recent chat lines in chronological order: Vec<(role, text)>, filtered to `ctx`.
    async fn recent_messages(&self, limit: usize, ctx: &AccessContext) -> Result<Vec<(String, String)>>;
    /// Transcript lines with id > `after_id`, ascending: Vec<(id, role, text)>. For the consolidation
    /// pass, which advances a cursor over what it has already distilled into typed memory.
    /// OPERATOR-INTERNAL: only system paths (compaction, research sync) may call this — it is not
    /// reachable from any channel/command/tool. Gets a ctx param when those paths are ctx-threaded.
    async fn messages_since(&self, after_id: i64, limit: usize) -> Result<Vec<(i64, String, String)>>;
    /// Wall-clock times (ms) of USER turns at or after `since_ms`, ascending. The record of when
    /// the person actually spoke — the only honest way to settle an engagement claim after its
    /// window has closed. OPERATOR-INTERNAL, same as `messages_since`. Default: no record.
    async fn user_turn_times(&self, _since_ms: i64) -> Result<Vec<i64>> {
        Ok(Vec::new())
    }

    // ── Purpose Gate v1 (the read-boundary purpose policy; defaults = inert for fakes) ──
    /// Explicitly tag a belief's sensitivity class by canonical proposition —
    /// overrides the deterministic write-time classifier in either direction
    /// (a correction path: "that's not sensitive" / "treat that as health").
    async fn set_belief_sensitivity(&self, _proposition: &str, _class: crate::purpose::Sensitivity) -> Result<()> {
        Ok(())
    }
    /// Create a standing purpose grant — the ONLY way a cross-owner or
    /// out-of-policy sensitive-class read opens. Expiring and revocable.
    /// Returns the grant id. OPERATOR-INTERNAL: wire only to owner surfaces.
    async fn grant_purpose(&self, _spec: crate::purpose::PurposeGrantSpec) -> Result<i64> {
        Err(crate::MindError::Invalid("this memory backend has no purpose-grant support".into()))
    }
    /// Revoke a standing grant by id. Revocation is immediate and permanent.
    async fn revoke_purpose_grant(&self, _id: i64) -> Result<bool> {
        Ok(false)
    }
    /// Every grant on record, including revoked/expired ones (the audit story).
    async fn list_purpose_grants(&self) -> Result<Vec<crate::purpose::PurposeGrant>> {
        Ok(vec![])
    }

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
    /// Actor command-queue backlog as (queued_or_running, high_water_since_spawn).
    /// Default zeros keep fake/inert facades honest ("no queue" is the truthful report for a
    /// facade that has one). The real actor reports live gauges; surfaces use this to show
    /// whether memory work is outrunning the single-owner actor.
    fn backlog_depth(&self) -> (usize, usize) {
        (0, 0)
    }
    /// Feed a proactive send's fate (engaged vs ignored) into the engine's WORLD MODEL (per-time-bin
    /// engagement learning), personality feedback, and bond progression.
    async fn record_proactive_outcome(&self, _sent_ms: i64, _engaged: bool) -> Result<()> {
        Ok(())
    }
    /// Same world-model transition as `record_proactive_outcome`, WITHOUT the personality and
    /// bond nudges. For settling claims whose window closed long ago: the engagement record is
    /// historical fact worth learning from, but replaying six weeks of relationship steps in one
    /// batch is not what those nudges mean — 650 of them at once would bury the trait they move.
    async fn record_proactive_outcome_backfill(&self, _sent_ms: i64, _engaged: bool) -> Result<()> {
        Ok(())
    }
    /// This person's OVERALL engagement rate with proactive messages — the scale that a single
    /// moment's receptivity has to be read against. Default: unknown.
    async fn proactive_baseline_rate(&self) -> Result<Option<f64>> {
        Ok(None)
    }
    /// Predicted engagement rate for a proactive send RIGHT NOW (None until the world model has
    /// enough transitions to say anything real).
    async fn proactive_receptivity(&self) -> Result<Option<f64>> {
        Ok(None)
    }
    /// One compact line fusing the engine's relationship state — bond level + leading personality
    /// trait (how to SPEAK), the user's inferred current mode (what to MATCH), and any activity
    /// burst today (when to be extra concise). None when the engine has nothing yet.
    async fn relationship_lens(&self) -> Result<Option<String>> {
        Ok(None)
    }
    /// Total durable beliefs held (for the self-model panel — introspection must not undersell).
    async fn belief_count(&self) -> Result<u64> {
        Ok(0)
    }

    // ── engine demand (cognitive-urgency scoring for the proactive digest) ──────────────────────
    /// How urgently does the mind need to recall / verify the given topic? Derived from the
    /// cumulative confidence-deficit of matching beliefs: a topic backed by many uncertain or
    /// sparse beliefs scores closer to 1.0; a well-understood topic scores near 0.0. Returns [0,1].
    /// Default: 0.0 (no engine data — callers must degrade gracefully to raw pressure order).
    async fn recall_demand_for(&self, _about: &str) -> Result<f64> {
        Ok(0.0)
    }

    /// Engine demand — batch variant: one [0,1] demand score per entry in `topics`, in the same
    /// order. Default: delegates to `recall_demand_for` per entry; override for efficiency.
    async fn knowledge_gaps(&self, topics: &[String]) -> Result<Vec<f64>> {
        let mut out = Vec::with_capacity(topics.len());
        for t in topics {
            out.push(self.recall_demand_for(t).await?);
        }
        Ok(out)
    }
}
