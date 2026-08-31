//! EXIT — the half of a trade that was missing.
//!
//! The first position this mind ever took was a short in WMT on a same-day thesis: "US sales growth
//! at its weakest pace since 2020 is a fundamental deterioration driving the -9% drop, offering a
//! clear short entry on the news". It was entered, logged as a graded prediction, and then held
//! overnight — because nothing in the system had an opinion about when to close.
//!
//! That is not a small omission. A SAME-DAY thesis held overnight is no longer that thesis: it has
//! become a swing trade that nobody decided on, exposed to a gap nobody sized for. The entry was a
//! judgment; continuing to hold was an accident.
//!
//! ## Why a time limit is the important one
//!
//! Stops and targets are the famous half, and they matter. But the rule that actually protects a
//! thesis is its HORIZON: a view about how the market digests today's news expires when today does.
//! Holding past it is not patience, it is forgetting what the bet was.
//!
//! Every exit says which rule fired, because "closed at a loss" and "closed because the thesis
//! expired" are different facts about the same trade, and only one of them says the strategy was
//! wrong.

use serde::{Deserialize, Serialize};

/// What a position is allowed to do before it must be closed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExitRule {
    /// Close if the position moves this far against the entry, in percent.
    pub stop_pct: f64,
    /// Close if it moves this far in favour, in percent.
    pub target_pct: f64,
    /// Close when the thesis horizon passes, whatever the price is doing.
    pub horizon_ms: i64,
}

impl Default for ExitRule {
    fn default() -> Self {
        // 3% stop and 5% target against a move that was already 9%: the position is sized small and
        // the thesis is about the rest of a move, not the whole of it. The horizon is the session —
        // a same-day view has no business surviving the day it was about.
        Self {
            stop_pct: 3.0,
            target_pct: 5.0,
            horizon_ms: 8 * 60 * 60 * 1000,
        }
    }
}

/// Which rule ended the trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    /// It went against the view far enough to say the view was wrong.
    Stopped,
    /// It went the predicted way far enough to take.
    Target,
    /// The thesis was about a window, and the window closed.
    HorizonPassed,
}

impl ExitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped out — the move went against the thesis",
            Self::Target => "target hit — the thesis played out",
            Self::HorizonPassed => "thesis expired — it was a same-day view and the day is over",
        }
    }

    /// Does this outcome say the VIEW was wrong? A horizon exit does not: the thesis simply ran out
    /// of time, which is a different fact from being mistaken, and conflating them would poison the
    /// ledger in both directions.
    pub fn says_the_view_was_wrong(self) -> bool {
        matches!(self, Self::Stopped)
    }
}

/// A position with its rule and its clock.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenPosition {
    pub symbol: String,
    /// Negative for a short.
    pub qty: f64,
    pub entry: f64,
    pub entered_at_ms: i64,
    pub rule: ExitRule,
}

impl OpenPosition {
    pub fn is_short(&self) -> bool {
        self.qty < 0.0
    }

    /// Percent move IN FAVOUR of the position. Negative means it is losing.
    ///
    /// Sign handling is the whole point: for a short, a falling price is a gain. Getting this
    /// backwards would close winners at the stop and hold losers to the target.
    pub fn favour_pct(&self, price: f64) -> f64 {
        if self.entry <= 0.0 {
            return 0.0;
        }
        let raw = (price / self.entry - 1.0) * 100.0;
        if self.is_short() {
            -raw
        } else {
            raw
        }
    }
}

/// Should this position be closed now, and why?
pub fn should_close(p: &OpenPosition, price: f64, now_ms: i64) -> Option<ExitReason> {
    let fav = p.favour_pct(price);
    if fav <= -p.rule.stop_pct {
        return Some(ExitReason::Stopped);
    }
    if fav >= p.rule.target_pct {
        return Some(ExitReason::Target);
    }
    if now_ms.saturating_sub(p.entered_at_ms) >= p.rule.horizon_ms {
        return Some(ExitReason::HorizonPassed);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    fn wmt_short(at_ms: i64) -> OpenPosition {
        // The real first position.
        OpenPosition {
            symbol: "WMT".into(),
            qty: -2.0,
            entry: 103.75,
            entered_at_ms: at_ms,
            rule: ExitRule::default(),
        }
    }

    #[test]
    fn a_falling_price_is_a_gain_for_a_short() {
        // The sign error that would close every winner at the stop and ride every loser to the
        // target. Worth a test of its own because it is silent: the numbers all look plausible.
        let p = wmt_short(0);
        assert!(p.favour_pct(100.0) > 0.0, "price down = short in profit");
        assert!(p.favour_pct(107.0) < 0.0, "price up = short losing");
        let long = OpenPosition { qty: 2.0, ..p };
        assert!(long.favour_pct(107.0) > 0.0);
        assert!(long.favour_pct(100.0) < 0.0);
    }

    #[test]
    fn the_real_position_is_held_while_it_is_going_nowhere() {
        // WMT closed at 103.84 against a 103.75 entry — a rounding error, not a signal. Nothing
        // should fire on that.
        let p = wmt_short(0);
        assert_eq!(should_close(&p, 103.84, HOUR), None);
    }

    #[test]
    fn a_same_day_thesis_does_not_survive_the_day() {
        // THE omission this module exists for. The WMT short was a view about how the market
        // digested one morning's news, and it was held overnight because nothing had an opinion
        // about when a thesis expires. Holding past the horizon is not patience — it is a different
        // trade, entered by forgetting.
        let p = wmt_short(0);
        assert_eq!(
            should_close(&p, 103.84, 9 * HOUR),
            Some(ExitReason::HorizonPassed)
        );
    }

    #[test]
    fn a_horizon_exit_does_not_mean_the_view_was_wrong() {
        // "Closed at a loss" and "closed because time ran out" are different facts about the same
        // trade, and only one of them scores against the strategy.
        assert!(!ExitReason::HorizonPassed.says_the_view_was_wrong());
        assert!(!ExitReason::Target.says_the_view_was_wrong());
        assert!(ExitReason::Stopped.says_the_view_was_wrong());
    }

    #[test]
    fn the_stop_fires_before_the_horizon_does() {
        // A position that is already wrong should not wait for the clock.
        let p = wmt_short(0);
        assert_eq!(
            should_close(&p, 107.0, 1),
            Some(ExitReason::Stopped),
            "3% against on a short = price up 3%"
        );
    }

    #[test]
    fn the_target_fires_when_the_thesis_plays_out() {
        let p = wmt_short(0);
        // 5% in favour of a short means the price fell 5%.
        assert_eq!(should_close(&p, 98.5, HOUR), Some(ExitReason::Target));
    }
}
