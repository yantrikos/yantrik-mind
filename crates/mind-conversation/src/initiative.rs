//! E.INIT1 (Stage B of the proactive plan): initiative CANDIDATES, written to a ledger and never
//! delivered. A detector notices a moment that might be worth telling the person about and records
//! the card it would have sent, why, and why now; nothing is leased, queued or shown. Whether such
//! moments are worth receiving is judged afterwards, by retrospective review, before anything goes
//! live (Stage C).
//!
//! The first detector is delegated work that has run past its usual time. E.INIT2 adds "coming
//! up": something the Mind already holds -- a person's date, its own calendar, a task dated in its
//! text -- close enough to be worth a heads-up. The calendar heads-up
//! is the OS's (Pranab, 16 Oct: the desktop owns the calendar and raises its own `calendar`
//! cards), so there is none here.

use std::sync::{Arc, Mutex};

use crate::{ConversationEngine, TurnIdentity};

/// The private profile entry holding the candidates, newest last.
pub(crate) const CANDIDATES_KEY: &str = "initiative_candidates";
/// How many candidates the entry keeps.
const KEEP: usize = 500;

/// One candidate: the contract the plan names, and the card it would have been.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub family: String,
    pub trigger: String,
    pub evidence_refs: Vec<String>,
    pub observed_at_ms: u64,
    pub what_changed: String,
    pub why_it_matters: String,
    pub next_step: String,
    pub useful_until_ms: u64,
    pub dedupe_key: String,
    pub would_interrupt: bool,
    pub text: String,
}

/// A delegated job running now, as the stalled detector sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningJob {
    pub id: String,
    pub kind: &'static str,
    pub task: String,
    pub started_ms: u64,
}

/// The registry of running research and coder jobs.
pub type JobRegistry = Arc<Mutex<Vec<RunningJob>>>;

/// Holds a job's place in the registry; dropping it -- the job finished, failed or panicked --
/// takes the job out.
pub struct JobGuard {
    registry: JobRegistry,
    id: String,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.registry.lock().unwrap_or_else(|e| e.into_inner()).retain(|j| j.id != self.id);
    }
}

/// A task as a card names it: at most 120 characters, cut at a word with an ellipsis (541 showed
/// "… ep": a silent mid-word cut reads as wrong data).
pub(crate) fn short_task(task: &str) -> String {
    let task = task.trim();
    if task.chars().count() <= 120 {
        return task.to_string();
    }
    let head: String = task.chars().take(119).collect();
    let cut = head.rfind(' ').filter(|i| *i > 60).unwrap_or(head.len());
    format!("{}\u{2026}", head[..cut].trim_end())
}

/// How long a delegation of this kind may run before it counts as stalled. `YM_STALL_MINUTES`
/// overrides every kind (a drive knob, so a live check need not wait for a naturally slow job);
/// unset, zero or unparseable keeps the defaults.
pub(crate) fn stall_ms(kind: &str) -> u64 {
    stall_ms_with(kind, std::env::var("YM_STALL_MINUTES").ok().as_deref())
}

/// E.INIT2: the heads-up stage for an item `days_out` local days away -- "ahead" at 2-3 days,
/// "soon" at 0-1, none otherwise.
pub(crate) fn lead_stage(days_out: i64) -> Option<&'static str> {
    match days_out {
        0..=1 => Some("soon"),
        2..=3 => Some("ahead"),
        _ => None,
    }
}

/// E.INIT2: the card a "coming up" candidate would have been.
pub(crate) fn coming_up_text(subject: &str, when: &str, days_out: i64) -> String {
    match days_out {
        0 => format!("{subject}: today ({when})."),
        1 => format!("{subject}: tomorrow ({when})."),
        n => format!("{subject}: {when}, in {n} days."),
    }
}

pub(crate) fn stall_ms_with(kind: &str, minutes: Option<&str>) -> u64 {
    if let Some(m) = minutes.and_then(|m| m.trim().parse::<u64>().ok()).filter(|m| *m > 0) {
        return m * 60_000;
    }
    match kind {
        "research" => 15 * 60_000,
        _ => 30 * 60_000,
    }
}

impl ConversationEngine {
    /// Register a delegated job as running; keep the guard for as long as the job runs.
    pub fn start_job(&self, kind: &'static str, task: &str) -> JobGuard {
        let started_ms = Self::now_ms();
        let id = format!("{kind}-{started_ms}-{}", self.running_jobs.lock().unwrap_or_else(|e| e.into_inner()).len());
        self.running_jobs.lock().unwrap_or_else(|e| e.into_inner()).push(RunningJob {
            id: id.clone(),
            kind,
            task: short_task(task),
            started_ms,
        });
        JobGuard { registry: self.running_jobs.clone(), id }
    }

    /// The candidates recorded so far, oldest first.
    pub async fn initiative_candidates(&self) -> Vec<Candidate> {
        self.memory
            .profile_get(CANDIDATES_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Record a candidate unless one with its dedupe key is already there. Returns whether it was
    /// new. A textless decision event counts it; the text stays in the private profile entry.
    pub async fn record_candidate(&self, c: Candidate) -> bool {
        let mut all = self.initiative_candidates().await;
        if all.iter().any(|x| x.dedupe_key == c.dedupe_key) {
            return false;
        }
        let mut ev = mind_observability::DecisionEvent::new(&format!("initiative-{}", c.observed_at_ms), "initiative_candidate");
        ev.ts_ms = c.observed_at_ms;
        ev.actor = Some("initiative".into());
        ev.goal_id = Some(format!("initiative:{}", c.family));
        ev.trigger = Some(c.trigger.clone());
        ev.object_id = Some(c.dedupe_key.clone());
        ev.chosen = Some(if c.would_interrupt { "interrupt" } else { "inbox" }.into());
        self.recorder.record(ev);
        all.push(c);
        if all.len() > KEEP {
            let cut = all.len() - KEEP;
            all.drain(..cut);
        }
        let _ = self.memory.profile_set(CANDIDATES_KEY, &serde_json::to_string(&all).unwrap_or_default()).await;
        true
    }

    /// E.INIT1: one shadow pass at `now_ms`. Returns how many new candidates were recorded.
    /// Calls no model and delivers nothing.
    pub async fn shadow_initiative_at(&self, now_ms: u64) -> usize {
        let mut running: Vec<(String, String, String, u64)> = self
            .running_jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|j| (j.id.clone(), j.kind.to_string(), j.task.clone(), j.started_ms))
            .collect();
        for row in self.job_rows().await.into_iter().filter(|r| r.status == "running") {
            let task = if row.task.is_empty() { row.name.clone() } else { row.task.clone() };
            let kind = if row.kind.is_empty() { "delegation".to_string() } else { row.kind.clone() };
            running.push((row.id, kind, short_task(&task), row.started_ms.max(0) as u64));
        }
        let mut fresh = 0;
        for (id, kind, task, started) in running {
            let ran = now_ms.saturating_sub(started);
            if ran < stall_ms(&kind) {
                continue;
            }
            let minutes = ran / 60_000;
            let spent = if minutes == 1 { "1 minute".to_string() } else { format!("{minutes} minutes") };
            let c = Candidate {
                family: "delegated".into(),
                trigger: format!("{kind} job running {minutes} min"),
                evidence_refs: vec![format!("job:{id}")],
                observed_at_ms: now_ms,
                what_changed: format!("the {kind} job has run {spent}"),
                why_it_matters: "longer than this usually takes".into(),
                next_step: "none needed; the result comes when it is done".into(),
                useful_until_ms: now_ms + 60 * 60_000,
                dedupe_key: format!("delegated-stalled:{id}"),
                would_interrupt: false,
                text: format!(
                    "Still working on \"{task}\": {spent} in, longer than this usually takes. Nothing needs you; I'll bring the result when it's done."
                ),
            };
            if self.record_candidate(c).await {
                fresh += 1;
            }
        }
        fresh + self.shadow_coming_up(now_ms).await
    }

    /// E.INIT2: what the Mind already holds that is 0-3 days away, as candidates. A reminder with a
    /// due time is skipped -- it fires as an obligation already. No desktop read, no model call.
    async fn shadow_coming_up(&self, now_ms: u64) -> usize {
        let today = crate::local_now();
        let (reminders, _) = self.split_tasks().await;
        let timed: std::collections::HashSet<String> =
            reminders.iter().filter(|t| t.due_ms.is_some()).map(|t| t.id.clone()).collect();
        let mut fresh = 0;
        // Four days read, the local date decides: `upcoming_spine(3)` ends 72 h from now, so an item
        // three days out but later in the day than now fell outside it.
        // E.SPINE3: and the dates the primary told me.
        let primary = TurnIdentity::primary();
        let ctx = mind_types::AccessContext::principal(primary.viewer(), mind_types::Purpose::conversation(&primary.owner));
        for (ms, line, id) in self.with_told(self.upcoming_spine(4).await, 4, &ctx).await {
            if id.as_ref().is_some_and(|i| timed.contains(i)) {
                continue;
            }
            let Some(at) = chrono::DateTime::from_timestamp_millis(ms) else { continue };
            let date = at.with_timezone(today.offset()).date_naive();
            let days_out = (date - today.date_naive()).num_days();
            let Some(stage) = lead_stage(days_out) else { continue };
            let subject = line.split_once(": ").map_or(line.as_str(), |(_, rest)| rest).trim().trim_start_matches('\u{23f0}').trim();
            let when = date.format("%a %-d %b").to_string();
            let end_of_day = date
                .and_hms_opt(23, 59, 59)
                .and_then(|d| d.and_local_timezone(*today.offset()).single())
                .map_or(now_ms, |d| d.timestamp_millis().max(0) as u64);
            let c = Candidate {
                family: "coming_up".into(),
                trigger: format!("{days_out} day(s) out"),
                evidence_refs: id.as_deref().map(crate::task_trace).into_iter().collect(),
                observed_at_ms: now_ms,
                what_changed: format!("{subject} is {days_out} day(s) away"),
                why_it_matters: "something they told me about is close".into(),
                next_step: "none; a heads-up".into(),
                useful_until_ms: end_of_day,
                dedupe_key: format!("coming-up:{date}:{}:{stage}", &mind_spec::sha256_hex(subject.to_lowercase().as_bytes())[..16]),
                would_interrupt: false,
                text: coming_up_text(subject, &when, days_out),
            };
            if self.record_candidate(c).await {
                fresh += 1;
            }
        }
        fresh
    }
}

#[cfg(test)]
mod init1_tests {
    use super::*;
    use mind_inference::InferencePool;
    use mind_memory::MemoryHandle;
    use mind_types::MemoryFacade;

    fn engine() -> (ConversationEngine, Arc<dyn MemoryFacade>) {
        let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
        let pool = InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("unused")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        (ConversationEngine::new(mem.clone(), pool, "YM"), mem)
    }

    const MIN: u64 = 60_000;

    /// A research job past 15 minutes is one candidate however many passes run; one at 5 minutes
    /// and one that finished are none; a pass delivers nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_stalled_job_is_one_candidate_and_nothing_is_delivered() {
        let (conv, mem) = engine();
        let guard = conv.start_job("research", "ext4 vs btrfs for a home server");
        let started = conv.running_jobs.lock().unwrap()[0].started_ms;
        assert_eq!(conv.shadow_initiative_at(started + 5 * MIN).await, 0, "a 5-minute job is not stalled");
        let mut fresh = 0;
        for i in 0..3 {
            fresh += conv.shadow_initiative_at(started + 16 * MIN + i * MIN).await;
        }
        assert_eq!(fresh, 1, "one stall, one candidate");
        let all = conv.initiative_candidates().await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].family, "delegated");
        assert!(!all[0].would_interrupt);
        assert!(all[0].text.starts_with("Still working on \"ext4 vs btrfs for a home server\": 16 minutes in"), "{}", all[0].text);
        // Nothing delivered: no notice, no held note, no task.
        assert!(conv.take_held_notes().is_empty());
        assert!(conv.take_notifications().is_empty());
        assert!(mem.list_tasks(false).await.unwrap().is_empty());
        // Finished: out of the registry, and no new candidate.
        drop(guard);
        assert!(conv.running_jobs.lock().unwrap().is_empty(), "the guard left the job registered");
        let _second = conv.start_job("code", "a todo app");
        let started2 = conv.running_jobs.lock().unwrap()[0].started_ms;
        assert_eq!(conv.shadow_initiative_at(started2 + 20 * MIN).await, 0, "a 20-minute coder job is not stalled");
    }

    /// Both delegated-work sites register their job, and hold the guard inside the spawned task --
    /// a site that slipped would make its stalls invisible to the detector.
    #[test]
    fn the_research_and_coder_jobs_are_registered() {
        let src = include_str!("capabilities.rs");
        assert_eq!(src.matches("host.start_job(\"research\"").count(), 1);
        assert_eq!(src.matches("host.start_job(\"code\"").count(), 1);
        assert_eq!(src.matches("let _running = running;").count(), 2, "a guard is not moved into its task");
    }

    /// E.INIT2: a task dated in its text 3 days out is one "ahead" candidate and one 1 day out a
    /// "soon" one, however many passes run; a reminder with a due time and an item 10 days out are
    /// none; a pass delivers nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn something_coming_up_is_one_candidate_per_stage() {
        let (conv, mem) = engine();
        let today = crate::local_now();
        let on = |d: i64| (today + chrono::Duration::days(d)).format("%-d %B").to_string();
        mem.add_task(&format!("Arun's housewarming on {}", on(3)), "medium", None).await.unwrap();
        mem.add_task(&format!("Pick up the cake on {}", on(1)), "medium", None).await.unwrap();
        mem.add_task(&format!("Dentist appointment on {}", on(10)), "medium", None).await.unwrap();
        let due = (today + chrono::Duration::days(2)).timestamp_millis() as u64;
        mem.add_task("Call the bank about the card", "medium", Some(due)).await.unwrap();
        let now = today.timestamp_millis() as u64;
        let mut fresh = 0;
        for i in 0..3 {
            fresh += conv.shadow_initiative_at(now + i * MIN).await;
        }
        let all = conv.initiative_candidates().await;
        assert_eq!(fresh, 2, "{all:#?}");
        let party = all.iter().find(|c| c.text.contains("Arun")).expect("no housewarming candidate");
        assert_eq!(party.family, "coming_up");
        assert!(party.dedupe_key.ends_with(":ahead") && party.text.ends_with("in 3 days."), "{party:?}");
        let cake = all.iter().find(|c| c.text.contains("cake")).expect("no cake candidate");
        assert!(cake.dedupe_key.ends_with(":soon") && cake.text.contains(": tomorrow ("), "{cake:?}");
        assert!(all.iter().all(|c| !c.would_interrupt && !c.text.contains("bank") && !c.text.contains("Dentist")), "{all:#?}");
        assert!(conv.take_held_notes().is_empty() && conv.take_notifications().is_empty(), "a candidate was delivered");
    }

    /// E.SPINE3: a date the person told me, 2 days out, is a coming_up candidate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_told_date_is_coming_up() {
        let (conv, mem) = engine();
        let on = (crate::local_now() + chrono::Duration::days(2)).format("%-d %B").to_string();
        mem.remember_as_belief(mind_types::BeliefAssertion {
            statement: format!("Neighbour Arun's birthday is on {on}"),
            polarity: 1.0,
            weight: 0.8,
            source_event: Some("agent".into()),
            provenance: "told".into(),
        })
        .await
        .unwrap();
        assert_eq!(conv.shadow_initiative_at(crate::local_now().timestamp_millis() as u64).await, 1);
        let c = &conv.initiative_candidates().await[0];
        assert!(c.family == "coming_up" && c.text.contains("Arun's birthday") && c.dedupe_key.ends_with(":ahead"), "{c:?}");
    }

    /// E.INIT2: the stage boundaries and the card's wording.
    #[test]
    fn the_coming_up_stages_and_cards() {
        assert_eq!((lead_stage(-1), lead_stage(0), lead_stage(1)), (None, Some("soon"), Some("soon")));
        assert_eq!((lead_stage(2), lead_stage(3), lead_stage(4)), (Some("ahead"), Some("ahead"), None));
        assert_eq!(coming_up_text("Priya's birthday", "Tue 4 Nov", 0), "Priya's birthday: today (Tue 4 Nov).");
        assert_eq!(coming_up_text("Priya's birthday", "Tue 4 Nov", 3), "Priya's birthday: Tue 4 Nov, in 3 days.");
    }

    /// Amendment 3: the drive knob overrides every kind; nothing usable keeps the defaults.
    #[test]
    fn the_stall_knob_overrides_only_when_it_is_usable() {
        assert_eq!(stall_ms_with("research", None), 15 * MIN);
        assert_eq!(stall_ms_with("code", None), 30 * MIN);
        assert_eq!(stall_ms_with("research", Some("1")), MIN);
        assert_eq!(stall_ms_with("code", Some(" 2 ")), 2 * MIN);
        for bad in ["0", "", "soon", "-3"] {
            assert_eq!(stall_ms_with("research", Some(bad)), 15 * MIN, "{bad:?}");
        }
    }

    /// 541 (db75300): "1 minutes in", and the task cut mid-word with no sign of the cut.
    #[test]
    fn the_card_reads_cleanly() {
        let long = "Compare every major approach to long-term memory for language-model agents published since 2023, with the trade-offs of each one";
        let short = short_task(long);
        assert!(short.chars().count() <= 120 && short.ends_with('\u{2026}'), "{short}");
        assert!(short.starts_with("Compare every major approach") && !short.ends_with(" \u{2026}"), "{short}");
        assert_eq!(short_task("a todo app"), "a todo app");
        let src = include_str!("initiative.rs");
        assert!(src.contains("if minutes == 1 { \"1 minute\".to_string() }"));
    }

    /// A job that panics is still taken out of the registry.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_job_that_panics_leaves_the_registry() {
        let (conv, _mem) = engine();
        let guard = conv.start_job("research", "x");
        let task = tokio::spawn(async move {
            let _g = guard;
            panic!("the job died");
        });
        assert!(task.await.is_err());
        assert!(conv.running_jobs.lock().unwrap().is_empty());
    }
}
