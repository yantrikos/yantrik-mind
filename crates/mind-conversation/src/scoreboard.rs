//! The Outer Scoreboard (One Mind vision, organ #2) — ONE measured "how am I
//! actually doing", joined from the instruments that already exist and kept
//! SEGMENTED — by instrument, by domain, by outcome class — never collapsed
//! into a single number. The vision's own warning names why: "an engagement
//! rate with a silence-gated denominator is spam wearing a metric." Every rate
//! on this board names a denominator an adversary would accept, and the panels
//! measured against reality (corrections, judgment verdicts) outrank the ones
//! whose denominator the mind itself chose (proactive sends).
//!
//! Doctrine (locked above the optimizer): measured state over remembered state
//! for every self-claim. A self-claim that contradicts this board is the thing
//! that must yield. What is not yet instrumented is SAID, not smoothed over —
//! a silent gap reads as "covered" when it isn't.

use super::*;

/// The turn-level reward channel: two counters, deliberately never a ratio —
/// tacit acceptance is weaker evidence than an explicit correction, and a
/// quotient would launder that asymmetry away.
#[derive(Debug, Clone, Default)]
pub struct TurnPanel {
    pub corrected: u64,
    pub accepted: u64,
    /// (what I answered, how it was corrected) — the lesson pairs, newest last.
    pub recent: Vec<(String, String)>,
}

/// One domain of proactive work over the trailing window. `sends` is
/// self-chosen (the silence-gated denominator); `engaged + ignored` is the
/// resolved denominator a rate may honestly stand on; `pending` is what a
/// naive percentage would silently absorb.
#[derive(Debug, Clone)]
pub struct DomainPanel {
    pub domain: String,
    pub sends: u32,
    pub engaged: u32,
    pub ignored: u32,
    pub corrected: u32,
    pub pending: u32,
    /// Current pacing multiplier (1.0 = normal; >1 = slowed after being ignored).
    pub pace: f64,
}

/// One tool's measured reliability from the engine bandit (Beta posterior).
#[derive(Debug, Clone)]
pub struct ToolPanel {
    pub tool: String,
    pub rate: f64,
    pub n: u64,
}

/// One knowledge pack's local ladder — counts only; every rate needs its own
/// denominator said aloud, and `graded < used` is censoring, not failure.
#[derive(Debug, Clone)]
pub struct PackPanel {
    pub pack_id: String,
    pub surfaced: u64,
    pub used: u64,
    pub graded: u64,
    pub good: u64,
}

/// The joined board. Panels stay separate; there is deliberately no method
/// that reduces this struct to one score.
#[derive(Debug, Clone, Default)]
pub struct Scoreboard {
    pub window_days: i64,
    pub turn: TurnPanel,
    pub domains: Vec<DomainPanel>,
    /// The judgment instrument's own rendering (immutable p-at-emission grades,
    /// Brier-skill bucketing built to defeat "the questions got easier").
    pub judgment: Option<String>,
    pub tools: Vec<ToolPanel>,
    /// Knowledge packs' local ladders (ARCH-6 P.2), empty when no pack evidence has reached a turn.
    pub packs: Vec<PackPanel>,
    /// World-model receptivity prediction, if it has enough transitions to speak.
    pub receptivity: Option<f64>,
    /// Segmentation axes the vision asks for that nothing measures yet. Said
    /// plainly on the board so absence is never mistaken for health.
    pub not_instrumented: &'static [&'static str],
}

pub(crate) const NOT_INSTRUMENTED: &[&str] = &["risk tier", "channel", "latency"];

impl Scoreboard {
    /// Pure assembly from the measured stores — no engine handle, so the board
    /// is testable exactly as it renders.
    pub(crate) fn from_parts(
        window_days: i64,
        turn_grades: Option<serde_json::Value>,
        domain_stats: std::collections::BTreeMap<String, (u32, u32, u32, u32, u32)>,
        paces: std::collections::BTreeMap<String, f64>,
        judgment: Option<String>,
        tools: Vec<(String, f64, u64)>,
        receptivity: Option<f64>,
    ) -> Scoreboard {
        let g = turn_grades.unwrap_or_else(|| serde_json::json!({}));
        let turn = TurnPanel {
            corrected: g["corrected"].as_u64().unwrap_or(0),
            accepted: g["accepted"].as_u64().unwrap_or(0),
            recent: g["recent"]
                .as_array()
                .map(|rs| {
                    rs.iter()
                        .filter_map(|r| {
                            Some((r["answer"].as_str()?.to_string(), r["correction"].as_str()?.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        let domains = domain_stats
            .into_iter()
            .map(|(domain, (sends, engaged, ignored, corrected, pending))| DomainPanel {
                pace: paces.get(&domain).copied().unwrap_or(1.0),
                domain,
                sends,
                engaged,
                ignored,
                corrected,
                pending,
            })
            .collect();
        Scoreboard {
            window_days,
            turn,
            domains,
            judgment,
            tools: tools.into_iter().map(|(tool, rate, n)| ToolPanel { tool, rate, n }).collect(),
            packs: Vec::new(),
            receptivity,
            not_instrumented: NOT_INSTRUMENTED,
        }
    }

    pub(crate) fn with_packs(mut self, packs: Vec<PackPanel>) -> Scoreboard {
        self.packs = packs;
        self
    }

    /// Deterministic rendering — every line traceable to a measured row. The
    /// order is the trust order: instruments graded against reality first,
    /// self-selected-denominator instruments after, gaps last.
    pub fn render(&self) -> String {
        let mut out = format!(
            "OUTER SCOREBOARD (measured, {}d window; segmented — no single number exists)\n",
            self.window_days
        );
        // 1) Turn grades — reality grading every answer.
        out.push_str(&format!(
            "\nANSWERS (graded by your next message): {} corrected · {} tacitly accepted — counts, not a ratio; acceptance is weaker evidence than correction.\n",
            self.turn.corrected, self.turn.accepted
        ));
        for (answer, correction) in self.turn.recent.iter().rev().take(3) {
            out.push_str(&format!(
                "  ✗ \"{}\" → \"{}\"\n",
                answer.chars().take(70).collect::<String>(),
                correction.chars().take(70).collect::<String>()
            ));
        }
        // 2) Judgment — the hardest instrument on the board (immutable p at emission).
        match &self.judgment {
            Some(j) => out.push_str(&format!("\nJUDGMENT (calibration against outcomes):\n{}\n", j.trim())),
            None => out.push_str("\nJUDGMENT: no graded predictions yet — the calibration record starts when the first verdict lands.\n"),
        }
        // 3) Tools — engine-measured reliability, worst first.
        if self.tools.is_empty() {
            out.push_str("\nTOOLS: no measured outcomes yet.\n");
        } else {
            out.push_str("\nTOOLS (measured reliability, worst first):\n");
            for t in self.tools.iter().take(10) {
                out.push_str(&format!("  {} — {:.0}% over {} runs\n", t.tool, t.rate * 100.0, t.n));
            }
        }
        // 3b) Pack evidence — a knowledge pack's local ladder, counts with their denominators.
        if !self.packs.is_empty() {
            out.push_str(
                "\nPACK EVIDENCE (surfaced → used [word-overlap proxy] → graded by your next message → accepted; graded is a subset of used — the rest is censored, not failed):\n",
            );
            for p in self.packs.iter().take(10) {
                out.push_str(&format!(
                    "  {}: {} surfaced · {} used · {} graded → {} accepted\n",
                    p.pack_id, p.surfaced, p.used, p.graded, p.good
                ));
            }
        }
        // 4) Proactive domains — the self-selected denominator, labeled as such.
        if self.domains.is_empty() {
            out.push_str("\nPROACTIVE: nothing sent in the window.\n");
        } else {
            out.push_str(
                "\nPROACTIVE, per domain (sends are self-chosen — silence never enters this denominator, so these rows rank BELOW the panels above):\n",
            );
            for d in &self.domains {
                let resolved = d.engaged + d.ignored;
                let rate = if resolved > 0 {
                    format!("{} of {} resolved engaged", d.engaged, resolved)
                } else {
                    "none resolved yet".to_string()
                };
                let pace = if (d.pace - 1.0).abs() > f64::EPSILON { format!(" · pace {:.1}x", d.pace) } else { String::new() };
                let pending = if d.pending > 0 { format!(" · {} pending", d.pending) } else { String::new() };
                out.push_str(&format!(
                    "  {}: {} sent → {} · {} ignored · {} corrected{pending}{pace}\n",
                    d.domain, d.sends, rate, d.ignored, d.corrected
                ));
            }
        }
        if let Some(r) = self.receptivity {
            out.push_str(&format!(
                "\nRECEPTIVITY (world model, learned from send outcomes): {:.0}% right now.\n",
                r * 100.0
            ));
        }
        // 5) The honest gap line — what the vision asks to segment by that nothing measures yet.
        out.push_str(&format!(
            "\nNOT YET INSTRUMENTED: {} — absent because unmeasured, not because healthy.\n",
            self.not_instrumented.join(", ")
        ));
        out
    }
}

impl super::ConversationEngine {
    /// Assemble the Outer Scoreboard from the live measured stores. This is the
    /// board every self-claim must yield to; `ym scoreboard` renders it, and the
    /// nightly narrative (organ #3) will render FROM it, never around it.
    pub async fn outer_scoreboard(&self, window_days: i64) -> Scoreboard {
        let since = chrono::Utc::now().timestamp_millis() - window_days * 86_400_000;
        let grades: Option<serde_json::Value> =
            self.memory.profile_get("turn_grades").await.ok().flatten().and_then(|s| serde_json::from_str(&s).ok());
        let ledger = self.ledger().await;
        let stats = Self::ledger_stats(&ledger, since);
        let mut paces = std::collections::BTreeMap::new();
        for d in stats.keys() {
            paces.insert(d.clone(), self.domain_pace(d).await);
        }
        // A typed absence beats six empty buckets: only render the trend
        // instrument when at least one graded verdict exists on the ledger.
        let graded_rows = self
            .memory
            .profile_get("judgment_ledger")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
            .map(|v| v.len())
            .unwrap_or(0);
        let judgment = if graded_rows == 0 { None } else { Some(self.judgment_trend_report().await) };
        let tools: Vec<(String, f64, u64)> = self
            .memory
            .tool_track_record()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, _, n)| *n >= 2)
            .collect();
        let receptivity = self.memory.proactive_receptivity().await.ok().flatten();
        let packs = self
            .memory
            .pack_stats()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|s| PackPanel { pack_id: s.pack_id, surfaced: s.surfaced, used: s.used, graded: s.graded, good: s.good })
            .collect();
        Scoreboard::from_parts(window_days, grades, stats, paces, judgment, tools, receptivity).with_packs(packs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Scoreboard {
        let grades = serde_json::json!({
            "corrected": 3, "accepted": 41,
            "recent": [{"ts": 1, "answer": "the meeting is at 4", "correction": "no, it moved to 5"}]
        });
        let mut stats = std::collections::BTreeMap::new();
        // 6 sent: 2 engaged, 1 ignored, 1 corrected, 2 pending — a naive pct would
        // report 33% engagement and silently absorb the pending rows.
        stats.insert("bills".to_string(), (6u32, 2u32, 1u32, 1u32, 2u32));
        let mut paces = std::collections::BTreeMap::new();
        paces.insert("bills".to_string(), 1.5f64);
        Scoreboard::from_parts(
            14,
            Some(grades),
            stats,
            paces,
            Some("Brier skill +0.12 vs base rate (n=9)".into()),
            vec![("web_search".into(), 0.62, 21)],
            Some(0.4),
        )
    }

    /// The pack panel is counts with denominators in the heading, and it is absent — not zeros —
    /// when no pack evidence has reached a turn.
    #[test]
    fn pack_panel_is_counts_with_denominators_or_absent() {
        let without = board().render();
        assert!(!without.contains("PACK EVIDENCE"), "{without}");
        let with = board()
            .with_packs(vec![PackPanel { pack_id: "yantrik/web-craft@0.3.0".into(), surfaced: 12, used: 5, graded: 4, good: 3 }])
            .render();
        assert!(with.contains("yantrik/web-craft@0.3.0: 12 surfaced · 5 used · 4 graded → 3 accepted"), "{with}");
        assert!(with.contains("censored, not failed"), "{with}");
    }

    #[test]
    fn every_rate_names_an_honest_denominator() {
        let r = board().render();
        // The proactive panel stands on the RESOLVED denominator and shows pending —
        // never a naked percentage over self-chosen sends.
        assert!(r.contains("2 of 3 resolved engaged"), "{r}");
        assert!(r.contains("2 pending"), "{r}");
        assert!(r.contains("self-chosen"), "the silence-gated denominator must be named: {r}");
        assert!(!r.contains("33%"), "no naked engagement percentage: {r}");
        // Turn grades stay two counters, never a quotient.
        assert!(r.contains("3 corrected · 41 tacitly accepted"), "{r}");
    }

    #[test]
    fn panels_stay_segmented_and_gaps_are_said() {
        let r = board().render();
        assert!(r.contains("no single number exists"), "{r}");
        for panel in ["ANSWERS", "JUDGMENT", "TOOLS", "PROACTIVE"] {
            assert!(r.contains(panel), "missing panel {panel}: {r}");
        }
        // The unmeasured axes the vision asks for are declared, not implied healthy.
        assert!(r.contains("NOT YET INSTRUMENTED: risk tier, channel, latency"), "{r}");
        // Trust order: judgment (graded against reality) renders before the
        // self-selected proactive rows.
        assert!(r.find("JUDGMENT").unwrap() < r.find("PROACTIVE").unwrap(), "{r}");
    }

    #[test]
    fn empty_stores_render_as_absence_not_zeros_pretending_to_be_health() {
        let b = Scoreboard::from_parts(14, None, Default::default(), Default::default(), None, vec![], None);
        let r = b.render();
        assert!(r.contains("no graded predictions yet"), "{r}");
        assert!(r.contains("nothing sent in the window"), "{r}");
        assert!(r.contains("no measured outcomes yet"), "{r}");
    }

    #[test]
    fn correction_lessons_survive_onto_the_board() {
        let r = board().render();
        assert!(r.contains("the meeting is at 4"), "{r}");
        assert!(r.contains("no, it moved to 5"), "{r}");
    }
}
