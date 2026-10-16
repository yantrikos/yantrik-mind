//! E.INIT1 (Stage B of the proactive plan): initiative CANDIDATES, written to a ledger and never
//! delivered. A detector notices a moment that might be worth telling the person about and records
//! the card it would have sent, why, and why now; nothing is leased, queued or shown. Whether such
//! moments are worth receiving is judged afterwards, by retrospective review, before anything goes
//! live (Stage C).
//!
//! The first detector is delegated work that has run past its usual time. The calendar heads-up
//! is the OS's (Pranab, 16 Oct: the desktop owns the calendar and raises its own `calendar`
//! cards), so there is none here.

use std::sync::{Arc, Mutex};

use crate::ConversationEngine;

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
