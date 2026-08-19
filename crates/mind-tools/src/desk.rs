//! DESK — a trading lens for the generic surfer.
//!
//! `surf` knows how to keep a roster of live sources, glance at each, and notice when one changed.
//! It deliberately does not know what a trader is. This module supplies the trading-specific half:
//! the question to ask about a broadcast frame, and what counts as a change on a trading desk.
//!
//! Anything else the mind ends up watching — a news channel, a status page, a match — brings its own
//! lens and reuses everything around it. That split is the difference between a surfer and a
//! trading-desk-watcher that happens to accept URLs.
//!
//! ## Reading the bar, which is harder than it looks
//!
//! TraderTV's lower-third gives each trader TWO rows, aligned to a LONG and a SHORT label. Each row
//! holds either "no positions" or a ticker with a price:
//!
//! ```text
//!   CHERIF   LONG  no positions          JOE   LONG  TQQQ 71.15
//!            SHORT NVAX 8.54                   SHORT no positions
//! ```
//!
//! Cherif is SHORT NVAX; Joe is LONG TQQQ. An earlier reducer skipped any entry containing "no
//! positions" and therefore threw away both — it would have reported an active desk as flat, which
//! is the exact event this exists to catch. The phrase appears NEXT TO a held position, not instead
//! of one, and only a screenshot made that obvious.

use crate::surf::Lens;

/// The trading lens: read the position bar, compare what is held.
pub const DESK_LENS: Lens = Lens {
    name: "desk",
    prompt: "The banner across the bottom lists traders. Each trader has TWO rows: the upper row \
             belongs to LONG and the lower row to SHORT. A row shows either 'no positions' or a \
             ticker symbol with a price — so a trader can be flat on one side while holding on the \
             other.\n\
             Output ONE LINE PER TRADER, in exactly this form, nothing else, no prose:\n\
             NAME | LONG=<ticker or NONE> | SHORT=<ticker or NONE>\n\
             Copy the trader names and ticker symbols exactly as printed. If no trader banner is \
             visible, reply with the single word NONE.",
    reduce: exposure,
};

/// What the desk is HOLDING — the only part of a trading screen that is a signal.
///
/// Prices tick, charts redraw, and the ticker tape scrolls to a fresh set of symbols every second;
/// none of that is a state. What survives is the set of open positions, so two readings of an
/// unchanged desk reduce to the same value and a position opening or closing is the only thing that
/// can move it.
pub fn exposure(reading: &str) -> Option<std::collections::BTreeSet<String>> {
    let mut held = std::collections::BTreeSet::new();
    let mut saw_a_trader = false;
    for line in reading.lines() {
        let up = line.trim().to_uppercase();
        if !up.contains("LONG=") || !up.contains("SHORT=") {
            continue;
        }
        saw_a_trader = true;
        let name = up.split('|').next().unwrap_or("").trim().to_string();
        for side in ["LONG", "SHORT"] {
            let Some(rest) = up.split(&format!("{side}=")).nth(1) else { continue };
            let val = rest.split('|').next().unwrap_or("").trim();
            let empty = val.is_empty()
                || val.starts_with("NONE")
                || val.starts_with("NO POSITION")
                || val.starts_with("FLAT")
                || val.starts_with('-');
            if empty {
                continue;
            }
            // The symbol only — the price beside it moves constantly and is not the position.
            let sym: String = val
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '.')
                .find(|t| !t.is_empty() && t.chars().any(|c| c.is_ascii_alphabetic()))
                .unwrap_or("?")
                .to_string();
            held.insert(format!("{name}:{side}:{sym}"));
        }
    }
    if saw_a_trader {
        Some(held)
    } else {
        None // no trader rows at all — a failed or malformed look, never a transition.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surf::changed_by;

    fn changed(a: &str, b: &str) -> bool {
        changed_by(&DESK_LENS, a, b)
    }

    #[test]
    fn a_held_position_sits_beside_the_words_no_positions() {
        // From two screenshots of the real broadcast: Cherif SHORT NVAX while his LONG row reads
        // "no positions", Joe LONG TQQQ while his SHORT row reads "no positions". An earlier reducer
        // skipped any entry containing that phrase and discarded BOTH — reporting an active desk as
        // flat, which is precisely the event this exists to catch.
        let held = "CHERIF | LONG=NONE | SHORT=NVAX\nJOE | LONG=TQQQ | SHORT=NONE";
        let e = exposure(held).unwrap();
        assert!(e.contains("CHERIF:SHORT:NVAX"), "{e:?}");
        assert!(e.contains("JOE:LONG:TQQQ"), "{e:?}");
        assert_eq!(e.len(), 2, "exactly two open positions: {e:?}");

        let flat = "CHERIF | LONG=NONE | SHORT=NONE\nJOE | LONG=NONE | SHORT=NONE";
        assert_eq!(exposure(flat), Some(Default::default()));
        assert!(changed(held, flat), "closing both positions is a transition");
        assert!(changed(flat, held), "and opening them is the signal");
    }

    #[test]
    fn a_price_beside_the_ticker_is_not_part_of_the_position() {
        let a = "CHERIF | LONG=NONE | SHORT=NVAX 8.54\nJOE | LONG=TQQQ 71.15 | SHORT=NONE";
        let b = "CHERIF | LONG=NONE | SHORT=NVAX 8.61\nJOE | LONG=TQQQ 71.20 | SHORT=NONE";
        assert!(!changed(a, b), "the same positions at new prices are not a transition");
    }

    #[test]
    fn an_unchanged_desk_is_quiet() {
        let a = "CHERIF | LONG=NONE | SHORT=NONE\nJOE | LONG=NONE | SHORT=NONE";
        assert!(!changed(a, a));
    }

    #[test]
    fn a_reading_with_no_trader_rows_is_a_failed_look_not_a_transition() {
        // Prose from a model that ignored the format, and the readings the OLD prompt produced.
        // Both are unusable, and unusable must never compare unequal and invent a signal.
        assert_eq!(exposure("I cannot make out this image."), None);
        assert_eq!(exposure("POSITIONS: CHERIF=FLAT, JOE=FLAT\nTICKERS: TSM, AAPL"), None);
        assert!(!changed("CHERIF | LONG=NONE | SHORT=NONE", "I cannot make out this image."));
    }
}
