//! escrow — INTERRUPTION ESCROW: make SILENCE accountable.
//!
//! Sol's #5 (rid 019f4c65). Most days this system says nothing, and that is the point — but an
//! unaccountable silence is indistinguishable from a broken feature. Today the mind decides not to
//! speak dozens of times and leaves no trace, so "it never bothers me" and "it is silently failing"
//! look identical from the outside. This module makes the *choice not to speak* a first-class,
//! reviewable record: what was held, what it was worth, what it would have cost, and why the mind
//! stayed quiet.
//!
//!   "I held three low-stakes notes during family time; one has now crossed your
//!    'tell me before it becomes expensive' rule — the fare changed and your options expire at 6."
//!
//! THE RULE THAT KEEPS THIS FROM BECOMING A BACKLOG DUMP: a held candidate may resurface ONLY after
//! an OBSERVED MATERIAL CHANGE — never because time merely passed. That distinction is the whole
//! design. A queue that releases on elapsed time inevitably floods the user the moment the gate
//! opens; a queue that releases on *change* stays silent forever if nothing actually changed, which
//! is the correct behaviour. Low-value candidates EXPIRE rather than accumulate.
//!
//! This is the same failure that killed the tension drive (measured 2026-07-25: 2,602 open urges,
//! 17 ever discharged, ranked so the oldest could never surface). Escrow is built to not repeat it:
//! bounded, expiring, and released by evidence rather than by patience.

use serde_json::Value;

/// Why the mind chose not to speak. Each is a legitimate reason; recording WHICH one is what makes
/// the silence reviewable rather than mysterious.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Silence {
    /// The user muted this class. A standing instruction, not a judgement call.
    Muted,
    /// Already knocked today — restraint by budget.
    DailyCap,
    /// Predicted engagement was below the lowest speakable band. Not worth the interruption.
    BelowBand,
    /// The recipient looked unreceptive right now (timing, not value).
    Unreceptive,
}

impl Silence {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Silence::Muted => "muted",
            Silence::DailyCap => "daily-cap",
            Silence::BelowBand => "below-band",
            Silence::Unreceptive => "unreceptive",
        }
    }
    /// Human phrasing for the silence report.
    pub(crate) fn explain(&self) -> &'static str {
        match self {
            Silence::Muted => "you muted these",
            Silence::DailyCap => "I'd already used today's one interruption",
            Silence::BelowBand => "I wasn't confident enough it was worth your attention",
            Silence::Unreceptive => "it looked like a bad moment",
        }
    }
}

/// A cheap content fingerprint of a candidate. Two candidates with the same fingerprint are "the
/// same thing"; a changed fingerprint is the OBSERVED MATERIAL CHANGE that may release a held item.
/// Deliberately content-based (title + body + expiry) rather than time-based.
pub(crate) fn material_hash(p: &Value) -> u64 {
    let mut h: u64 = 1469598103934665603; // FNV-1a
    let mut eat = |s: &str| {
        for b in s.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(1099511628211);
        }
    };
    eat(p.get("title").and_then(|x| x.as_str()).unwrap_or(""));
    eat(p.get("body").and_then(|x| x.as_str()).unwrap_or(""));
    eat(&p
        .get("expiry_ms")
        .and_then(|x| x.as_i64())
        .unwrap_or(0)
        .to_string());
    h
}

/// May a previously-held candidate be offered again?
///
/// ONLY on a material change. Elapsed time is explicitly not a reason — this is the guard against
/// the escrow becoming a delayed-notification backlog that dumps itself when the gate opens.
/// A muted class never resurfaces at all; the user's standing instruction outranks new evidence.
pub(crate) fn may_resurface(prev_hash: u64, now_hash: u64, prev_reason: Silence) -> bool {
    if prev_reason == Silence::Muted {
        return false;
    }
    prev_hash != now_hash
}

/// Drop held records that are no longer worth carrying: past their expiry, or low-value and stale.
/// Returns the retained set. Bounded by construction so the ledger cannot grow without limit.
pub(crate) fn prune(
    mut held: Vec<Value>,
    now_ms: i64,
    stale_days: i64,
    low_value: f64,
) -> Vec<Value> {
    let cutoff = now_ms - stale_days * 86_400_000;
    held.retain(|h| {
        let at = h.get("at_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        let benefit = h
            .get("predicted_benefit")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        // A high-value hold is kept longer; a low-value one ages out fast rather than accumulating.
        if benefit < low_value {
            at > cutoff
        } else {
            at > cutoff - 7 * 86_400_000
        }
    });
    if held.len() > 100 {
        let cut = held.len() - 100;
        held.drain(..cut);
    }
    held
}

/// Render the accountability report: what the mind chose NOT to say, and why.
pub(crate) fn render(held: &[Value]) -> String {
    if held.is_empty() {
        return "🤫 Interruption escrow: nothing held back — I haven't had anything worth interrupting you for.".into();
    }
    let mut s = format!(
        "🤫 Interruption escrow — {} thing(s) I chose NOT to interrupt you with:\n",
        held.len()
    );
    for h in held.iter().take(8) {
        let title = h
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("(untitled)");
        let why = h.get("reason").and_then(|x| x.as_str()).unwrap_or("?");
        let benefit = h
            .get("predicted_benefit")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        s.push_str(&format!(
            "   · \"{title}\" — held because {why} (I put its value at {benefit:.0}%)\n"
        ));
    }
    s.push_str(
        "   These surface only if something actually CHANGES about them — never just because time passed.",
    );
    s
}

impl super::ConversationEngine {
    async fn load_escrow(&self) -> Vec<Value> {
        self.memory
            .profile_get("interruption_escrow")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Record that the mind had something worth saying and chose not to say it — with what it was
    /// worth, why it stayed quiet, and a fingerprint of the candidate so a later MATERIAL change can
    /// release it. Re-holding the same unchanged candidate updates the existing row rather than
    /// stacking duplicates (the mistake that buried the tension ledger).
    pub(crate) async fn escrow_hold(
        &self,
        pkt: &Value,
        reason: Silence,
        benefit: f64,
        now_ms: i64,
    ) {
        let id = pkt
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let hash = material_hash(pkt);
        let mut held = self.load_escrow().await;
        if let Some(row) = held
            .iter_mut()
            .find(|h| h.get("candidate").and_then(|x| x.as_str()) == Some(id.as_str()))
        {
            row["reason"] = serde_json::json!(reason.as_str());
            row["material_hash"] = serde_json::json!(hash.to_string());
            row["times_held"] =
                serde_json::json!(row.get("times_held").and_then(|x| x.as_i64()).unwrap_or(1) + 1);
        } else {
            held.push(serde_json::json!({
                "candidate": id,
                "title": pkt.get("title").and_then(|x| x.as_str()).unwrap_or(""),
                "reason": reason.as_str(),
                "why": reason.explain(),
                "predicted_benefit": benefit,
                "material_hash": hash.to_string(),
                "at_ms": now_ms,
                "times_held": 1,
            }));
        }
        let stale_days: i64 = std::env::var("YM_ESCROW_STALE_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(14);
        let kept = prune(held, now_ms, stale_days, 0.55);
        let _ = self
            .memory
            .profile_set(
                "interruption_escrow",
                &serde_json::to_string(&kept).unwrap_or_default(),
            )
            .await;
    }

    /// Has this candidate been held before under a rule that still binds? Returns true when the
    /// mind should stay quiet about it — i.e. it was held and NOTHING material has changed since.
    /// Time passing is deliberately not a release condition.
    pub(crate) async fn escrow_still_held(&self, pkt: &Value) -> bool {
        let id = pkt.get("id").and_then(|x| x.as_str()).unwrap_or("");
        let held = self.load_escrow().await;
        let Some(row) = held
            .iter()
            .find(|h| h.get("candidate").and_then(|x| x.as_str()) == Some(id))
        else {
            return false;
        };
        let prev: u64 = row
            .get("material_hash")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let reason = match row.get("reason").and_then(|x| x.as_str()).unwrap_or("") {
            "muted" => Silence::Muted,
            "daily-cap" => Silence::DailyCap,
            "unreceptive" => Silence::Unreceptive,
            _ => Silence::BelowBand,
        };
        !may_resurface(prev, material_hash(pkt), reason)
    }

    /// The accountability surface: `ym silence`. What the mind chose not to interrupt you with.
    pub async fn escrow_report(&self) -> String {
        render(&self.load_escrow().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_800_000_000_000;
    const DAY: i64 = 86_400_000;

    fn pkt(title: &str, body: &str, expiry: i64) -> Value {
        json!({ "title": title, "body": body, "expiry_ms": expiry })
    }

    fn held(benefit: f64, at: i64) -> Value {
        json!({ "title": "t", "reason": "below-band", "predicted_benefit": benefit, "at_ms": at })
    }

    /// THE CORE RULE. Waiting is not new information — a held item must not free itself just because
    /// the clock moved, or the escrow becomes a backlog that floods the user when the gate opens.
    #[test]
    fn time_alone_never_resurfaces_a_held_candidate() {
        let p = pkt("Vendor quote", "accept / counter / decline", NOW + DAY);
        let h = material_hash(&p);
        // Same content, any amount of time later: still silent.
        assert!(!may_resurface(h, material_hash(&p), Silence::BelowBand));
        assert!(!may_resurface(h, material_hash(&p), Silence::DailyCap));
        assert!(!may_resurface(h, material_hash(&p), Silence::Unreceptive));
    }

    #[test]
    fn a_material_change_does_resurface_it() {
        let before = pkt(
            "Vendor quote",
            "accept at 4,200 / counter at 3,900",
            NOW + DAY,
        );
        // The number changed — that is real new information about the same thing.
        let after = pkt(
            "Vendor quote",
            "accept at 4,600 / counter at 4,100",
            NOW + DAY,
        );
        assert!(may_resurface(
            material_hash(&before),
            material_hash(&after),
            Silence::BelowBand
        ));
        // So does the deadline moving.
        let sooner = pkt(
            "Vendor quote",
            "accept at 4,200 / counter at 3,900",
            NOW + 3_600_000,
        );
        assert!(may_resurface(
            material_hash(&before),
            material_hash(&sooner),
            Silence::Unreceptive
        ));
    }

    #[test]
    fn a_muted_class_stays_silent_even_on_material_change() {
        let before = pkt("Vendor quote", "a", NOW + DAY);
        let after = pkt("Vendor quote", "b", NOW + DAY);
        assert!(
            !may_resurface(
                material_hash(&before),
                material_hash(&after),
                Silence::Muted
            ),
            "the user's standing instruction outranks new evidence"
        );
    }

    #[test]
    fn low_value_holds_expire_instead_of_accumulating() {
        let recent_low = held(0.2, NOW - 2 * DAY);
        let old_low = held(0.2, NOW - 20 * DAY);
        let old_high = held(0.9, NOW - 20 * DAY);
        let kept = prune(vec![recent_low, old_low, old_high], NOW, 14, 0.5);
        assert_eq!(
            kept.len(),
            2,
            "the stale low-value hold is dropped, the valuable one is kept"
        );
        assert!(kept
            .iter()
            .any(|h| h["predicted_benefit"].as_f64() == Some(0.9)));
        assert!(!kept
            .iter()
            .any(|h| h["predicted_benefit"].as_f64() == Some(0.2)
                && h["at_ms"].as_i64() == Some(NOW - 20 * DAY)));
    }

    #[test]
    fn the_ledger_is_bounded() {
        let many: Vec<Value> = (0..250).map(|i| held(0.9, NOW - i)).collect();
        assert!(
            prune(many, NOW, 14, 0.5).len() <= 100,
            "escrow can never grow without limit"
        );
    }

    #[test]
    fn the_report_states_what_was_held_and_why() {
        let s = render(&[held(0.62, NOW)]);
        assert!(s.contains("chose NOT to interrupt"));
        assert!(s.contains("below-band"));
        assert!(
            s.contains("never just because time passed"),
            "the rule is stated to the user"
        );
        // And the empty case reads as health, not absence.
        assert!(render(&[]).contains("nothing held back"));
    }
}
