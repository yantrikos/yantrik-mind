//! TRADES — the record that links a position to the prediction that opened it.
//!
//! Two failures shared one cause, and both were live for five days.
//!
//! The WMT short was entered on a SAME-DAY thesis and was still open on day five, because the exit
//! rules could not fire: `follow` reads positions from the broker, and a broker position carries no
//! entry time. Every position therefore looked zero seconds old, so the horizon — the rule that
//! actually protects a same-day view — never came due.
//!
//! And six `hunt` predictions sat "awaiting their deadline" long after their 24-hour deadline
//! passed, because nothing grades a trading prediction. The ledger recorded what the mind believed
//! and never found out whether it was right, which is the whole loop failing quietly: logged,
//! deadline passed, no verdict, no learning.
//!
//! So a trade gets a record of its own at the moment it is opened: when, at what price, and — the
//! part that closes the loop — WHICH prediction it was. Grading then needs no text parsing of a
//! thesis sentence, because the link is a stored id rather than something to be inferred later.

use serde::{Deserialize, Serialize};

/// One position the mind opened, with the prediction it was betting on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenTrade {
    pub symbol: String,
    /// Negative for a short.
    pub qty: f64,
    pub entry: f64,
    pub opened_at_ms: i64,
    /// The judgment-ledger ref this position is the wager on. Grading resolves through this.
    pub judgment_ref: String,
    /// The thesis, kept for the exit report — a close that cannot say what it was betting on is
    /// a number without a reason.
    pub thesis: String,
    /// Was capital committed, or is this a VIEW recorded only to be graded?
    ///
    /// The learning rate is the binding constraint on ever knowing whether this works. To tell a
    /// 55% edge from luck takes ~780 graded calls: at one trade a day that is two years, at six a
    /// day it is four months. `hunt` sees six to fourteen tradeable names a session and takes at
    /// most one, so grading only the taken ones throws away most of the evidence available.
    ///
    /// A view costs no capital and carries no risk, and grades identically against the tape. Kept
    /// in the same book because they must be graded by the same code — a separate path would drift,
    /// and then the cheap evidence would be the untrustworthy kind.
    #[serde(default = "yes")]
    pub staked: bool,
}

/// serde default for records written before views existed: those were all real positions.
fn yes() -> bool {
    true
}

impl OpenTrade {
    pub fn is_short(&self) -> bool {
        self.qty < 0.0
    }

    /// Percent move IN FAVOUR. For a short a falling price is a gain; getting this backwards grades
    /// every winner as a loss.
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

    /// Did the prediction come true? A trade is graded on DIRECTION, not on whether it was closed
    /// at a profit after costs — the claim was "this should be profitable", and a position that
    /// moved the predicted way was a correct read even if the exit was mistimed.
    pub fn was_right(&self, price: f64) -> bool {
        self.favour_pct(price) > 0.0
    }
}

/// One broker-reconciled close. Quotes never enter this record: entry and exit are execution
/// prices, quantity is signed (negative for shorts), and costs reduce the result explicitly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClosedTrade {
    pub desk: String,
    pub symbol: String,
    pub qty: f64,
    pub entry: f64,
    pub exit: f64,
    #[serde(default)]
    pub fees: f64,
    pub opened_at_ms: i64,
    pub closed_at_ms: i64,
    pub exit_order_id: String,
}

impl ClosedTrade {
    pub fn net_pnl(&self) -> Option<f64> {
        let valid = self.qty.is_finite()
            && self.qty != 0.0
            && self.entry.is_finite()
            && self.entry > 0.0
            && self.exit.is_finite()
            && self.exit > 0.0
            && self.fees.is_finite()
            && self.fees >= 0.0
            && self.closed_at_ms >= self.opened_at_ms
            && !self.exit_order_id.trim().is_empty();
        valid.then_some((self.exit - self.entry) * self.qty - self.fees)
    }

    pub fn return_pct(&self) -> Option<f64> {
        let capital = self.entry * self.qty.abs();
        self.net_pnl().map(|pnl| pnl / capital * 100.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PerformanceSummary {
    pub trades: usize,
    pub wins: usize,
    pub net_pnl: f64,
    pub gross_profit: f64,
    /// Positive magnitude of losing P&L.
    pub gross_loss: f64,
    pub expectancy: f64,
    pub profit_factor: Option<f64>,
    pub max_realized_drawdown: f64,
}

/// Summarize only complete, broker-attributed executions, ordered by their actual close time.
pub fn summarize_closed(book: &[ClosedTrade]) -> PerformanceSummary {
    let mut outcomes: Vec<(i64, f64)> = book
        .iter()
        .filter_map(|trade| trade.net_pnl().map(|pnl| (trade.closed_at_ms, pnl)))
        .collect();
    outcomes.sort_by_key(|(closed_at_ms, _)| *closed_at_ms);
    if outcomes.is_empty() {
        return PerformanceSummary::default();
    }

    let mut summary = PerformanceSummary {
        trades: outcomes.len(),
        ..Default::default()
    };
    let mut equity_curve = 0.0_f64;
    let mut equity_peak = 0.0_f64;
    for (_, pnl) in outcomes {
        summary.net_pnl += pnl;
        if pnl > 0.0 {
            summary.wins += 1;
            summary.gross_profit += pnl;
        } else if pnl < 0.0 {
            summary.gross_loss += -pnl;
        }
        equity_curve += pnl;
        equity_peak = equity_peak.max(equity_curve);
        summary.max_realized_drawdown = summary
            .max_realized_drawdown
            .max(equity_peak - equity_curve);
    }
    summary.expectancy = summary.net_pnl / summary.trades as f64;
    summary.profit_factor =
        (summary.gross_loss > 0.0).then_some(summary.gross_profit / summary.gross_loss);
    summary
}

/// A 95% Wilson score interval for the observed win rate. Unlike the naive `wins / trades`
/// number, this stays appropriately wide for small samples and never escapes 0–100%.
pub fn win_rate_interval_95(wins: usize, trades: usize) -> Option<(f64, f64)> {
    if trades == 0 || wins > trades {
        return None;
    }
    let n = trades as f64;
    let observed = wins as f64 / n;
    let z = 1.959_963_984_540_054_f64;
    let z_squared = z * z;
    let denominator = 1.0 + z_squared / n;
    let center = (observed + z_squared / (2.0 * n)) / denominator;
    let spread =
        z * ((observed * (1.0 - observed) + z_squared / (4.0 * n)) / n).sqrt() / denominator;
    Some(((center - spread).max(0.0), (center + spread).min(1.0)))
}

/// Store one broker close exactly once. Broker status polling is deliberately retryable, so the
/// execution id—not the symbol—is the identity of a closed trade. Replacing an existing row also
/// lets a later broker response fill in final fees without counting the same close twice.
pub fn upsert_closed(book: &mut Vec<ClosedTrade>, trade: ClosedTrade) {
    book.retain(|row| row.exit_order_id != trade.exit_order_id);
    book.push(trade);
}

/// Parse the stored ledger. A corrupt or absent record yields an empty book rather than an error:
/// losing the record must never stop the mind from reading its positions from the broker, which
/// remains the authority on what is actually held.
pub fn parse_book(raw: &str) -> Vec<OpenTrade> {
    serde_json::from_str(raw).unwrap_or_default()
}

pub fn render_book(book: &[OpenTrade]) -> String {
    serde_json::to_string(book).unwrap_or_else(|_| "[]".to_string())
}

/// Add a trade, replacing any existing record for the same symbol.
///
/// Replacing rather than appending: the broker nets positions per symbol, so two records for one
/// symbol would grade the same holding twice and disagree about when it opened.
pub fn upsert(book: &mut Vec<OpenTrade>, t: OpenTrade) {
    book.retain(|x| !x.symbol.eq_ignore_ascii_case(&t.symbol));
    book.push(t);
}

pub fn take(book: &mut Vec<OpenTrade>, symbol: &str) -> Option<OpenTrade> {
    let i = book
        .iter()
        .position(|x| x.symbol.eq_ignore_ascii_case(symbol))?;
    Some(book.remove(i))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wmt(opened: i64) -> OpenTrade {
        OpenTrade {
            symbol: "WMT".into(),
            qty: -2.0,
            entry: 103.75,
            opened_at_ms: opened,
            judgment_ref: "WMT".into(),
            thesis: "weakest US sales growth since 2020 — further downside".into(),
            staked: true,
        }
    }

    fn retried_close(exit: f64, fees: f64) -> ClosedTrade {
        ClosedTrade {
            desk: "day".into(),
            symbol: "WMT".into(),
            qty: 2.0,
            entry: 100.0,
            exit,
            fees,
            opened_at_ms: 1,
            closed_at_ms: 2,
            exit_order_id: "close-1".into(),
        }
    }

    #[test]
    fn retrying_a_broker_close_does_not_double_count_it() {
        let mut book = vec![];
        upsert_closed(&mut book, retried_close(104.0, 0.0));
        upsert_closed(&mut book, retried_close(104.0, 1.0));

        assert_eq!(book.len(), 1);
        assert_eq!(book[0].fees, 1.0);
        assert_eq!(summarize_closed(&book).net_pnl, 7.0);
    }

    #[test]
    fn win_rate_uncertainty_does_not_call_one_win_an_edge() {
        let (one_win_low, one_win_high) = win_rate_interval_95(1, 1).unwrap();
        assert!((one_win_low - 0.2065).abs() < 0.0001);
        assert_eq!(one_win_high, 1.0);

        let (balanced_low, balanced_high) = win_rate_interval_95(50, 100).unwrap();
        assert!((balanced_low - 0.4038).abs() < 0.0001);
        assert!((balanced_high - 0.5962).abs() < 0.0001);
        assert_eq!(win_rate_interval_95(2, 1), None);
        assert_eq!(win_rate_interval_95(0, 0), None);
    }

    #[test]
    fn the_real_trade_is_graded_on_direction() {
        // Entered at 103.75 short; the tape went to 105.44. The thesis said further downside, so
        // this is simply wrong, and the ledger should say so rather than leave it pending forever.
        let t = wmt(0);
        assert!(
            !t.was_right(105.44),
            "price rose against a short — the view was wrong"
        );
        assert!(t.favour_pct(105.44) < 0.0);
        assert!(t.was_right(99.0), "price fell — the view was right");
    }

    #[test]
    fn an_entry_time_is_what_makes_a_horizon_possible() {
        // The whole reason this record exists: a broker position has no opened_at, so every
        // position read back looked brand new and a same-day thesis ran for five days.
        let t = wmt(1_000);
        assert_eq!(t.opened_at_ms, 1_000);
    }

    #[test]
    fn one_symbol_keeps_one_record() {
        // The broker nets per symbol; two records would grade one holding twice and disagree about
        // when it opened.
        let mut book = vec![wmt(1)];
        upsert(&mut book, wmt(2));
        assert_eq!(book.len(), 1);
        assert_eq!(book[0].opened_at_ms, 2);
    }

    #[test]
    fn a_lost_record_is_an_empty_book_not_an_error() {
        // The broker stays the authority on what is held; this record only adds when and why.
        assert!(parse_book("").is_empty());
        assert!(parse_book("{corrupt").is_empty());
        let book = vec![wmt(5)];
        assert_eq!(parse_book(&render_book(&book)), book);
    }

    #[test]
    fn a_view_grades_exactly_like_a_trade_but_risks_nothing() {
        // The whole point of views: six a day instead of one, at zero capital, graded by the same
        // code. Direction still lives in the sign of qty, so nothing about scoring changes.
        let view = OpenTrade {
            qty: -1.0,
            staked: false,
            ..wmt(0)
        };
        assert!(!view.staked);
        assert!(view.is_short());
        assert!(
            !view.was_right(105.44),
            "graded on the tape exactly as a real short would be"
        );
        assert!(view.was_right(99.0));
    }

    #[test]
    fn an_old_record_without_the_field_is_a_real_position() {
        // Records written before views existed were all staked; defaulting them to "view" would
        // quietly drop real positions out of position management.
        let old = r#"[{"symbol":"WMT","qty":-2.0,"entry":103.75,"opened_at_ms":1,"judgment_ref":"WMT","thesis":"t"}]"#;
        let book = parse_book(old);
        assert_eq!(book.len(), 1);
        assert!(
            book[0].staked,
            "a pre-views record is a real position, not a view"
        );
    }

    #[test]
    fn closing_removes_the_record_and_hands_back_its_prediction() {
        let mut book = vec![wmt(1)];
        let closed = take(&mut book, "wmt").expect("case-insensitive");
        assert_eq!(closed.judgment_ref, "WMT");
        assert!(book.is_empty());
    }

    fn closed(qty: f64, entry: f64, exit: f64, fees: f64, at: i64) -> ClosedTrade {
        ClosedTrade {
            desk: "test".into(),
            symbol: "XYZ".into(),
            qty,
            entry,
            exit,
            fees,
            opened_at_ms: 0,
            closed_at_ms: at,
            exit_order_id: format!("order-{at}"),
        }
    }

    #[test]
    fn realized_pnl_handles_longs_shorts_and_costs() {
        assert_eq!(closed(10.0, 100.0, 105.0, 2.0, 1).net_pnl(), Some(48.0));
        assert_eq!(closed(-5.0, 200.0, 190.0, 1.0, 1).net_pnl(), Some(49.0));
        assert_eq!(closed(2.0, 100.0, 90.0, 0.0, 1).net_pnl(), Some(-20.0));
    }

    #[test]
    fn performance_uses_execution_order_and_reports_drawdown() {
        let report = summarize_closed(&[
            closed(2.0, 100.0, 90.0, 0.0, 3),
            closed(-5.0, 200.0, 190.0, 0.0, 2),
            closed(10.0, 100.0, 105.0, 0.0, 1),
        ]);
        assert_eq!(report.trades, 3);
        assert_eq!(report.wins, 2);
        assert_eq!(report.net_pnl, 80.0);
        assert_eq!(report.gross_profit, 100.0);
        assert_eq!(report.gross_loss, 20.0);
        assert_eq!(report.profit_factor, Some(5.0));
        assert_eq!(report.max_realized_drawdown, 20.0);
        assert!((report.expectancy - 80.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn incomplete_or_impossible_execution_rows_are_not_scored() {
        let mut invalid = closed(1.0, 100.0, 110.0, 0.0, 1);
        invalid.exit_order_id.clear();
        assert_eq!(invalid.net_pnl(), None);
        assert_eq!(summarize_closed(&[invalid]), PerformanceSummary::default());
    }
}
