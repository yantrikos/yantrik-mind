//! handoff — a note from each self-build tick to the next one, so the loop is CONTINUOUS rather
//! than four amnesiac ticks a day.
//!
//! Today the goal generator is handed `git log --oneline -20`, which is MERGED COMMITS ONLY. It
//! therefore cannot see anything that did not merge: an abort, a draft left for a human, a
//! no-change run, or six consecutive GOAL-REJECTED ticks. It can propose the same doomed goal
//! forever and never know it already tried. Worse, it has no memory of its own INTENT — what it was
//! halfway through, what it deliberately deferred, what it learned from the attempt that failed.
//!
//! This is the same idea as the memory chain that carries a persona across sessions, applied one
//! level down: identity is continuity of the record, not of the process. A tick that leaves a note
//! for its successor is a loop with a thread through it; a tick that leaves nothing is a loop that
//! starts from zero every six hours and calls the result "autonomy".
//!
//! Two things are recorded, and the SECOND is the valuable one:
//!   · WHAT HAPPENED — goal + outcome, deterministic, always written even when the tick failed.
//!     A failure that leaves no trace is the one most likely to be repeated.
//!   · WHAT I'D DO NEXT — an optional first-person note carrying intent, which a commit log cannot.

use serde_json::Value;

/// How many past ticks the next one is shown. Enough to see a pattern, few enough to stay read.
pub(crate) const WINDOW: usize = 6;
/// Same goal attempted this many times without ever merging ⇒ stop proposing it.
pub(crate) const STUCK_AFTER: usize = 3;

/// Did this tick actually change the system?
pub(crate) fn merged(outcome: &str) -> bool {
    outcome.eq_ignore_ascii_case("MERGED")
}

/// Normalise a goal for repeat-detection: lowercase content words, so trivial rewordings of the
/// same doomed idea still collide. Without this, "Fix X" and "fix the X" read as different goals
/// and the loop congratulates itself on variety while retrying the same thing.
pub(crate) fn goal_key(goal: &str) -> String {
    let mut w: Vec<String> = goal
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|x| x.len() >= 4)
        .take(12)
        .map(|x| x.to_string())
        .collect();
    w.sort();
    w.dedup();
    w.join(" ")
}

/// Goals attempted `STUCK_AFTER`+ times with no merge — the loop is spinning on these.
pub(crate) fn stuck_goals(entries: &[Value]) -> Vec<String> {
    let mut tries: std::collections::HashMap<String, (usize, bool, String)> = Default::default();
    for e in entries {
        let goal = e.get("goal").and_then(|x| x.as_str()).unwrap_or("");
        if goal.is_empty() {
            continue;
        }
        let ok = merged(e.get("outcome").and_then(|x| x.as_str()).unwrap_or(""));
        let slot = tries.entry(goal_key(goal)).or_insert((0, false, goal.to_string()));
        slot.0 += 1;
        slot.1 |= ok;
    }
    let mut out: Vec<String> = tries
        .values()
        .filter(|(n, ever_merged, _)| *n >= STUCK_AFTER && !*ever_merged)
        .map(|(n, _, g)| format!("{} (tried {n}x, never merged)", g.chars().take(80).collect::<String>()))
        .collect();
    out.sort();
    out
}

/// The block the next tick reads. Leads with the note (intent) and ends with an explicit
/// instruction, because a log the model merely *sees* is weaker than one it is told what to do with.
pub(crate) fn render(entries: &[Value]) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let recent: Vec<&Value> = entries.iter().rev().take(WINDOW).collect();
    let mut s = String::from("WHAT MY PREVIOUS TICKS DID (including the ones that did NOT merge — the commit log hides these):\n");
    for e in &recent {
        let when = e.get("when").and_then(|x| x.as_str()).unwrap_or("");
        let goal: String = e.get("goal").and_then(|x| x.as_str()).unwrap_or("").chars().take(90).collect();
        let outcome = e.get("outcome").and_then(|x| x.as_str()).unwrap_or("?");
        s.push_str(&format!("- [{when}] {outcome}: {goal}\n"));
        if let Some(n) = e.get("note").and_then(|x| x.as_str()).filter(|n| !n.trim().is_empty()) {
            s.push_str(&format!("    note to self: {}\n", n.chars().take(240).collect::<String>()));
        }
    }
    let stuck = stuck_goals(entries);
    if !stuck.is_empty() {
        s.push_str("\nI AM SPINNING ON THESE — do NOT propose them again unless you take a genuinely different approach:\n");
        for g in stuck.iter().take(4) {
            s.push_str(&format!("- {g}\n"));
        }
    }
    s.push_str(
        "\nContinue the thread: if a previous tick left unfinished intent, prefer finishing it over starting \
         something new. Do not re-attempt anything above that failed, in the same way it failed.\n",
    );
    s
}

impl super::ConversationEngine {
    async fn load_handoff(&self) -> Vec<Value> {
        self.memory
            .profile_get("selfbuild_handoff")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Record what this tick did. Called at the END of every self-build run — including the runs
    /// that aborted, drafted, or produced nothing, because those are exactly the ones the commit log
    /// cannot show and the next tick most needs to know about.
    pub async fn handoff_write(&self, goal: &str, outcome: &str, note: &str) -> String {
        let mut log = self.load_handoff().await;
        log.push(serde_json::json!({
            "when": chrono::Utc::now().format("%Y-%m-%dT%H:%MZ").to_string(),
            "goal": goal.chars().take(220).collect::<String>(),
            "outcome": outcome,
            "note": note.chars().take(400).collect::<String>(),
        }));
        // Bounded: a handoff that grows forever stops being read, which is the same as not existing.
        if log.len() > 40 {
            let cut = log.len() - 40;
            log.drain(..cut);
        }
        let _ = self
            .memory
            .profile_set("selfbuild_handoff", &serde_json::to_string(&log).unwrap_or_default())
            .await;
        format!("handoff recorded ({outcome})")
    }

    /// The block injected into the next tick's goal prompt.
    pub async fn handoff_prompt(&self) -> String {
        render(&self.load_handoff().await)
    }

    /// `ym handoff` — what the loop has been telling itself.
    pub async fn handoff_report(&self) -> String {
        let log = self.load_handoff().await;
        if log.is_empty() {
            return "🧵 Self-build handoff: no ticks recorded yet — the next one will leave the first note.".into();
        }
        let merged_n = log.iter().filter(|e| merged(e.get("outcome").and_then(|x| x.as_str()).unwrap_or(""))).count();
        format!(
            "🧵 Self-build handoff — the thread between ticks\n\n{}\n{} tick(s) recorded, {merged_n} merged.",
            render(&log),
            log.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn e(goal: &str, outcome: &str, note: &str) -> Value {
        json!({ "when": "2026-07-29T06:00Z", "goal": goal, "outcome": outcome, "note": note })
    }

    #[test]
    fn failures_are_visible_because_the_commit_log_hides_them() {
        let log = vec![
            e("Add a retention cap to the spool", "ABORT-COMPILE", ""),
            e("Normalise belief text", "MERGED", ""),
        ];
        let s = render(&log);
        assert!(s.contains("ABORT-COMPILE"), "a failed tick must be visible to the next one: {s}");
        assert!(s.contains("the commit log hides these"), "the point is stated: {s}");
    }

    /// The loop must notice it is spinning. Six identical GOAL-REJECTED ticks actually happened
    /// (2026-07-27/28) and nothing in the system could see the pattern.
    #[test]
    fn repeated_unmerged_goals_are_called_out() {
        let log = vec![
            e("Fix the contradiction detector", "DRAFT-FOR-HUMAN", ""),
            e("fix THE contradiction detector.", "DRAFT-FOR-HUMAN", ""),
            e("Fix the contradiction  detector", "ABORT-COMPILE", ""),
        ];
        let stuck = stuck_goals(&log);
        assert_eq!(stuck.len(), 1, "trivial rewordings must collide, not read as variety: {stuck:?}");
        assert!(stuck[0].contains("tried 3x, never merged"));
        assert!(render(&log).contains("I AM SPINNING ON THESE"));
    }

    #[test]
    fn a_goal_that_eventually_merged_is_not_stuck() {
        let log = vec![
            e("Add retention to the spool", "ABORT-COMPILE", ""),
            e("Add retention to the spool", "DRAFT-FOR-HUMAN", ""),
            e("Add retention to the spool", "MERGED", ""),
        ];
        assert!(stuck_goals(&log).is_empty(), "success clears the spinning flag");
    }

    #[test]
    fn intent_carries_where_a_commit_message_cannot() {
        let log = vec![e(
            "Bound the escrow ledger",
            "MERGED",
            "Capped it at 100 but the pruning rule is arbitrary — next tick should derive it from actual read volume.",
        )];
        let s = render(&log);
        assert!(s.contains("note to self:"), "{s}");
        assert!(s.contains("derive it from actual read volume"), "unfinished intent survives: {s}");
        assert!(s.contains("prefer finishing it over starting something new"));
    }

    #[test]
    fn the_window_is_bounded_and_shows_the_newest() {
        let log: Vec<Value> = (0..20).map(|i| e(&format!("goal {i}"), "MERGED", "")).collect();
        let s = render(&log);
        assert!(s.contains("goal 19"), "newest shown");
        assert!(!s.contains("goal 0"), "oldest dropped — a note nobody reads is not a note");
        assert_eq!(s.matches("] MERGED:").count(), WINDOW);
    }
}
