//! mind-observability — the cognitive flight recorder.
//!
//! For meaningful decisions, preserve enough to reconstruct later: what Yantrik knew, what it
//! retrieved, what goal was active, what alternatives existed, why an action was chosen, which
//! policy checks ran, what was predicted, what actually happened, what changed because of that.
//!
//! # Non-negotiables (ARCH-5 §G.4)
//!
//! 1. **Append-only JSONL, hash-chained** — the same discipline as the read-receipt ledger
//!    (`mind-memory/src/receipts.rs`) and the immune trial ledger: each line is
//!    `{"chain":"<hex>","event":{…}}` where `chain = sha256(prev_hex ++ event_json)`, first line
//!    chains off `"genesis"`. Any edit/reorder/deletion of a middle line breaks every later hash.
//! 2. **Not another source of truth.** The recorder OBSERVES decisions; memory, the task store,
//!    packet KV, the judgment ledger remain authoritative. Nothing reads the flight recorder to
//!    decide anything.
//! 3. **Observability failure must never fail cognition.** [`DecisionLog::record`] cannot return
//!    an error to its caller: on the first write failure it loudly logs once and goes inert
//!    (fail-sticky, so a broken disk produces one warning, not one per turn).
//! 4. **No secrets, minimal private content.** Every free-text field passes through
//!    redaction: secret-shaped content is replaced by `[redacted-secret]` (the same detector
//!    that guards memory writes), and fields are truncated to a stated budget. IDs over raw
//!    text wherever an ID suffices.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static EVENT_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One recorded decision event. Every field except `trace_id`/`ts_ms`/`kind` is optional —
/// a recorder that demands completeness gets fed lies; this one records what exists.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecisionEvent {
    /// Correlates all events of one decision path. A uuid7 (time-ordered) or any caller-chosen id;
    /// `ym why <prefix>` matches by prefix so callers can use short ids.
    pub trace_id: String,
    pub ts_ms: u64,
    /// What kind of decision this was: `cognitive_run` | `packet_created` | `packet_resolved` |
    /// `reflex_enqueued` | `prediction_graded` | `selfbuild_deployed` | … Free-form but stable.
    pub kind: String,
    /// Which organ acted: `cognition` | `proactive` | `reflex` | `foresight` | `selfbuild` | …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Execution lane within an organ: `primary` | `member` | `sweep` | … Kept separate from
    /// `actor` so reports can compare lanes without erasing which organ made the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// Whose work this serves (the purpose gate's beneficiary label when known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The declared purpose label (e.g. `conversation→member:primary`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Opaque, deterministic fingerprint of the decision context. Equal fingerprints mean the same
    /// caller-supplied context bytes; the source text itself never enters the ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_fingerprint: Option<String>,
    /// Stable identity of the compiled goal. Unlike `goal` text, this survives wording changes and
    /// joins tool spans, completion grades, and later outcome evidence across turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    /// Active goal or commitment — BRIEF (truncated); enough to answer "why", not a data copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// What triggered this decision (due date, user message shape, tension id…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    /// Evidence/citation ids the decision rested on — IDs, not contents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_ids: Vec<String>,
    /// Alternatives that were considered (short labels).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    /// What was chosen (short label: tool name, stop reason, action id…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen: Option<String>,
    /// Alternatives explicitly rejected, with the reason where cheap: `["web_search (unavailable)"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rejected: Vec<String>,
    /// Policy verdicts that ran: `harm-gate:allow`, `purpose:allow(suppressed=2)`, `egress:mediated`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy: Vec<String>,
    /// What was predicted to happen (brief proposition).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicted: Option<String>,
    /// Confidence attached to that prediction, `0..=1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    /// What actually happened (brief observed result).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// Verdict on the outcome: `hit` | `miss` | `partial` | `engaged` | `ignored` | `rejected`…
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// How wrong the prediction was, when quantifiable (signed error vs threshold/target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prediction_error: Option<f64>,
    /// Brier loss for graded binary predictions: (predicted_probability − observed)². Signed
    /// error is diagnostic; Brier is the calibration metric — it separates "confidently right"
    /// from "accidentally right" and buckets cleanly by confidence band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brier: Option<f64>,
    /// Semantic success, when determinable: the tool ran AND its output carried substance.
    /// `execution_success` is implied by a graded verdict; this field begins separating
    /// "executed" from "useful" so today's binary never hardens into the definition of
    /// capability. None = not assessable for this outcome class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_success: Option<bool>,
    /// Wall-clock time spent executing the observed tool call. This excludes model planning and
    /// post-call grading so latency comparisons describe the capability boundary itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Version of the tool runtime/dispatcher that executed or classified this call. Model version
    /// is tracked separately when available; neither should be inferred from actor names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_version: Option<String>,
    /// Configured inference route for the decision (for example `chat=ollama-local:model`). This is
    /// deliberately a route, not an assertion about which fallback link actually served the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_route: Option<String>,
    /// Model-call attempts across a bounded cognitive run: decisions, replans, synthesis, and a
    /// configured grounding pass. This excludes compilation and work outside that run, so it is a
    /// resource proxy rather than token or monetary cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_calls: Option<u32>,
    /// Stable identity of the mechanism or authority that assigned the outcome grade. This is not
    /// the actor: a tool may act while a versioned classifier, user, or external evaluator grades.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_id: Option<String>,
    /// What changed because of this outcome (belief revised, skill quarantined, policy adjusted…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lesson: Option<String>,
    // ── SPAN LINKAGE ────────────────────────────────────────────────────────────
    // A trace label groups events; these three make them a CAUSAL TREE. event_id names this
    // span (so others can parent to it); parent_event_id points at the decision that caused
    // this one; object_id names the durable thing the event is about (a packet id, a task id,
    // a prediction ref) so `ym why pkt:…` reconstructs the life of an OBJECT across traces.
    // Sensitive source values must be represented with [`opaque_id`] before they reach the recorder.
    // All optional — v1 events without parents are roots of their trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
}

impl DecisionEvent {
    /// Start an event with only the mandatory trio filled.
    pub fn new(trace_id: impl Into<String>, kind: &str) -> Self {
        Self {
            trace_id: trace_id.into(),
            ts_ms: now_ms(),
            kind: kind.to_string(),
            actor: None,
            lane: None,
            subject: None,
            purpose: None,
            context_fingerprint: None,
            goal_id: None,
            goal: None,
            trigger: None,
            evidence_ids: Vec::new(),
            candidates: Vec::new(),
            chosen: None,
            rejected: Vec::new(),
            policy: Vec::new(),
            predicted: None,
            confidence: None,
            outcome: None,
            verdict: None,
            prediction_error: None,
            brier: None,
            semantic_success: None,
            latency_ms: None,
            tool_version: None,
            model_route: None,
            model_calls: None,
            evaluator_id: None,
            lesson: None,
            event_id: None,
            parent_event_id: None,
            object_id: None,
        }
    }

    /// A named span under `trace`, parented to `parent_event_id` when the caller knows it —
    /// how a turn becomes a tree (interpretation → plan → packet → tool-call → learning)
    /// rather than a flat list sharing a label. Generated event IDs include time, process, and
    /// sequence components so concurrent spans of the same kind remain distinct.
    pub fn span(trace_id: impl Into<String>, parent: Option<&str>, kind: &str) -> Self {
        let mut e = Self::new(trace_id, kind);
        let sequence = EVENT_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        e.event_id = Some(format!(
            "{kind}-{}-{}-{sequence}",
            now_ms(),
            std::process::id()
        ));
        e.parent_event_id = parent.map(String::from);
        e
    }

    /// Apply the redaction budget to every free-text field. Called by the log on append, so
    /// callers may pass human text without leaking responsibility for scanning it themselves.
    fn sanitized(mut self) -> Self {
        let b = |s: &str| brief(s, 160);
        self.trace_id = b(&self.trace_id);
        self.kind = brief(&self.kind, 48);
        self.actor = self.actor.map(|x| brief(&x, 32)).filter(|x| !x.is_empty());
        self.lane = self.lane.map(|x| brief(&x, 24)).filter(|x| !x.is_empty());
        self.subject = self.subject.map(|x| brief(&x, 64));
        self.purpose = self.purpose.map(|x| brief(&x, 64));
        self.context_fingerprint = self
            .context_fingerprint
            .map(|x| brief(&x, 64))
            .filter(|x| !x.is_empty());
        self.goal_id = self
            .goal_id
            .map(|x| brief(&x, 96))
            .filter(|x| !x.is_empty());
        self.goal = self.goal.map(|x| b(&x));
        self.trigger = self.trigger.map(|x| b(&x));
        self.chosen = self.chosen.map(|x| brief(&x, 120));
        self.predicted = self.predicted.map(|x| b(&x));
        self.outcome = self.outcome.map(|x| b(&x));
        self.verdict = self.verdict.map(|x| brief(&x, 24));
        self.lesson = self.lesson.map(|x| b(&x));
        self.event_id = self.event_id.map(|x| b(&x)).filter(|x| !x.is_empty());
        self.parent_event_id = self
            .parent_event_id
            .map(|x| b(&x))
            .filter(|x| !x.is_empty());
        self.object_id = self.object_id.map(|x| b(&x)).filter(|x| !x.is_empty());
        self.evaluator_id = self
            .evaluator_id
            .map(|x| brief(&x, 64))
            .filter(|x| !x.is_empty());
        self.tool_version = self
            .tool_version
            .map(|x| brief(&x, 64))
            .filter(|x| !x.is_empty());
        self.model_route = self
            .model_route
            .map(|x| brief(&x, 160))
            .filter(|x| !x.is_empty());
        // Invalid floating-point measurements must not poison JSON persistence or silently land in
        // a misleading calibration band. Dropping them preserves "unknown" instead of inventing a
        // clamped observation.
        self.confidence = self.confidence.filter(|value| valid_probability(*value));
        self.prediction_error = self.prediction_error.filter(|value| value.is_finite());
        self.brier = self.brier.filter(|value| valid_probability(*value));
        for v in [&mut self.candidates, &mut self.rejected, &mut self.policy] {
            for item in v.iter_mut() {
                *item = brief(item, 120);
            }
        }
        for id in &mut self.evidence_ids {
            *id = b(id);
        }
        self.evidence_ids.retain(|id| !id.trim().is_empty());
        self
    }
}

/// Redact + truncate one free-text field. Secret-shaped content never enters the ledger even
/// truncated — the detector is the SAME function guarding memory writes (one source of truth).
fn brief(text: &str, max_chars: usize) -> String {
    let mut s = if mind_types::contains_secret(text) {
        "[redacted-secret]".to_string()
    } else {
        text.trim().to_string()
    };
    if s.chars().count() > max_chars {
        s = s.chars().take(max_chars).collect::<String>() + "…";
    }
    s
}

fn valid_probability(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

#[derive(Serialize, Deserialize)]
struct ChainedLine {
    chain: String,
    event: DecisionEvent,
}

/// Health state for the recorder's write path. Observability must never crash cognition, but
/// PERMANENTLY disabling on one transient error would create a long invisible period — the
/// opposite failure. So: first failure marks unhealthy with a backoff window; during the window
/// `record` is a silent no-op; after it expires the next record RETRIES; success resets to
/// healthy. Cognition is unaffected in every state.
struct RecorderHealth {
    consecutive_failures: u32,
    last_failure_ms: Option<u64>,
}

impl RecorderHealth {
    const BASE_BACKOFF_MS: u64 = 30_000;
    const MAX_BACKOFF_MS: u64 = 10 * 60_000;

    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            last_failure_ms: None,
        }
    }

    /// Should we currently stay silent (inside a backoff window after failures)?
    fn in_backoff(&self, now_ms: u64) -> bool {
        let Some(last) = self.last_failure_ms else {
            return false;
        };
        let exp = Self::BASE_BACKOFF_MS
            .saturating_mul(1u64 << self.consecutive_failures.saturating_sub(1).min(5))
            .min(Self::MAX_BACKOFF_MS);
        now_ms.saturating_sub(last) < exp
    }

    fn note_failure(&mut self, now_ms: u64) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_failure_ms = Some(now_ms);
    }

    fn note_success(&mut self) {
        self.consecutive_failures = 0;
        self.last_failure_ms = None;
    }
}

/// Append-only, hash-chained decision log. Cloneable; all clones share head state.
pub struct DecisionLog {
    /// None = disabled by construction (eval harnesses, `:memory:` minds).
    path: Mutex<Option<PathBuf>>,
    health: Mutex<RecorderHealth>,
}

// NOTE: this handle deliberately holds NO chain state. It used to cache the head and the seen ids
// per handle, which is wrong whenever two handles address one file: handle B's append left handle
// A's cached head and id set stale, so A's next write chained onto a superseded head and could
// duplicate an id it had never seen written (Codex's review of P.4f). Everything about a file's
// chain now lives beside the file's lock, shared by every handle that names it.

impl std::fmt::Debug for DecisionLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = self.path.lock().unwrap_or_else(|e| e.into_inner());
        match &*p {
            Some(p) => write!(f, "DecisionLog({})", p.display()),
            None => write!(f, "DecisionLog(disabled)"),
        }
    }
}

impl DecisionLog {
    /// Open (or create) the log at `path`. An existing file continues its chain.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Mutex::new(Some(path.into())),
            health: Mutex::new(RecorderHealth::new()),
        }
    }

    /// A log that records nothing — the default for eval harnesses and scratch minds, so call
    /// sites can log unconditionally and stay branch-free.
    pub fn disabled() -> Self {
        Self {
            path: Mutex::new(None),
            health: Mutex::new(RecorderHealth::new()),
        }
    }

    /// From env override, else beside the DB (same convention as the read-receipt ledger):
    /// `YM_DECISION_LOG` wins, else `<db_path>.decisions.jsonl`, and `:memory:` DBs get nothing.
    pub fn for_db(db_path: &str) -> Self {
        match std::env::var("YM_DECISION_LOG") {
            Ok(p) if !p.trim().is_empty() => Self::open(p),
            _ if db_path != ":memory:" => Self::open(format!("{db_path}.decisions.jsonl")),
            _ => Self::disabled(),
        }
    }

    /// Record an event. CANNOT fail from the caller's perspective. Write failures mark the
    /// recorder unhealthy with an exponential backoff window (30s doubling to a 10min cap);
    /// inside the window recording is a silent no-op, after it the next record retries, and
    /// the first success resets to healthy. Cognition is unaffected in every state — but a
    /// transient disk hiccup costs seconds of blind spot, not the rest of the process's life.
    ///
    /// Takes the FILE's lock, like every other writer. It used to append without one, so an
    /// ordinary decision event could interleave with a durable outbox delivery and leave both
    /// chaining onto a head the other had already superseded (Codex's review of P.4f).
    pub fn record(&self, event: DecisionEvent) {
        let path = self.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(p) = path else { return };
        let now = now_ms();
        {
            let health = self.health.lock().unwrap_or_else(|e| e.into_inner());
            if health.in_backoff(now) {
                return;
            }
        }
        let state = path_state(&p);
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
        let prev = match st.head.clone() {
            Some(h) => h,
            None => chain_head(&p).unwrap_or_else(|| "genesis".to_string()),
        };
        match append_chained(&p, &event.sanitized(), &prev) {
            Ok(chain) => {
                st.head = Some(chain);
                self.health
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .note_success();
            }
            Err(e) => {
                st.head = None; // what is on disk is no longer known; the next writer asks the file
                self.health
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .note_failure(now);
                eprintln!(
                    "[flight-recorder] append failed ({e}); retrying after backoff (failure #{})",
                    self.health
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .consecutive_failures
                );
            }
        }
    }

    /// Record an event EXACTLY ONCE, and say what happened.
    ///
    /// `record` cannot fail from the caller's perspective, which is right for cognition — a disk
    /// hiccup must not stop the mind thinking — and wrong for anything holding a durable outbox: a
    /// caller that acknowledges a delivery `record` silently dropped has destroyed the evidence it
    /// was keeping (Codex's review of P.4a). This returns the outcome so an outbox can acknowledge
    /// only what is really on disk.
    ///
    /// Four things had to be true before that promise was worth anything, and none of them were
    /// (Codex's recorder review of P.4c):
    ///
    /// 1. CHECK AND APPEND ARE ONE ACT. The first cut released the id cache before appending, so
    ///    two concurrent drains could both look, both miss, and both write. The whole sequence now
    ///    runs under a lock keyed by the log's canonical PATH, because two handles can address one
    ///    file and a per-handle lock would not see the other.
    /// 2. THE TAIL IS VERIFIED BEFORE IT IS WRITTEN TO. A crash mid-write leaves a partial line;
    ///    appending onto it concatenates the next event into the fragment and quietly breaks the
    ///    chain from there on. Corruption anywhere in the log is now `Failed` and nothing is
    ///    written through it.
    /// 3. THE IDS COME FROM A VERIFIED CHAIN. Built from `read_events`, which skips what it cannot
    ///    parse, a forged line carrying a real id would answer "already present" and the outbox
    ///    would acknowledge an event the log does not honestly contain.
    /// 4. AN AMBIGUOUS WRITE IS RESOLVED, NOT ASSUMED. `sync_all` can fail after the bytes have
    ///    landed. The cache is invalidated and the log re-verified in the same critical section:
    ///    durable only if the valid chain really contains the id, `Failed` otherwise.
    ///
    /// `Ok(AlreadyPresent)` is a success for a retrying caller: the event IS durable, which is what
    /// the acknowledgement is about.
    pub fn record_once(&self, event: DecisionEvent) -> RecordOutcome {
        // Dedupe against the identity that is actually durable. Sanitizing only at append time
        // would compare a retry's raw id with the redacted id on disk and write it twice.
        let event = event.sanitized();
        let Some(id) = event.event_id.clone() else {
            return RecordOutcome::Failed("record_once needs an event_id — it is the identity that makes the write idempotent".into());
        };
        let path = self.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(p) = path else {
            return RecordOutcome::Disabled;
        };
        let now = now_ms();
        {
            let health = self.health.lock().unwrap_or_else(|e| e.into_inner());
            if health.in_backoff(now) {
                return RecordOutcome::Failed("recorder is in its failure backoff window".into());
            }
        }
        // ONE CRITICAL SECTION, per file, for the whole scan-check-append sequence — and a STRICT
        // RESCAN every time rather than anything remembered. A per-handle cache was the second
        // version of this bug: another handle's append left it stale, and the stale copy said an
        // id was absent that was already on disk (Codex's review of P.4f). An outbox delivery is
        // rare and the log is small; correctness is worth the read.
        let state = path_state(&p);
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());

        let (events, head) = match verified_scan(&p) {
            Ok(x) => x,
            Err(bad) => {
                st.head = None;
                return RecordOutcome::Failed(format!(
                    "the log does not verify at line {bad} — refusing to append onto a broken chain; repair or rotate it"
                ));
            }
        };
        if events
            .iter()
            .any(|e| e.event_id.as_deref() == Some(id.as_str()))
        {
            st.head = head;
            return RecordOutcome::AlreadyPresent;
        }
        let prev = head.unwrap_or_else(|| "genesis".to_string());
        match append_chained(&p, &event, &prev) {
            Ok(chain) => {
                st.head = Some(chain);
                self.health
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .note_success();
                RecordOutcome::Written
            }
            Err(e) => {
                // The bytes may or may not have landed. Ask the file, under the same lock, instead
                // of guessing.
                self.health
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .note_failure(now);
                st.head = None;
                match verified_scan(&p) {
                    Ok((evs, h)) if evs.iter().any(|x| x.event_id.as_deref() == Some(id.as_str())) => {
                        st.head = h;
                        RecordOutcome::AlreadyPresent
                    }
                    Ok((_, h)) => {
                        st.head = h;
                        RecordOutcome::Failed(format!("append failed and the event is not in the log: {e}"))
                    }
                    Err(bad) => RecordOutcome::Failed(format!(
                        "append failed ({e}) and the log no longer verifies at line {bad} — repair it before recording again"
                    )),
                }
            }
        }
    }

    /// Where this log writes, when active.
    pub fn trace_path(&self) -> Option<PathBuf> {
        self.path.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// EVERY recorded event, in order — what a report must read. `read_trace("")` is "the last
    /// few decisions" for a bare `ym why`, and a calibration or utilisation report computed over
    /// the last ten events is a number wearing a report's name.
    pub fn read_all(&self) -> Vec<DecisionEvent> {
        match self.trace_path() {
            Some(p) => read_events(&p),
            None => Vec::new(),
        }
    }

    /// Every recorded event only when the complete hash chain verifies. Metrics used as promotion
    /// gates must read through here: a parseable forged line is useful for forensic display but is
    /// not evidence and must never improve a completeness score.
    pub fn read_all_verified(&self) -> std::result::Result<Vec<DecisionEvent>, usize> {
        match self.trace_path() {
            Some(p) => read_events_verified(&p),
            None => Ok(Vec::new()),
        }
    }

    /// Every event under a trace-id prefix, in recorded order — the raw material for `ym why`.
    pub fn read_trace(&self, prefix: &str) -> Vec<DecisionEvent> {
        let Some(p) = self.trace_path() else {
            return vec![];
        };
        if prefix.trim().is_empty() {
            // No prefix = the most recent events, so `ym why` with no argument shows "the last
            // few decisions" instead of nothing.
            let all = read_events(&p);
            let start = all.len().saturating_sub(10);
            return all[start..].to_vec();
        }
        events_by_trace(&p, prefix)
    }
}

/// What a durable-delivery attempt actually did. `Written` and `AlreadyPresent` both mean the
/// event is on disk; nothing else does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    Written,
    /// This id is already in the log — a retry after a crash between the append and the
    /// acknowledgement. Durable, so an outbox may acknowledge it.
    AlreadyPresent,
    /// No log is configured (an eval harness, a test). Nothing was written and nothing can be.
    Disabled,
    Failed(String),
}

impl RecordOutcome {
    /// Is the event durably on disk? The only question an outbox should ask.
    pub fn is_durable(&self) -> bool {
        matches!(self, RecordOutcome::Written | RecordOutcome::AlreadyPresent)
    }
}

/// Everything about one LOG FILE's chain: its lock, and the head every writer must chain onto.
///
/// Shared by every `DecisionLog` handle that names the file, because the identity that matters is
/// the file and not the handle. Two mistakes were made here in turn (both found by Codex): first
/// the sequence was not atomic at all, then it was made atomic but each handle kept its OWN head
/// and id cache, which another handle's append silently invalidated. A writer now takes this lock
/// and derives the head from shared state or from the file itself — never from something it
/// remembered before another writer ran.
///
/// Process-scoped, and honestly so: two OS processes appending to one log would still race, and
/// this mind runs one. A cross-process guarantee needs a file lock and is not built here.
#[derive(Debug, Default)]
struct PathState {
    /// The chain value of the last line known to be on disk. `None` = ask the file.
    head: Option<String>,
}

static PATH_STATE: std::sync::Mutex<
    Option<std::collections::HashMap<PathBuf, std::sync::Arc<std::sync::Mutex<PathState>>>>,
> = std::sync::Mutex::new(None);

/// One key per file, stable whether or not the file exists yet.
///
/// `canonicalize` fails on a path that has not been created, so keying on it directly gave a
/// relative key before the first write and a canonical one after — two locks for one file, and no
/// mutual exclusion between the handle that created it and the next (Codex's review of P.4f).
/// The parent is canonicalised instead, which exists, and the file name is joined onto it.
fn lock_key(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| path.to_path_buf(), |c| c.join(path))
    };
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|p| p.join(name))
            .unwrap_or(absolute),
        _ => absolute,
    }
}

fn path_state(path: &Path) -> std::sync::Arc<std::sync::Mutex<PathState>> {
    let mut guard = PATH_STATE.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(Default::default)
        .entry(lock_key(path))
        .or_default()
        .clone()
}

/// Append one event onto an explicitly given head, and return the new chain value. The caller holds
/// the file's lock and decides what `prev` is — this never guesses from remembered state.
fn append_chained(path: &Path, event: &DecisionEvent, prev: &str) -> std::io::Result<String> {
    use std::io::Write;
    let event_json = serde_json::to_string(event).map_err(std::io::Error::other)?;
    let mut hasher = Sha256::new();
    hasher.update(prev.as_bytes());
    hasher.update(event_json.as_bytes());
    let chain = format!("{:x}", hasher.finalize());
    let line = format!("{{\"chain\":\"{chain}\",\"event\":{event_json}}}\n");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    f.sync_all()?;
    Ok(chain)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Stable, content-free identity for an observed object whose source value must not enter the
/// flight recorder. The namespace remains readable while the value is represented by 128 bits of
/// a domain-separated SHA-256 digest.
pub fn opaque_id(namespace: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(namespace.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("{namespace}:{}", &digest[..32])
}

/// The current chain head (last line's chain value), or None for missing/empty logs.
pub fn chain_head(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let last = content.lines().rev().find(|l| !l.trim().is_empty())?;
    let parsed: ChainedLine = serde_json::from_str(last).ok()?;
    Some(parsed.chain)
}

/// Recompute the chain line-by-line. Ok(n) = n valid events; Err(i) = first bad line index.
pub fn verify_log(path: &Path) -> Result<usize, usize> {
    let content = std::fs::read_to_string(path).map_err(|_| 0usize)?;
    let mut prev = "genesis".to_string();
    let mut n = 0usize;
    for (i, line) in content.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let parsed: ChainedLine = serde_json::from_str(line).map_err(|_| i)?;
        let event_json = serde_json::to_string(&parsed.event).map_err(|_| i)?;
        let mut hasher = Sha256::new();
        hasher.update(prev.as_bytes());
        hasher.update(event_json.as_bytes());
        let expect = format!("{:x}", hasher.finalize());
        if expect != parsed.chain {
            return Err(i);
        }
        prev = parsed.chain;
        n += 1;
    }
    Ok(n)
}

/// Every event whose chain verifies, walking from the genesis — and an error the moment one does
/// not, carrying how many were good before it.
///
/// `read_events` deliberately skips what it cannot parse, which is right for a report and wrong for
/// anything that must not be fooled: a line with a forged chain and a real event id would satisfy a
/// dedupe check built on it, and the outbox would acknowledge an event the log does not honestly
/// contain (Codex's review of P.4c). Durable delivery reads through here and nowhere else.
pub fn read_events_verified(path: &Path) -> std::result::Result<Vec<DecisionEvent>, usize> {
    verified_scan(path).map(|(events, _head)| events)
}

/// The verified events AND the chain value they end on — everything a writer needs to append
/// correctly, read from the file in one pass while holding its lock.
fn verified_scan(path: &Path) -> std::result::Result<(Vec<DecisionEvent>, Option<String>), usize> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        // A log that does not exist yet is an empty one; a log that cannot be READ is not.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(_) => return Err(0),
    };
    let mut prev: Option<String> = None;
    let mut out = Vec::new();
    for (i, line) in content.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let parsed: ChainedLine = serde_json::from_str(line).map_err(|_| i)?;
        let event_json = serde_json::to_string(&parsed.event).map_err(|_| i)?;
        let mut hasher = Sha256::new();
        hasher.update(prev.as_deref().unwrap_or("genesis").as_bytes());
        hasher.update(event_json.as_bytes());
        if format!("{:x}", hasher.finalize()) != parsed.chain {
            return Err(i);
        }
        prev = Some(parsed.chain);
        out.push(parsed.event);
    }
    // A trailing PARTIAL line — a crash mid-write — is not a valid event and must never be appended
    // onto: the next line would be concatenated into it and the whole tail would stop verifying.
    if !content.is_empty() && !content.ends_with('\n') {
        return Err(out.len());
    }
    Ok((out, prev))
}

/// All events, in file order (chain NOT verified here — pair with [`verify_log`] when it matters).
pub fn read_events(path: &Path) -> Vec<DecisionEvent> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return vec![];
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ChainedLine>(l).ok())
        .map(|c| c.event)
        .collect()
}

/// Every event whose trace_id starts with `trace_prefix`, in recorded order — the raw material
/// for `ym why`.
pub fn events_by_trace(path: &Path, trace_prefix: &str) -> Vec<DecisionEvent> {
    read_events(path)
        .into_iter()
        .filter(|e| e.trace_id.starts_with(trace_prefix))
        .collect()
}

/// Report which versioned evaluator assigned each persisted outcome grade. Coverage is computed
/// only over events that actually carry outcome evidence; routing decisions with a verdict but no
/// observed result are not mislabeled as grades.
pub fn render_evaluator_coverage(events: &[DecisionEvent]) -> String {
    let graded: Vec<&DecisionEvent> = events
        .iter()
        .filter(|e| {
            e.outcome.is_some()
                || e.semantic_success.is_some()
                || e.brier.is_some()
                || e.prediction_error.is_some()
        })
        .collect();
    if graded.is_empty() {
        return "No graded events yet — evaluator coverage appears once an outcome is recorded."
            .into();
    }
    let mut by_evaluator: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut missing = 0usize;
    for event in &graded {
        match event.evaluator_id.as_deref() {
            Some(id) => *by_evaluator.entry(id).or_insert(0) += 1,
            None => missing += 1,
        }
    }
    let stamped = graded.len() - missing;
    let mut out = format!(
        "EVALUATOR COVERAGE — {stamped}/{} graded event(s) stamped:\n",
        graded.len()
    );
    for (evaluator, count) in by_evaluator {
        out.push_str(&format!("  {evaluator}: {count}\n"));
    }
    if missing > 0 {
        out.push_str(&format!("  missing evaluator_id: {missing}\n"));
    }
    out
}

/// Report explicit execution-lane coverage across the recorder. Actor and lane are shown together
/// because a lane label without the organ that ran it recreates the ambiguity this field removed.
pub fn render_lane_coverage(events: &[DecisionEvent]) -> String {
    if events.is_empty() {
        return "No recorded events yet — lane coverage appears once decisions are recorded."
            .into();
    }
    let mut pairs: std::collections::BTreeMap<(&str, &str), usize> = Default::default();
    let mut missing = 0usize;
    for event in events {
        match event.lane.as_deref() {
            Some(lane) => {
                *pairs
                    .entry((event.actor.as_deref().unwrap_or("?"), lane))
                    .or_insert(0) += 1;
            }
            None => missing += 1,
        }
    }
    let stamped = events.len() - missing;
    let mut out = format!(
        "LANE COVERAGE — {stamped}/{} recorded event(s) stamped:\n",
        events.len()
    );
    for ((actor, lane), count) in pairs {
        out.push_str(&format!("  {actor} / {lane}: {count}\n"));
    }
    if missing > 0 {
        out.push_str(&format!("  missing lane: {missing}\n"));
    }
    out
}

/// Report latency coverage for tool calls that reached execution. Malformed and denied attempts do
/// not enter the denominator because no tool ran; all other observed tool outcomes should carry a
/// duration. Percentiles are nearest-rank values over the persisted samples.
pub fn render_latency_coverage(events: &[DecisionEvent]) -> String {
    let eligible: Vec<&DecisionEvent> = events
        .iter()
        .filter(|e| {
            e.kind == "tool_observed"
                && !matches!(e.verdict.as_deref(), Some("malformed" | "denied"))
        })
        .collect();
    if eligible.is_empty() {
        return "No executed tool observations yet — latency coverage appears after a tool runs."
            .into();
    }
    let mut samples: Vec<u64> = eligible.iter().filter_map(|e| e.latency_ms).collect();
    let missing = eligible.len() - samples.len();
    samples.sort_unstable();
    let stamped = samples.len();
    let mut out = format!(
        "TOOL LATENCY COVERAGE — {stamped}/{} executed call(s) timed:\n",
        eligible.len()
    );
    if !samples.is_empty() {
        let nearest_rank = |percent: usize| -> u64 {
            let index = (samples.len() * percent).div_ceil(100).saturating_sub(1);
            samples[index]
        };
        out.push_str(&format!(
            "  p50: {} ms · p95: {} ms · max: {} ms\n",
            nearest_rank(50),
            nearest_rank(95),
            samples[samples.len() - 1]
        ));
    }
    if missing > 0 {
        out.push_str(&format!("  missing latency_ms: {missing}\n"));
    }
    out
}

/// Report whether semantically assessable outcomes carry the explicit grade promised by the
/// schema. Only `ok`/`empty` tool observations enter the tool denominator: failed, unavailable,
/// denied, and malformed calls belong to execution/availability/safety, not semantic usefulness.
/// Tool-goal, pack-use, and pack-outcome grades are semantically assessable by definition. Only
/// hit/miss forecast grades have a binary semantic outcome; `unclear` is audited by the forecast
/// chain gate and deliberately excluded here rather than misreported as a missing Boolean grade.
pub fn render_semantic_coverage(events: &[DecisionEvent]) -> String {
    let eligible: Vec<&DecisionEvent> = events
        .iter()
        .filter(|e| match e.kind.as_str() {
            "tool_observed" => matches!(e.verdict.as_deref(), Some("ok" | "empty")),
            "prediction_graded" => matches!(e.verdict.as_deref(), Some("hit" | "miss")),
            "tool_goal_graded" | "pack_evidence_graded" => true,
            _ => e.semantic_success.is_some(),
        })
        .collect();
    if eligible.is_empty() {
        return "No semantic-grade candidates yet — coverage appears after a consequential outcome."
            .into();
    }
    let mut by_kind: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
    let mut stamped = 0usize;
    for event in &eligible {
        let row = by_kind.entry(event.kind.as_str()).or_insert((0, 0));
        row.1 += 1;
        if event.semantic_success.is_some() {
            stamped += 1;
            row.0 += 1;
        }
    }
    let mut out = format!(
        "SEMANTIC-SUCCESS COVERAGE — {stamped}/{} assessable outcome(s) graded:\n",
        eligible.len()
    );
    for (kind, (kind_stamped, total)) in by_kind {
        out.push_str(&format!("  {kind}: {kind_stamped}/{total}\n"));
    }
    let missing = eligible.len() - stamped;
    if missing > 0 {
        out.push_str(&format!("  missing semantic_success: {missing}\n"));
    }
    out
}

/// Report opaque context-fingerprint coverage for compiled cognition and the closed tool-learning
/// chain. Fingerprints are never printed: the operator gets coverage and the number of distinct
/// contexts without a stable identifier that could be copied into unrelated systems.
pub fn render_context_coverage(events: &[DecisionEvent]) -> String {
    let eligible: Vec<&DecisionEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e.kind.as_str(),
                "grounding_assembled"
                    | "goal_compiled"
                    | "cognitive_run"
                    | "cognitive_run_refused"
                    | "pack_route_shadow"
                    | "pack_surfaced"
                    | "pack_evidence_used"
                    | "pack_evidence_graded"
                    | "tool_predicted"
                    | "tool_observed"
                    | "tool_goal_graded"
            )
        })
        .collect();
    if eligible.is_empty() {
        return "No context-linked cognition events yet — coverage appears after compilation."
            .into();
    }
    let stamped = eligible
        .iter()
        .filter(|e| e.context_fingerprint.is_some())
        .count();
    let distinct = eligible
        .iter()
        .filter_map(|e| e.context_fingerprint.as_deref())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let mut out = format!(
        "CONTEXT FINGERPRINT COVERAGE — {stamped}/{} eligible event(s) stamped · {distinct} distinct context(s)\n",
        eligible.len()
    );
    let missing = eligible.len() - stamped;
    if missing > 0 {
        out.push_str(&format!("  missing context_fingerprint: {missing}\n"));
    }
    out
}

/// Report stable goal-identity coverage over the decision families that can participate in the
/// closed learning chain. Free-text `goal` is deliberately not treated as identity.
pub fn render_goal_id_coverage(events: &[DecisionEvent]) -> String {
    let eligible: Vec<&DecisionEvent> = events
        .iter()
        .filter(|e| {
            matches!(
                e.kind.as_str(),
                "goal_compiled"
                    | "cognitive_run"
                    | "cognitive_run_refused"
                    | "packet_created"
                    | "packet_resolved"
                    | "packet_expired"
                    | "tool_predicted"
                    | "tool_observed"
                    | "tool_goal_graded"
            )
        })
        .collect();
    if eligible.is_empty() {
        return "No goal-linked events yet — stable goal coverage appears after a compiled run or action packet."
            .into();
    }
    let stamped = eligible.iter().filter(|e| e.goal_id.is_some()).count();
    let distinct = eligible
        .iter()
        .filter_map(|e| e.goal_id.as_deref())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let mut out = format!(
        "STABLE GOAL COVERAGE — {stamped}/{} goal-linked event(s) stamped · {distinct} distinct goal(s)\n",
        eligible.len()
    );
    let missing = eligible.len() - stamped;
    if missing > 0 {
        out.push_str(&format!("  missing goal_id: {missing}\n"));
    }
    out
}

/// Report version-stamp coverage for tool-chain events. Unlike evaluator identity, this names the
/// runtime that dispatched the capability, so a behavior change can be compared across builds.
pub fn render_tool_version_coverage(events: &[DecisionEvent]) -> String {
    let eligible: Vec<&DecisionEvent> = events
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "tool_predicted" | "tool_observed"))
        .collect();
    if eligible.is_empty() {
        return "No tool-chain events yet — version coverage appears after a tool decision.".into();
    }
    let mut versions: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut missing = 0usize;
    for event in &eligible {
        match event.tool_version.as_deref() {
            Some(version) => *versions.entry(version).or_insert(0) += 1,
            None => missing += 1,
        }
    }
    let stamped = eligible.len() - missing;
    let mut out = format!(
        "TOOL VERSION COVERAGE — {stamped}/{} tool-chain event(s) stamped:\n",
        eligible.len()
    );
    for (version, count) in versions {
        out.push_str(&format!("  {version}: {count}\n"));
    }
    if missing > 0 {
        out.push_str(&format!("  missing tool_version: {missing}\n"));
    }
    out
}

/// Report configured model-route coverage for model-mediated runs and tool decisions. Routes say
/// what pools were selected; they intentionally do not claim which link in a chain served.
pub fn render_model_route_coverage(events: &[DecisionEvent]) -> String {
    let eligible: Vec<&DecisionEvent> = events
        .iter()
        .filter(|event| {
            matches!(
                event.kind.as_str(),
                "goal_compiled"
                    | "cognitive_run"
                    | "cognitive_run_refused"
                    | "tool_predicted"
                    | "tool_observed"
            ) || (event.kind == "prediction_graded"
                && event.evaluator_id.as_deref() == Some("grounded-forecast-judge-v1"))
        })
        .collect();
    if eligible.is_empty() {
        return "No model-mediated events yet — configured route coverage appears after cognition runs."
            .into();
    }
    let mut routes: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut missing = 0usize;
    for event in &eligible {
        match event.model_route.as_deref() {
            Some(route) => *routes.entry(route).or_insert(0) += 1,
            None => missing += 1,
        }
    }
    let stamped = eligible.len() - missing;
    let mut out = format!(
        "CONFIGURED MODEL ROUTE COVERAGE — {stamped}/{} model-mediated event(s) stamped:\n",
        eligible.len()
    );
    for (route, count) in routes {
        out.push_str(&format!("  {route}: {count}\n"));
    }
    if missing > 0 {
        out.push_str(&format!("  missing model_route: {missing}\n"));
    }
    out.push_str(
        "  note: configured route only; actual serving-link identity is not yet recorded\n",
    );
    out
}

/// Report monotonic grounding/compilation/run wall time plus the logical model-call counts exposed by
/// compilation, completed bounded runs, and forecast grading. Grounding model calls are not
/// attributed yet; naming that boundary prevents cheap proxies from silently hardening into a cost
/// claim.
pub fn render_model_call_resources(events: &[DecisionEvent]) -> String {
    let groundings: Vec<&DecisionEvent> = events
        .iter()
        .filter(|event| event.kind == "grounding_assembled")
        .collect();
    let compiles: Vec<&DecisionEvent> = events
        .iter()
        .filter(|event| event.kind == "goal_compiled")
        .collect();
    let runs: Vec<&DecisionEvent> = events
        .iter()
        .filter(|event| event.kind == "cognitive_run")
        .collect();
    let forecast_grades: Vec<&DecisionEvent> = events
        .iter()
        .filter(|event| event.kind == "prediction_graded")
        .collect();
    if groundings.is_empty() && compiles.is_empty() && runs.is_empty() && forecast_grades.is_empty()
    {
        return "No grounding, compiled, bounded-run, or forecast-grade events yet — resources appear after cognition or prediction grading."
            .into();
    }
    let compile_calls: Vec<u32> = compiles
        .iter()
        .filter_map(|event| event.model_calls)
        .collect();
    let mut compile_latency: Vec<u64> = compiles
        .iter()
        .filter_map(|event| event.latency_ms)
        .collect();
    compile_latency.sort_unstable();
    let call_samples: Vec<u32> = runs.iter().filter_map(|event| event.model_calls).collect();
    let missing_calls = runs.len() - call_samples.len();
    let total: u64 = call_samples.iter().map(|value| u64::from(*value)).sum();
    let mut latency_samples: Vec<u64> = runs.iter().filter_map(|event| event.latency_ms).collect();
    let missing_latency = runs.len() - latency_samples.len();
    latency_samples.sort_unstable();
    let mut out = String::from("COGNITION RESOURCES (logical model requests):\n");
    if !groundings.is_empty() {
        let mut grounding_latency: Vec<u64> = groundings
            .iter()
            .filter_map(|event| event.latency_ms)
            .collect();
        grounding_latency.sort_unstable();
        out.push_str(&format!(
            "  grounding assembly — {}/{} event(s) timed",
            grounding_latency.len(),
            groundings.len()
        ));
        if !grounding_latency.is_empty() {
            let p95 = (grounding_latency.len() * 95)
                .div_ceil(100)
                .saturating_sub(1);
            out.push_str(&format!(
                " · p95: {} ms · max: {} ms",
                grounding_latency[p95],
                grounding_latency[grounding_latency.len() - 1]
            ));
        }
        out.push('\n');
        let missing_latency = groundings.len() - grounding_latency.len();
        if missing_latency > 0 {
            out.push_str(&format!("    missing latency_ms: {missing_latency}\n"));
        }
    }
    if !compiles.is_empty() {
        let compile_total: u64 = compile_calls.iter().map(|value| u64::from(*value)).sum();
        out.push_str(&format!(
            "  compile — {}/{} event(s) counted, {}/{} timed",
            compile_calls.len(),
            compiles.len(),
            compile_latency.len(),
            compiles.len()
        ));
        if !compile_calls.is_empty() {
            out.push_str(&format!(" · calls: {compile_total}"));
        }
        if !compile_latency.is_empty() {
            let p95 = (compile_latency.len() * 95).div_ceil(100).saturating_sub(1);
            out.push_str(&format!(
                " · p95: {} ms · max: {} ms",
                compile_latency[p95],
                compile_latency[compile_latency.len() - 1]
            ));
        }
        out.push('\n');
        let missing_calls = compiles.len() - compile_calls.len();
        let missing_latency = compiles.len() - compile_latency.len();
        if missing_calls > 0 {
            out.push_str(&format!("    missing model_calls: {missing_calls}\n"));
        }
        if missing_latency > 0 {
            out.push_str(&format!("    missing latency_ms: {missing_latency}\n"));
        }
    }
    if !runs.is_empty() {
        out.push_str(&format!(
            "  bounded run — {}/{} event(s) counted, {}/{} timed:\n",
            call_samples.len(),
            runs.len(),
            latency_samples.len(),
            runs.len()
        ));
        if !call_samples.is_empty() {
            out.push_str(&format!(
                "    model calls — total: {total} · mean: {:.1} · max: {}\n",
                total as f64 / call_samples.len() as f64,
                call_samples.iter().max().copied().unwrap_or_default()
            ));
        }
        if !latency_samples.is_empty() {
            let nearest_rank = |percent: usize| -> u64 {
                let index = (latency_samples.len() * percent)
                    .div_ceil(100)
                    .saturating_sub(1);
                latency_samples[index]
            };
            out.push_str(&format!(
                "    wall time — p50: {} ms · p95: {} ms · max: {} ms\n",
                nearest_rank(50),
                nearest_rank(95),
                latency_samples[latency_samples.len() - 1]
            ));
        }
        if missing_calls > 0 {
            out.push_str(&format!("    missing model_calls: {missing_calls}\n"));
        }
        if missing_latency > 0 {
            out.push_str(&format!("    missing latency_ms: {missing_latency}\n"));
        }
    }
    if !forecast_grades.is_empty() {
        let forecast_calls: Vec<u32> = forecast_grades
            .iter()
            .filter_map(|event| event.model_calls)
            .collect();
        let forecast_total: u64 = forecast_calls.iter().map(|value| u64::from(*value)).sum();
        let grounded = forecast_grades
            .iter()
            .filter(|event| event.evaluator_id.as_deref() == Some("grounded-forecast-judge-v1"))
            .count();
        let receipts = forecast_grades
            .iter()
            .filter(|event| event.evaluator_id.as_deref() == Some("ledger-receipt-v1"))
            .count();
        out.push_str(&format!(
            "  forecast grading — {}/{} event(s) counted · model calls: {forecast_total} · grounded judges: {grounded} · ledger receipts: {receipts}\n",
            forecast_calls.len(),
            forecast_grades.len(),
        ));
        let missing_calls = forecast_grades.len() - forecast_calls.len();
        if missing_calls > 0 {
            out.push_str(&format!("    missing model_calls: {missing_calls}\n"));
        }
        let grounded_grades: Vec<&&DecisionEvent> = forecast_grades
            .iter()
            .filter(|event| event.evaluator_id.as_deref() == Some("grounded-forecast-judge-v1"))
            .collect();
        if !grounded_grades.is_empty() {
            let mut judge_latency: Vec<u64> = grounded_grades
                .iter()
                .filter_map(|event| event.latency_ms)
                .collect();
            judge_latency.sort_unstable();
            out.push_str(&format!(
                "    judge latency — {}/{} event(s) timed",
                judge_latency.len(),
                grounded_grades.len()
            ));
            if !judge_latency.is_empty() {
                let p95 = (judge_latency.len() * 95).div_ceil(100).saturating_sub(1);
                out.push_str(&format!(
                    " · p95: {} ms · max: {} ms",
                    judge_latency[p95],
                    judge_latency[judge_latency.len() - 1]
                ));
            }
            out.push('\n');
            let missing_latency = grounded_grades.len() - judge_latency.len();
            if missing_latency > 0 {
                out.push_str(&format!("    missing latency_ms: {missing_latency}\n"));
            }
        }
    }
    out.push_str(
        "  boundary: grounding model-call attribution, tokens, monetary cost, and provider failover attempts are not recorded\n",
    );
    out
}

/// Measure the roadmap's closed-chain gate over the latest 200 tool calls. A call is
/// complete only when its observation joins to one prediction and the pair carries the provenance
/// needed to compare behavior across goals, contexts, lanes, evaluators, and runtime versions.
/// Aggregate defect counts are reported instead of identifiers, so this remains safe to paste into
/// an operations channel.
pub fn render_tool_chain_completeness(events: &[DecisionEvent]) -> String {
    const SAMPLE_LIMIT: usize = 200;

    let mut compiled_roots: std::collections::HashMap<&str, Vec<Option<&str>>> = Default::default();
    for event in events.iter().filter(|event| event.kind == "goal_compiled") {
        compiled_roots
            .entry(event.trace_id.as_str())
            .or_default()
            .push(event.event_id.as_deref());
    }
    let predictions: std::collections::HashMap<&str, &DecisionEvent> = events
        .iter()
        .filter(|event| event.kind == "tool_predicted")
        .filter_map(|event| event.event_id.as_deref().map(|id| (id, event)))
        .collect();
    let mut observed_parent_counts: std::collections::HashMap<&str, usize> = Default::default();
    for parent in events
        .iter()
        .filter(|event| event.kind == "tool_observed")
        .filter_map(|event| event.parent_event_id.as_deref())
    {
        *observed_parent_counts.entry(parent).or_insert(0) += 1;
    }
    // One row per observed call, plus every prediction that has no observed child. Sampling only
    // observations would make a crash between the two events disappear from the denominator and
    // let the report go falsely green precisely when the recorder lost closure.
    let mut calls: Vec<(usize, Option<&DecisionEvent>, Option<&DecisionEvent>)> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.kind == "tool_observed")
        .map(|(index, observation)| {
            let prediction = observation
                .parent_event_id
                .as_deref()
                .and_then(|parent| predictions.get(parent).copied());
            (index, prediction, Some(observation))
        })
        .collect();
    calls.extend(
        events
            .iter()
            .enumerate()
            .filter(|(_, event)| event.kind == "tool_predicted")
            .filter(|(_, prediction)| {
                prediction
                    .event_id
                    .as_deref()
                    .is_none_or(|id| !observed_parent_counts.contains_key(id))
            })
            .map(|(index, prediction)| (index, Some(prediction), None)),
    );
    calls.sort_unstable_by_key(|(index, _, _)| std::cmp::Reverse(*index));
    calls.truncate(SAMPLE_LIMIT);
    if calls.is_empty() {
        return "No tool-chain calls yet — completeness appears after a tool decision.".into();
    }

    let mut complete = 0usize;
    let mut defects: std::collections::BTreeMap<&str, usize> = Default::default();
    for (_, prediction, observation) in &calls {
        let mut row_complete = true;
        let mut require = |condition: bool, label: &'static str| {
            if !condition {
                row_complete = false;
                *defects.entry(label).or_insert(0) += 1;
            }
        };

        require(prediction.is_some(), "prediction link");
        require(observation.is_some(), "observation link");
        if let (Some(prediction), Some(observation)) = (prediction, observation) {
            if let Some(roots) = compiled_roots.get(prediction.trace_id.as_str()) {
                require(
                    roots.len() == 1
                        && roots[0].is_some()
                        && prediction.parent_event_id.as_deref() == roots[0],
                    "bounded root linkage",
                );
            }
            require(
                observation
                    .parent_event_id
                    .as_deref()
                    .and_then(|parent| observed_parent_counts.get(parent))
                    == Some(&1),
                "observation cardinality",
            );
            require(prediction.trace_id == observation.trace_id, "trace linkage");
            require(
                prediction.object_id.is_some() && prediction.object_id == observation.object_id,
                "object linkage",
            );
            require(
                prediction.actor.is_some() && prediction.actor == observation.actor,
                "actor",
            );
            require(
                prediction.lane.is_some() && prediction.lane == observation.lane,
                "lane",
            );
            require(
                prediction.context_fingerprint.is_some()
                    && prediction.context_fingerprint == observation.context_fingerprint,
                "context_fingerprint",
            );
            require(
                prediction.goal_id.is_some() && prediction.goal_id == observation.goal_id,
                "goal_id",
            );
            require(
                prediction.tool_version.is_some()
                    && prediction.tool_version == observation.tool_version,
                "tool_version",
            );
            require(
                prediction.model_route.is_some()
                    && prediction.model_route == observation.model_route,
                "model_route",
            );
            require(prediction.predicted.is_some(), "predicted outcome");
            require(
                prediction.confidence.is_some_and(valid_probability),
                "predicted probability",
            );
        }
        if let Some(observation) = observation {
            require(observation.verdict.is_some(), "actual verdict");
            require(observation.evaluator_id.is_some(), "evaluator_id");
            if !matches!(observation.verdict.as_deref(), Some("malformed" | "denied")) {
                require(observation.latency_ms.is_some(), "latency_ms");
            }
            if matches!(observation.verdict.as_deref(), Some("ok" | "empty")) {
                require(observation.semantic_success.is_some(), "semantic_success");
            }
        }
        if row_complete {
            complete += 1;
        }
    }

    let total = calls.len();
    let percent = 100.0 * complete as f64 / total as f64;
    let mut out = format!(
        "TOOL CHAIN COMPLETENESS — {complete}/{total} latest call(s) complete ({percent:.1}%; gate ≥99%)\n"
    );
    if defects.is_empty() {
        out.push_str("  no missing or mismatched provenance in this sample\n");
    } else {
        for (field, count) in defects {
            out.push_str(&format!("  missing or mismatched {field}: {count}\n"));
        }
    }
    out
}

/// Measure causal and grading completeness for the latest 200 terminal action-packet outcomes.
/// Proposed packets that are still waiting for an owner word stay outside the denominator; a
/// resolved or expired packet is complete only when exactly one creation root and one terminal
/// event share the trace, the terminal points to that root, and its grade names an evaluator.
pub fn render_packet_chain_completeness(events: &[DecisionEvent]) -> String {
    const SAMPLE_LIMIT: usize = 200;

    let mut roots: std::collections::HashMap<&str, Vec<&DecisionEvent>> = Default::default();
    let mut terminal_counts: std::collections::HashMap<&str, usize> = Default::default();
    for event in events {
        match event.kind.as_str() {
            "packet_created" => roots
                .entry(event.trace_id.as_str())
                .or_default()
                .push(event),
            "packet_resolved" | "packet_expired" => {
                *terminal_counts.entry(event.trace_id.as_str()).or_insert(0) += 1;
            }
            _ => {}
        }
    }
    let terminals: Vec<&DecisionEvent> = events
        .iter()
        .rev()
        .filter(|event| matches!(event.kind.as_str(), "packet_resolved" | "packet_expired"))
        .take(SAMPLE_LIMIT)
        .collect();
    let remaining = SAMPLE_LIMIT.saturating_sub(terminals.len());
    let current_ms = i64::try_from(now_ms()).unwrap_or(i64::MAX);
    let overdue_unclosed: Vec<(&str, &Vec<&DecisionEvent>)> = roots
        .iter()
        .filter(|(trace_id, _)| !terminal_counts.contains_key(*trace_id))
        .filter(|(_, items)| {
            items.iter().any(|root| {
                root.policy
                    .iter()
                    .flat_map(|policy| policy.split_whitespace())
                    .find_map(|token| token.strip_prefix("expiry_ms="))
                    .and_then(|value| value.parse::<i64>().ok())
                    .is_some_and(|expiry_ms| expiry_ms < current_ms)
            })
        })
        .map(|(trace_id, items)| (*trace_id, items))
        .take(remaining)
        .collect();
    if terminals.is_empty() && overdue_unclosed.is_empty() {
        return "No packet closure candidates yet — completeness appears after a decision, expiry, or overdue proposal."
            .into();
    }

    let mut complete = 0usize;
    let mut defects: std::collections::BTreeMap<&str, usize> = Default::default();
    if !overdue_unclosed.is_empty() {
        defects.insert("terminal event", overdue_unclosed.len());
        let duplicate_roots = overdue_unclosed
            .iter()
            .filter(|(_, items)| items.len() != 1)
            .count();
        if duplicate_roots > 0 {
            defects.insert("creation cardinality", duplicate_roots);
        }
    }
    for terminal in &terminals {
        let mut row_complete = true;
        let mut require = |condition: bool, label: &'static str| {
            if !condition {
                row_complete = false;
                *defects.entry(label).or_insert(0) += 1;
            }
        };
        let trace_roots = roots.get(terminal.trace_id.as_str());
        require(
            trace_roots.is_some_and(|items| items.len() == 1),
            "creation cardinality",
        );
        if let Some(root) = trace_roots.and_then(|items| items.first()) {
            require(root.event_id.is_some(), "creation event_id");
            require(
                root.confidence.is_some_and(valid_probability),
                "packet confidence",
            );
            let expiry_horizons: Vec<i64> = root
                .policy
                .iter()
                .flat_map(|policy| policy.split_whitespace())
                .filter_map(|token| token.strip_prefix("expiry_ms="))
                .filter_map(|value| value.parse::<i64>().ok())
                .collect();
            require(expiry_horizons.len() == 1, "expiry horizon");
            let terminal_expiry_horizons: Vec<i64> = terminal
                .policy
                .iter()
                .flat_map(|policy| policy.split_whitespace())
                .filter_map(|token| token.strip_prefix("expiry_ms="))
                .filter_map(|value| value.parse::<i64>().ok())
                .collect();
            require(
                terminal_expiry_horizons.len() == 1,
                "terminal expiry horizon",
            );
            require(
                expiry_horizons.len() == 1
                    && terminal_expiry_horizons.len() == 1
                    && expiry_horizons[0] == terminal_expiry_horizons[0],
                "expiry horizon linkage",
            );
            let provenance: Vec<&str> = root
                .policy
                .iter()
                .flat_map(|policy| policy.split_whitespace())
                .filter_map(|token| token.strip_prefix("provenance="))
                .collect();
            require(
                provenance.len() == 1 && matches!(provenance[0], "inferred" | "observed" | "told"),
                "trigger provenance",
            );
            require(
                terminal.parent_event_id.is_some() && terminal.parent_event_id == root.event_id,
                "causal parent linkage",
            );
            require(
                root.object_id.is_some() && terminal.object_id == root.object_id,
                "object linkage",
            );
            require(
                root.goal_id.is_some() && terminal.goal_id == root.goal_id,
                "goal_id linkage",
            );
            require(
                root.actor.is_some() && terminal.actor == root.actor,
                "actor linkage",
            );
            require(
                root.lane.is_some() && terminal.lane == root.lane,
                "lane linkage",
            );
        }
        require(
            terminal_counts.get(terminal.trace_id.as_str()) == Some(&1),
            "terminal cardinality",
        );
        require(terminal.verdict.is_some(), "actual verdict");
        require(terminal.semantic_success.is_some(), "semantic_success");
        let expected_evaluator = match terminal.kind.as_str() {
            "packet_resolved" => "owner-packet-decision-v1",
            "packet_expired" => "packet-expiry-clock-v1",
            _ => unreachable!("terminal filter admits only known packet outcomes"),
        };
        require(
            terminal.evaluator_id.as_deref() == Some(expected_evaluator),
            "evaluator identity",
        );
        let grade_matches_outcome = match terminal.kind.as_str() {
            "packet_expired" => {
                terminal.verdict.as_deref() == Some("expired")
                    && terminal.semantic_success == Some(false)
            }
            "packet_resolved" => matches!(
                (terminal.verdict.as_deref(), terminal.semantic_success),
                (Some("confirmed"), Some(true)) | (Some("rejected"), Some(false))
            ),
            _ => unreachable!("terminal filter admits only known packet outcomes"),
        };
        require(grade_matches_outcome, "outcome grade");
        if row_complete {
            complete += 1;
        }
    }

    let total = terminals.len() + overdue_unclosed.len();
    let percent = 100.0 * complete as f64 / total as f64;
    let mut out = format!(
        "PACKET CHAIN COMPLETENESS — {complete}/{total} latest packet lifecycle(s) complete ({percent:.1}%; gate ≥99%)\n"
    );
    if defects.is_empty() {
        out.push_str("  no missing or mismatched packet provenance in this sample\n");
    } else {
        for (field, count) in defects {
            out.push_str(&format!("  missing or mismatched {field}: {count}\n"));
        }
    }
    out
}

/// Measure causal and scoring completeness for the latest 200 forecast closures. A complete
/// lifecycle has one immutable `prediction_made` root, one terminal grade parented to it, the exact
/// issued confidence on both sides, and execution provenance that distinguishes a zero-model
/// ledger receipt from a model-judged forecast. Hit/miss grades must carry internally consistent
/// error/Brier fields; unclear closures must omit those binary calibration claims. An overdue root
/// without any terminal event is an incomplete lifecycle, not an invisible still-open prediction.
pub fn render_forecast_chain_completeness(events: &[DecisionEvent]) -> String {
    const SAMPLE_LIMIT: usize = 200;
    const EPSILON: f64 = 1e-12;

    let mut roots: std::collections::HashMap<&str, Vec<&DecisionEvent>> = Default::default();
    let mut terminal_counts: std::collections::HashMap<&str, usize> = Default::default();
    for event in events {
        if event.kind == "prediction_made" {
            roots
                .entry(event.trace_id.as_str())
                .or_default()
                .push(event);
        } else if event.kind == "prediction_graded"
            && matches!(event.verdict.as_deref(), Some("hit" | "miss" | "unclear"))
        {
            *terminal_counts.entry(event.trace_id.as_str()).or_insert(0) += 1;
        }
    }
    let terminals: Vec<&DecisionEvent> = events
        .iter()
        .rev()
        .filter(|event| {
            event.kind == "prediction_graded"
                && matches!(event.verdict.as_deref(), Some("hit" | "miss" | "unclear"))
        })
        .take(SAMPLE_LIMIT)
        .collect();
    let remaining = SAMPLE_LIMIT.saturating_sub(terminals.len());
    let current_ms = i64::try_from(now_ms()).unwrap_or(i64::MAX);
    let overdue_unclosed: Vec<(&str, &Vec<&DecisionEvent>)> = roots
        .iter()
        .filter(|(trace_id, _)| !terminal_counts.contains_key(*trace_id))
        .filter(|(_, items)| {
            items.iter().any(|root| {
                root.policy
                    .iter()
                    .flat_map(|policy| policy.split_whitespace())
                    .find_map(|token| token.strip_prefix("resolve_by_ms="))
                    .and_then(|value| value.parse::<i64>().ok())
                    .is_some_and(|resolve_by_ms| resolve_by_ms < current_ms)
            })
        })
        .map(|(trace_id, items)| (*trace_id, items))
        .take(remaining)
        .collect();
    if terminals.is_empty() && overdue_unclosed.is_empty() {
        return "No forecast closure candidates yet — completeness appears after a grade or overdue prediction."
            .into();
    }

    let mut complete = 0usize;
    let mut defects: std::collections::BTreeMap<&str, usize> = Default::default();
    if !overdue_unclosed.is_empty() {
        defects.insert("terminal event", overdue_unclosed.len());
        let duplicate_roots = overdue_unclosed
            .iter()
            .filter(|(_, items)| items.len() != 1)
            .count();
        if duplicate_roots > 0 {
            defects.insert("creation cardinality", duplicate_roots);
        }
    }
    for terminal in &terminals {
        let mut row_complete = true;
        let mut require = |condition: bool, label: &'static str| {
            if !condition {
                row_complete = false;
                *defects.entry(label).or_insert(0) += 1;
            }
        };
        let trace_roots = roots.get(terminal.trace_id.as_str());
        require(
            trace_roots.is_some_and(|items| items.len() == 1),
            "creation cardinality",
        );
        if let Some(root) = trace_roots.and_then(|items| items.first()) {
            require(root.event_id.is_some(), "creation event_id");
            require(root.predicted.is_some(), "predicted proposition");
            require(
                root.confidence.is_some_and(valid_probability),
                "issued confidence",
            );
            let deadlines: Vec<i64> = root
                .policy
                .iter()
                .flat_map(|policy| policy.split_whitespace())
                .filter_map(|token| token.strip_prefix("resolve_by_ms="))
                .filter_map(|value| value.parse::<i64>().ok())
                .collect();
            require(deadlines.len() == 1, "creation resolution deadline");
            let terminal_deadlines: Vec<i64> = terminal
                .policy
                .iter()
                .flat_map(|policy| policy.split_whitespace())
                .filter_map(|token| token.strip_prefix("resolve_by_ms="))
                .filter_map(|value| value.parse::<i64>().ok())
                .collect();
            require(
                terminal_deadlines.len() == 1,
                "terminal resolution deadline",
            );
            require(
                deadlines.len() == 1
                    && terminal_deadlines.len() == 1
                    && deadlines[0] == terminal_deadlines[0],
                "resolution deadline linkage",
            );
            require(
                terminal.parent_event_id.is_some() && terminal.parent_event_id == root.event_id,
                "causal parent linkage",
            );
            require(
                root.object_id.is_some() && terminal.object_id == root.object_id,
                "object linkage",
            );
            require(
                root.actor.is_some() && terminal.actor == root.actor,
                "actor linkage",
            );
            require(
                root.lane.is_some() && terminal.lane == root.lane,
                "lane linkage",
            );
            require(terminal.confidence == root.confidence, "confidence linkage");
        }
        require(
            terminal_counts.get(terminal.trace_id.as_str()) == Some(&1),
            "terminal cardinality",
        );
        require(terminal.outcome.is_some(), "actual outcome");
        if terminal.verdict.as_deref() == Some("unclear") {
            require(
                terminal.semantic_success.is_none()
                    && terminal.prediction_error.is_none()
                    && terminal.brier.is_none(),
                "unclear calibration exclusion",
            );
        } else {
            let observed = if terminal.verdict.as_deref() == Some("hit") {
                1.0
            } else {
                0.0
            };
            require(
                terminal.semantic_success == Some(observed == 1.0),
                "outcome grade",
            );
            if let Some(confidence) = terminal
                .confidence
                .filter(|value| valid_probability(*value))
            {
                require(
                    terminal
                        .prediction_error
                        .is_some_and(|value| (value - (observed - confidence)).abs() <= EPSILON),
                    "prediction error",
                );
                require(
                    terminal.brier.is_some_and(|value| {
                        (value - (confidence - observed).powi(2)).abs() <= EPSILON
                    }),
                    "brier score",
                );
            } else {
                require(false, "graded confidence");
            }
        }
        match terminal.evaluator_id.as_deref() {
            Some("grounded-forecast-judge-v1") => {
                require(terminal.model_calls == Some(1), "model-call attribution");
                require(terminal.model_route.is_some(), "model route");
                require(terminal.latency_ms.is_some(), "judge latency");
            }
            Some("ledger-receipt-v1") => {
                require(terminal.model_calls == Some(0), "model-call attribution");
                require(terminal.model_route.is_none(), "receipt route exclusion");
                require(terminal.latency_ms.is_none(), "receipt latency exclusion");
            }
            _ => require(false, "evaluator identity"),
        }
        if row_complete {
            complete += 1;
        }
    }

    let total = terminals.len() + overdue_unclosed.len();
    let percent = 100.0 * complete as f64 / total as f64;
    let mut out = format!(
        "FORECAST CHAIN COMPLETENESS — {complete}/{total} latest forecast lifecycle(s) complete ({percent:.1}%; gate ≥99%)\n"
    );
    if defects.is_empty() {
        out.push_str("  no missing or mismatched forecast provenance in this sample\n");
    } else {
        for (field, count) in defects {
            out.push_str(&format!("  missing or mismatched {field}: {count}\n"));
        }
    }
    out
}

// ── calibration by confidence band ───────────────────────────────────────────

/// Render calibration tables from an event stream. Tool execution pairs are joined through span
/// linkage (`tool_predicted` → child `tool_observed`); hit/miss forecasts are already closed rows
/// in `prediction_graded`. The two families stay in separate tables so a strong tool prior cannot
/// hide weak world forecasting (or vice versa). Bands drifting below their predicted value are
/// overconfidence; above it, underconfidence (which wastes good tools and is real too).
pub fn render_calibration(events: &[DecisionEvent]) -> String {
    // Join predictions to outcomes through parent_event_id → predicted.event_id.
    let pred_by_event: std::collections::HashMap<&str, &DecisionEvent> = events
        .iter()
        .filter(|e| e.kind == "tool_predicted")
        .filter_map(|e| e.event_id.as_deref().map(|id| (id, e)))
        .collect();
    let mut tool_rows: Vec<(f64, f64)> = Vec::new(); // (predicted_confidence, observed 0/1)
    for o in events.iter().filter(|e| e.kind == "tool_observed") {
        let Some(vd) = &o.verdict else { continue };
        let observed = match vd.as_str() {
            "ok" | "empty" => 1.0,
            "failed" => 0.0,
            _ => continue, // unavailable/denied grade nothing here
        };
        if let Some(parent) = &o.parent_event_id {
            if let Some(p) = pred_by_event.get(parent.as_str()) {
                if let Some(c) = p.confidence.filter(|value| valid_probability(*value)) {
                    tool_rows.push((c, observed));
                }
            }
        }
    }
    let forecast_rows: Vec<(f64, f64)> = events
        .iter()
        .filter(|event| event.kind == "prediction_graded")
        .filter_map(|event| {
            let observed = match event.verdict.as_deref()? {
                "hit" => 1.0,
                "miss" => 0.0,
                _ => return None,
            };
            event
                .confidence
                .filter(|value| valid_probability(*value))
                .map(|confidence| (confidence, observed))
        })
        .collect();
    if tool_rows.is_empty() && forecast_rows.is_empty() {
        return "No graded predictions yet — calibration appears once a tool or forecast has an observed outcome.".into();
    }
    let mut out = String::from(
        "CALIBRATION BY CONFIDENCE BAND (predicted vs actually-observed; families separated):\n",
    );
    append_calibration_bands(&mut out, "tool execution", &tool_rows);
    append_calibration_bands(&mut out, "world forecasts", &forecast_rows);
    out
}

fn append_calibration_bands(out: &mut String, label: &str, rows: &[(f64, f64)]) {
    if rows.is_empty() {
        return;
    }
    let mut bands: [(Vec<f64>, Vec<f64>); 10] = Default::default();
    for (c, o) in rows {
        let b = ((c * 10.0).floor() as usize).clamp(0, 9);
        bands[b].0.push(*c);
        bands[b].1.push(*o);
    }
    out.push_str(&format!("  {label} (n={}):\n", rows.len()));
    for (b, (confs, outs)) in bands.iter().enumerate() {
        if confs.is_empty() {
            continue;
        }
        let mean_c = confs.iter().sum::<f64>() / confs.len() as f64;
        let rate = outs.iter().sum::<f64>() / outs.len() as f64;
        let brier = outs
            .iter()
            .zip(confs)
            .map(|(o, c)| (*c - *o).powi(2))
            .sum::<f64>()
            / outs.len() as f64;
        out.push_str(&format!(
            "    {:.0}-{:.0}%: n={:>2} · predicted {:.2} · observed {:.2} · brier {:.3}{}\n",
            b * 10,
            b * 10 + 10,
            outs.len(),
            mean_c,
            rate,
            brier,
            if (rate - mean_c).abs() <= 0.15 {
                ""
            } else if rate < mean_c {
                "  ← OVERCONFIDENT"
            } else {
                "  ← underconfident"
            }
        ));
    }
}

/// GOAL CONTRIBUTION report: aggregate `tool_goal_graded` events per tool across all runs.
/// This is where "search_web executes 94% of the time" grows into "…and materially advanced
/// its goal in K of N graded runs" — the third success kind, measured from persisted verdicts.
pub fn render_goal_contribution(events: &[DecisionEvent]) -> String {
    let mut rows: std::collections::BTreeMap<String, (usize, usize)> = Default::default(); // tool -> (contributed, graded)
    for e in events.iter().filter(|e| e.kind == "tool_goal_graded") {
        let tool = e
            .object_id
            .as_deref()
            .unwrap_or("?")
            .trim_start_matches("tool:");
        let row = rows.entry(tool.to_string()).or_insert((0, 0));
        row.1 += 1;
        if e.verdict.as_deref() == Some("evidence_used") {
            row.0 += 1;
        }
    }
    if rows.is_empty() {
        return "No evidence-utilization grades yet — tools are graded when a cognitive run completes with cited evidence.".into();
    }
    let mut out = String::from("EVIDENCE UTILIZATION (of graded runs — a proxy for goal contribution: did the run's findings cite this tool's output? NOT yet causal goal contribution):\n");
    let mut any = false;
    for (tool, (contributed, graded)) in &rows {
        if *graded >= 3 {
            any = true;
            out.push_str(&format!(
                "  {tool}: {contributed}/{graded} ({:.0}%)\n",
                100.0 * *contributed as f64 / *graded as f64
            ));
        }
    }
    if !any {
        // Show raw counts until samples exist — never hide that the number is too young to trust.
        for (tool, (contributed, graded)) in &rows {
            out.push_str(&format!(
                "  {tool}: {contributed}/{graded} (too few runs to rank)\n"
            ));
        }
    }
    out
}

/// One pack's local ladder, recounted from the flight recorder — the witness that fails
/// independently of the SQL counters in `mind_pack_stats` (Doctrine 3: two mechanisms, one truth).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackCounts {
    pub surfaced: usize,
    pub used: usize,
    pub unused: usize,
    /// Graded rows split by whether the reply had used the evidence — the split Doctrine 2 needs:
    /// if grading happens more often after a used row than an unused one (or the reverse), the
    /// observation of outcomes is selective and every rate below is a rate on a biased sample.
    pub graded_used: usize,
    pub graded_unused: usize,
    pub good: usize,
}

impl PackCounts {
    pub fn graded(&self) -> usize {
        self.graded_used + self.graded_unused
    }
}

/// Per-pack counts from `pack_surfaced` / `pack_evidence_used` / `pack_evidence_graded` events.
pub fn pack_evidence_counts(
    events: &[DecisionEvent],
) -> std::collections::BTreeMap<String, PackCounts> {
    let mut rows: std::collections::BTreeMap<String, PackCounts> = Default::default();
    for e in events {
        let Some(pack) = e.object_id.as_deref().and_then(|o| o.strip_prefix("pack:")) else {
            continue;
        };
        let row = rows.entry(pack.to_string()).or_default();
        match e.kind.as_str() {
            "pack_surfaced" => row.surfaced += 1,
            "pack_evidence_used" => match e.verdict.as_deref() {
                Some("used") => row.used += 1,
                _ => row.unused += 1,
            },
            "pack_evidence_graded" => {
                match e.semantic_success {
                    Some(true) => row.graded_used += 1,
                    _ => row.graded_unused += 1,
                }
                if e.verdict.as_deref() == Some("accepted") {
                    row.good += 1;
                }
            }
            _ => {}
        }
    }
    rows
}

/// `ym why packs` — every pack's local ladder with its denominators, the censoring rate, and the
/// Doctrine 2 audit ABOVE the rates: whether outcomes were observed as often for used evidence as
/// for unused. A rate printed under a selective-observation warning is a rate the reader has been
/// told not to trust.
pub fn render_pack_evidence(events: &[DecisionEvent]) -> String {
    let rows = pack_evidence_counts(events);
    if rows.is_empty() {
        return "No pack evidence recorded yet — rows appear when a mounted pack's evidence reaches a turn.".into();
    }
    let mut out = String::from(
        "PACK EVIDENCE (flight recorder; per pack: surfaced → used [word-overlap proxy, not causal use] → graded by the next message → accepted [tacit; weaker than a correction]):\n",
    );
    for (pack, c) in &rows {
        let graded = c.graded();
        let p_used = if c.used > 0 {
            Some(c.graded_used as f64 / c.used as f64)
        } else {
            None
        };
        let p_unused = if c.unused > 0 {
            Some(c.graded_unused as f64 / c.unused as f64)
        } else {
            None
        };
        let audit = match (p_used, p_unused) {
            (Some(a), Some(b)) if c.used >= 5 && c.unused >= 5 && (a - b).abs() >= 0.15 => format!(
                "  ⚠ SELECTIVE OBSERVATION: P(graded | used) = {:.0}% vs P(graded | unused) = {:.0}% — the rates below stand on a biased sample\n",
                a * 100.0,
                b * 100.0
            ),
            (Some(a), Some(b)) if c.used >= 5 && c.unused >= 5 => {
                format!("  observation audit: P(graded | used) = {:.0}% vs P(graded | unused) = {:.0}% — comparable\n", a * 100.0, b * 100.0)
            }
            _ => "  observation audit: too few rows on one side to compare (needs ≥5 used and ≥5 unused)\n".to_string(),
        };
        out.push_str(&format!(
            "  {pack}: surfaced {} · used {} of {} surfaced · graded {} of {} surfaced ({} after use, {} after non-use) · accepted {} of {} graded · censored {} of {} surfaced never graded\n",
            c.surfaced,
            c.used,
            c.surfaced,
            graded,
            c.surfaced,
            c.graded_used,
            c.graded_unused,
            c.good,
            graded,
            c.surfaced.saturating_sub(graded),
            c.surfaced
        ));
        out.push_str(&audit);
    }
    out
}

/// `ym why routes` — the shadowed coverage router's record: how often it would have leased and
/// why it abstained, and the free consistency witness a shadow gives: per trace, did the pack it
/// would have leased match the pack whose rows actually cleared the floor? Neither instrument is
/// ground truth; where they disagree, one of them is reading coverage differently from the corpus,
/// and the labelled set (E.PK3) says which.
pub fn render_pack_routes(events: &[DecisionEvent]) -> String {
    let routes: Vec<&DecisionEvent> = events
        .iter()
        .filter(|e| e.kind == "pack_route_shadow")
        .collect();
    if routes.is_empty() {
        return "No shadow routes recorded yet — one is written per turn, every lane, even with no packs (abstain:no_packs) and when the router fails (abstain:router_error).".into();
    }
    let mut by_verdict: std::collections::BTreeMap<String, usize> = Default::default();
    for r in &routes {
        *by_verdict
            .entry(r.verdict.clone().unwrap_or_else(|| "?".into()))
            .or_insert(0) += 1;
    }
    // Per trace: the pack that surfaced (P.2's witness) vs the pack the router would have leased.
    let surfaced: std::collections::HashMap<&str, Vec<&str>> = events
        .iter()
        .filter(|e| e.kind == "pack_surfaced")
        .fold(Default::default(), |mut m, e| {
            if let Some(p) = e.object_id.as_deref() {
                m.entry(e.trace_id.as_str()).or_default().push(p);
            }
            m
        });
    let (mut agree, mut lease_nothing_surfaced, mut abstain_something_surfaced, mut disagree) =
        (0usize, 0usize, 0usize, 0usize);
    for r in &routes {
        let s = surfaced.get(r.trace_id.as_str());
        match (&r.chosen, s) {
            (Some(c), Some(ps)) if ps.iter().any(|p| p == c) => agree += 1,
            (Some(_), Some(_)) => disagree += 1,
            (Some(_), None) => lease_nothing_surfaced += 1,
            (None, Some(_)) => abstain_something_surfaced += 1,
            (None, None) => agree += 1, // abstained, and nothing cleared the floor either
        }
    }
    let policy = routes
        .last()
        .and_then(|r| r.policy.first().cloned())
        .unwrap_or_else(|| "?".into());
    let members = routes
        .iter()
        .filter(|r| {
            r.lane.as_deref() == Some("member")
                || (r.lane.is_none() && r.actor.as_deref() == Some("member"))
        })
        .count();
    let mut out = format!(
        "SHADOW ROUTES ({policy}; recorded, never acted on) — {} turn(s), {} of them member lane:\n",
        routes.len(),
        members
    );
    for (v, n) in &by_verdict {
        out.push_str(&format!("  {v}: {n}\n"));
    }
    out.push_str(&format!(
        "  against the floor (P.2's witness, same trace): agree {agree} · would-lease but nothing surfaced {lease_nothing_surfaced} · abstained while something surfaced {abstain_something_surfaced} · different pack {disagree}\n"
    ));
    out.push_str("  (agreement here is consistency between two instruments, not correctness — the labelled set in mind-evals is the bar)");
    out
}

/// POLICY-DISAGREEMENT cohort report: every recorded `selection_flipped` — the cases where the
/// learned reliability ranking overruled the legacy semantic-only ranking. These are the only
/// decisions where the two policies differ, so they are where a policy improvement (or rot)
/// shows up undiluted. Outcome join is pending trace linkage; today's table is frequency by
/// pair and by the evidence strength behind the flip.
pub fn render_policy_flips(events: &[DecisionEvent]) -> String {
    let flips: Vec<&DecisionEvent> = events
        .iter()
        .filter(|e| e.kind == "selection_flipped")
        .collect();
    if flips.is_empty() {
        return "No policy disagreements recorded yet — flips appear when measured history first overrules semantics.".into();
    }
    let mut pairs: std::collections::BTreeMap<String, usize> = Default::default();
    let mut bands: [(usize, usize); 10] = Default::default(); // (count, with strong evidence n>=10)
    let mut unknown_confidence = 0usize;
    for f in &flips {
        let legacy = f.rejected.first().map_or_else(
            || "?".into(),
            |r| r.split_whitespace().next().unwrap_or("?").to_string(),
        );
        let selected = f.chosen.as_deref().unwrap_or("?");
        *pairs.entry(format!("{legacy} → {selected}")).or_insert(0) += 1;
        let n_strong = f
            .policy
            .iter()
            .find_map(|p| {
                p.strip_prefix("empirical prior n=")
                    .and_then(|n| n.split(' ').next())
                    .and_then(|n| n.parse::<u64>().ok())
            })
            .unwrap_or(0);
        if let Some(confidence) = f.confidence.filter(|value| valid_probability(*value)) {
            let b = ((confidence * 10.0).floor() as usize).clamp(0, 9);
            bands[b].0 += 1;
            if n_strong >= 10 {
                bands[b].1 += 1;
            }
        } else {
            unknown_confidence += 1;
        }
    }
    let mut out = format!(
        "POLICY DISAGREEMENTS ({}) — learned ranking vs legacy semantic-only:\n",
        flips.len()
    );
    for (pair, n) in &pairs {
        out.push_str(&format!("  {pair}: {n}×\n"));
    }
    out.push_str("  by chosen-prior band (high-evidence flips are the trustworthy subset):\n");
    for (b, (total, strong)) in bands.iter().enumerate() {
        if *total > 0 {
            out.push_str(&format!(
                "    {:.0}-{:.0}%: {total} flips · {strong} backed by n≥10\n",
                b * 10,
                b * 10 + 10
            ));
        }
    }
    if unknown_confidence > 0 {
        out.push_str(&format!(
            "    unknown: {unknown_confidence} flips (missing or invalid confidence)\n"
        ));
    }
    out.push_str(
        "  outcome join pending: grade these traces when their goals complete to compute Y vs X.\n",
    );
    out
}

/// Render one trace's causal path in human-readable form — persisted evidence, never narration.
pub fn render_trace(events: &[DecisionEvent]) -> String {
    if events.is_empty() {
        return "no recorded events under this trace".to_string();
    }
    let mut out = String::new();
    for (i, e) in events.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&format!(
            "[{}] {} · trace {} · actor {}",
            i + 1,
            e.kind,
            e.trace_id,
            e.actor.as_deref().unwrap_or("?")
        ));
        if let Some(id) = &e.event_id {
            out.push_str(&format!("\n    span: {id}"));
        }
        if let Some(lane) = &e.lane {
            out.push_str(&format!("\n    lane: {lane}"));
        }
        if let Some(s) = &e.subject {
            out.push_str(&format!(" · subject {s}"));
        }
        if let Some(p) = &e.purpose {
            out.push_str(&format!(" · purpose {p}"));
        }
        if let Some(context) = &e.context_fingerprint {
            out.push_str(&format!("\n    context: {context}"));
        }
        if let Some(goal_id) = &e.goal_id {
            out.push_str(&format!("\n    goal id: {goal_id}"));
        }
        let field = |out: &mut String, label: &str, v: &Option<String>| {
            if let Some(x) = v {
                out.push_str(&format!("\n    {label}: {x}"));
            }
        };
        field(&mut out, "goal", &e.goal);
        field(&mut out, "trigger", &e.trigger);
        if !e.evidence_ids.is_empty() {
            out.push_str(&format!("\n    evidence: {}", e.evidence_ids.join(", ")));
        }
        if !e.candidates.is_empty() {
            out.push_str(&format!("\n    considered: {}", e.candidates.join("; ")));
        }
        field(&mut out, "chose", &e.chosen);
        if !e.rejected.is_empty() {
            out.push_str(&format!("\n    rejected: {}", e.rejected.join("; ")));
        }
        if !e.policy.is_empty() {
            out.push_str(&format!("\n    policy: {}", e.policy.join(", ")));
        }
        if e.predicted.is_some() || e.confidence.is_some() {
            out.push_str(&format!(
                "\n    predicted: {} (confidence {})",
                e.predicted.as_deref().unwrap_or("?"),
                e.confidence
                    .map_or_else(|| "?".into(), |c| format!("{c:.2}"))
            ));
        }
        field(&mut out, "outcome", &e.outcome);
        field(&mut out, "verdict", &e.verdict);
        if let Some(err) = e.prediction_error {
            out.push_str(&format!("\n    prediction error: {err:+.3}"));
        }
        if let Some(brier) = e.brier {
            out.push_str(&format!("\n    brier: {brier:.3}"));
        }
        if let Some(success) = e.semantic_success {
            out.push_str(&format!("\n    semantic success: {success}"));
        }
        if let Some(latency_ms) = e.latency_ms {
            out.push_str(&format!("\n    latency: {latency_ms} ms"));
        }
        if let Some(tool_version) = &e.tool_version {
            out.push_str(&format!("\n    tool version: {tool_version}"));
        }
        if let Some(model_route) = &e.model_route {
            out.push_str(&format!("\n    model route: {model_route}"));
        }
        if let Some(model_calls) = e.model_calls {
            out.push_str(&format!("\n    model calls: {model_calls}"));
        }
        if let Some(evaluator) = &e.evaluator_id {
            out.push_str(&format!("\n    evaluator: {evaluator}"));
        }
        field(&mut out, "lesson", &e.lesson);
        if let Some(obj) = &e.object_id {
            out.push_str(&format!("\n    object: {obj}"));
        }
        if let Some(p) = &e.parent_event_id {
            out.push_str(&format!("\n    parent span: {p}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_ids_are_stable_and_never_embed_the_source_value() {
        let sensitive = "query with ghp_SECRET12345 and private context";
        let id = opaque_id("discover", sensitive);
        assert!(id.starts_with("discover:"), "{id}");
        assert_eq!(id.len(), "discover:".len() + 32, "128-bit digest: {id}");
        assert!(!id.contains("ghp_"), "{id}");
        assert!(!id.contains("private"), "{id}");
        assert_eq!(id, opaque_id("discover", sensitive));
        assert_ne!(id, opaque_id("discover", "a different query"));
        assert_ne!(id, opaque_id("tool", sensitive));
    }

    #[test]
    fn invalid_numeric_metrics_are_dropped_instead_of_poisoning_the_ledger() {
        let mut invalid = DecisionEvent::new("metrics", "graded");
        invalid.confidence = Some(f64::NAN);
        invalid.prediction_error = Some(f64::INFINITY);
        invalid.brier = Some(-0.01);
        let invalid = invalid.sanitized();
        assert_eq!(invalid.confidence, None);
        assert_eq!(invalid.prediction_error, None);
        assert_eq!(invalid.brier, None);

        let mut valid = DecisionEvent::new("metrics", "graded");
        valid.confidence = Some(1.0);
        valid.prediction_error = Some(-2.5);
        valid.brier = Some(0.0);
        let valid = valid.sanitized();
        assert_eq!(valid.confidence, Some(1.0));
        assert_eq!(valid.prediction_error, Some(-2.5));
        assert_eq!(valid.brier, Some(0.0));
    }

    #[test]
    fn calibration_ignores_invalid_legacy_confidence() {
        let mut prediction = DecisionEvent::new("metrics", "tool_predicted");
        prediction.event_id = Some("prediction-1".into());
        prediction.confidence = Some(1.5);
        let mut observation = DecisionEvent::new("metrics", "tool_observed");
        observation.parent_event_id = prediction.event_id.clone();
        observation.verdict = Some("ok".into());

        assert!(render_calibration(&[prediction, observation]).starts_with("No graded"));
    }

    #[test]
    fn calibration_keeps_tool_execution_and_world_forecasts_separate() {
        let mut prediction = DecisionEvent::new("metrics", "tool_predicted");
        prediction.event_id = Some("prediction-1".into());
        prediction.confidence = Some(0.8);
        let mut observation = DecisionEvent::new("metrics", "tool_observed");
        observation.parent_event_id = prediction.event_id.clone();
        observation.verdict = Some("failed".into());

        let mut forecast = DecisionEvent::new("forecast", "prediction_graded");
        forecast.confidence = Some(0.7);
        forecast.verdict = Some("hit".into());

        let report = render_calibration(&[prediction, observation, forecast]);
        assert!(report.contains("tool execution (n=1)"), "{report}");
        assert!(report.contains("world forecasts (n=1)"), "{report}");
        assert!(
            report.contains("70-80%: n= 1 · predicted 0.70 · observed 1.00 · brier 0.090"),
            "{report}"
        );
        assert!(
            report.contains("80-90%: n= 1 · predicted 0.80 · observed 0.00 · brier 0.640"),
            "{report}"
        );
    }

    #[test]
    fn policy_flip_report_does_not_invent_confidence_for_unknown_values() {
        let flip = |confidence| {
            let mut event = DecisionEvent::new("metrics", "selection_flipped");
            event.chosen = Some("learned".into());
            event.rejected = vec!["legacy score=0.8".into()];
            event.confidence = confidence;
            event
        };
        let report = render_policy_flips(&[flip(Some(0.75)), flip(None), flip(Some(1.5))]);

        assert!(report.contains("70-80%: 1 flips"), "{report}");
        assert!(
            report.contains("unknown: 2 flips (missing or invalid confidence)"),
            "{report}"
        );
        assert!(!report.contains("50-60%"), "{report}");
    }

    #[test]
    fn evaluator_coverage_counts_only_events_that_carry_outcome_evidence() {
        let mut stamped = DecisionEvent::new("metrics", "tool_observed");
        stamped.outcome = Some("worked".into());
        stamped.evaluator_id = Some("tool-outcome-v1".into());
        let mut missing = DecisionEvent::new("metrics", "packet_resolved");
        missing.semantic_success = Some(true);
        let mut route = DecisionEvent::new("metrics", "pack_route_shadow");
        route.verdict = Some("lease".into());

        let report = render_evaluator_coverage(&[stamped, missing, route]);
        assert!(report.contains("1/2 graded event(s) stamped"), "{report}");
        assert!(report.contains("tool-outcome-v1: 1"), "{report}");
        assert!(report.contains("missing evaluator_id: 1"), "{report}");
        assert!(render_evaluator_coverage(&[]).starts_with("No graded events"));
    }

    #[test]
    fn lane_coverage_keeps_actor_and_lane_distinct_and_reports_missing_rows() {
        let mut primary = DecisionEvent::new("lanes", "pack_route_shadow");
        primary.actor = Some("conversation".into());
        primary.lane = Some("primary".into());
        let mut member = primary.clone();
        member.lane = Some("member".into());
        let unstamped = DecisionEvent::new("lanes", "packet_created");

        let report = render_lane_coverage(&[primary, member, unstamped]);
        assert!(report.contains("2/3 recorded event(s) stamped"), "{report}");
        assert!(report.contains("conversation / primary: 1"), "{report}");
        assert!(report.contains("conversation / member: 1"), "{report}");
        assert!(report.contains("missing lane: 1"), "{report}");
        assert!(render_lane_coverage(&[]).starts_with("No recorded events"));
    }

    #[test]
    fn latency_coverage_excludes_non_calls_and_reports_percentiles_and_missing_rows() {
        let mut fast = DecisionEvent::new("latency", "tool_observed");
        fast.verdict = Some("ok".into());
        fast.latency_ms = Some(12);
        let mut slow = DecisionEvent::new("latency", "tool_observed");
        slow.verdict = Some("failed".into());
        slow.latency_ms = Some(120);
        let mut legacy = DecisionEvent::new("latency", "tool_observed");
        legacy.verdict = Some("empty".into());
        let mut denied = DecisionEvent::new("latency", "tool_observed");
        denied.verdict = Some("denied".into());

        let report = render_latency_coverage(&[fast, slow, legacy, denied]);
        assert!(report.contains("2/3 executed call(s) timed"), "{report}");
        assert!(report.contains("p50: 12 ms"), "{report}");
        assert!(report.contains("p95: 120 ms"), "{report}");
        assert!(report.contains("missing latency_ms: 1"), "{report}");
        assert!(render_latency_coverage(&[]).starts_with("No executed tool observations"));
    }

    #[test]
    fn semantic_coverage_is_kind_specific_and_excludes_non_semantic_outcomes() {
        let mut ok = DecisionEvent::new("semantic", "tool_observed");
        ok.verdict = Some("ok".into());
        ok.semantic_success = Some(true);
        let mut legacy_empty = DecisionEvent::new("semantic", "tool_observed");
        legacy_empty.verdict = Some("empty".into());
        let mut failed = DecisionEvent::new("semantic", "tool_observed");
        failed.verdict = Some("failed".into());
        let mut denied = DecisionEvent::new("semantic", "tool_observed");
        denied.verdict = Some("denied".into());
        let mut contribution = DecisionEvent::new("semantic", "tool_goal_graded");
        contribution.semantic_success = Some(false);
        let mut pack_use = DecisionEvent::new("semantic", "pack_evidence_used");
        pack_use.semantic_success = Some(true);
        let mut forecast = DecisionEvent::new("semantic", "prediction_graded");
        forecast.verdict = Some("hit".into());
        let mut unclear = DecisionEvent::new("semantic", "prediction_graded");
        unclear.verdict = Some("unclear".into());

        let report = render_semantic_coverage(&[
            ok,
            legacy_empty,
            failed,
            denied,
            contribution,
            pack_use,
            forecast,
            unclear,
        ]);
        assert!(
            report.contains("3/5 assessable outcome(s) graded"),
            "{report}"
        );
        assert!(report.contains("pack_evidence_used: 1/1"), "{report}");
        assert!(report.contains("prediction_graded: 0/1"), "{report}");
        assert!(report.contains("tool_goal_graded: 1/1"), "{report}");
        assert!(report.contains("tool_observed: 1/2"), "{report}");
        assert!(report.contains("missing semantic_success: 2"), "{report}");
        assert!(render_semantic_coverage(&[]).starts_with("No semantic-grade candidates"));
    }

    #[test]
    fn context_coverage_counts_without_printing_fingerprints() {
        let context = opaque_id("context", "private user request");
        let mut grounding = DecisionEvent::new("context", "grounding_assembled");
        grounding.context_fingerprint = Some(context.clone());
        let mut predicted = DecisionEvent::new("context", "tool_predicted");
        predicted.context_fingerprint = Some(context.clone());
        let mut observed = DecisionEvent::new("context", "tool_observed");
        observed.context_fingerprint = Some(context.clone());
        let mut compile = DecisionEvent::new("context", "goal_compiled");
        compile.context_fingerprint = Some(context.clone());
        let mut run = DecisionEvent::new("context", "cognitive_run");
        run.context_fingerprint = Some(context.clone());
        let mut refused = DecisionEvent::new("context", "cognitive_run_refused");
        refused.context_fingerprint = Some(context.clone());
        let mut grade = DecisionEvent::new("context", "tool_goal_graded");
        grade.context_fingerprint = Some(context.clone());
        let mut pack_used = DecisionEvent::new("context", "pack_evidence_used");
        pack_used.context_fingerprint = Some(context.clone());
        let legacy_pack = DecisionEvent::new("context", "pack_surfaced");
        let legacy = DecisionEvent::new("context", "tool_observed");

        let report = render_context_coverage(&[
            grounding,
            predicted,
            observed,
            compile,
            run,
            refused,
            grade,
            pack_used,
            legacy_pack,
            legacy,
        ]);
        assert!(
            report.contains("8/10 eligible event(s) stamped"),
            "{report}"
        );
        assert!(report.contains("1 distinct context(s)"), "{report}");
        assert!(
            report.contains("missing context_fingerprint: 2"),
            "{report}"
        );
        assert!(
            !report.contains(&context),
            "fingerprints stay out of aggregate reports"
        );
        assert!(render_context_coverage(&[]).starts_with("No context-linked"));
    }

    #[test]
    fn stable_goal_coverage_ignores_free_text_and_reports_missing_ids() {
        let mut run = DecisionEvent::new("goal", "cognitive_run");
        run.goal = Some("same words are not identity".into());
        run.goal_id = Some("goal-17".into());
        let mut tool = DecisionEvent::new("goal", "tool_observed");
        tool.goal_id = Some("goal-17".into());
        let mut legacy = DecisionEvent::new("goal", "tool_predicted");
        legacy.goal = Some("goal-17".into());
        let mut packet_created = DecisionEvent::new("packet", "packet_created");
        packet_created.goal_id = Some("node:future".into());
        let mut packet_resolved = DecisionEvent::new("packet", "packet_resolved");
        packet_resolved.goal_id = Some("node:future".into());
        let packet_expired = DecisionEvent::new("legacy-packet", "packet_expired");

        let report = render_goal_id_coverage(&[
            run,
            tool,
            legacy,
            packet_created,
            packet_resolved,
            packet_expired,
        ]);
        assert!(
            report.contains("4/6 goal-linked event(s) stamped"),
            "{report}"
        );
        assert!(report.contains("2 distinct goal(s)"), "{report}");
        assert!(report.contains("missing goal_id: 2"), "{report}");
        assert!(render_goal_id_coverage(&[]).starts_with("No goal-linked events"));
    }

    #[test]
    fn tool_version_coverage_groups_stamps_and_exposes_legacy_rows() {
        let mut predicted = DecisionEvent::new("versions", "tool_predicted");
        predicted.tool_version = Some("mind-conversation/0.1.0".into());
        let mut observed = DecisionEvent::new("versions", "tool_observed");
        observed.tool_version = Some("mind-conversation/0.1.0".into());
        let legacy = DecisionEvent::new("versions", "tool_observed");

        let report = render_tool_version_coverage(&[predicted, observed, legacy]);
        assert!(
            report.contains("2/3 tool-chain event(s) stamped"),
            "{report}"
        );
        assert!(report.contains("mind-conversation/0.1.0: 2"), "{report}");
        assert!(report.contains("missing tool_version: 1"), "{report}");
        assert!(render_tool_version_coverage(&[]).starts_with("No tool-chain events"));
    }

    #[test]
    fn model_route_coverage_is_explicit_about_configured_not_served_identity() {
        let mut run = DecisionEvent::new("models", "cognitive_run");
        run.model_route = Some("util=nim:model;chat=ollama-local:model".into());
        let mut tool = DecisionEvent::new("models", "tool_observed");
        tool.model_route = Some("ollama-local:model".into());
        let legacy = DecisionEvent::new("models", "tool_predicted");
        let mut grounded_forecast = DecisionEvent::new("models", "prediction_graded");
        grounded_forecast.evaluator_id = Some("grounded-forecast-judge-v1".into());
        grounded_forecast.model_route = Some("util=nim:model;research=remote:model".into());
        let mut receipt = DecisionEvent::new("models", "prediction_graded");
        receipt.evaluator_id = Some("ledger-receipt-v1".into());

        let report = render_model_route_coverage(&[run, tool, legacy, grounded_forecast, receipt]);
        assert!(
            report.contains("3/4 model-mediated event(s) stamped"),
            "{report}"
        );
        assert!(
            report.contains("util=nim:model;research=remote:model: 1"),
            "{report}"
        );
        assert!(report.contains("missing model_route: 1"), "{report}");
        assert!(report.contains("configured route only"), "{report}");
        assert!(render_model_route_coverage(&[]).starts_with("No model-mediated events"));
    }

    #[test]
    fn model_call_resources_keep_the_cost_boundary_explicit() {
        let mut grounding = DecisionEvent::new("resources", "grounding_assembled");
        grounding.latency_ms = Some(25);
        let mut compile = DecisionEvent::new("resources", "goal_compiled");
        compile.model_calls = Some(1);
        compile.latency_ms = Some(50);
        let mut first = DecisionEvent::new("resources", "cognitive_run");
        first.model_calls = Some(3);
        first.latency_ms = Some(120);
        let mut second = DecisionEvent::new("resources", "cognitive_run");
        second.model_calls = Some(7);
        second.latency_ms = Some(900);
        let legacy = DecisionEvent::new("resources", "cognitive_run");
        let mut forecast = DecisionEvent::new("resources", "prediction_graded");
        forecast.model_calls = Some(1);
        forecast.latency_ms = Some(80);
        forecast.evaluator_id = Some("grounded-forecast-judge-v1".into());
        let mut receipt = DecisionEvent::new("resources", "prediction_graded");
        receipt.model_calls = Some(0);
        receipt.evaluator_id = Some("ledger-receipt-v1".into());
        let legacy_forecast = DecisionEvent::new("resources", "prediction_graded");

        let report = render_model_call_resources(&[
            grounding,
            compile,
            first,
            second,
            legacy,
            forecast,
            receipt,
            legacy_forecast,
        ]);
        assert!(
            report.contains("grounding assembly — 1/1 event(s) timed · p95: 25 ms · max: 25 ms"),
            "{report}"
        );
        assert!(
            report.contains(
                "compile — 1/1 event(s) counted, 1/1 timed · calls: 1 · p95: 50 ms · max: 50 ms"
            ),
            "{report}"
        );
        assert!(
            report.contains("bounded run — 2/3 event(s) counted, 2/3 timed"),
            "{report}"
        );
        assert!(
            report.contains("model calls — total: 10 · mean: 5.0 · max: 7"),
            "{report}"
        );
        assert!(
            report.contains("wall time — p50: 120 ms · p95: 900 ms · max: 900 ms"),
            "{report}"
        );
        assert!(report.contains("missing model_calls: 1"), "{report}");
        assert!(report.contains("missing latency_ms: 1"), "{report}");
        assert!(
            report.contains("forecast grading — 2/3 event(s) counted · model calls: 1 · grounded judges: 1 · ledger receipts: 1"),
            "{report}"
        );
        assert!(
            report.contains("judge latency — 1/1 event(s) timed · p95: 80 ms · max: 80 ms"),
            "{report}"
        );
        assert_eq!(
            report.matches("missing model_calls: 1").count(),
            2,
            "{report}"
        );
        assert!(report.contains("tokens, monetary cost"), "{report}");
        assert!(render_model_call_resources(&[]).starts_with("No grounding, compiled"));
    }

    #[test]
    fn tool_chain_completeness_reports_complete_pairs_and_actionable_defects() {
        let compile = DecisionEvent::span("chain", None, "goal_compiled");
        let mut prediction =
            DecisionEvent::span("chain", compile.event_id.as_deref(), "tool_predicted");
        prediction.actor = Some("conversation".into());
        prediction.lane = Some("primary".into());
        prediction.context_fingerprint = Some("context:opaque".into());
        prediction.goal_id = Some("goal-1".into());
        prediction.tool_version = Some("mind-conversation/0.1.0".into());
        prediction.model_route = Some("util=scripted;chat=scripted;research=scripted".into());
        prediction.object_id = Some("calc:opaque".into());
        prediction.predicted = Some("usable output".into());
        prediction.confidence = Some(0.5);
        let mut observation =
            DecisionEvent::span("chain", prediction.event_id.as_deref(), "tool_observed");
        observation.actor = prediction.actor.clone();
        observation.lane = prediction.lane.clone();
        observation.context_fingerprint = prediction.context_fingerprint.clone();
        observation.goal_id = prediction.goal_id.clone();
        observation.tool_version = prediction.tool_version.clone();
        observation.model_route = prediction.model_route.clone();
        observation.object_id = prediction.object_id.clone();
        observation.verdict = Some("ok".into());
        observation.evaluator_id = Some("tool-outcome-v1".into());
        observation.latency_ms = Some(3);
        observation.semantic_success = Some(true);

        let mut orphan = DecisionEvent::new("legacy", "tool_observed");
        orphan.parent_event_id = Some("missing".into());
        orphan.verdict = Some("failed".into());

        let unobserved = DecisionEvent::span("abandoned", None, "tool_predicted");

        let report = render_tool_chain_completeness(&[
            compile.clone(),
            prediction.clone(),
            observation.clone(),
            orphan,
            unobserved,
        ]);
        assert!(
            report.contains("1/3 latest call(s) complete (33.3%; gate ≥99%)"),
            "{report}"
        );
        assert!(
            report.contains("missing or mismatched prediction link: 1"),
            "{report}"
        );
        assert!(
            report.contains("missing or mismatched observation link: 1"),
            "{report}"
        );
        assert!(
            report.contains("missing or mismatched evaluator_id: 1"),
            "{report}"
        );
        assert!(
            report.contains("missing or mismatched latency_ms: 1"),
            "{report}"
        );

        prediction.parent_event_id = Some("not-the-compile-root".into());
        let broken = render_tool_chain_completeness(&[compile, prediction, observation]);
        assert!(broken.contains("0/1 latest call(s) complete"), "{broken}");
        assert!(
            broken.contains("missing or mismatched bounded root linkage: 1"),
            "{broken}"
        );
        assert!(render_tool_chain_completeness(&[]).starts_with("No tool-chain calls"));
    }

    #[test]
    fn tool_chain_completeness_rejects_two_observations_for_one_prediction() {
        let mut prediction = DecisionEvent::span("duplicate", None, "tool_predicted");
        prediction.actor = Some("conversation".into());
        prediction.lane = Some("primary".into());
        prediction.context_fingerprint = Some("context:opaque".into());
        prediction.goal_id = Some("goal-1".into());
        prediction.tool_version = Some("mind-conversation/0.1.0".into());
        prediction.model_route = Some("util=scripted;chat=scripted;research=scripted".into());
        prediction.object_id = Some("calc:opaque".into());
        prediction.predicted = Some("usable output".into());
        prediction.confidence = Some(0.5);
        let observation = |event_id: &str| {
            let mut event = DecisionEvent::new("duplicate", "tool_observed");
            event.event_id = Some(event_id.into());
            event.parent_event_id = prediction.event_id.clone();
            event.actor = prediction.actor.clone();
            event.lane = prediction.lane.clone();
            event.context_fingerprint = prediction.context_fingerprint.clone();
            event.goal_id = prediction.goal_id.clone();
            event.tool_version = prediction.tool_version.clone();
            event.model_route = prediction.model_route.clone();
            event.object_id = prediction.object_id.clone();
            event.verdict = Some("ok".into());
            event.evaluator_id = Some("tool-outcome-v1".into());
            event.latency_ms = Some(3);
            event.semantic_success = Some(true);
            event
        };

        let first = observation("observation-1");
        let second = observation("observation-2");
        let report = render_tool_chain_completeness(&[prediction, first, second]);
        assert!(report.contains("0/2 latest call(s) complete"), "{report}");
        assert!(
            report.contains("missing or mismatched observation cardinality: 2"),
            "{report}"
        );
    }

    #[test]
    fn packet_chain_completeness_reports_causal_grades_and_duplicates() {
        let mut root = DecisionEvent::span("packet-1", None, "packet_created");
        root.object_id = Some("packet-1".into());
        root.goal_id = Some("node:one".into());
        root.actor = Some("proactive".into());
        root.lane = Some("primary".into());
        root.confidence = Some(0.8);
        root.policy = vec!["confirmation_required=false provenance=inferred expiry_ms=9".into()];
        let mut resolved =
            DecisionEvent::span("packet-1", root.event_id.as_deref(), "packet_resolved");
        resolved.verdict = Some("confirmed".into());
        resolved.object_id = root.object_id.clone();
        resolved.goal_id = root.goal_id.clone();
        resolved.actor = root.actor.clone();
        resolved.lane = root.lane.clone();
        resolved.semantic_success = Some(true);
        resolved.evaluator_id = Some("owner-packet-decision-v1".into());
        resolved.policy = vec!["expiry_ms=9".into()];

        let mut orphan = DecisionEvent::new("packet-2", "packet_expired");
        orphan.verdict = Some("expired".into());
        orphan.semantic_success = Some(false);

        let report = render_packet_chain_completeness(&[root.clone(), resolved.clone(), orphan]);
        assert!(
            report.contains("1/2 latest packet lifecycle(s) complete (50.0%; gate ≥99%)"),
            "{report}"
        );
        assert!(
            report.contains("missing or mismatched creation cardinality: 1"),
            "{report}"
        );
        assert!(
            report.contains("missing or mismatched evaluator identity: 1"),
            "{report}"
        );

        let mut wrong_expiry = resolved.clone();
        wrong_expiry.policy = vec!["expiry_ms=10".into()];
        let wrong_expiry_report = render_packet_chain_completeness(&[root, wrong_expiry]);
        assert!(
            wrong_expiry_report.contains("missing or mismatched expiry horizon linkage: 1"),
            "{wrong_expiry_report}"
        );

        let mut invalid_confidence_root =
            DecisionEvent::span("invalid-confidence", None, "packet_created");
        invalid_confidence_root.object_id = Some("invalid-confidence".into());
        invalid_confidence_root.goal_id = Some("node:confidence".into());
        invalid_confidence_root.actor = Some("proactive".into());
        invalid_confidence_root.lane = Some("primary".into());
        invalid_confidence_root.policy =
            vec!["confirmation_required=false provenance=told expiry_ms=9".into()];
        let mut invalid_confidence_terminal = DecisionEvent::span(
            "invalid-confidence",
            invalid_confidence_root.event_id.as_deref(),
            "packet_resolved",
        );
        invalid_confidence_terminal.object_id = invalid_confidence_root.object_id.clone();
        invalid_confidence_terminal.goal_id = invalid_confidence_root.goal_id.clone();
        invalid_confidence_terminal.actor = invalid_confidence_root.actor.clone();
        invalid_confidence_terminal.lane = invalid_confidence_root.lane.clone();
        invalid_confidence_terminal.verdict = Some("confirmed".into());
        invalid_confidence_terminal.semantic_success = Some(true);
        invalid_confidence_terminal.evaluator_id = Some("owner-packet-decision-v1".into());
        let invalid_confidence_report = render_packet_chain_completeness(&[
            invalid_confidence_root,
            invalid_confidence_terminal,
        ]);
        assert!(
            invalid_confidence_report.contains("0/1 latest packet lifecycle(s) complete"),
            "{invalid_confidence_report}"
        );
        assert!(
            invalid_confidence_report.contains("missing or mismatched packet confidence: 1"),
            "{invalid_confidence_report}"
        );

        let mut wrong_root = DecisionEvent::span("wrong-grade", None, "packet_created");
        wrong_root.object_id = Some("wrong-grade".into());
        wrong_root.goal_id = Some("node:wrong".into());
        wrong_root.actor = Some("proactive".into());
        wrong_root.lane = Some("primary".into());
        wrong_root.policy = vec!["confirmation_required=false provenance=guessed".into()];
        let mut wrong = DecisionEvent::span(
            "wrong-grade",
            wrong_root.event_id.as_deref(),
            "packet_expired",
        );
        wrong.object_id = wrong_root.object_id.clone();
        wrong.goal_id = wrong_root.goal_id.clone();
        wrong.actor = wrong_root.actor.clone();
        wrong.lane = wrong_root.lane.clone();
        wrong.verdict = Some("expired".into());
        wrong.semantic_success = Some(true);
        wrong.evaluator_id = Some("owner-packet-decision-v1".into());
        let wrong_report = render_packet_chain_completeness(&[wrong_root, wrong]);
        assert!(
            wrong_report.contains("0/1 latest packet lifecycle(s) complete"),
            "{wrong_report}"
        );
        assert!(
            wrong_report.contains("missing or mismatched evaluator identity: 1"),
            "{wrong_report}"
        );
        assert!(
            wrong_report.contains("missing or mismatched outcome grade: 1"),
            "{wrong_report}"
        );
        assert!(
            wrong_report.contains("missing or mismatched trigger provenance: 1"),
            "{wrong_report}"
        );
        assert!(
            wrong_report.contains("missing or mismatched expiry horizon: 1"),
            "{wrong_report}"
        );

        let mut duplicate_root = DecisionEvent::span("duplicate", None, "packet_created");
        duplicate_root.object_id = Some("duplicate".into());
        duplicate_root.goal_id = Some("node:duplicate".into());
        duplicate_root.actor = Some("proactive".into());
        duplicate_root.lane = Some("primary".into());
        duplicate_root.policy =
            vec!["confirmation_required=false provenance=told expiry_ms=9".into()];
        let duplicate_root_id = duplicate_root.event_id.clone();
        let duplicate_object_id = duplicate_root.object_id.clone();
        let duplicate_goal_id = duplicate_root.goal_id.clone();
        let duplicate_actor = duplicate_root.actor.clone();
        let duplicate_lane = duplicate_root.lane.clone();
        let terminal = |kind: &str| {
            let mut event = DecisionEvent::span("duplicate", duplicate_root_id.as_deref(), kind);
            event.verdict = Some("expired".into());
            event.object_id = duplicate_object_id.clone();
            event.goal_id = duplicate_goal_id.clone();
            event.actor = duplicate_actor.clone();
            event.lane = duplicate_lane.clone();
            event.semantic_success = Some(false);
            event.evaluator_id = Some("packet-expiry-clock-v1".into());
            event
        };
        let duplicated = render_packet_chain_completeness(&[
            duplicate_root,
            terminal("packet_expired"),
            terminal("packet_resolved"),
        ]);
        assert!(
            duplicated.contains("0/2 latest packet lifecycle(s) complete"),
            "{duplicated}"
        );
        assert!(
            duplicated.contains("missing or mismatched terminal cardinality: 2"),
            "{duplicated}"
        );
        let mut overdue = DecisionEvent::span("overdue", None, "packet_created");
        overdue.policy = vec!["provenance=inferred expiry_ms=0".into()];
        let overdue_report = render_packet_chain_completeness(&[overdue.clone()]);
        assert!(
            overdue_report.contains("0/1 latest packet lifecycle(s) complete"),
            "{overdue_report}"
        );
        assert!(
            overdue_report.contains("missing or mismatched terminal event: 1"),
            "{overdue_report}"
        );
        let mut duplicate_overdue = overdue.clone();
        duplicate_overdue.event_id = Some("duplicate-overdue-packet-root".into());
        let duplicate_overdue_report =
            render_packet_chain_completeness(&[overdue, duplicate_overdue]);
        assert!(
            duplicate_overdue_report.contains("0/1 latest packet lifecycle(s) complete"),
            "{duplicate_overdue_report}"
        );
        assert!(
            duplicate_overdue_report.contains("missing or mismatched creation cardinality: 1"),
            "{duplicate_overdue_report}"
        );
        assert!(render_packet_chain_completeness(&[]).starts_with("No packet closure"));
    }

    #[test]
    fn forecast_chain_completeness_checks_issued_probability_and_judge_provenance() {
        let mut root = DecisionEvent::span("prediction:1", None, "prediction_made");
        root.object_id = Some("prediction:1".into());
        root.actor = Some("foresight".into());
        root.lane = Some("primary".into());
        root.predicted = Some("threshold is met".into());
        root.confidence = Some(0.7);
        root.policy = vec!["resolve_by_ms=9223372036854775807".into()];
        let mut grade = DecisionEvent::span(
            "prediction:1",
            root.event_id.as_deref(),
            "prediction_graded",
        );
        grade.object_id = root.object_id.clone();
        grade.actor = root.actor.clone();
        grade.lane = root.lane.clone();
        grade.confidence = root.confidence;
        grade.policy = root.policy.clone();
        grade.verdict = Some("hit".into());
        grade.outcome = Some("the threshold was met".into());
        grade.semantic_success = Some(true);
        grade.prediction_error = Some(0.3);
        grade.brier = Some(0.09);
        grade.evaluator_id = Some("grounded-forecast-judge-v1".into());
        grade.model_calls = Some(1);
        grade.model_route = Some("util=local;research=remote".into());
        grade.latency_ms = Some(25);

        let complete = render_forecast_chain_completeness(&[root.clone(), grade.clone()]);
        assert!(
            complete.contains("1/1 latest forecast lifecycle(s) complete"),
            "{complete}"
        );

        let mut wrong_deadline = grade.clone();
        wrong_deadline.policy = vec!["resolve_by_ms=7".into()];
        let wrong_deadline_report =
            render_forecast_chain_completeness(&[root.clone(), wrong_deadline]);
        assert!(
            wrong_deadline_report.contains("missing or mismatched resolution deadline linkage: 1"),
            "{wrong_deadline_report}"
        );

        grade.confidence = Some(0.8);
        grade.brier = Some(0.04);
        let broken = render_forecast_chain_completeness(&[root, grade]);
        assert!(
            broken.contains("0/1 latest forecast lifecycle(s) complete"),
            "{broken}"
        );
        assert!(
            broken.contains("missing or mismatched confidence linkage: 1"),
            "{broken}"
        );
        assert!(
            broken.contains("missing or mismatched prediction error: 1"),
            "{broken}"
        );
        let mut overdue = DecisionEvent::span("prediction:overdue", None, "prediction_made");
        overdue.policy = vec!["resolve_by_ms=0".into()];
        let overdue_report = render_forecast_chain_completeness(&[overdue.clone()]);
        assert!(
            overdue_report.contains("0/1 latest forecast lifecycle(s) complete"),
            "{overdue_report}"
        );
        assert!(
            overdue_report.contains("missing or mismatched terminal event: 1"),
            "{overdue_report}"
        );
        let mut duplicate_overdue = overdue.clone();
        duplicate_overdue.event_id = Some("duplicate-overdue-root".into());
        let duplicate_report = render_forecast_chain_completeness(&[overdue, duplicate_overdue]);
        assert!(
            duplicate_report.contains("0/1 latest forecast lifecycle(s) complete"),
            "{duplicate_report}"
        );
        assert!(
            duplicate_report.contains("missing or mismatched creation cardinality: 1"),
            "{duplicate_report}"
        );
        assert!(render_forecast_chain_completeness(&[]).starts_with("No forecast closure"));
    }

    #[test]
    fn forecast_chain_completeness_checks_unclear_calibration_exclusion() {
        let mut root = DecisionEvent::span("prediction:unclear", None, "prediction_made");
        root.object_id = Some("prediction:unclear".into());
        root.actor = Some("foresight".into());
        root.lane = Some("primary".into());
        root.predicted = Some("threshold is met".into());
        root.confidence = Some(0.7);
        root.policy = vec!["resolve_by_ms=9223372036854775807".into()];
        let mut grade = DecisionEvent::span(
            "prediction:unclear",
            root.event_id.as_deref(),
            "prediction_graded",
        );
        grade.object_id = root.object_id.clone();
        grade.actor = root.actor.clone();
        grade.lane = root.lane.clone();
        grade.confidence = root.confidence;
        grade.policy = root.policy.clone();
        grade.verdict = Some("unclear".into());
        grade.outcome = Some("available evidence is inconclusive".into());
        grade.evaluator_id = Some("grounded-forecast-judge-v1".into());
        grade.model_calls = Some(1);
        grade.model_route = Some("util=local;research=remote".into());
        grade.latency_ms = Some(25);

        let complete = render_forecast_chain_completeness(&[root.clone(), grade.clone()]);
        assert!(
            complete.contains("1/1 latest forecast lifecycle(s) complete"),
            "{complete}"
        );

        grade.brier = Some(0.25);
        let broken = render_forecast_chain_completeness(&[root, grade]);
        assert!(
            broken.contains("missing or mismatched unclear calibration exclusion: 1"),
            "{broken}"
        );
    }

    #[test]
    fn completeness_gate_reader_refuses_a_parseable_forged_event() {
        let path = mind_types::scratch::file("verified_gate", "jsonl");
        let _ = std::fs::remove_file(&path);
        let log = DecisionLog::open(&path);
        log.record(DecisionEvent::new("gate", "tool_predicted"));
        let line = std::fs::read_to_string(&path).unwrap();
        let mut forged: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        forged["event"]["kind"] = serde_json::Value::String("tool_observed".into());
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&forged).unwrap()),
        )
        .unwrap();

        assert_eq!(log.read_all_verified(), Err(0));
        assert_eq!(
            log.read_all().len(),
            1,
            "the forensic reader remains deliberately permissive"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn same_kind_spans_get_unique_event_ids_in_a_concurrent_burst() {
        let ids: std::collections::HashSet<String> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        (0..250)
                            .map(|_| {
                                DecisionEvent::span("burst", None, "tool_observed")
                                    .event_id
                                    .unwrap()
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|worker| worker.join().unwrap())
                .collect()
        });
        assert_eq!(ids.len(), 2_000, "causal span ids must not collide");
    }

    #[test]
    fn legacy_minimal_events_keep_all_new_fields_optional() {
        let decoded: DecisionEvent =
            serde_json::from_str(r#"{"trace_id":"legacy-trace","ts_ms":1,"kind":"legacy-event"}"#)
                .unwrap();
        let mut expected = DecisionEvent::new("legacy-trace", "legacy-event");
        expected.ts_ms = 1;
        assert_eq!(decoded, expected);
        assert_eq!(
            serde_json::to_value(&decoded).unwrap(),
            serde_json::json!({
                "trace_id": "legacy-trace",
                "ts_ms": 1,
                "kind": "legacy-event"
            }),
            "unset optional fields stay absent rather than expanding old records with nulls"
        );
    }

    /// `ym why packs`: counts per pack with their denominators, censoring said aloud, and the
    /// selective-observation audit ABOVE the rates.
    #[test]
    fn pack_evidence_report_keeps_denominators_and_audits_observation() {
        let mk = |kind: &str, verdict: Option<&str>, sem: Option<bool>| {
            let mut e = DecisionEvent::new("run-t", kind);
            e.object_id = Some("pack:yantrik/x@1.0.0".into());
            e.verdict = verdict.map(String::from);
            e.semantic_success = sem;
            e
        };
        let mut ev = Vec::new();
        for _ in 0..10 {
            ev.push(mk("pack_surfaced", None, None));
        }
        for _ in 0..6 {
            ev.push(mk("pack_evidence_used", Some("used"), None));
        }
        for _ in 0..4 {
            ev.push(mk("pack_evidence_used", Some("unused"), None));
        }
        // Every USED row got graded (4 accepted, 2 corrected); no UNUSED row did.
        for _ in 0..4 {
            ev.push(mk("pack_evidence_graded", Some("accepted"), Some(true)));
        }
        for _ in 0..2 {
            ev.push(mk("pack_evidence_graded", Some("corrected"), Some(true)));
        }
        let counts = pack_evidence_counts(&ev);
        let c = &counts["yantrik/x@1.0.0"];
        assert_eq!(
            (
                c.surfaced,
                c.used,
                c.unused,
                c.graded_used,
                c.graded_unused,
                c.good
            ),
            (10, 6, 4, 6, 0, 4)
        );
        let r = render_pack_evidence(&ev);
        assert!(r.contains("used 6 of 10 surfaced"), "{r}");
        assert!(
            r.contains("graded 6 of 10 surfaced (6 after use, 0 after non-use)"),
            "{r}"
        );
        assert!(r.contains("accepted 4 of 6 graded"), "{r}");
        assert!(r.contains("censored 4 of 10 surfaced never graded"), "{r}");
        assert!(
            r.contains("too few rows on one side"),
            "unused=4 cannot support the audit yet: {r}"
        );
        // A fifth unused row lets the audit speak — and what it says is: selective.
        ev.push(mk("pack_evidence_used", Some("unused"), None));
        let r2 = render_pack_evidence(&ev);
        assert!(r2.contains("SELECTIVE OBSERVATION"), "{r2}");
        assert!(render_pack_evidence(&[]).contains("No pack evidence"));
    }

    /// `ym why routes`: verdict counts, and the per-trace consistency between the shadow route
    /// and what P.2 saw surface.
    #[test]
    fn shadow_route_report_counts_verdicts_and_checks_them_against_the_floor() {
        let mut ev = Vec::new();
        let route = |trace: &str, chosen: Option<&str>, verdict: &str| {
            let mut e = DecisionEvent::new(trace, "pack_route_shadow");
            e.chosen = chosen.map(|c| format!("pack:{c}"));
            e.verdict = Some(verdict.into());
            e.policy = vec!["coverage-router-v1".into()];
            e
        };
        let surfaced = |trace: &str, pack: &str| {
            let mut e = DecisionEvent::new(trace, "pack_surfaced");
            e.object_id = Some(format!("pack:{pack}"));
            e
        };
        ev.push(route("t1", Some("a"), "lease"));
        ev.push(surfaced("t1", "a")); // agree
        let mut member_route = route("t2", Some("a"), "lease");
        member_route.actor = Some("conversation".into());
        member_route.lane = Some("member".into());
        ev.push(member_route); // nothing surfaced
        ev.push(route("t3", None, "abstain:below_floor"));
        ev.push(surfaced("t3", "b")); // abstained while something surfaced
        ev.push(route("t4", Some("a"), "lease"));
        ev.push(surfaced("t4", "b")); // different pack
        ev.push(route("t5", None, "abstain:tie")); // nothing surfaced either → agree
        let r = render_pack_routes(&ev);
        assert!(r.contains("5 turn(s), 1 of them member lane"), "{r}");
        assert!(
            r.contains("lease: 3")
                && r.contains("abstain:below_floor: 1")
                && r.contains("abstain:tie: 1"),
            "{r}"
        );
        assert!(r.contains("agree 2 · would-lease but nothing surfaced 1 · abstained while something surfaced 1 · different pack 1"), "{r}");
        assert!(render_pack_routes(&[]).contains("No shadow routes"));
    }

    /// Unique per RUN and removed when the test ends. Keyed on the pid alone this both leaked and
    /// could hand a test a previous run's chain through a recycled pid (E.SCRATCH1).
    fn scratch(tag: &str) -> mind_types::scratch::Scratch {
        mind_types::scratch::file(&format!("flight_{tag}"), "jsonl")
    }

    fn ev(trace: &str, kind: &str, chosen: &str) -> DecisionEvent {
        let mut e = DecisionEvent::new(trace, kind);
        e.actor = Some("cognition".into());
        e.chosen = Some(chosen.into());
        e
    }

    /// P.4c (Codex's review of P.4a): `record` cannot fail from the caller's side, which is right
    /// for cognition and wrong for a durable outbox — a caller that acknowledges what `record`
    /// silently dropped has destroyed the evidence it was keeping. `record_once` reports what
    /// really happened, and a stable id is not idempotence on an append-only log unless something
    /// consults it: a crash between the append and the acknowledgement re-delivers the same event.
    #[test]
    fn record_once_reports_delivery_and_never_writes_one_id_twice() {
        // A unique path per run. (The first version of this test appeared to run twice and I
        // blamed multiple test binaries; the real cause was a stray `#[test]` this patch left
        // stacked on the function — Codex found it. The comment is corrected rather than removed
        // because the wrong diagnosis is the more useful half of the story.)
        let dir = mind_types::scratch::dir("rec_once");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("d.jsonl");
        let ev = |id: &str| {
            let mut e = DecisionEvent::new("t", "pack_leased");
            e.event_id = Some(id.to_string());
            e
        };

        let log = DecisionLog::open(&path);
        assert_eq!(
            log.record_once(ev("lease:leased:a:1")),
            RecordOutcome::Written
        );
        // The same id again — the re-delivery a crash before the acknowledgement produces.
        assert_eq!(
            log.record_once(ev("lease:leased:a:1")),
            RecordOutcome::AlreadyPresent
        );
        assert!(
            RecordOutcome::AlreadyPresent.is_durable(),
            "a retry finds it durable, which is what the ack asks"
        );
        assert_eq!(
            log.read_all()
                .iter()
                .filter(|e| e.event_id.as_deref() == Some("lease:leased:a:1"))
                .count(),
            1,
            "written once"
        );
        assert_eq!(
            log.record_once(ev("lease:released:a:1")),
            RecordOutcome::Written
        );
        assert_eq!(log.read_all().len(), 2);

        // A RESTART: a new log over the same file must still refuse the duplicate, so the ids have
        // to be read from disk rather than remembered in this process.
        let reopened = DecisionLog::open(&path);
        assert_eq!(
            reopened.record_once(ev("lease:leased:a:1")),
            RecordOutcome::AlreadyPresent,
            "the ids on disk are what count"
        );
        assert_eq!(
            reopened.read_all().len(),
            2,
            "a restart did not duplicate the event"
        );

        // No id at all is a caller error, not a silent write.
        assert!(matches!(
            reopened.record_once(DecisionEvent::new("t", "pack_leased")),
            RecordOutcome::Failed(_)
        ));
        // A disabled log says so instead of pretending to have written.
        assert_eq!(
            DecisionLog::disabled().record_once(ev("x")),
            RecordOutcome::Disabled
        );
        assert!(!RecordOutcome::Disabled.is_durable());
        assert!(!RecordOutcome::Failed("x".into()).is_durable());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn record_once_dedupes_against_the_sanitized_durable_id() {
        let path = scratch("rec_once_redacted_id");
        let raw_id = "ghp_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX";
        let event = || {
            let mut e = DecisionEvent::new("t", "pack_leased");
            e.event_id = Some(raw_id.into());
            e
        };
        let log = DecisionLog::open(&path);
        assert_eq!(log.record_once(event()), RecordOutcome::Written);
        assert_eq!(log.record_once(event()), RecordOutcome::AlreadyPresent);
        let events = log.read_all();
        assert_eq!(events.len(), 1, "the redacted retry must not duplicate");
        assert_eq!(events[0].event_id.as_deref(), Some("[redacted-secret]"));
        assert!(!std::fs::read_to_string(&path).unwrap().contains(raw_id));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_once_rejects_blank_identity_after_sanitization() {
        let path = scratch("rec_once_blank_id");
        let mut event = DecisionEvent::new("t", "pack_leased");
        event.event_id = Some("   ".into());
        event.evaluator_id = Some("   ".into());
        let log = DecisionLog::open(&path);

        let outcome = log.record_once(event);
        assert!(
            matches!(outcome, RecordOutcome::Failed(ref message) if message.contains("needs an event_id")),
            "blank durable identity must fail rather than dedupe unrelated events: {outcome:?}"
        );
        assert!(log.read_all().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    /// P.4f (Codex's recorder review): the guarantees `record_once` has to keep beyond "the id is
    /// stable". Each of these was a way the outbox could acknowledge an event the log did not
    /// honestly contain.
    #[test]
    fn durable_delivery_survives_corruption_forgery_and_concurrency() {
        let dir = mind_types::scratch::dir("p4f");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ev = |id: &str| {
            let mut e = DecisionEvent::new("t", "pack_leased");
            e.event_id = Some(id.to_string());
            e
        };

        // A PARTIAL TAIL — a crash mid-write. Appending onto it would concatenate the next event
        // into the fragment and silently break the chain from there on.
        let torn = dir.join("torn.jsonl");
        let log = DecisionLog::open(&torn);
        assert_eq!(log.record_once(ev("a")), RecordOutcome::Written);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&torn)
                .unwrap();
            f.write_all(b"{\"chain\":\"deadbeef\",\"eve").unwrap(); // no newline: a torn line
        }
        let fresh = DecisionLog::open(&torn);
        match fresh.record_once(ev("b")) {
            RecordOutcome::Failed(why) => assert!(
                why.contains("does not verify") || why.contains("broken chain"),
                "{why}"
            ),
            other => panic!("appended onto a torn log: {other:?}"),
        }
        assert!(
            !std::fs::read_to_string(&torn).unwrap().contains("\"b\""),
            "nothing may be written through corruption"
        );

        // A FORGED LINE carrying a real id. `read_events` would happily hand back its event, and a
        // dedupe built on that would answer AlreadyPresent for something the chain does not contain.
        let forged = dir.join("forged.jsonl");
        let log = DecisionLog::open(&forged);
        assert_eq!(log.record_once(ev("real")), RecordOutcome::Written);
        {
            use std::io::Write;
            let line = "{\"chain\":\"0000000000000000000000000000000000000000000000000000000000000000\",\"event\":{\"trace_id\":\"t\",\"ts_ms\":1,\"kind\":\"pack_leased\",\"event_id\":\"forged\"}}\n";
            std::fs::OpenOptions::new()
                .append(true)
                .open(&forged)
                .unwrap()
                .write_all(line.as_bytes())
                .unwrap();
        }
        assert!(
            read_events(&forged)
                .iter()
                .any(|e| e.event_id.as_deref() == Some("forged")),
            "the unverified reader is fooled — that is the point"
        );
        assert!(
            read_events_verified(&forged).is_err(),
            "the verified reader is not"
        );
        match DecisionLog::open(&forged).record_once(ev("forged")) {
            RecordOutcome::Failed(_) => {}
            other => panic!("a forged chain was treated as durable: {other:?}"),
        }

        // CONCURRENCY: many threads, many handles, one file, one id. Exactly one may write it.
        let shared = dir.join("shared.jsonl");
        let written = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let already = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let (shared, written, already) = (shared.clone(), written.clone(), already.clone());
                scope.spawn(move || {
                    // A SEPARATE handle per thread: the lock cannot live inside one of them.
                    let mut e = DecisionEvent::new("t", "pack_leased");
                    e.event_id = Some("one-and-only".into());
                    match DecisionLog::open(&shared).record_once(e) {
                        RecordOutcome::Written => {
                            written.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        }
                        RecordOutcome::AlreadyPresent => {
                            already.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        }
                        other => panic!("unexpected outcome under contention: {other:?}"),
                    };
                });
            }
        });
        assert_eq!(
            written.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exactly one writer"
        );
        assert_eq!(
            already.load(std::sync::atomic::Ordering::SeqCst),
            7,
            "the rest found it durable"
        );
        assert_eq!(verify_log(&shared), Ok(1), "and the chain still verifies");
        assert_eq!(read_events_verified(&shared).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P.4g (Codex's review of P.4f): WARM HANDLES, NOT COLD ONES. P.4f's concurrency test spawned
    /// a fresh `DecisionLog` per thread, so every writer scanned the file after taking the lock and
    /// the bug could not appear. The bug needs a handle that has already written — its cached head
    /// and id set survive another handle's append, and the next write chains onto a superseded head
    /// or re-writes an id it never saw land. This is that exact interleaving.
    #[test]
    fn a_warm_handle_cannot_write_over_what_another_handle_appended() {
        let dir = mind_types::scratch::dir("p4g_warm");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("d.jsonl");
        let ev = |id: &str| {
            let mut e = DecisionEvent::new("t", "pack_leased");
            e.event_id = Some(id.to_string());
            e
        };
        let a = DecisionLog::open(&path);
        let b = DecisionLog::open(&path);

        // A writes, and is now WARM: whatever it remembers about this file, it remembers now.
        assert_eq!(a.record_once(ev("seed")), RecordOutcome::Written);
        // B writes something A has never seen.
        assert_eq!(b.record_once(ev("x")), RecordOutcome::Written);
        // A is asked for the same id. It must find B's write, not its own stale picture.
        assert_eq!(
            a.record_once(ev("x")),
            RecordOutcome::AlreadyPresent,
            "a warm handle wrote a duplicate"
        );
        assert_eq!(
            verify_log(&path),
            Ok(2),
            "and the chain must still verify end to end"
        );
        assert_eq!(read_events_verified(&path).unwrap().len(), 2);

        // The same trap with a plain `record` in the middle: A's next durable write must chain onto
        // B's line, not onto the head A remembered before it.
        b.record(ev("plain-from-b"));
        assert_eq!(a.record_once(ev("y")), RecordOutcome::Written);
        assert_eq!(
            verify_log(&path),
            Ok(4),
            "an ordinary record must not break a warm handle's chain"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P.4g: `record` used to append with no lock at all, so an ordinary decision event could
    /// interleave with a durable delivery and leave both chaining onto a superseded head. Every
    /// writer to a path now takes the same lock; the chain is the proof.
    #[test]
    fn ordinary_records_and_durable_deliveries_share_one_chain_under_contention() {
        let dir = mind_types::scratch::dir("p4g_mix");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("d.jsonl");
        let warm = DecisionLog::open(&path);
        warm.record(DecisionEvent::new("t", "warmup")); // warm one handle before the storm
        std::thread::scope(|scope| {
            for i in 0..6 {
                let path = path.clone();
                scope.spawn(move || {
                    let log = DecisionLog::open(&path);
                    for j in 0..5 {
                        if (i + j) % 2 == 0 {
                            log.record(DecisionEvent::new("t", "cognitive_run"));
                        } else {
                            let mut e = DecisionEvent::new("t", "pack_leased");
                            e.event_id = Some(format!("id-{i}-{j}"));
                            let _ = log.record_once(e);
                        }
                    }
                });
            }
            for j in 0..5 {
                let mut e = DecisionEvent::new("t", "pack_released");
                e.event_id = Some(format!("warm-{j}"));
                let _ = warm.record_once(e);
                warm.record(DecisionEvent::new("t", "cognitive_run"));
            }
        });
        let total = read_events(&path).len();
        assert_eq!(
            verify_log(&path),
            Ok(total),
            "mixed writers must leave ONE unbroken chain"
        );
        let ids: Vec<String> = read_events_verified(&path)
            .unwrap()
            .into_iter()
            .filter_map(|e| e.event_id)
            .collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            ids.len(),
            unique.len(),
            "no event id may appear twice: {ids:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// P.4g: one file, one lock — even when the first handle is made BEFORE the file exists.
    /// Keying on `canonicalize` alone gave a relative key pre-creation and a canonical one after,
    /// so the handle that created the log and the next handle locked different things.
    #[test]
    fn a_handle_made_before_the_file_exists_shares_the_lock_with_one_made_after() {
        let dir = mind_types::scratch::dir("p4g_key");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("late.jsonl");
        assert!(!path.exists(), "the premise: it does not exist yet");
        let before = lock_key(&path);
        let early = DecisionLog::open(&path);
        let mut e = DecisionEvent::new("t", "pack_leased");
        e.event_id = Some("first".into());
        assert_eq!(early.record_once(e), RecordOutcome::Written);
        let after = lock_key(&path);
        assert_eq!(
            before, after,
            "the key must not change when the file appears"
        );
        assert!(after.is_absolute(), "and it must be absolute: {after:?}");
        // A handle made after creation sees the same id and the same chain.
        let late = DecisionLog::open(&path);
        let mut e = DecisionEvent::new("t", "pack_leased");
        e.event_id = Some("first".into());
        assert_eq!(late.record_once(e), RecordOutcome::AlreadyPresent);
        assert_eq!(verify_log(&path), Ok(1));
        // A relative spelling of the same file keys the same way.
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(rel) = path.strip_prefix(&cwd) {
                assert_eq!(lock_key(rel), after, "a relative spelling is the same file");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn appends_reads_and_verifies() {
        let path = scratch("ok");
        let log = DecisionLog::open(&path);
        log.record(ev("t1", "cognitive_run", "synthesize"));
        log.record(ev("t1", "packet_created", "watch_price"));
        log.record(ev("t2", "reflex_enqueued", "goal line"));
        assert_eq!(verify_log(&path), Ok(3), "chain verifies end to end");
        let all = read_events(&path);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].kind, "cognitive_run");
        assert_eq!(all[2].trace_id, "t2");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tamper_breaks_the_chain() {
        let path = scratch("tamper");
        let log = DecisionLog::open(&path);
        log.record(ev("t1", "a", "one"));
        log.record(ev("t1", "b", "two"));
        let content = std::fs::read_to_string(&path).unwrap();
        let tampered = content.replacen("\"two\"", "\"rewritten\"", 1);
        std::fs::write(&path, tampered).unwrap();
        assert!(
            verify_log(&path).is_err(),
            "an edited event must not verify"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deletion_breaks_every_later_hash() {
        let path = scratch("delete");
        let log = DecisionLog::open(&path);
        for i in 0..4 {
            log.record(ev("t", "k", &format!("n{i}")));
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let without_line2: String = content
            .lines()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, l)| format!("{l}\n"))
            .collect();
        std::fs::write(&path, without_line2).unwrap();
        assert!(
            verify_log(&path).is_err(),
            "removing a middle line must break the chain"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn chain_survives_reopen() {
        let path = scratch("reopen");
        {
            let log = DecisionLog::open(&path);
            log.record(ev("t1", "a", "first"));
        }
        // A NEW handle (fresh process shape) continues the persisted chain.
        let log2 = DecisionLog::open(&path);
        log2.record(ev("t1", "b", "second"));
        assert_eq!(verify_log(&path), Ok(2));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn one_trace_reconstructs_in_order_across_kinds() {
        let path = scratch("trace");
        let log = DecisionLog::open(&path);
        // Interleave two traces; reconstruction must pull out only t-a's events in order.
        let mut start = DecisionEvent::new("2026-trace-alpha", "packet_created");
        start.trigger = Some("future_node:brishti-birthday window opened".into());
        start.candidates = vec!["gift_scout".into(), "ask_owner".into()];
        start.rejected = vec!["ask_owner (answerable from memory)".into()];
        start.chosen = Some("gift_scout".into());
        start.policy = vec![
            "harm-gate:allow".into(),
            "purpose:allow(suppressed=0)".into(),
        ];
        start.predicted = Some("owner confirms within a day".into());
        start.confidence = Some(0.62);
        log.record(start);
        log.record(ev("unrelated", "cognitive_run", "x"));
        let mut done = ev("2026-trace-alpha", "packet_resolved", "confirmed by owner");
        done.outcome = Some("owner accepted the packet".into());
        done.verdict = Some("engaged".into());
        done.prediction_error = Some(0.38);
        done.brier = Some(0.1444);
        done.semantic_success = Some(true);
        done.latency_ms = Some(87);
        done.lesson = Some("gift_scout reliability +1 observation".into());
        log.record(done);

        let trace = events_by_trace(&path, "2026-trace-al");
        assert_eq!(trace.len(), 2, "prefix match pulls only this trace");
        assert_eq!(trace[0].kind, "packet_created");
        assert_eq!(trace[1].kind, "packet_resolved");
        let rendered = render_trace(&trace);
        for needle in [
            "packet_created",
            "trigger: future_node",
            "considered: gift_scout",
            "rejected: ask_owner",
            "policy: harm-gate:allow",
            "predicted: owner confirms within a day (confidence 0.62)",
            "verdict: engaged",
            "prediction error: +0.380",
            "brier: 0.144",
            "semantic success: true",
            "latency: 87 ms",
            "lesson:",
        ] {
            assert!(
                rendered.contains(needle),
                "rendered trace must contain '{needle}':\n{rendered}"
            );
        }
        assert!(!rendered.contains("unrelated"), "other traces stay out");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn free_text_fields_are_truncated() {
        let long = "word ".repeat(200);
        let mut e = ev("t", "k", "x");
        e.goal = Some(long.clone());
        e.outcome = Some(long.clone());
        let s = e.sanitized();
        assert!(
            s.goal.as_ref().unwrap().chars().count() <= 161,
            "goal truncated to budget + ellipsis"
        );
        assert!(s.goal.as_ref().unwrap().ends_with('…'));
        assert!(s.outcome.as_ref().unwrap().chars().count() <= 161);
    }

    #[test]
    fn secrets_never_enter_the_ledger() {
        // The same detector that guards memory writes guards the recorder; a field that carries
        // a secret-shaped string is replaced wholesale — no partial content survives truncation.
        let mut e = ev("t", "k", "x");
        e.goal =
            Some("deploy with token ghp_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX and continue".into());
        e.trigger = Some("AKIAIOSFODNN7EXAMPLE rotation due".into());
        e.trace_id = "ghp_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX".into();
        e.event_id = Some("ghp_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX".into());
        e.parent_event_id = Some("ghp_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX".into());
        e.object_id = Some("ghp_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX".into());
        e.evidence_ids = vec!["ghp_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX".into()];
        let s = e.sanitized();
        assert_eq!(s.goal.as_deref(), Some("[redacted-secret]"), "{:?}", s.goal);
        assert_eq!(s.trigger.as_deref(), Some("[redacted-secret]"));
        assert_eq!(s.trace_id, "[redacted-secret]");
        assert_eq!(s.event_id.as_deref(), Some("[redacted-secret]"));
        assert_eq!(s.parent_event_id.as_deref(), Some("[redacted-secret]"));
        assert_eq!(s.object_id.as_deref(), Some("[redacted-secret]"));
        assert_eq!(s.evidence_ids, ["[redacted-secret]"]);
        // And the serialized form really is what lands on disk.
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("ghp_"), "no secret material in the record");
        assert!(!json.contains("AKIA"), "no key id in the record");
    }

    #[test]
    fn disabled_log_is_a_no_op_and_failures_do_not_poison_callers() {
        // Disabled by construction.
        let off = DecisionLog::disabled();
        off.record(ev("t", "k", "x")); // must not panic

        // A path that cannot be written (parent is a FILE): the record fails internally,
        // marks the recorder unhealthy (backoff window), and later records inside the window
        // are silent no-ops. The caller never sees an error or panic either way. After the
        // backoff expires the recorder retries automatically — recovery needs no restart.
        let base = scratch("blocked");
        std::fs::write(&base, "i am a file").unwrap();
        let blocked_path = base.join("nested").join("log.jsonl");
        let log = DecisionLog::open(&blocked_path);
        log.record(ev("t", "k", "x")); // fails -> unhealthy, backoff window opens
        log.record(ev("t", "k", "y")); // inside window: silent no-op
        assert_eq!(read_events(&blocked_path), vec![], "nothing was written");
        assert_eq!(
            render_trace(&events_by_trace(&base, "t")),
            "no recorded events under this trace"
        );
        let _ = std::fs::remove_file(&base);

        // The disabled() constructor also renders gracefully.
        assert_eq!(render_trace(&[]), "no recorded events under this trace");
    }

    #[test]
    fn spans_link_into_a_causal_tree() {
        let path = scratch("spans");
        let log = DecisionLog::open(&path);
        // interpretation → plan → packet, chained by parent_event_id; the packet names its object.
        let root = DecisionEvent::span("turn-8271", None, "interpretation");
        let root_id = root.event_id.clone().unwrap();
        let mut plan = DecisionEvent::span("turn-8271", Some(&root_id), "plan");
        plan.chosen = Some("prepare gift shortlist".into());
        let plan_id = plan.event_id.clone().unwrap();
        let mut pkt = DecisionEvent::span("turn-8271", Some(&plan_id), "packet_created");
        pkt.object_id = Some("pkt:abc".into());
        for e in [root, plan, pkt] {
            log.record(e);
        }
        assert_eq!(verify_log(&path), Ok(3));
        let trace = events_by_trace(&path, "turn-8271");
        assert_eq!(trace.len(), 3);
        assert_eq!(
            trace[1].parent_event_id.as_deref(),
            trace[0].event_id.as_deref(),
            "plan parents to interpretation"
        );
        assert_eq!(
            trace[2].parent_event_id.as_deref(),
            trace[1].event_id.as_deref(),
            "packet parents to plan"
        );
        assert_eq!(trace[2].object_id.as_deref(), Some("pkt:abc"));
        let rendered = render_trace(&trace);
        assert!(rendered.contains(&format!("span: {root_id}")), "{rendered}");
        assert!(rendered.contains("object: pkt:abc"), "{rendered}");
        assert!(rendered.contains("parent span:"), "{rendered}");
        let _ = std::fs::remove_file(&path);
    }
}

#[cfg(test)]
mod float_stability {
    use super::*;

    /// THE SERDE_JSON SCAR (2026-08-24). serde_json's DEFAULT float formatter is not
    /// round-trip stable: a brier of `0.11111111111111113` re-serialized as `...112` — one
    /// ulp of drift, which silently broke every later hash in the chain (verify Err(3) on a
    /// four-line log). Fix: workspace serde_json carries `float_roundtrip`. This test pins
    /// that: a chain whose events carry awkward f64s must verify after reopen, or every
    /// graded prediction in the system is one write away from an unverifiable ledger.
    #[test]
    fn floats_survive_write_read_verify_without_ulp_drift() {
        let p = mind_types::scratch::file("float_rt", "jsonl");
        let log = DecisionLog::open(&p);
        let mut pred = DecisionEvent::new("run-t", "tool_predicted");
        pred.event_id = Some("tool_predicted-123".into());
        pred.confidence = Some(2.0f64 / 3.0); // non-terminating binary expansion
        log.record(pred);
        let mut obs = DecisionEvent::new("run-t", "tool_observed");
        obs.parent_event_id = Some("tool_predicted-123".into());
        obs.prediction_error = Some(1.0 - 2.0f64 / 3.0);
        obs.brier = Some((2.0f64 / 3.0 - 1.0).powi(2));
        obs.semantic_success = Some(false);
        log.record(obs);

        // Fresh handle = re-read from disk, exactly what verification does in production.
        assert_eq!(
            verify_log(&p),
            Ok(2),
            "1-ulp drift on reserialization breaks the chain"
        );
        let events = read_events(&p);
        assert_eq!(events[0].confidence, Some(2.0f64 / 3.0));
        assert_eq!(
            events[1].parent_event_id.as_deref(),
            Some("tool_predicted-123"),
            "causal span linkage survives the durable recorder round trip"
        );
        assert_eq!(
            events[1].brier,
            Some((2.0f64 / 3.0 - 1.0).powi(2)),
            "bit-exact through disk"
        );
        assert_eq!(
            events[1].semantic_success,
            Some(false),
            "semantic outcome survives the durable recorder round trip"
        );
        let _ = std::fs::remove_file(&p);
    }
}

/// E.SEC1b boundary proof 2 of 4 — the flight recorder's redaction (Codex point 4).
#[cfg(test)]
mod sec1b_boundary {
    #[test]
    fn the_ledger_redacts_a_secret_and_the_line_it_writes_carries_no_part_of_it() {
        for text in [
            "my password is hunter2",
            "ghp_SECRET12345",
            "-----BEGIN RSA PRIVATE KEY-----",
        ] {
            let out = super::brief(text, 160);
            assert_eq!(
                out, "[redacted-secret]",
                "secret-shaped free text is replaced, not truncated"
            );
            for word in text.split_whitespace().filter(|w| w.len() >= 4) {
                assert!(
                    !out.contains(word),
                    "the ledger line leaked {word:?}: {out}"
                );
            }
        }
    }

    #[test]
    fn redaction_replaces_rather_than_shortens() {
        // Truncating a secret still writes the front of it. The distinction matters: an earlier
        // shape of this could have kept the first 160 characters of a PEM block.
        let long_secret = format!("my password is hunter2 {}", "x".repeat(400));
        assert_eq!(super::brief(&long_secret, 160), "[redacted-secret]");
        // And the control: ordinary long text IS truncated, so the test above is meaningful.
        let ordinary = "y".repeat(400);
        let out = super::brief(&ordinary, 160);
        assert!(
            out.ends_with('…') && out.chars().count() == 161,
            "ordinary text truncates: {}",
            out.chars().count()
        );
    }
}
