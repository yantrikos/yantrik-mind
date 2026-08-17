//! Proactive-mirror pace ledger -- tracks the mind's own proactive sends vs the user's reactions per domain. Extracted from lib.rs.

use super::*;

/// Does this message read as a correction of what was just said?
///
/// Deliberately conservative: only openings and phrases that are overwhelmingly correction-shaped.
/// "actually" mid-sentence, bare "no" answering a question the mind asked, and topic changes must
/// NOT match — a false "corrected" is worse than a missed one, because the counter's value is that
/// it can be trusted.
pub(crate) fn reads_as_correction(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    const OPENERS: &[&str] = &[
        "no, ", "no - ", "no — ", "nope, ", "wrong", "that's wrong", "thats wrong", "that is wrong",
        "not true", "that's not", "thats not", "that is not what", "i didn't ask", "i didnt ask",
        "i meant ", "that's incorrect", "incorrect.", "you're wrong", "youre wrong", "not what i asked",
        "not what i meant", "you misunderstood", "you got it wrong", "actually, no",
    ];
    OPENERS.iter().any(|o| t.starts_with(o) || (o.len() > 8 && t.contains(o)))
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
            .profile_set("ledger", &serde_json::to_string(&v[start..]).unwrap_or_default())
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
            self.ledger_correction("turn", &prev.chars().take(140).collect::<String>(), &user_text.chars().take(200).collect::<String>()).await;
        }
        let _ = self.memory.profile_set("turn_grades", &g.to_string()).await;
    }

    /// Remember what was just said, so the NEXT message can grade it.
    pub(crate) fn note_turn_answer(&self, answer: &str) {
        *self.last_turn_answer.lock().unwrap() = Some(answer.chars().take(300).collect());
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

    /// Per-domain scoreboard over a trailing window: (sends, engaged, ignored, corrected).
    pub(crate) fn ledger_stats(l: &[serde_json::Value], since_ms: i64) -> std::collections::BTreeMap<String, (u32, u32, u32, u32)> {
        let mut m: std::collections::BTreeMap<String, (u32, u32, u32, u32)> = std::collections::BTreeMap::new();
        for e in l {
            if e["ts"].as_i64().unwrap_or(0) < since_ms {
                continue;
            }
            let d = e["domain"].as_str().unwrap_or("general").to_string();
            let s = m.entry(d).or_insert((0, 0, 0, 0));
            s.0 += 1;
            match e["outcome"].as_str().unwrap_or("pending") {
                "engaged" => s.1 += 1,
                "ignored" => s.2 += 1,
                "corrected" => s.3 += 1,
                _ => {}
            }
        }
        m
    }

}
