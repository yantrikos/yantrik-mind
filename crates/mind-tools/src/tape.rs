//! THE TAPE — what the traders are actually holding, recorded over time.
//!
//! Six attempts at learning from a trading broadcast established that the commentary is thin and
//! the POSITION BAR is not: it says what these people are in, right now, in a fixed place on the
//! screen, and the vision model reads it reliably. What they say is narration; what they hold is
//! evidence.
//!
//! This turns those readings into a durable series, because one observation answers nothing and a
//! few thousand answer the only question that matters here: if you had shadowed these positions
//! with a realistic delay, what would have happened? That is a counterfactual computed from their
//! real trades and real prices, and it costs nothing to find out.
//!
//! ## Two rules the parser holds
//!
//! **Ambiguity is recorded as Unknown, never guessed.** A fabricated position poisons every
//! backtest built on the series, and it does so invisibly — the arithmetic still runs. Refusing to
//! parse costs one sample; inventing one corrupts the answer.
//!
//! **The raw caption is kept beside the parse.** The parser will improve, and when it does the old
//! recordings must be re-readable rather than thrown away. Provenance beats cleverness: keep the
//! evidence, not just the conclusion.

use serde::{Deserialize, Serialize};

/// Which way a trader is leaning, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Long,
    Short,
    /// Explicitly holding nothing — "no positions" on the bar. This is real information, not an
    /// absence: knowing they are flat is what makes an entry visible when it appears.
    Flat,
    /// The bar was present but could not be read with confidence.
    Unknown,
}

/// One trader's state at one instant, with the raw text that produced it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraderState {
    pub trader: String,
    pub side: Side,
    /// The symbol held, when one is legible.
    pub symbol: Option<String>,
    /// The raw caption line this came from — kept so a better parser can revisit it later.
    pub evidence: String,
}

/// One sample of the whole bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeSample {
    pub at_ms: i64,
    pub source: String,
    pub states: Vec<TraderState>,
}

/// Names that are not tickers, however much they look like one in caps.
const NOT_TICKERS: &[&str] = &[
    "LONG", "SHORT", "FLAT", "NO", "POSITIONS", "POSITION", "TRADER", "LIVE", "THE", "AND", "USD",
    "PNL", "P", "L", "BUY", "SELL", "OPEN", "CLOSE", "HIGH", "LOW", "VOL", "AVG", "QTY", "TV",
];

/// A plausible ticker: 1–5 capitals, not a word the bar uses for something else.
fn looks_like_ticker(tok: &str) -> bool {
    let t = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    (1..=5).contains(&t.len())
        && t.chars().all(|c| c.is_ascii_uppercase())
        && !NOT_TICKERS.contains(&t)
}

/// Parse one trader's segment of the bar.
///
/// The critical subtlety, learned from real captions: the bar ALWAYS shows the words LONG and
/// SHORT, because they are button labels rather than state. Seeing "LONG" proves nothing. The
/// evidence of an actual position is a symbol; the evidence of no position is the phrase "no
/// positions" sitting where a symbol would be.
pub fn parse_trader_segment(trader: &str, segment: &str) -> TraderState {
    let lower = segment.to_lowercase();
    let evidence = segment.trim().chars().take(160).collect::<String>();
    if lower.contains("no position") || lower.contains("flat") {
        return TraderState { trader: trader.to_string(), side: Side::Flat, symbol: None, evidence };
    }
    // The trader's OWN NAME is a short uppercase token and parses as a ticker unless excluded —
    // "CHEIF LONG OSHR" otherwise reads as a position in CHEIF. The segment begins with the name
    // by construction, so it is always the first candidate and always the wrong one.
    let own = trader.trim().to_uppercase();
    let symbol = segment
        .split_whitespace()
        .filter(|t| {
            let c = t.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_uppercase();
            c != own && !own.starts_with(&c) && !c.starts_with(&own)
        })
        .find(|t| looks_like_ticker(t))
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_alphanumeric()).to_uppercase());
    // A side word only counts as state once a symbol establishes that a position exists.
    let side = match symbol {
        None => Side::Unknown,
        Some(_) => {
            let has_long = lower.contains("long");
            let has_short = lower.contains("short");
            match (has_long, has_short) {
                (true, false) => Side::Long,
                (false, true) => Side::Short,
                // Both words present is the button row, not a direction — refuse to guess.
                _ => Side::Unknown,
            }
        }
    };
    TraderState { trader: trader.to_string(), side, symbol, evidence }
}

/// Split a vision caption of the position bar into per-trader segments and parse each.
///
/// `traders` are the names to look for; supplying them explicitly avoids inventing a trader out of
/// an OCR artefact, and the roster changes rarely enough to be configuration.
pub fn parse_bar(caption: &str, traders: &[String]) -> Vec<TraderState> {
    let mut out = Vec::new();
    // Locate each trader name, then read up to the next trader name.
    let upper = caption.to_uppercase();
    let mut marks: Vec<(usize, &String)> = Vec::new();
    for t in traders {
        let needle = t.to_uppercase();
        let mut from = 0usize;
        while let Some(rel) = upper[from..].find(&needle) {
            marks.push((from + rel, t));
            from = from + rel + needle.len();
            if marks.len() > 32 {
                break;
            }
        }
    }
    marks.sort_by_key(|(i, _)| *i);
    for (n, (start, trader)) in marks.iter().enumerate() {
        // Only the FIRST mention of each trader is treated as their segment start.
        if marks[..n].iter().any(|(_, t)| t == trader) {
            continue;
        }
        let end = marks[n + 1..]
            .iter()
            .find(|(_, t)| t != trader)
            .map(|(i, _)| *i)
            .unwrap_or(caption.len());
        let seg = caption.get(*start..end).unwrap_or("");
        out.push(parse_trader_segment(trader, seg));
    }
    out
}

/// Find the trader names in a bar caption, rather than being told them.
///
/// The roster is not configuration: the morning shift and the midday shift are different people,
/// so a hardcoded list means the recorder silently stops working whenever the show changes hands —
/// which is exactly what happened the first time this ran live, with the list holding yesterday's
/// names while the screen showed today's.
///
/// The bar's structure is stable even though its cast is not: a name, then LONG/SHORT/positions
/// close behind it. That shape is what gets matched.
pub fn discover_traders(caption: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let bytes: Vec<char> = caption.chars().collect();
    let lower = caption.to_lowercase();
    let mut i = 0usize;
    while i < bytes.len() {
        if !bytes[i].is_ascii_alphabetic() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let word: String = bytes[start..i].iter().collect();
        // Names are 2–12 letters and written in caps or Titlecase on this bar.
        let plausible = (2..=12).contains(&word.len())
            && (word.chars().all(|c| c.is_ascii_uppercase())
                || (word.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false)
                    && word.chars().skip(1).all(|c| c.is_ascii_lowercase())));
        if !plausible || NOT_TICKERS.contains(&word.to_uppercase().as_str()) || NOT_NAMES.contains(&word.to_uppercase().as_str()) {
            continue;
        }
        // A trader name has position language close behind it.
        let win_end = (i + 48).min(lower.len());
        let after = lower.get(i..win_end).unwrap_or("");
        if after.contains("long") || after.contains("short") || after.contains("position") {
            let up = word.to_uppercase();
            if !found.contains(&up) {
                found.push(up);
            }
        }
    }
    found
}

/// Words that appear beside position language but never name a trader.
const NOT_NAMES: &[&str] = &[
    "LONG", "SHORT", "FLAT", "NO", "POSITIONS", "POSITION", "TRADER", "TRADERS", "BAR", "BOTTOM",
    "SCREEN", "HERE", "THE", "AND", "FOR", "EACH", "BASED", "INFORMATION", "LIVE", "SHOW", "IS",
    "ON", "AT", "OF", "IN", "TV", "BUY", "SELL", "NONE", "BOTH", "WITH", "HAS", "ARE",
];

/// Parse the bar, discovering the roster when one is not supplied.
pub fn parse_bar_auto(caption: &str, hint: &[String]) -> Vec<TraderState> {
    let mut roster: Vec<String> = hint.to_vec();
    for t in discover_traders(caption) {
        if !roster.iter().any(|r| r.eq_ignore_ascii_case(&t)) {
            roster.push(t);
        }
    }
    parse_bar(caption, &roster)
}

/// Append a sample to the tape ledger (JSONL). Best-effort: losing a sample must never break the
/// watch that produced it.
pub fn append_sample(path: &std::path::Path, sample: &TapeSample) -> std::io::Result<()> {
    use std::io::Write as _;
    let line = serde_json::to_string(sample).map_err(std::io::Error::other)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Read the tape back for analysis.
pub fn read_tape(path: &std::path::Path) -> Vec<TapeSample> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines().filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// The transitions that matter: flat → position (an entry) and position → flat (an exit). These
/// are the only moments a shadow could act on, so the counterfactual is built from them rather
/// than from every sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub at_ms: i64,
    pub trader: String,
    pub kind: String,
    pub symbol: Option<String>,
    pub side: Side,
}

/// Walk the tape and emit entries/exits per trader.
pub fn transitions(tape: &[TapeSample]) -> Vec<Transition> {
    use std::collections::HashMap;
    let mut last: HashMap<String, (Side, Option<String>)> = HashMap::new();
    let mut out = Vec::new();
    for s in tape {
        for st in &s.states {
            if st.side == Side::Unknown {
                continue; // never build a transition out of an unreadable sample
            }
            let prev = last.get(&st.trader).cloned();
            let now = (st.side, st.symbol.clone());
            match (&prev, &now) {
                (Some((Side::Flat, _)), (Side::Long, _)) | (Some((Side::Flat, _)), (Side::Short, _)) => {
                    out.push(Transition { at_ms: s.at_ms, trader: st.trader.clone(), kind: "entry".into(), symbol: st.symbol.clone(), side: st.side });
                }
                (Some((Side::Long, _)), (Side::Flat, _)) | (Some((Side::Short, _)), (Side::Flat, _)) => {
                    out.push(Transition { at_ms: s.at_ms, trader: st.trader.clone(), kind: "exit".into(), symbol: prev.as_ref().and_then(|p| p.1.clone()), side: Side::Flat });
                }
                _ => {}
            }
            last.insert(st.trader.clone(), now);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster() -> Vec<String> {
        vec!["CHERIF".to_string(), "OBI".to_string()]
    }

    /// The REAL caption from 2026-08-17 — both traders flat. Note the bar shows LONG and SHORT for
    /// each of them as button labels; reading those as state would invent two positions.
    #[test]
    fn the_real_flat_bar_parses_as_flat_not_as_long() {
        let caption = "CHERIF LONG SHORT no positions no positions OBI LONG SHORT no positions no positions";
        let states = parse_bar(caption, &roster());
        assert_eq!(states.len(), 2, "{states:?}");
        assert!(states.iter().all(|s| s.side == Side::Flat), "button labels are not positions: {states:?}");
        assert!(states.iter().all(|s| s.symbol.is_none()));
    }

    /// The REAL caption from 2026-08-18 — a live position.
    #[test]
    fn a_real_held_position_parses_with_its_symbol() {
        let caption = "CHEIF LONG OSHR 0.74";
        let states = parse_bar(caption, &["CHEIF".to_string()]);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].side, Side::Long);
        assert_eq!(states[0].symbol.as_deref(), Some("OSHR"));
        // The raw text is kept so a better parser can revisit this sample later.
        assert!(states[0].evidence.contains("OSHR"));
    }

    #[test]
    fn an_unreadable_segment_is_unknown_rather_than_a_guess() {
        // A fabricated position poisons every backtest built on the series, invisibly.
        let s = parse_trader_segment("OBI", "OBI ▓▓ smudge ▓▓");
        assert_eq!(s.side, Side::Unknown);
        assert_eq!(s.symbol, None);
        // Both direction words with a symbol is the button row — still refuses to pick.
        let both = parse_trader_segment("OBI", "OBI LONG SHORT AAPL");
        assert_eq!(both.side, Side::Unknown, "cannot tell direction from a button row");
    }

    #[test]
    fn bar_furniture_is_never_mistaken_for_a_ticker() {
        for w in ["LONG", "SHORT", "NO", "POSITIONS", "PNL", "USD"] {
            assert!(!looks_like_ticker(w), "{w} is not a ticker");
        }
        assert!(looks_like_ticker("OSHR") && looks_like_ticker("MU") && looks_like_ticker("SPY"));
        assert!(!looks_like_ticker("toolong6"), "too long");
    }

    #[test]
    fn transitions_find_the_entry_and_the_exit_and_ignore_unreadable_samples() {
        let mk = |at: i64, side: Side, sym: Option<&str>| TapeSample {
            at_ms: at,
            source: "t".into(),
            states: vec![TraderState { trader: "CHERIF".into(), side, symbol: sym.map(|s| s.into()), evidence: String::new() }],
        };
        let tape = vec![
            mk(1000, Side::Flat, None),
            mk(2000, Side::Unknown, None), // a bad frame must not create a transition
            mk(3000, Side::Long, Some("OSHR")),
            mk(4000, Side::Long, Some("OSHR")),
            mk(5000, Side::Flat, None),
        ];
        let t = transitions(&tape);
        assert_eq!(t.len(), 2, "{t:?}");
        assert_eq!(t[0].kind, "entry");
        assert_eq!(t[0].at_ms, 3000);
        assert_eq!(t[0].symbol.as_deref(), Some("OSHR"));
        assert_eq!(t[1].kind, "exit");
        assert_eq!(t[1].at_ms, 5000);
        assert_eq!(t[1].symbol.as_deref(), Some("OSHR"), "an exit remembers what was held");
    }

    /// The REAL caption from the first live run — today's shift was NEAL and JOE while the
    /// configured roster still held yesterday's CHERIF and OBI, so the recorder read nothing.
    /// The roster is not configuration; the shift changes and the parser must keep up.
    #[test]
    fn traders_are_discovered_from_the_bar_not_configured() {
        let caption = "Based on the traders' position bar at the bottom:
                       *   **NEAL**: LONG no positions; SHORT no positions
                       *   **JOE**: LONG no positions; SHORT no positions";
        let found = discover_traders(caption);
        assert!(found.contains(&"NEAL".to_string()), "{found:?}");
        assert!(found.contains(&"JOE".to_string()), "{found:?}");
        // Bar furniture and prose must never be mistaken for a person.
        for junk in ["LONG", "SHORT", "POSITIONS", "TRADERS", "BOTTOM", "BASED"] {
            assert!(!found.contains(&junk.to_string()), "{junk} is not a trader: {found:?}");
        }
        // And with an empty hint the bar still parses to two flat traders.
        let states = parse_bar_auto(caption, &[]);
        assert_eq!(states.len(), 2, "{states:?}");
        assert!(states.iter().all(|s| s.side == Side::Flat), "{states:?}");
    }

    /// Yesterday's caption must still work — discovery adds to the hint, never replaces it.
    #[test]
    fn discovery_also_handles_the_earlier_shift_and_a_held_position() {
        let held = parse_bar_auto("CHEIF LONG OSHR 0.74", &[]);
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!(held[0].trader, "CHEIF");
        assert_eq!(held[0].side, Side::Long);
        assert_eq!(held[0].symbol.as_deref(), Some("OSHR"));
    }

    #[test]
    fn the_tape_round_trips_through_the_ledger() {
        let mut p = std::env::temp_dir();
        p.push(format!("ym_tape_test_{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let s = TapeSample {
            at_ms: 1,
            source: "stream".into(),
            states: vec![TraderState { trader: "CHERIF".into(), side: Side::Long, symbol: Some("OSHR".into()), evidence: "raw".into() }],
        };
        append_sample(&p, &s).unwrap();
        append_sample(&p, &s).unwrap();
        let back = read_tape(&p);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].states[0].symbol.as_deref(), Some("OSHR"));
        let _ = std::fs::remove_file(&p);
    }
}
