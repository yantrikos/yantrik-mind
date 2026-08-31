//! PHASE 3B RED EXECUTIVE ORACLE (`docs/PHASE3_WORLD_STATE_V1.md` lineage, E.EX0).
//!
//! Truth is frozen (`world-state-v1.1`). This benchmark asks a NEW question the current
//! system has never been forced to answer:
//!
//! > Given a trustworthy situation, does it deserve ACT, MONITOR, or deliberate IGNORE?
//!
//! By contract this file implements NO executive. It declares independent ground truth
//! (expected posture + reason category + hand-authored outcome table per situation),
//! probes TODAY'S system through the only doors that exist, scores separately, and
//! reports a confusion matrix plus silence-credit metrics. Correct silence is competence.
//! Gated behind `YM_EXEC_3B=1`; run with --nocapture.

use mind_memory::MemoryHandle;
use mind_types::{AccessContext, MemoryFacade};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Posture {
    Ignore,
    Monitor,
    Act,
}
impl Posture {
    fn name(self) -> &'static str {
        match self {
            Posture::Ignore => "IGNORE",
            Posture::Monitor => "MONITOR",
            Posture::Act => "ACT",
        }
    }
}

#[derive(Clone)]
pub struct Situation {
    pub id: &'static str,
    pub family: &'static str,
    pub facts: &'static str,
    pub want: Posture,
    pub reason: &'static str,
    /// Hand-authored ground-truth expected cost of each posture [ignore, monitor, act] (#13).
    pub outcomes: [u8; 3],
    pub window_open: bool,
    pub commitment_minutes: Option<i64>,
    pub resource_block: Option<&'static str>,
    pub user_in_meeting: bool,
    pub requires_user_interrupt: bool,
    pub late_recovery: bool,
    /// Set-scoring (#9): additional candidate decisions inside one shared context.
    pub candidates: &'static [(&'static str, Posture)],
}

impl Default for Situation {
    fn default() -> Self {
        Situation {
            id: "?",
            family: "?",
            facts: "",
            want: Posture::Monitor,
            reason: "",
            outcomes: [3, 1, 3],
            window_open: true,
            commitment_minutes: None,
            resource_block: None,
            user_in_meeting: false,
            requires_user_interrupt: false,
            late_recovery: false,
            candidates: &[],
        }
    }
}

fn s(
    id: &'static str,
    family: &'static str,
    facts: &'static str,
    want: Posture,
    reason: &'static str,
    outcomes: [u8; 3],
) -> Situation {
    Situation {
        id,
        family,
        facts,
        want,
        reason,
        outcomes,
        ..Default::default()
    }
}

/// THE EXECUTIVE SITUATIONS — hand-authored; families per PHASE 3B directive.
fn situations() -> Vec<Situation> {
    let mut v = vec![
    // ── A. IGNORE: harmless noise ──
    s("pkg_late_carrier_tonight", "A-ignore",
        "package one day late; carrier confirmed delivery tonight; no downstream dependency; no useful user action",
        Posture::Ignore, "no meaningful consequence / already handled", [0, 1, 3]),
    s("stale_eta_after_delivered", "A-ignore",
        "old ETA email says Monday; authoritative carrier scan says delivered-Saturday; work item closed",
        Posture::Ignore, "already resolved; weaker late evidence must not resurrect work", [0, 1, 3]),
    s(
        "meeting_note_minuted",
        "A-ignore",
        "calendar ping about a meeting whose notes are already filed under entity meeting.notes",
        Posture::Ignore,
        "already handled",
        [0, 1, 2],
    ),
    s(
        "dup_calendar_ping",
        "A-ignore",
        "duplicate calendar event identity re-delivered (source_event_id seen)",
        Posture::Ignore,
        "duplicate ingestion; nothing new",
        [0, 0, 2],
    ),
    s(
        "newsletter_digest",
        "A-ignore",
        "low-value newsletter batch arrived; optional reading only",
        Posture::Ignore,
        "small benefit + interruption cost => ignore",
        [0, 1, 2],
    ),
    // ── B. MONITOR: uncertainty with future trigger ──
    s("venue_conflict_tonight", "B-monitor",
        "meeting.location Conflicted(Room4, Zoom); meeting tomorrow; authoritative confirmation expected tonight",
        Posture::Monitor, "do not act on one side of unresolved world state", [2, 0, 2]),
    s("alice_doc_inprogress_wed", "B-monitor",
        "Alice owes document, deadline Friday; today Wednesday; Alice confirmed Tuesday in-progress",
        Posture::Monitor, "waiting on someone; deadline not near", [2, 0, 2]),
    s(
        "balance_bank_feed_conflict",
        "B-monitor",
        "account balance Conflicted(bank-statement, app-cache); reconciliation runs tonight",
        Posture::Monitor,
        "conflicted input; resolver scheduled",
        [2, 0, 3],
    ),
    s(
        "price_watch_far",
        "B-monitor",
        "price dropped on watched item; threshold for worthwhile buy not reached; no deadline",
        Posture::Monitor,
        "potentially relevant; evidence insufficient to act",
        [1, 0, 2],
    ),
    ];
    // ── C. ACT: expiring intervention window ──
    let mut pp = s("passport_window_open", "C-act",
        "passport expires before booked international trip; renewal lead time approaching minimum safe window; high confidence",
        Posture::Act, "intervention window open; cost of inaction exceeds intervention", [3, 2, 0]);
    pp.requires_user_interrupt = true; // only Pranab can renew a passport
    v.push(pp);
    let mut dd = s("doc_due_2h_dependency", "C-act",
        "document due in 2 hours; still unresolved; meeting depends on it; responsible party known; wait already elapsed",
        Posture::Act, "dependency failure; window closing", [3, 2, 0]);
    dd.requires_user_interrupt = true; // the responsible party must be reached
    v.push(dd);
    // ── D/E folded above; ── F. IGNORE: resolved ──
    v.push(s(
        "flight_refunded_auto",
        "F-resolved",
        "flight risk detected earlier; ticket already cancelled and refunded automatically",
        Posture::Ignore,
        "already resolved",
        [0, 1, 3],
    ));
    v.push(s(
        "key_rotated_old_alert",
        "F-resolved",
        "alert references deploy-key active-v1; authoritative state rotated-v2 since",
        Posture::Ignore,
        "superseded state; alert stale",
        [0, 1, 2],
    ));
    v.push(s(
        "commitment_met_today",
        "F-resolved",
        "promised form submitted this morning; receipt stored",
        Posture::Ignore,
        "commitment satisfied; invented obligation forbidden",
        [0, 1, 2],
    ));
    // ── G. Deadline curves: same issue, different times ──
    let mut curve = |id: &'static str,
                     days: i64,
                     want: Posture,
                     reason: &'static str,
                     out: [u8; 3],
                     interrupt: bool,
                     late: bool| {
        let mut sit = s(
            id,
            "G-curve",
            "outdoor event; weak rain signal; renewal-style preparation task",
            want,
            reason,
            out,
        );
        sit.requires_user_interrupt = interrupt;
        sit.late_recovery = late;
        sit.window_open = !late;
        let _ = days;
        v.push(sit);
    };
    curve(
        "event_prep_T21d",
        21,
        Posture::Monitor,
        "window known but unopened: scheduled reconsideration",
        [1, 0, 3],
        false,
        false,
    );
    curve(
        "weather_T14d",
        14,
        Posture::Monitor,
        "uncertain signal; watch for confidence",
        [1, 0, 3],
        false,
        false,
    );
    curve(
        "weather_T10d_decision",
        10,
        Posture::Monitor,
        "decision deadline approaching; still low confidence",
        [1, 0, 2],
        false,
        false,
    );
    curve(
        "prep_T2d_internal",
        2,
        Posture::Act,
        "act internally without interrupting (reserve equipment)",
        [2, 1, 0],
        false,
        false,
    );
    curve(
        "warn_T4h_interrupt",
        0,
        Posture::Act,
        "intervention window nearly closed; user must decide",
        [3, 2, 0],
        true,
        false,
    );
    curve(
        "recovery_T_plus_1h",
        0,
        Posture::Act,
        "missed window; recovery mode",
        [3, 2, 1],
        true,
        true,
    );
    // ── Commitments outrank opportunities (#8) ──
    let mut cm = s("form_90min_promise", "H-commitment",
        "user promised to submit form today; deadline 90 minutes; unrelated medium-benefit price drop also present",
        Posture::Act, "commitment deadline converging; interrupts opportunity", [3, 1, 0]);
    cm.commitment_minutes = Some(90);
    cm.requires_user_interrupt = false; // submit internally, confirm quietly
    v.push(cm);
    v.push(s(
        "call_mom_evening",
        "H-commitment",
        "promised evening call; now morning; plenty of margin",
        Posture::Monitor,
        "commitment tracked; wrong time to act",
        [1, 0, 2],
    ));
    v.push(s(
        "opportunity_vs_promise",
        "H-commitment",
        "medium-benefit opportunity while a promise deadline converges",
        Posture::Ignore,
        "opportunity yields to commitment",
        [1, 1, 2],
    ));
    // ── Competing candidate sets (#9): score SETS of decisions ──
    let mut set1 = s("candidate_set_base", "I-competing",
        "A urgent-but-auto-handled; B deadline tomorrow; C optional opportunity; D uncertain future risk",
        Posture::Act, "set anchor: B acts; others distributed", [2, 1, 0]);
    set1.candidates = &[
        ("A_handled", Posture::Ignore),
        ("B_deadline", Posture::Act),
        ("C_optional", Posture::Ignore),
        ("D_risk", Posture::Monitor),
    ];
    v.push(set1);
    let mut set2 = s(
        "candidate_set_resource_blocked",
        "I-competing",
        "same set as base but token budget exhausted and network unavailable for B's action",
        Posture::Monitor,
        "resource scarcity demotes ACT to MONITOR; need persists",
        [1, 0, 3],
    );
    set2.resource_block = Some("budget+network");
    set2.candidates = &[
        ("A_handled", Posture::Ignore),
        ("B_blocked", Posture::Monitor),
        ("C_optional", Posture::Ignore),
        ("D_risk", Posture::Monitor),
    ];
    v.push(set2);
    // ── Resource scarcity standalone (#10) ──
    let mut netdown = s(
        "refresh_network_down",
        "J-resource",
        "state refresh would resolve uncertainty; network unavailable now",
        Posture::Monitor,
        "safe action cannot execute now; need persists",
        [1, 0, 3],
    );
    netdown.resource_block = Some("network");
    v.push(netdown);
    let mut asleep = s(
        "user_sleeping_low_urgency",
        "J-resource",
        "low urgency item surfacing while user sleeps",
        Posture::Monitor,
        "receptivity zero; defer surface",
        [1, 0, 2],
    );
    asleep.user_in_meeting = false;
    v.push(asleep);
    // ── Receptivity (#7) ──
    let mut mtg = s(
        "low_urg_during_meeting",
        "K-receptivity",
        "low urgency item; user currently in meeting",
        Posture::Monitor,
        "interrupt cost maximal now",
        [1, 0, 3],
    );
    mtg.user_in_meeting = true;
    v.push(mtg);
    v.push(s(
        "same_item_user_free",
        "K-receptivity",
        "same low urgency item; user free; act internally then surface summary",
        Posture::Act,
        "receptive context; internal action cheap",
        [1, 1, 0],
    ));
    // ── Waiting-on-someone convergence (#12) ──
    let mut afm = s(
        "alice_friday_morning_missing",
        "L-wait",
        "Friday morning; promised document not delivered; meeting depends on it Monday-prep",
        Posture::Act,
        "grace elapsed; intervene now",
        [3, 2, 0],
    );
    afm.requires_user_interrupt = true;
    v.push(afm);
    let mut late = s(
        "alice_friday_passed_no_doc",
        "L-wait",
        "Friday end of day; document still missing; dependency meeting already started prep",
        Posture::Act,
        "late recovery; escalate differently",
        [3, 2, 1],
    );
    late.late_recovery = true;
    late.window_open = false;
    late.requires_user_interrupt = true;
    v.push(late);
    v
}

#[derive(Debug, PartialEq)]
enum ProbeDecision {
    Unrepresentable,
    RecallOnly,
    #[expect(
        dead_code,
        reason = "reserved for the future executive API whose absence this baseline oracle measures"
    )]
    Chose(Posture),
}

async fn probe(mem: &MemoryHandle, sit: &Situation) -> ProbeDecision {
    // The ONLY executive-ish door today: semantic belief recall over transcript-fed text.
    // There is no posture API, no arbitration seam, no silence credit anywhere (verified E.EX0).
    let ctx = AccessContext::operator_audit();
    let needle = sit.id.replace('_', " ");
    match mem.beliefs_matching_n(&needle, 3, &ctx).await {
        Ok(hits) if !hits.is_empty() => ProbeDecision::RecallOnly,
        _ => ProbeDecision::Unrepresentable,
    }
}

/// Oracle self-check (#17 pre-run): an inconsistent expectation is ORACLE_ERROR, recorded
/// before any scoring. Expected posture must be (co-)minimal in the outcome table; ACT
/// requires an open window and strictly better outcome than ignoring.
fn self_check(sits: &[Situation]) -> usize {
    let mut errs = 0;
    for sit in sits {
        if sit.id == "?"
            || sit.family == "?"
            || sit.facts.trim().is_empty()
            || sit.reason.trim().is_empty()
        {
            println!("ORACLE_ERROR {} has incomplete fixture metadata", sit.id);
            errs += 1;
        }
        if sit.outcomes.iter().any(|cost| *cost > 3) {
            println!(
                "ORACLE_ERROR {} has an outcome cost outside 0..=3: {:?}",
                sit.id, sit.outcomes
            );
            errs += 1;
        }
        let min = sit.outcomes.iter().min().copied().unwrap();
        if sit.outcomes[sit.want as usize] > min {
            println!(
                "ORACLE_ERROR {} want={:?} not outcome-minimal {:?}",
                sit.id, sit.want, sit.outcomes
            );
            errs += 1;
        }
        if sit.late_recovery && sit.window_open {
            println!(
                "ORACLE_ERROR {} marks recovery while the original window is open",
                sit.id
            );
            errs += 1;
        }
        if sit.want == Posture::Act
            && ((!sit.window_open && !sit.late_recovery) || sit.outcomes[0] <= sit.outcomes[2])
        {
            println!(
                "ORACLE_ERROR {} ACT lacks open-window/cost justification",
                sit.id
            );
            errs += 1;
        }
    }
    errs
}

#[test]
fn executive_fixture_truth_is_internally_consistent() {
    let sits = situations();
    assert_eq!(
        self_check(&sits),
        0,
        "executive oracle fixtures must be coherent before scoring"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phase3b_red_executive_baseline() {
    if std::env::var("YM_EXEC_3B").as_deref() != Ok("1") {
        println!("EXEC-ORACLE: gated (set YM_EXEC_3B=1)");
        return;
    }
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let sits = situations();

    let oracle_errors = self_check(&sits);
    assert_eq!(
        oracle_errors, 0,
        "executive oracle internally inconsistent ({oracle_errors})"
    );

    // Score every decision (single + expanded candidate sets) against today's system.
    let mut decisions: Vec<(String, Posture, ProbeDecision)> = Vec::new();
    for sit in &sits {
        if sit.candidates.is_empty() {
            let d = probe(&mem, sit).await;
            decisions.push((sit.id.to_string(), sit.want, d));
        } else {
            for (cid, want) in sit.candidates {
                let d = probe(&mem, sit).await; // one door per context today
                decisions.push((format!("{}::{cid}", sit.id), *want, d));
            }
        }
    }

    let mut matrix = [[0usize; 4]; 3]; // rows actual I/M/A, cols predicted I/M/A/UNREP
    let idx = |p: Posture| p as usize;
    for (_id, want, got) in &decisions {
        let g = match got {
            ProbeDecision::Chose(p) => idx(*p),
            _ => 3,
        };
        matrix[idx(*want)][g] += 1;
    }
    let class = |p: Posture| -> (usize, usize) {
        let (mut tp, mut fp) = (0, 0);
        for (_i, w, g) in &decisions {
            let gp = match g {
                ProbeDecision::Chose(p) => Some(*p),
                _ => None,
            };
            if gp == Some(p) {
                if *w == p {
                    tp += 1;
                } else {
                    fp += 1;
                }
            }
        }
        (tp, fp)
    };
    let (atp, afp) = class(Posture::Act);
    let (mtp, mfp) = class(Posture::Monitor);
    let (itp, ifp) = class(Posture::Ignore);
    let total = decisions.len();
    let unrep = decisions
        .iter()
        .filter(|(_, _, g)| *g == ProbeDecision::Unrepresentable)
        .count();
    let recall_only = decisions
        .iter()
        .filter(|(_, _, g)| *g == ProbeDecision::RecallOnly)
        .count();
    let recall = |p: Posture| decisions.iter().filter(|(_, w, _)| *w == p).count();
    let correct_silence = matrix[0][0];
    let unnecessary_action = matrix[0][2] + matrix[1][2];
    let missed_intervention = matrix[2][0] + matrix[2][1] + matrix[2][3];

    println!("PHASE 3B RED EXECUTIVE BASELINE");
    println!(
        "  decisions scored: {total} (situations {}, set-expanded)",
        sits.len()
    );
    println!("  EX0 FROZEN BASELINE (starting record, superseded by scoped coverage above): representable 0/37, recall-only fragments: {recall_only}, unrepresentable: {unrep}");
    println!(
        "  ACT      precision {atp}/{} recall {}/{}",
        atp + afp,
        atp,
        recall(Posture::Act)
    );
    println!(
        "  MONITOR  precision {mtp}/{} recall {}/{}",
        mtp + mfp,
        mtp,
        recall(Posture::Monitor)
    );
    println!(
        "  IGNORE   precision {itp}/{} recall {}/{}",
        itp + ifp,
        itp,
        recall(Posture::Ignore)
    );
    println!("  correct_silence={correct_silence} unnecessary_action={unnecessary_action} missed_intervention={missed_intervention}");
    println!("  confusion (rows=actual I/M/A, cols=pred I/M/A/UNREPRESENTABLE):");
    for (posture, row) in [Posture::Ignore, Posture::Monitor, Posture::Act]
        .into_iter()
        .zip(&matrix)
    {
        println!(
            "    {}: {:?}  total={}",
            posture.name(),
            row,
            row.iter().sum::<usize>()
        );
    }
    println!("  capability verification: executive choice abstraction ABSENT; central arbitration ABSENT;");
    println!(
        "    silence credit ABSENT; monitor semantics ABSENT; cross-organ prioritization ABSENT;"
    );
    println!("    mind-proactive pipeline+commitment ledger: doc-comment stub only; belief-recall fragments only.");

    // ── EX1: posture semantics earned for TEN decisions (E.EX1) ─────────────────────────
    const DAY: i64 = 86_400_000;
    // Production mapping uses OBSERVABLE fixture facts ONLY; outcome tables stay oracle-side.
    const EX1_SCOPE: &[&str] = &[
        "stale_eta_after_delivered",
        "flight_refunded_auto",
        "key_rotated_old_alert",
        "meeting_note_minuted",
        "weather_T14d",
        "weather_T10d_decision",
        "doc_due_2h_dependency",
        "passport_window_open",
        "prep_T2d_internal",
        "warn_T4h_interrupt",
    ];
    let ex1_candidate = |id: &str| -> mind_proactive::ExecutiveCandidate {
        use mind_proactive::ExecutiveCandidate;
        let mut c = ExecutiveCandidate {
            candidate_id: id.into(),
            source_ref: format!("world:{id}"),
            now_ms: 0,
            urgency: 1,
            deadline_at_ms: None,
            already_resolved: false,
            useful_action_available: false,
            internal_capability: true,
            blocked: false,
            waiting_on_someone: false,
            intervention_window_open: false,
            execution_cost: 1,
            interruption_cost: 2,
            risk: 1,
            confidence: 0.95,
            intervention_not_before_ms: None,
            intervention_by_ms: None,
            interrupt_lead_ms: None,
            commitment: None,
            converging_obligation_due_ms: None,
            wait_grace_until_ms: None,
            resources: None,
        };
        match id {
            "stale_eta_after_delivered"
            | "flight_refunded_auto"
            | "key_rotated_old_alert"
            | "meeting_note_minuted" => {
                c.already_resolved = true;
            }
            "weather_T14d" | "weather_T10d_decision" => {
                c.deadline_at_ms = Some(if id.ends_with("14d") {
                    14 * DAY
                } else {
                    10 * DAY
                });
                c.urgency = 1;
            }
            "doc_due_2h_dependency" => {
                c.deadline_at_ms = Some(2 * 3_600_000);
                c.useful_action_available = true;
                c.intervention_window_open = true;
                c.internal_capability = false; // needs the responsible party, i.e. the user's channel
                c.urgency = 3;
            }
            "passport_window_open" => {
                c.deadline_at_ms = Some(30 * DAY);
                c.useful_action_available = true;
                c.intervention_window_open = true; // renewal lead-time window is open NOW
                c.internal_capability = false; // only Pranab can renew a passport
                c.urgency = 3;
            }
            "prep_T2d_internal" | "warn_T4h_interrupt" => {
                c.useful_action_available = true;
                c.intervention_window_open = true;
                c.internal_capability = id == "prep_T2d_internal";
                c.urgency = if id.starts_with("warn") { 3 } else { 2 };
            }
            other => unreachable!("EX1 scope drift: {other}"),
        }
        c
    };
    let mut ex1_total = 0usize;
    let mut ex1_correct = 0usize;
    let mut ex1_failures: Vec<String> = Vec::new();
    for sit in &sits {
        if !EX1_SCOPE.contains(&sit.id) {
            continue;
        }
        ex1_total += 1;
        let cand = ex1_candidate(sit.id);
        let d = mind_proactive::arbitrate(&cand);
        let got_posture = match d.posture {
            mind_proactive::Posture::Ignore => Posture::Ignore,
            mind_proactive::Posture::Monitor => Posture::Monitor,
            mind_proactive::Posture::Act => Posture::Act,
        };
        let mut ok = got_posture == sit.want;
        // MONITOR must answer "what would cause me to reconsider?"
        if got_posture == Posture::Monitor {
            ok = ok
                && d.monitor
                    .as_ref()
                    .map(|m| !m.wake_when.is_empty() || m.review_at_ms.is_some())
                    .unwrap_or(false);
        }
        // ACT interrupt flag must match the authored expectation (#6)
        if got_posture == Posture::Act {
            ok = ok && d.requires_user_interrupt == sit.requires_user_interrupt;
        }
        if ok {
            ex1_correct += 1;
        } else {
            ex1_failures.push(format!(
                "{} got {:?}/{:?} want {:?}",
                sit.id, d.posture, d.reason_code, sit.want
            ));
        }
    }
    println!(
        "EX1 SCOPE: {ex1_correct}/{ex1_total} GREEN {}",
        if ex1_failures.is_empty() {
            String::new()
        } else {
            format!("{ex1_failures:?}")
        }
    );

    // ── EX2: temporal escalation — SIX curve decisions from ONE unchanged situation ─────
    // Only now_ms differs across evaluations; no new events, no priority scores.
    const EX2_SCOPE: &[&str] = &[
        "event_prep_T21d",
        "weather_T14d",
        "weather_T10d_decision",
        "prep_T2d_internal",
        "warn_T4h_interrupt",
        "recovery_T_plus_1h",
    ];
    let d2 = 30 * DAY;
    let ex2_now = |id: &str| match id {
        "event_prep_T21d" => d2 - 21 * DAY,
        "weather_T14d" => d2 - 14 * DAY,
        "weather_T10d_decision" => d2 - 10 * DAY,
        "prep_T2d_internal" => d2 - 2 * DAY,
        "warn_T4h_interrupt" => d2 - 4 * 3_600_000,
        _ => d2 + 3_600_000, // recovery_T_plus_1h
    };
    let mut ex2_total = 0usize;
    let mut ex2_correct = 0usize;
    let mut ex2_failures: Vec<String> = Vec::new();
    for sit in &sits {
        if !EX2_SCOPE.contains(&sit.id) {
            continue;
        }
        ex2_total += 1;
        let cand = mind_proactive::ExecutiveCandidate {
            candidate_id: sit.id.into(),
            source_ref: format!("world:{}", sit.id),
            now_ms: ex2_now(sit.id),
            urgency: 2,
            deadline_at_ms: Some(d2),
            already_resolved: false,
            useful_action_available: true,
            internal_capability: true,
            blocked: false,
            waiting_on_someone: false,
            intervention_window_open: false,
            execution_cost: 1,
            interruption_cost: 2,
            risk: 1,
            confidence: 0.95,
            intervention_not_before_ms: Some(d2 - 9 * DAY),
            intervention_by_ms: Some(d2),
            interrupt_lead_ms: Some(4 * 3_600_000),
            commitment: None,
            converging_obligation_due_ms: None,
            wait_grace_until_ms: None,
            resources: None,
        };
        let dec = mind_proactive::arbitrate(&cand);
        let got = match dec.posture {
            mind_proactive::Posture::Ignore => Posture::Ignore,
            mind_proactive::Posture::Monitor => Posture::Monitor,
            mind_proactive::Posture::Act => Posture::Act,
        };
        let mut ok = got == sit.want && dec.requires_user_interrupt == sit.requires_user_interrupt;
        if got == Posture::Monitor {
            ok = ok
                && dec
                    .monitor
                    .as_ref()
                    .map(|m| !m.wake_when.is_empty())
                    .unwrap_or(false);
        }
        // recovery must never masquerade as ordinary action
        if sit.late_recovery {
            ok = ok && dec.reason_code == "deadline_missed_recovery";
        }
        if ok {
            ex2_correct += 1;
        } else {
            ex2_failures.push(format!(
                "{} got {:?}/{}",
                sit.id, dec.posture, dec.reason_code
            ));
        }
    }
    println!(
        "EX2 SCOPE: {ex2_correct}/{ex2_total} GREEN {}",
        if ex2_failures.is_empty() {
            String::new()
        } else {
            format!("{ex2_failures:?}")
        }
    );

    // ── EX3: obligations outrank opportunities; waits honor grace (view-based) ───────────
    const EX3_SCOPE: &[&str] = &[
        "form_90min_promise",
        "call_mom_evening",
        "opportunity_vs_promise",
        "alice_doc_inprogress_wed",
        "alice_friday_morning_missing",
        "alice_friday_passed_no_doc",
    ];
    let mut ex3_total = 0usize;
    let mut ex3_correct = 0usize;
    let mut ex3_failures: Vec<String> = Vec::new();
    for sit in &sits {
        if !EX3_SCOPE.contains(&sit.id) {
            continue;
        }
        ex3_total += 1;
        let mut cand = mind_proactive::ExecutiveCandidate {
            candidate_id: sit.id.into(),
            source_ref: format!("world:{}", sit.id),
            now_ms: 0,
            urgency: 1,
            deadline_at_ms: None,
            already_resolved: false,
            useful_action_available: false,
            internal_capability: true,
            blocked: false,
            waiting_on_someone: false,
            intervention_window_open: false,
            execution_cost: 1,
            interruption_cost: 2,
            risk: 1,
            confidence: 0.95,
            intervention_not_before_ms: None,
            intervention_by_ms: None,
            interrupt_lead_ms: None,
            commitment: None,
            converging_obligation_due_ms: None,
            wait_grace_until_ms: None,
            resources: None,
        };
        match sit.id {
            "form_90min_promise" => {
                cand.commitment = Some(mind_proactive::CommitmentView {
                    ref_id: "task:form-42".into(),
                    source_organ: "mind-tasks",
                    made_at_ms: 0,
                    due_at_ms: 90 * 60_000,
                    fulfilled: false,
                });
                cand.useful_action_available = true;
                cand.intervention_window_open = true;
            }
            "call_mom_evening" => {
                cand.commitment = Some(mind_proactive::CommitmentView {
                    ref_id: "promise:call-mom".into(),
                    source_organ: "promise-ledger",
                    made_at_ms: 0,
                    due_at_ms: 10 * 3_600_000,
                    fulfilled: false,
                });
            }
            "opportunity_vs_promise" => {
                cand.converging_obligation_due_ms = Some(90 * 60_000);
            }
            "alice_doc_inprogress_wed" => {
                cand.waiting_on_someone = true;
                cand.wait_grace_until_ms = Some(48 * 3_600_000);
                cand.deadline_at_ms = Some(72 * 3_600_000);
            }
            "alice_friday_morning_missing" => {
                cand.waiting_on_someone = true;
                cand.wait_grace_until_ms = Some(-3_600_000); // elapsed an hour ago
                cand.deadline_at_ms = Some(12 * 3_600_000);
                cand.useful_action_available = true;
                cand.intervention_window_open = true;
                cand.internal_capability = false;
            }
            _ => {
                // alice_friday_passed_no_doc
                cand.waiting_on_someone = true;
                cand.wait_grace_until_ms = Some(-6 * 3_600_000);
                cand.useful_action_available = true;
                cand.intervention_window_open = true;
                cand.internal_capability = false;
            }
        }
        let dec = mind_proactive::arbitrate(&cand);
        let got = match dec.posture {
            mind_proactive::Posture::Ignore => Posture::Ignore,
            mind_proactive::Posture::Monitor => Posture::Monitor,
            mind_proactive::Posture::Act => Posture::Act,
        };
        let mut ok = got == sit.want && dec.requires_user_interrupt == sit.requires_user_interrupt;
        if got == Posture::Monitor {
            ok = ok
                && dec
                    .monitor
                    .as_ref()
                    .map(|m| !m.wake_when.is_empty())
                    .unwrap_or(false);
        }
        if ok {
            ex3_correct += 1;
        } else {
            ex3_failures.push(format!(
                "{} got {:?}/{} want {:?}",
                sit.id, dec.posture, dec.reason_code, sit.want
            ));
        }
    }
    println!(
        "EX3 SCOPE: {ex3_correct}/{ex3_total} GREEN {}",
        if ex3_failures.is_empty() {
            String::new()
        } else {
            format!("{ex3_failures:?}")
        }
    );
    const EX4_SCOPE: &[&str] = &[
        "refresh_network_down",
        "user_sleeping_low_urgency",
        "low_urg_during_meeting",
        "same_item_user_free",
    ];
    let mut ex4_total = 0usize;
    let mut ex4_correct = 0usize;
    let mut ex4_failures: Vec<String> = Vec::new();
    for sit in &sits {
        if !EX4_SCOPE.contains(&sit.id) {
            continue;
        }
        ex4_total += 1;
        let mut cand = mind_proactive::ExecutiveCandidate {
            candidate_id: sit.id.into(),
            source_ref: format!("world:{}", sit.id),
            now_ms: 0,
            urgency: 1,
            deadline_at_ms: Some(48 * 3_600_000),
            already_resolved: false,
            useful_action_available: true,
            internal_capability: true,
            blocked: false,
            waiting_on_someone: false,
            intervention_window_open: true,
            execution_cost: 1,
            interruption_cost: 2,
            risk: 1,
            confidence: 0.95,
            intervention_not_before_ms: None,
            intervention_by_ms: None,
            interrupt_lead_ms: None,
            commitment: None,
            converging_obligation_due_ms: None,
            wait_grace_until_ms: None,
            resources: Some(mind_proactive::ResourceContextView {
                network_available: true,
                capability_available: true,
                budget_available: true,
                user_receptive: Some(true),
                quiet_hours: false,
                quiet_hours_end_ms: None,
            }),
        };
        match sit.id {
            "refresh_network_down" => cand.resources.as_mut().unwrap().network_available = false,
            // ISOLATED pin: meeting active = receptivity only; quiet hours false
            "low_urg_during_meeting" => {
                cand.internal_capability = false;
                cand.resources.as_mut().unwrap().user_receptive = Some(false);
            }
            // OVERLAP fixture: sleeping legitimately presents BOTH blockers; either
            // deterministic primary blocker is acceptable if its wake condition rides along
            "user_sleeping_low_urgency" => {
                cand.internal_capability = false;
                let r = cand.resources.as_mut().unwrap();
                r.user_receptive = Some(false);
                r.quiet_hours = true;
                r.quiet_hours_end_ms = Some(8 * 3_600_000);
            }
            _ => {}
        }
        let dec4 = mind_proactive::arbitrate(&cand);
        let got4 = match dec4.posture {
            mind_proactive::Posture::Ignore => Posture::Ignore,
            mind_proactive::Posture::Monitor => Posture::Monitor,
            mind_proactive::Posture::Act => Posture::Act,
        };
        let mut mismatches: Vec<String> = Vec::new();
        if got4 != sit.want {
            mismatches.push(format!("posture {:?}!={:?}", dec4.posture, sit.want));
        }
        if dec4.requires_user_interrupt != sit.requires_user_interrupt {
            mismatches.push(format!(
                "interrupt {}!={}",
                dec4.requires_user_interrupt, sit.requires_user_interrupt
            ));
        }
        if got4 == Posture::Monitor {
            let wake_ok = dec4
                .monitor
                .as_ref()
                .map(|m| !m.wake_when.is_empty())
                .unwrap_or(false);
            if !wake_ok {
                mismatches.push("monitor_without_wake".into());
            }
        }
        if sit.id == "user_sleeping_low_urgency" && got4 == Posture::Monitor {
            // overlap case: either deterministic primary blocker is acceptable
            let r = ["quiet_hours", "user_unavailable"].contains(&dec4.reason_code);
            // Relax ONLY the reason code here, never the rest. Clearing the accumulated list
            // instead would let an acceptable reason forgive a wrong posture, a MONITOR that
            // still demands an interrupt, or a MONITOR with no wake condition — i.e. the exact
            // defect this fixture was just fixed for would pass silently.
            if !r {
                mismatches.push(format!("reason {}", dec4.reason_code));
            }
        }
        if mismatches.is_empty() {
            ex4_correct += 1;
        } else {
            ex4_failures.push(format!("{} MISMATCH [{}]", sit.id, mismatches.join(", ")));
        }
    }
    println!(
        "EX4 SCOPE: {ex4_correct}/{ex4_total} GREEN {}",
        if ex4_failures.is_empty() {
            String::new()
        } else {
            format!("{ex4_failures:?}")
        }
    );
    println!("COVERAGE NOW: {}/37 decisions earned (EX1 10 + EX2 6 + EX3 6 + EX4 {}); EX0 baseline below is the FROZEN starting record", ex1_total + ex2_total + ex3_total + ex4_correct, ex4_correct);

    // Retires when the executive seam earns full coverage — same discipline as E.W0.
    assert_eq!(
        total - unrep, total,
        "PHASE 3B baseline still RED by design ({unrep}/{total} UNREPRESENTABLE) — builds the failure record that earns the smallest executive seam"
    );
    assert_eq!(
        ex1_correct, ex1_total,
        "EX1 scope regressed ({ex1_correct}/{ex1_total}) — posture semantics must hold while coverage expands"
    );
    assert_eq!(
        ex2_correct, ex2_total,
        "EX2 scope regressed ({ex2_correct}/{ex2_total}) — temporal escalation must hold while coverage expands"
    );
    assert_eq!(
        ex3_correct, ex3_total,
        "EX3 scope regressed ({ex3_correct}/{ex3_total}) — obligation/wait semantics must hold while coverage expands"
    );
}
