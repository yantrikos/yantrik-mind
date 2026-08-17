//! The Reflex Arc (One Mind vision, organ #4) — recurring CORRECTIONS become
//! self-build goals behind the six-condition gate. The regret wire (dream.rs)
//! covers asks the mind failed to anticipate; this is the other half of the
//! arc: answers the owner had to correct, which until now taught nothing
//! durable beyond a counter. Two misses on one subject is a capability gap,
//! not bad luck.
//!
//! The gate, verbatim from the vision — clustered misses · a reproduced
//! failing fixture · a named module · predicted metric movement · a rollback
//! condition · post-deploy measurement — and its hard rule: **no repro, no
//! build.** Four conditions are derivable from the cluster itself; the fixture
//! is not derivable and must be attached (`ym reflex fixture <id> <test>`)
//! before a draft may touch the build queue. A draft that never earns its
//! fixture sits visibly in `ym reflex` forever — an honest backlog beats a
//! speculative build.

use super::*;

/// A drafted reflex goal: the six conditions as fields, so the gate is a
/// structural check rather than prose hygiene.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReflexDraft {
    pub id: u64,
    /// The cluster key — the informative word the corrections share.
    pub subject: String,
    /// Condition 1 — clustered misses: the correction pairs, verbatim (>= 2).
    pub evidence: Vec<String>,
    /// Condition 3 — the named module the fix belongs to.
    pub module: String,
    /// Condition 4 — the predicted metric movement, with its baseline.
    pub metric: String,
    /// Condition 5 — the rollback condition.
    pub rollback: String,
    /// Condition 6 — the post-deploy measurement.
    pub measure: String,
    /// Condition 2 — the reproduced failing fixture. Not derivable; attached
    /// explicitly. None = the gate holds the draft out of the queue.
    pub fixture: Option<String>,
    /// "draft" | "queued".
    pub status: String,
    pub ts: i64,
}

/// Which of the six conditions are still unmet. Empty = the gate opens.
pub(crate) fn unmet_conditions(d: &ReflexDraft) -> Vec<&'static str> {
    let mut out = Vec::new();
    if d.evidence.len() < 2 {
        out.push("clustered misses (need >= 2 corrections on the subject)");
    }
    // The fixture must at least NAME a test; whether it is genuinely red is the
    // build pipeline's job to verify before fixing (stated in the goal line).
    if !d.fixture.as_deref().is_some_and(|f| f.contains("test")) {
        out.push("reproduced failing fixture (no repro, no build)");
    }
    if !d.module.contains("mind-") {
        out.push("named module");
    }
    if d.metric.is_empty() {
        out.push("predicted metric movement");
    }
    if d.rollback.is_empty() {
        out.push("rollback condition");
    }
    if d.measure.is_empty() {
        out.push("post-deploy measurement");
    }
    out
}

/// The single line the build queue receives once the gate opens — every
/// condition rides inside it, because the queue file is line-oriented and the
/// builder must see the whole contract.
pub(crate) fn goal_line(d: &ReflexDraft) -> String {
    let brief: Vec<String> = d.evidence.iter().take(3).map(|e| e.chars().take(90).collect()).collect();
    format!(
        "REFLEX ({} corrections on \"{}\"): the owner had to correct these answers: {}. \
         Fix in {}. Fixture {} — verify it is RED before the fix and green after; abort if it does not reproduce. \
         Predicted metric: {}. Rollback: {}. Post-deploy measurement: {}.",
        d.evidence.len(),
        d.subject,
        brief.join(" | "),
        d.module,
        d.fixture.as_deref().unwrap_or("(missing)"),
        d.metric,
        d.rollback,
        d.measure,
    )
}

const STOPWORDS: &[&str] = &[
    "about", "after", "again", "answer", "asked", "before", "being", "could", "didn", "doesn", "should",
    "their", "there", "these", "thing", "think", "wasn", "where", "which", "would", "wrong", "actually",
];

fn informative_words(text: &str) -> std::collections::BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 5 && !STOPWORDS.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Deterministic clustering: corrections sharing an informative word cluster
/// under that word; the most-shared word wins first, each correction joins one
/// cluster only. Returns (subject, member texts), largest first.
pub(crate) fn cluster_corrections(rows: &[String]) -> Vec<(String, Vec<String>)> {
    let sets: Vec<std::collections::BTreeSet<String>> = rows.iter().map(|r| informative_words(r)).collect();
    let mut by_word: std::collections::BTreeMap<String, Vec<usize>> = std::collections::BTreeMap::new();
    for (i, set) in sets.iter().enumerate() {
        for w in set {
            by_word.entry(w.clone()).or_default().push(i);
        }
    }
    let mut candidates: Vec<(String, Vec<usize>)> = by_word.into_iter().filter(|(_, is)| is.len() >= 2).collect();
    // Largest cluster first; among words spanning the same corrections, the longer
    // word is the more specific subject ("standup" over "moved"); alpha last, so
    // the whole ordering is deterministic.
    candidates.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(b.0.len().cmp(&a.0.len())).then(a.0.cmp(&b.0)));
    let mut consumed = vec![false; rows.len()];
    let mut out = Vec::new();
    for (word, idxs) in candidates {
        let members: Vec<usize> = idxs.into_iter().filter(|i| !consumed[*i]).collect();
        if members.len() < 2 {
            continue;
        }
        for &i in &members {
            consumed[i] = true;
        }
        out.push((word, members.into_iter().map(|i| rows[i].clone()).collect()));
    }
    out
}

/// Draft a reflex goal from a cluster: the four derivable conditions filled
/// from the cluster itself, the fixture left honestly empty.
pub(crate) fn draft_from_cluster(id: u64, subject: &str, evidence: Vec<String>, now_ms: i64) -> ReflexDraft {
    let n = evidence.len();
    ReflexDraft {
        id,
        subject: subject.to_string(),
        evidence,
        module: "crates/mind-conversation (the turn pipeline that produced the corrected answers)".into(),
        metric: format!(
            "ledger corrections mentioning \"{subject}\" in the 14 days after deploy fall below the cluster baseline of {n}"
        ),
        rollback: "revert the merge if the fixture regresses or corrections on this subject rise within 14 days of deploy".into(),
        measure: format!("ym scoreboard ANSWERS panel + ledger rows filtered to \"{subject}\", read at deploy+14d"),
        fixture: None,
        status: "draft".into(),
        ts: now_ms,
    }
}

impl super::ConversationEngine {
    async fn reflex_log(&self) -> Vec<ReflexDraft> {
        self.memory
            .profile_get("reflex_log")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    async fn save_reflex_log(&self, log: &[ReflexDraft]) {
        let start = log.len().saturating_sub(40);
        let _ = self
            .memory
            .profile_set("reflex_log", &serde_json::to_string(&log[start..]).unwrap_or_default())
            .await;
    }

    /// Once per night, per-date — same shape as the narrative gate, own key.
    pub async fn reflex_due(&self) -> bool {
        use chrono::Timelike;
        let today = local_now();
        if !(2..=6).contains(&today.hour()) {
            return false;
        }
        let date = today.format("%Y-%m-%d").to_string();
        let last = self.memory.profile_get("reflex_last_date").await.ok().flatten().unwrap_or_default();
        last != date
    }

    /// The nightly arc: cluster the ledger's corrections, draft what's new,
    /// enqueue any draft whose gate has opened. Returns short log lines.
    pub async fn reflex_tick(&self) -> String {
        let date = local_now().format("%Y-%m-%d").to_string();
        let _ = self.memory.profile_set("reflex_last_date", &date).await;
        let ledger = self.ledger().await;
        let corrections: Vec<String> = ledger
            .iter()
            .filter(|e| e["outcome"].as_str() == Some("corrected"))
            .map(|e| {
                format!(
                    "{} -> {}",
                    e["what"].as_str().unwrap_or("?"),
                    e["lesson"].as_str().unwrap_or("?")
                )
            })
            .collect();
        let mut log = self.reflex_log().await;
        let mut out: Vec<String> = Vec::new();
        let now_ms = chrono::Utc::now().timestamp_millis();
        for (subject, evidence) in cluster_corrections(&corrections) {
            if log.iter().any(|d| d.subject == subject) {
                continue; // idempotent per subject — the log remembers, wired or not
            }
            let id = log.iter().map(|d| d.id).max().unwrap_or(0) + 1;
            let draft = draft_from_cluster(id, &subject, evidence, now_ms);
            out.push(format!(
                "drafted #{id} \"{subject}\" ({} corrections) — gate holds: {}",
                draft.evidence.len(),
                unmet_conditions(&draft).join("; ")
            ));
            log.push(draft);
        }
        // Any draft whose gate has opened (fixture attached since) gets queued.
        for d in log.iter_mut() {
            if d.status == "draft" && unmet_conditions(d).is_empty() {
                if Self::enqueue_reflex_goal(d) {
                    d.status = "queued".into();
                    out.push(format!("queued #{} \"{}\" into the self-build queue", d.id, d.subject));
                }
            }
        }
        self.save_reflex_log(&log).await;
        if out.is_empty() {
            "reflex: no new correction clusters; no gates opened.".to_string()
        } else {
            format!("reflex: {}", out.join(" · "))
        }
    }

    /// Append to the build queue (bounded, dedup-by-subject) — the same file
    /// and courtesy rules the regret wire uses.
    fn enqueue_reflex_goal(d: &ReflexDraft) -> bool {
        let goals_path = std::path::PathBuf::from(
            std::env::var("YM_SELFBUILD_GOALS").unwrap_or_else(|_| {
                format!("{}/selfbuild-goals.txt", std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".into()))
            }),
        );
        let Ok(cur) = std::fs::read_to_string(&goals_path) else { return false };
        if cur.contains(&format!("\"{}\"", d.subject)) || cur.lines().filter(|l| !l.trim().is_empty()).count() >= 8 {
            return false;
        }
        let mut cur = cur;
        cur.push_str(&format!("{}\n", goal_line(d)));
        std::fs::write(&goals_path, cur).is_ok()
    }

    /// `ym reflex` — the drafts, each with its gate state made visible.
    pub async fn reflex_report(&self) -> String {
        let log = self.reflex_log().await;
        if log.is_empty() {
            return "No reflex drafts — no correction has clustered yet. The arc drafts when a subject is corrected twice.".into();
        }
        let mut out = String::from("REFLEX ARC (corrections → gated self-build goals; no repro, no build):\n");
        for d in log.iter().rev().take(12) {
            let unmet = unmet_conditions(d);
            let gate = if d.status == "queued" {
                "QUEUED".to_string()
            } else if unmet.is_empty() {
                "gate OPEN (queues tonight)".to_string()
            } else {
                format!("gate holds: {}", unmet.join("; "))
            };
            out.push_str(&format!(
                "#{} \"{}\" — {} corrections · {}\n",
                d.id,
                d.subject,
                d.evidence.len(),
                gate
            ));
        }
        out.push_str("`ym reflex fixture <id> <test path>` attaches the reproduced failing fixture.");
        out
    }

    /// `ym reflex fixture <id> <test>` — attach condition 2; queue immediately
    /// if that was the last unmet condition.
    pub async fn reflex_attach_fixture(&self, id: u64, fixture: &str) -> String {
        let mut log = self.reflex_log().await;
        let Some(d) = log.iter_mut().find(|d| d.id == id) else {
            return format!("No reflex draft #{id}.");
        };
        d.fixture = Some(fixture.trim().to_string());
        let unmet = unmet_conditions(d);
        let reply = if !unmet.is_empty() {
            format!("Fixture noted on #{id}, but the gate still holds: {}", unmet.join("; "))
        } else if d.status == "queued" {
            format!("#{id} was already queued.")
        } else if Self::enqueue_reflex_goal(d) {
            d.status = "queued".into();
            format!("#{id} \"{}\" — gate open, queued into the self-build loop.", d.subject)
        } else {
            format!("#{id} gate is open but the queue refused it (full, duplicate subject, or missing file).")
        };
        self.save_reflex_log(&log).await;
        reply
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<String> {
        vec![
            "the meeting is at 4 -> no, the standup moved to 5".into(),
            "standup is on Monday -> wrong, standup moved to Tuesday".into(),
            "the sky is blue -> actually it was overcast".into(),
        ]
    }

    #[test]
    fn corrections_cluster_by_shared_informative_word() {
        let cs = cluster_corrections(&rows());
        assert_eq!(cs.len(), 1, "one real cluster: {cs:?}");
        assert_eq!(cs[0].0, "standup");
        assert_eq!(cs[0].1.len(), 2);
        // Stop/short words never become subjects, singletons never cluster.
        assert!(!cs.iter().any(|(s, _)| s == "wrong" || s == "overcast"), "{cs:?}");
    }

    #[test]
    fn no_repro_no_build_is_structural() {
        let d = draft_from_cluster(1, "standup", vec!["a -> b".into(), "c -> d".into()], 0);
        let unmet = unmet_conditions(&d);
        assert_eq!(unmet, vec!["reproduced failing fixture (no repro, no build)"], "only the fixture may hold a derived draft: {unmet:?}");
        // A fixture that names no test does not satisfy the repro condition.
        let mut bogus = d.clone();
        bogus.fixture = Some("just trust me".into());
        assert!(!unmet_conditions(&bogus).is_empty());
        // A named test opens the gate.
        let mut ok = d;
        ok.fixture = Some("cognitive::tests::standup_time_is_grounded".into());
        assert!(unmet_conditions(&ok).is_empty());
    }

    #[test]
    fn the_goal_line_carries_all_six_conditions() {
        let mut d = draft_from_cluster(2, "standup", vec!["a -> b".into(), "c -> d".into()], 0);
        d.fixture = Some("cognitive::tests::standup_time_is_grounded".into());
        let line = goal_line(&d);
        for must in [
            "2 corrections on \"standup\"",             // 1: clustered misses
            "standup_time_is_grounded",                  // 2: fixture
            "RED before the fix",                        //    …verified red first
            "crates/mind-conversation",                  // 3: named module
            "fall below the cluster baseline of 2",      // 4: predicted metric
            "Rollback: revert the merge",                // 5: rollback condition
            "deploy+14d",                                // 6: post-deploy measurement
        ] {
            assert!(line.contains(must), "goal line missing {must:?}: {line}");
        }
        assert!(!line.contains('\n'), "the queue file is line-oriented");
    }
}
