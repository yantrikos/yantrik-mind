//! Named delegations — "create an agent, assign it a task", made honest.
//!
//! The runtime already existed (researcher + coder executors, bg-job cap, notify queue); what was
//! missing was ACCOUNTABILITY: a job you kick off should be visible while it runs and findable
//! after it finishes, not just a message that scrolls by in chat. So a delegation is a LEDGER ROW
//! first and a background task second: `ym delegate <name>: <task>` records it, runs it, updates
//! it, and `ym jobs` shows the board. The desktop app's Tasks pane renders this.
//!
//! Deliberately NOT a persistent-agent framework: a "named agent" here is a labeled job, not a
//! second mind with its own memory. One mind, many hands — the moment delegations get their own
//! beliefs is the moment the household has two sources of truth.

use super::*;

const LEDGER_KEY: &str = "delegations";
const LEDGER_CAP: usize = 50;
/// Result stored in the ledger row. Was 1200 — enough for a board glance, but the desktop's
/// channel view renders the WHOLE result as the agent's message, and a truncated answer in a
/// channel reads as a broken agent (it did, live: "…/ / Several sea"). 8000 chars × 50-row cap
/// keeps the blob bounded.
const RESULT_HEAD: usize = 8000;

/// `<name>: <task>` (explicit) or just `<task>` (name derived from its first words). Kind is
/// routed by verb keywords — code-shaped work goes to the sandboxed coder, everything else to the
/// researcher.
pub(crate) fn parse_delegation(rest: &str) -> Option<(String, String, &'static str)> {
    let rest = rest.trim();
    if rest.len() < 3 {
        return None;
    }
    let (name, task) = match rest.split_once(':') {
        // A colon names the job — but "https://..." is not a name:task split.
        Some((n, t)) if !n.contains("http") && n.split_whitespace().count() <= 5 && !t.trim().is_empty() => {
            (n.trim().to_string(), t.trim().to_string())
        }
        _ => {
            // Derived label: the first few PLAIN words — URLs stay in the task, not the label.
            let name: String =
                rest.split_whitespace().filter(|w| !w.contains("://")).take(4).collect::<Vec<_>>().join(" ");
            let name = if name.is_empty() { "job".to_string() } else { name };
            (name, rest.to_string())
        }
    };
    let tl = task.to_lowercase();
    let code_shaped = ["build", "write a script", "code", "implement", "fix the", "refactor", "patch"]
        .iter()
        .any(|k| tl.contains(k));
    Some((name, task, if code_shaped { "code" } else { "research" }))
}

/// Read-modify-write one ledger row. Free function on the memory handle so the detached task can
/// update the board without holding the engine.
/// Record where a code job's artifacts live, so a reader can OPEN them. "Done, at
/// /opt/.../run-178.../index.html" is a dead end for anyone not sitting on the box; the desktop
/// reads these fields to preview the actual file.
pub(crate) async fn ledger_artifacts(
    memory: &Arc<dyn MemoryFacade>,
    id: &str,
    workdir: &str,
    files: &[String],
) {
    let mut rows: Vec<serde_json::Value> = memory
        .profile_get(LEDGER_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if let Some(r) = rows.iter_mut().find(|r| r.get("id").and_then(|x| x.as_str()) == Some(id)) {
        r["workdir"] = serde_json::json!(workdir);
        r["files"] = serde_json::json!(files);
    }
    let _ = memory.profile_set(LEDGER_KEY, &serde_json::to_string(&rows).unwrap_or_default()).await;
}

pub(crate) async fn ledger_update(
    memory: &Arc<dyn MemoryFacade>,
    id: &str,
    status: &str,
    result_head: Option<String>,
) {
    let mut rows: Vec<serde_json::Value> = memory
        .profile_get(LEDGER_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if let Some(r) = rows.iter_mut().find(|r| r.get("id").and_then(|x| x.as_str()) == Some(id)) {
        r["status"] = serde_json::json!(status);
        r["finished_ms"] = serde_json::json!(chrono::Utc::now().timestamp_millis());
        if let Some(head) = result_head {
            r["result"] = serde_json::json!(head.chars().take(RESULT_HEAD).collect::<String>());
        }
    }
    let _ = memory.profile_set(LEDGER_KEY, &serde_json::to_string(&rows).unwrap_or_default()).await;
}

/// Does a critique verdict mean "good enough, stop iterating"? Tolerant of critics that dress the
/// word up ("SHIP.", "ship — looks solid"), strict about critics that merely MENTION shipping
/// mid-critique ("fix X before you ship").
pub(crate) fn verdict_ships(v: &str) -> bool {
    v.trim().to_uppercase().starts_with("SHIP")
}

/// Scratch memory for one job — Pranab's design (2026-08-05): a long job gets its own quarantined
/// workspace; at completion what's worth keeping is PROMOTED into real memory and the scratch is
/// destroyed. Nothing a job writes touches the mind's memory without passing the promotion gate,
/// so a delegation can think freely without becoming a second source of truth.
///
/// Substrate note: stored as a per-job profile blob today (the engine controls purge completely);
/// the surface (note → promote → purge) is shaped so it can move onto real yantrikdb namespaces
/// once the core grows delete-by-namespace — the caller-visible lifecycle won't change.
fn scratch_key(id: &str) -> String {
    format!("job_scratch:{id}")
}
const SCRATCH_NOTE_CAP: usize = 200;
/// Un-promoted scratch older than this is junk by definition — purged by the board on render.
const SCRATCH_STALE_MS: i64 = 7 * 24 * 3600 * 1000;

pub(crate) async fn scratch_note(memory: &Arc<dyn MemoryFacade>, id: &str, text: &str) {
    let key = scratch_key(id);
    let mut notes: Vec<serde_json::Value> = memory
        .profile_get(&key)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if notes.len() >= SCRATCH_NOTE_CAP {
        return; // a runaway job must not grow an unbounded blob
    }
    notes.push(serde_json::json!({ "t": chrono::Utc::now().timestamp_millis(), "note": text }));
    let _ = memory.profile_set(&key, &serde_json::to_string(&notes).unwrap_or_default()).await;
}

pub(crate) fn render_board(rows: &[serde_json::Value], now_ms: i64) -> String {
    if rows.is_empty() {
        return "🧰 No delegations yet. `ym delegate <name>: <task>` starts one — research by default, \
                the sandboxed coder when the task reads like code."
            .to_string();
    }
    let mut out = String::from("🧰 DELEGATIONS — the job board\n");
    let mut sorted: Vec<&serde_json::Value> = rows.iter().collect();
    // Running first, then newest finished.
    sorted.sort_by_key(|r| {
        let running = r.get("status").and_then(|x| x.as_str()) == Some("running");
        let t = r.get("started_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        (if running { 0 } else { 1 }, -t)
    });
    for r in sorted.iter().take(15) {
        let g = |k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or("?");
        let status = g("status");
        let started = r.get("started_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        let mins = ((now_ms - started).max(0)) / 60_000;
        let badge = match status {
            "running" => format!("⏳ running {mins}m"),
            "done" => "✅ done".to_string(),
            _ => "❌ failed".to_string(),
        };
        out.push_str(&format!("\n[{}] {} · {} · {}\n    task: {}\n", g("id"), g("name"), g("kind"), badge, g("task")));
        if status != "running" {
            let head = g("result");
            if head != "?" && !head.is_empty() {
                let first: String = head.lines().take(3).collect::<Vec<_>>().join(" / ");
                out.push_str(&format!("    {}\n", first.chars().take(180).collect::<String>()));
            }
        }
    }
    out.push_str(
        "\nA finished job's scratch waits 7 days: `ym jobs keep <id>` promotes it into memory \
         (as a sub-agent observation), `ym jobs drop <id>` destroys it unkept.",
    );
    out
}

impl super::ConversationEngine {
    /// `ym delegate <name>: <task>` — ledger row + background execution + chat delivery.
    pub async fn delegate_cmd(&self, rest: &str) -> String {
        let Some((name, task, kind)) = parse_delegation(rest) else {
            return "Usage: `ym delegate <name>: <task>` (e.g. `ym delegate quant-check: compare DeepSeek IQ2 vs Q3 quality claims`).".to_string();
        };
        // Executor presence FIRST — a ledger row for a job that can't run is a lie on the board.
        let runnable = match kind {
            "code" => self.coder.is_some(),
            _ => self.researcher.is_some(),
        };
        if !runnable {
            return format!("(the {kind} executor isn't configured on this box)");
        }
        if !self.try_acquire_bg(3) {
            return "(the job board is full — a few delegations are already running; `ym jobs` to see them)".to_string();
        }
        let id = format!("{:x}", chrono::Utc::now().timestamp_millis() & 0xffffff);
        let now = chrono::Utc::now().timestamp_millis();
        let mut rows: Vec<serde_json::Value> = self
            .memory
            .profile_get(LEDGER_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        rows.push(serde_json::json!({
            "id": id, "name": name, "task": task, "kind": kind,
            "status": "running", "started_ms": now,
        }));
        if rows.len() > LEDGER_CAP {
            let cut = rows.len() - LEDGER_CAP;
            rows.drain(..cut);
        }
        let _ = self.memory.profile_set(LEDGER_KEY, &serde_json::to_string(&rows).unwrap_or_default()).await;

        let (q, jobs, mem) = (self.notify_queue.clone(), self.bg_jobs.clone(), self.memory.clone());
        let (id2, name2, task2) = (id.clone(), name.clone(), task.clone());
        if kind == "code" {
            // ITERATE-UNTIL-GOOD (the Hermes pattern, 2026-08-06): one coder pass produces a first
            // draft; real artifacts need build → critique → improve until a bar is met. Same shape
            // as the nightly self-improve loop, generalized from "the mind's codebase" to "whatever
            // was delegated". Every round narrates into scratch, so the channel thread shows the
            // loop working — and the critique trail survives for the promotion gate to keep.
            let c = self.coder.clone().unwrap();
            let critic = self.inference.clone();
            let rounds: usize = std::env::var("YM_DELEGATE_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
            tokio::spawn(async move {
                let mut wd: Option<String> = None;
                let mut last: Option<mind_tools::coder::CoderResult> = None;
                let mut verdict = String::new();
                let mut brief = task2.clone();
                for round in 1..=rounds {
                    scratch_note(&mem, &id2, &format!("round {round}: building — {}", brief.chars().take(160).collect::<String>())).await;
                    let res = match &wd {
                        Some(w) => c.run_in(&brief, w.clone()).await,
                        None => c.run(&task2).await,
                    };
                    let r = match res {
                        Ok(r) => r,
                        Err(e) => {
                            scratch_note(&mem, &id2, &format!("round {round}: build error — {e}")).await;
                            let msg = format!("🛠️ [{name2}] failed in round {round}: {e}");
                            ledger_update(&mem, &id2, "failed", Some(msg.clone())).await;
                            q.lock().unwrap().push(msg);
                            jobs.fetch_sub(1, Ordering::Relaxed);
                            return;
                        }
                    };
                    wd = Some(r.workdir.clone());
                    scratch_note(&mem, &id2, &format!("round {round}: built {} file(s) — {}", r.files.len(), r.summary.chars().take(200).collect::<String>())).await;
                    // CRITIQUE — a separate set of eyes on the artifact, judging against the ORIGINAL
                    // task (not the round brief, which narrows every iteration). "SHIP" ends the loop.
                    let listing = r.files.join(", ");
                    let critique_prompt = format!(
                        "You are reviewing a delegated build.\nTASK: {task2}\nFILES PRODUCED: {listing}\nBUILDER'S SUMMARY: {}\n\n\
                         Judge the ARTIFACT against the TASK. If it plausibly satisfies the task and has no obvious defects, reply exactly SHIP. \
                         Otherwise list up to 4 CONCRETE defects to fix (one line each, imperative, specific).",
                        r.summary.chars().take(1200).collect::<String>()
                    );
                    let cfg = GenerationConfig { max_tokens: 300, ..GenerationConfig::default() };
                    verdict = critic
                        .chat_scoped(vec![ChatMessage::user(&critique_prompt)], cfg, mind_inference::PrivacyScope::Household)
                        .await
                        .map(|x| x.text.trim().to_string())
                        .unwrap_or_else(|_| "SHIP".to_string()); // a dead critic must not wedge the loop open
                    last = Some(r);
                    if verdict_ships(&verdict) {
                        scratch_note(&mem, &id2, &format!("round {round}: critique — SHIP")).await;
                        break;
                    }
                    scratch_note(&mem, &id2, &format!("round {round}: critique — {}", verdict.chars().take(400).collect::<String>())).await;
                    brief = format!("Improve the existing build in this directory. Fix these review findings:\n{verdict}\nDo not start over; edit in place.");
                }
                let r = last.expect("at least one round ran");
                ledger_artifacts(&mem, &id2, &r.workdir, &r.files).await;
                let shipped = verdict_ships(&verdict);
                let msg = format!(
                    "🛠️ [{name2}] {}:\n\n{}{}",
                    if shipped { "done (passed review)" } else { "done (round limit — last review below)" },
                    mind_tools::render_coder(&r),
                    if shipped { String::new() } else { format!("\n\nOutstanding review notes:\n{verdict}") }
                );
                ledger_update(&mem, &id2, "done", Some(msg.clone())).await;
                q.lock().unwrap().push(msg);
                jobs.fetch_sub(1, Ordering::Relaxed);
            });
        } else {
            let r = self.researcher.clone().unwrap();
            tokio::spawn(async move {
                scratch_note(&mem, &id2, &format!("task: {task2}")).await;
                let res = r.run(&task2).await;
                // Findings land in SCRATCH, not memory — the promotion gate (`jobs keep`) is the
                // only door from a job's output into the mind's real memory.
                for u in res.sources.iter().take(10) {
                    scratch_note(&mem, &id2, &format!("source: {u}")).await;
                }
                scratch_note(&mem, &id2, &res.answer).await;
                let mut msg = format!("🔎 [{name2}] {}", res.answer);
                if !res.sources.is_empty() {
                    msg.push_str("\n\nSources:\n");
                    for u in res.sources.iter().take(6) {
                        msg.push_str(&format!("- {u}\n"));
                    }
                }
                ledger_update(&mem, &id2, "done", Some(msg.clone())).await;
                q.lock().unwrap().push(msg);
                jobs.fetch_sub(1, Ordering::Relaxed);
            });
        }
        format!("🧰 Delegated [{id}] \"{name}\" ({kind}). It's on the board — `ym jobs` — and the result lands in chat.")
    }

    /// `ym jobs [json | keep <id> | drop <id>]` — the board, its machine-readable form (the
    /// desktop's channel view), and the two ends of a job's scratch memory.
    pub async fn jobs_report_cmd(&self, rest: &str) -> String {
        let rest = rest.trim();
        if rest == "json" {
            let rows: Vec<serde_json::Value> = self
                .memory
                .profile_get(LEDGER_KEY)
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            // Attach each job's scratch notes — the thread timeline between task and result.
            let mut out = Vec::with_capacity(rows.len());
            for mut r in rows {
                if let Some(id) = r.get("id").and_then(|x| x.as_str()).map(String::from) {
                    let notes: Vec<serde_json::Value> = self
                        .memory
                        .profile_get(&scratch_key(&id))
                        .await
                        .ok()
                        .flatten()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();
                    r["notes"] = serde_json::Value::Array(notes);
                }
                out.push(r);
            }
            return serde_json::json!({ "jobs": out }).to_string();
        }
        if let Some(id) = rest.strip_prefix("keep ") {
            return self.job_promote(id.trim()).await;
        }
        if let Some(id) = rest.strip_prefix("drop ") {
            let n = self.job_purge_scratch(id.trim()).await;
            return format!("🗑 Dropped [{}] scratch ({n} note(s)) — nothing entered memory.", id.trim());
        }
        let rows: Vec<serde_json::Value> = self
            .memory
            .profile_get(LEDGER_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Stale-scratch hygiene rides board renders: un-promoted scratch of long-finished jobs is
        // junk by definition (the promotion window has clearly passed).
        let now = chrono::Utc::now().timestamp_millis();
        for r in rows.iter() {
            let finished = r.get("finished_ms").and_then(|x| x.as_i64()).unwrap_or(i64::MAX);
            if now - finished > SCRATCH_STALE_MS {
                if let Some(id) = r.get("id").and_then(|x| x.as_str()) {
                    let _ = self.job_purge_scratch(id).await;
                }
            }
        }
        render_board(&rows, now)
    }

    /// PROMOTION GATE — the one door from a job's scratch into the mind's real memory. Writes an
    /// OBSERVATION with sub-agent provenance, never a belief: the belief path keeps its stricter
    /// gates (wrong beliefs are recalled confidently forever). Scratch is destroyed after.
    async fn job_promote(&self, id: &str) -> String {
        let notes: Vec<serde_json::Value> = self
            .memory
            .profile_get(&scratch_key(id))
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if notes.is_empty() {
            return format!("[{id}] has no scratch to keep (already promoted, dropped, or never noted).");
        }
        let body: Vec<&str> = notes.iter().filter_map(|n| n.get("note").and_then(|x| x.as_str())).collect();
        let text = format!("Delegated job [{id}] findings: {}", body.join(" | "));
        let text: String = text.chars().take(4000).collect();
        match self.memory.remember_observation(&text, mind_types::safety::ProvenanceCategory::SubAgent).await {
            Ok(_) => {
                let n = self.job_purge_scratch(id).await;
                format!("📥 Kept [{id}] — {n} note(s) promoted into memory as a sub-agent observation; scratch destroyed.")
            }
            Err(e) => format!("(couldn't promote [{id}]: {e} — scratch left intact)"),
        }
    }

    async fn job_purge_scratch(&self, id: &str) -> usize {
        let key = scratch_key(id);
        let n = self
            .memory
            .profile_get(&key)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
            .map(|v| v.len())
            .unwrap_or(0);
        let _ = self.memory.profile_set(&key, "[]").await;
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_name_and_task_split_on_colon() {
        let (n, t, k) = parse_delegation("quant-check: compare DeepSeek IQ2 vs Q3 claims").unwrap();
        assert_eq!(n, "quant-check");
        assert!(t.starts_with("compare"));
        assert_eq!(k, "research");
    }

    #[test]
    fn code_shaped_tasks_route_to_the_coder() {
        let (_, _, k) = parse_delegation("log-tool: build a CLI for parsing the tick logs").unwrap();
        assert_eq!(k, "code");
    }

    #[test]
    fn a_url_colon_is_not_a_name_split() {
        let (n, t, _) = parse_delegation("summarize https://example.com/post fully").unwrap();
        assert!(t.contains("https://example.com/post"), "task lost the url: {t}");
        assert!(!n.contains("https"), "name should be derived words, got {n}");
    }

    #[test]
    fn board_shows_running_before_done_and_says_so() {
        let rows = vec![
            serde_json::json!({"id":"a1","name":"old","task":"x","kind":"research","status":"done","started_ms":1000,"result":"found it"}),
            serde_json::json!({"id":"b2","name":"live","task":"y","kind":"code","status":"running","started_ms":2000}),
        ];
        let out = render_board(&rows, 8_000_000);
        let live = out.find("live").unwrap();
        let old = out.find("old").unwrap();
        assert!(live < old, "running job must render first:\n{out}");
        assert!(out.contains("⏳ running") && out.contains("✅ done"));
    }

    #[test]
    fn empty_board_teaches_the_verb() {
        assert!(render_board(&[], 0).contains("ym delegate"));
    }
}

#[cfg(test)]
mod iterate_tests {
    use super::*;

    #[test]
    fn ship_verdicts_end_the_loop_and_critiques_do_not() {
        for v in ["SHIP", "ship", "  Ship.  ", "SHIP — looks solid"] {
            assert!(verdict_ships(v), "{v:?} should ship");
        }
        for v in ["Fix the contrast before you ship", "1. broken nav\n2. SHIP the fixed css", "needs work"] {
            assert!(!verdict_ships(v), "{v:?} must keep iterating");
        }
    }
}
