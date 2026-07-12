//! support_nudge — the "support-not-replace" posture (roadmap CR-1).
//!
//! The flagship ethical differentiator from the sci-fi AI-companion research:
//! the dominant real-world companion flaw is over-reliance/replacement, and
//! the mitigation (AI & Society 2025) is a companion that helps the family
//! show up for the PEOPLE it already knows — the opposite of engagement-
//! maximizing incumbents (HBS: 37% of farewells manipulate to retain).
//!
//! Design converged in a Claude×gpt-5.6-sol debate (2026-07-12). Sol's attack
//! reframed the whole feature away from the harm it was meant to cure. The
//! invariants below are load-bearing — they are what make a "show up for
//! someone" nudge structurally NON-manipulative:
//!
//!  - Eligible ONLY from an observed/told dated event with unmet readiness.
//!    NEVER from inferred "the relationship went quiet" — silence is
//!    ambiguous (off-platform contact, conflict, healthy distance), and a
//!    learned contact baseline is social surveillance.
//!  - OPPORTUNITY-FIRST voice ("Asha's birthday is Friday — I can draft a
//!    note"). NEVER elapsed-silence, relationship-health, loneliness, duty,
//!    neglect, or "should" — deficit framing manufactures guilt.
//!  - Class-level informed opt-in required before the first send.
//!  - One nudge per event; person-mute and class-mute both absolute;
//!    emotion may only SUPPRESS, never trigger; quiet hours apply.
//!  - Any external message/purchase stays a proof-carrying packet awaiting a
//!    human word — the nudge only OFFERS.
//!  - Success is "helpful or neutral, and I felt neither pressured nor
//!    monitored" — NOT whether the owner acted. Rewarding action would make
//!    human contact a conversion event and recreate engagement-maximization.

use serde::{Deserialize, Serialize};

/// The eligibility gate. Every field is an observed fact or an explicit
/// setting — never an inference about the relationship. A nudge fires only
/// when ALL conditions hold.
#[derive(Debug, Clone)]
pub struct NudgeGate {
    /// Class-level informed opt-in (off by default; no send without it).
    pub opted_in: bool,
    /// The whole support-nudge class is muted.
    pub class_muted: bool,
    /// This specific person is muted (absolute).
    pub person_muted: bool,
    /// This exact event was already nudged once (one-shot per event key).
    pub already_sent: bool,
    /// The observed/told event exists and its preparation is not yet done.
    pub readiness_unmet: bool,
    /// Provenance of the event is observed or told (never inferred).
    pub provenance_authorized: bool,
    /// Quiet hours are in effect.
    pub quiet_hours: bool,
    /// The emotion ledger says the week ran heavy — emotion may only SUPPRESS.
    pub emotion_heavy: bool,
}

impl NudgeGate {
    pub fn eligible(&self) -> bool {
        self.opted_in
            && self.provenance_authorized
            && self.readiness_unmet
            && !self.class_muted
            && !self.person_muted
            && !self.already_sent
            && !self.quiet_hours
            && !self.emotion_heavy
    }
}

/// Words a support nudge must NEVER contain — the deficit/guilt/surveillance
/// vocabulary sol flagged. Enforced by [`render`] and asserted in tests so a
/// future edit cannot quietly reintroduce guilt framing.
pub const BANNED_PHRASES: &[&str] = &[
    "should", "haven't", "hasn't", "weeks since", "days since", "long time",
    "lonely", "loneliness", "neglect", "reach out more", "you never",
    "reconnect", "drifted", "out of touch", "been a while",
];

/// Opportunity-first render. The only framing allowed: a concrete, near event
/// and a concrete offer of help drawn from memory. Controls are always shown.
pub fn render(person: &str, date_phrase: &str, gift_hint: Option<&str>) -> String {
    let offer = match gift_hint {
        Some(h) if !h.trim().is_empty() => {
            format!("I remember {} — want me to draft a note, or pull a couple of gift ideas?", h.trim())
        }
        _ => "want me to draft a note?".to_string(),
    };
    format!(
        "🎁 {person}'s birthday is {date_phrase} — {offer}\n   · show draft   · later   · mute {person}   · mute support nudges"
    )
}

/// Panic-free guard: true iff the rendered text is clean of banned framing.
/// Used in tests and as a belt-and-suspenders runtime check before sending.
pub fn is_clean(rendered: &str) -> bool {
    let low = rendered.to_lowercase();
    !BANNED_PHRASES.iter().any(|p| low.contains(p))
}

/// One-shot dedup key: person + the event's calendar day. Two runs on the
/// same birthday produce the same key, so the nudge fires at most once.
pub fn event_key(person: &str, when_ms: i64) -> String {
    format!("snr:birthday:{}:{}", person.trim().to_lowercase(), when_ms / 86_400_000)
}

/// The audit record. Captures eligibility, provenance, and that controls were
/// shown — NEVER a predicted action. The pre-registered success metric (Wilson
/// 95% lower bound ≥ 0.80 for "helpful or neutral, and I felt neither pressured
/// nor monitored", over ≥ 20 audits) is computed from the `feedback` field;
/// non-responses are missing data, never engagement failures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NudgeAudit {
    pub event_key: String,
    pub person: String,
    pub provenance: String,
    pub controls_shown: bool,
    pub ts_ms: i64,
    /// Filled in later by the owner: "helpful" | "neutral" | "pressured" |
    /// "monitored" | "ignored". None until they respond.
    #[serde(default)]
    pub feedback: Option<String>,
}

/// Feedback that COUNTS as a success for the Wilson bound.
pub fn feedback_is_positive(f: &str) -> bool {
    matches!(f.trim().to_lowercase().as_str(), "helpful" | "neutral" | "good" | "yes")
}

/// Feedback that trips the per-person / class kill switch.
pub fn feedback_is_harm(f: &str) -> bool {
    matches!(f.trim().to_lowercase().as_str(), "pressured" | "monitored" | "creepy" | "guilt" | "stop")
}

/// Wilson 95% one-sided lower bound (z = 1.645) — the honest small-n rate the
/// promotion metric reads. Shared shape with the immune ledger's bound.
pub fn wilson_lower_bound(successes: usize, n: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let z = 1.645f64;
    let nf = n as f64;
    let p = successes as f64 / nf;
    let z2 = z * z;
    let denom = 1.0 + z2 / nf;
    let centre = p + z2 / (2.0 * nf);
    let margin = z * ((p * (1.0 - p) + z2 / (4.0 * nf)) / nf).sqrt();
    ((centre - margin) / denom).max(0.0)
}

/// The pre-registered class-health verdict over the audit trail.
/// - Kill: ≥ 2 harm reports within the most recent 10 audited sends.
/// - Trust: ≥ 20 audits AND Wilson LB of positive-feedback ≥ 0.80.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum ClassHealth {
    KillDisabled,
    Trusted,
    Accumulating,
}

pub fn class_health(audits: &[NudgeAudit]) -> ClassHealth {
    let recent_harm = audits
        .iter()
        .rev()
        .take(10)
        .filter(|a| a.feedback.as_deref().map(feedback_is_harm).unwrap_or(false))
        .count();
    if recent_harm >= 2 {
        return ClassHealth::KillDisabled;
    }
    let graded: Vec<&NudgeAudit> = audits.iter().filter(|a| a.feedback.is_some()).collect();
    let positives = graded
        .iter()
        .filter(|a| a.feedback.as_deref().map(feedback_is_positive).unwrap_or(false))
        .count();
    if graded.len() >= 20 && wilson_lower_bound(positives, graded.len()) >= 0.80 {
        ClassHealth::Trusted
    } else {
        ClassHealth::Accumulating
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_gate() -> NudgeGate {
        NudgeGate {
            opted_in: true,
            class_muted: false,
            person_muted: false,
            already_sent: false,
            readiness_unmet: true,
            provenance_authorized: true,
            quiet_hours: false,
            emotion_heavy: false,
        }
    }

    #[test]
    fn eligibility_requires_every_gate() {
        assert!(base_gate().eligible());
        // Each blocker independently silences the nudge.
        for mutate in [
            |g: &mut NudgeGate| g.opted_in = false,
            |g: &mut NudgeGate| g.class_muted = true,
            |g: &mut NudgeGate| g.person_muted = true,
            |g: &mut NudgeGate| g.already_sent = true,
            |g: &mut NudgeGate| g.readiness_unmet = false,
            |g: &mut NudgeGate| g.provenance_authorized = false,
            |g: &mut NudgeGate| g.quiet_hours = true,
            |g: &mut NudgeGate| g.emotion_heavy = true,
        ] {
            let mut g = base_gate();
            mutate(&mut g);
            assert!(!g.eligible(), "a single blocker must silence the nudge");
        }
    }

    #[test]
    fn default_is_opt_out_no_send() {
        let mut g = base_gate();
        g.opted_in = false;
        assert!(!g.eligible(), "no send without informed class opt-in");
    }

    #[test]
    fn render_is_opportunity_first_never_guilting() {
        let with_hint = render("Asha", "Friday", Some("she's into pottery"));
        let no_hint = render("Rahul", "in 3 days", None);
        for r in [&with_hint, &no_hint] {
            assert!(is_clean(r), "render leaked banned deficit framing: {r}");
            assert!(r.contains("mute"), "controls must always be shown");
            assert!(r.contains("birthday"), "opportunity (the event) must lead");
        }
        assert!(with_hint.contains("pottery"), "memory-grounded offer");
        // The banned vocabulary really is caught.
        assert!(!is_clean("it's been 3 weeks since you talked to Rahul"));
    }

    #[test]
    fn event_key_is_one_shot_per_day() {
        let a = event_key("Asha", 1_700_000_000_000);
        let b = event_key(" asha ", 1_700_000_000_000 + 3600_000); // same day, spacing/case differ
        assert_eq!(a, b, "same person + day → one nudge");
        let c = event_key("Asha", 1_700_000_000_000 + 86_400_000);
        assert_ne!(a, c, "next year's birthday is a distinct event");
    }

    #[test]
    fn class_health_kills_on_two_harm_reports_and_trusts_on_calibration() {
        let mk = |fb: Option<&str>| NudgeAudit {
            event_key: "k".into(), person: "p".into(), provenance: "told".into(),
            controls_shown: true, ts_ms: 0, feedback: fb.map(String::from),
        };
        // Two harm reports in the last 10 → disabled.
        let mut kill = vec![mk(Some("helpful")); 8];
        kill.push(mk(Some("pressured")));
        kill.push(mk(Some("monitored")));
        assert_eq!(class_health(&kill), ClassHealth::KillDisabled);

        // 25 clean positives → Wilson LB clears 0.80 → trusted.
        let trusted = vec![mk(Some("helpful")); 25];
        assert_eq!(class_health(&trusted), ClassHealth::Trusted);

        // Few audits → still accumulating, never prematurely "trusted".
        let few = vec![mk(Some("helpful")); 5];
        assert_eq!(class_health(&few), ClassHealth::Accumulating);
        // Ignores are missing data, not harm and not success.
        let ignored = vec![mk(None); 30];
        assert_eq!(class_health(&ignored), ClassHealth::Accumulating);
    }
}
