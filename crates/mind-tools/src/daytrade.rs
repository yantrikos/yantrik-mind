//! Deterministic intraday setup and risk math for the Mind's paper-only day-trader.
//!
//! The language model may explain evidence, but it does not get to invent the price levels or the
//! size. Those are derived from printed bars and account equity here, in pure functions that can
//! be tested without a broker or a network.

use crate::market::Bar;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TradeSide {
    Long,
    Short,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayTradePlan {
    pub side: TradeSide,
    pub entry: f64,
    pub invalidation: f64,
    pub target: f64,
    pub setup: String,
}

impl DayTradePlan {
    pub fn risk_per_share(&self) -> f64 {
        (self.entry - self.invalidation).abs()
    }

    pub fn reward_per_share(&self) -> f64 {
        (self.target - self.entry).abs()
    }

    pub fn reward_to_risk(&self) -> f64 {
        let risk = self.risk_per_share();
        if risk <= 0.0 {
            0.0
        } else {
            self.reward_per_share() / risk
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DayRiskLimits {
    /// Maximum account equity intentionally lost if one stop fills as planned.
    pub risk_fraction_per_trade: f64,
    /// Hard session drawdown gate, measured against session-start equity.
    pub max_daily_loss_fraction: f64,
    /// A day-trading agent earns the right to be selective; it may not churn indefinitely.
    pub max_entries_per_session: u32,
    /// Independent of stop distance, no order may occupy more than this share of equity.
    pub max_notional_fraction: f64,
    pub min_reward_to_risk: f64,
}

impl Default for DayRiskLimits {
    fn default() -> Self {
        Self {
            risk_fraction_per_trade: 0.0025,
            max_daily_loss_fraction: 0.01,
            max_entries_per_session: 3,
            max_notional_fraction: 0.10,
            min_reward_to_risk: 2.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DayRiskState {
    #[serde(default)]
    pub session_date: String,
    #[serde(default)]
    pub session_start_equity: f64,
    #[serde(default)]
    pub entries: u32,
    #[serde(default)]
    pub halted_reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DayTradeRefusal {
    InvalidPlan,
    RewardTooSmall,
    SessionHalted(String),
    DailyLossLimit,
    EntryLimit,
    SizeBelowOneShare,
}

impl std::fmt::Display for DayTradeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPlan => write!(
                f,
                "entry, invalidation, and target do not form a valid trade"
            ),
            Self::RewardTooSmall => write!(f, "planned reward does not justify the planned risk"),
            Self::SessionHalted(reason) => write!(f, "session is halted: {reason}"),
            Self::DailyLossLimit => write!(f, "session drawdown reached the daily loss limit"),
            Self::EntryLimit => write!(f, "session entry limit reached"),
            Self::SizeBelowOneShare => {
                write!(f, "risk and notional caps allow less than one share")
            }
        }
    }
}

/// A single transparent playbook: a confirmed break from the first fifteen-minute range.
///
/// The fourth-or-later five-minute bar must close outside the opening range with at least 80% of
/// the opening bars' average volume. The old boundary becomes invalidation and the target is two
/// planned risk units away. Very tiny or very wide stops are rejected because both are usually a
/// data/latency artefact rather than an executable setup.
pub fn opening_range_breakout(bars: &[Bar]) -> Option<DayTradePlan> {
    if bars.len() < 4 {
        return None;
    }
    let opening = &bars[..3];
    if opening.iter().any(|bar| !valid_bar(bar)) {
        return None;
    }
    let latest = bars.last()?;
    if !valid_bar(latest) {
        return None;
    }
    let opening_high = opening
        .iter()
        .map(|bar| bar.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let opening_low = opening
        .iter()
        .map(|bar| bar.low)
        .fold(f64::INFINITY, f64::min);
    let opening_avg_volume = opening.iter().map(|bar| bar.volume).sum::<f64>() / 3.0;
    if opening_avg_volume > 0.0 && latest.volume < opening_avg_volume * 0.8 {
        return None;
    }

    let (side, invalidation) = if latest.close > opening_high {
        (TradeSide::Long, opening_high)
    } else if latest.close < opening_low {
        (TradeSide::Short, opening_low)
    } else {
        return None;
    };
    let entry = latest.close;
    let risk = (entry - invalidation).abs();
    let risk_fraction = risk / entry;
    if !(0.0015..=0.015).contains(&risk_fraction) {
        return None;
    }
    let target = match side {
        TradeSide::Long => entry + 2.0 * risk,
        TradeSide::Short => entry - 2.0 * risk,
    };
    Some(DayTradePlan {
        side,
        entry,
        invalidation,
        target,
        setup: "15-minute opening-range breakout with volume confirmation".to_string(),
    })
}

/// Admit and size one paper trade from the risk budget. Returns whole-share quantity.
pub fn size_for_risk(
    equity: f64,
    current_equity: f64,
    state: &DayRiskState,
    limits: DayRiskLimits,
    plan: &DayTradePlan,
) -> Result<f64, DayTradeRefusal> {
    if !state.halted_reason.is_empty() {
        return Err(DayTradeRefusal::SessionHalted(state.halted_reason.clone()));
    }
    if state.entries >= limits.max_entries_per_session {
        return Err(DayTradeRefusal::EntryLimit);
    }
    if state.session_start_equity > 0.0
        && current_equity <= state.session_start_equity * (1.0 - limits.max_daily_loss_fraction)
    {
        return Err(DayTradeRefusal::DailyLossLimit);
    }
    if !valid_plan(plan) {
        return Err(DayTradeRefusal::InvalidPlan);
    }
    if plan.reward_to_risk() + f64::EPSILON < limits.min_reward_to_risk {
        return Err(DayTradeRefusal::RewardTooSmall);
    }
    if equity <= 0.0 || current_equity <= 0.0 {
        return Err(DayTradeRefusal::InvalidPlan);
    }
    let by_risk = (current_equity * limits.risk_fraction_per_trade / plan.risk_per_share()).floor();
    let by_notional = (equity * limits.max_notional_fraction / plan.entry).floor();
    let qty = by_risk.min(by_notional);
    if qty < 1.0 {
        Err(DayTradeRefusal::SizeBelowOneShare)
    } else {
        Ok(qty)
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

fn valid_plan(plan: &DayTradePlan) -> bool {
    [plan.entry, plan.invalidation, plan.target]
        .into_iter()
        .all(|value| value.is_finite() && value > 0.0)
        && match plan.side {
            TradeSide::Long => plan.invalidation < plan.entry && plan.entry < plan.target,
            TradeSide::Short => plan.target < plan.entry && plan.entry < plan.invalidation,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(open: f64, high: f64, low: f64, close: f64, volume: f64) -> Bar {
        Bar {
            time: "2026-08-31T13:30:00Z".to_string(),
            open,
            high,
            low,
            close,
            volume,
        }
    }

    #[test]
    fn confirmed_opening_range_breakout_has_falsifiable_levels() {
        let bars = vec![
            bar(100.0, 100.8, 99.7, 100.4, 1_000.0),
            bar(100.4, 101.0, 100.1, 100.8, 1_100.0),
            bar(100.8, 101.2, 100.5, 101.0, 900.0),
            bar(101.0, 101.8, 100.9, 101.6, 1_200.0),
        ];
        let plan = opening_range_breakout(&bars).expect("confirmed breakout");
        assert_eq!(plan.side, TradeSide::Long);
        assert_eq!(plan.invalidation, 101.2);
        assert!((plan.reward_to_risk() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn range_noise_and_weak_volume_are_not_setups() {
        let base = vec![
            bar(100.0, 101.0, 99.0, 100.0, 1_000.0),
            bar(100.0, 101.0, 99.0, 100.0, 1_000.0),
            bar(100.0, 101.0, 99.0, 100.0, 1_000.0),
        ];
        let mut inside = base.clone();
        inside.push(bar(100.0, 100.8, 99.2, 100.5, 1_000.0));
        assert!(opening_range_breakout(&inside).is_none());
        let mut weak = base;
        weak.push(bar(100.8, 101.5, 100.7, 101.3, 100.0));
        assert!(opening_range_breakout(&weak).is_none());
    }

    #[test]
    fn sizing_is_stop_based_and_bounded_by_notional() {
        let plan = DayTradePlan {
            side: TradeSide::Long,
            entry: 100.0,
            invalidation: 99.0,
            target: 102.0,
            setup: "test".to_string(),
        };
        let qty = size_for_risk(
            100_000.0,
            100_000.0,
            &DayRiskState::default(),
            DayRiskLimits::default(),
            &plan,
        )
        .unwrap();
        assert_eq!(
            qty, 100.0,
            "10% notional cap binds before the $250 risk cap"
        );
    }

    #[test]
    fn daily_loss_and_entry_limits_are_hard_gates() {
        let plan = DayTradePlan {
            side: TradeSide::Short,
            entry: 100.0,
            invalidation: 101.0,
            target: 98.0,
            setup: "test".to_string(),
        };
        let mut state = DayRiskState {
            session_start_equity: 100_000.0,
            ..Default::default()
        };
        assert_eq!(
            size_for_risk(100_000.0, 99_000.0, &state, DayRiskLimits::default(), &plan,),
            Err(DayTradeRefusal::DailyLossLimit)
        );
        state.entries = 3;
        assert_eq!(
            size_for_risk(
                100_000.0,
                100_000.0,
                &state,
                DayRiskLimits::default(),
                &plan,
            ),
            Err(DayTradeRefusal::EntryLimit)
        );
    }
}
