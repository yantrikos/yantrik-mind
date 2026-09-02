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

/// Hard ceiling for UI/API consumers of the recent decision stream. Analytics that need complete
/// history use [`DecisionLog::read_all_verified`] explicitly; interactive surfaces stay bounded.
pub const DECISION_TAIL_MAX: usize = 200;

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

    /// A bounded, integrity-verified, re-sanitized tail for operator surfaces.
    ///
    /// The whole chain is verified before selecting the tail, so a forged or corrupt earlier line
    /// cannot disappear outside the requested window. Events are sanitized again after reading to
    /// protect the UI from valid legacy records written before today's detector/budgets existed.
    pub fn read_tail_verified(
        &self,
        limit: usize,
    ) -> std::result::Result<Vec<DecisionEvent>, usize> {
        let events = self.read_all_verified()?;
        let keep = limit.min(DECISION_TAIL_MAX);
        let start = events.len().saturating_sub(keep);
        Ok(events
            .into_iter()
            .skip(start)
            .map(DecisionEvent::sanitized)
            .collect())
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

/// Aggregate time bounds for one completeness sample. `timestamped` is explicit because a
/// partially malformed sample can still have honest bounds without pretending every row carried a
/// usable timestamp.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompletenessWindow {
    pub oldest_ts_ms: u64,
    pub newest_ts_ms: u64,
    pub timestamped: usize,
}

/// The deliberately separate completeness contract for malformed preflight refusals. These rows
/// never enter the executed-call denominator because no tool prediction or egress occurred.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreflightCompleteness {
    pub complete: usize,
    pub total: usize,
    pub defects: std::collections::BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<CompletenessWindow>,
}

/// Typed, aggregate-only result of the tool-chain provenance gate. This is the source of truth for
/// both human prose and machine surfaces; callers must not recover numbers by parsing the prose.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolChainCompleteness {
    pub complete: usize,
    pub total: usize,
    pub defects: std::collections::BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<CompletenessWindow>,
    pub preflight: PreflightCompleteness,
}

/// Measure the roadmap's closed-chain gate over the latest 200 tool calls. A call is
/// complete only when its observation joins to one prediction and the pair carries the provenance
/// needed to compare behavior across goals, contexts, lanes, evaluators, and runtime versions.
/// Malformed arguments refused before prediction are not tool calls; they are reported separately
/// under a strict preflight-refusal contract so safe boundary behavior cannot either poison or evade
/// the executed-call gate. Aggregate defect counts are reported instead of identifiers, so this
/// remains safe to expose to an operations surface.
pub fn tool_chain_completeness(events: &[DecisionEvent]) -> ToolChainCompleteness {
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
    let mut prediction_id_counts: std::collections::HashMap<&str, usize> = Default::default();
    for event_id in events
        .iter()
        .filter(|event| event.kind == "tool_predicted")
        .filter_map(|event| event.event_id.as_deref())
    {
        *prediction_id_counts.entry(event_id).or_insert(0) += 1;
    }
    let mut observed_parent_counts: std::collections::HashMap<&str, usize> = Default::default();
    for parent in events
        .iter()
        .filter(|event| event.kind == "tool_observed")
        .filter_map(|event| event.parent_event_id.as_deref())
    {
        *observed_parent_counts.entry(parent).or_insert(0) += 1;
    }
    let mut events_by_id: std::collections::HashMap<&str, Vec<&DecisionEvent>> = Default::default();
    for event in events {
        if let Some(event_id) = event.event_id.as_deref() {
            events_by_id.entry(event_id).or_default().push(event);
        }
    }
    // A malformed observation is an explicit preflight refusal when it has no parent (the standalone
    // EngineBus shape) or parents directly to the run's goal_compiled root (the production loop
    // shape). In either case the argument boundary refused it before a prediction or egress existed.
    // A dangling parent, or a parent of any other kind, remains an ordinary call and must join a
    // prediction; checking the verdict alone would let an orphan evade this gate.
    let is_preflight_refusal = |event: &DecisionEvent| {
        event.kind == "tool_observed"
            && event.verdict.as_deref() == Some("malformed")
            && match event.parent_event_id.as_deref() {
                None => !compiled_roots.contains_key(event.trace_id.as_str()),
                Some(parent) => events_by_id.get(parent).is_some_and(|parents| {
                    parents.len() == 1
                        && parents[0].kind == "goal_compiled"
                        && parents[0].trace_id == event.trace_id
                        && compiled_roots
                            .get(event.trace_id.as_str())
                            .is_some_and(|roots| roots.len() == 1 && roots[0] == Some(parent))
                }),
            }
    };
    let mut preflight_refusals: Vec<(usize, &DecisionEvent)> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| is_preflight_refusal(event))
        .collect();
    preflight_refusals.sort_unstable_by_key(|(index, _)| std::cmp::Reverse(*index));
    preflight_refusals.truncate(SAMPLE_LIMIT);

    // One row per observed call, plus every prediction that has no observed child. Sampling only
    // observations would make a crash between the two events disappear from the denominator and
    // let the report go falsely green precisely when the recorder lost closure.
    let mut calls: Vec<(usize, Option<&DecisionEvent>, Option<&DecisionEvent>)> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.kind == "tool_observed" && !is_preflight_refusal(event))
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
    if calls.is_empty() && preflight_refusals.is_empty() {
        return ToolChainCompleteness::default();
    }

    let mut complete = 0usize;
    let mut defects: std::collections::BTreeMap<&str, usize> = Default::default();
    for (_, prediction, observation) in &calls {
        let mut row_complete = true;
        let present = |value: &Option<String>| {
            value
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        };
        let mut require = |condition: bool, label: &'static str| {
            if !condition {
                row_complete = false;
                *defects.entry(label).or_insert(0) += 1;
            }
        };

        require(prediction.is_some(), "prediction link");
        require(observation.is_some(), "observation link");
        if let Some(prediction) = prediction {
            require(present(&prediction.event_id), "prediction event_id");
            require(
                prediction
                    .event_id
                    .as_deref()
                    .and_then(|event_id| prediction_id_counts.get(event_id))
                    == Some(&1),
                "prediction cardinality",
            );
            require(
                prediction
                    .event_id
                    .as_deref()
                    .and_then(|event_id| events_by_id.get(event_id))
                    .is_some_and(|matches| matches.len() == 1),
                "prediction event_id uniqueness",
            );
            require(prediction.ts_ms > 0, "prediction ts_ms");
        }
        if let (Some(prediction), Some(observation)) = (prediction, observation) {
            if let Some(roots) = compiled_roots.get(prediction.trace_id.as_str()) {
                let root_id = roots.first().copied().flatten();
                let root_linked = roots.len() == 1
                    && root_id.is_some()
                    && prediction.parent_event_id.as_deref() == root_id;
                require(root_linked, "bounded root linkage");
                if let Some(root_id) = root_id.filter(|_| root_linked) {
                    require(
                        !root_id.trim().is_empty()
                            && events_by_id.get(root_id).is_some_and(|matches| {
                                matches.len() == 1
                                    && matches[0].kind == "goal_compiled"
                                    && matches[0].trace_id == prediction.trace_id
                                    && matches[0].ts_ms > 0
                                    && matches[0].ts_ms <= prediction.ts_ms
                            }),
                        "bounded root integrity",
                    );
                }
            } else {
                require(prediction.parent_event_id.is_none(), "bounded root linkage");
            }
            require(
                observation
                    .parent_event_id
                    .as_deref()
                    .and_then(|parent| observed_parent_counts.get(parent))
                    == Some(&1),
                "observation cardinality",
            );
            require(
                !prediction.trace_id.trim().is_empty()
                    && !observation.trace_id.trim().is_empty()
                    && prediction.trace_id == observation.trace_id,
                "trace linkage",
            );
            require(
                prediction.ts_ms > 0 && observation.ts_ms >= prediction.ts_ms,
                "temporal ordering",
            );
            require(
                present(&prediction.object_id) && prediction.object_id == observation.object_id,
                "object linkage",
            );
            require(
                present(&prediction.actor) && prediction.actor == observation.actor,
                "actor",
            );
            require(
                present(&prediction.lane) && prediction.lane == observation.lane,
                "lane",
            );
            require(
                present(&prediction.context_fingerprint)
                    && prediction.context_fingerprint == observation.context_fingerprint,
                "context_fingerprint",
            );
            require(
                present(&prediction.goal_id) && prediction.goal_id == observation.goal_id,
                "goal_id",
            );
            require(
                present(&prediction.tool_version)
                    && prediction.tool_version == observation.tool_version,
                "tool_version",
            );
            require(
                present(&prediction.model_route)
                    && prediction.model_route == observation.model_route,
                "model_route",
            );
            require(present(&prediction.predicted), "predicted outcome");
            require(
                prediction.confidence.is_some_and(valid_probability),
                "predicted probability",
            );
            if let Some(probability) = prediction
                .confidence
                .filter(|value| valid_probability(*value))
            {
                let observed = match observation.verdict.as_deref() {
                    Some("ok" | "empty") => Some(1.0),
                    Some("failed") => Some(0.0),
                    _ => None,
                };
                if let Some(observed) = observed {
                    let expected_error = observed - probability;
                    let expected_brier = (probability - observed).powi(2);
                    require(
                        observation.prediction_error.is_some_and(|actual| {
                            actual.is_finite() && (actual - expected_error).abs() <= 1e-9
                        }),
                        "prediction_error consistency",
                    );
                    require(
                        observation.brier.is_some_and(|actual| {
                            actual.is_finite() && (actual - expected_brier).abs() <= 1e-9
                        }),
                        "brier consistency",
                    );
                } else if matches!(
                    observation.verdict.as_deref(),
                    Some("unavailable" | "denied" | "malformed")
                ) {
                    require(
                        observation.prediction_error.is_none() && observation.brier.is_none(),
                        "calibration exclusion",
                    );
                }
            }
        }
        if let Some(observation) = observation {
            require(present(&observation.event_id), "observation event_id");
            require(
                observation
                    .event_id
                    .as_deref()
                    .and_then(|event_id| events_by_id.get(event_id))
                    .is_some_and(|matches| matches.len() == 1),
                "observation event_id uniqueness",
            );
            require(observation.ts_ms > 0, "observation ts_ms");
            require(observation.outcome.is_some(), "actual outcome");
            require(
                matches!(
                    observation.verdict.as_deref(),
                    Some("ok" | "empty" | "unavailable" | "denied" | "failed" | "malformed")
                ),
                "actual verdict",
            );
            require(
                observation.evaluator_id.as_deref() == Some("tool-outcome-v1"),
                "evaluator_id",
            );
            require(present(&observation.lesson), "lesson");
            if !matches!(observation.verdict.as_deref(), Some("malformed" | "denied")) {
                require(observation.latency_ms.is_some(), "latency_ms");
            }
            match observation.verdict.as_deref() {
                Some("ok") => require(
                    observation.semantic_success == Some(true),
                    "semantic_success consistency",
                ),
                Some("empty") => require(
                    observation.semantic_success == Some(false),
                    "semantic_success consistency",
                ),
                _ => {}
            }
        }
        if row_complete {
            complete += 1;
        }
    }

    let total = calls.len();
    // E.AGI-A2: a live reading must say WHEN its evidence is from — a window full of history
    // reads very differently from a window of yesterday's traffic. Timestamps only, aggregate.
    let mut span_ts: Vec<u64> = calls
        .iter()
        .flat_map(|(_, prediction, observation)| {
            prediction
                .iter()
                .chain(observation.iter())
                .map(|event| event.ts_ms)
        })
        .filter(|ts| *ts > 0)
        .collect();
    span_ts.sort_unstable();
    let window = span_ts
        .first()
        .zip(span_ts.last())
        .map(|(oldest, newest)| CompletenessWindow {
            oldest_ts_ms: *oldest,
            newest_ts_ms: *newest,
            timestamped: span_ts.len(),
        });

    let mut refusal_complete = 0usize;
    let mut refusal_defects: std::collections::BTreeMap<&str, usize> = Default::default();
    for (_, observation) in &preflight_refusals {
        let mut row_complete = true;
        let present = |value: &Option<String>| {
            value
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        };
        let mut require = |condition: bool, label: &'static str| {
            if !condition {
                row_complete = false;
                *refusal_defects.entry(label).or_insert(0) += 1;
            }
        };
        require(present(&observation.event_id), "event_id");
        if let Some(event_id) = observation.event_id.as_deref() {
            require(
                events_by_id
                    .get(event_id)
                    .is_some_and(|matches| matches.len() == 1),
                "event_id uniqueness",
            );
        }
        require(!observation.trace_id.trim().is_empty(), "trace_id");
        require(observation.ts_ms > 0, "ts_ms");
        if let Some(parent_id) = observation.parent_event_id.as_deref() {
            require(
                !parent_id.trim().is_empty()
                    && events_by_id.get(parent_id).is_some_and(|matches| {
                        matches.len() == 1
                            && matches[0].kind == "goal_compiled"
                            && matches[0].trace_id == observation.trace_id
                            && matches[0].ts_ms > 0
                            && matches[0].ts_ms <= observation.ts_ms
                    }),
                "bounded root integrity",
            );
        }
        require(present(&observation.actor), "actor");
        require(present(&observation.lane), "lane");
        require(
            present(&observation.context_fingerprint),
            "context_fingerprint",
        );
        require(present(&observation.goal_id), "goal_id");
        require(present(&observation.tool_version), "tool_version");
        require(present(&observation.model_route), "model_route");
        require(present(&observation.object_id), "object_id");
        require(
            observation.evaluator_id.as_deref() == Some("tool-outcome-v1"),
            "evaluator_id",
        );
        require(present(&observation.outcome), "outcome");
        require(present(&observation.lesson), "lesson");
        require(
            observation.prediction_error.is_none() && observation.brier.is_none(),
            "calibration exclusion",
        );
        if row_complete {
            refusal_complete += 1;
        }
    }
    let refusal_total = preflight_refusals.len();
    let mut refusal_ts: Vec<u64> = preflight_refusals
        .iter()
        .map(|(_, observation)| observation.ts_ms)
        .filter(|ts| *ts > 0)
        .collect();
    refusal_ts.sort_unstable();
    let refusal_window = refusal_ts
        .first()
        .zip(refusal_ts.last())
        .map(|(oldest, newest)| CompletenessWindow {
            oldest_ts_ms: *oldest,
            newest_ts_ms: *newest,
            timestamped: refusal_ts.len(),
        });

    ToolChainCompleteness {
        complete,
        total,
        defects: defects
            .into_iter()
            .map(|(field, count)| (field.to_string(), count))
            .collect(),
        window,
        preflight: PreflightCompleteness {
            complete: refusal_complete,
            total: refusal_total,
            defects: refusal_defects
                .into_iter()
                .map(|(field, count)| (field.to_string(), count))
                .collect(),
            window: refusal_window,
        },
    }
}

/// Render the typed tool-chain report for operators. Keep this prose stable, but never require a
/// machine consumer to reverse-engineer it.
pub fn render_tool_chain_completeness(events: &[DecisionEvent]) -> String {
    let report = tool_chain_completeness(events);
    if report.total == 0 && report.preflight.total == 0 {
        return "No tool-chain calls yet — completeness appears after a tool decision.".into();
    }

    let mut out = if report.total == 0 {
        "No tool-chain calls yet — completeness appears after a tool decision.\n".into()
    } else {
        let percent = 100.0 * report.complete as f64 / report.total as f64;
        let mut out = format!(
            "TOOL CHAIN COMPLETENESS — {}/{} latest call(s) complete ({percent:.1}%; gate ≥99%)\n",
            report.complete, report.total
        );
        if let Some(window) = &report.window {
            out.push_str(&format!(
                "  window spans ts_ms {}..{} across {} sampled call(s)\n",
                window.oldest_ts_ms, window.newest_ts_ms, report.total
            ));
        }
        if report.defects.is_empty() {
            out.push_str("  no missing or mismatched provenance in this sample\n");
        } else {
            for (field, count) in &report.defects {
                out.push_str(&format!("  missing or mismatched {field}: {count}\n"));
            }
        }
        out
    };

    if report.preflight.total > 0 {
        out.push_str(&format!(
            "  PREFLIGHT REFUSALS — {}/{} latest malformed refusal(s) complete (prediction intentionally absent)\n",
            report.preflight.complete, report.preflight.total
        ));
        if let Some(window) = &report.preflight.window {
            out.push_str(&format!(
                "    refusal window spans ts_ms {}..{} across {}/{} timestamped refusal(s)\n",
                window.oldest_ts_ms,
                window.newest_ts_ms,
                window.timestamped,
                report.preflight.total
            ));
        }
        for (field, count) in &report.preflight.defects {
            out.push_str(&format!(
                "    missing or mismatched preflight {field}: {count}\n"
            ));
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
        observation.outcome = Some("42".into());
        observation.verdict = Some("ok".into());
        observation.evaluator_id = Some("tool-outcome-v1".into());
        observation.latency_ms = Some(3);
        observation.semantic_success = Some(true);
        observation.prediction_error = Some(0.5);
        observation.brier = Some(0.25);
        observation.lesson = Some("the execution matched the prior".into());

        let mut orphan = DecisionEvent::new("legacy", "tool_observed");
        orphan.parent_event_id = Some("missing".into());
        orphan.verdict = Some("failed".into());

        let unobserved = DecisionEvent::span("abandoned", None, "tool_predicted");

        let events = [
            compile.clone(),
            prediction.clone(),
            observation.clone(),
            orphan,
            unobserved,
        ];
        let typed = tool_chain_completeness(&events);
        assert_eq!((typed.complete, typed.total), (1, 3));
        assert_eq!(typed.defects.get("prediction link"), Some(&1));
        assert_eq!(typed.defects.get("observation link"), Some(&1));
        assert_eq!(typed.defects.get("evaluator_id"), Some(&1));
        assert!(typed.window.is_some());
        let report = render_tool_chain_completeness(&events);
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

        let mut blank_prediction = prediction.clone();
        blank_prediction.actor = Some("   ".into());
        blank_prediction.predicted = Some("\t".into());
        blank_prediction.ts_ms = 0;
        let mut blank_observation = observation.clone();
        blank_observation.actor = blank_prediction.actor.clone();
        blank_observation.evaluator_id = Some(" ".into());
        blank_observation.ts_ms = 0;
        let blank =
            render_tool_chain_completeness(&[compile.clone(), blank_prediction, blank_observation]);
        assert!(blank.contains("0/1 latest call(s) complete"), "{blank}");
        for defect in [
            "actor",
            "predicted outcome",
            "evaluator_id",
            "prediction ts_ms",
            "observation ts_ms",
        ] {
            assert!(
                blank.contains(&format!("missing or mismatched {defect}: 1")),
                "{blank}"
            );
        }

        let mut blank_prediction_id = prediction.clone();
        blank_prediction_id.event_id = Some(" ".into());
        let mut observation_for_blank_prediction = observation.clone();
        observation_for_blank_prediction.parent_event_id = blank_prediction_id.event_id.clone();
        let blank = render_tool_chain_completeness(&[
            compile.clone(),
            blank_prediction_id,
            observation_for_blank_prediction,
        ]);
        assert!(
            blank.contains("missing or mismatched prediction event_id: 1"),
            "{blank}"
        );

        let mut blank_observation_id = observation.clone();
        blank_observation_id.event_id = Some(" ".into());
        let blank = render_tool_chain_completeness(&[
            compile.clone(),
            prediction.clone(),
            blank_observation_id,
        ]);
        assert!(
            blank.contains("missing or mismatched observation event_id: 1"),
            "{blank}"
        );

        let mut contradictory_observation = observation.clone();
        contradictory_observation.semantic_success = Some(false);
        let contradictory = render_tool_chain_completeness(&[
            compile.clone(),
            prediction.clone(),
            contradictory_observation,
        ]);
        assert!(
            contradictory.contains("0/1 latest call(s) complete"),
            "{contradictory}"
        );
        assert!(
            contradictory.contains("missing or mismatched semantic_success consistency: 1"),
            "{contradictory}"
        );
        let mut contradictory_empty = observation.clone();
        contradictory_empty.verdict = Some("empty".into());
        contradictory_empty.semantic_success = Some(true);
        let contradictory = render_tool_chain_completeness(&[
            compile.clone(),
            prediction.clone(),
            contradictory_empty,
        ]);
        assert!(
            contradictory.contains("missing or mismatched semantic_success consistency: 1"),
            "{contradictory}"
        );

        let mut invalid_verdict = observation.clone();
        invalid_verdict.verdict = Some("mystery".into());
        let invalid =
            render_tool_chain_completeness(&[compile.clone(), prediction.clone(), invalid_verdict]);
        assert!(
            invalid.contains("missing or mismatched actual verdict: 1"),
            "{invalid}"
        );

        let mut wrong_evaluator = observation.clone();
        wrong_evaluator.evaluator_id = Some("unversioned-evaluator".into());
        let wrong_evaluator =
            render_tool_chain_completeness(&[compile.clone(), prediction.clone(), wrong_evaluator]);
        assert!(
            wrong_evaluator.contains("missing or mismatched evaluator_id: 1"),
            "{wrong_evaluator}"
        );

        let mut incomplete_observation = observation.clone();
        incomplete_observation.outcome = None;
        incomplete_observation.lesson = Some("   ".into());
        let incomplete = render_tool_chain_completeness(&[
            compile.clone(),
            prediction.clone(),
            incomplete_observation,
        ]);
        assert!(
            incomplete.contains("missing or mismatched actual outcome: 1"),
            "{incomplete}"
        );
        assert!(
            incomplete.contains("missing or mismatched lesson: 1"),
            "{incomplete}"
        );

        let mut inconsistent_grade = observation.clone();
        inconsistent_grade.prediction_error = Some(0.25);
        inconsistent_grade.brier = Some(0.125);
        let inconsistent = render_tool_chain_completeness(&[
            compile.clone(),
            prediction.clone(),
            inconsistent_grade,
        ]);
        assert!(
            inconsistent.contains("missing or mismatched prediction_error consistency: 1"),
            "{inconsistent}"
        );
        assert!(
            inconsistent.contains("missing or mismatched brier consistency: 1"),
            "{inconsistent}"
        );

        let mut falsely_graded_denial = observation.clone();
        falsely_graded_denial.verdict = Some("denied".into());
        falsely_graded_denial.semantic_success = None;
        let excluded = render_tool_chain_completeness(&[
            compile.clone(),
            prediction.clone(),
            falsely_graded_denial,
        ]);
        assert!(
            excluded.contains("missing or mismatched calibration exclusion: 1"),
            "{excluded}"
        );

        let mut out_of_order = observation.clone();
        out_of_order.ts_ms = prediction.ts_ms - 1;
        let out_of_order =
            render_tool_chain_completeness(&[compile.clone(), prediction.clone(), out_of_order]);
        assert!(
            out_of_order.contains("missing or mismatched temporal ordering: 1"),
            "{out_of_order}"
        );

        let mut duplicate_root = compile.clone();
        duplicate_root.trace_id = "foreign-trace".into();
        let ambiguous_root = render_tool_chain_completeness(&[
            compile.clone(),
            duplicate_root,
            prediction.clone(),
            observation.clone(),
        ]);
        assert!(
            ambiguous_root.contains("missing or mismatched bounded root integrity: 1"),
            "{ambiguous_root}"
        );

        let mut zero_time_root = compile.clone();
        zero_time_root.ts_ms = 0;
        let zero_time_root = render_tool_chain_completeness(&[
            zero_time_root,
            prediction.clone(),
            observation.clone(),
        ]);
        assert!(
            zero_time_root.contains("missing or mismatched bounded root integrity: 1"),
            "{zero_time_root}"
        );

        let mut blank_root = compile.clone();
        blank_root.event_id = Some("   ".into());
        let mut blank_root_prediction = prediction.clone();
        blank_root_prediction.parent_event_id = blank_root.event_id.clone();
        let blank_root = render_tool_chain_completeness(&[
            blank_root,
            blank_root_prediction,
            observation.clone(),
        ]);
        assert!(
            blank_root.contains("missing or mismatched bounded root integrity: 1"),
            "{blank_root}"
        );

        let mut future_root = compile.clone();
        future_root.ts_ms = prediction.ts_ms + 1;
        let future_root =
            render_tool_chain_completeness(&[future_root, prediction.clone(), observation.clone()]);
        assert!(
            future_root.contains("missing or mismatched bounded root integrity: 1"),
            "{future_root}"
        );

        let mut dangling_root_prediction = prediction.clone();
        dangling_root_prediction.parent_event_id = Some("missing-compile-root".into());
        let dangling_root =
            render_tool_chain_completeness(&[dangling_root_prediction, observation.clone()]);
        assert!(
            dangling_root.contains("missing or mismatched bounded root linkage: 1"),
            "{dangling_root}"
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
    fn tool_chain_completeness_separates_strict_preflight_refusals_from_calls() {
        let mut refusal = DecisionEvent::new("refused", "tool_observed");
        refusal.ts_ms = 42;
        refusal.actor = Some("conversation".into());
        refusal.lane = Some("primary".into());
        refusal.context_fingerprint = Some("context:opaque".into());
        refusal.goal_id = Some("freeform:opaque".into());
        refusal.tool_version = Some("mind-conversation/0.1.0".into());
        refusal.model_route = Some("scripted".into());
        refusal.object_id = Some("calc:malformed".into());
        refusal.outcome = Some("missing required expression".into());
        refusal.verdict = Some("malformed".into());
        refusal.evaluator_id = Some("tool-outcome-v1".into());
        refusal.lesson = Some("planner arguments did not fit the tool".into());
        refusal.event_id = Some("refusal-1".into());

        let typed = tool_chain_completeness(&[refusal.clone()]);
        assert_eq!((typed.complete, typed.total), (0, 0));
        assert_eq!((typed.preflight.complete, typed.preflight.total), (1, 1));
        assert_eq!(typed.preflight.window.as_ref().unwrap().timestamped, 1);
        let report = render_tool_chain_completeness(&[refusal.clone()]);
        assert!(report.starts_with("No tool-chain calls yet"), "{report}");
        assert!(
            report.contains("PREFLIGHT REFUSALS — 1/1 latest malformed refusal(s) complete"),
            "{report}"
        );
        assert!(
            report.contains("prediction intentionally absent"),
            "{report}"
        );
        assert!(
            report.contains("refusal window spans ts_ms 42..42 across 1/1 timestamped refusal(s)"),
            "{report}"
        );
        assert!(!report.contains("prediction link"), "{report}");

        // Production parents a preflight refusal directly to goal_compiled so it remains in the
        // run's causal tree without pretending that a tool prediction existed.
        let mut root = DecisionEvent::new("refused", "goal_compiled");
        root.event_id = Some("goal-root".into());
        root.ts_ms = 41;
        refusal.parent_event_id = Some("goal-root".into());
        let report = render_tool_chain_completeness(&[root.clone(), refusal.clone()]);
        assert!(report.starts_with("No tool-chain calls yet"), "{report}");
        assert!(
            report.contains("PREFLIGHT REFUSALS — 1/1 latest malformed refusal(s) complete"),
            "{report}"
        );
        assert!(!report.contains("prediction link"), "{report}");

        let mut wrong_evaluator = refusal.clone();
        wrong_evaluator.parent_event_id = None;
        wrong_evaluator.evaluator_id = Some("unversioned-evaluator".into());
        let report = render_tool_chain_completeness(&[wrong_evaluator]);
        assert!(report.contains("PREFLIGHT REFUSALS — 0/1"), "{report}");
        assert!(
            report.contains("missing or mismatched preflight evaluator_id: 1"),
            "{report}"
        );

        let mut falsely_graded = refusal.clone();
        falsely_graded.parent_event_id = None;
        falsely_graded.prediction_error = Some(0.25);
        falsely_graded.brier = Some(0.0625);
        let report = render_tool_chain_completeness(&[falsely_graded]);
        assert!(report.contains("PREFLIGHT REFUSALS — 0/1"), "{report}");
        assert!(
            report.contains("missing or mismatched preflight calibration exclusion: 1"),
            "{report}"
        );

        // Parentless is only the standalone-bus shape when the trace declares no compiled root.
        // Once a bounded root exists, dropping its parent link is a causal defect, not an exemption.
        let mut missing_parent = refusal.clone();
        missing_parent.parent_event_id = None;
        let report = render_tool_chain_completeness(&[root.clone(), missing_parent]);
        assert!(report.contains("0/1 latest call(s) complete"), "{report}");
        assert!(
            report.contains("missing or mismatched prediction link: 1"),
            "{report}"
        );
        assert!(!report.contains("PREFLIGHT REFUSALS"), "{report}");

        let mut zero_time_root = root.clone();
        zero_time_root.ts_ms = 0;
        let report = render_tool_chain_completeness(&[zero_time_root, refusal.clone()]);
        assert!(report.contains("PREFLIGHT REFUSALS — 0/1"), "{report}");
        assert!(
            report.contains("missing or mismatched preflight bounded root integrity: 1"),
            "{report}"
        );

        let mut future_root = root.clone();
        future_root.ts_ms = refusal.ts_ms + 1;
        let report = render_tool_chain_completeness(&[future_root, refusal.clone()]);
        assert!(report.contains("PREFLIGHT REFUSALS — 0/1"), "{report}");
        assert!(
            report.contains("missing or mismatched preflight bounded root integrity: 1"),
            "{report}"
        );

        let mut blank_root = root.clone();
        blank_root.event_id = Some("   ".into());
        let mut blank_parent_refusal = refusal.clone();
        blank_parent_refusal.parent_event_id = blank_root.event_id.clone();
        let report = render_tool_chain_completeness(&[blank_root, blank_parent_refusal]);
        assert!(report.contains("PREFLIGHT REFUSALS — 0/1"), "{report}");
        assert!(
            report.contains("missing or mismatched preflight bounded root integrity: 1"),
            "{report}"
        );

        // Duplicate span IDs make the alleged root ambiguous; ambiguity must fail closed rather
        // than letting insertion order decide whether an observation escapes the call gate.
        let mut duplicate = DecisionEvent::new("refused", "tool_predicted");
        duplicate.event_id = Some("goal-root".into());
        let report = render_tool_chain_completeness(&[root.clone(), duplicate, refusal.clone()]);
        assert!(report.contains("0/1 latest call(s) complete"), "{report}");
        assert!(!report.contains("PREFLIGHT REFUSALS"), "{report}");

        // An ID match across traces is not causal linkage. A cross-trace alleged root must remain
        // visible as a broken ordinary call rather than qualifying for the refusal exemption.
        let mut cross_trace_root = root.clone();
        cross_trace_root.trace_id = "other-trace".into();
        let report = render_tool_chain_completeness(&[cross_trace_root, refusal.clone()]);
        assert!(report.contains("0/1 latest call(s) complete"), "{report}");
        assert!(
            report.contains("missing or mismatched prediction link: 1"),
            "{report}"
        );
        assert!(!report.contains("PREFLIGHT REFUSALS"), "{report}");

        // A bounded run has exactly one compiled root. Even individually unique root IDs are
        // ambiguous when the trace declares two roots, so neither can grant an exemption.
        let mut second_root = DecisionEvent::new("refused", "goal_compiled");
        second_root.event_id = Some("second-root".into());
        let report = render_tool_chain_completeness(&[root.clone(), second_root, refusal.clone()]);
        assert!(report.contains("0/1 latest call(s) complete"), "{report}");
        assert!(!report.contains("PREFLIGHT REFUSALS"), "{report}");

        // Mixing the two populations must not let safe refusals dilute the executed-call gate.
        let call_root = DecisionEvent::span("mixed", None, "goal_compiled");
        let mut prediction =
            DecisionEvent::span("mixed", call_root.event_id.as_deref(), "tool_predicted");
        prediction.actor = Some("conversation".into());
        prediction.lane = Some("primary".into());
        prediction.context_fingerprint = Some("context:opaque".into());
        prediction.goal_id = Some("goal:mixed".into());
        prediction.tool_version = Some("mind-conversation/0.1.0".into());
        prediction.model_route = Some("scripted".into());
        prediction.object_id = Some("calc:opaque".into());
        prediction.predicted = Some("usable output".into());
        prediction.confidence = Some(0.5);
        let mut observation =
            DecisionEvent::span("mixed", prediction.event_id.as_deref(), "tool_observed");
        observation.actor = prediction.actor.clone();
        observation.lane = prediction.lane.clone();
        observation.context_fingerprint = prediction.context_fingerprint.clone();
        observation.goal_id = prediction.goal_id.clone();
        observation.tool_version = prediction.tool_version.clone();
        observation.model_route = prediction.model_route.clone();
        observation.object_id = prediction.object_id.clone();
        observation.outcome = Some("42".into());
        observation.verdict = Some("ok".into());
        observation.evaluator_id = Some("tool-outcome-v1".into());
        observation.latency_ms = Some(3);
        observation.semantic_success = Some(true);
        observation.prediction_error = Some(0.5);
        observation.brier = Some(0.25);
        observation.lesson = Some("the execution matched the prior".into());
        refusal.parent_event_id = None;
        let report =
            render_tool_chain_completeness(&[call_root, prediction, observation, refusal.clone()]);
        assert!(report.contains("1/1 latest call(s) complete"), "{report}");
        assert!(report.contains("PREFLIGHT REFUSALS — 1/1"), "{report}");

        let mut incomplete = refusal.clone();
        incomplete.parent_event_id = None;
        incomplete.ts_ms = 0;
        incomplete.evaluator_id = None;
        incomplete.outcome = None;
        let report = render_tool_chain_completeness(&[incomplete]);
        assert!(report.contains("PREFLIGHT REFUSALS — 0/1"), "{report}");
        assert!(
            report.contains("missing or mismatched preflight evaluator_id: 1"),
            "{report}"
        );
        assert!(
            report.contains("missing or mismatched preflight outcome: 1"),
            "{report}"
        );
        assert!(
            report.contains("missing or mismatched preflight ts_ms: 1"),
            "{report}"
        );

        let mut empty = refusal.clone();
        empty.parent_event_id = None;
        empty.event_id = Some(String::new());
        empty.lesson = Some("   ".into());
        let report = render_tool_chain_completeness(&[empty]);
        assert!(report.contains("PREFLIGHT REFUSALS — 0/1"), "{report}");
        assert!(
            report.contains("missing or mismatched preflight event_id: 1"),
            "{report}"
        );
        assert!(
            report.contains("missing or mismatched preflight lesson: 1"),
            "{report}"
        );

        let duplicate_refusal = refusal.clone();
        let report = render_tool_chain_completeness(&[refusal.clone(), duplicate_refusal]);
        assert!(report.contains("PREFLIGHT REFUSALS — 0/2"), "{report}");
        assert!(
            report.contains("missing or mismatched preflight event_id uniqueness: 2"),
            "{report}"
        );

        // A verdict cannot claim the exemption while also claiming to be the child of a prediction.
        // With a dangling parent it remains an ordinary broken chain.
        refusal.parent_event_id = Some("missing-prediction".into());
        let report = render_tool_chain_completeness(&[refusal]);
        assert!(report.contains("0/1 latest call(s) complete"), "{report}");
        assert!(
            report.contains("missing or mismatched prediction link: 1"),
            "{report}"
        );
        assert!(!report.contains("PREFLIGHT REFUSALS"), "{report}");
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
            event.outcome = Some("42".into());
            event.verdict = Some("ok".into());
            event.evaluator_id = Some("tool-outcome-v1".into());
            event.latency_ms = Some(3);
            event.semantic_success = Some(true);
            event.prediction_error = Some(0.5);
            event.brier = Some(0.25);
            event.lesson = Some("the execution matched the prior".into());
            event
        };

        let first = observation("observation-1");
        let second = observation("observation-2");
        let report = render_tool_chain_completeness(&[prediction.clone(), first.clone(), second]);
        assert!(report.contains("0/2 latest call(s) complete"), "{report}");
        assert!(
            report.contains("missing or mismatched observation cardinality: 2"),
            "{report}"
        );

        let duplicate_prediction = prediction.clone();
        let report =
            render_tool_chain_completeness(&[prediction.clone(), duplicate_prediction, first]);
        assert!(report.contains("0/1 latest call(s) complete"), "{report}");
        assert!(
            report.contains("missing or mismatched prediction cardinality: 1"),
            "{report}"
        );

        let mut second_prediction = prediction.clone();
        second_prediction.event_id = Some("prediction-2".into());
        let first = observation("shared-observation");
        let mut second = observation("shared-observation");
        second.parent_event_id = second_prediction.event_id.clone();
        let report =
            render_tool_chain_completeness(&[prediction, first, second_prediction, second]);
        assert!(report.contains("0/2 latest call(s) complete"), "{report}");
        assert!(
            report.contains("missing or mismatched observation event_id uniqueness: 2"),
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
    fn operator_tail_is_bounded_verified_and_resanitized() {
        let path = scratch("operator_tail");
        let log = DecisionLog::open(&path);
        for i in 0..=DECISION_TAIL_MAX {
            log.record(ev("tail", "decision", &format!("n{i}")));
        }
        // Simulate a valid legacy record whose text predates append-time redaction. The public
        // operator-tail reader must still scan it before returning anything to a UI.
        let mut legacy = DecisionEvent::new("tail", "decision");
        legacy.goal = Some("my password is hunter2 and must never reach a dashboard".into());
        let previous = chain_head(&path).expect("existing chain head");
        append_chained(&path, &legacy, &previous).expect("append valid legacy-shaped record");

        let tail = log
            .read_tail_verified(usize::MAX)
            .expect("the complete chain verifies");
        assert_eq!(tail.len(), DECISION_TAIL_MAX, "the UI ceiling is binding");
        assert_eq!(
            tail.last().and_then(|event| event.goal.as_deref()),
            Some("[redacted-secret]")
        );
        assert!(
            log.read_tail_verified(0)
                .expect("zero is still verified")
                .is_empty(),
            "a zero-sized request returns no records"
        );

        // Corruption anywhere in the chain withholds the whole tail rather than presenting a
        // plausible-looking suffix as verified evidence.
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"torn\":")
            .unwrap();
        assert!(log.read_tail_verified(10).is_err());
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

// ───────────────────────────── L1 (ARCH7): the loop ledger, v3 ─────────────────────────────
// Code-owned bounded schema (Codex's L1 reviews): a loop tick is built ONLY from the typed parts
// below — loop, host, opportunity, considered signals, policy lines, result — so no raw content
// can be constructed into the log; the renderer parses every row back through the same types and
// counts what it cannot read as malformed. Opportunities dedupe by id (an act wins over a held
// record for the same window; otherwise the latest stands) and duplicates are reported.

macro_rules! bounded_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];
            pub fn as_str(self) -> &'static str { match self { $($name::$variant => $text),+ } }
            pub fn parse(text: &str) -> Option<Self> { match text { $($text => Some($name::$variant)),+, _ => None } }
        }
    };
}

bounded_enum! {
    /// Every background loop the ledger knows. Adding a loop is a schema change, on purpose.
    LoopId {
        Dmn => "dmn", Knock => "knock", Digest => "digest", Ask => "ask", Patterns => "patterns",
        HomeWatch => "home-watch", Resolve => "resolve", ProfileRefresh => "profile-refresh",
        Family => "family", FollowUp => "follow-up", PriceWatch => "price-watch",
        MemberBeat => "member-beat", Ics => "ics", LeaseSweep => "lease-sweep",
        MailSweep => "mail-sweep", Whois => "whois", TraditionPrep => "tradition-prep",
        Heartbeat => "heartbeat", WorldShadow => "world-shadow",
    }
}
bounded_enum! {
    /// Who ran the loop: the Telegram poll loop, the headless heartbeat, or (L3a) the
    /// process-hosted runner that runs on every box. The executor, never the delivery surface.
    LoopHost { Telegram => "telegram", Headless => "headless", Process => "process" }
}
bounded_enum! {
    /// What a loop did when it acted. Counts ride in `outcome` as `count:<n>`; no content ever.
    LoopOutcome {
        Dreamed => "dreamed", Knocked => "knocked", Evaluated => "evaluated",
        DigestSent => "digest-sent", NothingToSay => "nothing-to-say", Asked => "asked",
        NothingToAsk => "nothing-to-ask", Ran => "ran", Delegations => "delegations",
        Surfaced => "surfaced", FoundUndelivered => "found-undelivered",
        NothingFound => "nothing-found", FoundQueued => "found-queued",
    }
}
bounded_enum! {
    /// Why a due loop did not act.
    HeldReason {
        IdleGate => "idle-gate", QuietHours => "quiet-hours", Receptivity => "receptivity",
        Disabled => "disabled", SpokeAlready => "spoke-already", NothingDue => "nothing-due",
        NoChat => "no-chat", Budget => "budget", NoPresence => "no-presence",
    }
}
bounded_enum! {
    /// The signals a loop may say it weighed. A closed list: a new signal is a schema change.
    ConsideredSignal {
        Tensions => "tensions", Beliefs => "beliefs", PaperDesk => "paper-desk",
        Packets => "packets", Receptivity => "receptivity", DailyCap => "daily-cap",
        Urges => "urges", ExecutiveShadow => "executive-shadow", Name => "name",
        Purpose => "purpose", FollowUps => "follow-ups", DueDelegations => "due-delegations",
        DueHorizons => "due-horizons",
    }
}
bounded_enum! {
    /// A named budget a loop consulted.
    BudgetKind {
        DmnOneCall => "dmn-one-call", ReceptivityGate => "receptivity-gate",
        ResolveGrade => "resolve-grade", ProfileLearnOneCall => "profile-learn-one-call",
        PatternsOneCall => "patterns-one-call",
    }
}
bounded_enum! {
    /// A named cap a loop consulted.
    CapKind { OnePerDay => "one-per-day", OneOutstanding => "one-outstanding" }
}
bounded_enum! {
    /// L3b: what a loop handed to the delivery seam. A closed list; the text never rides here.
    DeliveryKind {
        Verdict => "verdict", ProfileRefresh => "profile-refresh", Pattern => "pattern",
        HorizonTick => "horizon-tick", Knock => "knock", Digest => "digest", Ask => "ask",
    }
}
bounded_enum! {
    /// L3b: where the delivery seam put a line. Only `telegram-accepted` is DELIVERED; a queued
    /// console notice is a promise the cockpit still has to keep, and the journal is nowhere.
    DeliveryOutcome {
        TelegramAccepted => "telegram-accepted", ConsoleQueued => "console-queued",
        Undelivered => "undelivered", HeldNoPresence => "held-no-presence",
    }
}

pub const DELIVERY_LEDGER_VERSION: &str = "delivery-ledger-v1";

/// L3b: one delivery decision — kind, outcome, the receipt id when one exists, and the size.
/// Typed so no free text reaches the log; recorded exactly once per `deliver` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryTick {
    pub kind: DeliveryKind,
    pub outcome: DeliveryOutcome,
    /// The console notice id for `console-queued`; none otherwise.
    pub receipt_id: Option<String>,
    pub chars: u32,
}

impl DeliveryTick {
    /// Only Telegram acceptance counts as delivered (Codex's L3b accounting note).
    pub fn delivered(&self) -> bool {
        self.outcome == DeliveryOutcome::TelegramAccepted
    }
    pub fn to_event(&self, ts_ms: u64) -> DecisionEvent {
        let mut ev = DecisionEvent::new(
            &format!("delivery-{}-{ts_ms}", self.kind.as_str()),
            "delivery",
        );
        ev.ts_ms = ts_ms;
        ev.actor = Some("delivery".into());
        ev.lane = Some("primary".into());
        ev.goal_id = Some(format!("delivery:{}", self.kind.as_str()));
        ev.trigger = Some(self.outcome.as_str().into());
        ev.object_id = self.receipt_id.clone();
        ev.chosen = Some(self.outcome.as_str().into());
        ev.verdict = Some(
            if self.delivered() {
                "delivered"
            } else {
                "undelivered"
            }
            .into(),
        );
        ev.outcome = Some(format!("chars:{}", self.chars));
        ev.evaluator_id = Some(DELIVERY_LEDGER_VERSION.into());
        ev
    }
}

/// A fully validated stored delivery row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedDelivery {
    pub kind: DeliveryKind,
    pub outcome: DeliveryOutcome,
    pub receipt_id: Option<String>,
    pub chars: u32,
}

pub fn parse_delivery(e: &DecisionEvent) -> Option<ParsedDelivery> {
    if e.kind != "delivery" || e.evaluator_id.as_deref() != Some(DELIVERY_LEDGER_VERSION) {
        return None;
    }
    let kind = DeliveryKind::parse(e.goal_id.as_deref()?.strip_prefix("delivery:")?)?;
    let outcome = DeliveryOutcome::parse(e.trigger.as_deref()?)?;
    if e.chosen.as_deref()? != outcome.as_str() || e.actor.as_deref()? != "delivery" {
        return None;
    }
    let expected_verdict = if outcome == DeliveryOutcome::TelegramAccepted {
        "delivered"
    } else {
        "undelivered"
    };
    if e.verdict.as_deref()? != expected_verdict {
        return None;
    }
    let chars = e
        .outcome
        .as_deref()?
        .strip_prefix("chars:")?
        .parse::<u32>()
        .ok()?;
    if (outcome == DeliveryOutcome::ConsoleQueued) != e.object_id.is_some() {
        return None;
    }
    // A console receipt id is exactly the store's shape: `notice:` + 64 lower-hex.
    if let Some(id) = e.object_id.as_deref() {
        let hex = id.strip_prefix("notice:")?;
        if hex.len() != 64
            || !hex
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return None;
        }
    }
    Some(ParsedDelivery {
        kind,
        outcome,
        receipt_id: e.object_id.clone(),
        chars,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct DeliveryLedgerRow {
    pub kind: String,
    pub telegram_accepted: u32,
    pub console_queued: u32,
    pub undelivered: u32,
    /// L3c: an engaging line held because nobody was there to see it; nothing was queued.
    pub held_no_presence: u32,
    pub chars: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct DeliveryLedger {
    pub version: String,
    pub rows: Vec<DeliveryLedgerRow>,
    pub malformed: usize,
}

/// The delivery ledger over `[now_ms - window_ms, now_ms]`: one row per kind, counts by outcome.
pub fn delivery_ledger(events: &[DecisionEvent], now_ms: u64, window_ms: u64) -> DeliveryLedger {
    let since = now_ms.saturating_sub(window_ms);
    let mut malformed = 0usize;
    let mut rows: std::collections::BTreeMap<String, DeliveryLedgerRow> = Default::default();
    for e in events
        .iter()
        .filter(|e| e.kind == "delivery" && e.ts_ms >= since && e.ts_ms <= now_ms)
    {
        let Some(d) = parse_delivery(e) else {
            malformed += 1;
            continue;
        };
        let row = rows
            .entry(d.kind.as_str().to_string())
            .or_insert_with(|| DeliveryLedgerRow {
                kind: d.kind.as_str().into(),
                ..Default::default()
            });
        match d.outcome {
            DeliveryOutcome::TelegramAccepted => row.telegram_accepted += 1,
            DeliveryOutcome::ConsoleQueued => row.console_queued += 1,
            DeliveryOutcome::Undelivered => row.undelivered += 1,
            DeliveryOutcome::HeldNoPresence => row.held_no_presence += 1,
        }
        row.chars += u64::from(d.chars);
    }
    DeliveryLedger {
        version: DELIVERY_LEDGER_VERSION.into(),
        rows: rows.into_values().collect(),
        malformed,
    }
}

pub fn render_delivery_ledger_at(events: &[DecisionEvent], now_ms: u64) -> String {
    let ledger = delivery_ledger(events, now_ms, 24 * 60 * 60 * 1000);
    if ledger.rows.is_empty() {
        return format!(
            "No deliveries in the last 24 h (as of ts_ms {}); malformed {}.",
            now_ms, ledger.malformed
        );
    }
    let mut out = format!(
        "DELIVERY LEDGER {} — last 24 h as of ts_ms {} ({} kind(s); malformed {})\n",
        ledger.version,
        now_ms,
        ledger.rows.len(),
        ledger.malformed
    );
    for r in ledger.rows {
        out.push_str(&format!(
            "  {:<16} telegram-accepted {:>3} · console-queued {:>3} · undelivered {:>3} · held-no-presence {:>3} · chars {}\n",
            r.kind, r.telegram_accepted, r.console_queued, r.undelivered, r.held_no_presence, r.chars
        ));
    }
    out.push_str("Only telegram-accepted is delivered; a queued notice is a promise the cockpit keeps by acknowledging it.\n");
    out
}

pub fn render_delivery_ledger(events: &[DecisionEvent]) -> String {
    render_delivery_ledger_at(events, now_ms())
}

/// A policy line the loop actually consulted, typed so it renders bounded text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopPolicy {
    Cadence(u64),
    Idle(u64),
    Beat(u64),
    Report(u64),
    Budget(BudgetKind),
    Cap(CapKind),
}
impl LoopPolicy {
    pub fn render(self) -> String {
        match self {
            LoopPolicy::Cadence(s) => format!("cadence:{s}s"),
            LoopPolicy::Idle(s) => format!("idle:{s}s"),
            LoopPolicy::Beat(s) => format!("beat:{s}s"),
            LoopPolicy::Report(s) => format!("report:{s}s"),
            LoopPolicy::Budget(b) => format!("budget:{}", b.as_str()),
            LoopPolicy::Cap(c) => format!("cap:{}", c.as_str()),
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        let (k, v) = text.split_once(':')?;
        let secs = |v: &str| v.strip_suffix('s').and_then(|d| d.parse::<u64>().ok());
        match k {
            "cadence" => secs(v).map(LoopPolicy::Cadence),
            "idle" => secs(v).map(LoopPolicy::Idle),
            "beat" => secs(v).map(LoopPolicy::Beat),
            "report" => secs(v).map(LoopPolicy::Report),
            "budget" => BudgetKind::parse(v).map(LoopPolicy::Budget),
            "cap" => CapKind::parse(v).map(LoopPolicy::Cap),
            _ => None,
        }
    }
}

/// The unit the ledger records: one due opportunity of one loop. Ids are rendered by the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopOpportunity {
    /// The due window a legacy timer opens; `process_start` makes it unique across restarts.
    Window {
        loop_id: LoopId,
        process_start_ms: u64,
        key: u64,
    },
    /// One idle stretch, keyed by the activity that opened it.
    Stretch { loop_id: LoopId, start_ms: u64 },
    /// A wall-clock bucket (may repeat across a restart — the aggregate dedupes and reports).
    Bucket { loop_id: LoopId, n: u64 },
    /// An operator-forced whois (`ym whois`): its own opportunity, keyed by its instant. The
    /// loop is fixed by the variant — no other loop can be forced.
    Forced { at_ms: u64 },
}
impl LoopOpportunity {
    pub fn loop_id(self) -> LoopId {
        match self {
            LoopOpportunity::Window { loop_id, .. }
            | LoopOpportunity::Stretch { loop_id, .. }
            | LoopOpportunity::Bucket { loop_id, .. } => loop_id,
            LoopOpportunity::Forced { .. } => LoopId::Whois,
        }
    }
    pub fn id(self) -> String {
        match self {
            LoopOpportunity::Window {
                loop_id,
                process_start_ms,
                key,
            } => {
                format!("{}:due:{process_start_ms}:{key}", loop_id.as_str())
            }
            LoopOpportunity::Stretch { loop_id, start_ms } => {
                format!("{}:idle:{start_ms}", loop_id.as_str())
            }
            LoopOpportunity::Bucket { loop_id, n } => format!("{}:bucket:{n}", loop_id.as_str()),
            LoopOpportunity::Forced { at_ms } => format!("whois:forced:{at_ms}"),
        }
    }
    /// The inverse of `id`, strict: anything that is not exactly one of the three shapes fails.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split(':');
        let loop_id = LoopId::parse(parts.next()?)?;
        let kind = parts.next()?;
        let a = parts.next()?.parse::<u64>().ok()?;
        match (kind, parts.next(), parts.next()) {
            ("due", Some(b), None) => Some(LoopOpportunity::Window {
                loop_id,
                process_start_ms: a,
                key: b.parse().ok()?,
            }),
            ("idle", None, None) => Some(LoopOpportunity::Stretch {
                loop_id,
                start_ms: a,
            }),
            ("bucket", None, None) => Some(LoopOpportunity::Bucket { loop_id, n: a }),
            ("forced", None, None) if loop_id == LoopId::Whois => {
                Some(LoopOpportunity::Forced { at_ms: a })
            }
            _ => None,
        }
    }
}

/// L3a bumped v3 → v4 for the `process` host (a bounded-enum addition is a schema change by this
/// ledger's own rule). v3 rows stay in the log and read as superseded by version, never malformed.
pub const LOOP_LEDGER_VERSION: &str = "loop-ledger-v4";
pub const LOOP_LEDGER_V3: &str = "loop-ledger-v3";

/// One loop tick. Fields are private: the only way to build one is from the typed parts, so the
/// log can never receive free text under this kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopTick {
    opportunity: LoopOpportunity,
    host: LoopHost,
    considered: Vec<ConsideredSignal>,
    policy: Vec<LoopPolicy>,
    result: Result<LoopOutcome, HeldReason>,
    count: Option<u32>,
    /// Operation-local model calls the loop body itself reported; `None` = unknown. Never a
    /// global counter, never inferred from a send.
    model_calls: Option<u32>,
    wall_ms: u64,
}
impl LoopTick {
    pub fn acted(opportunity: LoopOpportunity, host: LoopHost, outcome: LoopOutcome) -> Self {
        Self {
            opportunity,
            host,
            considered: Vec::new(),
            policy: Vec::new(),
            result: Ok(outcome),
            count: None,
            model_calls: None,
            wall_ms: 0,
        }
    }
    pub fn held(opportunity: LoopOpportunity, host: LoopHost, reason: HeldReason) -> Self {
        Self {
            opportunity,
            host,
            considered: Vec::new(),
            policy: Vec::new(),
            result: Err(reason),
            count: None,
            model_calls: None,
            wall_ms: 0,
        }
    }
    pub fn considered(mut self, signals: &[ConsideredSignal]) -> Self {
        self.considered = signals.to_vec();
        self
    }
    pub fn policy(mut self, lines: &[LoopPolicy]) -> Self {
        self.policy = lines.to_vec();
        self
    }
    pub fn count(mut self, n: u32) -> Self {
        self.count = Some(n);
        self
    }
    pub fn model_calls(mut self, n: Option<u32>) -> Self {
        self.model_calls = n;
        self
    }
    pub fn wall_ms(mut self, ms: u64) -> Self {
        self.wall_ms = ms;
        self
    }
    pub fn opportunity_id(&self) -> String {
        self.opportunity.id()
    }
    /// The event exactly as the ledger stores it; `parse_tick` is its inverse.
    pub fn to_event(&self, ts_ms: u64) -> DecisionEvent {
        let loop_id = self.opportunity.loop_id();
        let mut ev = DecisionEvent::new(&format!("loop-{}", self.opportunity.id()), "loop_tick");
        ev.ts_ms = ts_ms;
        ev.actor = Some(format!("loop:{}", loop_id.as_str()));
        ev.lane = Some("primary".into());
        ev.goal_id = Some(format!("loop:{}", loop_id.as_str()));
        ev.trigger = Some(self.host.as_str().into());
        ev.object_id = Some(self.opportunity.id());
        ev.candidates = self
            .considered
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        ev.policy = self.policy.iter().map(|p| p.render()).collect();
        match self.result {
            Ok(outcome) => {
                ev.chosen = Some(outcome.as_str().into());
                ev.verdict = Some("acted".into());
            }
            Err(held) => ev.verdict = Some(format!("held:{}", held.as_str())),
        }
        ev.outcome = self.count.map(|c| format!("count:{c}"));
        ev.model_calls = self.model_calls;
        ev.latency_ms = Some(self.wall_ms);
        ev.evaluator_id = Some(LOOP_LEDGER_VERSION.into());
        ev
    }
}

/// A fully validated stored row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTick {
    pub opportunity: LoopOpportunity,
    pub host: LoopHost,
    pub considered: Vec<ConsideredSignal>,
    pub policy: Vec<LoopPolicy>,
    pub result: Result<LoopOutcome, HeldReason>,
    pub count: Option<u32>,
}

/// Validate one stored row against the whole schema; `None` when any field cannot be read.
pub fn parse_tick(e: &DecisionEvent) -> Option<ParsedTick> {
    if e.kind != "loop_tick" {
        return None;
    }
    let opportunity = LoopOpportunity::parse(e.object_id.as_deref()?)?;
    let loop_id = opportunity.loop_id();
    let expected = format!("loop:{}", loop_id.as_str());
    if e.goal_id.as_deref()? != expected || e.actor.as_deref()? != expected {
        return None;
    }
    let host = LoopHost::parse(e.trigger.as_deref()?)?;
    let considered = e
        .candidates
        .iter()
        .map(|c| ConsideredSignal::parse(c))
        .collect::<Option<Vec<_>>>()?;
    let policy = e
        .policy
        .iter()
        .map(|p| LoopPolicy::parse(p))
        .collect::<Option<Vec<_>>>()?;
    let result = match e.verdict.as_deref()? {
        "acted" => Ok(LoopOutcome::parse(e.chosen.as_deref()?)?),
        v => {
            if e.chosen.is_some() {
                return None;
            }
            Err(HeldReason::parse(v.strip_prefix("held:")?)?)
        }
    };
    let count = match e.outcome.as_deref() {
        None => None,
        Some(o) => Some(o.strip_prefix("count:")?.parse::<u32>().ok()?),
    };
    Some(ParsedTick {
        opportunity,
        host,
        considered,
        policy,
        result,
        count,
    })
}

/// Aggregate view of one loop over a window. Reasons, labels and counts only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopLedgerRow {
    pub loop_id: String,
    pub hosts: std::collections::BTreeSet<String>,
    pub opportunities: usize,
    pub acted: usize,
    pub held: std::collections::BTreeMap<String, usize>,
    pub outcomes: std::collections::BTreeMap<String, usize>,
    /// How many opportunities named each signal / policy line.
    pub considered: std::collections::BTreeMap<String, usize>,
    pub policy: std::collections::BTreeMap<String, usize>,
    pub counted: u64,
    /// Opportunities of this loop that were emitted more than once (held then acted, or a
    /// restart re-emitting a wall-clock bucket) — reduced to one row each, counted here so a
    /// paired analysis can report with and without them.
    pub duplicated_opportunities: usize,
    /// Ticks whose model-call count the loop body reported, and their sum; unknown is not zero.
    pub model_calls_measured: usize,
    pub model_calls: u64,
    pub wall_ms: u64,
    pub first_ts_ms: u64,
    pub last_ts_ms: u64,
}

/// The whole ledger over a window anchored to a clock the CALLER names.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopLedger {
    pub version: String,
    pub now_ms: u64,
    pub since_ms: u64,
    pub loops: Vec<LoopLedgerRow>,
    /// `loop_tick` rows the schema could not read.
    pub malformed: usize,
    /// Rows written by an earlier ledger version, excluded.
    pub superseded: usize,
    /// The NUMBER OF OPPORTUNITY IDS that appeared more than once (a restart re-emitting a
    /// wall-clock bucket, or a held record followed by its act); each collapsed to one row.
    pub duplicates: usize,
}

/// The loop ledger over `[now_ms - window_ms, now_ms]`, anchored to the caller's clock.
pub fn loop_ledger(events: &[DecisionEvent], now_ms: u64, window_ms: u64) -> LoopLedger {
    let since = now_ms.saturating_sub(window_ms);
    let mut malformed = 0usize;
    let mut superseded = 0usize;
    // One row per OPPORTUNITY: an act wins over a held record for the same id; otherwise the
    // latest record stands.
    let mut by_id: std::collections::BTreeMap<String, (&DecisionEvent, ParsedTick)> =
        Default::default();
    let mut dup_ids: std::collections::BTreeSet<String> = Default::default();
    for e in events
        .iter()
        .filter(|e| e.kind == "loop_tick" && e.ts_ms >= since && e.ts_ms <= now_ms)
    {
        if e.evaluator_id.as_deref() != Some(LOOP_LEDGER_VERSION) {
            superseded += 1;
            continue;
        }
        let Some(tick) = parse_tick(e) else {
            malformed += 1;
            continue;
        };
        let id = tick.opportunity.id();
        match by_id.get(&id) {
            None => {
                by_id.insert(id, (e, tick));
            }
            Some((prev, prev_tick)) => {
                dup_ids.insert(id.clone());
                // Input-order invariant: an act beats a hold; within the same class the later
                // timestamp wins, and on an equal timestamp the later input wins.
                let replace = match (tick.result.is_ok(), prev_tick.result.is_ok()) {
                    (true, false) => true,
                    (false, true) => false,
                    _ => e.ts_ms >= prev.ts_ms,
                };
                if replace {
                    by_id.insert(id, (e, tick));
                }
            }
        }
    }
    let duplicates = dup_ids.len();
    let mut rows: std::collections::BTreeMap<LoopId, LoopLedgerRow> = Default::default();
    for (e, tick) in by_id.into_values() {
        let id = tick.opportunity.loop_id();
        let row = rows.entry(id).or_insert_with(|| LoopLedgerRow {
            loop_id: id.as_str().into(),
            first_ts_ms: e.ts_ms,
            ..Default::default()
        });
        row.opportunities += 1;
        if dup_ids.contains(&tick.opportunity.id()) {
            row.duplicated_opportunities += 1;
        }
        row.hosts.insert(tick.host.as_str().into());
        match tick.result {
            Ok(outcome) => {
                row.acted += 1;
                *row.outcomes.entry(outcome.as_str().into()).or_insert(0) += 1;
            }
            Err(held) => *row.held.entry(held.as_str().into()).or_insert(0) += 1,
        }
        for c in &tick.considered {
            *row.considered.entry(c.as_str().into()).or_insert(0) += 1;
        }
        for p in &tick.policy {
            *row.policy.entry(p.render()).or_insert(0) += 1;
        }
        row.counted += u64::from(tick.count.unwrap_or(0));
        if let Some(c) = e.model_calls {
            row.model_calls_measured += 1;
            row.model_calls += u64::from(c);
        }
        row.wall_ms += e.latency_ms.unwrap_or(0);
        row.first_ts_ms = row.first_ts_ms.min(e.ts_ms);
        row.last_ts_ms = row.last_ts_ms.max(e.ts_ms);
    }
    LoopLedger {
        version: LOOP_LEDGER_VERSION.into(),
        now_ms,
        since_ms: since,
        loops: rows.into_values().collect(),
        malformed,
        superseded,
        duplicates,
    }
}

fn join_counts(m: &std::collections::BTreeMap<String, usize>) -> String {
    if m.is_empty() {
        return "—".to_string();
    }
    m.iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `ym why loops`: the last 24 hours of the mind's idle time, one loop per block, anchored to now.
pub fn render_loop_ledger_at(events: &[DecisionEvent], now_ms: u64) -> String {
    let ledger = loop_ledger(events, now_ms, 24 * 60 * 60 * 1000);
    if ledger.loops.is_empty() {
        return format!(
            "No loop opportunities in the last 24 h (as of ts_ms {}); superseded rows {}, malformed {}, duplicates {}.",
            now_ms, ledger.superseded, ledger.malformed, ledger.duplicates
        );
    }
    let mut out = format!(
        "LOOP LEDGER {} — last 24 h as of ts_ms {} ({} loop(s); superseded {}, malformed {}, duplicates {})\n",
        ledger.version,
        now_ms,
        ledger.loops.len(),
        ledger.superseded,
        ledger.malformed,
        ledger.duplicates
    );
    for r in ledger.loops {
        let calls = if r.model_calls_measured == 0 {
            "unknown".to_string()
        } else {
            format!(
                "{} (reported on {}/{})",
                r.model_calls, r.model_calls_measured, r.opportunities
            )
        };
        out.push_str(&format!(
            "  {:<16} host {:<9} opportunities {:>4} · acted {:>3} · held [{}] · model calls {} · wall {} ms · counted {}\n      outcomes: {}\n      considered: {}\n      policy: {}\n",
            r.loop_id,
            r.hosts.iter().cloned().collect::<Vec<_>>().join("+"),
            r.opportunities,
            r.acted,
            join_counts(&r.held),
            calls,
            r.wall_ms,
            r.counted,
            join_counts(&r.outcomes),
            join_counts(&r.considered),
            join_counts(&r.policy),
        ));
    }
    out
}

/// The `verified_report` shape: anchored to the wall clock at render time.
pub fn render_loop_ledger(events: &[DecisionEvent]) -> String {
    render_loop_ledger_at(events, now_ms())
}

/// The opportunity arithmetic every cadence loop shares, pure so a replayed schedule can prove
/// "one held emission per opportunity". A wall-clock loop with period `p` has one opportunity per
/// bucket `floor(now / p)`; a legacy-timer loop has one per due window (keyed by the timer's
/// last-run stamp); the knock has one per idle stretch. The gate remembers the last key it
/// recorded so a held state is emitted once, and `mark` lets the act that closes a window keep a
/// later wake from adding a held record after it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpportunityGate {
    /// The last key a record (act or hold) was emitted under.
    last_key: Option<u64>,
    /// The last key an ACT was claimed under. Kept apart from `last_key` so a hold recorded
    /// under a key does not starve the act that follows when conditions clear, while a second
    /// act under the same key (a detached body that has not yet moved its stamp) is refused.
    last_acted_key: Option<u64>,
}
impl OpportunityGate {
    pub fn bucket(now_ms: u64, period_secs: u64) -> u64 {
        now_ms / period_secs.max(1).saturating_mul(1000)
    }
    pub fn take_bucket(
        &mut self,
        loop_id: LoopId,
        now_ms: u64,
        period_secs: u64,
    ) -> Option<LoopOpportunity> {
        let n = Self::bucket(now_ms, period_secs);
        self.take_key(n)
            .then_some(LoopOpportunity::Bucket { loop_id, n })
    }
    pub fn take_window(
        &mut self,
        loop_id: LoopId,
        process_start_ms: u64,
        key: u64,
    ) -> Option<LoopOpportunity> {
        self.take_key(key).then_some(LoopOpportunity::Window {
            loop_id,
            process_start_ms,
            key,
        })
    }
    pub fn take_stretch(&mut self, loop_id: LoopId, start_ms: u64) -> Option<LoopOpportunity> {
        self.take_key(start_ms)
            .then_some(LoopOpportunity::Stretch { loop_id, start_ms })
    }
    pub fn mark(&mut self, key: u64) {
        self.last_key = Some(key);
        self.last_acted_key = Some(key);
    }
    /// Claim the act for this opportunity: allowed once per key, even after a hold was
    /// recorded under it (Hold* → one Act); refused for a second act under the same key.
    /// After the claim no hold can be recorded under the key either.
    pub fn take_act(&mut self, key: u64) -> bool {
        if self.last_acted_key == Some(key) {
            return false;
        }
        self.mark(key);
        true
    }
    fn take_key(&mut self, key: u64) -> bool {
        if self.last_key == Some(key) {
            return false;
        }
        self.last_key = Some(key);
        true
    }
}

#[cfg(test)]
mod loop_ledger_tests {
    use super::*;

    /// L3a: the v3 → v4 migration. A v3 row is superseded by version, never malformed; a v4 row
    /// hosted by the process parses; a row with a host the enum does not know is malformed.
    #[test]
    fn v3_rows_are_superseded_never_malformed_and_the_process_host_parses() {
        let w = LoopOpportunity::Window {
            loop_id: LoopId::Ics,
            process_start_ms: 7,
            key: 0,
        };
        let mut v3 = LoopTick::acted(w, LoopHost::Telegram, LoopOutcome::Ran).to_event(10);
        v3.evaluator_id = Some(LOOP_LEDGER_V3.into());
        let v4 = LoopTick::acted(w, LoopHost::Process, LoopOutcome::Ran).to_event(11);
        let mut unknown = LoopTick::acted(w, LoopHost::Process, LoopOutcome::Ran).to_event(12);
        unknown.trigger = Some("mainframe".into());
        let ledger = loop_ledger(&[v3, v4, unknown], 20, 100);
        assert_eq!(ledger.superseded, 1, "the v3 row is superseded by version");
        assert_eq!(ledger.malformed, 1, "an unknown host is malformed");
        assert_eq!(ledger.loops.len(), 1);
        assert!(ledger.loops[0].hosts.contains("process"));
        assert_eq!(LoopHost::parse("process"), Some(LoopHost::Process));
        assert_eq!(LoopHost::parse("mainframe"), None);
    }

    fn ev(t: LoopTick, ts: u64) -> DecisionEvent {
        t.to_event(ts)
    }
    fn dmn_window(key: u64) -> LoopOpportunity {
        LoopOpportunity::Window {
            loop_id: LoopId::Dmn,
            process_start_ms: 77,
            key,
        }
    }

    #[test]
    fn the_schema_round_trips_and_rejects_what_it_does_not_own() {
        for id in LoopId::ALL {
            assert_eq!(LoopId::parse(id.as_str()), Some(*id));
        }
        for p in [
            LoopPolicy::Cadence(300),
            LoopPolicy::Idle(600),
            LoopPolicy::Beat(30),
            LoopPolicy::Report(600),
            LoopPolicy::Budget(BudgetKind::DmnOneCall),
            LoopPolicy::Cap(CapKind::OnePerDay),
        ] {
            assert_eq!(LoopPolicy::parse(&p.render()), Some(p));
        }
        for o in [
            dmn_window(5),
            LoopOpportunity::Stretch {
                loop_id: LoopId::Knock,
                start_ms: 9,
            },
            LoopOpportunity::Bucket {
                loop_id: LoopId::Heartbeat,
                n: 3,
            },
        ] {
            assert_eq!(LoopOpportunity::parse(&o.id()), Some(o));
        }
        assert_eq!(
            LoopOpportunity::parse("dmn:due:77"),
            None,
            "a window needs its key"
        );
        assert_eq!(LoopOpportunity::parse("dmn:idle:1:2"), None);
        assert_eq!(LoopPolicy::parse("cadence:soon"), None);
        let e = ev(
            LoopTick::held(dmn_window(1), LoopHost::Telegram, HeldReason::IdleGate)
                .policy(&[LoopPolicy::Cadence(300)]),
            10,
        );
        assert_eq!(e.verdict.as_deref(), Some("held:idle-gate"));
        assert_eq!(e.object_id.as_deref(), Some("dmn:due:77:1"));
        assert!(e.context_fingerprint.is_none() && e.predicted.is_none());
        let parsed = parse_tick(&e).unwrap();
        assert_eq!(parsed.policy, vec![LoopPolicy::Cadence(300)]);
        // Any field outside the schema makes the row malformed: a free label, a bad count, an
        // actor that disagrees with the opportunity, an act without an outcome.
        let mut bad = e.clone();
        bad.candidates = vec!["vibes".into()];
        assert!(parse_tick(&bad).is_none());
        let mut bad = e.clone();
        bad.outcome = Some("count:many".into());
        assert!(parse_tick(&bad).is_none());
        let mut bad = e.clone();
        bad.actor = Some("loop:knock".into());
        assert!(parse_tick(&bad).is_none());
        let mut bad = e.clone();
        bad.verdict = Some("acted".into());
        assert!(parse_tick(&bad).is_none(), "acted needs an outcome");
    }

    #[test]
    fn the_ledger_is_anchored_dedupes_by_opportunity_and_keeps_unknown_calls_unknown() {
        const B: u64 = 200_000_000_000;
        let mut events = vec![
            ev(
                LoopTick::acted(dmn_window(1), LoopHost::Telegram, LoopOutcome::Dreamed)
                    .count(3)
                    .considered(&[ConsideredSignal::Tensions])
                    .policy(&[LoopPolicy::Cadence(300)]),
                B + 1_000,
            ),
            ev(
                LoopTick::held(dmn_window(2), LoopHost::Telegram, HeldReason::IdleGate)
                    .policy(&[LoopPolicy::Cadence(300)]),
                B + 2_000,
            ),
            ev(
                LoopTick::held(
                    LoopOpportunity::Bucket {
                        loop_id: LoopId::Heartbeat,
                        n: 4,
                    },
                    LoopHost::Headless,
                    HeldReason::NothingDue,
                )
                .policy(&[LoopPolicy::Beat(30), LoopPolicy::Report(600)]),
                B + 3_000,
            ),
            ev(
                LoopTick::acted(dmn_window(0), LoopHost::Telegram, LoopOutcome::Dreamed)
                    .model_calls(Some(1)),
                1_000,
            ), // days ago
        ];
        // A held record and then the act that closed the SAME window: one opportunity, act wins.
        let w = LoopOpportunity::Window {
            loop_id: LoopId::Ask,
            process_start_ms: 77,
            key: 9,
        };
        events.push(ev(
            LoopTick::held(w, LoopHost::Telegram, HeldReason::IdleGate),
            B + 2_500,
        ));
        events.push(ev(
            LoopTick::acted(w, LoopHost::Telegram, LoopOutcome::Asked).model_calls(Some(1)),
            B + 2_600,
        ));
        // A restart re-emitting the same heartbeat bucket: counted once, reported as a duplicate.
        events.push(ev(
            LoopTick::held(
                LoopOpportunity::Bucket {
                    loop_id: LoopId::Heartbeat,
                    n: 4,
                },
                LoopHost::Headless,
                HeldReason::NothingDue,
            )
            .policy(&[LoopPolicy::Beat(30), LoopPolicy::Report(600)]),
            B + 3_100,
        ));
        // A v1/v2 row and a row with an unknown label are counted, not aggregated.
        let mut old = ev(
            LoopTick::acted(dmn_window(3), LoopHost::Telegram, LoopOutcome::Dreamed),
            B + 1_500,
        );
        old.evaluator_id = Some("loop-ledger-v2".into());
        events.push(old);
        let mut bad = ev(
            LoopTick::acted(dmn_window(4), LoopHost::Telegram, LoopOutcome::Dreamed),
            B + 1_600,
        );
        bad.verdict = Some("held:because".into());
        bad.chosen = None;
        events.push(bad);

        let now = B + 4_000;
        let ledger = loop_ledger(&events, now, 24 * 60 * 60 * 1000);
        assert_eq!(
            (ledger.superseded, ledger.malformed, ledger.duplicates),
            (1, 1, 2)
        );
        let ask = ledger.loops.iter().find(|r| r.loop_id == "ask").unwrap();
        assert_eq!((ask.opportunities, ask.acted), (1, 1));
        assert!(ask.held.is_empty());
        let hb = ledger
            .loops
            .iter()
            .find(|r| r.loop_id == "heartbeat")
            .unwrap();
        assert_eq!((hb.opportunities, hb.duplicated_opportunities), (1, 1));
        assert_eq!(
            ask.duplicated_opportunities, 1,
            "held-then-act is one duplicated opportunity"
        );
        assert_eq!(hb.policy.get("beat:30s"), Some(&1));
        let dmn = ledger.loops.iter().find(|r| r.loop_id == "dmn").unwrap();
        assert_eq!((dmn.opportunities, dmn.acted, dmn.counted), (2, 1, 3));
        assert_eq!(dmn.considered.get("tensions"), Some(&1));
        assert_eq!(
            dmn.model_calls_measured, 0,
            "no body reported a count: unknown, not zero"
        );
        // Anchored to `now`, not to the newest event.
        let later = loop_ledger(&events, now + 2 * 24 * 60 * 60 * 1000, 24 * 60 * 60 * 1000);
        assert!(later.loops.is_empty());
        let text = render_loop_ledger_at(&events, now);
        assert!(text.contains("considered: tensions 1") && text.contains("policy: cadence:300s 2"));
        assert!(text.contains("duplicates 2") && text.contains("model calls unknown"));
    }

    /// Codex's v3 review: the reducer must not depend on input order. Three rows for one
    /// opportunity — an older act, a newer act, a hold — must reduce to the NEWER act however
    /// they are ordered, and `duplicates` counts ids, not extra rows.
    #[test]
    fn the_reducer_is_input_order_invariant_and_counts_duplicated_ids() {
        const B: u64 = 200_000_000_000;
        let w = LoopOpportunity::Window {
            loop_id: LoopId::Digest,
            process_start_ms: 1,
            key: 5,
        };
        let older = ev(
            LoopTick::acted(w, LoopHost::Telegram, LoopOutcome::NothingToSay),
            B + 1_000,
        );
        let newer = ev(
            LoopTick::acted(w, LoopHost::Telegram, LoopOutcome::DigestSent),
            B + 2_000,
        );
        let held = ev(
            LoopTick::held(w, LoopHost::Telegram, HeldReason::IdleGate),
            B + 3_000,
        );
        let orders: [[&DecisionEvent; 3]; 4] = [
            [&older, &newer, &held],
            [&newer, &older, &held],
            [&held, &newer, &older],
            [&held, &older, &newer],
        ];
        for order in orders {
            let events: Vec<DecisionEvent> = order.iter().map(|e| (*e).clone()).collect();
            let ledger = loop_ledger(&events, B + 4_000, 24 * 60 * 60 * 1000);
            let row = ledger.loops.iter().find(|r| r.loop_id == "digest").unwrap();
            assert_eq!((row.opportunities, row.acted), (1, 1));
            assert_eq!(
                row.outcomes.get("digest-sent"),
                Some(&1),
                "the newer act stands"
            );
            assert!(row.held.is_empty(), "a hold never survives an act");
            assert_eq!(
                ledger.duplicates, 1,
                "one duplicated id, whatever the row count"
            );
            assert_eq!(row.duplicated_opportunities, 1);
        }
        // Equal timestamps: the later input wins (deterministic, documented).
        let a = ev(
            LoopTick::acted(w, LoopHost::Telegram, LoopOutcome::NothingToSay),
            B + 9,
        );
        let b = ev(
            LoopTick::acted(w, LoopHost::Telegram, LoopOutcome::DigestSent),
            B + 9,
        );
        let ledger = loop_ledger(&[a.clone(), b.clone()], B + 100, 1_000);
        assert_eq!(ledger.loops[0].outcomes.get("digest-sent"), Some(&1));
        let ledger = loop_ledger(&[b, a], B + 100, 1_000);
        assert_eq!(ledger.loops[0].outcomes.get("nothing-to-say"), Some(&1));
    }

    #[test]
    fn a_replayed_schedule_yields_one_held_record_per_opportunity_and_an_act_marks_its_window() {
        // Wake every 1.5 s for an hour against a 300 s bucket: 12 buckets, 12 held ids.
        let mut gate = OpportunityGate::default();
        let mut ids = Vec::new();
        let mut t = 1_788_300_000_000u64;
        while t < 1_788_300_000_000 + 3_600_000 {
            if let Some(o) = gate.take_bucket(LoopId::Heartbeat, t, 300) {
                ids.push(o.id());
            }
            t += 1_500;
        }
        assert_eq!(ids.len(), 12);
        assert_eq!(
            ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            12
        );
        // A legacy-timer window: held once, then the act marks it, then no more held records.
        let mut g = OpportunityGate::default();
        assert!(g.take_window(LoopId::Dmn, 77, 1_000).is_some());
        assert!(g.take_window(LoopId::Dmn, 77, 1_000).is_none());
        g.mark(1_000);
        assert!(g.take_window(LoopId::Dmn, 77, 1_000).is_none());
        assert!(
            g.take_window(LoopId::Dmn, 77, 2_000).is_some(),
            "a new window records again"
        );
        // A stretch records once however many wakes it sees.
        let mut k = OpportunityGate::default();
        assert!(
            k.take_stretch(LoopId::Knock, 5).is_some()
                && k.take_stretch(LoopId::Knock, 5).is_none()
        );
    }
}

// ───────────────────────────── L1 (ARCH7): the gate decision ─────────────────────────────
// What one wake decides for one gate. The decision itself is made by `Gated` below, per kind,
// in the legacy blocker order; nothing else decides.

/// What one wake decides for one gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Not due: nothing runs, nothing is recorded (cadence itself is never a recorded skip).
    NotDue,
    /// Due and clear: the body runs; the act records under this window.
    Act,
    /// Due but held: the body does not run; the hold records once per window.
    Hold(HeldReason),
}

// ───────────────────────────── L1 (ARCH7): the legacy gate kinds ─────────────────────────────
// Each background loop's run predicate is one `LegacyGate` kind. A site never assembles the
// gate's state by hand: it calls the constructor of its kind, which takes ONLY the inputs that
// kind reads, as named fields — a kind cannot be handed a signal it does not consult, and two
// booleans cannot be swapped positionally. The fixture below replays a day of states through a
// test-local transcription of every legacy predicate (never through this code) and requires the
// seam to agree on every wake: due-ness, run/hold, the first blocker's name, and the SET of
// opportunity ids emitted against the due occurrences the transcription enumerates.

/// A legacy timer or persisted cadence: where it is, when it last ran, how often it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timer {
    pub now_ms: u64,
    /// The legacy timer's last-run stamp, or a persisted cadence's last-run stamp.
    pub last_ms: u64,
    /// The effective period in ms (a persisted cadence's may be domain-paced).
    pub period_ms: u64,
}

impl Timer {
    /// Due-ness as every legacy timer computes it. The one place the arithmetic lives.
    pub fn due(&self) -> bool {
        self.now_ms.saturating_sub(self.last_ms) >= self.period_ms
    }
}

/// Whether a chat exists to speak into, and whether quiet hours block speaking now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Presence {
    pub chat_present: bool,
    pub quiet: bool,
}

/// What the idle-gated loop reads besides its timer and presence, by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdleInputs {
    /// The loop's own switch.
    pub enabled: bool,
    /// Another loop already spoke this wake; this one yields.
    pub spoke: bool,
    /// The user has been idle for the required stretch.
    pub idle: bool,
}

/// Everything a gate can read on one wake. Constructed only through `Gated`'s per-kind
/// constructors; fields a kind does not consult are left at their neutral value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateState {
    pub now_ms: u64,
    pub last_ms: u64,
    pub period_ms: u64,
    pub enabled: bool,
    pub chat_present: bool,
    pub quiet: bool,
    pub idle: bool,
    pub spoke: bool,
    pub receptive: bool,
    pub forced: bool,
}

impl GateState {
    fn neutral(timer: Timer) -> Self {
        GateState {
            now_ms: timer.now_ms,
            last_ms: timer.last_ms,
            period_ms: timer.period_ms,
            enabled: true,
            chat_present: true,
            quiet: false,
            idle: true,
            spoke: false,
            receptive: true,
            forced: false,
        }
    }
}

/// The distinct run predicates the poll loop contains, each with its legacy blocker order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyGate {
    /// `enabled && due && chat && !quiet` — home-watch, family, follow-up, price-watch.
    TimerChatQuiet,
    /// `due` — resolve, profile-refresh, ICS, lease-sweep.
    TimerUnconditional,
    /// `due && !quiet` — member-beat.
    TimerQuiet,
    /// `!spoke && enabled && chat && !quiet && idle && due` — patterns.
    IdleGated,
    /// `chat && !quiet && due && receptive` — tradition-prep; whois when not forced.
    PersistedReceptive,
    /// `!quiet && due && chat` — mail-sweep (quiet is checked before due; chat before run).
    PersistedChatQuiet,
    /// `chat && forced` — a forced whois runs regardless of quiet, due or receptivity.
    Forced,
}

/// One wake of one gate: the kind and exactly the state that kind reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gated {
    kind: LegacyGate,
    state: GateState,
}

impl Gated {
    /// Home-watch, family, follow-up, price-watch: a chat-facing timer with an on/off switch.
    pub fn timer_chat_quiet(timer: Timer, presence: Presence, enabled: bool) -> Self {
        Gated {
            kind: LegacyGate::TimerChatQuiet,
            state: GateState {
                enabled,
                chat_present: presence.chat_present,
                quiet: presence.quiet,
                ..GateState::neutral(timer)
            },
        }
    }
    /// Resolve, profile-refresh, ICS, lease-sweep: runs whenever due, speaks to nobody.
    pub fn timer(timer: Timer) -> Self {
        Gated {
            kind: LegacyGate::TimerUnconditional,
            state: GateState::neutral(timer),
        }
    }
    /// Member-beat: due and not quiet; no chat needed.
    pub fn timer_quiet(timer: Timer, quiet: bool) -> Self {
        Gated {
            kind: LegacyGate::TimerQuiet,
            state: GateState {
                quiet,
                ..GateState::neutral(timer)
            },
        }
    }
    /// Patterns: yields to a loop that already spoke, then its switch, then the chat, quiet
    /// hours and the idle stretch (the legacy `idle_ok`, split into its three reasons).
    pub fn idle_gated(timer: Timer, presence: Presence, idle: IdleInputs) -> Self {
        Gated {
            kind: LegacyGate::IdleGated,
            state: GateState {
                enabled: idle.enabled,
                spoke: idle.spoke,
                chat_present: presence.chat_present,
                quiet: presence.quiet,
                idle: idle.idle,
                ..GateState::neutral(timer)
            },
        }
    }
    /// Tradition-prep and an unforced whois: a persisted daily cadence behind receptivity.
    pub fn persisted_receptive(timer: Timer, presence: Presence, receptive: bool) -> Self {
        Gated {
            kind: LegacyGate::PersistedReceptive,
            state: GateState {
                chat_present: presence.chat_present,
                quiet: presence.quiet,
                receptive,
                ..GateState::neutral(timer)
            },
        }
    }
    /// Mail-sweep: a persisted daily cadence that checks quiet first and needs a chat to run.
    pub fn persisted_chat_quiet(timer: Timer, presence: Presence) -> Self {
        Gated {
            kind: LegacyGate::PersistedChatQuiet,
            state: GateState {
                chat_present: presence.chat_present,
                quiet: presence.quiet,
                ..GateState::neutral(timer)
            },
        }
    }
    /// A forced whois: its own occurrence at this instant, needing only a chat.
    pub fn forced(now_ms: u64, chat_present: bool) -> Self {
        Gated {
            kind: LegacyGate::Forced,
            state: GateState {
                chat_present,
                forced: true,
                ..GateState::neutral(Timer {
                    now_ms,
                    last_ms: 0,
                    period_ms: 0,
                })
            },
        }
    }

    pub fn kind(&self) -> LegacyGate {
        self.kind
    }
    pub fn state(&self) -> &GateState {
        &self.state
    }
    /// Due-ness as the legacy code computes it for this kind.
    pub fn due(&self) -> bool {
        match self.kind {
            // A forced ask is only evaluated when a chat exists (legacy nests it under
            // `chat != 0`); without one it is not an opportunity at all.
            LegacyGate::Forced => self.state.forced && self.state.chat_present,
            _ => Timer {
                now_ms: self.state.now_ms,
                last_ms: self.state.last_ms,
                period_ms: self.state.period_ms,
            }
            .due(),
        }
    }
    /// The legacy timer's next last-run stamp after this wake's decision — the reset rule per
    /// kind, transcribed from the poll loop: a chat-facing timer resets whenever it is due and
    /// its switch is on (whether or not it ran); an unconditional timer resets when it runs;
    /// member-beat and patterns reset only when they run; a persisted cadence's stamp is
    /// written by the run itself, so the site's value never moves here; a forced occurrence has
    /// no timer.
    pub fn advance(&self, decision: GateDecision) -> u64 {
        let st = &self.state;
        match (self.kind, decision) {
            (_, GateDecision::NotDue) => st.last_ms,
            (LegacyGate::TimerChatQuiet, GateDecision::Hold(HeldReason::Disabled)) => st.last_ms,
            (LegacyGate::TimerChatQuiet, _) => st.now_ms,
            (LegacyGate::TimerUnconditional, _) => st.now_ms,
            (LegacyGate::TimerQuiet | LegacyGate::IdleGated, GateDecision::Act) => st.now_ms,
            (LegacyGate::TimerQuiet | LegacyGate::IdleGated, GateDecision::Hold(_)) => st.last_ms,
            (
                LegacyGate::PersistedReceptive
                | LegacyGate::PersistedChatQuiet
                | LegacyGate::Forced,
                _,
            ) => st.last_ms,
        }
    }
    /// The decision, with each kind's blockers checked in the order the legacy code checked
    /// them, so a hold names the FIRST legacy blocker, not the seam's generic precedence.
    pub fn decide(&self) -> GateDecision {
        if !self.due() {
            return GateDecision::NotDue;
        }
        let st = &self.state;
        let hold = |r: HeldReason| GateDecision::Hold(r);
        match self.kind {
            LegacyGate::TimerChatQuiet => {
                if !st.enabled {
                    hold(HeldReason::Disabled)
                } else if !st.chat_present {
                    hold(HeldReason::NoChat)
                } else if st.quiet {
                    hold(HeldReason::QuietHours)
                } else {
                    GateDecision::Act
                }
            }
            LegacyGate::TimerUnconditional => GateDecision::Act,
            LegacyGate::TimerQuiet => {
                if st.quiet {
                    hold(HeldReason::QuietHours)
                } else {
                    GateDecision::Act
                }
            }
            LegacyGate::IdleGated => {
                if st.spoke {
                    hold(HeldReason::SpokeAlready)
                } else if !st.enabled {
                    hold(HeldReason::Disabled)
                } else if !st.chat_present {
                    hold(HeldReason::NoChat)
                } else if st.quiet {
                    hold(HeldReason::QuietHours)
                } else if !st.idle {
                    hold(HeldReason::IdleGate)
                } else {
                    GateDecision::Act
                }
            }
            LegacyGate::PersistedReceptive => {
                if !st.chat_present {
                    hold(HeldReason::NoChat)
                } else if st.quiet {
                    hold(HeldReason::QuietHours)
                } else if !st.receptive {
                    hold(HeldReason::Receptivity)
                } else {
                    GateDecision::Act
                }
            }
            LegacyGate::PersistedChatQuiet => {
                if st.quiet {
                    hold(HeldReason::QuietHours)
                } else if !st.chat_present {
                    hold(HeldReason::NoChat)
                } else {
                    GateDecision::Act
                }
            }
            LegacyGate::Forced => GateDecision::Act,
        }
    }
}

#[cfg(test)]
mod legacy_gate_tests {
    use super::*;

    // ── The oracle: every legacy predicate transcribed HERE, from the poll loop as it was
    // before L1b, calling nothing in the production gate. A bug in `Timer::due`, in a
    // constructor's wiring, or in `Gated::decide` must fail against this, not agree with it.

    fn oracle_due(kind: LegacyGate, st: &GateState) -> bool {
        match kind {
            LegacyGate::Forced => st.forced && st.chat_present,
            _ => st.now_ms >= st.last_ms && st.now_ms - st.last_ms >= st.period_ms,
        }
    }

    fn oracle_runs(kind: LegacyGate, st: &GateState) -> bool {
        let due = oracle_due(kind, st);
        match kind {
            LegacyGate::TimerChatQuiet => st.enabled && due && st.chat_present && !st.quiet,
            LegacyGate::TimerUnconditional => due,
            LegacyGate::TimerQuiet => due && !st.quiet,
            LegacyGate::IdleGated => {
                !st.spoke && st.enabled && st.chat_present && !st.quiet && st.idle && due
            }
            LegacyGate::PersistedReceptive => st.chat_present && !st.quiet && due && st.receptive,
            LegacyGate::PersistedChatQuiet => !st.quiet && due && st.chat_present,
            LegacyGate::Forced => st.chat_present && st.forced,
        }
    }

    /// The first condition the legacy `if` chain fails on, for a due wake that does not run.
    fn oracle_first_blocker(kind: LegacyGate, st: &GateState) -> Option<HeldReason> {
        let checks: &[(bool, HeldReason)] = match kind {
            LegacyGate::TimerChatQuiet => &[
                (!st.enabled, HeldReason::Disabled),
                (!st.chat_present, HeldReason::NoChat),
                (st.quiet, HeldReason::QuietHours),
            ],
            LegacyGate::TimerUnconditional | LegacyGate::Forced => &[],
            LegacyGate::TimerQuiet => &[(st.quiet, HeldReason::QuietHours)],
            LegacyGate::IdleGated => &[
                (st.spoke, HeldReason::SpokeAlready),
                (!st.enabled, HeldReason::Disabled),
                (!st.chat_present, HeldReason::NoChat),
                (st.quiet, HeldReason::QuietHours),
                (!st.idle, HeldReason::IdleGate),
            ],
            LegacyGate::PersistedReceptive => &[
                (!st.chat_present, HeldReason::NoChat),
                (st.quiet, HeldReason::QuietHours),
                (!st.receptive, HeldReason::Receptivity),
            ],
            LegacyGate::PersistedChatQuiet => &[
                (st.quiet, HeldReason::QuietHours),
                (!st.chat_present, HeldReason::NoChat),
            ],
        };
        checks.iter().find(|(blocks, _)| *blocks).map(|(_, r)| *r)
    }

    /// The legacy reset rule, transcribed from where each `last_x = now` sits in the poll loop.
    fn oracle_next_last(kind: LegacyGate, st: &GateState) -> u64 {
        let due = oracle_due(kind, st);
        let runs = oracle_runs(kind, st);
        match kind {
            // `if enabled { if due { last = now; if chat && !quiet { run } } }`
            LegacyGate::TimerChatQuiet => {
                if st.enabled && due {
                    st.now_ms
                } else {
                    st.last_ms
                }
            }
            // `if due { run; last = now }`
            LegacyGate::TimerUnconditional => {
                if due {
                    st.now_ms
                } else {
                    st.last_ms
                }
            }
            // `if due && !quiet { run; last = now }` / `if !spoke && on && idle_ok && due { run; last = now }`
            LegacyGate::TimerQuiet | LegacyGate::IdleGated => {
                if runs {
                    st.now_ms
                } else {
                    st.last_ms
                }
            }
            LegacyGate::PersistedReceptive
            | LegacyGate::PersistedChatQuiet
            | LegacyGate::Forced => st.last_ms,
        }
    }

    /// The constructor a site of this kind calls, driven from the raw signals of the wake —
    /// so the fixture exercises the same wiring the sites use.
    fn gated_for(kind: LegacyGate, st: &GateState) -> Gated {
        let timer = Timer {
            now_ms: st.now_ms,
            last_ms: st.last_ms,
            period_ms: st.period_ms,
        };
        let presence = Presence {
            chat_present: st.chat_present,
            quiet: st.quiet,
        };
        match kind {
            LegacyGate::TimerChatQuiet => Gated::timer_chat_quiet(timer, presence, st.enabled),
            LegacyGate::TimerUnconditional => Gated::timer(timer),
            LegacyGate::TimerQuiet => Gated::timer_quiet(timer, st.quiet),
            LegacyGate::IdleGated => Gated::idle_gated(
                timer,
                presence,
                IdleInputs {
                    enabled: st.enabled,
                    spoke: st.spoke,
                    idle: st.idle,
                },
            ),
            LegacyGate::PersistedReceptive => {
                Gated::persisted_receptive(timer, presence, st.receptive)
            }
            LegacyGate::PersistedChatQuiet => Gated::persisted_chat_quiet(timer, presence),
            LegacyGate::Forced => Gated::forced(st.now_ms, st.chat_present),
        }
    }

    /// A deterministic day of wakes with the state changing on a fixed schedule; every kind
    /// must agree with the oracle on every wake, and the set of opportunity ids emitted must
    /// equal the set of due occurrences the oracle enumerates.
    fn day(kind: LegacyGate, period_ms: u64) -> (usize, usize, usize, usize) {
        let start = 1_788_300_000_000u64;
        let mut t = start;
        let end = start + 24 * 60 * 60 * 1000;
        let mut last = 0u64;
        let mut acts = 0usize;
        let mut holds = 0usize;
        let mut due_occurrences = std::collections::BTreeSet::new();
        let mut emitted = std::collections::BTreeSet::new();
        // Raw attempts per opportunity key, in order (true = act, false = hold), so repeated
        // acts under one key cannot hide inside the emitted SET.
        let mut attempts: std::collections::BTreeMap<u64, Vec<bool>> = Default::default();
        let mut gate = OpportunityGate::default();
        let mut wake_no = 0u64;
        while t < end {
            let hour = (t - start) / 3_600_000;
            let st = GateState {
                now_ms: t,
                last_ms: last,
                period_ms,
                enabled: hour != 5,
                chat_present: hour >= 1,
                quiet: hour < 8 || (hour == 5),
                idle: (wake_no / 40) % 3 != 0,
                spoke: wake_no % 97 == 0,
                receptive: (wake_no / 7) % 4 != 0,
                forced: wake_no % 1_000 == 0,
            };
            if kind == LegacyGate::Forced && !st.forced {
                // The site constructs a forced gate only on a forced wake; on any other wake
                // there is no forced opportunity to decide on.
                t += 1_500;
                wake_no += 1;
                continue;
            }
            let gated = gated_for(kind, &st);
            assert_eq!(gated.kind(), kind);
            let due = oracle_due(kind, &st);
            assert_eq!(
                gated.due(),
                due,
                "{kind:?}: due-ness disagrees with the oracle at {t}"
            );
            let legacy = oracle_runs(kind, &st);
            let decision = gated.decide();
            match decision {
                GateDecision::NotDue => assert!(!due, "{kind:?}: NotDue while the oracle is due"),
                GateDecision::Act => {
                    assert!(legacy, "{kind:?}: seam acts where legacy would not run");
                    acts += 1;
                }
                GateDecision::Hold(reason) => {
                    assert!(
                        due && !legacy,
                        "{kind:?}: seam holds where legacy would run"
                    );
                    assert_eq!(
                        Some(reason),
                        oracle_first_blocker(kind, &st),
                        "{kind:?}: hold does not name the first legacy blocker at {t}"
                    );
                    holds += 1;
                }
            }
            if legacy {
                assert_eq!(
                    decision,
                    GateDecision::Act,
                    "{kind:?}: legacy runs but seam does not act"
                );
            }
            // The oracle enumerates due occurrences (one per distinct last-run window while
            // due; one per instant when forced) and the ids the site would emit for them.
            if due {
                let key = if kind == LegacyGate::Forced { t } else { last };
                due_occurrences.insert(key);
                match decision {
                    GateDecision::Act => {
                        let opp = if kind == LegacyGate::Forced {
                            LoopOpportunity::Forced { at_ms: t }
                        } else {
                            LoopOpportunity::Window {
                                loop_id: LoopId::HomeWatch,
                                process_start_ms: start,
                                key: last,
                            }
                        };
                        gate.mark(key);
                        emitted.insert(opp.id());
                        attempts.entry(key).or_default().push(true);
                    }
                    GateDecision::Hold(_) => {
                        if let Some(opp) = gate.take_window(LoopId::HomeWatch, start, last) {
                            emitted.insert(opp.id());
                        }
                        attempts.entry(key).or_default().push(false);
                    }
                    GateDecision::NotDue => {}
                }
            }
            // The site's timer moves exactly as the legacy reset rule says, on every wake.
            let next = gated.advance(decision);
            assert_eq!(
                next,
                oracle_next_last(kind, &st),
                "{kind:?}: timer transition differs at {t}"
            );
            last = next;
            // A persisted cadence's stamp is written by the run itself, outside the site:
            // model that external write so the next wake opens a new window.
            if matches!(
                kind,
                LegacyGate::PersistedReceptive | LegacyGate::PersistedChatQuiet
            ) && decision == GateDecision::Act
            {
                last = t;
            }
            t += 1_500;
            wake_no += 1;
        }
        // One act per opportunity at most, and never anything after it: a key may hold
        // (repeatedly, where the timer does not reset on a hold) and then act, but an act
        // closes the opportunity.
        for (key, seq) in &attempts {
            let acts_here = seq.iter().filter(|a| **a).count();
            assert!(acts_here <= 1, "{kind:?}: {acts_here} acts under key {key}");
            if let Some(pos) = seq.iter().position(|a| *a) {
                assert_eq!(
                    pos + 1,
                    seq.len(),
                    "{kind:?}: an attempt follows the act under key {key}"
                );
            }
        }
        (acts, holds, due_occurrences.len(), emitted.len())
    }

    /// The detached mail sweep's sequence at the gate: a hold recorded under a window must
    /// not starve the act that follows when conditions clear, and a wake that arrives before
    /// the body has moved the persisted stamp must not spawn the same sweep again.
    #[test]
    fn a_detached_act_runs_once_per_window_and_is_never_starved_by_an_earlier_hold() {
        let mut g = OpportunityGate::default();
        // Hold(quiet) then Act: the hold records once, the act spawns once.
        assert!(g.take_window(LoopId::MailSweep, 1, 100).is_some());
        assert!(g.take_window(LoopId::MailSweep, 1, 100).is_none());
        assert!(g.take_act(100), "an earlier hold must not starve the act");
        assert!(
            !g.take_act(100),
            "a second wake before the stamp moved must not spawn again"
        );
        assert!(
            g.take_window(LoopId::MailSweep, 1, 100).is_none(),
            "no hold after the act"
        );
        // Act then Act under the next window: spawns once.
        assert!(g.take_act(200));
        assert!(!g.take_act(200));
        // The stamp moved: a fresh window, a fresh act.
        assert!(g.take_act(300));
    }

    #[test]
    fn every_kind_agrees_with_the_test_local_oracle_over_a_replayed_day() {
        for (kind, period_ms) in [
            (LegacyGate::TimerChatQuiet, 120_000),
            (LegacyGate::TimerUnconditional, 3_600_000),
            (LegacyGate::TimerQuiet, 120_000),
            (LegacyGate::IdleGated, 600_000),
            (LegacyGate::PersistedReceptive, 86_400_000 / 4),
            (LegacyGate::PersistedChatQuiet, 86_400_000 / 4),
        ] {
            let (acts, holds, due, emitted) = day(kind, period_ms);
            assert!(acts > 0, "{kind:?} never acted");
            if kind != LegacyGate::TimerUnconditional {
                assert!(holds > 0, "{kind:?} never held");
            }
            assert_eq!(emitted, due, "{kind:?}: emitted ids != due occurrences");
        }
        let (acts, _holds, due, emitted) = day(LegacyGate::Forced, 0);
        assert!(acts > 0);
        assert_eq!(emitted, due);
    }

    /// A due wake with several blockers at once names the one the legacy chain hit first.
    #[test]
    fn a_hold_names_the_first_legacy_blocker_not_the_generic_precedence() {
        let timer = Timer {
            now_ms: 10_000_000,
            last_ms: 0,
            period_ms: 1_000,
        };
        let dark_and_alone = Presence {
            chat_present: false,
            quiet: true,
        };
        // Mail-sweep checked quiet before the chat.
        assert_eq!(
            Gated::persisted_chat_quiet(timer, dark_and_alone).decide(),
            GateDecision::Hold(HeldReason::QuietHours)
        );
        // Home-watch checked the chat before quiet, and its switch before both.
        assert_eq!(
            Gated::timer_chat_quiet(timer, dark_and_alone, true).decide(),
            GateDecision::Hold(HeldReason::NoChat)
        );
        assert_eq!(
            Gated::timer_chat_quiet(timer, dark_and_alone, false).decide(),
            GateDecision::Hold(HeldReason::Disabled)
        );
        // Patterns yielded to a loop that already spoke before consulting its own switch.
        assert_eq!(
            Gated::idle_gated(
                timer,
                dark_and_alone,
                IdleInputs {
                    enabled: false,
                    spoke: true,
                    idle: false
                }
            )
            .decide(),
            GateDecision::Hold(HeldReason::SpokeAlready)
        );
        assert_eq!(
            Gated::idle_gated(
                timer,
                dark_and_alone,
                IdleInputs {
                    enabled: false,
                    spoke: false,
                    idle: false
                }
            )
            .decide(),
            GateDecision::Hold(HeldReason::Disabled)
        );
        assert_eq!(
            Gated::idle_gated(
                timer,
                dark_and_alone,
                IdleInputs {
                    enabled: true,
                    spoke: false,
                    idle: false
                }
            )
            .decide(),
            GateDecision::Hold(HeldReason::NoChat)
        );
        // Tradition-prep consulted receptivity only past the chat and quiet.
        assert_eq!(
            Gated::persisted_receptive(timer, dark_and_alone, false).decide(),
            GateDecision::Hold(HeldReason::NoChat)
        );
        // A forced ask without a chat is not an opportunity at all.
        assert_eq!(Gated::forced(5, false).decide(), GateDecision::NotDue);
        assert_eq!(Gated::forced(5, true).decide(), GateDecision::Act);
    }

    #[test]
    fn a_forced_occurrence_and_a_persisted_window_are_distinct_ids() {
        let a = LoopOpportunity::Forced { at_ms: 5 };
        let b = LoopOpportunity::Window {
            loop_id: LoopId::Whois,
            process_start_ms: 1,
            key: 5,
        };
        assert_ne!(a.id(), b.id());
        assert_eq!(LoopOpportunity::parse(&a.id()), Some(a));
        assert_eq!(LoopOpportunity::parse("whois:forced:x"), None);
        // Only whois can be forced: the variant carries no loop, and the parser refuses any
        // other loop's forced label.
        assert_eq!(a.loop_id(), LoopId::Whois);
        assert_eq!(a.id(), "whois:forced:5");
        assert_eq!(LoopOpportunity::parse("home-watch:forced:5"), None);
        assert_eq!(LoopOpportunity::parse("dmn:forced:5"), None);
    }
}

#[cfg(test)]
mod delivery_ledger_tests {
    use super::*;

    /// Every outcome round-trips through the typed event; only Telegram acceptance is
    /// delivered; the ledger counts by kind; a doctored row is malformed, never counted.
    #[test]
    fn delivery_records_round_trip_and_only_telegram_counts_as_delivered() {
        let accepted = DeliveryTick {
            kind: DeliveryKind::Verdict,
            outcome: DeliveryOutcome::TelegramAccepted,
            receipt_id: None,
            chars: 42,
        };
        let queued = DeliveryTick {
            kind: DeliveryKind::Pattern,
            outcome: DeliveryOutcome::ConsoleQueued,
            receipt_id: Some(format!("notice:{}", "0123456789abcdef".repeat(4))),
            chars: 7,
        };
        let lost = DeliveryTick {
            kind: DeliveryKind::HorizonTick,
            outcome: DeliveryOutcome::Undelivered,
            receipt_id: None,
            chars: 3,
        };
        assert!(accepted.delivered());
        assert!(!queued.delivered());
        assert!(!lost.delivered());
        for (tick, ts) in [(&accepted, 100u64), (&queued, 200), (&lost, 300)] {
            let ev = tick.to_event(ts);
            assert_eq!(ev.kind, "delivery");
            assert_eq!(ev.ts_ms, ts);
            let parsed = parse_delivery(&ev).expect("round trip");
            assert_eq!(parsed.kind, tick.kind);
            assert_eq!(parsed.outcome, tick.outcome);
            assert_eq!(parsed.receipt_id, tick.receipt_id);
            assert_eq!(parsed.chars, tick.chars);
            assert_eq!(
                ev.verdict.as_deref(),
                Some(if tick.delivered() {
                    "delivered"
                } else {
                    "undelivered"
                })
            );
        }
        // Doctored: a queued row with no receipt id, and a verdict that lies about delivery.
        let mut no_receipt = queued.to_event(201);
        no_receipt.object_id = None;
        assert!(parse_delivery(&no_receipt).is_none());
        let mut short_id = queued.to_event(202);
        short_id.object_id = Some("notice:0011223344556677".into());
        assert!(parse_delivery(&short_id).is_none());
        let mut liar = lost.to_event(301);
        liar.verdict = Some("delivered".into());
        assert!(parse_delivery(&liar).is_none());
        let mut wrong_version = accepted.to_event(101);
        wrong_version.evaluator_id = Some("delivery-ledger-v0".into());
        assert!(parse_delivery(&wrong_version).is_none());
        let events = vec![
            accepted.to_event(100),
            accepted.to_event(110),
            queued.to_event(200),
            lost.to_event(300),
            liar,
            no_receipt,
        ];
        let ledger = delivery_ledger(&events, 1_000, 10_000);
        assert_eq!(ledger.malformed, 2);
        assert_eq!(ledger.rows.len(), 3);
        let row = |k: &str| ledger.rows.iter().find(|r| r.kind == k).unwrap().clone();
        assert_eq!(row("verdict").telegram_accepted, 2);
        assert_eq!(row("verdict").chars, 84);
        assert_eq!(row("pattern").console_queued, 1);
        assert_eq!(row("horizon-tick").undelivered, 1);
        let text = render_delivery_ledger_at(&events, 1_000);
        assert!(text.contains("DELIVERY LEDGER delivery-ledger-v1"));
        assert!(text.contains("Only telegram-accepted is delivered"));
        // Outside the window: nothing.
        assert!(render_delivery_ledger_at(&events, 100 * 3_600_000).starts_with("No deliveries"));
        // The new budget lines and the queued outcome render and parse as bounded text.
        for b in [
            BudgetKind::ResolveGrade,
            BudgetKind::ProfileLearnOneCall,
            BudgetKind::PatternsOneCall,
        ] {
            let line = LoopPolicy::Budget(b).render();
            assert_eq!(LoopPolicy::parse(&line), Some(LoopPolicy::Budget(b)));
        }
        assert_eq!(
            LoopOutcome::parse("found-queued"),
            Some(LoopOutcome::FoundQueued)
        );
    }
}
