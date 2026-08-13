//! followthrough — closing the loop on a commitment instead of carrying it forever.
//!
//! # The behaviour this fixes
//!
//! A reminder tied to an occasion — "order the Rosefield watch for her birthday" — had no ending. The
//! escalating nudges fire once each and stop, which is right. But the task stayed OPEN, so it kept
//! appearing in the grounding as a live commitment, and the mind kept offering to help with it. Three
//! weeks after the birthday it was still asking whether to finalise the order.
//!
//! That is worse than forgetting. Forgetting is a gap; offering to help with something that already
//! happened says the mind is not tracking reality, and every such offer costs the operator a moment to
//! dismiss. The nagging is not a proactivity bug — it is a MISSING LIFECYCLE.
//!
//! # The lifecycle
//!
//! ```text
//! Live ──due──▶ JustDue ──grace──▶ NeedsClosure ──asked once──▶ Closed
//!                                        │
//!                                        └── never answered ──▶ Closed (silently)
//! ```
//!
//! `NeedsClosure` earns exactly ONE question, and it is a genuine one rather than another nudge:
//! "you never told me what you ended up giving her — how did it go?" That is the only phrasing that is
//! both useful (it closes an information gap the mind actually has) and finite. Then the thread closes
//! whether or not it was answered, because a question nobody answers twice is a question that should
//! not be asked twice.
//!
//! # Why the classifier is pure
//!
//! Every decision here is a subtraction on two timestamps and one boolean. Putting it behind a pure
//! function means the awkward cases — a task due today, a task with no deadline at all, one already
//! asked about — are testable directly instead of reachable only by waiting three weeks.

use serde::{Deserialize, Serialize};

use mind_types::Task;

/// Days after the deadline during which an overdue nudge is still the right response.
///
/// Two days, because "you missed this" is useful the morning after and merely annoying a fortnight
/// later — by then the question is what happened, not a reminder that it was due.
pub(crate) const GRACE_DAYS: i64 = 2;

/// Days after the closure question at which the thread closes regardless of answer.
pub(crate) const CLOSE_AFTER_DAYS: i64 = 3;

/// Where a commitment sits in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    /// Ahead of its deadline, or has none. Carry it.
    Live,
    /// Just past due — an overdue nudge is still apt.
    JustDue { days_over: i64 },
    /// Long past due and never resolved. Worth ONE question about what happened.
    NeedsClosure { days_over: i64 },
    /// Asked and unanswered, or so old the answer no longer matters. Close it.
    Abandoned { days_over: i64 },
}

impl ThreadState {
    /// Should this still be carried in the grounding as a live commitment?
    ///
    /// This is the property that stops the nagging: a stale thread leaves the prompt, so the model
    /// cannot offer help with it, because it no longer knows about it as something outstanding.
    pub fn is_carried(&self) -> bool {
        matches!(self, Self::Live | Self::JustDue { .. })
    }

    /// Should the runtime close this task now?
    pub fn should_close(&self) -> bool {
        matches!(self, Self::Abandoned { .. })
    }
}

/// Classify one commitment.
///
/// `asked_ms` is when the closure question was put (None = never asked). Pure: no clock, no store.
pub fn classify(task: &Task, now_ms: i64, deadline_ms: Option<i64>, asked_ms: Option<i64>) -> ThreadState {
    // No deadline means no occasion to be past. An open-ended intention ("read more") is not stale
    // just because time passed, and closing those would delete the user's own standing goals.
    let Some(dl) = deadline_ms.or_else(|| task.due_ms.map(|m| m as i64)) else {
        return ThreadState::Live;
    };
    let days_over = (now_ms - dl) / 86_400_000;
    if days_over < 0 {
        return ThreadState::Live;
    }
    if days_over <= GRACE_DAYS {
        return ThreadState::JustDue { days_over };
    }
    match asked_ms {
        // Asked, and long enough ago that a reply was not coming.
        Some(asked) if (now_ms - asked) / 86_400_000 >= CLOSE_AFTER_DAYS => ThreadState::Abandoned { days_over },
        // Asked recently — waiting, not nagging.
        Some(_) => ThreadState::Live,
        None => ThreadState::NeedsClosure { days_over },
    }
}

/// The one question a stale thread earns.
///
/// Phrased as a genuine information gap rather than a reminder, because that is the difference between
/// closing a loop and nagging. The mind really does not know how this turned out, and knowing is worth
/// something: it is the outcome that makes the next gift, the next deadline, better advised.
pub fn closure_question(description: &str, days_over: i64) -> String {
    let when = match days_over {
        d if d < 10 => "last week".to_string(),
        d if d < 45 => format!("about {} weeks ago", (d / 7).max(1)),
        d => format!("about {} months ago", (d / 30).max(1)),
    };
    format!(
        "Something I never closed out \u{2014} \u{201c}{}\u{201d} was due {when}, and you never told me how it \
         went. Did it happen? If it is not a thing any more, say so and I will drop it.",
        description.trim()
    )
}

/// A DEADLINE in a task description, resolved without rolling forward a year.
///
/// `parse_text_date_ms` rolls a past date to next year, which is exactly right for a recurring
/// occasion: a birthday on 23 July, read in August, means next July. It is exactly wrong for a
/// deadline. "Order the watch before July 17th", read in August, resolved to July of NEXT year — so
/// the task sat 340 days in the future, never became overdue, never triggered a nudge, and was carried
/// as live work indefinitely. That single line is why the nagging survived the first fix.
///
/// So a deadline resolves to THIS year, past or not. The one exception is a wrap-around: a date more
/// than six months behind is more likely the coming one ("January 5th" read in December), because
/// `Task` carries no creation timestamp to disambiguate with. Six months is the only threshold that
/// splits those two readings without a third piece of information.
pub fn parse_deadline_ms(text: &str, today: &chrono::DateTime<chrono::FixedOffset>) -> Option<i64> {
    use chrono::Datelike;
    const MONTHS: [(&str, u32); 12] = [
        ("january", 1), ("february", 2), ("march", 3), ("april", 4), ("may", 5), ("june", 6),
        ("july", 7), ("august", 8), ("september", 9), ("october", 10), ("november", 11), ("december", 12),
    ];
    let low = text.to_lowercase();
    for (name, m) in MONTHS {
        for pat in [name, &name[..3]] {
            let mut start = 0;
            while let Some(pos) = low[start..].find(pat) {
                let at = start + pos;
                let end = at + pat.len();
                let before_ok = at == 0 || !low.as_bytes()[at - 1].is_ascii_alphabetic();
                let after_ok = low[end..].chars().next().map(|c| !c.is_ascii_alphabetic()).unwrap_or(false);
                if before_ok && after_ok {
                    let digits: String =
                        low[end..].trim_start().chars().take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(d) = digits.parse::<u32>() {
                        if (1..=31).contains(&d) {
                            let this_year = chrono::NaiveDate::from_ymd_opt(today.year(), m, d)?;
                            let behind = (today.date_naive() - this_year).num_days();
                            let nd = if behind > 183 {
                                chrono::NaiveDate::from_ymd_opt(today.year() + 1, m, d)?
                            } else {
                                this_year
                            };
                            let ts = nd.and_hms_opt(12, 0, 0)?.and_local_timezone(*today.offset()).single()?;
                            return Some(ts.timestamp_millis());
                        }
                    }
                }
                start = end;
            }
        }
    }
    None
}

/// Did the user just tell us to stop tracking something?
///
/// Deliberately narrow. "I'm not doing that any more" must close a thread, but an ordinary sentence
/// containing the word "stop" must not — a false positive silently deletes a commitment the user still
/// holds, which is the worse failure by far.
pub fn is_stop_tracking(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    const PHRASES: &[&str] = &[
        "stop tracking",
        "don't track",
        "dont track",
        "stop reminding",
        "no longer tracking",
        "not tracking anymore",
        "not tracking any more",
        "not tracking that",
        "forget about that",
        "drop it",
        "drop that",
        "no longer relevant",
        "cancel that reminder",
        "already done",
        "already did",
        "it's done",
        "its done",
        "that's done",
        "thats done",
    ];
    PHRASES.iter().any(|p| t.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;

    fn task(desc: &str, due: Option<i64>) -> Task {
        Task {
            id: "t1".into(),
            description: desc.into(),
            status: "pending".into(),
            priority: "medium".into(),
            due_ms: due.map(|d| d as u64),
        }
    }

    #[test]
    fn a_future_commitment_is_live() {
        let now = 100 * DAY;
        let t = task("order the watch", Some(now + 5 * DAY));
        assert_eq!(classify(&t, now, None, None), ThreadState::Live);
        assert!(classify(&t, now, None, None).is_carried());
    }

    /// The morning after is a nudge, not an interrogation.
    #[test]
    fn just_past_due_is_still_a_nudge() {
        let now = 100 * DAY;
        let t = task("order the watch", Some(now - DAY));
        let s = classify(&t, now, None, None);
        assert_eq!(s, ThreadState::JustDue { days_over: 1 });
        assert!(s.is_carried(), "a fresh miss is still worth carrying");
        assert!(!s.should_close());
    }

    /// THE BUG. Three weeks after the birthday the mind was still offering to finalise the gift order.
    /// Past the grace window a thread stops being carried, so it leaves the prompt and the model cannot
    /// offer help with something that already happened.
    #[test]
    fn a_long_dead_thread_stops_being_carried() {
        let now = 100 * DAY;
        let t = task("order the Rosefield watch for her birthday", Some(now - 21 * DAY));
        let s = classify(&t, now, None, None);
        assert_eq!(s, ThreadState::NeedsClosure { days_over: 21 });
        assert!(!s.is_carried(), "a three-week-old commitment must not read as outstanding work");
    }

    /// It earns exactly one question, and the question is about the OUTCOME — which is information the
    /// mind genuinely lacks — not another reminder of a date that has gone.
    #[test]
    fn the_closure_question_asks_what_happened_and_offers_an_exit() {
        let q = closure_question("order the Rosefield watch for her birthday", 21);
        assert!(q.contains("Rosefield"), "{q}");
        assert!(q.contains("how it went") || q.contains("Did it happen"), "it asks about the outcome: {q}");
        assert!(q.contains("drop it"), "and offers a way out, so it need never be asked again: {q}");
        assert!(!q.contains("OVERDUE"), "it is not another nudge: {q}");
    }

    /// Asked and answered by silence: close it. A question nobody answers twice should not be asked
    /// twice.
    #[test]
    fn an_unanswered_question_closes_the_thread() {
        let now = 100 * DAY;
        let t = task("order the watch", Some(now - 21 * DAY));
        // Asked yesterday — still waiting, not nagging.
        assert_eq!(classify(&t, now, None, Some(now - DAY)), ThreadState::Live);
        // Asked a week ago and never answered.
        let s = classify(&t, now, None, Some(now - 7 * DAY));
        assert_eq!(s, ThreadState::Abandoned { days_over: 21 });
        assert!(s.should_close());
        assert!(!s.is_carried());
    }

    /// An open-ended intention is not stale merely because time passed. Closing those would delete the
    /// user's own standing goals, which is a far worse failure than carrying one too long.
    #[test]
    fn a_commitment_with_no_deadline_is_never_stale() {
        let t = task("read more books", None);
        assert_eq!(classify(&t, 10_000 * DAY, None, None), ThreadState::Live);
        assert!(classify(&t, 10_000 * DAY, None, Some(0)).is_carried());
    }

    /// A deadline parsed out of the TEXT ("by July 17th") counts, since that is how most of these are
    /// actually recorded — the due_ms field is often empty.
    #[test]
    fn a_text_parsed_deadline_counts_as_the_deadline() {
        let now = 100 * DAY;
        let t = task("order the watch by July 17th", None);
        assert_eq!(
            classify(&t, now, Some(now - 30 * DAY), None),
            ThreadState::NeedsClosure { days_over: 30 }
        );
    }

    #[test]
    fn stop_tracking_is_recognised() {
        for s in [
            "stop tracking that",
            "I'm not tracking that anymore",
            "don't track the gift thing",
            "forget about that",
            "already did it",
            "that's done",
            "drop it",
        ] {
            assert!(is_stop_tracking(s), "should close: {s}");
        }
    }

    /// A false positive silently deletes a commitment the user still holds, which is much worse than
    /// carrying one an extra day. So ordinary sentences must not trip it.
    #[test]
    fn ordinary_sentences_do_not_close_threads() {
        for s in [
            "what's on my list?",
            "stop the music",
            "did the deployment finish?",
            "I need to track my spending better",
            "don't forget the milk",
            "is it done yet?",
            "when is it done?",
        ] {
            assert!(!is_stop_tracking(s), "must NOT close: {s}");
        }
    }
}

impl super::ConversationEngine {
    /// Pairs the operator has ruled are NOT duplicates of each other (`consolidate … except <id>`).
    pub(crate) async fn not_duplicate_pairs(&self) -> std::collections::HashSet<String> {
        self.memory
            .profile_get(super::NOT_DUPLICATE_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default()
    }

    /// Record a standing veto so the matcher stops re-proposing a pair the operator rejected.
    pub(crate) async fn remember_not_duplicate(&self, pairs: &[String]) {
        if pairs.is_empty() {
            return;
        }
        let mut all = self.not_duplicate_pairs().await;
        all.extend(pairs.iter().cloned());
        let mut list: Vec<String> = all.into_iter().collect();
        list.sort(); // stable on disk, so a diff of the profile is readable
        let _ = self
            .memory
            .profile_set(super::NOT_DUPLICATE_KEY, &serde_json::to_string(&list).unwrap_or_default())
            .await;
    }

    /// When each thread's closure question was asked. Keyed by task id.
    pub(crate) async fn closure_asks(&self) -> serde_json::Map<String, serde_json::Value> {
        self.memory
            .profile_get("closure_asks")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    async fn set_closure_asks(&self, m: serde_json::Map<String, serde_json::Value>) {
        let _ = self
            .memory
            .profile_set("closure_asks", &serde_json::Value::Object(m).to_string())
            .await;
    }

    /// The tick half of the lifecycle: ask about what is unresolved, close what is finished.
    ///
    /// Returns the closure questions to send — at most `MAX_ASKS_PER_TICK`, because a mind that opens
    /// six loose ends at once has produced a chore rather than closed a loop.
    pub async fn close_stale_threads(&self) -> Vec<String> {
        /// One at a time. Closure is a conversation, and six questions is an audit.
        const MAX_ASKS_PER_TICK: usize = 1;

        let (open, _) = self.open_and_internal_tasks().await;
        if open.is_empty() {
            return Vec::new();
        }
        let today = super::local_now();
        let now = today.timestamp_millis();
        let mut asked = self.closure_asks().await;
        let mut out = Vec::new();
        let mut changed = false;

        for t in &open {
            let dl = parse_deadline_ms(&t.description, &today);
            let prior = asked.get(&t.id).and_then(|v| v.as_i64());
            match classify(t, now, dl, prior) {
                ThreadState::NeedsClosure { days_over } if out.len() < MAX_ASKS_PER_TICK => {
                    asked.insert(t.id.clone(), serde_json::json!(now));
                    changed = true;
                    out.push(closure_question(&t.description, days_over));
                }
                ThreadState::Abandoned { .. } => {
                    // Silently. Having already asked once, asking again to announce the closing would
                    // be a second interruption about something the user has shown they do not care
                    // about — the exact behaviour this whole module exists to remove.
                    let _ = self.memory.complete_task(&t.id).await;
                    asked.remove(&t.id);
                    changed = true;
                }
                _ => {}
            }
        }
        if changed {
            self.set_closure_asks(asked).await;
        }
        out
    }

    /// Close every stale thread whose description matches `needle` — the "stop tracking that" path.
    ///
    /// Matches OPEN tasks only, and reports what it closed by name so a wrong match is visible
    /// immediately rather than discovered as a missing commitment weeks later.
    pub async fn stop_tracking(&self, needle: &str) -> String {
        let needle = needle.trim().to_lowercase();
        if needle.len() < 3 {
            return "Which one should I drop? Name a few words from it.".to_string();
        }
        let (open, _) = self.open_and_internal_tasks().await;
        let hits: Vec<&mind_types::Task> = open
            .iter()
            .filter(|t| t.description.to_lowercase().contains(&needle))
            .collect();
        if hits.is_empty() {
            return format!("Nothing open matches \u{201c}{needle}\u{201d} \u{2014} `ym tasks` lists what I am carrying.");
        }
        let mut closed = Vec::new();
        for t in &hits {
            if self.memory.complete_task(&t.id).await.unwrap_or(false) {
                closed.push(t.description.clone());
            }
        }
        let mut asked = self.closure_asks().await;
        for t in &hits {
            asked.remove(&t.id);
        }
        self.set_closure_asks(asked).await;
        match closed.len() {
            0 => "I could not close that one.".to_string(),
            1 => format!("Dropped: {}. I will stop bringing it up.", closed[0]),
            n => format!("Dropped {n}: {}. I will stop bringing those up.", closed.join("; ")),
        }
    }

    /// Open tasks split personal/internal WITHOUT the staleness filter.
    ///
    /// `split_tasks` deliberately hides stale threads, so the lifecycle cannot use it — it would never
    /// see the very threads it exists to close. Same partition, no filter.
    pub(crate) async fn open_and_internal_tasks(&self) -> (Vec<mind_types::Task>, Vec<mind_types::Task>) {
        let open: Vec<mind_types::Task> = self
            .memory
            .list_tasks(false)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.is_open())
            .collect();
        open.into_iter().partition(|t| super::is_personal_reminder(&t.description))
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;

    fn aug11() -> chrono::DateTime<chrono::FixedOffset> {
        chrono::DateTime::parse_from_rfc3339("2026-08-11T10:00:00-05:00").unwrap()
    }

    /// THE LINE THAT KEPT THE NAGGING ALIVE. `parse_text_date_ms` rolls a past date forward a year, so
    /// "before July 17th" read in August resolved to July 2027 — 340 days in the FUTURE. The task never
    /// became overdue, never triggered a nudge, and was carried as live work indefinitely.
    #[test]
    fn a_deadline_that_has_passed_reads_as_passed() {
        let today = aug11();
        let dl = parse_deadline_ms("Order Brishti's Rosefield watch before July 17th", &today).unwrap();
        assert!(dl < today.timestamp_millis(), "17 July is BEHIND 11 August, not ahead of it");
        // 24, not 25: the deadline resolves to NOON on 17 July and "today" is 10am on 11 August, so the
        // final partial day floors away. Worth pinning exactly — an off-by-one here is the difference
        // between a thread closing on time and lingering one more day.
        let days_over = (today.timestamp_millis() - dl) / 86_400_000;
        assert_eq!(days_over, 24, "17 July noon to 11 August 10am is 24 whole days");

        // And the classifier now sees it, which is the whole point.
        let t = mind_types::Task {
            id: "t1".into(),
            description: "Order Brishti's Rosefield watch before July 17th".into(),
            status: "pending".into(),
            priority: "high".into(),
            due_ms: None,
        };
        let s = classify(&t, today.timestamp_millis(), parse_deadline_ms(&t.description, &today), None);
        assert!(matches!(s, ThreadState::NeedsClosure { .. }), "got {s:?}");
        assert!(!s.is_carried());
    }

    /// A deadline still ahead stays ahead.
    #[test]
    fn a_future_deadline_is_still_future() {
        let today = aug11();
        let dl = parse_deadline_ms("file the return by October 3", &today).unwrap();
        assert!(dl > today.timestamp_millis());
    }

    /// The wrap-around case: read in December, "January 5th" means the coming January, not eleven
    /// months ago. Six months is the only threshold that splits the two readings, since `Task` carries
    /// no creation timestamp to disambiguate with.
    #[test]
    fn a_date_far_behind_is_read_as_the_coming_one() {
        let december = chrono::DateTime::parse_from_rfc3339("2026-12-20T10:00:00-05:00").unwrap();
        let dl = parse_deadline_ms("renew it by January 5", &december).unwrap();
        assert!(dl > december.timestamp_millis(), "January means NEXT January when read in December");

        // But a date only weeks behind is genuinely behind.
        let dl2 = parse_deadline_ms("renew it by November 5", &december).unwrap();
        assert!(dl2 < december.timestamp_millis(), "5 November is behind 20 December");
    }

    /// A recurring occasion must KEEP rolling forward — the birthday parser is right for birthdays, and
    /// this change must not have altered it.
    #[test]
    fn the_recurring_parser_is_untouched() {
        let today = aug11();
        let birthday = crate::parse_text_date_ms("Brishti's birthday is July 23", &today).unwrap();
        assert!(birthday > today.timestamp_millis(), "a birthday in July means NEXT July");
    }

    #[test]
    fn text_with_no_date_yields_none() {
        assert!(parse_deadline_ms("call mum more often", &aug11()).is_none());
    }
}
