//! The proactive funnel ledger — WHERE candidates die, counted per stage per day.
//!
//! "Urges surfaced: 2%" was one opaque number; it named no gate, so every fix was a guess. This
//! ledger tags every kill site in the notice → tension → knock → sent pipeline with a structured
//! reason, so the question "which gate is the mass murderer" has a data answer. The builder model
//! asked for exactly this: without kill reasons its nightly gradient cannot distinguish "I am dumb"
//! from "I am slow" (qwen3.8-max consultation, 2026-08-04).
//!
//! Design constraints:
//! - Bumps happen on hot paths (event storms, every knock attempt) — one profile-KV read-modify-
//!   write per bump is fine at knock cadence, but RAW EVENT counts go through an in-memory tally
//!   flushed on the next debounced evaluation instead (see `ConversationEngine::note_event`).
//! - Date-bucketed, 14-day retention, pruned on write — the report answers "this week", not "ever".
//! - This is fail-soft instrumentation, so it must be verified end-to-end once deployed: it cannot
//!   report its own absence (the handoff_write lesson).

use super::*;

pub(crate) const FUNNEL_KEY: &str = "funnel_counters";
pub(crate) const FUNNEL_KEEP_DAYS: usize = 14;

/// Stage labels, kept flat ("stage:reason") so new reasons need no schema change.
/// Current vocabulary:
///   event:<domain>            raw external event received (ha:binary_sensor, cli, ...)
///   twitch:eval               a fast-twitch evaluation actually ran (post-debounce)
///   twitch:alert              a fast-twitch evaluation produced an alert that was delivered
///   knock:no-packets          maybe_knock ran, store empty
///   knock:not-knockworthy     packet failed the knockworthy contract (unprepared/expired/no proof)
///   knock:provenance          packet trigger was inferred/studied — not allowed to interrupt
///   knock:escrow-held         already held, no material change
///   knock:no-candidate        packets existed but none survived the search
///   knock:muted / knock:daily-cap / knock:unreceptive / knock:below-band   gate holds (escrowed)
///   knock:sent                a knock went out
pub(crate) fn prune(
    mut counters: serde_json::Map<String, serde_json::Value>,
    today: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut days: Vec<String> = counters.keys().cloned().collect();
    days.sort();
    // Keep the newest FUNNEL_KEEP_DAYS buckets. `today` is included even if not yet present so a
    // fresh day never evicts itself.
    let mut keep: Vec<&String> = days.iter().filter(|d| d.as_str() <= today).collect();
    if keep.len() > FUNNEL_KEEP_DAYS {
        let cut: Vec<String> = keep
            .drain(..keep.len() - FUNNEL_KEEP_DAYS)
            .cloned()
            .collect();
        for d in cut {
            counters.remove(&d);
        }
    }
    counters
}

/// Render the report: per-stage totals over the window, with the knock stages shown as a funnel.
pub(crate) fn render(counters: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut totals: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut days = 0usize;
    for (_, stages) in counters.iter() {
        days += 1;
        if let Some(m) = stages.as_object() {
            for (k, v) in m {
                *totals.entry(k.clone()).or_insert(0) += v.as_u64().unwrap_or(0);
            }
        }
    }
    if totals.is_empty() {
        return "📊 Funnel: no data yet — the counters only started with this deploy. Give it a day.".to_string();
    }
    let get = |k: &str| totals.get(k).copied().unwrap_or(0);
    let events: u64 = totals
        .iter()
        .filter(|(k, _)| k.starts_with("event:"))
        .map(|(_, v)| *v)
        .sum();
    let kills: Vec<(&str, u64)> = [
        "knock:no-packets",
        "knock:not-knockworthy",
        "knock:provenance",
        "knock:escrow-held",
        "knock:no-candidate",
        "knock:muted",
        "knock:daily-cap",
        "knock:unreceptive",
        "knock:below-band",
    ]
    .iter()
    .map(|k| (k.strip_prefix("knock:").unwrap(), get(k)))
    .filter(|(_, n)| *n > 0)
    .collect();
    let total_kills: u64 = kills.iter().map(|(_, n)| n).sum();
    let sent = get("knock:sent");
    let mut out = format!("📊 PROACTIVE FUNNEL — last {days} day(s)\n\n");
    out.push_str(&format!("  events noticed        {events:>6}\n"));
    for (k, v) in totals.iter().filter(|(k, _)| k.starts_with("event:")) {
        out.push_str(&format!(
            "    {:<20} {v:>6}\n",
            k.strip_prefix("event:").unwrap()
        ));
    }
    out.push_str(&format!(
        "  twitch evaluations    {:>6}\n",
        get("twitch:eval")
    ));
    out.push_str(&format!(
        "  twitch alerts sent    {:>6}\n",
        get("twitch:alert")
    ));
    out.push_str(&format!("  knock attempts killed {total_kills:>6}\n"));
    for (k, v) in &kills {
        out.push_str(&format!("    {k:<20} {v:>6}\n"));
    }
    out.push_str(&format!("  knocks sent           {sent:>6}\n"));
    if total_kills + sent > 0 {
        out.push_str(&format!(
            "\n  {:.0}% of knock attempts died; the biggest killer above is the gate to fix (or to trust).",
            total_kills as f64 * 100.0 / (total_kills + sent) as f64
        ));
    }
    out
}

impl super::ConversationEngine {
    /// Count a raw external event — in-memory only; storms must not hit the DB. Flushed by the next
    /// debounced evaluation (or a funnel report).
    pub fn note_event(&self, domain: &str) {
        let mut t = self.event_tally.lock().unwrap();
        *t.entry(format!("event:{domain}")).or_insert(0) += 1;
    }

    /// Persist one funnel bump (and any accumulated event tallies) into the date-bucketed ledger.
    pub(crate) async fn funnel_bump(&self, stage: &str) {
        let drained: Vec<(String, u64)> = {
            let mut t = self.event_tally.lock().unwrap();
            t.drain().collect()
        };
        let today = local_now().format("%Y-%m-%d").to_string();
        let mut counters = self
            .memory
            .profile_get(FUNNEL_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        let day = counters
            .entry(today.clone())
            .or_insert_with(|| serde_json::json!({}));
        if let Some(m) = day.as_object_mut() {
            let mut bump = |k: &str, by: u64| {
                let n = m.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                m.insert(k.to_string(), serde_json::json!(n + by));
            };
            if !stage.is_empty() {
                bump(stage, 1);
            }
            for (k, by) in drained {
                bump(&k, by);
            }
        }
        let counters = prune(counters, &today);
        let _ = self
            .memory
            .profile_set(FUNNEL_KEY, &serde_json::Value::Object(counters).to_string())
            .await;
    }

    /// `ym funnel` — the per-gate kill report.
    pub async fn funnel_report(&self) -> String {
        // Flush pending event tallies first so the report reflects now, not the last evaluation.
        self.funnel_bump("").await;
        let counters = self
            .memory
            .profile_get(FUNNEL_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        render(&counters)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_keeps_newest_window() {
        let mut m = serde_json::Map::new();
        for d in 1..=20 {
            m.insert(
                format!("2026-07-{d:02}"),
                serde_json::json!({"knock:sent": 1}),
            );
        }
        let out = prune(m, "2026-07-20");
        assert_eq!(out.len(), FUNNEL_KEEP_DAYS);
        assert!(out.contains_key("2026-07-20"), "newest bucket must survive");
        assert!(
            !out.contains_key("2026-07-01"),
            "oldest bucket must be pruned"
        );
    }

    #[test]
    fn render_names_the_biggest_killer_stage() {
        let mut m = serde_json::Map::new();
        m.insert(
            "2026-08-04".to_string(),
            serde_json::json!({"event:ha:lock": 40, "twitch:eval": 3, "knock:not-knockworthy": 9, "knock:sent": 1}),
        );
        let out = render(&m);
        assert!(
            out.contains("not-knockworthy") && out.contains("9"),
            "kill reason missing:\n{out}"
        );
        assert!(
            out.contains("90% of knock attempts died"),
            "kill rate missing:\n{out}"
        );
    }

    #[test]
    fn empty_ledger_says_so_instead_of_rendering_zeros() {
        assert!(render(&serde_json::Map::new()).contains("no data yet"));
    }
}
