//! Proactive-mirror pace ledger -- tracks the mind's own proactive sends vs the user's reactions per domain. Extracted from lib.rs.

use super::*;

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
