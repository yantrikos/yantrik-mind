//! EX4-LIVE-A — the executive runs on the live path, and decides nothing.
//!
//! Phase 3B built a posture arbiter (`mind_proactive::arbitrate`) that reached 26/37 on the
//! executive oracle while having NO caller outside its own crate and the eval harness. The live
//! decision about whether to say something unprompted was still `proactive_receptivity_ok()`.
//! Two proactive systems existed and the principled one did not run, which meant the benchmark
//! could have reached 37/37 without changing the mind's behaviour by one message.
//!
//! This module moves the arbiter from BENCHMARKED to REACHABLE + SHADOWED, and no further. It runs
//! at ONE named call site on real inputs, records what it would have done, and never decides
//! anything. The legacy gate stays authoritative. See ledger E.D1-E.D4 for the contract.
//!
//! Three things the contract insists on, each paid for by a real defect:
//!
//! 1. ONE RECORD PER OPPORTUNITY, not per tick. `last_digest` only resets when receptivity PASSES,
//!    so a suppressed opportunity is re-evaluated every ~25s by the poll loop — ~144 times an hour
//!    against a single record for a send. Keyed per tick, suppressed cases would outnumber sent
//!    ones by a thousand to one and every disagreement count would be meaningless while looking
//!    substantial. That is E.P2's bias inverted: multiplied failures rather than deleted ones.
//!
//! 2. CENSORING IS PRESERVED, NEVER IMPUTED. When the legacy gate declines, the short-circuit means
//!    `proactive_digest()` never runs, so it is unknown whether there was anything to say at all —
//!    and it CANNOT be cheaply discovered, because that function calls `discharge_tension()` on
//!    what it renders ("surfaced once; don't repeat"). Evaluating it speculatively would consume
//!    the tensions and silently discard things the mind meant to raise. So the outcome is recorded
//!    as censored. It is never turned into "ignored", "failure", or 0.
//!
//! 3. PENDING RETIRES OR ALARMS. An engagement claim that nobody returns to is how 650 claims
//!    accumulated for 46 days. A record pending well past its window is an INSTRUMENTATION DEFECT
//!    and says so, rather than quietly scoring as a failure.

use mind_proactive::{ExecutiveCandidate, Posture, ResourceContextView};
use serde::{Deserialize, Serialize};

/// What the LEGACY gate did at this opportunity. Deliberately not a boolean: at this site a drop
/// can mean "we decided not to speak" or "there was nothing to say", and those answer different
/// questions. Collapsing them would put a different question's data in the disagreement column.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LegacyOutcome {
    /// The gate passed, content existed, the message went out.
    Sent,
    /// The gate passed and `proactive_digest()` returned None — a real third case here.
    NothingToSay,
    /// The receptivity gate declined. Content existence is unknown by construction.
    DeclinedByReceptivity,
    /// Recorded but not yet resolved this opportunity.
    Undetermined,
}

/// Whether the message content was ever established. Only knowable when the gate let evaluation
/// proceed — which is exactly why a receptivity decline is not a proactive-send disagreement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ContentStatus {
    KnownMessage,
    KnownNothingToSay,
    UnknownDueToLegacyShortCircuit,
}

/// Whether the engagement outcome can ever be observed for this record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OutcomeStatus {
    /// Sent, and the engagement window has resolved in the judgment ledger.
    Observable,
    /// Sent, window still open.
    Pending,
    /// The legacy gate declined, so no send happened and no outcome exists. NOT a failure.
    CensoredByLegacyDrop,
    /// Nothing was sent because there was nothing to say. No policy question arises.
    NotApplicable,
}

/// One opportunity. Not one tick.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShadowRecord {
    /// The cadence window this belongs to (`last_digest` at the eligible cut). The dedupe key.
    pub window_id: i64,
    pub decided_at_ms: i64,
    /// How many times the poll loop re-reached the eligible cut inside this one window. Kept as a
    /// count so the re-evaluation volume is visible WITHOUT it inflating the sample.
    pub evaluations: u32,

    pub executive_posture: String,
    pub executive_reason: String,
    pub executive_wake: Vec<String>,
    pub executive_requires_interrupt: bool,

    pub legacy: LegacyOutcome,
    pub content: ContentStatus,
    pub outcome: OutcomeStatus,
    /// For a send: the judgment-ledger `ref` of the engagement claim. The join key to an
    /// INDEPENDENT witness — the ledger is written by code that knows nothing about this module.
    /// Captured from `note_proactive_sent` directly rather than reconstructed from a timestamp,
    /// because `judgment_log` stamps its own `t` after an awaited read and the two differ.
    pub claim_ref: Option<String>,

    // Provenance. Candidate derivation WILL change; an ACT-rate without these is uninterpretable.
    pub experiment_id: String,
    pub executive_policy: String,
    pub build_commit: String,
    pub call_site: String,
}

pub const EXPERIMENT_ID: &str = "ex4-live-a-v1";
pub const EXECUTIVE_POLICY: &str = "exec-v1";
pub const CALL_SITE: &str = "telegram.periodic_proactive_digest";
const STORE: &str = "ex4_shadow";
const MAX_RECORDS: usize = 500;
/// The engagement window the legacy resolver uses.
const WINDOW_MS: i64 = 90 * 60_000;
/// Past window + this, a Pending record is an instrumentation defect, not an ignored message.
const PENDING_ALARM_MARGIN_MS: i64 = 6 * 3_600_000;

/// Re-evaluations of the CURRENT window that have not been flushed to the store yet.
///
/// Without this the shadow would rewrite the whole record list on every poll tick. A suppressed
/// opportunity re-reaches the eligible cut roughly every 25s, which is ~205KB rewritten 144 times
/// an hour — around 0.7 GB/day of write amplification on the box, caused entirely by an experiment
/// that is supposed to observe the system without disturbing it. The decision itself is made ONCE
/// per opportunity (that is the semantics anyway: the record keeps the FIRST decision), and only
/// the counter moves afterwards, so the counter is what gets batched.
static EVAL_CACHE: std::sync::Mutex<(i64, u32)> = std::sync::Mutex::new((i64::MIN, 0));
/// Flush the counter at most this often — ~27 minutes at a 25s poll. A restart loses at most this
/// many counted evaluations, which costs nothing: the count is context, not evidence.
const EVAL_FLUSH_EVERY: u32 = 64;

/// Build the candidate from what THIS site genuinely knows, and nothing else.
///
/// Deliberately degraded. Fifteen-odd fields stay at conservative defaults because the periodic
/// digest has no authoritative notion of them — `already_resolved` most of all: there is no
/// particular issue here whose resolution could be established, so it stays false-as-unestablished
/// rather than being manufactured because the field exists. Whether `arbitrate` behaves sensibly on
/// a partial candidate is itself the evidence: if it only works when an evaluator fills every
/// field, `ExecutiveCandidate` has quietly stopped being a narrow waist.
pub(crate) fn candidate_for_digest(
    now_ms: i64,
    quiet_hours: bool,
    quiet_hours_end_ms: Option<i64>,
    user_receptive: Option<bool>,
) -> ExecutiveCandidate {
    ExecutiveCandidate {
        candidate_id: "periodic_digest".into(),
        source_ref: CALL_SITE.into(),
        now_ms,
        // A digest of open tensions is the lowest urgency thing the mind says.
        urgency: 1,
        deadline_at_ms: None,
        already_resolved: false,
        useful_action_available: true,
        // A digest exists to be READ. It cannot be discharged internally.
        internal_capability: false,
        blocked: false,
        waiting_on_someone: false,
        intervention_window_open: true,
        execution_cost: 1,
        interruption_cost: 2,
        risk: 1,
        confidence: 0.9,
        intervention_not_before_ms: None,
        intervention_by_ms: None,
        interrupt_lead_ms: None,
        commitment: None,
        converging_obligation_due_ms: None,
        wait_grace_until_ms: None,
        resources: Some(ResourceContextView {
            network_available: true,
            capability_available: true,
            budget_available: true,
            user_receptive,
            quiet_hours,
            quiet_hours_end_ms,
        }),
    }
}

pub(crate) fn posture_name(p: Posture) -> &'static str {
    match p {
        Posture::Ignore => "IGNORE",
        Posture::Monitor => "MONITOR",
        Posture::Act => "ACT",
    }
}

/// Fold a fresh decision into the list, keyed on the OPPORTUNITY.
///
/// A repeat inside the same window bumps `evaluations` and leaves the decision alone. This is the
/// whole defence against the ~1000:1 over-counting the poll loop would otherwise produce.
pub(crate) fn upsert(list: &mut Vec<ShadowRecord>, rec: ShadowRecord, extra_evals: u32) -> bool {
    if let Some(existing) = list.iter_mut().find(|r| r.window_id == rec.window_id) {
        existing.evaluations = existing.evaluations.saturating_add(1 + extra_evals);
        return false;
    }
    list.push(rec);
    if list.len() > MAX_RECORDS {
        let cut = list.len() - MAX_RECORDS;
        list.drain(..cut);
    }
    true
}

/// Attach what the legacy gate ended up doing, and the outcome status that follows from it.
pub(crate) fn note_legacy(
    list: &mut Vec<ShadowRecord>,
    window_id: i64,
    legacy: LegacyOutcome,
    claim_ref: Option<String>,
) {
    let Some(r) = list.iter_mut().find(|r| r.window_id == window_id) else {
        return;
    };
    r.content = match legacy {
        LegacyOutcome::Sent => ContentStatus::KnownMessage,
        LegacyOutcome::NothingToSay => ContentStatus::KnownNothingToSay,
        // The gate stopped evaluation before content existed. Unknown, and not discoverable
        // without consuming the tensions — see the module header.
        LegacyOutcome::DeclinedByReceptivity | LegacyOutcome::Undetermined => {
            ContentStatus::UnknownDueToLegacyShortCircuit
        }
    };
    r.outcome = match legacy {
        LegacyOutcome::Sent => OutcomeStatus::Pending,
        LegacyOutcome::NothingToSay => OutcomeStatus::NotApplicable,
        LegacyOutcome::DeclinedByReceptivity => OutcomeStatus::CensoredByLegacyDrop,
        LegacyOutcome::Undetermined => OutcomeStatus::Pending,
    };
    r.legacy = legacy;
    if claim_ref.is_some() {
        r.claim_ref = claim_ref;
    }
}

/// Is this record's engagement claim overdue past any honest explanation?
///
/// Not "ignored". A window that closed hours ago with no graded outcome means the resolver never
/// came back, which is the 650-orphaned-claims failure recurring. It must alarm, not score.
pub(crate) fn is_instrumentation_defect(r: &ShadowRecord, now_ms: i64) -> bool {
    r.outcome == OutcomeStatus::Pending
        && now_ms.saturating_sub(r.decided_at_ms) > WINDOW_MS + PENDING_ALARM_MARGIN_MS
}

impl crate::ConversationEngine {
    async fn ex4_load(&self) -> Vec<ShadowRecord> {
        self.memory.profile_get(STORE).await.ok().flatten()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    async fn ex4_save(&self, list: &[ShadowRecord]) {
        let _ = self.memory
            .profile_set(STORE, &serde_json::to_string(list).unwrap_or_default())
            .await;
    }

    /// THE ELIGIBLE CUT. Called after the legacy preconditions pass and BEFORE the receptivity
    /// gate branches, so both sides of the decision are recorded even though only one side's
    /// outcome can ever be observed.
    ///
    /// Runs the production arbiter on real organ inputs and persists what it would have done.
    /// Decides nothing — the caller must not use the return value for control flow.
    pub async fn ex4_shadow_decide(
        &self,
        window_id: i64,
        quiet_hours: bool,
        quiet_hours_end_ms: Option<i64>,
    ) -> String {
        let now = chrono::Utc::now().timestamp_millis();
        // Receptivity from the live organ, read exactly as the legacy gate reads it — the executive
        // is being compared against that policy, not handed a different measurement.
        let receptive = match self.memory.proactive_receptivity().await {
            Ok(Some(r)) => {
                let base = self.memory.proactive_baseline_rate().await.ok().flatten();
                Some(r >= crate::proactive::dead_zone_floor(base))
            }
            // No data yet is NOT "unreceptive". Unknown stays unknown.
            _ => None,
        };
        // Same opportunity as last tick? Then the decision is already recorded — by design, the
        // record keeps the FIRST decision — and only the re-evaluation counter needs to move.
        // Batch it rather than rewriting the store every 25 seconds.
        let mut carry = 0u32;
        {
            let mut c = EVAL_CACHE.lock().unwrap_or_else(|e| e.into_inner());
            if c.0 == window_id {
                c.1 = c.1.saturating_add(1);
                if c.1 < EVAL_FLUSH_EVERY {
                    return String::new(); // nothing persisted, nothing decided
                }
                carry = std::mem::take(&mut c.1);
            } else {
                *c = (window_id, 0);
            }
        }
        let cand = candidate_for_digest(now, quiet_hours, quiet_hours_end_ms, receptive);
        let d = mind_proactive::arbitrate(&cand);
        let mut list = self.ex4_load().await;
        upsert(&mut list, ShadowRecord {
            window_id,
            decided_at_ms: now,
            evaluations: 1,
            executive_posture: posture_name(d.posture).to_string(),
            executive_reason: d.reason_code.to_string(),
            executive_wake: d.monitor.as_ref()
                .map(|m| m.wake_when.iter().map(|w| format!("{w:?}")).collect())
                .unwrap_or_default(),
            executive_requires_interrupt: d.requires_user_interrupt,
            legacy: LegacyOutcome::Undetermined,
            content: ContentStatus::UnknownDueToLegacyShortCircuit,
            outcome: OutcomeStatus::Pending,
            claim_ref: None,
            experiment_id: EXPERIMENT_ID.into(),
            executive_policy: EXECUTIVE_POLICY.into(),
            build_commit: option_env!("YM_BUILD_COMMIT").unwrap_or("unknown").to_string(),
            call_site: CALL_SITE.into(),
        }, carry);
        self.ex4_save(&list).await;
        posture_name(d.posture).to_string()
    }

    /// Record what the LEGACY gate actually did at this opportunity. Legacy stays authoritative;
    /// this only observes it.
    pub async fn ex4_shadow_note_legacy(
        &self,
        window_id: i64,
        legacy: LegacyOutcome,
        claim_ref: Option<String>,
    ) {
        let mut list = self.ex4_load().await;
        note_legacy(&mut list, window_id, legacy, claim_ref);
        self.ex4_save(&list).await;
    }

    /// EX4-LIVE-A report. States reachability, the quadrants and the censoring rate — and makes NO
    /// claim that either policy is better. A send the person ignored does not establish that
    /// deferring would have produced a better total outcome; interruption cost and delayed
    /// usefulness are unmeasured. Hence "shadow-consistent", never "correct".
    pub async fn ex4_report(&self) -> String {
        let list = self.ex4_load().await;
        if list.is_empty() {
            return "EX4-LIVE-A: no opportunities recorded yet - the executive has not been reached on the live path".to_string();
        }
        // INDEPENDENT WITNESS (Doctrine 3): outcomes come from the judgment ledger, written by the
        // legacy send path, which knows nothing about this module. Joined by the ref captured at
        // send time - never reconstructed from a timestamp, because judgment_log stamps its own `t`
        // after an awaited read and the two differ.
        let led: Vec<serde_json::Value> = self.memory.profile_get("judgment_ledger").await.ok().flatten()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let outcome_of = |rf: &str| -> Option<bool> {
            led.iter()
                .find(|r| r.get("ref").and_then(|x| x.as_str()) == Some(rf))
                .and_then(|r| r.get("outcome"))
                .and_then(|x| x.as_bool().or_else(|| x.as_i64().map(|n| n != 0)))
        };
        let now = chrono::Utc::now().timestamp_millis();
        let (mut sent, mut nothing, mut declined, mut undetermined) = (0u32, 0u32, 0u32, 0u32);
        let (mut agree, mut disagree) = (0u32, 0u32);
        let (mut observed, mut censored, mut pending, mut defects) = (0u32, 0u32, 0u32, 0u32);
        let (mut engaged, mut ignored) = (0u32, 0u32);
        let mut ticks = 0u64;
        for r in &list {
            ticks += r.evaluations as u64;
            match r.legacy {
                LegacyOutcome::Sent => sent += 1,
                LegacyOutcome::NothingToSay => nothing += 1,
                LegacyOutcome::DeclinedByReceptivity => declined += 1,
                LegacyOutcome::Undetermined => undetermined += 1,
            }
            // Only opportunities where the legacy gate actually reached a SPEAK/DECLINE verdict are
            // a policy comparison. "Nothing to say" answers a different question entirely.
            let exec_would_speak = r.executive_posture == "ACT";
            let legacy_spoke = r.legacy == LegacyOutcome::Sent;
            if matches!(r.legacy, LegacyOutcome::Sent | LegacyOutcome::DeclinedByReceptivity) {
                if exec_would_speak == legacy_spoke { agree += 1 } else { disagree += 1 }
            }
            let resolved = r.claim_ref.as_deref().and_then(outcome_of);
            match resolved {
                Some(true) => { engaged += 1; observed += 1; }
                Some(false) => { ignored += 1; observed += 1; }
                None => match r.outcome {
                    OutcomeStatus::CensoredByLegacyDrop => censored += 1,
                    OutcomeStatus::NotApplicable => {}
                    OutcomeStatus::Observable => observed += 1,
                    OutcomeStatus::Pending => {
                        if is_instrumentation_defect(r, now) { defects += 1 } else { pending += 1 }
                    }
                },
            }
        }
        let mut out = String::new();
        out.push_str(&format!("EX4-LIVE-A - executive SHADOWED on the live path ({EXPERIMENT_ID}, {EXECUTIVE_POLICY})\n"));
        out.push_str(&format!("  call site: {CALL_SITE}\n"));
        out.push_str(&format!("  opportunities {} (from {ticks} eligible-cut evaluations - re-evaluation does NOT inflate the sample)\n", list.len()));
        out.push_str(&format!("  legacy: sent {sent} - nothing-to-say {nothing} - declined-by-receptivity {declined} - undetermined {undetermined}\n"));
        out.push_str(&format!("  policy agreement {agree} - disagreement {disagree}\n"));
        out.push_str(&format!("  outcomes: observable {observed} (engaged {engaged}, ignored {ignored}) - pending {pending} - CENSORED {censored}\n"));
        if defects > 0 {
            out.push_str(&format!("  !! {defects} record(s) pending far past their window - INSTRUMENTATION DEFECT, not ignored messages\n"));
        }
        out.push_str("  censored = the legacy gate declined, so nothing was sent and no outcome exists.\n");
        out.push_str("  Not a failure and not a zero. The disagreement that would justify switching is\n");
        out.push_str("  exactly the one this design cannot observe (ledger E.D2 / E.D4).\n");
        out.push_str("  Agreement above is SHADOW-CONSISTENT EVIDENCE, not proof that either policy is better.\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(window_id: i64, at: i64) -> ShadowRecord {
        ShadowRecord {
            window_id,
            decided_at_ms: at,
            evaluations: 1,
            executive_posture: "MONITOR".into(),
            executive_reason: "quiet_hours".into(),
            executive_wake: vec!["receptivity".into()],
            executive_requires_interrupt: false,
            legacy: LegacyOutcome::Undetermined,
            content: ContentStatus::UnknownDueToLegacyShortCircuit,
            outcome: OutcomeStatus::Pending,
            claim_ref: None,
            experiment_id: EXPERIMENT_ID.into(),
            executive_policy: EXECUTIVE_POLICY.into(),
            build_commit: "test".into(),
            call_site: CALL_SITE.into(),
        }
    }

    /// The defence against the poll loop. `last_digest` only resets when receptivity PASSES, so a
    /// suppressed opportunity re-reaches the eligible cut every ~25s — ~144 times an hour. Keyed
    /// per tick that would outnumber a send by ~1000:1 and every disagreement count would be
    /// meaningless while looking substantial.
    #[test]
    fn a_re_evaluated_opportunity_is_one_record_not_one_per_tick() {
        let mut list = Vec::new();
        for i in 0..144 {
            upsert(&mut list, rec(1_000, 1_000 + i * 25_000), 0);
        }
        assert_eq!(list.len(), 1, "one opportunity is one record");
        assert_eq!(list[0].evaluations, 144, "but the re-evaluation volume stays visible");
        assert_eq!(list[0].decided_at_ms, 1_000, "the decision is the FIRST one, not the latest");

        // A genuinely new opportunity (the cadence reset) is a new record.
        upsert(&mut list, rec(9_999, 9_999), 0);
        assert_eq!(list.len(), 2);
    }

    /// The counter batches, and a batched flush must not lose the evaluations it carried.
    ///
    /// Without batching the shadow rewrites the whole record list on every poll tick: ~205KB, 144
    /// times an hour, ~0.7 GB/day of writes caused purely by an experiment meant to observe the
    /// system without disturbing it. A shadow that degrades production is not a shadow.
    #[test]
    fn batched_evaluations_are_carried_not_dropped() {
        let mut list = Vec::new();
        assert!(upsert(&mut list, rec(1, 1), 0), "first sighting creates the opportunity");
        assert_eq!(list[0].evaluations, 1);

        // A flush arriving after 63 unpersisted ticks must add all of them, plus itself.
        assert!(!upsert(&mut list, rec(1, 999), 63), "same window is never a new opportunity");
        assert_eq!(list[0].evaluations, 65, "the carried ticks must survive the batching");
        assert_eq!(list[0].decided_at_ms, 1, "and the decision still belongs to the first tick");
        assert_eq!(list.len(), 1);
    }

    /// A censored outcome must never become a failure. That conversion is precisely the defect
    /// that made the engagement rate read 43pct when it was 31pct.
    #[test]
    fn a_legacy_decline_is_censored_never_ignored() {
        let mut list = vec![rec(1, 1)];
        note_legacy(&mut list, 1, LegacyOutcome::DeclinedByReceptivity, None);
        assert_eq!(list[0].outcome, OutcomeStatus::CensoredByLegacyDrop);
        assert_eq!(
            list[0].content,
            ContentStatus::UnknownDueToLegacyShortCircuit,
            "content existence is unknown when the gate short-circuits before the digest"
        );
        assert!(list[0].claim_ref.is_none(), "nothing was sent, so there is no claim to join to");
    }

    /// Gate passed, nothing to say. Not a policy disagreement in either direction.
    #[test]
    fn nothing_to_say_is_not_applicable_not_a_drop() {
        let mut list = vec![rec(2, 2)];
        note_legacy(&mut list, 2, LegacyOutcome::NothingToSay, None);
        assert_eq!(list[0].outcome, OutcomeStatus::NotApplicable);
        assert_eq!(list[0].content, ContentStatus::KnownNothingToSay);
    }

    /// A send carries the ledger ref it was actually logged under — captured, not reconstructed
    /// from a timestamp. `judgment_log` stamps its own `t` after an awaited read, so a
    /// timestamp-derived join matches only the rows where the millisecond did not tick over.
    #[test]
    fn a_send_joins_by_the_ref_it_was_logged_under() {
        let mut list = vec![rec(3, 3)];
        note_legacy(&mut list, 3, LegacyOutcome::Sent, Some("1756142400123".into()));
        assert_eq!(list[0].outcome, OutcomeStatus::Pending);
        assert_eq!(list[0].claim_ref.as_deref(), Some("1756142400123"));
    }

    /// Pending past any honest explanation is an instrumentation defect, not an ignored message.
    #[test]
    fn a_stuck_pending_record_alarms_rather_than_scoring_as_ignored() {
        let mut list = vec![rec(4, 0)];
        note_legacy(&mut list, 4, LegacyOutcome::Sent, Some("x".into()));
        let inside = WINDOW_MS + PENDING_ALARM_MARGIN_MS - 1;
        assert!(!is_instrumentation_defect(&list[0], inside), "still legitimately pending");
        assert!(
            is_instrumentation_defect(&list[0], inside + 2),
            "past the window plus margin the resolver never came back — alarm, do not score"
        );
    }

    /// The candidate is deliberately degraded: `already_resolved` must not be manufactured just
    /// because the field exists, and a digest cannot be discharged internally.
    #[test]
    fn the_digest_candidate_claims_only_what_the_site_knows() {
        let c = candidate_for_digest(0, true, Some(8 * 3_600_000), Some(false));
        assert!(!c.already_resolved, "this site cannot establish resolution of anything");
        assert!(!c.internal_capability, "a digest exists to be read; it cannot be handled internally");
        let r = c.resources.as_ref().expect("resource view is the point of EX4");
        assert!(r.quiet_hours);
        assert_eq!(r.user_receptive, Some(false));
    }

    /// The arbiter must produce a usable decision from a deliberately partial candidate. If it
    /// cannot, `ExecutiveCandidate` has stopped being a narrow waist and that is the finding.
    #[test]
    fn arbitrate_survives_a_degraded_real_candidate() {
        let c = candidate_for_digest(0, true, Some(8 * 3_600_000), Some(false));
        let d = mind_proactive::arbitrate(&c);
        assert_eq!(d.posture, Posture::Monitor, "quiet hours defers a low-urgency digest");
        assert!(!d.requires_user_interrupt, "a deferral must not also demand an interrupt");
        let m = d.monitor.as_ref().expect("a MONITOR must say what would make it reconsider");
        assert!(!m.wake_when.is_empty());
    }

    /// The disagreement this site actually exposes: the legacy chain at the digest has NO
    /// quiet-hours check (unlike the anticipate path), so it can speak at 3am. The executive
    /// defers. That is a real policy difference, not a re-derivation of the legacy boolean.
    #[test]
    fn the_executive_defers_in_quiet_hours_where_this_site_legacy_does_not_look() {
        let quiet = candidate_for_digest(0, true, Some(8 * 3_600_000), Some(true));
        assert_eq!(
            mind_proactive::arbitrate(&quiet).posture,
            Posture::Monitor,
            "receptive but asleep: the executive still defers"
        );
        let awake = candidate_for_digest(0, false, None, Some(true));
        assert_eq!(
            mind_proactive::arbitrate(&awake).posture,
            Posture::Act,
            "awake and receptive: the executive would speak, agreeing with legacy"
        );
    }
}
