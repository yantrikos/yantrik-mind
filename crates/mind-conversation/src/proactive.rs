//! Proactive drive -- vigilance scan, DMN tick, digest/ask, deadline follow-ups. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// SELF-VIGILANCE (self-healing) — read the mind's own self-build cron log and, if its most recent
    /// run FAILED, emit an Operational urge so the failure surfaces (via the digest) instead of dying
    /// silently. Observation-only (rung 1–2): it never remediates, just notices + records. Cheap (a
    /// file read), no LLM. Deduped on (kind, about) so the same failure accrues rather than floods.
    pub async fn vigilance_scan(&self) -> Option<String> {
        let path = std::env::var("YM_CRON_LOG")
            .unwrap_or_else(|_| "/var/lib/yantrik-mind/selfbuild-cron.log".to_string());
        let log = std::fs::read_to_string(&path).ok()?;
        let about = Self::vigilance_scan_text(&log)?;
        let _ = self.memory.record_tension(mind_types::TensionKind::Operational, 0.85, &about).await;
        Some(about)
    }

    /// Pure failure-detector over a self-build log (testable). Looks ONLY at the most recent tick block
    /// and flags it only on an EXPLICIT failure signature — never on a merely-incomplete block (which
    /// could be a run still in progress), so it doesn't false-alarm. Returns a short description, or None.
    pub(crate) fn vigilance_scan_text(log: &str) -> Option<String> {
        let block = log.rsplit_once("self-build tick start").map(|(_, a)| a).unwrap_or(log);
        // Real failures only — NOT "auto-merge BLOCKED" (that's a controlled draft, working as intended).
        // The auth signatures exist because of a real blind spot (2026-07-16): a revoked OAuth token
        // failed the self-improve loop for DAYS — five junk PRs merged with "Failed to authenticate.
        // API Error: 401 …" as the title — and nothing here matched, so the self-healing rung stayed
        // silent and the mind reported itself healthy. The watchdog must know what a lockout looks like.
        const SIGS: &[&str] = &[
            "No such file", "ABORT:", "MERGE-FAIL", "PR-FAIL", "could not compile",
            "clone failed", "tests failed", "timeout: failed to run",
            "Failed to authenticate", "API Error: 401", "API Error: 403",
            "access token has been revoked", "Invalid authentication credentials", "Invalid API key",
        ];
        let hit = SIGS.iter().find(|s| block.contains(**s))?;
        let line = block.lines().find(|l| l.contains(*hit)).unwrap_or(hit).trim();
        Some(format!("my last self-build run failed — {}", line.chars().take(160).collect::<String>()))
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
                    .recall_typed(mind_types::RecallQuery { text: String::new(), top_k: 8, kind: None }, &mind_types::AccessContext::Operator)
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
                    if stale > 0 { parts.push(format!("{stale} stale")); }
                    if fragile > 0 { parts.push(format!("{fragile} fragile")); }
                    format!("[dmn] {}", parts.join(", "))
                });
            }
            // RECONCILE — judge ONE open contradiction, apply the verdict as signed evidence on the
            // winning and losing belief nodes so confidence scores actually shift, then bank an
            // observability note and emit a COHERENCE tension. UNRESOLVED leaves scores unchanged.
            1 => {
                let cs = self.memory.conflicts(&mind_types::AccessContext::Operator).await.unwrap_or_default();
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
                        ChatMessage::system("You weigh conflicting beliefs cautiously. One sentence."),
                        ChatMessage::user(&prompt),
                    ];
                    // PRIVATE-GROUNDED: this prompt carries two of the household's stored beliefs
                    // VERBATIM (read with AccessContext::Operator — every member's private facts), so
                    // it must PREFER the private (owned-hardware) lane and only escalate to cloud with
                    // an audit. It was an unscoped `chat()` = a silent Household (cloud) call on every
                    // reconcile tick — the same leak agent_loop already fixed, missed on this path.
                    if let Ok(r) = self.inference.chat_grounded(messages, GenerationConfig::default()).await {
                        let verdict = r.text.trim();
                        let verdict_upper = verdict.to_uppercase();
                        let (winner, loser, verdict_label) =
                            if verdict_upper.starts_with('A') {
                                (Some(c.belief_a.as_str()), Some(c.belief_b.as_str()), "→ A wins")
                            } else if verdict_upper.starts_with('B') {
                                (Some(c.belief_b.as_str()), Some(c.belief_a.as_str()), "→ B wins")
                            } else {
                                (None, None, "→ unresolved")
                            };
                        if let (Some(w), Some(l)) = (winner, loser) {
                            let _ = self.memory.remember_as_belief(BeliefAssertion {
                                statement: w.to_string(),
                                polarity: 1.0,
                                weight: 0.5,
                                source_event: Some("dmn_reconcile".into()),
                                provenance: "dmn".into(),
                            }).await;
                            let _ = self.memory.remember_as_belief(BeliefAssertion {
                                statement: l.to_string(),
                                polarity: -1.0,
                                weight: 0.5,
                                source_event: Some("dmn_reconcile".into()),
                                provenance: "dmn".into(),
                            }).await;
                        }
                        let note: String =
                            format!("On the tension '{}' vs '{}': {}", c.belief_a, c.belief_b, verdict)
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
                    .recall_typed(mind_types::RecallQuery { text: String::new(), top_k: 10, kind: None }, &mind_types::AccessContext::Operator)
                    .await
                    .unwrap_or_default();
                if rs.len() < 3 {
                    log.push("[dmn] associate: too little stored to connect".to_string());
                    return log;
                }
                let facts = rs.iter().map(|r| format!("- {}", r.item.text)).collect::<Vec<_>>().join("\n");
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
                if let Ok(r) = self.inference.chat_grounded(messages, GenerationConfig::default()).await {
                    let insight = r.text.trim();
                    if insight.len() > 8 {
                        let statement: String =
                            format!("(hypothesis) {insight}").chars().take(400).collect();
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
        let winners: Vec<_> = open.into_iter().filter(|t| t.pressure >= min_pressure).collect();
        if winners.is_empty() {
            return None; // nothing clears the bar → stay silent (the default)
        }
        // Re-rank by cognitive urgency: base pressure × (1 + engine demand for the topic). Tensions
        // whose subject overlaps with low-confidence beliefs score higher — what the mind most needs
        // to address surfaces first rather than treating all passing tensions as pressure-equivalent.
        let topics: Vec<String> = winners.iter().map(|t| t.about.clone()).collect();
        let demands = self.memory.knowledge_gaps(&topics).await.unwrap_or_else(|_| vec![0.0; topics.len()]);
        let mut scored: Vec<(usize, f64)> = winners
            .iter()
            .enumerate()
            .map(|(i, t)| (i, t.pressure * (1.0 + demands.get(i).copied().unwrap_or(0.0))))
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

    /// ASK DRIVE — curiosity turned OUTWARD, as a progressive interview rather than a fixed list. A
    /// companion shouldn't wait to be fed; when it doesn't know you it ASKS, in order: first your NAME,
    /// then your PURPOSE (what you want from it), then purpose-grounded follow-ups one at a time — and
    /// it goes quiet once it knows enough (never pesters). The caller gates it to ≤1/period + idle +
    /// quiet-hours. Name/purpose answers are captured directly (`handle_turn` → `capture_onboard`);
    /// later answers flow back as ordinary chat → consolidation → typed beliefs.
    pub async fn proactive_ask(&self) -> Option<String> {
        // Don't stack a new question while we're still awaiting an answer to the last one.
        if self.pending_slot().await.is_some() {
            return None;
        }
        let name = self.memory.profile_get("name").await.ok().flatten();
        if name.is_none() {
            self.set_pending_slot(Some("name")).await;
            return Some("Before we really get going — what should I call you?".to_string());
        }
        let purpose = self.memory.profile_get("purpose").await.ok().flatten();
        if purpose.is_none() {
            self.set_pending_slot(Some("purpose")).await;
            return Some(format!(
                "What would you most like me to help you with, {}? Knowing your main goal lets me be genuinely useful instead of generic.",
                name.unwrap_or_default()
            ));
        }
        // INTERESTS stage — actively learn the user's world (hobbies, what they follow, the people and
        // companies they care about) so grounding, gifts, and the entity-sim have real material. Asks one
        // uncovered dimension per tick; once all are covered it falls through to the purpose taper.
        let covered = self.ask_covered().await;
        if let Some((key, q)) = INTEREST_DIMS.iter().find(|(k, _)| !covered.iter().any(|c| c == k)) {
            self.set_pending_slot(Some(&format!("interest:{key}"))).await;
            return Some(q.to_string());
        }
        // OPEN stage — purpose-grounded follow-ups, but taper once the brain knows enough about you.
        let enough: usize = std::env::var("YM_ASK_ENOUGH").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
        let known = self
            .memory
            .recall_typed(mind_types::RecallQuery { text: String::new(), top_k: 64, kind: None }, &mind_types::AccessContext::Operator)
            .await
            .map(|r| r.len())
            .unwrap_or(0);
        if known >= enough {
            return None;
        }
        self.purpose_followup(&purpose.unwrap_or_default()).await
    }

    /// The in-flight get-to-know-you question, PERSISTED in the substrate. This was an in-memory
    /// Mutex — and every restart (which self-deploy now does several times a day) silently dropped
    /// it, so the user's answer arrived with no question pending, got treated as ordinary chat, and
    /// the drive re-asked later ("I already told you!"). The bug class that keeps biting: state
    /// that gates cross-turn behavior must live in the substrate, not the process.
    pub(crate) async fn pending_slot(&self) -> Option<String> {
        self.memory
            .profile_get("pending_onboard")
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
    }

    pub(crate) async fn set_pending_slot(&self, v: Option<&str>) {
        let _ = self.memory.profile_set("pending_onboard", v.unwrap_or("")).await;
    }

    /// Curiosity as NORMAL conversation: occasionally close a reply with one get-to-know-you
    /// question instead of quarantining all asks behind idle gates. Paced (YM_ASK_PIGGYBACK_SECS,
    /// default 4h), skipped while a question is already pending. Most of the "how much do you
    /// actually know about me" gaps close here — in the flow of talk, not in scheduled pings.
    pub(crate) async fn maybe_piggyback_ask(&self) -> Option<String> {
        if std::env::var("YM_ASK_PIGGYBACK").map(|v| v == "off").unwrap_or(false) {
            return None;
        }
        if self.pending_slot().await.is_some() {
            return None;
        }
        let period_ms: i64 = std::env::var("YM_ASK_PIGGYBACK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(14_400) * 1000;
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
        let (key, q) = INTEREST_DIMS.iter().find(|(k, _)| !covered.iter().any(|c| c == k))?;
        self.set_pending_slot(Some(&format!("interest:{key}"))).await;
        let _ = self.memory.profile_set("ask_piggyback_ms", &now.to_string()).await;
        Some(q.to_string())
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
        let _ = self.memory.profile_set("ask_covered", &serde_json::to_string(&c).unwrap_or_else(|_| "[]".into())).await;
    }

    /// Mark that a proactive message just went out — the world model's engagement resolver picks
    /// it up: a user reply within 90 min = ENGAGED; silence past the window = IGNORED (resolved by
    /// the poll loop). Last send wins; one outstanding ledger entry at a time.
    pub async fn note_proactive_sent(&self) {
        let now = chrono::Utc::now().timestamp_millis();
        let _ = self.memory.profile_set("proactive_pending", &now.to_string()).await;
        // JUDGMENT LEDGER: a proactive send IS a falsifiable prediction — "the recipient engages
        // within the window". p = the learned engagement rate (improvable). Graded on resolve. This
        // is the mandatory-eligibility auto-log (Terra's anti-gaming rule): no opt-in, no post-hoc p.
        let p = self.memory.proactive_receptivity().await.ok().flatten().unwrap_or(0.5);
        self.judgment_log("proactive", "engagement", "recipient engages within 90m", p, now + 90 * 60_000, &now.to_string()).await;
    }

    /// Resolve the outstanding proactive send, if any. `via_user_turn`: the user just spoke —
    /// engaged iff within the window. Otherwise (tick path) only resolves STALE entries as ignored.
    pub async fn resolve_proactive(&self, via_user_turn: bool) {
        let Some(sent_ms) = self
            .memory
            .profile_get("proactive_pending")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse::<i64>().ok())
        else {
            return;
        };
        let now = chrono::Utc::now().timestamp_millis();
        let within = now - sent_ms <= 90 * 60_000;
        if via_user_turn {
            let _ = self.memory.record_proactive_outcome(sent_ms, within).await;
            self.judgment_grade(&sent_ms.to_string(), within).await; // grade the engagement prediction
            let _ = self.memory.profile_set("proactive_pending", "").await;
        } else if !within {
            let _ = self.memory.record_proactive_outcome(sent_ms, false).await;
            self.judgment_grade(&sent_ms.to_string(), false).await;
            let _ = self.memory.profile_set("proactive_pending", "").await;
        }
    }

    /// Gate for OPTIONAL proactive beats: false only when the learned world model says this moment
    /// is a dead zone (<35% engagement). True until there's real data — never guess-gate.
    pub async fn proactive_receptivity_ok(&self) -> bool {
        match self.memory.proactive_receptivity().await {
            Ok(Some(r)) => r >= 0.35,
            _ => true,
        }
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
            let deadline = t.due_ms.map(|m| m as i64).or_else(|| parse_text_date_ms(&t.description, &today));
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
            let _ = self.memory.profile_set("task_nudges", &fired.to_string()).await;
        }
        out
    }

}
