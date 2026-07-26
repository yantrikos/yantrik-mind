//! courier — THE FUTURE-SELF COURIER: keep the promise, wait without nagging, return when reality
//! makes the next move cheap.
//!
//!   "In March you told me, 'when the renewal arrives, compare it with last year before I forget' —
//!    it arrived this morning, and the side-by-side is ready. Want the 30-second version?"
//!
//! Sol's #3 (rid 019f4c65), and the SUPPLY the calibrated knock was missing: measured on the live
//! box the day the knock shipped, all 52 action packets classified `inferred`, so the knock could
//! never fire. Packets came only from festival/birthday/trip emissaries — nothing turned "you told
//! me to do X when Y happens" into prepared work. This module is that missing producer, and by
//! construction everything it creates carries provenance `told`.
//!
//! THE AUTHORITY RULE IS THE WHOLE DESIGN. Only an EXPLICIT commitment may open a thread. The mind
//! may infer that you probably want something — it may not manufacture a promise you never made and
//! then interrupt you about it. That is the difference between a companion and a system that
//! confuses accumulated observation with earned authority. So detection here is deliberately
//! CONSERVATIVE and deterministic (no LLM, no "sounds like an intention" judgement): a false
//! negative costs a missed convenience, a false positive costs trust.

/// A promise the user explicitly made, waiting for reality to make it actionable.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Commitment {
    /// What must happen first — "the renewal arrives".
    pub trigger: String,
    /// What was asked for — "compare it with last year".
    pub action: String,
}

/// Conditional leads that can open a thread. Each marks a FUTURE event, not a present request.
const LEADS: &[&str] = &["when ", "once ", "as soon as ", "next time ", "whenever "];

/// The action must be directed at the assistant. Without one of these the sentence is an
/// observation about the world ("when it rains the roof leaks"), not a commitment to act.
const ACTION_CUES: &[&str] = &[
    "remind me", "tell me", "let me know", "show me", "ping me", "flag it", "flag that",
    "compare", "check", "book", "order", "draft", "send", "look into", "follow up", "chase",
    "get me", "find me", "put together", "prepare",
];

/// Phrases people attach to commitments that carry no content of their own.
const FILLER: &[&str] = &["before i forget", "don't forget", "dont forget", "please", "can you", "could you"];

fn tidy(s: &str) -> String {
    let mut t = s.trim().trim_matches(|c: char| c == ',' || c == '.' || c == '"').trim().to_string();
    let low = t.to_lowercase();
    for f in FILLER {
        if let Some(pos) = low.find(f) {
            // strip the filler wherever it sits, then re-tidy the seam
            let mut rebuilt = String::with_capacity(t.len());
            rebuilt.push_str(&t[..pos]);
            rebuilt.push_str(&t[pos + f.len()..]);
            t = rebuilt.trim().trim_start_matches(',').trim().to_string();
            break;
        }
    }
    t.trim().trim_matches(|c: char| c == ',' || c == '.').trim().to_string()
}

/// Detect an EXPLICIT conditional commitment. `None` for anything less than unambiguous — questions,
/// bare reminders with no triggering event, and world-observations all return `None` on purpose.
pub(crate) fn detect(msg: &str) -> Option<Commitment> {
    let raw = msg.trim();
    // A question is a request for information, never a promise to be kept later.
    if raw.contains('?') {
        return None;
    }
    let low = raw.to_lowercase();
    // Find the earliest conditional lead.
    let (lead_at, lead) = LEADS
        .iter()
        .filter_map(|l| low.find(l).map(|i| (i, *l)))
        .min_by_key(|(i, _)| *i)?;
    let after = &raw[lead_at + lead.len()..];
    // The clause splits at the first comma: "<trigger>, <action>". Without the comma we cannot tell
    // where the condition ends, and guessing is how a false promise gets manufactured.
    let (trigger_part, action_part) = after.split_once(',')?;
    let trigger = tidy(trigger_part);
    let action = tidy(action_part);
    if trigger.len() < 3 || action.len() < 3 {
        return None;
    }
    // The action must actually ask the assistant to do something.
    let al = action.to_lowercase();
    if !ACTION_CUES.iter().any(|c| al.contains(c)) {
        return None;
    }
    Some(Commitment { trigger, action })
}

/// Content words of a trigger — used to decide whether a later observation satisfies it.
pub(crate) fn trigger_terms(trigger: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "a", "an", "my", "our", "his", "her", "their", "this", "that", "it", "is", "are",
        "was", "were", "and", "or", "for", "to", "of", "in", "on", "at", "by", "with", "arrives",
        "arrive", "comes", "come", "happens", "happen", "lands", "shows",
    ];
    trigger
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4 && !STOP.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Has `observation` satisfied this trigger? Requires EVERY content term to appear — a single
/// shared word ("renewal" alone matching "car renewal" vs "gym renewal") is not an event.
/// Returns false when the trigger has no content terms, so a vague trigger never auto-fires.
pub(crate) fn observation_satisfies(trigger: &str, observation: &str) -> bool {
    let terms = trigger_terms(trigger);
    if terms.is_empty() {
        return false;
    }
    let obs = observation.to_lowercase();
    terms.iter().all(|t| obs.contains(t.as_str()))
}

/// Retirement signals — the user saying the thread is finished. Checked before anything else so a
/// closed thread never knocks again.
pub(crate) fn is_retirement(msg: &str) -> bool {
    let m = msg.trim().to_lowercase();
    ["done", "handled", "sorted", "not relevant", "never mind", "nevermind", "forget it", "cancel that"]
        .iter()
        .any(|s| m == *s || m.starts_with(&format!("{s} ")))
}

impl super::ConversationEngine {
    async fn load_threads(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("courier_threads")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    async fn save_threads(&self, t: &[serde_json::Value]) {
        let _ = self
            .memory
            .profile_set("courier_threads", &serde_json::to_string(&t).unwrap_or_default())
            .await;
    }

    /// Capture an EXPLICIT commitment from this turn as a longitudinal thread. Returns the
    /// acknowledgement to append, or None when the message wasn't an unambiguous promise (the
    /// common case — see `detect`, which refuses to guess).
    pub async fn courier_capture(&self, user_text: &str) -> Option<String> {
        let c = detect(user_text)?;
        let now = chrono::Utc::now().timestamp_millis();
        // A promise with no horizon becomes a haunting. 180 days is long enough for "when the
        // renewal arrives" and short enough that a forgotten thread dies quietly.
        let ttl_days: i64 =
            std::env::var("YM_COURIER_TTL_DAYS").ok().and_then(|s| s.parse().ok()).unwrap_or(180);
        let mut threads = self.load_threads().await;
        // Same promise twice ⇒ refresh it rather than keeping two.
        if threads.iter().any(|t| {
            t.get("trigger").and_then(|x| x.as_str()) == Some(c.trigger.as_str())
                && t.get("status").and_then(|x| x.as_str()) == Some("open")
        }) {
            return None;
        }
        threads.push(serde_json::json!({
            "id": format!("thr:{now:x}"),
            "trigger": c.trigger,
            "action": c.action,
            "quote": user_text.chars().take(240).collect::<String>(),
            "said_ms": now,
            "expires_ms": now + ttl_days * 86_400_000,
            // By CONSTRUCTION: a thread can only be opened by an explicit statement, so its
            // provenance is always `told`. This is what makes the resulting packet knock-eligible.
            "provenance": "told",
            "status": "open",
        }));
        if threads.len() > 200 {
            let cut = threads.len() - 200;
            threads.drain(..cut);
        }
        self.save_threads(&threads).await;
        Some(format!("Noted — when {} , I'll {}.", c.trigger, c.action))
    }

    /// Retire any open thread the user just closed ("done", "not relevant"). Returns how many.
    pub async fn courier_retire(&self, user_text: &str) -> usize {
        if !is_retirement(user_text) {
            return 0;
        }
        let mut threads = self.load_threads().await;
        let mut n = 0;
        for t in threads.iter_mut() {
            if t.get("status").and_then(|x| x.as_str()) == Some("fired") {
                t["status"] = serde_json::json!("done");
                n += 1;
            }
        }
        if n > 0 {
            self.save_threads(&threads).await;
        }
        n
    }

    /// SCAN: expire what aged out, and fire any thread whose trigger a recent OBSERVATION satisfies.
    /// Firing creates the proof-carrying packet — stamped `told` — that the calibrated knock needs.
    /// Run on the idle tick. Returns short log lines.
    pub async fn courier_scan(&self) -> Vec<String> {
        let mut out = Vec::new();
        let now = chrono::Utc::now().timestamp_millis();
        let mut threads = self.load_threads().await;
        if threads.is_empty() {
            return out;
        }
        // Recent things the mind actually SAW or was TOLD — the only admissible evidence that a
        // trigger occurred. An inference that it "probably happened" may never fire a thread.
        let ctx = mind_types::AccessContext::Operator;
        let recent = self.memory.recent_messages(40, &ctx).await.unwrap_or_default();
        let mut changed = false;
        let mut fired: Vec<(String, String, String, String)> = Vec::new();
        for t in threads.iter_mut() {
            if t.get("status").and_then(|x| x.as_str()) != Some("open") {
                continue;
            }
            if t.get("expires_ms").and_then(|x| x.as_i64()).map(|e| e <= now).unwrap_or(false) {
                t["status"] = serde_json::json!("expired");
                changed = true;
                continue;
            }
            let trigger = t.get("trigger").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let quote = t.get("quote").and_then(|x| x.as_str()).unwrap_or("").to_string();
            // The PROMISE ITSELF must not fire the thread. The sentence that opened it obviously
            // mentions the trigger, so skip the original wording and require a SEPARATE observation.
            let Some((_, observation)) = recent
                .iter()
                .find(|(_, text)| text.trim() != quote.trim() && observation_satisfies(&trigger, text))
            else {
                continue;
            };
            let action = t.get("action").and_then(|x| x.as_str()).unwrap_or("").to_string();
            t["status"] = serde_json::json!("fired");
            changed = true;
            fired.push((trigger.clone(), action, quote.clone(), observation.clone()));
        }
        if changed {
            self.save_threads(&threads).await;
        }
        for (trigger, action, quote, observation) in fired {
            let title = format!("{action} — you asked for this when {trigger}");
            let mut evidence =
                vec![format!("you said: {quote}"), format!("observed: {observation}")];
            // ACTUALLY DO THE WORK. A packet that only restates the promise is a reminder wearing a
            // butler's coat — and it would make the knock's "I've prepared X" a lie. So the moment a
            // thread fires, the sub-agent goes and produces the real deliverable (the comparison,
            // the numbers, the draft), and THAT is what gets held for delivery. This is the night
            // shift doing homework while the user is idle, which is the whole premise.
            let prepared = match &self.researcher {
                Some(r) => {
                    let task = format!(
                        "The user asked me, in their own words: \"{quote}\"\n\
                         That moment has now arrived — I observed: \"{observation}\"\n\n\
                         DO THE WORK NOW: {action}.\n\
                         Produce the finished result itself, not a plan to produce it — concrete \
                         numbers, names, dates and a clear recommendation where one is warranted. \
                         Keep it under 200 words so it can be read in thirty seconds. If you cannot \
                         verify something, say so plainly rather than guessing."
                    );
                    // BOUNDED: this runs on the idle tick, which shares the poll loop with live
                    // messages. A sub-agent that wanders must not stall the user's next reply — on
                    // timeout we degrade to the honest reminder rather than blocking.
                    let secs: u64 = std::env::var("YM_COURIER_WORK_SECS")
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(150);
                    match tokio::time::timeout(std::time::Duration::from_secs(secs), r.run(&task)).await {
                        Ok(res) => {
                            for u in res.sources.iter().take(4) {
                                evidence.push(format!("source: {u}"));
                            }
                            (!res.answer.trim().is_empty()).then_some(res.answer)
                        }
                        Err(_) => {
                            out.push("[courier] preparation timed out — holding the reminder only".into());
                            None
                        }
                    }
                }
                None => None,
            };
            // Honest fallback: with no research capability wired, say plainly that this is the
            // reminder and not prepared work, rather than letting the knock overclaim.
            let body = match &prepared {
                Some(work) => format!(
                    "You asked: \"{quote}\"\nThat moment arrived — I saw: \"{observation}\"\n\n\
                     ── what you asked for ──\n{work}"
                ),
                None => format!(
                    "You asked: \"{quote}\"\nThat moment arrived — I saw: \"{observation}\"\n\n\
                     I haven't been able to do the work itself (no research capability configured), \
                     so this is the reminder only: {action}."
                ),
            };
            let id = self
                .packet_add_told(
                    "courier",
                    None,
                    "courier",
                    &title,
                    &body,
                    &format!("told: {trigger}"),
                    evidence,
                    if prepared.is_some() { 0.9 } else { 0.6 },
                    false,
                    now + 14 * 86_400_000,
                )
                .await;
            // Structural honesty: the knock says "I've prepared X" only when this is true.
            self.packet_mark_prepared(&id, prepared.is_some()).await;
            out.push(format!(
                "[courier] thread fired -> packet {id} ({})",
                if prepared.is_some() { "work prepared" } else { "reminder only" }
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_an_explicit_conditional_commitment() {
        let c = detect("when the insurance renewal arrives, compare it with last year before I forget").unwrap();
        // The trigger keeps its natural wording (it is quoted back to the user); `trigger_terms`
        // does the stop-word stripping for MATCHING, so the two concerns stay separate.
        assert!(c.trigger.to_lowercase().contains("insurance renewal"), "trigger: {}", c.trigger);
        assert!(c.action.to_lowercase().starts_with("compare it with last year"), "action: {}", c.action);

        let c2 = detect("Once the Amazon order ships, let me know so I can plan the day").unwrap();
        assert!(c2.trigger.to_lowercase().contains("amazon order"), "{c2:?}");
        assert!(c2.action.to_lowercase().contains("let me know"));
    }

    /// The authority rule, negatively. Each of these is something a looser detector would happily
    /// turn into a promise the user never made — and then interrupt them about.
    #[test]
    fn refuses_to_manufacture_promises() {
        // A question is not a commitment.
        assert!(detect("when does the renewal arrive?").is_none());
        // A world-observation with no request directed at the assistant.
        assert!(detect("when it rains, the roof leaks").is_none());
        // A conditional with no action cue — musing, not delegation.
        assert!(detect("when the quote arrives, it'll probably be higher").is_none());
        // No conditional lead at all: an ordinary present-tense request, handled elsewhere.
        assert!(detect("remind me to call the vendor").is_none());
        // No comma: we cannot tell where the condition ends, so we refuse rather than guess.
        assert!(detect("when the renewal arrives compare it with last year").is_none());
        // Empty-ish clauses.
        assert!(detect("when x, ok").is_none());
    }

    #[test]
    fn an_observation_must_match_the_whole_trigger() {
        let t = "the insurance renewal";
        assert!(observation_satisfies(t, "The insurance renewal just landed in your inbox"));
        // A partial match is NOT the event — "renewal" alone could be any renewal.
        assert!(!observation_satisfies(t, "your gym renewal is due"));
        // Unrelated text never fires it.
        assert!(!observation_satisfies(t, "dinner with Priya on Friday"));
        // A trigger with no content words must never auto-fire.
        assert!(!observation_satisfies("it happens", "anything at all"));
    }

    #[test]
    fn retirement_is_recognised_but_not_over_eagerly() {
        assert!(is_retirement("done"));
        assert!(is_retirement("Not relevant"));
        assert!(is_retirement("handled it myself"));
        // Ordinary conversation containing the word must not retire a thread.
        assert!(!is_retirement("I haven't done the taxes yet"));
        assert!(!is_retirement("is that done?"));
    }
}
