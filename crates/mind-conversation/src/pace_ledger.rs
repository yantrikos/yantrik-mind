//! Proactive-mirror pace ledger -- tracks the mind's own proactive sends vs the user's reactions per domain. Extracted from lib.rs.

/// Versioned identity for the conservative next-message correction heuristic. A non-correction is
/// only tacit acceptance, and the recorded outcome keeps that weaker evidence explicit.
pub(crate) const PACK_EVIDENCE_EVALUATOR_ID: &str = "next-message-correction-heuristic-v1";

/// Versioned identity for the lexical proxy that grades whether surfaced pack evidence appeared in
/// the answer. This is evidence-use correlation, not a claim that the evidence caused the answer.
pub(crate) const PACK_EVIDENCE_USE_EVALUATOR_ID: &str = "pack-evidence-word-overlap-v1";

/// Does this message read as a correction of what was just said?
///
/// Deliberately conservative: only openings and phrases that are overwhelmingly correction-shaped.
/// "actually" mid-sentence, bare "no" answering a question the mind asked, and topic changes must
/// NOT match — a false "corrected" is worse than a missed one, because the counter's value is that
/// it can be trusted.
pub(crate) fn reads_as_correction(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    const OPENERS: &[&str] = &[
        "no, ",
        "no - ",
        "no — ",
        "nope, ",
        "wrong",
        "that's wrong",
        "thats wrong",
        "that is wrong",
        "not true",
        "that's not",
        "thats not",
        "that is not what",
        "i didn't ask",
        "i didnt ask",
        "i meant ",
        "that's incorrect",
        "incorrect.",
        "you're wrong",
        "youre wrong",
        "not what i asked",
        "not what i meant",
        "you misunderstood",
        "you got it wrong",
        "actually, no",
    ];
    OPENERS
        .iter()
        .any(|o| t.starts_with(o) || (o.len() > 8 && t.contains(o)))
}

impl super::ConversationEngine {
    pub(crate) async fn ledger(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("ledger")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn save_ledger(&self, v: &[serde_json::Value]) {
        let start = v.len().saturating_sub(600);
        let _ = self
            .memory
            .profile_set(
                "ledger",
                &serde_json::to_string(&v[start..]).unwrap_or_default(),
            )
            .await;
    }

    /// Mirror a proactively-sent message into the transcript — the mind must REMEMBER its own
    /// pings, or replies to them land with no referent ("Which bill are we talking about?").
    pub async fn mirror_proactive(&self, text: &str) {
        let _ = self.memory.append_message("assistant", text).await;
    }

    /// Log a proactive act as a pending prediction ("I judged this worth your attention").
    pub async fn ledger_sent(&self, domain: &str, what: &str) {
        let mut l = self.ledger().await;
        l.push(serde_json::json!({
            "ts": chrono::Utc::now().timestamp_millis(),
            "domain": domain,
            "what": what.chars().take(140).collect::<String>(),
            "outcome": "pending",
            "lesson": null,
        }));
        self.save_ledger(&l).await;
    }

    /// Grade the PREVIOUS exchange from the shape of the CURRENT user message — the turn-level
    /// reward channel the mind never had. Skills and tools carry measured ledgers; ANSWERS did
    /// not: a correction was absorbed into the next reply and taught nothing durable. Detection
    /// is deterministic and conservative (high precision — a false "corrected" poisons the
    /// signal); anything not correction-shaped counts as tacit acceptance, which is weaker
    /// evidence and weighed as such by keeping the two as separate counters, never a ratio
    /// pretending to be a score.
    pub(crate) async fn grade_previous_turn(&self, user_text: &str) {
        let prev = self.last_turn_answer.lock().unwrap().take();
        let Some(prev) = prev else { return };
        let corrected = reads_as_correction(user_text);
        // Rung three for whatever pack evidence the graded answer had surfaced: the grade of the
        // answer is the grade of its evidence, for want of anything finer. Censoring stays explicit
        // — a pack whose answer never came (`used == None`) is not graded, and a turn that surfaced
        // nothing grades nothing.
        let packs = std::mem::take(&mut *self.turn_packs.lock().unwrap());
        for p in packs {
            let Some(used) = p.used else { continue };
            let mut ev = mind_observability::DecisionEvent::span(
                &p.trace,
                p.used_event_id.as_deref(),
                "pack_evidence_graded",
            );
            ev.actor = Some("conversation".into());
            ev.lane = Some(p.lane.clone());
            ev.context_fingerprint = Some(p.context_fingerprint.clone());
            ev.object_id = Some(format!("pack:{}", p.pack_id));
            ev.verdict = Some(if corrected { "corrected" } else { "accepted" }.to_string());
            ev.semantic_success = Some(used);
            ev.evaluator_id = Some(PACK_EVIDENCE_EVALUATOR_ID.into());
            ev.outcome = Some(
                if corrected {
                    "the next message read as a correction"
                } else {
                    "the next message did not read as a correction (tacit acceptance — weaker evidence)"
                }
                .to_string(),
            );
            self.recorder.record(ev);
            let _ = self
                .memory
                .record_pack_event(
                    &p.pack_id,
                    mind_types::memory::PackEvent::Graded { good: !corrected },
                )
                .await;
        }
        let mut g: serde_json::Value = self
            .memory
            .profile_get("turn_grades")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({ "corrected": 0, "accepted": 0, "recent": [] }));
        let key = if corrected { "corrected" } else { "accepted" };
        g[key] = serde_json::json!(g[key].as_u64().unwrap_or(0) + 1);
        if corrected {
            // The lesson keeps WHAT was corrected beside HOW — the pair a future grounding pass
            // (or the regret replay) needs to learn anything from it.
            let recent = g["recent"].as_array().cloned().unwrap_or_default();
            let mut recent: Vec<serde_json::Value> = recent;
            recent.push(serde_json::json!({
                "ts": chrono::Utc::now().timestamp_millis(),
                "answer": prev.chars().take(200).collect::<String>(),
                "correction": user_text.chars().take(200).collect::<String>(),
            }));
            let start = recent.len().saturating_sub(10);
            g["recent"] = serde_json::json!(recent[start..]);
            self.ledger_correction(
                "turn",
                &prev.chars().take(140).collect::<String>(),
                &user_text.chars().take(200).collect::<String>(),
            )
            .await;
        }
        let _ = self.memory.profile_set("turn_grades", &g.to_string()).await;
    }

    /// Remember what was just said, so the NEXT message can grade it — and judge whether the answer
    /// USED the pack evidence this turn surfaced (rung two of a pack's local ladder).
    pub(crate) async fn note_turn_answer(&self, answer: &str) {
        *self.last_turn_answer.lock().unwrap() = Some(answer.chars().take(300).collect());
        let mut packs = std::mem::take(&mut *self.turn_packs.lock().unwrap());
        for p in packs.iter_mut() {
            // Judged once. `turn()` calls this once per primary turn, after the grounding that
            // stashed the evidence; a second call must not re-judge (and re-count) the same rows.
            if p.used.is_some() {
                continue;
            }
            let (used, share) = evidence_used_any(&p.rows, answer);
            let mut ev = mind_observability::DecisionEvent::span(
                &p.trace,
                p.surfaced_event_id.as_deref(),
                "pack_evidence_used",
            );
            ev.actor = Some("conversation".into());
            ev.lane = Some(p.lane.clone());
            ev.context_fingerprint = Some(p.context_fingerprint.clone());
            ev.object_id = Some(format!("pack:{}", p.pack_id));
            ev.verdict = Some(if used { "used" } else { "unused" }.to_string());
            ev.semantic_success = Some(used);
            ev.evaluator_id = Some(PACK_EVIDENCE_USE_EVALUATOR_ID.into());
            ev.confidence = Some(share);
            ev.lesson = Some("proxy: the best-matching surfaced row's share of informative words reappearing in the reply (any row clearing counts as use) — not causal use".to_string());
            p.used_event_id = ev.event_id.clone();
            p.used = Some(used);
            self.recorder.record(ev);
            if used {
                let _ = self
                    .memory
                    .record_pack_event(&p.pack_id, mind_types::memory::PackEvent::Used)
                    .await;
            }
        }
        *self.turn_packs.lock().unwrap() = packs;
    }

    /// Log a user correction — the most valuable signal there is. The lesson is permanent.
    pub async fn ledger_correction(&self, domain: &str, what: &str, lesson: &str) {
        let mut l = self.ledger().await;
        l.push(serde_json::json!({
            "ts": chrono::Utc::now().timestamp_millis(),
            "domain": domain,
            "what": what.chars().take(140).collect::<String>(),
            "outcome": "corrected",
            "lesson": lesson.chars().take(200).collect::<String>(),
        }));
        self.save_ledger(&l).await;
    }

    /// Resolve recent pending predictions: the user replying within the window = engaged; the
    /// stale-resolver calling with false = ignored. Mirrors the world-model resolution.
    pub async fn ledger_resolve(&self, engaged: bool) {
        let now = chrono::Utc::now().timestamp_millis();
        let mut l = self.ledger().await;
        let mut changed = false;
        for e in l.iter_mut().rev().take(12) {
            if e["outcome"].as_str() == Some("pending") {
                let age = now - e["ts"].as_i64().unwrap_or(0);
                if age < 90 * 60_000 {
                    e["outcome"] = serde_json::json!(if engaged { "engaged" } else { "ignored" });
                    changed = true;
                }
            }
        }
        if changed {
            self.save_ledger(&l).await;
        }
    }

    /// Pacing multiplier for a domain (1.0 = normal; >1 = slowed because it was being ignored).
    /// Consulted by the due-gates; adjusted only by the weekly review — policy changes are
    /// deliberate, logged, and reversible, never twitchy.
    pub async fn domain_pace(&self, domain: &str) -> f64 {
        self.memory
            .profile_get(&format!("pace:{domain}"))
            .await
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1.0)
    }

    /// Per-domain scoreboard over a trailing window: (sends, engaged, ignored,
    /// corrected, pending). Pending is counted, never absorbed — a rate that
    /// silently drops unresolved rows is flattering itself (see scoreboard.rs).
    pub(crate) fn ledger_stats(
        l: &[serde_json::Value],
        since_ms: i64,
    ) -> std::collections::BTreeMap<String, (u32, u32, u32, u32, u32)> {
        let mut m: std::collections::BTreeMap<String, (u32, u32, u32, u32, u32)> =
            std::collections::BTreeMap::new();
        for e in l {
            if e["ts"].as_i64().unwrap_or(0) < since_ms {
                continue;
            }
            let d = e["domain"].as_str().unwrap_or("general").to_string();
            let s = m.entry(d).or_insert((0, 0, 0, 0, 0));
            s.0 += 1;
            match e["outcome"].as_str().unwrap_or("pending") {
                "engaged" => s.1 += 1,
                "ignored" => s.2 += 1,
                "corrected" => s.3 += 1,
                _ => s.4 += 1,
            }
        }
        m
    }
}

/// The pack evidence one turn surfaced, carried from grounding to the answer to the next message.
#[derive(Debug, Clone)]
pub(crate) struct TurnPackEvidence {
    pub(crate) pack_id: String,
    pub(crate) trace: String,
    pub(crate) lane: String,
    pub(crate) context_fingerprint: String,
    /// The surfaced rows, each kept whole: the used-proxy is judged PER ROW, because a reply that
    /// faithfully uses one of five surfaced rows shares few words with the other four.
    pub(crate) rows: Vec<String>,
    pub(crate) surfaced_event_id: Option<String>,
    pub(crate) used: Option<bool>,
    pub(crate) used_event_id: Option<String>,
}

/// Did the reply USE any of the surfaced rows? Judged per row — `evidence_used` on each — and the
/// pack counts as used when ANY row clears; the share reported is the best row's. Judging the
/// rows as one text would divide one row's words by five rows' vocabulary and call faithful use of
/// one row "unused" (Codex's review of P.2).
pub(crate) fn evidence_used_any(rows: &[String], reply: &str) -> (bool, f64) {
    rows.iter()
        .map(|r| evidence_used(r, reply))
        .fold((false, 0.0), |(u, s), (ru, rs)| {
            (u || ru, if rs > s { rs } else { s })
        })
}

/// Did the reply USE this row? A PROXY, deliberately cheap and deterministic: the share of the
/// row's informative words (five letters or more) that reappear in the reply. `(true, share)`
/// when at least three reappear and they are a quarter or more of the row's vocabulary. It
/// cannot see paraphrase and it cannot see causation — a reply can restate a row it would have
/// written anyway (measured live on 2026-08-26: a reply that clearly acted on a row — "kill the
/// gradient", "break the six-card grid" — scored 0.18 and read as unused). It is named as a proxy
/// everywhere it is reported, and P.5 will not learn from it until the grade rung has enough rows
/// to say whether it predicts anything.
pub(crate) fn evidence_used(evidence: &str, reply: &str) -> (bool, f64) {
    fn informative(s: &str) -> std::collections::HashSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 5)
            .map(str::to_string)
            .collect()
    }
    let ev = informative(evidence);
    if ev.is_empty() {
        return (false, 0.0);
    }
    let rp = informative(reply);
    let shared = ev.iter().filter(|w| rp.contains(*w)).count();
    let share = shared as f64 / ev.len() as f64;
    (shared >= 3 && share >= 0.25, share)
}

#[cfg(test)]
mod evidence_proxy_tests {
    use super::evidence_used;

    #[test]
    fn the_used_proxy_needs_real_overlap_and_says_how_much() {
        let row =
            "Contrast — body text needs at least 4.5 to 1 against its background to be readable.";
        let (used, share) = evidence_used(row, "For body text you want contrast of at least 4.5:1 against the background so it stays readable.");
        assert!(used && share >= 0.25, "share {share}");
        let (unused, share2) = evidence_used(row, "I don't know.");
        assert!(!unused && share2 == 0.0);
        // One shared word is not use, however long it is.
        let (thin, _) = evidence_used(row, "the background matters");
        assert!(!thin);
        assert_eq!(evidence_used("", "anything"), (false, 0.0));
    }

    /// Five rows surfaced, the reply restates ONE: judged per row it is used, with that row's
    /// share; judged against the union vocabulary it would have read as unused.
    #[test]
    fn using_one_of_five_surfaced_rows_counts_as_used() {
        let rows: Vec<String> = vec![
            "Contrast — body text needs at least 4.5 to 1 against its background to be readable.".into(),
            "Typography — set body text on a modular scale with a measure of 45 to 75 characters per line.".into(),
            "Spacing — derive every gap in a layout from one base unit so the page reads as a system.".into(),
            "Motion — animate only transform and opacity so the compositor does the work.".into(),
            "Focus — every interactive control needs a visible focus ring that is not the browser default.".into(),
        ];
        let reply = "For body text you want contrast of at least 4.5 to 1 against the background so it stays readable.";
        let (used, share) = super::evidence_used_any(&rows, reply);
        assert!(used, "one faithfully used row is use");
        assert!(share >= 0.25, "the best row's share is reported: {share}");
        let (union_used, union_share) = super::evidence_used(
            &rows.join(
                "
",
            ),
            reply,
        );
        assert!(
            !union_used && union_share < 0.25,
            "the union denominator would have hidden it: {union_share}"
        );
        let (none, _) = super::evidence_used_any(&rows, "I don't know.");
        assert!(!none);
    }
}
