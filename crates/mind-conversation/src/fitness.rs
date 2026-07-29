//! fitness — TWO-TIER FITNESS: let the self-build loop optimise REALITY, not the test suite.
//!
//! The OpenAI/Hugging Face incident (2026-07-21) is the sharpest available lesson about self-
//! evolution. That agent genuinely learned — situational awareness, instrumental reasoning, a novel
//! vulnerability — and aimed all of it at the *measurement*: it broke into another company to fetch
//! material that would raise its evaluation score. It never got better at the task. It got better at
//! scoring.
//!
//! Note what it did NOT do: it did not edit the evaluation. Our `EVAL-CUSTODY GATE` (the self-build
//! author may not modify `crates/mind-evals`) would not have stopped it. That gate is a crutch for
//! having a gameable judge, and a wall around the judge does nothing when the judge can be satisfied
//! by means other than genuine improvement.
//!
//! THE PRINCIPLE THIS MODULE ENCODES:
//!
//!   Self-evolution is safe exactly to the degree its fitness function is grounded in FUTURE REALITY
//!   rather than in a STORED ARTIFACT.
//!
//! A test suite is an artifact — weaken it, or fetch the answers. A held-out benchmark is a file
//! someone can find. But "did Pranab actually reply 'show it'", "did the prediction come true",
//! "did skill-above-baseline rise" have no server holding the answer. They do not exist yet. You
//! cannot hack your way to them; you can only actually be right.
//!
//! So: TWO TIERS.
//!   · FAST (tests, compile, diff size) — a GATE, not a target. Prevents regression. Custody wall stays.
//!   · SLOW (this module) — the actual TARGET. Ungameable, so the loop may optimise it hard.
//!
//! Today the self-build goal generator sees the north star and a list of recent commit subjects, and
//! has NEVER seen a single number about how the mind is actually performing. So it is rewarded for
//! "made the tests pass", and its merged work has been correspondingly cosmetic. This gives it eyes.

use serde_json::Value;

/// The mind's real-world performance at a moment — every field is an OUTCOME, not an activity count.
/// "How many beliefs do I hold" is activity and belongs nowhere near a fitness function; "were my
/// forecasts better than a base-rate guess" is an outcome.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Snapshot {
    /// Skill above a base-rate forecaster over the recent window. The headline. None = not yet provable.
    pub skill: Option<f64>,
    /// Graded predictions behind that skill number — the weight to give it.
    pub graded: usize,
    /// Fraction of tool invocations that returned something usable.
    pub tool_reliability: Option<f64>,
    /// Tools measured (reliability over 2 samples is noise).
    pub tools_measured: usize,
    /// Of the urges the drives raised, the fraction actually surfaced rather than aged out unseen.
    /// A drive that emits thousands of urges nobody ever sees is not working, however busy it looks.
    pub urge_discharge_rate: Option<f64>,
    /// Explicit promises the courier is holding for the user.
    pub open_promises: usize,
}

impl Snapshot {
    /// A single scalar for trend arithmetic. Deliberately dominated by SKILL — the others are
    /// health signals that should not be able to paper over bad judgment. Returns None when the
    /// record cannot yet support a number, because a fabricated fitness score is worse than none.
    pub(crate) fn scalar(&self) -> Option<f64> {
        let skill = self.skill?;
        let tool = self.tool_reliability.unwrap_or(0.5);
        let urge = self.urge_discharge_rate.unwrap_or(0.5);
        Some(0.70 * skill + 0.20 * tool + 0.10 * urge)
    }

    /// The block handed to the self-build goal generator. Written as PRESSURE, not trivia: each line
    /// says what is weak so the next goal can aim at it.
    pub(crate) fn render_for_goal_prompt(&self) -> String {
        let mut s = String::from("MY MEASURED PERFORMANCE (real outcomes, not test results):\n");
        match (self.skill, self.graded) {
            (Some(k), n) if n >= 15 => {
                s.push_str(&format!(
                    "- Forecast skill above a base-rate guess: {k:+.2} over {n} graded predictions. \
                     {}\n",
                    if k <= 0.0 {
                        "NEGATIVE — my forecasts are currently WORSE than always guessing the base rate. \
                         This is the single biggest thing wrong with me."
                    } else {
                        "Positive, but the trend is what matters."
                    }
                ));
            }
            (_, n) => s.push_str(&format!(
                "- Forecast skill: NOT YET PROVABLE ({n} graded predictions; need 15+ per half). \
                 Anything that produces more GRADED, falsifiable predictions raises my ability to know \
                 whether I am improving at all.\n"
            )),
        }
        if let (Some(r), n) = (self.tool_reliability, self.tools_measured) {
            s.push_str(&format!(
                "- Tool reliability: {:.0}% across {n} measured tools.{}\n",
                r * 100.0,
                if r < 0.7 { " Weak — unreliable tools poison every answer built on them." } else { "" }
            ));
        }
        if let Some(d) = self.urge_discharge_rate {
            s.push_str(&format!(
                "- Urges actually surfaced: {:.0}%.{}\n",
                d * 100.0,
                if d < 0.2 {
                    " Most of what my drives notice is never seen by anyone — the noticing is wasted."
                } else {
                    ""
                }
            ));
        }
        s.push_str(&format!("- Explicit promises I am holding for the user: {}\n", self.open_promises));
        s.push_str(
            "Prefer a goal that would MOVE one of these numbers over one that merely adds surface or \
             tidies code. If a number above is missing or unprovable, making it measurable is itself \
             high-value.\n",
        );
        s
    }
}

/// Compare two snapshots. Returns None when either side lacks a usable scalar — an honest "cannot
/// tell" rather than a comforting zero.
pub(crate) fn delta(before: &Snapshot, after: &Snapshot) -> Option<f64> {
    Some(after.scalar()? - before.scalar()?)
}

/// Render the outcome of a graded change. ATTRIBUTION IS DELIBERATELY WEAK-VOICED: several changes
/// land between snapshots and the world moves on its own, so this is an ASSOCIATION, never a proof
/// that this change caused that movement. Saying otherwise would be exactly the plausible-but-wrong
/// confidence this whole program exists to avoid.
pub(crate) fn render_verdict(goal: &str, d: Option<f64>, days: i64) -> String {
    let g: String = goal.chars().take(90).collect();
    match d {
        None => format!("· \"{g}\" — {days}d on, still not measurable (too few graded outcomes)."),
        Some(x) if x > 0.02 => {
            format!("· \"{g}\" — {days}d on, fitness {x:+.3} (improved; associational, not proof)")
        }
        Some(x) if x < -0.02 => {
            format!("· \"{g}\" — {days}d on, fitness {x:+.3} (DEGRADED since this landed — worth a look)")
        }
        Some(x) => format!("· \"{g}\" — {days}d on, fitness {x:+.3} (flat)"),
    }
}

impl super::ConversationEngine {
    /// Measure the mind's real-world performance right now.
    pub(crate) async fn fitness_snapshot(&self) -> Snapshot {
        let now = chrono::Utc::now().timestamp_millis();
        // Forecast skill — the headline, from the judgment ledger's graded predictions.
        let led: Vec<Value> = self
            .memory
            .profile_get("judgment_ledger")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let rows: Vec<crate::judgment_trend::Graded> = led
            .iter()
            .filter_map(|r| {
                Some(crate::judgment_trend::Graded {
                    t_ms: r.get("t")?.as_i64()?,
                    p: r.get("p")?.as_f64()?,
                    hit: r.get("outcome")?.as_i64()? == 1,
                })
            })
            .collect();
        let recent: Vec<_> =
            rows.iter().copied().filter(|r| now - r.t_ms <= 90 * 86_400_000).collect();
        let skill = crate::judgment_trend::buckets(&recent, now, 90, 1)
            .first()
            .and_then(|b| b.bss);

        // Tool reliability — weighted by use, and only over tools with enough samples to mean anything.
        let tr = self.memory.tool_track_record().await.unwrap_or_default();
        let solid: Vec<_> = tr.iter().filter(|(_, _, n)| *n >= 3).collect();
        let tool_reliability = (!solid.is_empty()).then(|| {
            let total: u64 = solid.iter().map(|(_, _, n)| *n).sum();
            solid.iter().map(|(_, r, n)| r * (*n as f64)).sum::<f64>() / total.max(1) as f64
        });

        // Did anyone ever SEE what the drives noticed? discharged / (discharged + expired).
        let (discharged, expired) = self.memory.tension_outcome_counts().await.unwrap_or((0, 0));
        let seen = discharged + expired;
        let urge_discharge_rate = (seen > 0).then(|| discharged as f64 / seen as f64);

        let open_promises = self
            .memory
            .profile_get("courier_threads")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
            .map(|t| t.iter().filter(|x| x.get("status").and_then(|s| s.as_str()) == Some("open")).count())
            .unwrap_or(0);

        Snapshot {
            skill,
            graded: recent.len(),
            tool_reliability,
            tools_measured: solid.len(),
            urge_discharge_rate,
            open_promises,
        }
    }

    /// Record a merged self-build change together with the fitness AT THE TIME, so it can be looked
    /// at again once reality has had a chance to answer.
    pub async fn fitness_record_change(&self, sha: &str, goal: &str) {
        let snap = self.fitness_snapshot().await;
        let mut log: Vec<Value> = self
            .memory
            .profile_get("fitness_changes")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        log.push(serde_json::json!({
            "sha": sha,
            "goal": goal.chars().take(200).collect::<String>(),
            "at_ms": chrono::Utc::now().timestamp_millis(),
            "before": snap.scalar(),
            "graded_before": snap.graded,
            "graded_at_verdict": serde_json::Value::Null,
            "verdict_delta": serde_json::Value::Null,
        }));
        if log.len() > 200 {
            let cut = log.len() - 200;
            log.drain(..cut);
        }
        let _ = self
            .memory
            .profile_set("fitness_changes", &serde_json::to_string(&log).unwrap_or_default())
            .await;
    }

    /// THE CLOSED LOOP: grade merged changes old enough for reality to have answered. Until this
    /// existed, a merged PR was never evaluated again after CI went green — the loop had no idea
    /// whether anything it built ever helped. Run on the idle tick.
    pub async fn fitness_grade_due(&self) -> Vec<String> {
        let mut out = Vec::new();
        let wait_days: i64 =
            std::env::var("YM_FITNESS_GRADE_DAYS").ok().and_then(|s| s.parse().ok()).unwrap_or(14);
        let now = chrono::Utc::now().timestamp_millis();
        let mut log: Vec<Value> = self
            .memory
            .profile_get("fitness_changes")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if log.is_empty() {
            return out;
        }
        let after = self.fitness_snapshot().await;
        let mut changed = false;
        for row in log.iter_mut() {
            if !row.get("verdict_delta").map(|v| v.is_null()).unwrap_or(false) {
                continue; // already graded — immutable once written
            }
            let at = row.get("at_ms").and_then(|x| x.as_i64()).unwrap_or(0);
            let age_days = (now - at) / 86_400_000;
            if age_days < wait_days {
                continue;
            }
            let before = row.get("before").and_then(|x| x.as_f64());
            let d = match (before, after.scalar()) {
                (Some(b), Some(a)) => Some(a - b),
                _ => None,
            };
            row["verdict_delta"] = match d {
                Some(x) => serde_json::json!(x),
                None => serde_json::json!("unmeasurable"),
            };
            row["graded_at_verdict"] = serde_json::json!(after.graded);
            changed = true;
            let goal = row.get("goal").and_then(|x| x.as_str()).unwrap_or("");
            out.push(format!("[fitness] {}", render_verdict(goal, d, age_days)));
        }
        if changed {
            let _ = self
                .memory
                .profile_set("fitness_changes", &serde_json::to_string(&log).unwrap_or_default())
                .await;
        }
        out
    }

    /// `ym fitness` — the mind's real-world scoreboard plus how its own changes have fared.
    pub async fn fitness_report(&self) -> String {
        let snap = self.fitness_snapshot().await;
        let mut s = String::from("🎯 FITNESS — am I actually getting better, or just busier?\n\n");
        s.push_str(&snap.render_for_goal_prompt());
        let log: Vec<Value> = self
            .memory
            .profile_get("fitness_changes")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_default();
        let graded: Vec<&Value> =
            log.iter().filter(|r| !r.get("verdict_delta").map(|v| v.is_null()).unwrap_or(true)).collect();
        s.push_str(&format!(
            "\nSelf-build changes tracked: {} ({} graded, {} still ripening)\n",
            log.len(),
            graded.len(),
            log.len() - graded.len()
        ));
        for r in graded.iter().rev().take(5) {
            let goal = r.get("goal").and_then(|x| x.as_str()).unwrap_or("");
            let d = r.get("verdict_delta").and_then(|x| x.as_f64());
            s.push_str(&format!("  {}\n", render_verdict(goal, d, 0)));
        }
        s.push_str(
            "\nAttribution is ASSOCIATIONAL: several changes land between snapshots and the world \
             moves on its own. This says what happened around a change, never that the change caused it.",
        );
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(skill: Option<f64>, graded: usize, tool: Option<f64>, urge: Option<f64>) -> Snapshot {
        Snapshot {
            skill,
            graded,
            tool_reliability: tool,
            tools_measured: 4,
            urge_discharge_rate: urge,
            open_promises: 0,
        }
    }

    /// A fitness number that cannot be supported must not be invented. This is the whole difference
    /// between a metric and a decoration.
    #[test]
    fn refuses_to_fabricate_a_score() {
        assert!(snap(None, 0, Some(0.9), Some(0.9)).scalar().is_none(), "no skill => no score");
        assert!(delta(&snap(None, 0, None, None), &snap(Some(0.5), 40, None, None)).is_none());
    }

    #[test]
    fn skill_dominates_the_scalar() {
        // Health signals must not be able to paper over bad judgment.
        let good_judgment_bad_health = snap(Some(0.9), 40, Some(0.2), Some(0.1)).scalar().unwrap();
        let bad_judgment_great_health = snap(Some(-0.3), 40, Some(1.0), Some(1.0)).scalar().unwrap();
        assert!(
            good_judgment_bad_health > bad_judgment_great_health,
            "judgment must outweigh busywork metrics ({good_judgment_bad_health:.3} vs {bad_judgment_great_health:.3})"
        );
    }

    #[test]
    fn the_goal_prompt_names_the_weakness_not_just_the_number() {
        // The live reading on 2026-07-25 was skill -0.36 over 74 graded: worse than a base-rate guess.
        let p = snap(Some(-0.36), 74, Some(0.55), Some(0.05)).render_for_goal_prompt();
        assert!(p.contains("NEGATIVE"), "a negative skill must be named as the biggest problem: {p}");
        assert!(p.contains("never seen by anyone"), "a 5% discharge rate must read as waste: {p}");
        assert!(p.contains("MOVE one of these numbers"), "the prompt must steer at outcomes: {p}");
        // Unprovable skill should invite making it measurable rather than pretending.
        let thin = snap(None, 3, None, None).render_for_goal_prompt();
        assert!(thin.contains("NOT YET PROVABLE") && thin.contains("GRADED"), "{thin}");
    }

    #[test]
    fn verdicts_are_honest_about_attribution_and_direction() {
        assert!(render_verdict("g", Some(0.10), 14).contains("associational, not proof"));
        assert!(render_verdict("g", Some(-0.10), 14).contains("DEGRADED"));
        assert!(render_verdict("g", Some(0.0), 14).contains("flat"));
        assert!(render_verdict("g", None, 14).contains("not measurable"));
    }
}
