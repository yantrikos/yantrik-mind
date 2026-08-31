//! Narrative-as-checksum (One Mind vision, organ #3) — the nightly first-person
//! paragraph is RENDERED from measured rows, never free-written. "If it can
//! invent, it will eventually become mythology." The structure is fixed: one
//! regret, one watch-for, one policy in force, one forbidden self-claim — each
//! line traceable to a measured row, persisted WITH its basis (so tomorrow can
//! re-render and diff — the checksum), and recalled every turn through the
//! telemetry block, which is how "recalled at boot" survives a process that
//! reboots many times a day.
//!
//! The LLM is structurally out of this loop: the render is `format!` over the
//! Outer Scoreboard and the regret log. (The weekly self-report still hands
//! facts to a model for prose — that surface is a letter to the operator; THIS
//! is the self-record, and the self-record does not get to be eloquent.)

use super::*;

/// The measured rows a narrative is rendered from — persisted beside the text
/// so any later reader can check the paragraph against its basis.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NarrativeBasis {
    pub corrected: u64,
    pub accepted: u64,
    /// (ask, epoch ms) of the newest regret on the log, if any.
    pub regret: Option<(String, i64)>,
    /// Worst measured tool (name, rate, n), if any has n >= 2.
    pub worst_tool: Option<(String, f64, u64)>,
    /// Domains currently paced away from 1.0: (domain, multiplier).
    pub paces: Vec<(String, f64)>,
}

/// The one self-claim class the record forbids every night — doctrine earned
/// the hard way (the mind once recalled a test double as its production
/// failover chain). A constant, because an invariant that varies is a mood.
pub const FORBIDDEN_SELF_CLAIM: &str =
    "I do not describe my own configuration or capabilities from memory — `myself` measures them; memory only remembers them.";

/// Pure render: first person, deterministic, every clause traceable to the
/// basis. No adjectives the rows don't pay for.
pub(crate) fn render_narrative(date: &str, basis: &NarrativeBasis) -> String {
    let mut p = format!(
        "SELF-RECORD {date} (rendered from measured rows; nothing free-written). \
         The conversation corrected me {} times and let {} answers stand.",
        basis.corrected, basis.accepted
    );
    match &basis.regret {
        Some((ask, _)) => p.push_str(&format!(
            " My regret on record: I missed a foreseeable ask — \"{}\".",
            ask.chars().take(120).collect::<String>()
        )),
        None => p.push_str(" No regrets on the log."),
    }
    match &basis.worst_tool {
        Some((tool, rate, n)) => p.push_str(&format!(
            " Watch-for: {tool} is my least reliable tool, {:.0}% over {n} runs — verify before I lean on it.",
            rate * 100.0
        )),
        None => p.push_str(" Watch-for: no tool has enough measured runs to rank yet."),
    }
    if basis.paces.is_empty() {
        p.push_str(" Policy in force: every domain at normal pace.");
    } else {
        let ps: Vec<String> = basis
            .paces
            .iter()
            .map(|(d, m)| format!("{d} slowed {m:.1}x"))
            .collect();
        p.push_str(&format!(
            " Policy in force: {} — set by the weekly review, not by mood.",
            ps.join(", ")
        ));
    }
    p.push_str(&format!(" Forbidden self-claim: {FORBIDDEN_SELF_CLAIM}"));
    p
}

impl super::ConversationEngine {
    /// Once per night, per-date, in the night window (2-6am local) — the same
    /// shape as `night_shift_due`, on its own key so a dry treasury or a failed
    /// shift never costs the self-record.
    pub async fn narrative_due(&self) -> bool {
        use chrono::Timelike;
        let today = local_now();
        if !(2..=6).contains(&today.hour()) {
            return false;
        }
        let date = today.format("%Y-%m-%d").to_string();
        let last = self
            .memory
            .profile_get("narrative_last_date")
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        last != date
    }

    /// Gather the measured basis, render tonight's paragraph, persist both.
    /// Also the `ym narrative now` output.
    pub async fn nightly_narrative_tick(&self) -> String {
        let date = local_now().format("%Y-%m-%d").to_string();
        let board = self.outer_scoreboard(14).await;
        let regret = self
            .memory
            .profile_get("regret_log")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
            .and_then(|log| {
                log.last().map(|r| {
                    (
                        r["ask"].as_str().unwrap_or("?").to_string(),
                        r["ts"].as_i64().unwrap_or(0),
                    )
                })
            });
        let basis = NarrativeBasis {
            corrected: board.turn.corrected,
            accepted: board.turn.accepted,
            regret,
            // tool_track_record arrives worst-first; the board pre-filtered n >= 2.
            worst_tool: board.tools.first().map(|t| (t.tool.clone(), t.rate, t.n)),
            paces: board
                .domains
                .iter()
                .filter(|d| (d.pace - 1.0).abs() > f64::EPSILON)
                .map(|d| (d.domain.clone(), d.pace))
                .collect(),
        };
        let text = render_narrative(&date, &basis);
        let record = serde_json::json!({
            "date": date,
            "text": text,
            "basis": serde_json::to_value(&basis).unwrap_or(serde_json::Value::Null),
        });
        let _ = self
            .memory
            .profile_set("narrative_last", &record.to_string())
            .await;
        let _ = self.memory.profile_set("narrative_last_date", &date).await;
        // Rolling log, capped — yesterday's behavior stays answerable to today's.
        let mut log: Vec<serde_json::Value> = self
            .memory
            .profile_get("narrative_log")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        log.retain(|r| r["date"].as_str() != Some(record["date"].as_str().unwrap_or("")));
        log.push(record);
        if log.len() > 30 {
            let cut = log.len() - 30;
            log.drain(..cut);
        }
        let _ = self
            .memory
            .profile_set("narrative_log", &serde_json::Value::Array(log).to_string())
            .await;
        text
    }

    /// The latest persisted self-record, if any — what the grounding recalls.
    pub async fn last_narrative(&self) -> Option<(String, String)> {
        let v: serde_json::Value = serde_json::from_str(
            &self
                .memory
                .profile_get("narrative_last")
                .await
                .ok()
                .flatten()?,
        )
        .ok()?;
        Some((
            v["date"].as_str()?.to_string(),
            v["text"].as_str()?.to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn basis() -> NarrativeBasis {
        NarrativeBasis {
            corrected: 3,
            accepted: 41,
            regret: Some(("remind me before the visa window closed".into(), 5)),
            worst_tool: Some(("web_search".into(), 0.62, 21)),
            paces: vec![("bills".into(), 1.5)],
        }
    }

    #[test]
    fn the_narrative_is_rendered_not_written() {
        let p = render_narrative("2026-08-16", &basis());
        // Every fixed organ of the record is present…
        assert!(p.contains("SELF-RECORD 2026-08-16"), "{p}");
        assert!(p.contains("corrected me 3 times"), "{p}");
        assert!(p.contains("My regret on record"), "{p}");
        assert!(p.contains("visa window"), "{p}");
        assert!(p.contains("Watch-for: web_search"), "{p}");
        assert!(p.contains("62% over 21 runs"), "{p}");
        assert!(p.contains("bills slowed 1.5x"), "{p}");
        assert!(p.contains(FORBIDDEN_SELF_CLAIM), "{p}");
        // …and the render is a pure function: same rows, same paragraph, always.
        assert_eq!(
            p,
            render_narrative("2026-08-16", &basis()),
            "the checksum property"
        );
    }

    #[test]
    fn empty_rows_render_as_stated_absence() {
        let empty = NarrativeBasis {
            corrected: 0,
            accepted: 0,
            regret: None,
            worst_tool: None,
            paces: vec![],
        };
        let p = render_narrative("2026-08-17", &empty);
        assert!(p.contains("No regrets on the log"), "{p}");
        assert!(p.contains("no tool has enough measured runs"), "{p}");
        assert!(p.contains("every domain at normal pace"), "{p}");
        // The forbidden-claim line survives even an empty night.
        assert!(p.contains(FORBIDDEN_SELF_CLAIM), "{p}");
    }
}
