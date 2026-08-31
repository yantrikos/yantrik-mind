//! Deterministic signal and risk math for the Mind's paper-only 24/7 crypto agent.
//!
//! Crypto is not an equities session stretched across the weekend: it is continuous, fractional,
//! long/flat spot trading. The model may explain evidence, but bars determine the levels and pure
//! arithmetic determines the notional allocation.

use crate::market::Bar;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CryptoTradePlan {
    pub entry: f64,
    pub invalidation: f64,
    pub target: f64,
    pub setup: String,
}

impl CryptoTradePlan {
    pub fn risk_fraction(&self) -> f64 {
        (self.entry - self.invalidation) / self.entry
    }

    pub fn reward_to_risk(&self) -> f64 {
        let risk = self.entry - self.invalidation;
        if risk <= 0.0 {
            0.0
        } else {
            (self.target - self.entry) / risk
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CryptoRiskLimits {
    pub risk_fraction_per_trade: f64,
    pub max_daily_loss_fraction: f64,
    pub max_entries_per_utc_day: u32,
    pub max_notional_fraction: f64,
    pub min_reward_to_risk: f64,
    pub min_notional: f64,
}

impl Default for CryptoRiskLimits {
    fn default() -> Self {
        Self {
            risk_fraction_per_trade: 0.002,
            max_daily_loss_fraction: 0.0075,
            max_entries_per_utc_day: 2,
            max_notional_fraction: 0.05,
            min_reward_to_risk: 2.0,
            min_notional: 10.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CryptoRiskState {
    #[serde(default)]
    pub utc_date: String,
    #[serde(default)]
    pub start_equity: f64,
    #[serde(default)]
    pub entries: u32,
    #[serde(default)]
    pub halted_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoRefusal {
    InvalidPlan,
    RewardTooSmall,
    Halted(String),
    DailyLossLimit,
    EntryLimit,
    NotionalTooSmall,
}

impl std::fmt::Display for CryptoRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPlan => write!(f, "crypto entry, stop, and target are invalid"),
            Self::RewardTooSmall => write!(f, "planned crypto reward is too small for the risk"),
            Self::Halted(reason) => write!(f, "crypto risk is halted: {reason}"),
            Self::DailyLossLimit => write!(f, "UTC-day crypto loss limit reached"),
            Self::EntryLimit => write!(f, "UTC-day crypto entry limit reached"),
            Self::NotionalTooSmall => write!(f, "risk caps permit less than the minimum notional"),
        }
    }
}

/// A long-only spot setup: the latest completed 15-minute bar must close above the prior eight
/// hours' high, above their mean close, and carry at least 1.25x their average volume.
pub fn continuous_breakout(bars: &[Bar]) -> Option<CryptoTradePlan> {
    const LOOKBACK: usize = 32;
    if bars.len() < LOOKBACK + 1 {
        return None;
    }
    let window = &bars[bars.len() - (LOOKBACK + 1)..];
    if window.iter().any(|bar| !valid_bar(bar)) {
        return None;
    }
    let prior = &window[..LOOKBACK];
    let latest = window.last()?;
    let prior_high = prior
        .iter()
        .map(|bar| bar.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let mean_close = prior.iter().map(|bar| bar.close).sum::<f64>() / LOOKBACK as f64;
    let mean_volume = prior.iter().map(|bar| bar.volume).sum::<f64>() / LOOKBACK as f64;
    if latest.close <= prior_high
        || latest.close <= mean_close
        || mean_volume <= 0.0
        || latest.volume < mean_volume * 1.25
    {
        return None;
    }
    let entry = latest.close;
    let invalidation = prior_high;
    let risk = entry - invalidation;
    let risk_fraction = risk / entry;
    if !(0.004..=0.03).contains(&risk_fraction) {
        return None;
    }
    Some(CryptoTradePlan {
        entry,
        invalidation,
        target: entry + 2.0 * risk,
        setup: "15-minute breakout above the prior 8-hour high with trend and volume confirmation"
            .to_string(),
    })
}

/// Risk-derived dollar notional for a fractional spot order.
pub fn notional_for_risk(
    equity: f64,
    current_equity: f64,
    state: &CryptoRiskState,
    limits: CryptoRiskLimits,
    plan: &CryptoTradePlan,
) -> Result<f64, CryptoRefusal> {
    if !state.halted_reason.is_empty() {
        return Err(CryptoRefusal::Halted(state.halted_reason.clone()));
    }
    if state.entries >= limits.max_entries_per_utc_day {
        return Err(CryptoRefusal::EntryLimit);
    }
    if state.start_equity > 0.0
        && current_equity <= state.start_equity * (1.0 - limits.max_daily_loss_fraction)
    {
        return Err(CryptoRefusal::DailyLossLimit);
    }
    if !valid_plan(plan) || equity <= 0.0 || current_equity <= 0.0 {
        return Err(CryptoRefusal::InvalidPlan);
    }
    if plan.reward_to_risk() + f64::EPSILON < limits.min_reward_to_risk {
        return Err(CryptoRefusal::RewardTooSmall);
    }
    let by_risk = current_equity * limits.risk_fraction_per_trade / plan.risk_fraction();
    let by_notional = equity * limits.max_notional_fraction;
    let notional = by_risk.min(by_notional).floor();
    if notional < limits.min_notional {
        Err(CryptoRefusal::NotionalTooSmall)
    } else {
        Ok(notional)
    }
}

fn valid_bar(bar: &Bar) -> bool {
    [bar.open, bar.high, bar.low, bar.close, bar.volume]
        .into_iter()
        .all(f64::is_finite)
        && bar.open > 0.0
        && bar.high >= bar.low
        && bar.close > 0.0
        && bar.volume >= 0.0
}

fn valid_plan(plan: &CryptoTradePlan) -> bool {
    [plan.entry, plan.invalidation, plan.target]
        .into_iter()
        .all(|value| value.is_finite() && value > 0.0)
        && plan.invalidation < plan.entry
        && plan.entry < plan.target
        && plan.risk_fraction() > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(close: f64, high: f64, volume: f64) -> Bar {
        Bar {
            time: "2026-08-30T00:00:00Z".to_string(),
            open: close,
            high,
            low: close * 0.995,
            close,
            volume,
        }
    }

    #[test]
    fn a_confirmed_continuous_breakout_has_falsifiable_levels() {
        let mut bars = (0..32)
            .map(|index| bar(100.0 + index as f64 * 0.01, 100.5, 100.0))
            .collect::<Vec<_>>();
        bars.push(bar(101.2, 101.4, 150.0));
        let plan = continuous_breakout(&bars).expect("confirmed breakout");
        assert_eq!(plan.invalidation, 100.5);
        assert!((plan.reward_to_risk() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn weak_volume_and_inside_noise_are_not_crypto_setups() {
        let base = (0..32)
            .map(|_| bar(100.0, 100.5, 100.0))
            .collect::<Vec<_>>();
        let mut inside = base.clone();
        inside.push(bar(100.4, 100.6, 150.0));
        assert!(continuous_breakout(&inside).is_none());
        let mut weak = base;
        weak.push(bar(101.2, 101.4, 100.0));
        assert!(continuous_breakout(&weak).is_none());
    }

    #[test]
    fn crypto_notional_is_fractional_and_bounded() {
        let plan = CryptoTradePlan {
            entry: 100.0,
            invalidation: 99.0,
            target: 102.0,
            setup: "test".to_string(),
        };
        assert_eq!(
            notional_for_risk(
                100_000.0,
                100_000.0,
                &CryptoRiskState::default(),
                CryptoRiskLimits::default(),
                &plan,
            ),
            Ok(5_000.0),
            "the five-percent notional cap binds before the risk budget"
        );
    }

    #[test]
    fn utc_loss_and_entry_limits_are_hard_gates() {
        let plan = CryptoTradePlan {
            entry: 100.0,
            invalidation: 99.0,
            target: 102.0,
            setup: "test".to_string(),
        };
        let mut state = CryptoRiskState {
            start_equity: 100_000.0,
            ..Default::default()
        };
        assert_eq!(
            notional_for_risk(
                100_000.0,
                99_250.0,
                &state,
                CryptoRiskLimits::default(),
                &plan,
            ),
            Err(CryptoRefusal::DailyLossLimit)
        );
        state.entries = 2;
        assert_eq!(
            notional_for_risk(
                100_000.0,
                100_000.0,
                &state,
                CryptoRiskLimits::default(),
                &plan,
            ),
            Err(CryptoRefusal::EntryLimit)
        );
    }
}
