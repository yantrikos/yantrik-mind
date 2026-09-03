//! Proactive drive -- vigilance scan, DMN tick, digest/ask, deadline follow-ups. Extracted from lib.rs.

use super::*;

// E.G2a's tests live ABOVE the proactive surface on purpose: E.G1's source guard scans every
// byte after the shadow consult for the tokens a decision must never touch, and a test that
// filters events by kind would otherwise trip it.
/// E.G2a: every knock evaluation ends in exactly one `knock_disposition` event whose parent is
/// the paired world-shadow row. That an evaluation which never began (YM_KNOCK=off) leaves
/// neither row is pinned by the source guard below, not by a runtime test: `YM_KNOCK` is
/// process-wide, and a test that flips it races every other test that evaluates a knock.
#[cfg(test)]
mod knock_disposition_tests {
    use super::*;
    use mind_inference::{InferencePool, ScriptedLLM};
    use mind_memory::MemoryHandle;
    use std::sync::Arc;
    use yantrik_ml::LLMBackend;

    const SRC: &str = include_str!("proactive.rs");

    fn engine(tag: &str) -> (ConversationEngine, std::path::PathBuf) {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let conv = ConversationEngine::new(
            Arc::new(mem) as Arc<dyn MemoryFacade>,
            InferencePool::new(Arc::new(ScriptedLLM::new("x")) as Arc<dyn LLMBackend>, 1),
            "JARVIS",
        );
        let dir = std::env::temp_dir().join(format!("ym-eg2a-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = Arc::new(mind_observability::DecisionLog::open(dir.join("d.jsonl")));
        (conv.with_recorder(log), dir)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_evaluation_with_no_packets_leaves_one_shadow_and_one_joined_disposition() {
        let (conv, dir) = engine("nopk");
        assert!(
            conv.prepare_knock().await.is_none(),
            "no packet ⇒ no knock (unchanged)"
        );
        let events = conv
            .recorder()
            .read_tail_verified(20)
            .expect("chain verifies");
        let shadows: Vec<_> = events.iter().filter(|e| e.kind == "world_shadow").collect();
        let disp: Vec<_> = events
            .iter()
            .filter(|e| e.kind == "knock_disposition")
            .collect();
        assert_eq!(shadows.len(), 1, "one paired shadow row");
        assert_eq!(disp.len(), 1, "exactly one disposition per evaluation");
        assert_eq!(
            disp[0].parent_event_id.as_deref(),
            Some(shadows[0].trace_id.as_str()),
            "the disposition's parent is the shadow row — the offline join key"
        );
        assert_eq!(disp[0].chosen.as_deref(), Some("no_packets"));
        assert_eq!(disp[0].verdict.as_deref(), Some("before-gate"));
        assert_eq!(
            disp[0].semantic_success, None,
            "receptive is null before the gate"
        );
        assert_eq!(disp[0].object_id, None, "no packet ref unless sent");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Source guards: every exit of `maybe_knock` after the shadow row is preceded by a disposition;
    /// the set of dispositions is exactly the preregistered nine; the judgment ref is untouched.
    #[test]
    fn every_exit_after_the_shadow_carries_a_disposition_and_the_ref_is_byte_identical() {
        // L3c: the evaluation is `prepare_knock`; the sent branch is `commit_knock`; both precede
        // `knock_reply` in this file, so the window covers the split as it covered the whole.
        let start = SRC
            .find(concat!("pub async fn ", "prepare_knock("))
            .unwrap();
        let end = start
            + SRC[start..]
                .find(concat!("pub async fn ", "knock_reply("))
                .unwrap();
        let body = &SRC[start..end];
        let shadow_at = body
            .find("record_world_shadow(now, \"knock-receptivity\")")
            .unwrap();
        let after = &body[shadow_at..];
        let mut idx = 0;
        let mut exits = 0;
        while let Some(i) = after[idx..].find("return None;") {
            let at = idx + i;
            let window = &after[at.saturating_sub(400)..at];
            assert!(
                window.contains("record_knock_disposition("),
                "an exit without a disposition at offset {at}"
            );
            exits += 1;
            idx = at + 12;
        }
        assert_eq!(exits, 3, "no_packets, candidate-none, blocked");
        assert!(
            after.contains(
                "record_knock_disposition_once(&c.eval_id, sent_ms, \"sent\", c.receptive, &sref)"
            ),
            "the sent branch records with the packet ref"
        );
        for d in [
            "no_packets",
            "not_knockworthy",
            "provenance",
            "escrow_held",
            "sent",
        ] {
            assert!(
                after.contains(&format!("\"{d}\"")),
                "disposition {d} is named"
            );
        }
        // muted / daily_cap / unreceptive / below_band come from Silence::as_str with '-' → '_'.
        assert!(after.contains("reason.as_str().replace('-', \"_\")"));
        assert!(
            after.contains("let sref = format!(\"knock:{}\", c.pkt_id);"),
            "the judgment ref is byte-identical — knock_reply rebuilds it"
        );
        // The evaluation begins after the precheck: the off-return precedes the shadow row.
        let off_at = body.find("return None;").unwrap();
        assert!(
            off_at < shadow_at,
            "YM_KNOCK=off exits before any row is written"
        );
    }
}

/// Maximum number of redacted offline-cognition lines retained for operator surfaces.
pub const DMN_LOG_CAPACITY: usize = 200;

/// One display-safe line from the default-mode network. This is deliberately an in-process,
/// best-effort observation rather than another authoritative memory store.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DmnLogEntry {
    pub at_ms: u64,
    pub tick_no: u64,
    pub phase: String,
    pub message: String,
}

#[derive(Default)]
pub(crate) struct DmnLog {
    entries: std::collections::VecDeque<DmnLogEntry>,
}

impl DmnLog {
    fn append_tick(&mut self, at_ms: u64, tick_no: u64, phase: u64, lines: &[String]) {
        let phase = match phase {
            0 => "rehearse",
            1 => "reconcile",
            _ => "associate",
        };
        for line in lines {
            self.entries.push_back(DmnLogEntry {
                at_ms,
                tick_no,
                phase: phase.to_string(),
                message: sanitize_dmn_log_line(line),
            });
            while self.entries.len() > DMN_LOG_CAPACITY {
                self.entries.pop_front();
            }
        }
    }

    fn tail(&self, limit: usize) -> Vec<DmnLogEntry> {
        let take = limit.min(DMN_LOG_CAPACITY).min(self.entries.len());
        self.entries
            .iter()
            .skip(self.entries.len().saturating_sub(take))
            .cloned()
            .map(|mut entry| {
                // Re-sanitize at the display boundary too. This protects the surface if a future
                // producer or a legacy in-process entry predates ingestion-time redaction.
                entry.message = sanitize_dmn_log_line(&entry.message);
                entry
            })
            .collect()
    }
}

fn sanitize_dmn_log_line(line: &str) -> String {
    let clipped: String = line.chars().take(400).collect();
    if mind_types::contains_secret(&clipped) {
        "[dmn] [redacted-secret]".to_string()
    } else {
        crate::redact::redact_stream(&clipped)
    }
}

#[cfg(test)]
mod dmn_log_tests {
    use super::*;

    #[test]
    fn history_is_bounded_and_tail_preserves_chronological_order() {
        let mut history = DmnLog::default();
        for tick in 0..DMN_LOG_CAPACITY + 5 {
            history.append_tick(
                1_000 + tick as u64,
                tick as u64,
                tick as u64 % 3,
                &[format!("[dmn] tick {tick}")],
            );
        }

        let all = history.tail(usize::MAX);
        assert_eq!(all.len(), DMN_LOG_CAPACITY);
        assert_eq!(all.first().map(|entry| entry.tick_no), Some(5));
        assert_eq!(
            all.last().map(|entry| entry.tick_no),
            Some((DMN_LOG_CAPACITY + 4) as u64)
        );
        assert_eq!(history.tail(2).len(), 2);
        assert!(history.tail(0).is_empty());
    }

    #[test]
    fn history_redacts_on_ingest_and_again_on_read() {
        let mut history = DmnLog::default();
        history.append_tick(
            1,
            1,
            0,
            &["[dmn] contacted alice.secret@example.com".to_string()],
        );
        assert!(!history.tail(1)[0].message.contains("alice.secret"));

        // Simulate an old entry written before ingestion-time sanitization existed.
        history.entries.push_back(DmnLogEntry {
            at_ms: 2,
            tick_no: 2,
            phase: "reconcile".to_string(),
            message: "[dmn] used token sk-live-EXAMPLE1234567890".to_string(),
        });
        let read = history.tail(1);
        assert_eq!(read[0].message, "[dmn] [redacted-secret]");
        assert!(!read[0].message.contains("EXAMPLE"));
    }
}

/// L3c: one outstanding proactive send, keyed by the claim's ref.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingSend {
    pub sent_ms: i64,
    pub r#ref: String,
    pub surface: String,
}

impl PendingSend {
    fn legacy(sent_ms: i64) -> Self {
        Self {
            sent_ms,
            r#ref: sent_ms.to_string(),
            surface: "telegram".into(),
        }
    }
    fn from_value(v: &serde_json::Value) -> Option<Self> {
        if let Some(n) = v.as_i64() {
            return Some(Self::legacy(n));
        }
        serde_json::from_value(v.clone()).ok()
    }
}

/// L3c: a knock decided but not yet committed — the facts the commit needs and the render uses.
#[derive(Debug, Clone, PartialEq)]
pub struct KnockCandidate {
    pub pkt_id: String,
    pub band: u8,
    pub p: f64,
    pub eval_id: String,
    pub trigger: String,
    pub title: String,
    /// The receptivity read at decision time; unknown when committed from a marker.
    pub receptive: Option<bool>,
}

impl KnockCandidate {
    /// The spoken line, rendered from the decided band and the packet's own words.
    pub fn render(&self) -> String {
        crate::knock::render(self.band, &self.trigger, &self.title)
    }
    /// L3c: the same line under another surface's band (the console's own calibration).
    pub fn render_with_band(&self, band: u8) -> String {
        crate::knock::render(band, &self.trigger, &self.title)
    }
    /// L3c: the candidate re-banded for the console: the console domain's probability decides
    /// the spoken band, or `None` when that probability is below the lowest speakable band.
    pub fn for_console(&self, console_p: f64) -> Option<KnockCandidate> {
        let band = crate::knock::band_for(console_p)?;
        Some(KnockCandidate {
            band,
            p: console_p,
            ..self.clone()
        })
    }
}

/// L3c: sixteen hex of a line's digest — the digest kind's marker ref.
pub fn text_digest16(text: &str) -> String {
    mind_spec::sha256_hex(text.as_bytes())[..16].to_string()
}

/// L3c-2: the digest's marker ref for ONE opportunity — the text and the window it was due in.
/// Equal text in another window is another displayed line and another claim.
pub fn digest_ref_for(text: &str, window_key: u64) -> String {
    format!(
        "digest:{}",
        &mind_spec::sha256_hex(format!("{text}\n{window_key}").as_bytes())[..16]
    )
}

/// L3c-2: the ask's marker ref for ONE opportunity — the slot (or `open`) and the window.
pub fn ask_ref_for(slot: Option<&str>, window_key: u64) -> String {
    format!(
        "ask:{}-{}",
        slot.unwrap_or("open"),
        &mind_spec::sha256_hex(format!("ask\n{window_key}").as_bytes())[..8]
    )
}

/// The slot an ask ref arms: the ref without the window suffix, or `None` for an open follow-up.
fn ask_slot_of(r#ref: &str) -> Option<String> {
    let body = r#ref.strip_prefix("ask:")?;
    let (slot, _window) = body.rsplit_once('-')?;
    (slot != "open").then(|| slot.to_string())
}

/// The mind's local calendar day of an instant (the knock's daily cap is a local-day cap).
fn local_day_of(ms: i64) -> String {
    let utc =
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).unwrap_or_else(chrono::Utc::now);
    if let Ok(name) = std::env::var("YM_TZ") {
        if let Ok(tz) = name.trim().parse::<chrono_tz::Tz>() {
            return utc.with_timezone(&tz).format("%Y-%m-%d").to_string();
        }
    }
    let off = std::env::var("YM_TZ_OFFSET_MINUTES")
        .ok()
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    let fo = chrono::FixedOffset::east_opt(off * 60)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).unwrap());
    utc.with_timezone(&fo).format("%Y-%m-%d").to_string()
}

/// L3c: a probability as the marker's integer units (0..=1000).
pub fn p_units(p: f64) -> u16 {
    (p.clamp(0.0, 1.0) * 1000.0).round() as u16
}

/// L3c: a question prepared but not yet armed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskCandidate {
    /// The slot the answer fills; `None` for an open follow-up that arms nothing.
    pub slot: Option<String>,
    pub text: String,
}

impl super::ConversationEngine {
    fn record_dmn_log(&self, phase: u64, tick_no: u64, lines: &[String]) {
        self.dmn_log
            .lock()
            .unwrap()
            .append_tick(Self::now_ms(), tick_no, phase, lines);
    }

    /// Bounded, chronological, display-safe offline-cognition history for read-only operator UIs.
    pub fn dmn_log_tail(&self, limit: usize) -> Vec<DmnLogEntry> {
        self.dmn_log.lock().unwrap().tail(limit)
    }

    /// SELF-VIGILANCE (self-healing) — read the mind's own self-build cron log and, if its most recent
    /// run FAILED, emit an Operational urge so the failure surfaces (via the digest) instead of dying
    /// silently. Observation-only (rung 1–2): it never remediates, just notices + records. Cheap (a
    /// file read), no LLM. Deduped on (kind, about) so the same failure accrues rather than floods.
    pub async fn vigilance_scan(&self) -> Option<String> {
        let path = std::env::var("YM_CRON_LOG")
            .unwrap_or_else(|_| "/var/lib/yantrik-mind/selfbuild-cron.log".to_string());
        let log = std::fs::read_to_string(&path).ok()?;
        let about = Self::vigilance_scan_text(&log)?;
        let _ = self
            .memory
            .record_tension(mind_types::TensionKind::Operational, 0.85, &about)
            .await;
        Some(about)
    }

    /// Pure failure-detector over a self-build log (testable). Looks ONLY at the most recent tick block
    /// and flags it only on an EXPLICIT failure signature — never on a merely-incomplete block (which
    /// could be a run still in progress), so it doesn't false-alarm. Returns a short description, or None.
    pub(crate) fn vigilance_scan_text(log: &str) -> Option<String> {
        let block = log
            .rsplit_once("self-build tick start")
            .map_or(log, |(_, a)| a);
        // Real failures only — NOT "auto-merge BLOCKED" (that's a controlled draft, working as intended).
        // The auth signatures exist because of a real blind spot (2026-07-16): a revoked OAuth token
        // failed the self-improve loop for DAYS — five junk PRs merged with "Failed to authenticate.
        // API Error: 401 …" as the title — and nothing here matched, so the self-healing rung stayed
        // silent and the mind reported itself healthy. The watchdog must know what a lockout looks like.
        const SIGS: &[&str] = &[
            "No such file",
            "ABORT:",
            "MERGE-FAIL",
            "PR-FAIL",
            "could not compile",
            "clone failed",
            "tests failed",
            "timeout: failed to run",
            "Failed to authenticate",
            "API Error: 401",
            "API Error: 403",
            "access token has been revoked",
            "Invalid authentication credentials",
            "Invalid API key",
        ];
        let hit = SIGS.iter().find(|s| block.contains(**s))?;
        let line = block
            .lines()
            .find(|l| l.contains(*hit))
            .unwrap_or(hit)
            .trim();
        // STABLE DEDUP KEY. Tensions dedupe on (kind, about), but the log line carries a timestamp,
        // so yesterday's failure and today's identical failure produced DIFFERENT `about` strings and
        // dedup never fired — one fresh 0.85 urge per day, forever (measured: the digest's entire
        // 12-slot window was self-build alarms). Strip the volatile timestamp so a recurring failure
        // ACCRUES on one row, which is what the dedup was designed to do.
        let stable = Self::strip_timestamps_of(&line.chars().take(160).collect::<String>());
        Some(format!("my last self-build run failed — {stable}"))
    }

    /// Remove volatile timestamps so a recurring failure keeps a STABLE identity across days.
    /// Handles ISO-8601 (`2026-07-22T18:17:01Z`), bare dates (`2026-07-22`), and clock times
    /// (`18:17:01`); everything else — the signature and the human-readable reason — is preserved,
    /// so the message stays diagnostic while the dedup key stops changing every run.
    pub(crate) fn strip_timestamps_of(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let b: Vec<char> = s.chars().collect();
        let mut i = 0usize;
        let digits = |from: usize, n: usize| -> bool {
            from + n <= b.len() && (from..from + n).all(|k| b[k].is_ascii_digit())
        };
        while i < b.len() {
            // yyyy-mm-dd (optionally followed by T/space + hh:mm[:ss][Z])
            if digits(i, 4)
                && i + 10 <= b.len()
                && b[i + 4] == '-'
                && digits(i + 5, 2)
                && b[i + 7] == '-'
                && digits(i + 8, 2)
            {
                i += 10;
                if i < b.len()
                    && (b[i] == 'T' || b[i] == ' ')
                    && digits(i + 1, 2)
                    && i + 3 < b.len()
                    && b[i + 3] == ':'
                {
                    i += 1;
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == ':' || b[i] == '.') {
                        i += 1;
                    }
                    if i < b.len() && b[i] == 'Z' {
                        i += 1;
                    }
                }
                continue;
            }
            // bare hh:mm:ss
            if digits(i, 2) && i + 5 <= b.len() && b[i + 2] == ':' && digits(i + 3, 2) {
                i += 5;
                if i + 3 <= b.len() && b[i] == ':' && digits(i + 1, 2) {
                    i += 3;
                }
                continue;
            }
            out.push(b[i]);
            i += 1;
        }
        // collapse the whitespace the removals left behind
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// DEFAULT-MODE ("sleep") TICK — offline cognition over the typed substrate, run by the channel
    /// ONLY when the user has been idle a while (so it never competes with a live turn). Where
    /// `consolidate()` FILES new experience into typed memory, this STRENGTHENS and RECOMBINES what's
    /// already stored — the other half of what a sleeping brain does. One bounded phase per call
    /// (≤1 LLM call), rotating rehearse → reconcile → associate. Everything is internal: nothing is
    /// sent to the user; insights are stored as low-certainty hypotheses the moat can surface later.
    /// Returns short log lines (the channel just prints them). Disabled by the caller via YM_DMN=off.
    pub async fn dmn_tick(&self) -> Vec<String> {
        let (phase, tick_no) = {
            let mut p = self.dmn_phase.lock().unwrap();
            let cur = *p % 3;
            let n = *p;
            *p = p.wrapping_add(1);
            (cur, n)
        };
        let mut log = Vec::new();
        // LEDGER HYGIENE: bound the tension table before anything reads it. Measured 2026-07-25 on
        // the live box: 2,602 open urges, 17 discharged EVER, ~90 new/day — the drive was ranking
        // over a swamp. Curiosity ages out in 14 days; contradictions (real epistemic debt) get 90.
        if let Ok(n) = self.memory.expire_stale_tensions(14, 90).await {
            if n > 0 {
                log.push(format!("[dmn] expired {n} stale tension(s)"));
            }
        }
        // TWO-TIER FITNESS: grade self-build changes old enough for reality to have answered. Until
        // this existed a merged PR was never looked at again after CI went green — the loop could not
        // tell whether anything it built ever helped.
        log.extend(self.fitness_grade_due().await);
        // TRADING CLAIMS THAT HAVE COME DUE. Same reason as the line above: a prediction nobody
        // returns to is not a prediction, it is a note. Six hunt claims sat "awaiting their
        // deadline" for five days because grading existed only as a command someone had to
        // remember to type — and the whole argument for recording a view is that it gets scored
        // without anyone deciding to score it.
        {
            let graded = self.grade_due_trades().await;
            // Only log when something actually resolved; the common case is "nothing due", and a
            // tick that narrates its own no-ops buries the lines that matter.
            if graded.contains("->") {
                log.push(format!("[dmn] {}", graded.replace(char::from(10), " ")));
            }
        }
        // THE PAPER DESK. Restrict it to phase 0, the DMN's non-LLM phase: a due hunt makes one
        // inference call, while the other two phases already spend their one call on reconciliation
        // or association. This preserves the tick's global one-call budget instead of making the
        // autonomous feature steal capacity invisibly.
        if phase == 0 {
            if let Some(report) = self.paper_desk_tick().await {
                log.push(format!("[dmn] paper-desk {report}"));
            }
            if let Some(report) = self.day_trader_tick().await {
                log.push(format!("[dmn] day-trader {report}"));
            }
            if let Some(report) = self.crypto_trader_tick().await {
                log.push(format!("[dmn] crypto-trader {report}"));
            }
        }
        // FUTURE-SELF COURIER: expire what aged out, and fire any promise whose trigger a recent
        // observation has satisfied — this is what produces `told`-stamped prepared work for the
        // calibrated knock (which, before the courier existed, had no eligible supply at all).
        log.extend(self.courier_scan().await);
        // SELF-VIGILANCE (self-healing rung 1): every idle tick, cheaply scan the mind's OWN health
        // (its self-build cron log) for failures and, if found, emit an Operational urge — so a broken
        // autonomous build SURFACES via the proactive digest instead of dying silently in a log.
        if let Some(v) = self.vigilance_scan().await {
            log.push(format!("[dmn] vigilance: {v}"));
        }
        match phase {
            // REHEARSE — re-touch the most load-bearing beliefs (recall refreshes recency/access; we do
            // NOT add evidence, which would inflate confidence — rehearsal strengthens, it doesn't vote).
            // VIGILANCE (staleness rung): emit a Staleness tension for any high-confidence belief whose
            // last update is older than YM_STALE_BELIEF_DAYS (default 30). This surfaces long-lived
            // certainties for re-verification via the proactive digest instead of serving them indefinitely.
            0 => {
                let stale_threshold_ms: u64 = std::env::var("YM_STALE_BELIEF_DAYS")
                    .ok()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(30)
                    .saturating_mul(86_400_000u64);
                let now = Self::now_ms();
                let rs = self
                    .memory
                    .recall_typed(
                        mind_types::RecallQuery {
                            text: String::new(),
                            top_k: 8,
                            kind: None,
                        },
                        &mind_types::AccessContext::operator(mind_types::Purpose::serving_primary(
                            mind_types::Activity::Proactive,
                        )),
                    )
                    .await
                    .unwrap_or_default();
                let mut stale = 0u32;
                let mut fragile = 0u32;
                for r in &rs {
                    if r.item.kind != mind_types::MemoryKind::Belief {
                        continue;
                    }
                    if r.item.confidence >= 0.7
                        && now.saturating_sub(r.item.updated_ms) > stale_threshold_ms
                    {
                        let snippet: String = r.item.text.chars().take(60).collect();
                        let _ = self
                            .memory
                            .record_tension(
                                mind_types::TensionKind::Staleness,
                                r.item.confidence.clamp(0.5, 1.0),
                                &format!("\"{snippet}\""),
                            )
                            .await;
                        stale += 1;
                    }
                    // Single-source certainty: high confidence backed by only one piece of
                    // evidence is fragile — surface it for re-verification before it hardens.
                    if r.item.confidence >= 0.8 && r.item.evidence_count == 1 {
                        let snippet: String = r.item.text.chars().take(60).collect();
                        let _ = self
                            .memory
                            .record_tension(
                                mind_types::TensionKind::VerificationDebt,
                                r.item.confidence.clamp(0.5, 1.0),
                                &format!("\"{snippet}\""),
                            )
                            .await;
                        fragile += 1;
                    }
                }
                log.push(if rs.is_empty() {
                    "[dmn] rehearse: nothing stored yet".to_string()
                } else {
                    let mut parts = vec![format!("rehearsed {} memories", rs.len())];
                    if stale > 0 {
                        parts.push(format!("{stale} stale"));
                    }
                    if fragile > 0 {
                        parts.push(format!("{fragile} fragile"));
                    }
                    format!("[dmn] {}", parts.join(", "))
                });
            }
            // RECONCILE — judge ONE open contradiction, apply the verdict as signed evidence on the
            // winning and losing belief nodes so confidence scores actually shift, then bank an
            // observability note and emit a COHERENCE tension. UNRESOLVED leaves scores unchanged.
            1 => {
                let cs = self
                    .memory
                    .conflicts(&mind_types::AccessContext::operator(
                        mind_types::Purpose::serving_primary(mind_types::Activity::Proactive),
                    ))
                    .await
                    .unwrap_or_default();
                // ROTATE through the open set rather than always taking `.first()`. An UNRESOLVED
                // verdict deliberately leaves both scores unchanged, so the same contradiction stays
                // at the head of the list and `.first()` would re-judge it EVERY cycle, forever —
                // burning a model call and re-sending the same private beliefs each time. Walking the
                // set means an unresolvable pair costs one call per full lap, not one per tick.
                let pick = cs.get((tick_no / 3) as usize % cs.len().max(1));
                if let Some(c) = pick {
                    let prompt = format!(
                        "Two of my stored beliefs conflict:\nA: {}\nB: {}\nWhich is better supported by general knowledge, or is this genuinely unresolved? Answer in ONE sentence, starting with A, B, or UNRESOLVED.",
                        c.belief_a, c.belief_b
                    );
                    let messages = vec![
                        ChatMessage::system(&self.persona),
                        ChatMessage::system(
                            "You weigh conflicting beliefs cautiously. One sentence.",
                        ),
                        ChatMessage::user(&prompt),
                    ];
                    // PRIVATE-GROUNDED: this prompt carries two of the household's stored beliefs
                    // VERBATIM (an operator-lane read — purpose-filtered now, but still private), so
                    // it must PREFER the private (owned-hardware) lane and only escalate to cloud with
                    // an audit. It was an unscoped `chat()` = a silent Household (cloud) call on every
                    // reconcile tick — the same leak agent_loop already fixed, missed on this path.
                    if let Ok(r) = self
                        .inference
                        .chat_grounded(messages, GenerationConfig::default())
                        .await
                    {
                        let verdict = r.text.trim();
                        let verdict_upper = verdict.to_uppercase();
                        let (winner, loser, verdict_label) = if verdict_upper.starts_with('A') {
                            (
                                Some(c.belief_a.as_str()),
                                Some(c.belief_b.as_str()),
                                "→ A wins",
                            )
                        } else if verdict_upper.starts_with('B') {
                            (
                                Some(c.belief_b.as_str()),
                                Some(c.belief_a.as_str()),
                                "→ B wins",
                            )
                        } else {
                            (None, None, "→ unresolved")
                        };
                        if let (Some(w), Some(l)) = (winner, loser) {
                            let _ = self
                                .memory
                                .remember_as_belief(BeliefAssertion {
                                    statement: w.to_string(),
                                    polarity: 1.0,
                                    weight: 0.5,
                                    source_event: Some("dmn_reconcile".into()),
                                    provenance: "dmn".into(),
                                })
                                .await;
                            let _ = self
                                .memory
                                .remember_as_belief(BeliefAssertion {
                                    statement: l.to_string(),
                                    polarity: -1.0,
                                    weight: 0.5,
                                    source_event: Some("dmn_reconcile".into()),
                                    provenance: "dmn".into(),
                                })
                                .await;
                        }
                        let note: String = format!(
                            "On the tension '{}' vs '{}': {}",
                            c.belief_a, c.belief_b, verdict
                        )
                        .chars()
                        .take(400)
                        .collect();
                        let _ = self
                            .memory
                            .remember_as_belief(BeliefAssertion {
                                statement: note,
                                polarity: 1.0,
                                weight: 0.3, // low-certainty note for observability
                                source_event: Some("dmn_reconcile".into()),
                                provenance: "dmn".into(),
                            })
                            .await;
                        // The COHERENCE drive emits an urge — pressure ~ contradiction severity.
                        let _ = self
                            .memory
                            .record_tension(
                                mind_types::TensionKind::Contradiction,
                                c.severity.clamp(0.3, 1.0),
                                &format!("\"{}\" vs \"{}\"", c.belief_a, c.belief_b),
                            )
                            .await;
                        log.push(format!("[dmn] reconciled 1 contradiction ({verdict_label}; evidence applied + urge recorded)"));
                    }
                } else {
                    log.push("[dmn] reconcile: no open contradictions".to_string());
                }
            }
            // ASSOCIATE — free-associate over stored beliefs for ONE non-obvious insight/question, and
            // store it as a low-certainty HYPOTHESIS (provenance=dmn) the mind can later test or surface.
            _ => {
                let rs = self
                    .memory
                    .recall_typed(
                        mind_types::RecallQuery {
                            text: String::new(),
                            top_k: 10,
                            kind: None,
                        },
                        &mind_types::AccessContext::operator(mind_types::Purpose::serving_primary(
                            mind_types::Activity::Proactive,
                        )),
                    )
                    .await
                    .unwrap_or_default();
                if rs.len() < 3 {
                    log.push("[dmn] associate: too little stored to connect".to_string());
                    self.record_dmn_log(phase, tick_no, &log);
                    return log;
                }
                let facts = rs
                    .iter()
                    .map(|r| format!("- {}", r.item.text))
                    .collect::<Vec<_>>()
                    .join("\n");
                let prompt = format!(
                    "Here is some of what I know:\n{facts}\n\nName ONE non-obvious connection, pattern, or question that emerges across these — something worth following up. Reply with a single sentence."
                );
                let messages = vec![
                    ChatMessage::system(&self.persona),
                    ChatMessage::system("You free-associate to surface one genuinely useful insight or question. One sentence, no preamble."),
                    ChatMessage::user(&prompt),
                ];
                // PRIVATE-GROUNDED (the widest of the two DMN prompts): this dumps the top-10 recalled
                // facts VERBATIM — arbitrary private household knowledge, read unrestricted — so it
                // takes the private lane first with an audited escalation, never a silent cloud call.
                if let Ok(r) = self
                    .inference
                    .chat_grounded(messages, GenerationConfig::default())
                    .await
                {
                    let insight = r.text.trim();
                    if insight.len() > 8 {
                        let statement: String = format!("(hypothesis) {insight}")
                            .chars()
                            .take(400)
                            .collect();
                        let _ = self
                            .memory
                            .remember_as_belief(BeliefAssertion {
                                statement,
                                polarity: 1.0,
                                weight: 0.3, // a hunch, not a fact
                                source_event: Some("dmn_associate".into()),
                                provenance: "dmn".into(),
                            })
                            .await;
                        // The CURIOSITY drive emits an urge to follow up the hunch (lower pressure).
                        let _ = self
                            .memory
                            .record_tension(mind_types::TensionKind::Curiosity, 0.4, insight)
                            .await;
                        log.push("[dmn] associated 1 hypothesis (+ curiosity urge)".to_string());
                    }
                }
            }
        }
        self.record_dmn_log(phase, tick_no, &log);
        log
    }

    /// PROACTIVE DIGEST (tension economy, Stage 2) — arbitration + conserved speech. Reads the open
    /// urges the drives accrued while idle and, ONLY if one clears the pressure bar, composes a short
    /// digest of the top few and DISCHARGES them (so they never repeat). Returns None to STAY SILENT —
    /// the default and the common case (null-discipline). This is the one path that messages the user
    /// unprompted; restraint is the whole design — a HIGH bar, ≤3 items, and the caller additionally
    /// gates on idle + quiet-hours + a once-per-period cap. Deterministic phrasing (no extra LLM call):
    /// the urges already carry human-readable `about` text from when the drive formed them.
    pub async fn proactive_digest(&self) -> Option<String> {
        let min_pressure: f64 = std::env::var("YM_PROACTIVE_MIN_PRESSURE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.7);
        let open = self.memory.open_tensions(12).await.unwrap_or_default();
        let winners: Vec<_> = open
            .into_iter()
            .filter(|t| t.pressure >= min_pressure)
            .collect();
        if winners.is_empty() {
            return None; // nothing clears the bar → stay silent (the default)
        }
        // Re-rank by cognitive urgency: base pressure × (1 + engine demand for the topic). Tensions
        // whose subject overlaps with low-confidence beliefs score higher — what the mind most needs
        // to address surfaces first rather than treating all passing tensions as pressure-equivalent.
        let topics: Vec<String> = winners.iter().map(|t| t.about.clone()).collect();
        let demands = self
            .memory
            .knowledge_gaps(&topics)
            .await
            .unwrap_or_else(|_| vec![0.0; topics.len()]);
        let mut scored: Vec<(usize, f64)> = winners
            .iter()
            .enumerate()
            .map(|(i, t)| {
                (
                    i,
                    t.pressure * (1.0 + demands.get(i).copied().unwrap_or(0.0)),
                )
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(3);
        let mut s = String::from("A few things surfaced while you were away:");
        for (idx, _urgency) in &scored {
            let t = &winners[*idx];
            let tag = match t.kind {
                mind_types::TensionKind::Contradiction => "possible contradiction",
                mind_types::TensionKind::Staleness => "may be going stale",
                mind_types::TensionKind::Curiosity => "a thread worth pulling",
                mind_types::TensionKind::VerificationDebt => "worth verifying",
                mind_types::TensionKind::Operational => "⚠ needs your attention",
            };
            s.push_str(&format!("\n• ({tag}) {}", t.about));
            let _ = self.memory.discharge_tension(&t.id).await; // surfaced once; don't repeat
        }
        Some(s)
    }

    /// E.G1/E.G1c: THE WORLD MODEL'S SHADOW. Record what world-state-v1.1 would say about the
    /// recipient's presence at this moment. The verdict goes to the flight recorder and is read by
    /// nothing that decides (source-guarded): shadow ranks, it does not choose (E.PK3's discipline).
    ///
    /// `moment` names the sample: `knock-receptivity` is the PAIRED sample (recorded at the knock's
    /// own decision moment, Telegram path); `headless-cadence` is the UNPAIRED sample the headless
    /// tick records on a fixed cadence, because E.G1c found the paired one lived inside a gate the
    /// canary can never open (no phone channel ⇒ no knock loop ⇒ zero events, ever). The two must
    /// never be pooled: one measures agreement with a decision, the other only that the pipeline
    /// ingestion → gate → verdict is alive.
    pub fn record_world_shadow(&self, now_ms: i64, moment: &str) -> String {
        let id = format!("world-shadow-{now_ms}");
        let mut shadow = mind_observability::DecisionEvent::new(&id, "world_shadow");
        shadow.actor = Some("proactive".into());
        shadow.lane = Some("primary".into());
        shadow.goal_id = Some(format!("worldshadow:{moment}"));
        shadow.context_fingerprint = Some(mind_observability::opaque_id("context", moment));
        shadow.chosen = Some("shadow-only".into());
        shadow.outcome = Some(self.world_shadow_presence(now_ms));
        shadow.verdict = Some("shadowed".into());
        shadow.evaluator_id = Some("world-state-v1.1".into());
        self.recorder.record(shadow);
        id
    }

    /// E.G2a: the knock evaluation's TERMINAL event — one per evaluation, whichever exit it took.
    /// `parent_event_id` is the paired shadow row's id (the offline join key E.G2's table needs);
    /// `receptive` is the legacy receptivity gate ALONE (`None` on exits before the gate);
    /// `object_id` is the packet ref `knock:<pkt_id>` on `sent`, so the judgment grade joins too
    /// — the ref itself is untouched, `knock_reply` reconstructs it byte for byte. Read by nothing
    /// on the decision path (source-guarded).
    pub fn record_knock_disposition(
        &self,
        eval_id: &str,
        now_ms: i64,
        disposition: &str,
        receptive: Option<bool>,
        object_id: Option<String>,
    ) {
        let mut ev = mind_observability::DecisionEvent::new(
            &format!("knock-disposition-{now_ms}"),
            "knock_disposition",
        );
        ev.actor = Some("proactive".into());
        ev.lane = Some("primary".into());
        ev.goal_id = Some("knock:evaluation".into());
        ev.parent_event_id = Some(eval_id.to_string());
        ev.context_fingerprint = Some(mind_observability::opaque_id(
            "context",
            "knock-receptivity",
        ));
        ev.chosen = Some(disposition.to_string());
        ev.verdict = Some(
            match receptive {
                Some(true) => "receptive",
                Some(false) => "unreceptive",
                None => "before-gate",
            }
            .into(),
        );
        ev.semantic_success = receptive;
        ev.object_id = object_id;
        ev.evaluator_id = Some("knock-disposition-v1".into());
        self.recorder.record(ev);
    }

    /// L1 (ARCH7): one record per background-loop OPPORTUNITY, acted or held, so the mind's idle
    /// time is in its decision record. The tick is a typed value (`mind_observability::LoopTick`)
    /// built only from the ledger's own enums — no free text can reach the log — and it is
    /// exactly one emission attempt (the recorder is best-effort under backoff). Read by nothing
    /// on any decision path: it is the loop ledger, not a gate.
    pub fn record_loop_tick(&self, tick: mind_observability::LoopTick) {
        let now = chrono::Utc::now().timestamp_millis() as u64;
        self.recorder.record(tick.to_event(now));
    }

    /// L1d: a detached speaker's TERMINAL record, written once by its deterministic identity
    /// (`loop-<opportunity id>:terminal`) through the recorder's once-by-id write, so a retry
    /// after any crash cannot write it twice.
    pub fn record_loop_tick_once(&self, tick: mind_observability::LoopTick) {
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let _ = self.recorder.record_once(tick.to_event(now));
    }

    /// L2-B: the attention shadow's ONE permitted write. A row per poll wake that had at least
    /// one due in-scope opportunity, carrying what the arbiter WOULD have chosen. It decides
    /// nothing: no caller reads it back, and the loops that ran had already run.
    pub fn record_attention_shadow(&self, shadow: &mind_observability::AttentionShadow) {
        let now = chrono::Utc::now().timestamp_millis() as u64;
        self.recorder.record(shadow.to_event(now));
    }

    /// L3b: one record per delivery decision — kind, outcome, receipt id, size; never the text.
    pub fn record_delivery(&self, tick: mind_observability::DeliveryTick) {
        let now = chrono::Utc::now().timestamp_millis() as u64;
        self.recorder.record(tick.to_event(now));
    }

    /// L1d: a persisted cadence stamp read side-effect free (0 when unset) — the opportunity key
    /// of the speakers whose due rule is "period elapsed since <stamp>".
    pub async fn profile_stamp_ms(&self, key: &str) -> u64 {
        self.memory
            .profile_get(key)
            .await
            .ok()
            .flatten()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .map(|n| n.max(0) as u64)
            .unwrap_or(0)
    }

    /// L3b: whether a proactive line was SENT within `within_ms` — the process runner's stand-in
    /// for the poll loop's per-tick `spoke` flag, so a pattern does not pile onto a fresh digest.
    pub async fn spoke_recently(&self, within_ms: i64) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        self.proactive_pending().await.iter().any(|sent| {
            sent.sent_ms >= 0
                && now
                    .checked_sub(sent.sent_ms)
                    .is_some_and(|age| age >= 0 && age <= within_ms)
        })
    }

    /// THE CALIBRATED KNOCK (sol's #1, day-one rung). At most ONE per day, and only when every part
    /// of the contract holds: proof-carrying prepared work exists, its trigger was OBSERVED or TOLD,
    /// the recipient looks receptive, and the predicted engagement clears the bar for a speakable
    /// band. The engagement probability is committed to the judgment ledger BEFORE delivery, so the
    /// spoken confidence is falsifiable — and those graded predictions are what give
    /// `judgment_trend` something to measure. Returns None to stay silent, which is the common case.
    ///
    /// Silence here is not a failure mode; it is the design. See `knock` for the full rationale.
    pub async fn prepare_knock(&self) -> Option<KnockCandidate> {
        if std::env::var("YM_KNOCK")
            .map(|v| v == "off")
            .unwrap_or(false)
        {
            return None;
        }
        let now = chrono::Utc::now().timestamp_millis();
        // E.G1: THE WORLD MODEL'S SHADOW, recorded at this decision moment and never read by
        // anything below this line (source-guarded). Shadow ranks; it does not choose.
        // E.G2a: the evaluation begins here (after the YM_KNOCK=off precheck) and ends with exactly
        // one `knock_disposition` event whose parent is this shadow row.
        let eval_id = self.record_world_shadow(now, "knock-receptivity");
        // ORDER CHANGED for INTERRUPTION ESCROW: find the CANDIDATE first, then evaluate the gates.
        // A silence is only meaningful — and only worth recording — when there was something real to
        // say. Checking gates first would discard the candidate and leave no trace of what was held,
        // which is exactly the unaccountable silence the escrow exists to end.
        // WORK FIRST: find prepared, proof-carrying, still-valid work. No packet ⇒ no knock, which is
        // what separates this from a notification.
        // AUTHORITY is part of the SEARCH, not a check applied after picking one: an inferred packet
        // sitting earlier in the store must not mask a legitimately-told one behind it (a bug the
        // knock tests caught — the mind would go silent because the wrong candidate was chosen
        // first). Both halves of the contract are required of the SAME packet.
        let packets = self.load_packets().await;
        if packets.is_empty() {
            // The feed, not a gate, is empty — historically THE most common killer and the least
            // visible one. Count it: "no packets" and "below-band" need different fixes.
            self.funnel_bump("knock:no-packets").await;
            self.record_knock_disposition(&eval_id, now, "no_packets", None, None);
            return None;
        }
        // Held candidates are SKIPPED, not treated as blockers: one thing the mind is rightly quiet
        // about must never silence an unrelated thing it should speak up on. (Same shape as the
        // authority bug — a single unsuitable candidate at the front of the list masking a good one
        // behind it.) The hold is checked inside the search, so the scan finds the first candidate
        // that is knockworthy, authorized, AND not already held-without-change.
        let mut candidate: Option<&serde_json::Value> = None;
        // Why the search came up empty, per packet class — only reported when NO candidate survives,
        // so a store where one good packet stands behind ten stale ones still reads as healthy.
        let (mut n_unworthy, mut n_provenance, mut n_held) = (0u32, 0u32, 0u32);
        for p in packets.iter() {
            if !crate::knock::packet_is_knockworthy(p, now) {
                n_unworthy += 1;
                continue;
            }
            // Read ONLY the explicit stamp. The old fallback to `reason` was reading a
            // system-written explanation ("festival within 9 days; supplies criterion unmet")
            // as if it were provenance — every packet classified `inferred` by accident, so the
            // knock could never fire. Absent stamp ⇒ not eligible, by decision now, not luck.
            if !crate::knock::trigger_may_interrupt(
                p.get("trigger_provenance")
                    .and_then(|x| x.as_str())
                    .unwrap_or(""),
            ) {
                n_provenance += 1;
                continue;
            }
            if self.escrow_still_held(p).await {
                n_held += 1;
                continue; // rightly quiet about this one; keep looking
            }
            candidate = Some(p);
            break;
        }
        let Some(pkt) = candidate else {
            let reason = if n_unworthy >= n_provenance && n_unworthy >= n_held {
                "knock:not-knockworthy"
            } else if n_provenance >= n_held {
                "knock:provenance"
            } else {
                "knock:escrow-held"
            };
            self.funnel_bump(reason).await;
            let disposition = match reason {
                "knock:not-knockworthy" => "not_knockworthy",
                "knock:provenance" => "provenance",
                _ => "escrow_held",
            };
            self.record_knock_disposition(&eval_id, now, disposition, None, None);
            return None;
        };
        // Raw receptivity, shrunk toward the GRADED engagement record before it drives anything.
        // With a young ledger the hardcoded 0.6 dominates; once knocks have real grades, the issued
        // probability earns its way back toward the world model. One number feeds both the band
        // choice and the ledger — the spoken confidence must be the accountable one.
        let p_raw = self
            .memory
            .proactive_receptivity()
            .await
            .ok()
            .flatten()
            .unwrap_or(0.6);
        let p_engage = self.shrunk_judgment_p("engagement", p_raw).await;
        // GATES, each recording WHY the mind stayed quiet. Silence is legitimate; unexplained
        // silence is not.
        let muted = self
            .memory
            .profile_get("knock_muted")
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some("1");
        let today = local_now().format("%Y-%m-%d").to_string();
        let cap_spent = self
            .memory
            .profile_get("knock_last_date")
            .await
            .ok()
            .flatten()
            .as_deref()
            == Some(today.as_str());
        let unreceptive = !self.proactive_receptivity_ok().await;
        let band_opt = crate::knock::band_for(p_engage);
        let blocked = if muted {
            Some(crate::escrow::Silence::Muted)
        } else if cap_spent {
            Some(crate::escrow::Silence::DailyCap)
        } else if unreceptive {
            Some(crate::escrow::Silence::Unreceptive)
        } else if band_opt.is_none() {
            Some(crate::escrow::Silence::BelowBand)
        } else {
            None
        };
        if let Some(reason) = blocked {
            self.funnel_bump(&format!("knock:{}", reason.as_str()))
                .await;
            self.escrow_hold(pkt, reason, p_engage, now).await;
            self.record_knock_disposition(
                &eval_id,
                now,
                &reason.as_str().replace('-', "_"),
                Some(!unreceptive),
                None,
            );
            return None;
        }
        let band = band_opt?;
        let title = pkt
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("a prepared option")
            .to_string();
        let trigger = pkt
            .get("reason")
            .and_then(|x| x.as_str())
            .unwrap_or("something you asked me to watch")
            .to_string();
        let pkt_id = pkt
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        Some(KnockCandidate {
            pkt_id,
            band,
            p: p_engage,
            eval_id,
            trigger,
            title,
            receptive: Some(!unreceptive),
        })
    }

    /// L3c: the knock's commits — the one claim (`knock:<pkt>`), the day's cap, the pending reply
    /// slot and the `sent` disposition — with the due clock from `sent_ms`. Telegram calls it
    /// only AFTER the API accepted the send; the console calls it at `shown`. A line that was
    /// never displayed earns nothing and arms nothing. Idempotent by the claim ref: a second call
    /// for the same packet writes nothing and returns false.
    pub async fn commit_knock(&self, c: &KnockCandidate, sent_ms: i64, surface: &str) -> bool {
        let _serial = self.engagement_lock.lock().await;
        self.commit_knock_locked(c, sent_ms, surface).await
    }

    /// CONVERGENT: every artifact is inspected and repaired on its own, so a crash after any one
    /// of them is finished by the next call without doubling anything. The row once by ref; the
    /// day's cap set to the SENT instant's local day iff it differs; the reply slot armed iff the
    /// claim is still ungraded (a replied knock is never re-armed) and the value differs; the
    /// `sent` disposition and the funnel stage recorded once, under a durable single-key marker.
    async fn commit_knock_locked(&self, c: &KnockCandidate, sent_ms: i64, surface: &str) -> bool {
        let sref = format!("knock:{}", c.pkt_id);
        let wrote = self
            .commit_engagement_locked(
                "knock",
                &sref,
                surface,
                sent_ms,
                c.p,
                &format!("recipient engages with the {}% knock within 90m", c.band),
            )
            .await;
        let graded = self
            .judgment_row_for_ref(&sref)
            .await
            .is_some_and(|row| row.get("outcome").is_some_and(|o| !o.is_null()));
        let day = local_day_of(sent_ms);
        if self
            .memory
            .profile_get("knock_last_date")
            .await
            .ok()
            .flatten()
            .as_deref()
            != Some(&day)
        {
            let _ = self.memory.profile_set("knock_last_date", &day).await;
        }
        if !graded
            && self
                .memory
                .profile_get("knock_pending")
                .await
                .ok()
                .flatten()
                .as_deref()
                != Some(c.pkt_id.as_str())
        {
            let _ = self.memory.profile_set("knock_pending", &c.pkt_id).await;
        }
        // The funnel stage at most once by a durable per-ref flag on the judgment row — the flag
        // FIRST, then the bump, so a crash between them under-counts once and never doubles.
        if !self.judgment_row_flag(&sref, "funnel_sent").await
            && self.set_judgment_row_flag(&sref, "funnel_sent").await
        {
            // Only a PERSISTED flag earns the bump: a failed durable write bumps nothing, so a
            // retry after a write error cannot double-count either.
            self.funnel_bump("knock:sent").await;
        }
        // The `sent` disposition exactly once: a deterministic event id, deduplicated by the
        // recorder itself (`record_once`), so a retry after any crash cannot write it twice.
        self.record_knock_disposition_once(&c.eval_id, sent_ms, "sent", c.receptive, &sref);
        wrote
    }

    /// Handle a reply to an outstanding knock: deliver the work, defer, or close the class. Grades
    /// the pre-committed prediction either way — a knock that went unwanted must cost the ledger.
    /// Returns None when there is no pending knock or the message isn't one of the three replies,
    /// so ordinary conversation flows through untouched.
    pub async fn knock_reply(&self, msg: &str) -> Option<String> {
        let pending = self
            .memory
            .profile_get("knock_pending")
            .await
            .ok()
            .flatten()?;
        let reply = crate::knock::KnockReply::parse(msg)?;
        let sref = format!("knock:{pending}");
        let _ = self.memory.profile_set("knock_pending", "").await;
        // L3c: the reply is the knock's one grade; the resolver must not grade it again.
        self.retire_pending_ref(&sref).await;
        match reply {
            crate::knock::KnockReply::ShowIt => {
                self.judgment_grade(&sref, true).await; // the interruption was earned
                Some(self.packet_show(&pending).await)
            }
            crate::knock::KnockReply::Later => {
                self.judgment_grade(&sref, false).await; // right thing, wrong moment — still a miss
                Some("Alright — I'll hold it until you ask.".to_string())
            }
            crate::knock::KnockReply::Mute => {
                self.judgment_grade(&sref, false).await;
                let _ = self.memory.profile_set("knock_muted", "1").await;
                Some("Understood — no more of these until you say `knocks on`.".to_string())
            }
        }
    }

    /// ASK DRIVE — curiosity turned OUTWARD, as a progressive interview rather than a fixed list. A
    /// companion shouldn't wait to be fed; when it doesn't know you it ASKS, in order: first your NAME,
    /// then your PURPOSE (what you want from it), then purpose-grounded follow-ups one at a time — and
    /// it goes quiet once it knows enough (never pesters). The caller gates it to ≤1/period + idle +
    /// quiet-hours. Name/purpose answers are captured directly (`handle_turn` → `capture_onboard`);
    /// later answers flow back as ordinary chat → consolidation → typed beliefs.
    pub async fn prepare_ask(&self) -> Option<AskCandidate> {
        // Don't stack a new question while we're still awaiting an answer to the last one.
        if self.pending_slot().await.is_some() {
            return None;
        }
        let name = self.memory.profile_get("name").await.ok().flatten();
        if name.is_none() {
            return Some(AskCandidate {
                slot: Some("name".into()),
                text: "Before we really get going — what should I call you?".to_string(),
            });
        }
        let purpose = self.memory.profile_get("purpose").await.ok().flatten();
        if purpose.is_none() {
            return Some(AskCandidate {
                slot: Some("purpose".into()),
                text: format!(
                    "What would you most like me to help you with, {}? Knowing your main goal lets me be genuinely useful instead of generic.",
                    name.unwrap_or_default()
                ),
            });
        }
        // INTERESTS stage — actively learn the user's world (hobbies, what they follow, the people and
        // companies they care about) so grounding, gifts, and the entity-sim have real material. Asks one
        // uncovered dimension per tick; once all are covered it falls through to the purpose taper.
        let covered = self.ask_covered().await;
        if let Some((key, q)) = INTEREST_DIMS
            .iter()
            .find(|(k, _)| !covered.iter().any(|c| c == k))
        {
            return Some(AskCandidate {
                slot: Some(format!("interest:{key}")),
                text: (*q).to_string(),
            });
        }
        // OPEN stage — purpose-grounded follow-ups, but taper once the brain knows enough about you.
        let enough: usize = std::env::var("YM_ASK_ENOUGH")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8);
        let known = self
            .memory
            .recall_typed(
                mind_types::RecallQuery {
                    text: String::new(),
                    top_k: 64,
                    kind: None,
                },
                &mind_types::AccessContext::operator(mind_types::Purpose::serving_primary(
                    mind_types::Activity::Proactive,
                )),
            )
            .await
            .map(|r| r.len())
            .unwrap_or(0);
        if known >= enough {
            return None;
        }
        self.purpose_followup(&purpose.unwrap_or_default())
            .await
            .map(|text| AskCandidate { slot: None, text })
    }

    /// L3c: arm the question. Only a delivered (Telegram) or SHOWN (console) ask may arm it;
    /// a queued, leased, expired or failed one never swallows an ordinary turn as its answer.
    pub async fn commit_ask(&self, c: &AskCandidate) {
        if let Some(slot) = &c.slot {
            self.set_pending_slot(Some(slot)).await;
        }
    }

    /// The knock for a caller that DISPLAYS SYNCHRONOUSLY (a console verb, a fixture): prepare,
    /// commit, render — the commit and the display are one step. The Telegram poll loop does NOT
    /// use this: it prepares, sends, and commits only after the API accepted the line.
    pub async fn maybe_knock(&self) -> Option<String> {
        let candidate = self.prepare_knock().await?;
        let now = chrono::Utc::now().timestamp_millis();
        self.commit_knock(&candidate, now, "telegram").await;
        Some(candidate.render())
    }

    /// The ask for a caller that displays synchronously: prepare, arm, return the line.
    pub async fn proactive_ask(&self) -> Option<String> {
        let candidate = self.prepare_ask().await?;
        self.commit_ask(&candidate).await;
        Some(candidate.text)
    }

    /// The in-flight get-to-know-you question, PERSISTED in the substrate. This was an in-memory
    /// Mutex — and every restart (which self-deploy now does several times a day) silently dropped
    /// it, so the user's answer arrived with no question pending, got treated as ordinary chat, and
    /// the drive re-asked later ("I already told you!"). The bug class that keeps biting: state
    /// that gates cross-turn behavior must live in the substrate, not the process.
    /// Is a question armed to swallow the next message as its answer?
    pub(crate) async fn has_pending_slot(&self) -> bool {
        self.pending_slot()
            .await
            .is_some_and(|s| !s.trim().is_empty())
    }

    /// Does this line look like a CONTROL COMMAND rather than a human answer? Deliberately narrow:
    /// one token, no spaces, and either a leading `/` or snake_case/kebab-case. A real one-word
    /// answer ("Ritu", "Priya") must still answer normally — a guard that swallows real names would
    /// be worse than the bug it fixes.
    pub(crate) fn is_command_shaped(line: &str) -> bool {
        let t = line.trim();
        if t.is_empty() || t.contains(char::is_whitespace) {
            return false;
        }
        t.starts_with('/')
            || ((t.contains('_') || t.contains('-'))
                && t.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '/'))
    }

    pub(crate) async fn pending_slot(&self) -> Option<String> {
        self.memory
            .profile_get("pending_onboard")
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
    }

    pub(crate) async fn set_pending_slot(&self, v: Option<&str>) {
        let _ = self
            .memory
            .profile_set("pending_onboard", v.unwrap_or(""))
            .await;
    }

    /// Curiosity as NORMAL conversation: occasionally close a reply with one get-to-know-you
    /// question instead of quarantining all asks behind idle gates. Paced (YM_ASK_PIGGYBACK_SECS,
    /// default 4h), skipped while a question is already pending. Most of the "how much do you
    /// actually know about me" gaps close here — in the flow of talk, not in scheduled pings.
    pub(crate) async fn maybe_piggyback_ask(&self) -> Option<String> {
        if std::env::var("YM_ASK_PIGGYBACK")
            .map(|v| v == "off")
            .unwrap_or(false)
        {
            return None;
        }
        if self.pending_slot().await.is_some() {
            return None;
        }
        let period_ms: i64 = std::env::var("YM_ASK_PIGGYBACK_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(14_400)
            * 1000;
        let now = chrono::Utc::now().timestamp_millis();
        let last: i64 = self
            .memory
            .profile_get("ask_piggyback_ms")
            .await
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if now - last < period_ms {
            return None;
        }
        let covered = self.ask_covered().await;
        let (key, q) = INTEREST_DIMS
            .iter()
            .find(|(k, _)| !covered.iter().any(|c| c == k))?;
        self.set_pending_slot(Some(&format!("interest:{key}")))
            .await;
        let _ = self
            .memory
            .profile_set("ask_piggyback_ms", &now.to_string())
            .await;
        Some((*q).to_string())
    }

    /// Which interest dimensions the ask-drive has already covered (persisted, so it never re-asks).
    pub(crate) async fn ask_covered(&self) -> Vec<String> {
        self.memory
            .profile_get("ask_covered")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn mark_ask_covered(&self, key: &str) {
        let mut c = self.ask_covered().await;
        if !c.iter().any(|x| x == key) {
            c.push(key.to_string());
        }
        let _ = self
            .memory
            .profile_set(
                "ask_covered",
                &serde_json::to_string(&c).unwrap_or_else(|_| "[]".into()),
            )
            .await;
    }

    /// Mark that a proactive message just went out — the world model's engagement resolver picks
    /// it up: a user reply within 90 min = ENGAGED; silence past the window = IGNORED (resolved by
    /// the poll loop).
    ///
    /// This used to hold ONE send ("last send wins"), while the ledger below logged EVERY send. So
    /// a second beat going out before the first resolved silently orphaned the first claim, and
    /// nothing could ever grade it: 650 of 932 claims sat permanently pending, some 46 days past a
    /// 90-minute deadline. The loss was not just volume. It was BIASED — an ignored send stays in
    /// the slot for the full 90 minutes and is therefore easy to clobber, while an engaged one
    /// clears on the next user turn, so ignored claims were preferentially destroyed and the
    /// surviving 30% read higher than the truth. A sampling rule that drops the failures is worse
    /// than no measurement, because it looks like a measurement.
    /// Returns the judgment-ledger `ref` this send was logged under, so a caller can join to
    /// the engagement outcome later. Returned rather than recomputed: `judgment_log` stamps its
    /// own `t` after an awaited read, so a timestamp-derived join matches only the rows where
    /// the millisecond happened not to tick over (see ledger E.P2).
    pub async fn note_proactive_sent(&self) -> String {
        let now = chrono::Utc::now().timestamp_millis();
        let p_raw = self
            .memory
            .proactive_receptivity()
            .await
            .ok()
            .flatten()
            .unwrap_or(0.5);
        let p = self.shrunk_judgment_p("engagement", p_raw).await;
        let r#ref = now.to_string();
        self.commit_engagement(
            "proactive",
            &r#ref,
            "telegram",
            now,
            p,
            "recipient engages within 90m",
        )
        .await;
        r#ref
    }

    /// L3c: ONE displayed line earns ONE engagement claim. The claim is keyed by a deterministic
    /// ref (`<sent_ms>` for the legacy Telegram beats, `knock:<pkt>` for a knock, the marker's
    /// ref for a console line) and this is idempotent by that ref: a second commit — a retry, a
    /// reconciler pass after a crash, a duplicate acknowledgement — writes nothing and returns
    /// false. `surface` chooses the calibration domain: `engagement` for Telegram (byte-compatible
    /// with every existing claim) and `engagement-console` for the cockpit, so a surface shift
    /// can never look like learning.
    pub async fn commit_engagement(
        &self,
        source: &str,
        r#ref: &str,
        surface: &str,
        sent_ms: i64,
        p: f64,
        claim: &str,
    ) -> bool {
        let _serial = self.engagement_lock.lock().await;
        self.commit_engagement_locked(source, r#ref, surface, sent_ms, p, claim)
            .await
    }

    /// The commit's body, run under the engagement lock by every caller. CONVERGENT: each
    /// artifact is inspected and repaired on its own — a judgment row exists once by ref (the
    /// ledger is the authority), the pending entry exists iff the claim is still ungraded — so a
    /// retry after a crash between the writes finishes the job and never doubles it. Returns
    /// true iff this call wrote the judgment row.
    async fn commit_engagement_locked(
        &self,
        source: &str,
        r#ref: &str,
        surface: &str,
        sent_ms: i64,
        p: f64,
        claim: &str,
    ) -> bool {
        let existing = self.judgment_row_for_ref(r#ref).await;
        if let Some(row) = existing {
            // The claim exists. Repair the pending entry only if the claim is still ungraded and
            // the entry is missing (a crash between the pending write and the ledger write, or
            // the reverse, both land here).
            let graded = row.get("outcome").is_some_and(|o| !o.is_null());
            if !graded {
                let mut pend = self.proactive_pending().await;
                if !pend.iter().any(|e| e.r#ref == r#ref) {
                    pend.push(PendingSend {
                        sent_ms,
                        r#ref: r#ref.to_string(),
                        surface: surface.to_string(),
                    });
                    self.set_proactive_pending(&pend).await;
                }
            }
            return false;
        }
        let mut pend = self.proactive_pending().await;
        if !pend.iter().any(|e| e.r#ref == r#ref) {
            pend.push(PendingSend {
                sent_ms,
                r#ref: r#ref.to_string(),
                surface: surface.to_string(),
            });
            // Bounded: the resolver retires entries within 90 minutes, so this holds a handful.
            // The cap exists so a resolver outage cannot grow it without limit.
            if pend.len() > 64 {
                let cut = pend.len() - 64;
                pend.drain(..cut);
            }
            self.set_proactive_pending(&pend).await;
        }
        let domain = if surface == "console" {
            "engagement-console"
        } else {
            "engagement"
        };
        // JUDGMENT LEDGER: a proactive send IS a falsifiable prediction — "the recipient engages
        // within the window". p = the learned engagement rate (improvable). Graded on resolve. This
        // is the mandatory-eligibility auto-log (Terra's anti-gaming rule): no opt-in, no post-hoc p.
        self.judgment_log(source, domain, claim, p, sent_ms + 90 * 60_000, r#ref)
            .await;
        true
    }

    /// A durable per-ref flag on the judgment row (a side effect's own idempotence marker).
    async fn judgment_row_flag(&self, r#ref: &str, flag: &str) -> bool {
        self.judgment_row_for_ref(r#ref)
            .await
            .is_some_and(|row| row.get(flag).and_then(|v| v.as_bool()) == Some(true))
    }

    /// Returns whether the flag was durably written (a missing row or a failed write is false).
    async fn set_judgment_row_flag(&self, r#ref: &str, flag: &str) -> bool {
        let mut led: Vec<serde_json::Value> = self
            .memory
            .profile_get("judgment_ledger")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut changed = false;
        for row in led.iter_mut() {
            if row.get("ref").and_then(|x| x.as_str()) == Some(r#ref) {
                row[flag] = serde_json::json!(true);
                changed = true;
            }
        }
        if !changed {
            return false;
        }
        self.memory
            .profile_set(
                "judgment_ledger",
                &serde_json::to_string(&led).unwrap_or_default(),
            )
            .await
            .is_ok()
    }

    /// The knock's `sent` disposition under a deterministic event id (`knock-disposition:<ref>`)
    /// through the recorder's once-by-id write.
    pub fn record_knock_disposition_once(
        &self,
        eval_id: &str,
        now_ms: i64,
        disposition: &str,
        receptive: Option<bool>,
        sref: &str,
    ) {
        let mut ev = mind_observability::DecisionEvent::new(
            &format!("knock-disposition-{now_ms}"),
            "knock_disposition",
        );
        ev.event_id = Some(format!("knock-disposition:{sref}"));
        ev.actor = Some("proactive".into());
        ev.lane = Some("primary".into());
        ev.goal_id = Some("knock:evaluation".into());
        ev.parent_event_id = Some(eval_id.to_string());
        ev.context_fingerprint = Some(mind_observability::opaque_id(
            "context",
            "knock-receptivity",
        ));
        ev.chosen = Some(disposition.to_string());
        ev.verdict = Some(
            match receptive {
                Some(true) => "receptive",
                Some(false) => "unreceptive",
                None => "before-gate",
            }
            .into(),
        );
        ev.semantic_success = receptive;
        ev.object_id = Some(sref.to_string());
        ev.evaluator_id = Some("knock-disposition-v1".into());
        let _ = self.recorder.record_once(ev);
    }

    /// The judgment row logged under `ref`, if any — the once-by-ref authority.
    async fn judgment_row_for_ref(&self, r#ref: &str) -> Option<serde_json::Value> {
        let led: Vec<serde_json::Value> = self
            .memory
            .profile_get("judgment_ledger")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        led.into_iter()
            .find(|row| row.get("ref").and_then(|x| x.as_str()) == Some(r#ref))
    }

    /// L3c: the console surface's probability — the global receptivity estimate shrunk only
    /// against console grades (cold-started from the global estimate when there are none).
    pub async fn console_engagement_p(&self) -> f64 {
        let p_raw = self
            .memory
            .proactive_receptivity()
            .await
            .ok()
            .flatten()
            .unwrap_or(0.5);
        self.shrunk_judgment_p("engagement-console", p_raw).await
    }

    /// L3c: commit the prediction an engaging notice carried, at the instant it was SHOWN, then
    /// mark the outbox item complete. Under the engagement lock as one critical section, so the
    /// web acknowledgement and the runner's reconciler cannot interleave. Convergent: the target
    /// commit is once-by-ref, the completion receipt is idempotent, and a crash between the two
    /// is finished by the next pass without a second row. Returns whether this call wrote the
    /// judgment row.
    pub async fn commit_shown_engagement(
        &self,
        notice_id: &str,
        marker: &mind_spec::EngagementMarker,
        shown_ms: u64,
    ) -> bool {
        let _serial = self.engagement_lock.lock().await;
        let sent_ms = i64::try_from(shown_ms).unwrap_or(i64::MAX);
        let wrote = match marker.kind {
            mind_spec::NoticeKind::Knock => {
                let pkt_id = marker
                    .r#ref
                    .strip_prefix("knock:")
                    .unwrap_or("")
                    .to_string();
                let candidate = KnockCandidate {
                    pkt_id,
                    band: marker.band,
                    p: marker.p(),
                    eval_id: marker.eval_id.clone(),
                    trigger: String::new(),
                    title: String::new(),
                    receptive: None,
                };
                self.commit_knock_locked(&candidate, sent_ms, "console")
                    .await
            }
            mind_spec::NoticeKind::Digest => {
                self.commit_engagement_locked(
                    "proactive",
                    &marker.r#ref,
                    "console",
                    sent_ms,
                    marker.p(),
                    "recipient engages within 90m",
                )
                .await
            }
            mind_spec::NoticeKind::Ask => {
                let wrote = self
                    .commit_engagement_locked(
                        "proactive",
                        &marker.r#ref,
                        "console",
                        sent_ms,
                        marker.p(),
                        "recipient engages within 90m",
                    )
                    .await;
                // The question is armed only now — a queued, leased or expired ask never
                // swallows an ordinary turn as its answer; an open follow-up arms nothing, as
                // its Telegram twin. CONVERGENT: armed iff the claim is still ungraded (an
                // answered ask is never re-armed) and the slot is not already this one.
                let graded = self
                    .judgment_row_for_ref(&marker.r#ref)
                    .await
                    .is_some_and(|row| row.get("outcome").is_some_and(|o| !o.is_null()));
                if !graded {
                    if let Some(slot) = ask_slot_of(&marker.r#ref) {
                        if self.pending_slot().await.as_deref() != Some(slot.as_str()) {
                            self.set_pending_slot(Some(&slot)).await;
                        }
                    }
                }
                wrote
            }
            _ => return false,
        };
        // Durable completion only after the target converged (the row exists by ref).
        if self.judgment_row_for_ref(&marker.r#ref).await.is_some() {
            let _ = self.mark_engagement_committed(notice_id);
        }
        wrote
    }

    /// L3c: finish any shown-but-uncommitted marker (the durable outbox) — each runner beat and
    /// at start. The outbox returns only items without a completion receipt, so a completed
    /// item can never be replayed. Returns how many this pass committed.
    pub async fn reconcile_shown_engagements(&self) -> usize {
        let Ok(shown) = self.shown_engagements() else {
            return 0;
        };
        let mut committed = 0usize;
        for entry in shown {
            if self
                .commit_shown_engagement(&entry.notice_id, &entry.marker, entry.shown_ms)
                .await
            {
                committed += 1;
            }
        }
        committed
    }

    /// L3c: retire one pending ref after an explicit grade (the knock's reply), so the resolver
    /// cannot grade it a second time.
    pub(crate) async fn retire_pending_ref(&self, r#ref: &str) {
        let _serial = self.engagement_lock.lock().await;
        let pend = self.proactive_pending().await;
        if pend.iter().any(|e| e.r#ref == r#ref) {
            let still: Vec<PendingSend> = pend.into_iter().filter(|e| e.r#ref != r#ref).collect();
            self.set_proactive_pending(&still).await;
        }
    }

    /// Resolve the outstanding proactive send, if any. `via_user_turn`: the user just spoke —
    /// engaged iff within the window. Otherwise (tick path) only resolves STALE entries as ignored.
    pub async fn resolve_proactive(&self, via_user_turn: bool) {
        let _serial = self.engagement_lock.lock().await;
        let pend = self.proactive_pending().await;
        if pend.is_empty() {
            return;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let mut still: Vec<PendingSend> = Vec::new();
        for entry in pend {
            // Fail closed on a stamp the arithmetic cannot vouch for: negative, in the future, or
            // so extreme the subtraction would overflow — it stays pending, graded by nobody.
            let age = match now.checked_sub(entry.sent_ms) {
                Some(age) if age >= 0 && entry.sent_ms >= 0 => age,
                _ => {
                    still.push(entry);
                    continue;
                }
            };
            let within = age <= 90 * 60_000;
            // A knock is graded by its explicit reply (show it / later / mute) or, unanswered, by
            // the stale path — never by an ordinary turn, which says nothing about the knock.
            let knock = entry.r#ref.starts_with("knock:");
            // Decide every send that CAN be decided, not just the newest. A user turn answers each
            // outstanding beat whose window still contains it; a window that has run out answers
            // itself. Anything else is genuinely undecided and stays pending.
            let outcome = match (via_user_turn, within) {
                (true, w) if !knock => Some(w),
                (true, true) => None,
                (_, false) => Some(false),
                (false, true) => None,
            };
            match outcome {
                Some(o) => {
                    let _ = self.memory.record_proactive_outcome(entry.sent_ms, o).await;
                    self.judgment_grade(&entry.r#ref, o).await;
                }
                None => still.push(entry),
            }
        }
        self.set_proactive_pending(&still).await;
    }

    /// The outstanding proactive sends. Reads the legacy single-integer form too, so the upgrade
    /// does not drop the one send that happens to be in flight when the new binary starts.
    /// L3c: typed and legacy-compatible. A bare number (the single-send form) or a number in the
    /// array (the L3b-era list) reads as `{sent_ms, ref: "<sent_ms>", surface: "telegram"}`, which
    /// is exactly the claim those sends were logged under; new entries are written typed.
    async fn proactive_pending(&self) -> Vec<PendingSend> {
        let raw = self
            .memory
            .profile_get("proactive_pending")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let raw = raw.trim();
        if raw.is_empty() {
            return Vec::new();
        }
        if let Ok(values) = serde_json::from_str::<Vec<serde_json::Value>>(raw) {
            return values.iter().filter_map(PendingSend::from_value).collect();
        }
        raw.parse::<i64>()
            .map(|n| vec![PendingSend::legacy(n)])
            .unwrap_or_default()
    }

    async fn set_proactive_pending(&self, v: &[PendingSend]) {
        let s = if v.is_empty() {
            String::new()
        } else {
            serde_json::to_string(v).unwrap_or_default()
        };
        let _ = self.memory.profile_set("proactive_pending", &s).await;
    }

    /// SETTLE the engagement claims that were orphaned before the pending list existed.
    ///
    /// While `proactive_pending` held a single send, a second beat going out before the first
    /// resolved destroyed the first claim's only route to a grade. That left 650 of 932 claims
    /// permanently pending — and destroyed them SELECTIVELY, because an ignored send occupies the
    /// slot for its full 90 minutes while an engaged one clears on the next user turn. The third
    /// that survived read 42.9% engagement; the whole record reads 31.3%. The hour-by-hour map that
    /// decides WHEN to speak was distorted worst of all: 07:00 looked like 100% receptive on the
    /// survivors and is 38% on the full record, 19:00 looked like 100% and is 43%.
    ///
    /// A closed window can still be settled honestly, because the transcript records when the
    /// person actually spoke. That is evidence, not a guess: checked against the 280 claims whose
    /// outcome IS recorded, reconstructing from the transcript agrees on 277 of them.
    ///
    /// Only settles claims whose deadline has passed and that fall inside the transcript's span.
    /// Outside it, silence in the record means "not recorded", not "not engaged" — and grading a
    /// missing record as a failure would manufacture exactly the bias this repairs.
    pub async fn backfill_proactive_grades(&self, act: bool) -> String {
        let led: Vec<serde_json::Value> = self
            .memory
            .profile_get("judgment_ledger")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Key on `ref`, NOT on `t`. A claim's identity is the ref its author chose; `t` is stamped
        // separately inside judgment_log, after an awaited read, so the two differ by however many
        // milliseconds that read took. Matching on `t` grades only the rows where the clock happened
        // not to tick in between — 24 of 650 on the live ledger, which looks like a partial write
        // and is actually a wrong join.
        let orphans: Vec<(String, i64, i64)> = led
            .iter()
            .filter(|r| r.get("source").and_then(|x| x.as_str()) == Some("proactive"))
            .filter(|r| r.get("outcome").is_some_and(|o| o.is_null()))
            .filter_map(|r| {
                let rf = r.get("ref")?.as_str()?.to_string();
                // The ref IS the send time for this source; fall back to `t` if it ever is not.
                let sent = rf.parse::<i64>().ok().or_else(|| r.get("t")?.as_i64())?;
                Some((rf, sent, r.get("grade_due")?.as_i64()?))
            })
            .collect();
        if orphans.is_empty() {
            return "no orphaned engagement claims — nothing to settle".to_string();
        }
        let earliest = orphans.iter().map(|(_, t, _)| *t).min().unwrap_or(0);
        let turns = self
            .memory
            .user_turn_times(earliest)
            .await
            .unwrap_or_default();
        let Some(&last_turn) = turns.last() else {
            return format!(
                "{} orphaned claims, but no transcript to settle them against",
                orphans.len()
            );
        };
        let now = chrono::Utc::now().timestamp_millis();
        let windows: Vec<(i64, i64)> = orphans.iter().map(|(_, s, d)| (*s, *d)).collect();
        let (verdicts, skipped) = settle_plan(&windows, &turns, last_turn, now);
        let engaged = verdicts.iter().filter(|(_, e)| *e).count() as u32;
        let ignored = verdicts.len() as u32 - engaged;
        let mut wrote = 0usize;
        if act {
            // ONE ledger write for all of them — it is a single JSON blob, so each grade otherwise
            // rewrites the whole thing.
            let refs: Vec<(String, bool)> = verdicts
                .iter()
                .map(|(i, e)| (orphans[*i].0.clone(), *e))
                .collect();
            wrote = self.judgment_grade_many(&refs).await;
            // The world model takes its own row per transition, binned on send time.
            for (i, e) in &verdicts {
                let _ = self
                    .memory
                    .record_proactive_outcome_backfill(orphans[*i].1, *e)
                    .await;
            }
        }
        let n = engaged + ignored;
        format!(
            "🕰️  {} {} orphaned engagement claim(s) from the transcript{}
  engaged {engaged} · ignored {ignored} → {:.1}% (the surviving third read 42.9%)
  {skipped} left pending — deadline not passed, or outside the transcript's span{}",
            if act { "SETTLED" } else { "would settle" },
            n,
            if act { "" } else { " — pass `act` to write" },
            if n > 0 {
                100.0 * f64::from(engaged) / f64::from(n)
            } else {
                0.0
            },
            // Report what the ledger actually took, not what was decided. A repair that reports its
            // intent instead of its effect is how the first run looked like it had written 650.
            if act {
                format!(
                    "
  ledger accepted {wrote} of {n}"
                )
            } else {
                String::new()
            },
        )
    }

    /// Gate for OPTIONAL proactive beats: false only when the learned world model says this moment
    /// is a dead zone FOR THIS PERSON. True until there's real data — never guess-gate.
    ///
    /// This used to be an absolute `>= 0.35`, which silently depended on the scale of the number
    /// it was comparing. That scale was wrong: engagement was measured on the third of claims the
    /// old single-slot resolver happened not to overwrite, and read 43% when the true rate is 31%.
    /// Settling the other 628 fixed the measurement — and would have muted the mind as a side
    /// effect, because four of the five time bins sit between 23% and 31% and all of them fall
    /// below a threshold that was tuned against the inflated numbers. A data-quality repair must
    /// not covertly change how talkative the thing is.
    ///
    /// So the question is asked relatively: is this moment materially worse than how this person
    /// responds in general? That is what "dead zone" always meant, and unlike a constant it
    /// survives the scale moving again. The floor keeps a pathologically low baseline from
    /// declaring every moment acceptable.
    pub async fn proactive_receptivity_ok(&self) -> bool {
        let Ok(Some(r)) = self.memory.proactive_receptivity().await else {
            return true;
        };
        let baseline = self.memory.proactive_baseline_rate().await.ok().flatten();
        r >= dead_zone_floor(baseline)
    }

    /// FOLLOW-THROUGH — the difference between filing a reminder and CARRYING it: open reminders
    /// with a deadline (due_ms, or a "by July 17th" date in the text) get escalating nudges as it
    /// approaches (10 / 5 / 2 days, then overdue), each stage fired once (persisted). A reminder
    /// that never resurfaces reads as forgotten — this is the anti-"not clicking" behavior.
    pub async fn deadline_followups(&self) -> Vec<String> {
        let (reminders, _) = self.split_tasks().await;
        if reminders.is_empty() {
            return Vec::new();
        }
        let today = local_now();
        let now = today.timestamp_millis();
        let mut fired: serde_json::Value = self
            .memory
            .profile_get("task_nudges")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let mut out = Vec::new();
        let mut changed = false;
        for t in &reminders {
            let deadline = t
                .due_ms
                .map(|m| m as i64)
                .or_else(|| parse_text_date_ms(&t.description, &today));
            let Some(dl) = deadline else { continue };
            let days_left = (dl - now) / 86_400_000;
            let stage = if days_left < 0 {
                "overdue"
            } else if days_left <= 2 {
                "2d"
            } else if days_left <= 5 {
                "5d"
            } else if days_left <= 10 {
                "10d"
            } else {
                continue;
            };
            let key = format!("{}|{stage}", t.id);
            if fired.get(&key).is_some() {
                continue;
            }
            fired[key] = serde_json::json!(now);
            changed = true;
            out.push(if days_left < 0 {
                format!("⚠️ This one's now OVERDUE: {} — want to knock it out together right now?", t.description)
            } else {
                format!(
                    "⏰ {} day(s) left: {} — say the word and I'll help move it (options, research, drafting — whatever it takes).",
                    days_left.max(0),
                    t.description
                )
            });
        }
        if changed {
            let _ = self
                .memory
                .profile_set("task_nudges", &fired.to_string())
                .await;
        }
        out
    }
}

/// Decide which orphaned engagement claims the transcript can honestly settle, and how.
///
/// Pure so the rule can be tested against a clock and a transcript that never existed. Two claims
/// are deliberately NOT settled:
///   - one whose 90-minute deadline has not passed — it is still live, not unanswered;
///   - one whose window runs past the last recorded turn — the transcript simply does not cover it.
///
/// The second is the one that matters. The box runs for weeks while the person is away, so silence
/// after the final recorded turn is the NORMAL state, and reading it as "ignored" would grade
/// hundreds of claims failed on missing evidence — manufacturing the exact bias this repairs,
/// while looking like thoroughness.
///
/// `turns` must be ascending. Returns (settled verdicts, count left pending).
pub(crate) fn settle_plan(
    orphans: &[(i64, i64)],
    turns: &[i64],
    last_turn: i64,
    now: i64,
) -> (Vec<(usize, bool)>, usize) {
    const WINDOW_MS: i64 = 90 * 60_000;
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for (i, &(sent, due)) in orphans.iter().enumerate() {
        if due > now || sent + WINDOW_MS > last_turn {
            skipped += 1;
            continue;
        }
        // Engaged iff the FIRST user turn after the send lands inside the window.
        let next = turns.partition_point(|&t| t <= sent);
        out.push((i, turns.get(next).is_some_and(|&t| t - sent <= WINDOW_MS)));
    }
    (out, skipped)
}

/// The receptivity below which a moment counts as a dead zone.
///
/// Relative to how this person responds in general, with two guards: a floor, so a baseline near
/// zero cannot wave everything through, and a ceiling, so an unusually responsive person does not
/// end up gated out of most of their own day. With no baseline yet, falls back to the original
/// absolute constant.
pub(crate) fn dead_zone_floor(baseline: Option<f64>) -> f64 {
    match baseline {
        Some(b) if b.is_finite() && b > 0.0 => (b * 0.6).clamp(0.10, 0.35),
        _ => 0.35,
    }
}
