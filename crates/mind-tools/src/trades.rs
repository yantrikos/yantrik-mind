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
        if self.is_short() { -raw } else { raw }
    }

    /// Did the prediction come true? A trade is graded on DIRECTION, not on whether it was closed
    /// at a profit after costs — the claim was "this should be profitable", and a position that
    /// moved the predicted way was a correct read even if the exit was mistimed.
    pub fn was_right(&self, price: f64) -> bool {
        self.favour_pct(price) > 0.0
    }
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
    let i = book.iter().position(|x| x.symbol.eq_ignore_ascii_case(symbol))?;
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

    #[test]
    fn the_real_trade_is_graded_on_direction() {
        // Entered at 103.75 short; the tape went to 105.44. The thesis said further downside, so
        // this is simply wrong, and the ledger should say so rather than leave it pending forever.
        let t = wmt(0);
        assert!(!t.was_right(105.44), "price rose against a short — the view was wrong");
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
        let view = OpenTrade { qty: -1.0, staked: false, ..wmt(0) };
        assert!(!view.staked);
        assert!(view.is_short());
        assert!(!view.was_right(105.44), "graded on the tape exactly as a real short would be");
        assert!(view.was_right(99.0));
    }

    #[test]
    fn an_old_record_without_the_field_is_a_real_position() {
        // Records written before views existed were all staked; defaulting them to "view" would
        // quietly drop real positions out of position management.
        let old = r#"[{"symbol":"WMT","qty":-2.0,"entry":103.75,"opened_at_ms":1,"judgment_ref":"WMT","thesis":"t"}]"#;
        let book = parse_book(old);
        assert_eq!(book.len(), 1);
        assert!(book[0].staked, "a pre-views record is a real position, not a view");
    }

    #[test]
    fn closing_removes_the_record_and_hands_back_its_prediction() {
        let mut book = vec![wmt(1)];
        let closed = take(&mut book, "wmt").expect("case-insensitive");
        assert_eq!(closed.judgment_ref, "WMT");
        assert!(book.is_empty());
    }
}
