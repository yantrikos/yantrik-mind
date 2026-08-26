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
//!    [`redact`]: secret-shaped content is replaced by `[redacted-secret]` (the same detector
//!    that guards memory writes), and fields are truncated to a stated budget. IDs over raw
//!    text wherever an ID suffices.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
    /// Whose work this serves (the purpose gate's beneficiary label when known).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The declared purpose label (e.g. `conversation→member:primary`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
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
    /// Confidence attached to that prediction, [0,1].
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
    /// What changed because of this outcome (belief revised, skill quarantined, policy adjusted…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lesson: Option<String>,
    // ── SPAN LINKAGE ────────────────────────────────────────────────────────────
    // A trace label groups events; these three make them a CAUSAL TREE. event_id names this
    // span (so others can parent to it); parent_event_id points at the decision that caused
    // this one; object_id names the durable thing the event is about (a packet id, a task id,
    // a prediction ref) so `ym why pkt:…` reconstructs the life of an OBJECT across traces.
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
            subject: None,
            purpose: None,
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
            lesson: None,
            event_id: None,
            parent_event_id: None,
            object_id: None,
        }
    }

    /// A named span under `trace`, parented to `parent_event_id` when the caller knows it —
    /// how a turn becomes a tree (interpretation → plan → packet → tool-call → learning)
    /// rather than a flat list sharing a label.
    pub fn span(trace_id: impl Into<String>, parent: Option<&str>, kind: &str) -> Self {
        let mut e = Self::new(trace_id, kind);
        e.event_id = Some(format!("{kind}-{}", now_ms()));
        e.parent_event_id = parent.map(String::from);
        e
    }

    /// Apply the redaction budget to every free-text field. Called by the log on append, so
    /// callers may pass human text without leaking responsibility for scanning it themselves.
    fn sanitized(mut self) -> Self {
        let b = |s: &str| brief(s, 160);
        self.kind = brief(&self.kind, 48);
        self.actor = self.actor.map(|x| brief(&x, 32));
        self.subject = self.subject.map(|x| brief(&x, 64));
        self.purpose = self.purpose.map(|x| brief(&x, 64));
        self.goal = self.goal.map(|x| b(&x));
        self.trigger = self.trigger.map(|x| b(&x));
        self.chosen = self.chosen.map(|x| brief(&x, 120));
        self.predicted = self.predicted.map(|x| b(&x));
        self.outcome = self.outcome.map(|x| b(&x));
        self.verdict = self.verdict.map(|x| brief(&x, 24));
        self.lesson = self.lesson.map(|x| b(&x));
        for v in [&mut self.candidates, &mut self.rejected, &mut self.policy] {
            for item in v.iter_mut() {
                *item = brief(item, 120);
            }
        }
        self.evidence_ids.retain(|id| !id.trim().is_empty());
        self
    }
}

/// Redact + truncate one free-text field. Secret-shaped content never enters the ledger even
/// truncated — the detector is the SAME function guarding memory writes (one source of truth).
fn brief(text: &str, max_chars: usize) -> String {
    let mut s = if mind_types::contains_secret(text) { "[redacted-secret]".to_string() } else { text.trim().to_string() };
    if s.chars().count() > max_chars {
        s = s.chars().take(max_chars).collect::<String>() + "…";
    }
    s
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
        Self { consecutive_failures: 0, last_failure_ms: None }
    }

    /// Should we currently stay silent (inside a backoff window after failures)?
    fn in_backoff(&self, now_ms: u64) -> bool {
        let Some(last) = self.last_failure_ms else { return false };
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
    head: Mutex<Option<String>>,
    health: Mutex<RecorderHealth>,
}

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
        Self { path: Mutex::new(Some(path.into())), head: Mutex::new(None), health: Mutex::new(RecorderHealth::new()) }
    }

    /// A log that records nothing — the default for eval harnesses and scratch minds, so call
    /// sites can log unconditionally and stay branch-free.
    pub fn disabled() -> Self {
        Self { path: Mutex::new(None), head: Mutex::new(None), health: Mutex::new(RecorderHealth::new()) }
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
        if let Err(e) = self.append_inner(&p, event.sanitized()) {
            self.health.lock().unwrap_or_else(|e| e.into_inner()).note_failure(now);
            eprintln!(
                "[flight-recorder] append failed ({e}); retrying after backoff (failure #{})",
                self.health.lock().unwrap_or_else(|e| e.into_inner()).consecutive_failures
            );
            return;
        }
        self.health.lock().unwrap_or_else(|e| e.into_inner()).note_success();
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

    /// Every event under a trace-id prefix, in recorded order — the raw material for `ym why`.
    pub fn read_trace(&self, prefix: &str) -> Vec<DecisionEvent> {
        let Some(p) = self.trace_path() else { return vec![] };
        if prefix.trim().is_empty() {
            // No prefix = the most recent events, so `ym why` with no argument shows "the last
            // few decisions" instead of nothing.
            let all = read_events(&p);
            let start = all.len().saturating_sub(10);
            return all[start..].to_vec();
        }
        events_by_trace(&p, prefix)
    }

    fn append_inner(&self, path: &Path, event: DecisionEvent) -> std::io::Result<()> {
        let mut head = self.head.lock().unwrap_or_else(|e| e.into_inner());
        let prev = match head.clone() {
            Some(h) => h,
            None => chain_head(path).unwrap_or_else(|| "genesis".to_string()),
        };
        let event_json = serde_json::to_string(&event).map_err(std::io::Error::other)?;
        let mut hasher = Sha256::new();
        hasher.update(prev.as_bytes());
        hasher.update(event_json.as_bytes());
        let chain = format!("{:x}", hasher.finalize());
        let line = format!("{{\"chain\":\"{chain}\",\"event\":{event_json}}}\n");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        f.write_all(line.as_bytes())?;
        f.sync_all()?;
        *head = Some(chain);
        Ok(())
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

/// All events, in file order (chain NOT verified here — pair with [`verify_log`] when it matters).
pub fn read_events(path: &Path) -> Vec<DecisionEvent> {
    let Ok(content) = std::fs::read_to_string(path) else { return vec![] };
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
    read_events(path).into_iter().filter(|e| e.trace_id.starts_with(trace_prefix)).collect()
}

// ── calibration by confidence band ───────────────────────────────────────────

/// Render the calibration table from an event stream, pairing predictions with their outcomes
/// through span linkage (`tool_predicted` → child `tool_observed`). This is the raw material
/// for statements of the form "when Yantrik predicts 80–90% tool success, success actually
/// occurs 84% of the time" — measured from persisted pairs, never narrated. Bands drifting
/// below their predicted value are overconfidence; above it, underconfidence (which wastes
/// good tools and is real too).
pub fn render_calibration(events: &[DecisionEvent]) -> String {
    // Join predictions to outcomes through parent_event_id → predicted.event_id.
    let pred_by_event: std::collections::HashMap<&str, &DecisionEvent> = events
        .iter()
        .filter(|e| e.kind == "tool_predicted")
        .filter_map(|e| e.event_id.as_deref().map(|id| (id, e)))
        .collect();
    let mut rows: Vec<(f64, f64)> = Vec::new(); // (predicted_confidence, observed 0/1)
    for o in events.iter().filter(|e| e.kind == "tool_observed") {
        let Some(vd) = &o.verdict else { continue };
        let observed = match vd.as_str() {
            "ok" | "empty" => 1.0,
            "failed" => 0.0,
            _ => continue, // unavailable/denied grade nothing here
        };
        if let Some(parent) = &o.parent_event_id {
            if let Some(p) = pred_by_event.get(parent.as_str()) {
                if let Some(c) = p.confidence {
                    rows.push((c, observed));
                }
            }
        }
    }
    if rows.is_empty() {
        return "No graded tool predictions yet — calibration appears once the loop predicts and observes.".into();
    }
    let mut bands: [(Vec<f64>, Vec<f64>); 10] = Default::default();
    for (c, o) in &rows {
        let b = ((c * 10.0).floor() as usize).clamp(0, 9);
        bands[b].0.push(*c);
        bands[b].1.push(*o);
    }
    let mut out = String::from("CALIBRATION BY CONFIDENCE BAND (predicted vs actually-observed):\n");
    for (b, (confs, outs)) in bands.iter().enumerate() {
        if confs.is_empty() {
            continue;
        }
        let mean_c = confs.iter().sum::<f64>() / confs.len() as f64;
        let rate = outs.iter().sum::<f64>() / outs.len() as f64;
        let brier = outs.iter().zip(confs).map(|(o, c)| (*c - *o).powi(2)).sum::<f64>() / outs.len() as f64;
        out.push_str(&format!(
            "  {:.0}-{:.0}%: n={:>2} · predicted {:.2} · observed {:.2} · brier {:.3}{}\n",
            b * 10,
            b * 10 + 10,
            outs.len(),
            mean_c,
            rate,
            brier,
            if (rate - mean_c).abs() <= 0.15 { "" } else if rate < mean_c { "  ← OVERCONFIDENT" } else { "  ← underconfident" }
        ));
    }
    out
}

/// GOAL CONTRIBUTION report: aggregate `tool_goal_graded` events per tool across all runs.
/// This is where "search_web executes 94% of the time" grows into "…and materially advanced
/// its goal in K of N graded runs" — the third success kind, measured from persisted verdicts.
pub fn render_goal_contribution(events: &[DecisionEvent]) -> String {
    let mut rows: std::collections::BTreeMap<String, (usize, usize)> = Default::default(); // tool -> (contributed, graded)
    for e in events.iter().filter(|e| e.kind == "tool_goal_graded") {
        let tool = e.object_id.as_deref().unwrap_or("?").trim_start_matches("tool:");
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
            out.push_str(&format!("  {tool}: {contributed}/{graded} ({:.0}%)\n", 100.0 * *contributed as f64 / *graded as f64));
        }
    }
    if !any {
        // Show raw counts until samples exist — never hide that the number is too young to trust.
        for (tool, (contributed, graded)) in &rows {
            out.push_str(&format!("  {tool}: {contributed}/{graded} (too few runs to rank)\n"));
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
pub fn pack_evidence_counts(events: &[DecisionEvent]) -> std::collections::BTreeMap<String, PackCounts> {
    let mut rows: std::collections::BTreeMap<String, PackCounts> = Default::default();
    for e in events {
        let Some(pack) = e.object_id.as_deref().and_then(|o| o.strip_prefix("pack:")) else { continue };
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
        let p_used = if c.used > 0 { Some(c.graded_used as f64 / c.used as f64) } else { None };
        let p_unused = if c.unused > 0 { Some(c.graded_unused as f64 / c.unused as f64) } else { None };
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
    let routes: Vec<&DecisionEvent> = events.iter().filter(|e| e.kind == "pack_route_shadow").collect();
    if routes.is_empty() {
        return "No shadow routes recorded yet — one is written per turn, every lane, even with no packs (abstain:no_packs) and when the router fails (abstain:router_error).".into();
    }
    let mut by_verdict: std::collections::BTreeMap<String, usize> = Default::default();
    for r in &routes {
        *by_verdict.entry(r.verdict.clone().unwrap_or_else(|| "?".into())).or_insert(0) += 1;
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
    let (mut agree, mut lease_nothing_surfaced, mut abstain_something_surfaced, mut disagree) = (0usize, 0usize, 0usize, 0usize);
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
    let policy = routes.last().and_then(|r| r.policy.first().cloned()).unwrap_or_else(|| "?".into());
    let members = routes.iter().filter(|r| r.actor.as_deref() == Some("member")).count();
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
    let flips: Vec<&DecisionEvent> = events.iter().filter(|e| e.kind == "selection_flipped").collect();
    if flips.is_empty() {
        return "No policy disagreements recorded yet — flips appear when measured history first overrules semantics.".into();
    }
    let mut pairs: std::collections::BTreeMap<String, usize> = Default::default();
    let mut bands: [(usize, usize); 10] = Default::default(); // (count, with strong evidence n>=10)
    for f in &flips {
        let legacy = f
            .rejected
            .first()
            .map(|r| r.split_whitespace().next().unwrap_or("?").to_string())
            .unwrap_or_else(|| "?".into());
        let selected = f.chosen.as_deref().unwrap_or("?");
        *pairs.entry(format!("{legacy} → {selected}")).or_insert(0) += 1;
        let n_strong = f
            .policy
            .iter()
            .find_map(|p| p.strip_prefix("empirical prior n=").and_then(|n| n.split(' ').next()).and_then(|n| n.parse::<u64>().ok()))
            .unwrap_or(0);
        let b = ((f.confidence.unwrap_or(0.5) * 10.0).floor() as usize).clamp(0, 9);
        bands[b].0 += 1;
        if n_strong >= 10 {
            bands[b].1 += 1;
        }
    }
    let mut out = format!("POLICY DISAGREEMENTS ({}) — learned ranking vs legacy semantic-only:\n", flips.len());
    for (pair, n) in &pairs {
        out.push_str(&format!("  {pair}: {n}×\n"));
    }
    out.push_str("  by chosen-prior band (high-evidence flips are the trustworthy subset):\n");
    for (b, (total, strong)) in bands.iter().enumerate() {
        if *total > 0 {
            out.push_str(&format!("    {:.0}-{:.0}%: {total} flips · {strong} backed by n≥10\n", b * 10, b * 10 + 10));
        }
    }
    out.push_str("  outcome join pending: grade these traces when their goals complete to compute Y vs X.\n");
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
        out.push_str(&format!("[{}] {} · trace {} · actor {}", i + 1, e.kind, e.trace_id, e.actor.as_deref().unwrap_or("?")));
        if let Some(s) = &e.subject { out.push_str(&format!(" · subject {s}")); }
        if let Some(p) = &e.purpose { out.push_str(&format!(" · purpose {p}")); }
        let field = |out: &mut String, label: &str, v: &Option<String>| {
            if let Some(x) = v { out.push_str(&format!("\n    {label}: {x}")); }
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
                e.confidence.map(|c| format!("{c:.2}")).unwrap_or_else(|| "?".into())
            ));
        }
        field(&mut out, "outcome", &e.outcome);
        field(&mut out, "verdict", &e.verdict);
        if let Some(err) = e.prediction_error {
            out.push_str(&format!("\n    prediction error: {err:+.3}"));
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
        assert_eq!((c.surfaced, c.used, c.unused, c.graded_used, c.graded_unused, c.good), (10, 6, 4, 6, 0, 4));
        let r = render_pack_evidence(&ev);
        assert!(r.contains("used 6 of 10 surfaced"), "{r}");
        assert!(r.contains("graded 6 of 10 surfaced (6 after use, 0 after non-use)"), "{r}");
        assert!(r.contains("accepted 4 of 6 graded"), "{r}");
        assert!(r.contains("censored 4 of 10 surfaced never graded"), "{r}");
        assert!(r.contains("too few rows on one side"), "unused=4 cannot support the audit yet: {r}");
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
        ev.push(route("t2", Some("a"), "lease")); // nothing surfaced
        ev.push(route("t3", None, "abstain:below_floor"));
        ev.push(surfaced("t3", "b")); // abstained while something surfaced
        ev.push(route("t4", Some("a"), "lease"));
        ev.push(surfaced("t4", "b")); // different pack
        ev.push(route("t5", None, "abstain:tie")); // nothing surfaced either → agree
        let r = render_pack_routes(&ev);
        assert!(r.contains("5 turn(s), 0 of them member lane"), "{r}");
        assert!(r.contains("lease: 3") && r.contains("abstain:below_floor: 1") && r.contains("abstain:tie: 1"), "{r}");
        assert!(r.contains("agree 2 · would-lease but nothing surfaced 1 · abstained while something surfaced 1 · different pack 1"), "{r}");
        assert!(render_pack_routes(&[]).contains("No shadow routes"));
    }

    fn scratch(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("ym_flight_{tag}_{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn ev(trace: &str, kind: &str, chosen: &str) -> DecisionEvent {
        let mut e = DecisionEvent::new(trace, kind);
        e.actor = Some("cognition".into());
        e.chosen = Some(chosen.into());
        e
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
        assert!(verify_log(&path).is_err(), "an edited event must not verify");
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
        let without_line2: String =
            content.lines().enumerate().filter(|(i, _)| *i != 1).map(|(_, l)| format!("{l}\n")).collect();
        std::fs::write(&path, without_line2).unwrap();
        assert!(matches!(verify_log(&path), Err(_)), "removing a middle line must break the chain");
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
        start.policy = vec!["harm-gate:allow".into(), "purpose:allow(suppressed=0)".into()];
        start.predicted = Some("owner confirms within a day".into());
        start.confidence = Some(0.62);
        log.record(start);
        log.record(ev("unrelated", "cognitive_run", "x"));
        let mut done = ev("2026-trace-alpha", "packet_resolved", "confirmed by owner");
        done.outcome = Some("owner accepted the packet".into());
        done.verdict = Some("engaged".into());
        done.prediction_error = None;
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
            "lesson:",
        ] {
            assert!(rendered.contains(needle), "rendered trace must contain '{needle}':\n{rendered}");
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
        assert!(s.goal.as_ref().unwrap().chars().count() <= 161, "goal truncated to budget + ellipsis");
        assert!(s.goal.as_ref().unwrap().ends_with('…'));
        assert!(s.outcome.as_ref().unwrap().chars().count() <= 161);
    }

    #[test]
    fn secrets_never_enter_the_ledger() {
        // The same detector that guards memory writes guards the recorder; a field that carries
        // a secret-shaped string is replaced wholesale — no partial content survives truncation.
        let mut e = ev("t", "k", "x");
        e.goal = Some("deploy with token ghp_XXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX and continue".into());
        e.trigger = Some("AKIAIOSFODNN7EXAMPLE rotation due".into());
        let s = e.sanitized();
        assert_eq!(s.goal.as_deref(), Some("[redacted-secret]"), "{:?}", s.goal);
        assert_eq!(s.trigger.as_deref(), Some("[redacted-secret]"));
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
        assert_eq!(render_trace(&events_by_trace(&base, "t")), "no recorded events under this trace");
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
        assert_eq!(trace[1].parent_event_id.as_deref(), trace[0].event_id.as_deref(), "plan parents to interpretation");
        assert_eq!(trace[2].parent_event_id.as_deref(), trace[1].event_id.as_deref(), "packet parents to plan");
        assert_eq!(trace[2].object_id.as_deref(), Some("pkt:abc"));
        let rendered = render_trace(&trace);
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
        let mut p = std::env::temp_dir();
        p.push(format!("ym_float_rt_{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
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
        assert_eq!(verify_log(&p), Ok(2), "1-ulp drift on reserialization breaks the chain");
        let events = read_events(&p);
        assert_eq!(events[0].confidence, Some(2.0f64 / 3.0));
        assert_eq!(events[1].brier, Some((2.0f64 / 3.0 - 1.0).powi(2)), "bit-exact through disk");
        let _ = std::fs::remove_file(&p);
    }
}

