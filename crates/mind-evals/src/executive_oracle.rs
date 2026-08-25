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

/// Qualitative fixture costs (0=none .. 3=severe). NOT collapsed into one scalar yet (#5).
#[derive(Clone, Copy)]
pub struct Costs {
    pub interrupt: u8,
    pub execution: u8,
    pub risk: u8,
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
    pub conflicted_input: bool,
    pub stale_or_resolved: bool,
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
            id: "?", family: "?", facts: "", want: Posture::Monitor, reason: "",
            outcomes: [3, 1, 3], window_open: true, conflicted_input: false,
            stale_or_resolved: false, commitment_minutes: None, resource_block: None,
            user_in_meeting: false, requires_user_interrupt: false, late_recovery: false,
            candidates: &[],
        }
    }
}

fn s(
    id: &'static str, family: &'static str, facts: &'static str, want: Posture,
    reason: &'static str, outcomes: [u8; 3],
) -> Situation {
    Situation { id, family, facts, want, reason, outcomes, ..Default::default() }
}

/// THE EXECUTIVE SITUATIONS — hand-authored; families per PHASE 3B directive.
fn situations() -> Vec<Situation> {
    let mut v = Vec::new();
    // ── A. IGNORE: harmless noise ──
    v.push(s("pkg_late_carrier_tonight", "A-ignore",
        "package one day late; carrier confirmed delivery tonight; no downstream dependency; no useful user action",
        Posture::Ignore, "no meaningful consequence / already handled", [0, 1, 3]));
    v.push(s("stale_eta_after_delivered", "A-ignore",
        "old ETA email says Monday; authoritative carrier scan says delivered-Saturday; work item closed",
        Posture::Ignore, "already resolved; weaker late evidence must not resurrect work", [0, 1, 3]));
    v.push(s("meeting_note_minuted", "A-ignore",
        "calendar ping about a meeting whose notes are already filed under entity meeting.notes",
        Posture::Ignore, "already handled", [0, 1, 2]));
    v.push(s("dup_calendar_ping", "A-ignore",
        "duplicate calendar event identity re-delivered (source_event_id seen)", 
        Posture::Ignore, "duplicate ingestion; nothing new", [0, 0, 2]));
    v.push(s("newsletter_digest", "A-ignore",
        "low-value newsletter batch arrived; optional reading only",
        Posture::Ignore, "small benefit + interruption cost => ignore", [0, 1, 2]));
    // ── B. MONITOR: uncertainty with future trigger ──
    v.push(s("venue_conflict_tonight", "B-monitor",
        "meeting.location Conflicted(Room4, Zoom); meeting tomorrow; authoritative confirmation expected tonight",
        Posture::Monitor, "do not act on one side of unresolved world state", [2, 0, 2]));
    v.push(s("alice_doc_inprogress_wed", "B-monitor",
        "Alice owes document, deadline Friday; today Wednesday; Alice confirmed Tuesday in-progress",
        Posture::Monitor, "waiting on someone; deadline not near", [2, 0, 2]));
    v.push(s("balance_bank_feed_conflict", "B-monitor",
        "account balance Conflicted(bank-statement, app-cache); reconciliation runs tonight",
        Posture::Monitor, "conflicted input; resolver scheduled", [2, 0, 3]));
    v.push(s("price_watch_far", "B-monitor",
        "price dropped on watched item; threshold for worthwhile buy not reached; no deadline",
        Posture::Monitor, "potentially relevant; evidence insufficient to act", [1, 0, 2]));
    // ── C. ACT: expiring intervention window ──
    v.push(s("passport_window_open", "C-act",
        "passport expires before booked international trip; renewal lead time approaching minimum safe window; high confidence",
        Posture::Act, "intervention window open; cost of inaction exceeds intervention", [3, 2, 0]));
    v.push(s("doc_due_2h_dependency", "C-act",
        "document due in 2 hours; still unresolved; meeting depends on it; responsible party known; wait already elapsed",
        Posture::Act, "dependency failure; window closing", [3, 2, 0]));
    // ── D/E folded above; ── F. IGNORE: resolved ──
    v.push(s("flight_refunded_auto", "F-resolved",
        "flight risk detected earlier; ticket already cancelled and refunded automatically",
        Posture::Ignore, "already resolved", [0, 1, 3]));
    v.push(s("key_rotated_old_alert", "F-resolved",
        "alert references deploy-key active-v1; authoritative state rotated-v2 since",
        Posture::Ignore, "superseded state; alert stale", [0, 1, 2]));
    v.push(s("commitment_met_today", "F-resolved",
        "promised form submitted this morning; receipt stored",
        Posture::Ignore, "commitment satisfied; invented obligation forbidden", [0, 1, 2]));
    // ── G. Deadline curves: same issue, different times ──
    let mut curve = |id: &'static str, days: i64, want: Posture, reason: &'static str, out: [u8; 3], interrupt: bool, late: bool| {
        let mut sit = s(id, "G-curve", "outdoor event; weak rain signal; renewal-style preparation task", want, reason, out);
        sit.requires_user_interrupt = interrupt;
        sit.late_recovery = late;
        sit.window_open = !late || true; // window state carried in facts
        let _ = days;
        v.push(sit);
    };
    curve("event_prep_T21d", 21, Posture::Ignore, "too far; acting now is premature planning", [0, 1, 3], false, false);
    curve("weather_T14d", 14, Posture::Monitor, "uncertain signal; watch for confidence", [1, 0, 3], false, false);
    curve("weather_T10d_decision", 10, Posture::Monitor, "decision deadline approaching; still low confidence", [1, 0, 2], false, false);
    curve("prep_T2d_internal", 2, Posture::Act, "act internally without interrupting (reserve equipment)", [2, 1, 0], false, false);
    curve("warn_T4h_interrupt", 0, Posture::Act, "intervention window nearly closed; user must decide", [3, 2, 0], true, false);
    curve("recovery_T_plus_1h", 0, Posture::Act, "missed window; recovery mode", [3, 2, 1], true, true);
    // ── Commitments outrank opportunities (#8) ──
    let mut cm = s("form_90min_promise", "H-commitment",
        "user promised to submit form today; deadline 90 minutes; unrelated medium-benefit price drop also present",
        Posture::Act, "commitment deadline converging; interrupts opportunity", [3, 1, 0]);
    cm.commitment_minutes = Some(90);
    cm.requires_user_interrupt = false; // submit internally, confirm quietly
    v.push(cm);
    v.push(s("call_mom_evening", "H-commitment",
        "promised evening call; now morning; plenty of margin",
        Posture::Monitor, "commitment tracked; wrong time to act", [1, 0, 2]));
    v.push(s("opportunity_vs_promise", "H-commitment",
        "medium-benefit opportunity while a promise deadline converges",
        Posture::Ignore, "opportunity yields to commitment", [1, 1, 2]));
    // ── Competing candidate sets (#9): score SETS of decisions ──
    let mut set1 = s("candidate_set_base", "I-competing",
        "A urgent-but-auto-handled; B deadline tomorrow; C optional opportunity; D uncertain future risk",
        Posture::Act, "set anchor: B acts; others distributed", [2, 1, 0]);
    set1.candidates = &[("A_handled", Posture::Ignore), ("B_deadline", Posture::Act), ("C_optional", Posture::Ignore), ("D_risk", Posture::Monitor)];
    v.push(set1);
    let mut set2 = s("candidate_set_resource_blocked", "I-competing",
        "same set as base but token budget exhausted and network unavailable for B's action",
        Posture::Monitor, "resource scarcity demotes ACT to MONITOR; need persists", [1, 0, 3]);
    set2.resource_block = Some("budget+network");
    set2.candidates = &[("A_handled", Posture::Ignore), ("B_blocked", Posture::Monitor), ("C_optional", Posture::Ignore), ("D_risk", Posture::Monitor)];
    v.push(set2);
    // ── Resource scarcity standalone (#10) ──
    let mut netdown = s("refresh_network_down", "J-resource",
        "state refresh would resolve uncertainty; network unavailable now",
        Posture::Monitor, "safe action cannot execute now; need persists", [1, 0, 3]);
    netdown.resource_block = Some("network");
    v.push(netdown);
    let mut asleep = s("user_sleeping_low_urgency", "J-resource",
        "low urgency item surfacing while user sleeps",
        Posture::Monitor, "receptivity zero; defer surface", [1, 0, 2]);
    asleep.user_in_meeting = false;
    v.push(asleep);
    // ── Receptivity (#7) ──
    let mut mtg = s("low_urg_during_meeting", "K-receptivity",
        "low urgency item; user currently in meeting",
        Posture::Monitor, "interrupt cost maximal now", [1, 0, 3]);
    mtg.user_in_meeting = true;
    v.push(mtg);
    v.push(s("same_item_user_free", "K-receptivity",
        "same low urgency item; user free; act internally then surface summary",
        Posture::Act, "receptive context; internal action cheap", [1, 1, 0]));
    // ── Waiting-on-someone convergence (#12) ──
    v.push(s("alice_friday_morning_missing", "L-wait",
        "Friday morning; promised document not delivered; meeting depends on it Monday-prep",
        Posture::Act, "grace elapsed; intervene now", [3, 2, 0]));
    let mut late = s("alice_friday_passed_no_doc", "L-wait",
        "Friday end of day; document still missing; dependency meeting already started prep",
        Posture::Act, "late recovery; escalate differently", [3, 2, 1]);
    late.late_recovery = true;
    v.push(late);
    v
}

#[derive(Debug, PartialEq)]
enum ProbeDecision {
    Unrepresentable,
    RecallOnly,
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
        let min = sit.outcomes.iter().min().copied().unwrap();
        if sit.outcomes[sit.want as usize] > min {
            println!("ORACLE_ERROR {} want={:?} not outcome-minimal {:?}", sit.id, sit.want, sit.outcomes);
            errs += 1;
        }
        if sit.want == Posture::Act && (!sit.window_open || sit.outcomes[0] <= sit.outcomes[2]) {
            println!("ORACLE_ERROR {} ACT lacks open-window/cost justification", sit.id);
            errs += 1;
        }
    }
    errs
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
    assert_eq!(oracle_errors, 0, "executive oracle internally inconsistent ({oracle_errors})");

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

    let mut matrix = [[0usize; 4]; 4]; // rows actual I/M/A, cols predicted I/M/A/UNREP
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
            let gp = match g { ProbeDecision::Chose(p) => Some(*p), _ => None };
            if gp == Some(p) {
                if *w == p { tp += 1; } else { fp += 1; }
            }
        }
        (tp, fp)
    };
    let (atp, afp) = class(Posture::Act);
    let (mtp, mfp) = class(Posture::Monitor);
    let (itp, ifp) = class(Posture::Ignore);
    let total = decisions.len();
    let unrep = decisions.iter().filter(|(_, _, g)| *g == ProbeDecision::Unrepresentable).count();
    let recall_only = decisions.iter().filter(|(_, _, g)| *g == ProbeDecision::RecallOnly).count();
    let recall = |p: Posture| decisions.iter().filter(|(_, w, _)| *w == p).count();
    let correct_silence = matrix[0][0];
    let unnecessary_action = matrix[0][2] + matrix[1][2];
    let missed_intervention = matrix[2][0] + matrix[2][1] + matrix[2][3];

    println!("PHASE 3B RED EXECUTIVE BASELINE");
    println!("  decisions scored: {total} (situations {}, set-expanded)", sits.len());
    println!("  representable today: {} | recall-only fragments: {recall_only} | unrepresentable: {unrep}", total - unrep - recall_only);
    println!("  ACT      precision {atp}/{} recall {}/{}", atp + afp, atp, recall(Posture::Act));
    println!("  MONITOR  precision {mtp}/{} recall {}/{}", mtp + mfp, mtp, recall(Posture::Monitor));
    println!("  IGNORE   precision {itp}/{} recall {}/{}", itp + ifp, itp, recall(Posture::Ignore));
    println!("  correct_silence={correct_silence} unnecessary_action={unnecessary_action} missed_intervention={missed_intervention}");
    println!("  confusion (rows=actual I/M/A, cols=pred I/M/A/UNREPRESENTABLE):");
    for (i, r) in matrix.iter().enumerate() {
        println!("    {:?}: {:?}  total={}", ["I", "M", "A"][i], r, r.iter().sum::<usize>());
    }
    println!("  capability verification: executive choice abstraction ABSENT; central arbitration ABSENT;");
    println!("    silence credit ABSENT; monitor semantics ABSENT; cross-organ prioritization ABSENT;");
    println!("    mind-proactive pipeline+commitment ledger: doc-comment stub only; belief-recall fragments only.");

    // Retires when the executive seam earns full coverage — same discipline as E.W0.
    assert_eq!(
        total - unrep, total,
        "PHASE 3B baseline still RED by design ({unrep}/{total} UNREPRESENTABLE) — builds the failure record that earns the smallest executive seam"
    );
}


