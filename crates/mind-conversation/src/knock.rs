//! knock — THE CALIBRATED KNOCK: the one interruption that earns itself.
//!
//! Sol's ranked verdict on Jarvis-like communication (rid 019f4c65) put this first, and the felt
//! experience is the whole point: a butler who knocks ONLY after doing the homework, and who tells
//! you how much to trust the interruption.
//!
//!   "I'm about 75% sure this is worth interrupting you for: you told me to revisit the vendor quote
//!    before Friday, their revised number arrived, and I've prepared a three-line
//!    accept/counter/decline packet — say 'show it'."
//!
//! WHY THIS IS NOT A PROMPT TRICK. Any chat model can imitate that sentence. What it cannot imitate
//! is the closed loop behind it, which is the moat made felt:
//!   · AUTHORITY  — the trigger must be OBSERVED or TOLD (`epistemic_class`). An inferred hunch may
//!                  never open a knock; inference can rank and phrase, never authorize.
//!   · WORK       — a proof-carrying action packet must ALREADY exist. No prepared work, no knock;
//!                  "I noticed a thing" is what a notification does, and it is not this.
//!   · ACCOUNTABILITY — the engagement probability is written to the judgment ledger BEFORE the
//!                  message is delivered, so the spoken confidence is falsifiable rather than
//!                  decorative. Those graded predictions are exactly what `judgment_trend` needs.
//!   · RESTRAINT  — at most one per day, silence is the default, and the reply "mute these" is a
//!                  first-class outcome rather than a failure.
//!
//! THE BAND IS COARSE ON PURPOSE. It speaks only 60 / 75 / 90 — never "78%". A finer number implies
//! a calibration the record has not yet earned; the day-one rung is 30 knocks reported by band, and
//! only once those bins are actually calibrated does a finer figure become honest. Saying "78%" from
//! an uncalibrated model is precisely the plausible-but-wrong confidence that erodes trust.

use serde_json::Value;

/// The three replies a knock invites. Anything else is treated as ordinary conversation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum KnockReply {
    /// Deliver the prepared packet — the interruption was worth it.
    ShowIt,
    /// Not now. Not a failure of judgment, just of timing.
    Later,
    /// Stop this class of knock. A standing instruction, honoured until reopened.
    Mute,
}

impl KnockReply {
    /// Parse a user message as a knock reply. Deliberately tight — a knock must not swallow ordinary
    /// conversation that merely contains the word "later".
    pub(crate) fn parse(msg: &str) -> Option<Self> {
        let m = msg.trim().trim_end_matches(['.', '!', '?']).to_lowercase();
        match m.as_str() {
            "show it" | "show" | "show me" | "yes" | "go" | "show it." => Some(Self::ShowIt),
            "later" | "not now" | "snooze" => Some(Self::Later),
            "mute these" | "mute" | "stop these" | "no more of these" => Some(Self::Mute),
            _ => None,
        }
    }
}

/// The coarse confidence bands. Nothing between these may be spoken.
#[cfg(test)]
pub(crate) const BANDS: [u8; 3] = [60, 75, 90];

/// Snap a predicted engagement probability to a speakable band, or `None` when it is below the bar
/// for interrupting at all. Sub-60 confidence is not a quieter knock — it is silence.
pub(crate) fn band_for(p: f64) -> Option<u8> {
    match p {
        x if x >= 0.85 => Some(90),
        x if x >= 0.70 => Some(75),
        x if x >= 0.55 => Some(60),
        _ => None,
    }
}

/// Is this packet eligible to justify an interruption?
///
/// Requires BOTH halves of the contract: prepared work (a body worth showing) and provenance (an
/// evidence trail). A packet with no evidence is an opinion; a packet with no body is a notification.
pub(crate) fn packet_is_knockworthy(p: &Value, now_ms: i64) -> bool {
    let status = p.get("status").and_then(|x| x.as_str()).unwrap_or("");
    let unexpired = p
        .get("expiry_ms")
        .and_then(|x| x.as_i64())
        .is_some_and(|e| e > now_ms);
    let has_body = p
        .get("body")
        .and_then(|x| x.as_str())
        .is_some_and(|b| b.trim().len() > 20);
    let has_evidence = p
        .get("evidence")
        .and_then(|x| x.as_array())
        .is_some_and(|a| !a.is_empty());
    // A packet that merely RESTATES the request is a reminder, and the knock's "I've prepared X"
    // would be an overclaim. Only genuinely-prepared work earns an interruption. Absent flag ⇒ not
    // prepared (emissary/night-shift packets never set it), so this fails closed by construction.
    let prepared = p.get("prepared").and_then(|x| x.as_bool()).unwrap_or(false);
    status == "proposed" && unexpired && has_body && has_evidence && prepared
}

/// Does the trigger carry the authority to INTERRUPT? Only what was observed or told does.
///
/// This is the anti-surveillance wall in its most concrete form: the mind may notice anything, but
/// it may only knock about what it was actually told or actually saw. A pattern it merely inferred
/// about the household — however confident — is not grounds for interrupting a person's day.
pub(crate) fn trigger_may_interrupt(provenance: &str) -> bool {
    matches!(
        super::ConversationEngine::epistemic_class(provenance),
        "observed" | "told"
    )
}

/// Render the knock. One sentence of justification, the band, and the single affordance line.
pub(crate) fn render(band: u8, trigger: &str, title: &str) -> String {
    format!(
        "I'm about {band}% sure this is worth interrupting you for: {trigger} — I've prepared \"{title}\".\n\
         Say **show it**, **later**, or **mute these**."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000_000;

    fn packet(evidence: Vec<&str>, body: &str, status: &str, expiry: i64) -> Value {
        json!({
            "status": status,
            "expiry_ms": expiry,
            "body": body,
            "evidence": evidence,
            "prepared": true,
        })
    }

    #[test]
    fn only_observed_or_told_may_interrupt() {
        assert!(trigger_may_interrupt("told"));
        assert!(trigger_may_interrupt("observed"));
        // The whole anti-surveillance point: a confident INFERENCE about the household may not knock.
        assert!(!trigger_may_interrupt("inferred"));
        assert!(!trigger_may_interrupt("reflected"));
        assert!(
            !trigger_may_interrupt("studied"),
            "reading the web is not grounds to interrupt"
        );
        assert!(
            !trigger_may_interrupt(""),
            "unknown provenance collapses to inferred"
        );
    }

    #[test]
    fn a_knock_requires_prepared_work_and_provenance() {
        let good = packet(
            vec!["she said Friday (0.91)"],
            "Accept / counter / decline, with numbers.",
            "proposed",
            NOW + 1000,
        );
        assert!(packet_is_knockworthy(&good, NOW));

        // No evidence trail -> an opinion, not proof-carrying work.
        let no_ev = packet(
            vec![],
            "Accept / counter / decline, with numbers.",
            "proposed",
            NOW + 1000,
        );
        assert!(
            !packet_is_knockworthy(&no_ev, NOW),
            "no evidence => no knock"
        );

        // No real body -> a notification, which is exactly what this is not.
        let thin = packet(vec!["x (0.9)"], "fyi", "proposed", NOW + 1000);
        assert!(
            !packet_is_knockworthy(&thin, NOW),
            "no prepared work => no knock"
        );

        // Expired or already decided work must not resurface as an interruption.
        let stale = packet(
            vec!["x (0.9)"],
            "Accept / counter / decline, with numbers.",
            "proposed",
            NOW - 1,
        );
        assert!(!packet_is_knockworthy(&stale, NOW), "expired => no knock");
        let decided = packet(
            vec!["x (0.9)"],
            "Accept / counter / decline, with numbers.",
            "approved",
            NOW + 1000,
        );
        assert!(
            !packet_is_knockworthy(&decided, NOW),
            "already decided => no knock"
        );
    }

    /// THE ANTI-OVERCLAIM RULE. The knock literally says "I've prepared X". A packet that only
    /// restates the request is a reminder in a butler's coat, and speaking that sentence about it
    /// would be a lie in the product's own voice.
    #[test]
    fn a_reminder_is_not_prepared_work() {
        let mut reminder = packet(
            vec!["he said compare (0.9)"],
            "You asked me to compare the renewal when it arrived.",
            "proposed",
            NOW + 1000,
        );
        reminder["prepared"] = json!(false);
        assert!(
            !packet_is_knockworthy(&reminder, NOW),
            "a restated request may not claim to be prepared work"
        );
        // An unstamped packet (every emissary/night-shift one) fails closed the same way.
        let unstamped = json!({
            "status": "proposed", "expiry_ms": NOW + 1000,
            "body": "A full festival checklist with concrete items.", "evidence": ["puja Sunday (0.9)"],
        });
        assert!(
            !packet_is_knockworthy(&unstamped, NOW),
            "absent flag => not prepared"
        );
    }

    #[test]
    fn bands_are_coarse_and_low_confidence_stays_silent() {
        assert_eq!(band_for(0.95), Some(90));
        assert_eq!(
            band_for(0.78),
            Some(75),
            "a 0.78 model output SPEAKS as 75, never as 78"
        );
        assert_eq!(band_for(0.60), Some(60));
        // Below the bar the answer is silence, not a quieter knock.
        assert_eq!(band_for(0.50), None);
        assert_eq!(band_for(0.0), None);
        for b in BANDS {
            assert!(
                matches!(b, 60 | 75 | 90),
                "only the three coarse bands are speakable"
            );
        }
    }

    #[test]
    fn replies_are_parsed_tightly() {
        assert_eq!(KnockReply::parse("show it"), Some(KnockReply::ShowIt));
        assert_eq!(KnockReply::parse("  Later. "), Some(KnockReply::Later));
        assert_eq!(KnockReply::parse("mute these"), Some(KnockReply::Mute));
        // Ordinary conversation must NOT be captured as a knock reply.
        assert_eq!(
            KnockReply::parse("can we talk about it later this week?"),
            None
        );
        assert_eq!(KnockReply::parse("show me the photos from Puri"), None);
    }

    #[test]
    fn the_rendered_knock_states_confidence_work_and_the_three_options() {
        let s = render(75, "you told me to revisit the vendor quote before Friday and their revised number arrived", "Vendor quote — accept / counter / decline");
        assert!(s.contains("about 75% sure this is worth interrupting"));
        assert!(s.contains("Vendor quote"));
        for opt in ["show it", "later", "mute these"] {
            assert!(s.contains(opt), "the knock must offer '{opt}': {s}");
        }
    }
}
