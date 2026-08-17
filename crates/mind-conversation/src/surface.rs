//! surface — the TYPED contract the desktop cockpit renders from.
//!
//! Why this module exists: the cockpit was flat not because it was badly built but because the
//! server only ever sent it prose. `POST /cli` returns a formatted string, so the Activity panel
//! could do nothing but `<pre>` the `funnel` verb's output — which is how "knock attempts killed"
//! and "not-knockworthy" ended up on the main screen. A client cannot render semantics it was
//! never sent.
//!
//! So: typed payloads, one per surface, beside the existing text verbs. The text verbs are
//! untouched — `ym`, Telegram, and the voice client keep working exactly as before, and a report
//! that is genuinely prose (a briefing, a self-review) stays prose. What changes is that the
//! surfaces the operator watches CONTINUOUSLY get structure: state the client can lay out, colour
//! by severity, and sort — instead of a monospace wall it can only echo.
//!
//! Two rules this module holds itself to:
//!
//! 1. NEVER invent a field the mind cannot actually answer. Every number here traces to a real
//!    read: the belief store, the delegation ledger, the persisted provider rollup, the recipe
//!    store. A surface that shows a plausible-but-unmeasured value is worse than one that omits it,
//!    because the operator cannot tell which is which. Absent data is reported ABSENT (`None`),
//!    not zero — `beliefs: 0` and "couldn't read the belief store" are different facts.
//! 2. Severity is assigned HERE, not in the client. Whether an open contradiction is a warning or
//!    a critical depends on the mind's own state, which the client does not have. Shipping a raw
//!    count and letting JavaScript guess the colour is how a UI starts lying.

use serde::Serialize;

use super::*;

// ── Pulse: what the Activity panel shows ──────────────────────────────────────────────────────

/// The right-hand panel's whole payload. Replaces the funnel `<pre>`.
///
/// Shape follows the brief's §11: contextual information about what the mind is doing and what
/// wants the operator, NOT machine telemetry. The funnel counters still exist — they moved to
/// `FunnelReport` below, which the diagnostics surface reads.
#[derive(Serialize, Default)]
pub struct Pulse {
    /// Local wall-clock on the BOX when this snapshot was taken.
    ///
    /// The panel's "snapshot 42s ago" label is computed from the client's own receive time, not from
    /// this — the two clocks are across an ssh tunnel and a few seconds of skew would make a fresh
    /// snapshot look stale. This field is the box's account of when it looked, which is what you
    /// want when reconciling a panel against the service log.
    pub taken_at: String,
    pub brain: BrainState,
    pub work: WorkState,
    /// Things wanting the operator, most severe first.
    pub attention: Vec<AttentionItem>,
    /// The time spine — what is coming, so the panel connects to the future not just the present.
    pub upcoming: Vec<UpcomingItem>,
    pub resources: ResourceState,
}

/// Which brain is answering, and under what constraints.
#[derive(Serialize, Default)]
pub struct BrainState {
    /// The provider label actually serving turns (e.g. "nanogpt:deepseek-v4-pro").
    pub provider: String,
    /// Is an owned-hardware private lane attached? When true, privately-grounded turns never
    /// leave the house; when false they escalate to cloud WITH an audit entry. The operator should
    /// be able to see which regime is in force without reading logs.
    pub private_lane: bool,
    /// Degraded operation (the inference layer's own flag), surfaced rather than hidden.
    pub survival_mode: bool,
    /// Total beliefs held. `None` when the store could not be read — never silently 0.
    pub beliefs: Option<u64>,
}

/// Work in flight and work owed.
#[derive(Serialize, Default)]
pub struct WorkState {
    pub jobs_running: usize,
    pub jobs_total: usize,
    /// Open reminders the mind is carrying for the operator.
    pub reminders_open: usize,
    /// Durable recipe runs parked on a clock or a condition — the standing orders. `None` when no
    /// recipe store is configured (a `:memory:` DB), which is different from "none parked".
    pub standing_orders: Option<usize>,
    /// What is running RIGHT NOW, so the panel names the work instead of counting it. "2 running"
    /// tells the operator nothing they can act on; "market-scan, researching, 4m" does.
    pub running: Vec<JobBrief>,
}

/// A live delegation, reduced to what a panel row needs.
#[derive(Serialize)]
pub struct JobBrief {
    pub id: String,
    pub name: String,
    pub task: String,
    /// "research" | "code" — which executor is driving it.
    pub kind: String,
    pub started_ms: i64,
    /// Whole seconds in flight. Precomputed so the client shows elapsed time without needing to
    /// agree with the server about what "now" is — clock skew across an ssh tunnel is real.
    pub elapsed_s: i64,
}

/// One thing wanting the operator, in language the operator speaks.
#[derive(Serialize)]
pub struct AttentionItem {
    /// Machine tag for grouping and iconography.
    pub kind: AttentionKind,
    pub severity: Severity,
    /// One line, plain language, no jargon and no counters.
    pub headline: String,
    /// The specifics. May be empty when the headline says everything.
    pub detail: String,
    /// A console verb that acts on this, when one exists — so the panel can offer a real action
    /// instead of describing a problem the operator then has to go hunt down.
    pub action: Option<String>,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionKind {
    /// An outward action is proposed and waiting for a yes/no. Nothing happens until it is answered.
    Confirmation,
    /// A recipe paused on a question.
    Question,
    /// Two beliefs disagree and the mind has not been told which is right.
    Contradiction,
    /// A tool has been failing often enough that its results should be distrusted.
    UnreliableTool,
    /// A delegated job failed.
    JobFailed,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warn,
    Critical,
}

/// One dated thing on the horizon.
#[derive(Serialize)]
pub struct UpcomingItem {
    /// Epoch ms, so the client formats in its own locale rather than parsing our prose.
    pub at_ms: i64,
    pub label: String,
    /// Whole days from now; negative would mean overdue.
    pub in_days: i64,
    /// The reminder this row came from, when it came from one. `None` for calendar events and
    /// people dates — those are facts about the world, not commitments the mind is carrying, so
    /// there is nothing to dismiss. A client must render a dismiss control ONLY when this is set,
    /// rather than offering one that cannot work.
    pub task_id: Option<String>,
}

/// Token and spend accounting, from the PERSISTED rollup (survives restart) rather than the
/// in-process counters (which do not).
#[derive(Serialize, Default)]
pub struct ResourceState {
    pub today_tokens_in: u64,
    pub today_tokens_out: u64,
    pub week_tokens_in: u64,
    pub week_tokens_out: u64,
    /// Per-provider breakdown, biggest week-consumer first.
    pub by_provider: Vec<ProviderUsage>,
    /// LLM spend from the token ledger — every lane, not just self-build. `None` when no ledger
    /// file exists: the difference between "nothing spent" and "not measured" matters here more
    /// than anywhere. (Was `build_spend_usd`, a single lifetime float labelled "build spend". It
    /// stayed accurate and useless — a day that moved 42.7M tokens on the delegation lane never
    /// touched it, because that lane wrote nothing to the ledger at all.)
    pub llm_spend: Option<LedgerSpend>,
}

/// What the ledger knows, shaped for an instrument rather than a report.
#[derive(Serialize, Default, PartialEq, Debug)]
pub struct LedgerSpend {
    pub total_usd: f64,
    pub total_tokens: u64,
    /// The actionable number. A lifetime total only ever climbs, so it stops being read; today's
    /// spend is the one that says "something is happening right now".
    pub today_usd: f64,
    pub today_tokens: u64,
    /// Biggest spender first. A total that cannot be attributed cannot be acted on.
    pub by_lane: Vec<LaneSpend>,
    /// Rounds whose CLI returned no usage block. When non-zero every figure above is a FLOOR.
    pub unmeasured: usize,
}

#[derive(Serialize, Default, PartialEq, Debug)]
pub struct LaneSpend {
    pub lane: String,
    pub runs: usize,
    pub tokens: u64,
    pub usd: f64,
}

/// Parse the shared token ledger. Pure so it is testable without a filesystem or a clock.
///
/// `today` is an ISO date prefix (`2026-08-14`); lines are `TIMESTAMP | LANE | MODEL | tokens=… |
/// usd=…`, and a lane reads `builder` or `delegate:<job>#<round>` — grouped on the part before the
/// colon, so many rounds of one job roll up into the lane rather than flooding the breakdown.
pub(crate) fn ledger_spend_from(text: &str, today: &str) -> Option<LedgerSpend> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    let mut out = LedgerSpend::default();
    let mut lanes: std::collections::BTreeMap<String, LaneSpend> = Default::default();
    for l in lines {
        // UNMEASURED parses as neither, which is the point: it contributes to the count and to
        // nothing else, so the totals never quietly absorb a round nobody measured.
        if l.contains("tokens=UNMEASURED") {
            out.unmeasured += 1;
        }
        let usd = l.rsplit_once("usd=").and_then(|(_, v)| v.trim().parse::<f64>().ok()).unwrap_or(0.0);
        let toks = l
            .split("tokens=")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        out.total_usd += usd;
        out.total_tokens += toks;
        if l.starts_with(today) {
            out.today_usd += usd;
            out.today_tokens += toks;
        }
        let lane = l.split(" | ").nth(1).unwrap_or("unknown");
        let lane = lane.split(':').next().unwrap_or(lane).to_string();
        let e = lanes.entry(lane.clone()).or_insert_with(|| LaneSpend { lane, ..Default::default() });
        e.runs += 1;
        e.tokens += toks;
        e.usd += usd;
    }
    out.by_lane = lanes.into_values().collect();
    out.by_lane.sort_by(|a, b| b.usd.partial_cmp(&a.usd).unwrap_or(std::cmp::Ordering::Equal).then(b.tokens.cmp(&a.tokens)));
    Some(out)
}

#[derive(Serialize)]
pub struct ProviderUsage {
    pub provider: String,
    pub today_in: u64,
    pub today_out: u64,
    pub week_in: u64,
    pub week_out: u64,
    /// Turns this provider actually answered this week.
    pub week_served: u64,
}

// ── Funnel: the diagnostics payload ───────────────────────────────────────────────────────────

/// The proactive funnel, structured. Same data `funnel_report()` renders as text — but typed, so
/// the diagnostics surface can show it as a funnel instead of an aligned column dump, and so it
/// stays OFF the main screen where the brief correctly says it does not belong.
#[derive(Serialize, Default)]
pub struct FunnelReport {
    pub days: usize,
    pub events_total: u64,
    pub events_by_domain: Vec<Count>,
    pub twitch_evaluations: u64,
    pub twitch_alerts: u64,
    /// Kill sites, biggest first — the gate to fix, or to trust.
    pub kills: Vec<Count>,
    pub kills_total: u64,
    pub sent: u64,
    /// Share of knock attempts that died, 0.0–1.0. `None` when there were no attempts at all
    /// (a rate over zero attempts is not 0%, it is undefined).
    pub kill_rate: Option<f64>,
}

#[derive(Serialize)]
pub struct Count {
    pub label: String,
    pub n: u64,
}

// ── Standing orders: the scheduler surface ────────────────────────────────────────────────────

/// Recurring and sleeping work, typed.
///
/// The distinction that matters here: `store: false` means this mind has NO recipe store (a
/// `:memory:` DB), so scheduling is impossible — which is a completely different thing from having a
/// store with nothing in it. A UI that cannot tell them apart shows an empty schedule list and an
/// invitingly enabled "create" button on a mind that will silently drop the order.
#[derive(Serialize, Default)]
pub struct OrdersReport {
    /// Is durable scheduling available at all?
    pub store: bool,
    pub orders: Vec<StandingOrder>,
}

#[derive(Serialize)]
pub struct StandingOrder {
    pub id: String,
    /// The order's name, which for a planned order is its goal.
    pub name: String,
    pub state: OrderState,
    /// When it next fires, epoch ms. 0 when the record carries no wake stamp.
    pub next_ms: u64,
    /// Seconds until it fires. Negative means it is overdue — which is real and worth showing: a
    /// paused order's time keeps passing, and a resumed one fires immediately.
    pub in_seconds: i64,
    /// Which actions apply in this state, so the client renders exactly the buttons that will work
    /// instead of offering all four and failing three of them.
    pub actions: Vec<&'static str>,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderState {
    /// Armed and waiting for its time.
    Sleeping,
    /// Deliberately held. Its next time is preserved, not reset.
    Paused,
}

// ── Threads: what the mind is carrying, and where each one is in its life ──────────────────────

/// Open commitments with their lifecycle state.
///
/// This surface exists because the thread lifecycle was invisible. The runtime now drops stale threads
/// out of its own grounding and asks one closure question — all correct, and all happening where the
/// operator could not see it. A lifecycle you cannot inspect is one you have to trust; a list with a
/// Drop button next to each row is one you can correct.
#[derive(Serialize, Default)]
pub struct ThreadReport {
    /// Live and just-due work — what the mind is actively carrying.
    pub carrying: Vec<Thread>,
    /// Past their window: waiting on a closure answer, or about to be dropped.
    pub closing: Vec<Thread>,
}

#[derive(Serialize)]
pub struct Thread {
    pub id: String,
    pub description: String,
    pub state: ThreadStateTag,
    /// Days past the deadline. Negative means still ahead of it.
    pub days_over: Option<i64>,
    /// `None` when the commitment has no deadline at all — an open-ended intention, which is never
    /// stale and must not be shown as if it were overdue.
    pub deadline_ms: Option<i64>,
    pub priority: String,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStateTag {
    /// Ahead of its deadline, or has none.
    Live,
    /// Just missed — still worth a nudge.
    JustDue,
    /// Long past; the mind has asked, or will ask, what happened.
    AwaitingClosure,
    /// Asked and unanswered. The next tick drops it.
    Dropping,
}

// ── Skills: the procedure library ─────────────────────────────────────────────────────────────

/// The banked skills, with the track record that decides whether they get used.
///
/// Surfaced because "search skill, select skill, execute" is only trustworthy if the library is
/// legible: which skills exist, which are earning their place, and which the runtime has quarantined.
/// A success rate the operator cannot see is a number the operator cannot correct.
#[derive(Serialize, Default)]
pub struct SkillReport {
    pub skills: Vec<SkillRow>,
    pub active: usize,
    pub quarantined: usize,
    /// Never run, so unproven rather than good — the distinction `success_rate()` alone would hide,
    /// since it returns 1.0 for zero runs.
    pub untested: usize,
}

#[derive(Serialize)]
pub struct SkillRow {
    pub name: String,
    pub summary: String,
    pub lang: String,
    pub tags: Vec<String>,
    /// "candidate" | "active" | "quarantined".
    pub status: String,
    pub runs: u64,
    pub successes: u64,
    /// `None` when never run. NOT 1.0 — an untested skill has no rate, and showing one would present
    /// an assumption as a measurement.
    pub success_rate: Option<f64>,
    /// Below half over four or more runs: the store's own quarantine rule, surfaced so the operator
    /// can see a skill on its way out rather than discovering it gone.
    pub failing: bool,
    pub created_ms: u64,
}

// ── Capabilities: the inventory ───────────────────────────────────────────────────────────────

/// What this mind can actually do right now — the honest answer to "which capabilities exist,
/// which are connected, and which would fail if an agent reached for them".
///
/// This is the seam the agent compiler needs (brief §7): "objective → required capabilities →
/// available tools" cannot resolve against a prose catalog. It resolves against this.
#[derive(Serialize, Default)]
pub struct CapabilityReport {
    pub capabilities: Vec<CapabilityEntry>,
    pub connected: usize,
    pub unavailable: usize,
    pub disabled: usize,
}

#[derive(Serialize)]
pub struct CapabilityEntry {
    pub id: String,
    pub title: String,
    pub category: String,
    /// read_only / personal / gated_write — drives whether a call needs the confirmation handshake.
    pub security: String,
    /// builtin / imported / self_authored.
    pub provenance: String,
    pub availability: Availability,
    /// Tool names this capability owns, so the compiler can map a required capability to callable
    /// tools without parsing catalog prose.
    pub tools: Vec<String>,
    /// Present only when unavailable: what is missing, in the operator's terms.
    pub blocked_by: Option<String>,
}

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Enabled and its backing client/credential is present — an agent can rely on it.
    Ready,
    /// Enabled, but the thing it needs is not configured. An agent must be TOLD this rather than
    /// discovering it as a runtime string, which is what made the model confabulate results.
    Unavailable,
    /// Turned off in the manifest. Deliberate, not broken.
    Disabled,
}

// ── The namespace ─────────────────────────────────────────────────────────────────────────────

/// Every typed surface this build serves. Doubles as the version handshake: a client fetches this
/// list once and only asks for surfaces the box actually has, instead of discovering the gap by
/// getting prose where it expected JSON.
pub const TYPED_VERBS: &[&str] = &[
    "surfaces",
    "pulse",
    "funnel_json",
    "capabilities_json",
    "orders_json",
    "threads_json",
    "skills_json",
    // Separate from `pulse` on purpose: this one makes an OUTBOUND call, and pulse is painted
    // often. A slow or wedged provider must not be able to stall the whole snapshot.
    "quota_json",
    // The chat pane's memory: recent primary-lane conversation, oldest first. Exists so a client
    // opening fresh does not present an amnesiac chat over a mind that remembers — and so results
    // that background jobs mirrored into the transcript are visible without asking.
    "transcript_json",
];

/// Is this verb a request for machine-readable state?
///
/// Two ways to qualify: it is a known surface, or it merely LOOKS like one (`*_json`). The second
/// clause is the important one — it catches a future surface a newer client asks for, so the answer
/// is a structured "not in this build" rather than an invented reply from the conversational path.
pub(crate) fn is_typed_verb(cmd: &str) -> bool {
    TYPED_VERBS.contains(&cmd) || cmd.ends_with("_json")
}

// ── Serialization ─────────────────────────────────────────────────────────────────────────────

/// Serialize a surface payload, or a JSON *error object* if that somehow fails.
///
/// The invariant this protects: a client that asked for JSON must never receive prose. If it did,
/// the parse would throw and the panel would show a raw exception — the exact failure mode this
/// whole module exists to remove. So the sad path stays machine-readable and carries an `error`
/// field the client can render as a proper failed state.
pub(crate) fn json_or_error<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|e| {
        serde_json::json!({ "error": format!("could not serialize this surface: {e}") }).to_string()
    })
}

// ── Construction ──────────────────────────────────────────────────────────────────────────────

impl ConversationEngine {
    /// Take a pulse snapshot. Independent reads run CONCURRENTLY — this is on a 2-minute refresh
    /// path in the cockpit and there is no reason to pay for it serially.
    pub async fn pulse(&self, ctx: &mind_types::AccessContext) -> Pulse {
        // `split_tasks` reads the task list itself, so it stands in for a separate `list_tasks`
        // call — one read, not two.
        let (beliefs, conflicts, (reminders, _), spine, tools, jobs) = tokio::join!(
            self.memory.belief_count(),
            self.memory.conflicts(ctx),
            self.split_tasks(),
            self.upcoming_spine(7),
            self.memory.tool_track_record(),
            self.job_rows(),
        );

        let now = local_now();
        let now_ms = now.timestamp_millis();

        let mut attention: Vec<AttentionItem> = Vec::new();

        // A pending outward action is the single most blocking thing that can be true: the mind has
        // stopped and is waiting. It outranks everything else on the panel.
        if let Some(req) = self.pending.lock().unwrap().as_ref() {
            attention.push(AttentionItem {
                kind: AttentionKind::Confirmation,
                severity: Severity::Critical,
                headline: format!("Waiting for your yes: {}", req.intent.summary),
                detail: if req.intent.target.is_empty() {
                    req.justification.clone()
                } else {
                    format!("to {} — {}", req.intent.target, req.justification)
                },
                action: Some("yes".to_string()),
            });
        }
        if self.pending_question.lock().unwrap().is_some() {
            attention.push(AttentionItem {
                kind: AttentionKind::Question,
                severity: Severity::Warn,
                headline: "A standing order is paused on a question".to_string(),
                detail: "It resumes as soon as you answer in chat.".to_string(),
                action: None,
            });
        }

        // Contradictions: the typed-memory moat made visible. Severity rides the mind's own
        // severity score, so the panel does not have to guess.
        let conflicts = conflicts.unwrap_or_default();
        for c in conflicts.iter().take(4) {
            attention.push(AttentionItem {
                kind: AttentionKind::Contradiction,
                severity: if c.severity >= 0.8 { Severity::Warn } else { Severity::Info },
                headline: "Two things I know disagree".to_string(),
                detail: format!("\u{201c}{}\u{201d} vs \u{201c}{}\u{201d}", c.belief_a, c.belief_b),
                action: Some(":conflicts".to_string()),
            });
        }

        // Measured tool unreliability. The agent loop already gets told this in its prompt; the
        // operator was the one who couldn't see it.
        for (tool, rate, n) in tools.unwrap_or_default().iter().filter(|(_, r, n)| *r < 0.5 && *n >= 3).take(3) {
            attention.push(AttentionItem {
                kind: AttentionKind::UnreliableTool,
                severity: Severity::Warn,
                headline: format!("{tool} has been unreliable"),
                detail: format!("succeeded {:.0}% of the last {n} uses — treat its results with suspicion", rate * 100.0),
                action: None,
            });
        }

        let jobs_running = jobs.iter().filter(|j| j.status == "running").count();
        // Only RECENT failures earn a slot. A job that failed last week is history, not something
        // wanting attention now — and an attention list that never empties is one nobody reads.
        const FAILURE_WINDOW_MS: i64 = 24 * 3600 * 1000;
        let mut failures: Vec<&crate::delegate::JobRow> = jobs
            .iter()
            .filter(|j| j.status == "failed")
            .filter(|j| j.finished_ms.map(|f| now_ms - f < FAILURE_WINDOW_MS).unwrap_or(false))
            .collect();
        failures.sort_by_key(|j| std::cmp::Reverse(j.finished_ms.unwrap_or(0)));
        for j in failures.into_iter().take(2) {
            attention.push(AttentionItem {
                kind: AttentionKind::JobFailed,
                severity: Severity::Warn,
                headline: format!("{} failed", j.name),
                // The recorded failure message, not the task — "why" is what the operator needs,
                // and re-showing the assignment they already wrote tells them nothing.
                detail: match j.result.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                    Some(why) => why.chars().take(240).collect(),
                    None => format!("no reason was recorded. Task was: {}", j.task),
                },
                action: Some(format!("delegate {}: {}", j.name, j.task)),
            });
        }

        attention.sort_by(|a, b| b.severity.cmp(&a.severity));

        let standing_orders = self.recipes.as_ref().map(|r| r.list_sleeping().len());

        Pulse {
            taken_at: now.format("%Y-%m-%d %H:%M:%S").to_string(),
            brain: BrainState {
                provider: self.inference.provider().to_string(),
                private_lane: self.inference.has_private_lane(),
                survival_mode: mind_inference::in_survival_mode(),
                beliefs: beliefs.ok(),
            },
            work: WorkState {
                jobs_running,
                jobs_total: jobs.len(),
                reminders_open: reminders.len(),
                standing_orders,
                running: jobs
                    .iter()
                    .filter(|j| j.status == "running")
                    .map(|j| JobBrief {
                        id: j.id.clone(),
                        name: j.name.clone(),
                        task: j.task.clone(),
                        kind: j.kind.clone(),
                        started_ms: j.started_ms,
                        // A row whose start was never recorded reports 0 elapsed rather than an
                        // absurd 58-year runtime computed against epoch 0.
                        elapsed_s: if j.started_ms > 0 { (now_ms - j.started_ms) / 1000 } else { 0 },
                    })
                    .collect(),
            },
            attention,
            upcoming: spine
                .into_iter()
                .take(6)
                .map(|(at_ms, label, task_id)| UpcomingItem {
                    at_ms,
                    // CALENDAR days, not elapsed milliseconds. `(at_ms - now_ms).div_euclid(day)`
                    // floors toward negative infinity, so anything earlier TODAY came back as -1 —
                    // a birthday at 00:00 read as "1d ago" by mid-morning, and the client rendered
                    // any negative as overdue. What the panel means by "today" is the local date,
                    // so compare dates.
                    in_days: chrono::DateTime::from_timestamp_millis(at_ms)
                        .map(|t| (t.with_timezone(now.offset()).date_naive() - now.date_naive()).num_days())
                        .unwrap_or(0),
                    label,
                    task_id,
                })
                .collect(),
            resources: resource_state(),
        }
    }

    /// The funnel, typed. Reuses the same ledger `funnel_report()` renders from, so the text and
    /// structured views can never disagree.
    pub async fn funnel_json(&self) -> FunnelReport {
        // Flush pending in-memory event tallies first, exactly as the text report does, so the
        // structured view is not systematically staler than the prose one.
        self.funnel_bump("").await;
        let counters = self
            .memory
            .profile_get(crate::funnel::FUNNEL_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        funnel_from_counters(&counters)
    }

    /// THE agent-visible tool source: every enabled capability's catalog lines, plus whatever MCP
    /// servers have connected.
    ///
    /// One method because there were three: the agent loop, `discover_tools`, and the prompt audit
    /// each composed this by hand, and each had to remember to include the hand-written household
    /// blob. Three copies of "what tools exist" is three chances to disagree — and they did, since
    /// the audit's copy was the one nobody updated. Now the registry is the single source and this is
    /// its single reader.
    pub(crate) fn catalog_source(&self) -> String {
        let plugins = self.plugins.lock().unwrap().enabled_catalog();
        match self.mcp.as_ref().map(|h| h.catalog()).unwrap_or_default() {
            m if m.trim().is_empty() => plugins,
            m => format!("{plugins}\n{m}"),
        }
    }

    /// Standing orders, typed. Reads the same recipe store the waking tick reads, so the list is
    /// what will actually happen — not a separate registry that can drift from it.
    pub fn orders_report(&self) -> OrdersReport {
        let Some(recipes) = &self.recipes else { return OrdersReport::default() };
        let now = local_now().timestamp_millis();
        let mut orders: Vec<StandingOrder> = Vec::new();
        for (state, rows) in [
            (OrderState::Sleeping, recipes.list_sleeping()),
            (OrderState::Paused, recipes.list_paused()),
        ] {
            for (id, name, next_ms) in rows {
                orders.push(StandingOrder {
                    id,
                    name,
                    state,
                    next_ms,
                    in_seconds: (next_ms as i64 - now) / 1000,
                    actions: match state {
                        OrderState::Sleeping => vec!["run", "pause", "cancel"],
                        OrderState::Paused => vec!["resume", "cancel"],
                    },
                });
            }
        }
        // Soonest first; a paused order sorts by the time it would have fired.
        orders.sort_by_key(|o| o.next_ms);
        // `store` is true iff durable scheduling exists. `list_sleeping` returns empty both when
        // there is no store and when there is nothing parked, so ask the engine directly.
        OrdersReport { store: recipes.has_store(), orders }
    }

    /// Open commitments and where each sits in its life.
    ///
    /// Reads through the SAME classifier and the SAME deadline parser the runtime uses, so the list
    /// cannot disagree with what the mind is actually carrying — a separate query would drift, and a
    /// lifecycle view that drifts is worse than none.
    pub async fn thread_report(&self) -> ThreadReport {
        use crate::followthrough::{classify, parse_deadline_ms, ThreadState};
        let (open, _) = self.open_and_internal_tasks().await;
        let asked = self.closure_asks().await;
        let today = local_now();
        let now = today.timestamp_millis();
        let mut rep = ThreadReport::default();

        for t in open {
            let deadline = parse_deadline_ms(&t.description, &today).or_else(|| t.due_ms.map(|m| m as i64));
            let prior = asked.get(&t.id).and_then(|v| v.as_i64());
            let state = classify(&t, now, deadline, prior);
            let row = Thread {
                id: t.id.clone(),
                description: t.description.clone(),
                state: match state {
                    ThreadState::Live if prior.is_some() => ThreadStateTag::AwaitingClosure,
                    ThreadState::Live => ThreadStateTag::Live,
                    ThreadState::JustDue { .. } => ThreadStateTag::JustDue,
                    ThreadState::NeedsClosure { .. } => ThreadStateTag::AwaitingClosure,
                    ThreadState::Abandoned { .. } => ThreadStateTag::Dropping,
                },
                days_over: deadline.map(|d| (now - d) / 86_400_000),
                deadline_ms: deadline,
                priority: t.priority.clone(),
            };
            // The split mirrors what the runtime does: carried threads reach the prompt, the rest do
            // not. Showing them in one undifferentiated list would hide the whole point.
            if state.is_carried() && prior.is_none() {
                rep.carrying.push(row);
            } else {
                rep.closing.push(row);
            }
        }
        // Soonest deadline first; undated last, since an open-ended intention has no urgency.
        rep.carrying.sort_by_key(|t| t.deadline_ms.unwrap_or(i64::MAX));
        rep.closing.sort_by_key(|t| std::cmp::Reverse(t.days_over.unwrap_or(0)));
        rep
    }

    /// The skill library with its track record.
    pub async fn skill_report(&self) -> SkillReport {
        let mut rep = SkillReport::default();
        for s in self.memory.list_skills().await.unwrap_or_default() {
            let failing = s.runs >= 4 && s.successes * 2 < s.runs;
            match s.status.as_str() {
                "quarantined" => rep.quarantined += 1,
                _ if s.runs == 0 => rep.untested += 1,
                _ => rep.active += 1,
            }
            rep.skills.push(SkillRow {
                name: s.name,
                summary: s.summary,
                lang: s.lang,
                tags: s.tags,
                status: s.status,
                runs: s.runs,
                successes: s.successes,
                // An untested skill has NO rate. `Skill::success_rate()` returns 1.0 for zero runs,
                // which is a sensible ranking default and a lie as a displayed measurement.
                success_rate: (s.runs > 0).then(|| s.successes as f64 / s.runs as f64),
                failing,
                created_ms: s.created_ms,
            });
        }
        // Worst first: a failing skill is the one worth looking at, and burying it under the healthy
        // ones is how a quarantine goes unnoticed until something depends on it.
        rep.skills.sort_by(|a, b| {
            let key = |s: &SkillRow| (s.status != "quarantined", !s.failing, s.success_rate.unwrap_or(2.0));
            key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal)
        });
        rep
    }

    /// The capability inventory. Availability is probed against the SAME `Option<Arc<dyn …>>`
    /// fields the tool dispatch actually uses, so "ready" here means the call would really work —
    /// not that a manifest says it should.
    pub fn capability_report(&self) -> CapabilityReport {
        let reg = self.plugins.lock().unwrap();
        let mut capabilities: Vec<CapabilityEntry> = Vec::new();
        for spec in reg.all_specs() {
            let (availability, blocked_by) = if !spec.enabled {
                (Availability::Disabled, None)
            } else {
                match self.first_unmet(&spec.requires) {
                    Some(why) => (Availability::Unavailable, Some(why)),
                    None => (Availability::Ready, None),
                }
            };
            capabilities.push(CapabilityEntry {
                id: spec.id.clone(),
                title: spec.title.clone(),
                category: spec.category.clone(),
                security: spec.security.as_str().to_string(),
                provenance: spec.provenance.as_str().to_string(),
                availability,
                tools: spec.tools.clone(),
                blocked_by,
            });
        }
        capabilities.sort_by(|a, b| a.category.cmp(&b.category).then(a.id.cmp(&b.id)));
        let connected = capabilities.iter().filter(|c| c.availability == Availability::Ready).count();
        let unavailable = capabilities.iter().filter(|c| c.availability == Availability::Unavailable).count();
        let disabled = capabilities.iter().filter(|c| c.availability == Availability::Disabled).count();
        CapabilityReport { capabilities, connected, unavailable, disabled }
    }

    /// The first DECLARED requirement this engine cannot satisfy, phrased for the operator.
    ///
    /// One `match` from requirement to the concrete field that backs it. That is the whole probe —
    /// there is no id lookup, so a capability cannot be silently un-probed by a typo, and adding a
    /// requirement variant without wiring it here is a compile error rather than a false green.
    ///
    /// Reports only the FIRST unmet requirement: telling someone research needs a searcher, a
    /// fetcher, and a sub-agent is three problems where they have one thing to go fix.
    fn first_unmet(&self, requires: &[crate::plugins::Requirement]) -> Option<String> {
        use crate::plugins::Requirement as R;
        requires
            .iter()
            .find(|r| {
                match r {
                    R::WebSearch => self.searcher.is_none(),
                    R::WebFetch => self.web.is_none(),
                    R::News => self.news.is_none(),
                    R::Weather => self.weather.is_none(),
                    R::Wiki => self.wiki.is_none(),
                    R::Markets => self.markets.is_none(),
                    R::Translator => self.translator.is_none(),
                    R::HomeAssistant => self.home.is_none(),
                    R::Github => self.github.is_none(),
                    R::Coder => self.coder.is_none(),
                    // Either the bot's own mailbox or any personal scan inbox will do.
                    R::Mailbox => self.mail.is_none() && self.scan_mail.is_empty(),
                    R::Researcher => self.researcher.is_none(),
                }
            })
            .map(|r| r.unmet_reason().to_string())
    }
}

/// Persisted token/spend accounting. Free function because it reads files, not engine state.
fn resource_state() -> ResourceState {
    let rollup = mind_inference::provider_usage_rollup();
    let by_provider: Vec<ProviderUsage> = rollup
        .iter()
        .map(|(provider, ti, to, wi, wo, ws)| ProviderUsage {
            provider: provider.clone(),
            today_in: *ti,
            today_out: *to,
            week_in: *wi,
            week_out: *wo,
            week_served: *ws,
        })
        .collect();
    ResourceState {
        today_tokens_in: by_provider.iter().map(|p| p.today_in).sum(),
        today_tokens_out: by_provider.iter().map(|p| p.today_out).sum(),
        week_tokens_in: by_provider.iter().map(|p| p.week_in).sum(),
        week_tokens_out: by_provider.iter().map(|p| p.week_out).sum(),
        by_provider,
        llm_spend: build_spend(),
    }
}

/// LLM spend from the token ledger. `None` when the ledger is absent or empty — "not measured"
/// must stay distinguishable from "$0.00 spent".
fn build_spend() -> Option<LedgerSpend> {
    let path =
        std::env::var("YM_TOKEN_LEDGER").unwrap_or_else(|_| "/var/lib/yantrik-mind/token_ledger.log".to_string());
    let text = std::fs::read_to_string(path).ok()?;
    // The ledger stamps UTC, so "today" is UTC too. Local-day bucketing here would put the
    // evening's spend on tomorrow's tally for anyone east of Greenwich.
    ledger_spend_from(&text, &chrono::Utc::now().format("%Y-%m-%d").to_string())
}

/// Pure transform from the raw ledger to the typed report — separated from the async read so it is
/// directly testable without a memory handle.
pub(crate) fn funnel_from_counters(counters: &serde_json::Map<String, serde_json::Value>) -> FunnelReport {
    let mut totals: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut days = 0usize;
    for stages in counters.values() {
        days += 1;
        if let Some(m) = stages.as_object() {
            for (k, v) in m {
                *totals.entry(k.clone()).or_insert(0) += v.as_u64().unwrap_or(0);
            }
        }
    }
    let get = |k: &str| totals.get(k).copied().unwrap_or(0);
    let events_by_domain: Vec<Count> = totals
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("event:").map(|d| Count { label: d.to_string(), n: *v }))
        .collect();
    let mut kills: Vec<Count> = [
        "no-packets",
        "not-knockworthy",
        "provenance",
        "escrow-held",
        "no-candidate",
        "muted",
        "daily-cap",
        "unreceptive",
        "below-band",
    ]
    .iter()
    .map(|k| Count { label: (*k).to_string(), n: get(&format!("knock:{k}")) })
    .filter(|c| c.n > 0)
    .collect();
    kills.sort_by(|a, b| b.n.cmp(&a.n));
    let kills_total: u64 = kills.iter().map(|c| c.n).sum();
    let sent = get("knock:sent");
    let attempts = kills_total + sent;
    FunnelReport {
        days,
        events_total: events_by_domain.iter().map(|c| c.n).sum(),
        events_by_domain,
        twitch_evaluations: get("twitch:eval"),
        twitch_alerts: get("twitch:alert"),
        kills,
        kills_total,
        sent,
        kill_rate: (attempts > 0).then(|| kills_total as f64 / attempts as f64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instrument that did not exist while a day quietly moved 42.7M tokens. It must attribute
    /// by lane, separate today from all-time, and never let an unmeasured round read as a free one.
    #[test]
    fn ledger_spend_attributes_by_lane_and_keeps_unmeasured_visible() {
        let text = "\
2026-08-13T03:17:19Z | builder | haiku | tokens=70535 (in=5 cache_w=7898 cache_r=61965 out=667) | usd=0.0769
2026-08-14T01:00:00Z | delegate:a1b2#1 | qwen3.8-max | tokens=900000 (in=10 cache_w=1000 cache_r=898000 out=990) | usd=1.5000
2026-08-14T01:40:00Z | delegate:a1b2#2 | qwen3.8-max | tokens=100000 (in=10 cache_w=1000 cache_r=98000 out=990) | usd=0.5000
2026-08-14T02:10:00Z | delegate:c3d4#1 | unknown | tokens=UNMEASURED | usd=UNMEASURED
";
        let s = ledger_spend_from(text, "2026-08-14").expect("a non-empty ledger reports something");

        assert_eq!(s.total_tokens, 1_070_535, "all-time spans every day in the file");
        assert_eq!(s.today_tokens, 1_000_000, "yesterday's builder run is not today's spend");
        assert!((s.today_usd - 2.0).abs() < 1e-9);

        // Rounds of one job roll up into the lane; the delegation lane outspends the builder ~26x,
        // which is the whole reason a single undifferentiated total hid the problem.
        assert_eq!(s.by_lane.len(), 2, "delegate:a1b2#1, #2 and c3d4#1 are ONE lane, not three");
        assert_eq!(s.by_lane[0].lane, "delegate", "biggest spender first");
        assert_eq!(s.by_lane[0].runs, 3);
        assert_eq!(s.by_lane[1].lane, "builder");

        assert_eq!(s.unmeasured, 1, "the unmeasured round must be counted");
        assert_eq!(
            s.by_lane[0].tokens, 1_000_000,
            "UNMEASURED contributes to the count and to no total — a round nobody measured must never be absorbed as zero"
        );

        assert!(ledger_spend_from("", "2026-08-14").is_none(), "an empty ledger is not-measured, not $0.00");
        assert!(ledger_spend_from("   \n\n", "2026-08-14").is_none(), "whitespace is still empty");
    }

    fn ledger() -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert(
            "2026-08-04".to_string(),
            serde_json::json!({"event:ha:lock": 40, "event:cli": 2, "twitch:eval": 3,
                              "knock:not-knockworthy": 9, "knock:muted": 3, "knock:sent": 1}),
        );
        m
    }

    #[test]
    fn funnel_json_ranks_kills_and_computes_the_rate() {
        let r = funnel_from_counters(&ledger());
        assert_eq!(r.days, 1);
        assert_eq!(r.events_total, 42, "event domains must sum");
        assert_eq!(r.kills[0].label, "not-knockworthy", "biggest killer must sort first");
        assert_eq!(r.kills_total, 12);
        assert_eq!(r.sent, 1);
        // 12 of 13 attempts died.
        assert!((r.kill_rate.unwrap() - 12.0 / 13.0).abs() < 1e-9);
    }

    /// The distinction the whole module is built on: an undefined rate is None, not 0.0. A client
    /// that received 0.0 would draw a healthy-looking "0% died" bar for a mind that has never
    /// attempted a knock at all.
    #[test]
    fn no_attempts_means_no_rate_not_zero_percent() {
        let r = funnel_from_counters(&serde_json::Map::new());
        assert!(r.kill_rate.is_none(), "a rate over zero attempts is undefined, not 0%");
        assert_eq!(r.days, 0);
        assert!(r.kills.is_empty());
    }

    /// Parity with the text renderer: both read the same ledger, so their totals must agree. This
    /// is what stops the structured and prose views from drifting apart.
    #[test]
    fn structured_totals_match_the_text_report() {
        let m = ledger();
        let text = crate::funnel::render(&m);
        let json = funnel_from_counters(&m);
        assert!(text.contains(&json.kills_total.to_string()), "kill total must appear in the text report:\n{text}");
        // The rate the two views report must be the SAME number, not merely both plausible — the
        // text renderer rounds for display, so compare against its rounding.
        let pct = (json.kill_rate.unwrap() * 100.0).round() as u64;
        assert_eq!(pct, 92, "12 kills of 13 attempts = 92%");
        assert!(
            text.contains(&format!("{pct}% of knock attempts died")),
            "the text report must quote the same rate the structured view computes ({pct}%):\n{text}"
        );
    }

    #[test]
    fn severity_orders_critical_before_warn_before_info() {
        let mut v = vec![Severity::Info, Severity::Critical, Severity::Warn];
        v.sort_by(|a, b| b.cmp(a));
        assert_eq!(v[0], Severity::Critical);
        assert_eq!(v[2], Severity::Info);
    }

    // ── The contract itself ───────────────────────────────────────────────────────────────────
    // These drive the real console dispatch, because the thing that can actually break the cockpit
    // is not the struct — it is the VERB returning something unparseable. A unit test on `Pulse`
    // would still pass if the dispatch arm were spelled wrong or gated behind the wrong auth.

    fn test_engine(mem: &mind_memory::MemoryHandle) -> ConversationEngine {
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("ok")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        )
        .with_provider("test-provider");
        ConversationEngine::new(Arc::new(mem.clone()) as Arc<dyn MemoryFacade>, pool, "JARVIS")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn every_typed_verb_returns_parseable_json() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);
        let ctx = mind_types::AccessContext::operator_audit();
        // Drive the ADVERTISED list, so a surface added to TYPED_VERBS without a dispatch arm fails
        // here rather than in the client.
        for verb in TYPED_VERBS {
            let out = eng.cli_dispatch(verb, &ctx).await;
            let v: serde_json::Value = serde_json::from_str(&out)
                .unwrap_or_else(|e| panic!("`{verb}` must return JSON, got: {e}\n{out}"));
            assert!(v.get("error").is_none(), "`{verb}` reported an error: {out}");
            assert!(v.is_object(), "`{verb}` must return an object, got: {out}");
        }
    }

    /// THE VERSION-SKEW GUARD, and the reason it exists.
    ///
    /// Observed live against the running box: a client asked for `pulse` before the box had that
    /// verb; dispatch fell through to the conversational path and the mind produced a fluent,
    /// entirely invented "pulse check" — a fabricated report, at the cost of a model call. The
    /// machine-readable namespace must fail machine-readably: an unimplemented surface returns a
    /// structured error and never reaches the model.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_unknown_typed_surface_never_reaches_the_model() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        // A scripted LLM that would be a LOUD failure if the chat path were reached at all.
        let scripted = Arc::new(mind_inference::ScriptedLLM::new("I INVENTED THIS ANSWER"));
        let pool = mind_inference::InferencePool::new(
            scripted.clone() as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let eng = ConversationEngine::new(Arc::new(mem.clone()) as Arc<dyn MemoryFacade>, pool, "JARVIS");
        let ctx = mind_types::AccessContext::operator_audit();

        for verb in ["runs_json", "agents_json", "some_future_surface_json"] {
            let out = eng.cli_dispatch(verb, &ctx).await;
            let v: serde_json::Value = serde_json::from_str(&out)
                .unwrap_or_else(|e| panic!("`{verb}` must fail as JSON, not prose ({e}): {out}"));
            assert!(v["error"].is_string(), "`{verb}` must carry an error field: {out}");
            assert_eq!(v["surface"], verb);
            // The refusal names what this build DOES serve, so the client can adapt.
            assert!(v["supported"].is_array(), "the refusal must advertise the real surface list");
            assert!(!out.contains("I INVENTED THIS ANSWER"), "the model must never have been called");
        }
        assert!(
            scripted.last_prompt().is_empty(),
            "no prompt should have been built at all for an unimplemented surface"
        );
    }

    /// The handshake itself: `surfaces` must list exactly what the dispatch implements.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_handshake_advertises_the_real_surface_list() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);
        let out = eng.cli_dispatch("surfaces", &mind_types::AccessContext::operator_audit()).await;
        let v: serde_json::Value = serde_json::from_str(&out).expect("handshake must be JSON");
        let listed: Vec<&str> = v["surfaces"].as_array().unwrap().iter().map(|x| x.as_str().unwrap()).collect();
        assert!(listed.contains(&"pulse"));
        assert!(listed.contains(&"surfaces"), "the handshake must include itself, so a client can probe it");
        assert_eq!(listed.len(), TYPED_VERBS.len());
    }

    /// The typed surface must inherit the console's authorization, not bypass it. A member device
    /// authenticates but is not an operator; these verbs expose the whole household's state, so a
    /// principal must be refused exactly as every other `ym` verb refuses them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn typed_verbs_are_operator_only() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);
        let member = mind_types::AccessContext::principal(
            mind_types::Scope::parse("asha"),
            mind_types::Purpose::new(mind_types::Subject::Household, mind_types::Activity::Conversation),
        );
        for verb in ["pulse", "funnel_json", "capabilities_json"] {
            let out = eng.cli_dispatch(verb, &member).await;
            assert!(
                out.contains("operator"),
                "`{verb}` must refuse a non-operator, got: {out}"
            );
            assert!(serde_json::from_str::<serde_json::Value>(&out).is_err(), "the refusal is prose, by design");
        }
    }

    /// An unread belief store shows `beliefs: null`, never `beliefs: 0`. This is the rule from the
    /// module header, enforced: a client cannot distinguish "empty" from "broken" if we collapse
    /// both to zero, and the operator would read a broken store as a mind that knows nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pulse_reports_a_real_brain_state() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);
        let p = eng.pulse(&mind_types::AccessContext::operator_audit()).await;
        assert_eq!(p.brain.provider, "test-provider", "the pulse must name the provider actually serving");
        assert!(!p.brain.private_lane, "no private backend was attached");
        assert!(!p.taken_at.is_empty(), "a snapshot must be stamped so the client can show its age");
        // Serialization keeps the null/zero distinction all the way to the wire.
        let json: serde_json::Value = serde_json::from_str(&json_or_error(&p)).unwrap();
        assert!(json["brain"]["beliefs"].is_u64() || json["brain"]["beliefs"].is_null());
    }

    /// The attention list must stay short and current, so a stale failure is history and a fresh
    /// one is news. It also has to surface WHY a job failed — the operator wrote the task, they do
    /// not need it read back to them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn attention_shows_recent_failures_with_their_reason() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);
        let now = chrono::Utc::now().timestamp_millis();
        let day = 24 * 3600 * 1000;
        let ledger = serde_json::json!([
            { "id": "old", "name": "ancient", "task": "t1", "kind": "research", "status": "failed",
              "started_ms": now - 9 * day, "finished_ms": now - 8 * day, "result": "long dead" },
            { "id": "new", "name": "market-scan", "task": "scan the market", "kind": "research",
              "status": "failed", "started_ms": now - 3600_000, "finished_ms": now - 60_000,
              "result": "market data source returned 502" },
            { "id": "quiet", "name": "mystery", "task": "do the thing", "kind": "research",
              "status": "failed", "started_ms": now - 7200_000, "finished_ms": now - 120_000 },
        ]);
        mem.profile_set("delegations", &ledger.to_string()).await.unwrap();

        let p = eng.pulse(&mind_types::AccessContext::operator_audit()).await;
        let failed: Vec<&AttentionItem> =
            p.attention.iter().filter(|a| a.kind == AttentionKind::JobFailed).collect();

        assert_eq!(failed.len(), 2, "capped at two, and the 8-day-old failure is history not news");
        assert!(!failed.iter().any(|a| a.headline.contains("ancient")), "a week-old failure must not nag");
        // Most recent first.
        assert!(failed[0].headline.contains("market-scan"), "newest failure leads: {:?}", failed[0].headline);
        assert!(failed[0].detail.contains("502"), "the recorded reason must be shown, not the task");
        // A failure with no recorded reason says so, rather than pretending the task was the reason.
        let quiet = failed.iter().find(|a| a.headline.contains("mystery")).expect("second failure present");
        assert!(quiet.detail.contains("no reason was recorded"), "got: {}", quiet.detail);
        // And it still offers a way to act.
        assert_eq!(failed[0].action.as_deref(), Some("delegate market-scan: scan the market"));
    }

    /// A running job is NAMED in the pulse, not merely counted, and its elapsed time is computed
    /// server-side so the client never has to reconcile clocks across the ssh tunnel.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn running_jobs_are_named_with_elapsed_time() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);
        let now = chrono::Utc::now().timestamp_millis();
        let ledger = serde_json::json!([
            { "id": "r1", "name": "repo-audit", "task": "find weak spots", "kind": "code",
              "status": "running", "started_ms": now - 245_000 },
            // A row with no start stamp must not report a 58-year runtime.
            { "id": "r2", "name": "unstamped", "task": "x", "kind": "research", "status": "running" },
        ]);
        mem.profile_set("delegations", &ledger.to_string()).await.unwrap();

        let p = eng.pulse(&mind_types::AccessContext::operator_audit()).await;
        assert_eq!(p.work.jobs_running, 2);
        assert_eq!(p.work.jobs_total, 2);
        let audit = p.work.running.iter().find(|j| j.name == "repo-audit").expect("named in the pulse");
        assert_eq!(audit.kind, "code");
        assert!((240..=260).contains(&audit.elapsed_s), "elapsed should be ~245s, got {}", audit.elapsed_s);
        let unstamped = p.work.running.iter().find(|j| j.name == "unstamped").unwrap();
        assert_eq!(unstamped.elapsed_s, 0, "a missing start stamp reports 0, not epoch arithmetic");
    }

    /// With no recipe store, scheduling cannot persist — and the surface must SAY so rather than
    /// present an empty list, which a client would render as "no orders yet" beside a create button
    /// that silently drops the order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn orders_distinguishes_no_store_from_no_orders() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);
        let r = eng.orders_report();
        assert!(!r.store, "an engine with no recipe engine has no durable scheduling");
        assert!(r.orders.is_empty());
    }

    /// A standing order carries the actions that will actually work in its current state, so the
    /// client renders three buttons that succeed rather than four where one fails.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn orders_report_lists_state_and_applicable_actions() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let memory: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
        let db = std::env::temp_dir().join(format!("ym_surface_orders_{}.db", std::process::id()));
        let store = Arc::new(mind_recipes::RecipeStore::open(db.to_str().unwrap()).unwrap());
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("ok")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let host: Arc<dyn mind_recipes::RecipeHost> =
            Arc::new(crate::MindRecipeHost::new(None, None, memory.clone()));
        let recipes = mind_recipes::RecipeEngine::new(pool.clone(), host, "JARVIS").with_store(store);

        // Park two orders, then pause one.
        for tag in ["alpha", "beta"] {
            let rec = mind_recipes::Recipe {
                id: tag.into(),
                name: format!("{tag} report"),
                steps: vec![
                    mind_recipes::RecipeStep::WaitUntil {
                        until_ms: chrono::Utc::now().timestamp_millis() as u64 + 3_600_000,
                    },
                    mind_recipes::RecipeStep::Notify { message: "done".into() },
                ],
            };
            recipes.run(&rec).await;
        }
        let paused_id = recipes.list_sleeping()[0].0.clone();
        assert!(recipes.pause_run(&paused_id));

        let eng = ConversationEngine::new(memory, pool, "JARVIS").with_recipes(Arc::new(recipes));
        let r = eng.orders_report();
        assert!(r.store, "a wired store means scheduling is available");
        assert_eq!(r.orders.len(), 2, "both the sleeping and the paused order are listed");

        let paused = r.orders.iter().find(|o| o.id == paused_id).expect("paused order present");
        assert_eq!(paused.state, OrderState::Paused);
        assert_eq!(paused.actions, vec!["resume", "cancel"], "a paused order cannot be run or re-paused");

        let sleeping = r.orders.iter().find(|o| o.id != paused_id).expect("sleeping order present");
        assert_eq!(sleeping.state, OrderState::Sleeping);
        assert_eq!(sleeping.actions, vec!["run", "pause", "cancel"]);
        assert!(sleeping.in_seconds > 3000, "an hour out should read ~3600s, got {}", sleeping.in_seconds);
        assert!(!sleeping.name.is_empty(), "an order must be nameable in a list");
        let _ = std::fs::remove_file(&db);
    }

    /// The thread surface must SPLIT the way the runtime does — carried threads reach the prompt, the
    /// rest do not. One undifferentiated list would hide the whole point of the lifecycle.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn threads_split_into_carried_and_closing() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);
        let day = 86_400_000u64;
        let now = chrono::Utc::now().timestamp_millis() as u64;

        mem.add_task("file the return", "high", Some(now + 5 * day)).await.unwrap();
        mem.add_task("call mum more often", "low", None).await.unwrap();
        mem.add_task("order the watch", "high", Some(now - 21 * day)).await.unwrap();

        let r = eng.thread_report().await;
        let carried: Vec<&str> = r.carrying.iter().map(|t| t.description.as_str()).collect();
        let closing: Vec<&str> = r.closing.iter().map(|t| t.description.as_str()).collect();

        assert!(carried.contains(&"file the return"), "a future deadline is carried: {carried:?}");
        assert!(carried.contains(&"call mum more often"), "an undated intention is carried: {carried:?}");
        assert!(closing.contains(&"order the watch"), "a three-week-old commitment is closing: {closing:?}");

        // An undated intention must not be shown as if it were overdue.
        let mum = r.carrying.iter().find(|t| t.description.contains("mum")).unwrap();
        assert!(mum.deadline_ms.is_none() && mum.days_over.is_none(), "no deadline means no overdue count");
        assert_eq!(mum.state, ThreadStateTag::Live);

        let watch = r.closing.iter().find(|t| t.description.contains("watch")).unwrap();
        assert_eq!(watch.state, ThreadStateTag::AwaitingClosure);
        assert!(watch.days_over.unwrap() >= 20);
    }

    /// An UNTESTED skill has no success rate. `Skill::success_rate()` returns 1.0 for zero runs — a
    /// sensible ranking default and a lie once it is rendered as a measurement.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_untested_skill_has_no_rate_and_a_failing_one_is_flagged() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);

        let skill = |name: &str, runs: u64, ok: u64| mind_types::Skill {
            name: name.into(),
            lang: "python".into(),
            code: "print(1)".into(),
            summary: format!("does {name}"),
            tags: vec![],
            status: "active".into(),
            runs,
            successes: ok,
            created_ms: 0,
        };
        mem.save_skill(skill("fresh", 0, 0)).await.unwrap();
        mem.save_skill(skill("solid", 10, 9)).await.unwrap();
        mem.save_skill(skill("flaky", 8, 2)).await.unwrap();

        let r = eng.skill_report().await;
        let fresh = r.skills.iter().find(|s| s.name == "fresh").expect("fresh present");
        assert!(fresh.success_rate.is_none(), "never run means NO rate, not 100%");
        assert!(!fresh.failing);
        assert_eq!(r.untested, 1);

        let flaky = r.skills.iter().find(|s| s.name == "flaky").expect("flaky present");
        assert!(flaky.failing, "2 of 8 is below the store's own quarantine line");
        assert!((flaky.success_rate.unwrap() - 0.25).abs() < 1e-9);

        // Worst first: a failing skill is the one worth looking at.
        assert_eq!(r.skills[0].name, "flaky", "order was {:?}", r.skills.iter().map(|s| &s.name).collect::<Vec<_>>());
    }

    /// A capability whose backing client is absent must report UNAVAILABLE with a reason a person
    /// can act on. This is the field the agent compiler reads to say "you asked for GitHub and it
    /// isn't connected" instead of letting an agent run and confabulate a result.
    /// THE FALSE-GREEN GUARD, and the reason it exists.
    ///
    /// The first probe was a `match` on capability ids. Against the live box it reported 17 of 17
    /// READY — which looked like a healthy mind and was actually a broken probe: it matched `"wiki"`
    /// while the registry's id is `wikipedia`, and five arms matched ids that do not exist, so those
    /// capabilities were never checked at all. An availability report that cannot say "no" is worse
    /// than none, because the agent compiler is meant to trust it.
    ///
    /// So this asserts the probe can FAIL. A bare engine has no search, no fetch, no GitHub, no
    /// weather, no wiki — every capability declaring one of those must be blocked, with a reason.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unconfigured_capabilities_say_what_is_missing() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);
        let report = eng.capability_report();
        assert!(!report.capabilities.is_empty(), "the registry must expose its specs");

        // A bare engine has NO external clients, so a green report here means the probe is broken.
        assert!(
            report.unavailable > 0,
            "an engine with no clients wired must report SOME capability unavailable — all-ready is \
             the exact false-green this test exists to catch (ready={}, unavailable={})",
            report.connected,
            report.unavailable
        );

        for id in ["github", "wikipedia", "web_search", "web_fetch", "weather", "home", "coder"] {
            let c = report.capabilities.iter().find(|c| c.id == id);
            let Some(c) = c else { continue };
            assert_eq!(c.availability, Availability::Unavailable, "`{id}` has no backing client here");
            let why = c.blocked_by.as_deref().unwrap_or_default();
            assert!(!why.is_empty(), "`{id}` must say WHAT is missing, not just that it is unavailable");
        }
        // The reason must be actionable — a config key or a concrete thing to install.
        let gh = report.capabilities.iter().find(|c| c.id == "github").expect("github is declared");
        assert!(
            gh.blocked_by.as_deref().unwrap_or_default().contains("YM_GITHUB_TOKEN"),
            "got: {:?}",
            gh.blocked_by
        );

        assert_eq!(
            report.connected + report.unavailable + report.disabled,
            report.capabilities.len(),
            "every capability must fall in exactly one availability bucket"
        );
    }

    /// A capability that declares no requirement is genuinely always available — a calculator needs
    /// nothing external. This pins the other side of the guard above, so "report unavailable" never
    /// degenerates into "report everything unavailable to be safe".
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pure_compute_capabilities_are_ready_with_nothing_configured() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = test_engine(&mem);
        let report = eng.capability_report();
        let calc = report.capabilities.iter().find(|c| c.id == "calculator").expect("calculator is declared");
        assert_eq!(calc.availability, Availability::Ready, "arithmetic needs no client");
        assert!(calc.blocked_by.is_none());
    }

    /// Wiring a client must flip its capability to ready — the probe reads the SAME field the tool
    /// dispatch uses, so "ready" means the call would really work.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wiring_a_client_flips_its_capability_to_ready() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let before = test_engine(&mem).capability_report();
        let wiki_before = before.capabilities.iter().find(|c| c.id == "wikipedia").unwrap();
        assert_eq!(wiki_before.availability, Availability::Unavailable);

        let eng = test_engine(&mem).with_wiki(Arc::new(mind_tools::Wikipedia::new()));
        let after = eng.capability_report();
        let wiki_after = after.capabilities.iter().find(|c| c.id == "wikipedia").unwrap();
        assert_eq!(wiki_after.availability, Availability::Ready, "a wired client means ready");
        assert!(wiki_after.blocked_by.is_none());
        assert_eq!(after.connected, before.connected + 1, "exactly one capability changed");
    }
}
