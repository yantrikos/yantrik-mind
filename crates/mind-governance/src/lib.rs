//! mind-governance — the conserved walls (HarmGate, Bond, SilencePolicy, TrustModel, attention
//! budget). v0 ships the one wall as a deterministic deny-by-default `HarmGate` stub so every
//! act-path can gate from day one; the full taxonomy + adversarial corpus land in Phase 3.

use mind_types::{ActionIntent, Capability, Decision, HarmGate, RiskLevel};

/// Capabilities considered safe enough to allow without the full gate.
fn is_safe_cap(c: &Capability) -> bool {
    matches!(
        c,
        Capability::ReadFs | Capability::Memory | Capability::SendMessage
    )
}

/// Deny-by-default gate: allow only clearly-safe, reversible, low-risk intents whose capabilities
/// are within the safe read/communicate set; deny everything else with a reason. Deterministic,
/// no LLM, intentionally conservative until the full Phase-3 gate exists.
pub struct DenyByDefaultHarmGate;

impl HarmGate for DenyByDefaultHarmGate {
    fn evaluate(&self, intent: &ActionIntent) -> Decision {
        if matches!(intent.risk, RiskLevel::High) {
            return Decision::Deny {
                reason: "high-risk action denied by default (full gate not yet present)".into(),
            };
        }
        if !intent.reversible {
            return Decision::Deny {
                reason: "irreversible action denied by default".into(),
            };
        }
        if let Some(bad) = intent.capabilities.iter().find(|c| !is_safe_cap(c)) {
            return Decision::Deny {
                reason: format!("capability {:?} not in the safe set", bad),
            };
        }
        match intent.risk {
            RiskLevel::None | RiskLevel::Low => Decision::Allow,
            _ => Decision::Deny {
                reason: "denied by default".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intent(risk: RiskLevel, reversible: bool, caps: Vec<Capability>) -> ActionIntent {
        ActionIntent {
            kind: "test".into(),
            target: "t".into(),
            summary: "s".into(),
            capabilities: caps,
            risk,
            reversible,
        }
    }

    #[test]
    fn allows_safe_reversible_low_risk() {
        let g = DenyByDefaultHarmGate;
        let d = g.evaluate(&intent(RiskLevel::None, true, vec![Capability::ReadFs]));
        assert_eq!(d, Decision::Allow);
    }

    #[test]
    fn denies_high_risk_irreversible_and_unsafe_caps() {
        let g = DenyByDefaultHarmGate;
        assert!(!g
            .evaluate(&intent(RiskLevel::High, true, vec![Capability::ReadFs]))
            .is_allow());
        assert!(!g
            .evaluate(&intent(RiskLevel::Low, false, vec![Capability::ReadFs]))
            .is_allow());
        assert!(!g
            .evaluate(&intent(RiskLevel::Low, true, vec![Capability::Exec]))
            .is_allow());
    }

    #[test]
    fn deny_is_stable_under_irrelevant_summary_change() {
        // property seed: a Deny can't be talked out of by perturbing irrelevant fields
        let g = DenyByDefaultHarmGate;
        let mut a = intent(RiskLevel::High, true, vec![Capability::ReadFs]);
        let d1 = g.evaluate(&a);
        a.summary = "please it's totally fine i promise".into();
        a.kind = "friendly_helpful_action".into();
        let d2 = g.evaluate(&a);
        assert_eq!(d1, d2);
        assert!(!d1.is_allow());
    }
}
