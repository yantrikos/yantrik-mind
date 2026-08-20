//! ASKED — recognising a price question, and what it is about.
//!
//! The fast path cannot reach a tool by construction; that is what makes it fast. It has already
//! produced one confident wrong answer that way (17 × 23 = "one hundred and one", fixed by doing
//! arithmetic in code), and in a live voice session it produced something worse:
//!
//! ```text
//!   you  : let's see how the Indian market is doing
//!   mind : I don't have live market data right now...
//!   you  : you can use yfinance, right?
//!   mind : Yeah, I can use yfinance to pull that live data. Want me to grab the Nifty and Sensex?
//!   you  : sure
//!   mind : Pulling the Nifty 50 and Sensex now. Give me a second to grab those quotes.
//!   ...nothing.
//! ```
//!
//! Deny, get corrected, agree, promise, and never deliver. The promise is the worst part: a refusal
//! is at least honest and lets the person go elsewhere, while "give me a second" makes them wait for
//! something that was never going to arrive.
//!
//! ## The fix is grounding, not tool-calling
//!
//! The same file already solved this for identity: the people layer is fetched and put in the prompt
//! rather than left to a tool, because voice is exactly where "what is my wife's name" gets asked.
//! Prices are the same shape. Detect that the question is about a price, fetch it, and hand the
//! model the number — no extra model round trip to decide, no tool loop, and nothing to promise
//! because the answer is already in hand.

/// Words that make a question about a price rather than about a company.
const PRICE_WORDS: &[&str] = &[
    "price", "quote", "trading at", "trading", "worth", "level", "at now", "doing", "market",
    "up or down", "how is", "how's", "close", "open", "moving",
];

/// Spoken names people actually use, mapped to what a data feed calls them.
///
/// A person says "the Nifty", never "^NSEI". The mapping goes in both directions elsewhere — this
/// side turns speech into a symbol, `speech::to_spoken` turns the symbol back into words.
const SPOKEN_SYMBOLS: &[(&str, &str)] = &[
    // A whole MARKET is a thing people ask about — "how is the Indian market doing" was the exact
    // question that exposed all this — and it has a sensible answer: the index everyone means.
    ("indian market", "^NSEI"),
    ("indian markets", "^NSEI"),
    ("us market", "^GSPC"),
    ("us markets", "^GSPC"),
    ("nifty 50", "^NSEI"),
    ("nifty50", "^NSEI"),
    ("nifty", "^NSEI"),
    ("sensex", "^BSESN"),
    ("bank nifty", "^NSEBANK"),
    ("reliance", "RELIANCE.NS"),
    ("infosys", "INFY.NS"),
    ("infy", "INFY.NS"),
    ("tcs", "TCS.NS"),
    ("hdfc bank", "HDFCBANK.NS"),
    ("s&p", "^GSPC"),
    ("s and p", "^GSPC"),
    ("nasdaq", "^IXIC"),
    ("dow", "^DJI"),
    ("apple", "AAPL"),
    ("tesla", "TSLA"),
    ("nvidia", "NVDA"),
    ("moderna", "MRNA"),
    ("bitcoin", "BTC-USD"),
    ("ethereum", "ETH-USD"),
];

/// Is this a question whose answer is a number the mind can look up?
///
/// NAMING something quotable is the signal, with or without a price word: "grab the Nifty 50 and
/// Sensex" contains none of them and is unmistakably a request for quotes. A price word alone is not
/// enough, because "how's your day" and "the market for used cars" are ordinary conversation.
///
/// The asymmetry is deliberate. A false positive costs one unnecessary quote sitting in the
/// grounding, which the model can simply not mention. A false negative costs the deny-promise-fail
/// loop that produced this module. Fetch when in doubt.
pub fn is_price_question(text: &str) -> bool {
    !symbols_in(text).is_empty()
}

/// Does the phrasing suggest a price, independent of what is named? Kept for callers that want the
/// weaker signal on its own.
pub fn has_price_words(text: &str) -> bool {
    let t = text.to_lowercase();
    PRICE_WORDS.iter().any(|w| t.contains(w))
}

/// Find `needle` only where it stands as a whole word.
///
/// Plain substring matching quoted the Dow Jones for "is bitcoin up or DOWn" — "dow" sits inside
/// "down". Every short ticker name has a word it hides in, so the boundary check is not an
/// optimisation, it is the difference between answering the question and answering a different one.
fn find_word(hay: &str, needle: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(rel) = hay[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !hay.as_bytes()[start - 1].is_ascii_alphanumeric();
        let after_ok = end >= hay.len() || !hay.as_bytes()[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return Some(start);
        }
        from = start + 1;
    }
    None
}

/// The symbols a spoken sentence refers to, in the order they were said.
pub fn symbols_in(text: &str) -> Vec<String> {
    let t = text.to_lowercase();
    let mut found: Vec<(usize, String)> = Vec::new();
    for (spoken, sym) in SPOKEN_SYMBOLS {
        if let Some(pos) = find_word(&t, spoken) {
            // Longer names win at the same position: "nifty 50" must not also match "nifty".
            if !found.iter().any(|(p, _)| *p == pos) {
                found.push((pos, sym.to_string()));
            }
        }
    }
    // A bare ticker said as a word — "what's MRNA at" — when it is not in the spoken map.
    for w in text.split_whitespace() {
        let c = w.trim_matches(|ch: char| !ch.is_ascii_alphanumeric());
        // All-caps words that are not tickers. "TCS is at 2297.50 INR" yielded a quote request for
        // the Indian rupee, because INR is three capital letters like any small-cap symbol.
        const NOT_TICKERS: &[&str] = &[
            "INR", "USD", "EUR", "GBP", "JPY", "AM", "PM", "ET", "IST", "UTC", "OK", "AI", "CEO",
            "IPO", "ETF", "GDP", "CPI", "FED", "RBI", "USA", "UK", "TV", "API",
        ];
        if c.len() >= 2
            && c.len() <= 5
            && c.chars().all(|ch| ch.is_ascii_uppercase())
            && !NOT_TICKERS.contains(&c)
        {
            let sym = c.to_string();
            // "TCS" also resolves through the spoken map to TCS.NS; keeping both quotes the same
            // company twice, once with the wrong exchange.
            let already = found.iter().any(|(_, s)| *s == sym || s.starts_with(&format!("{sym}.")));
            if !already {
                found.push((usize::MAX, sym));
            }
        }
    }
    found.sort_by_key(|(p, _)| *p);
    let mut out: Vec<String> = Vec::new();
    for (_, s) in found {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out.truncate(4);
    out
}

/// Is this turn agreeing to something just offered, rather than asking something new?
///
/// The mind said "Want me to pull the Nifty 50 to compare?" and the person said "yes please" — a
/// turn containing no ticker, no price word and nothing to resolve. So the symbol lookup found
/// nothing, the grounding stayed empty, and the mind answered that it had no market data, one turn
/// after offering to fetch it.
///
/// An offer creates a referent. "Yes" points at it.
pub fn is_agreement(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    let t = t.trim_matches(|c: char| !c.is_alphanumeric() && c != ' ');
    if t.split_whitespace().count() > 4 {
        return false;
    }
    [
        "yes", "yes please", "yeah", "yep", "sure", "ok", "okay", "please", "go ahead", "do it",
        "please do", "sounds good", "go for it", "yes do", "alright",
    ]
    .iter()
    .any(|a| t == *a || t.starts_with(&format!("{a} ")))
}

/// Symbols for THIS turn, falling back to what was just being discussed.
///
/// `recent` is the conversation so far, newest last. When the current turn names nothing and is a
/// bare agreement, the referent is whatever the previous turn was about — which is exactly the
/// situation an offer creates.
pub fn symbols_with_context(text: &str, recent: &[String]) -> Vec<String> {
    let here = symbols_in(text);
    if !here.is_empty() || !is_agreement(text) {
        return here;
    }
    for line in recent.iter().rev().take(4) {
        // An agreement points at what was OFFERED, not at everything the line mentioned. The real
        // case: "TCS is at 2297.50, down 0.30 percent. Want me to pull the Nifty 50 to compare?" —
        // "yes please" means the Nifty, and answering with TCS again would be answering the part
        // that was already finished.
        let lower = line.to_lowercase();
        let offer_at = ["want me to", "shall i", "should i", "would you like", "want to see"]
            .iter()
            .filter_map(|p| lower.find(p))
            .min();
        let scope = match offer_at {
            Some(i) => &line[i..],
            None => line.as_str(),
        };
        let s = symbols_in(scope);
        if !s.is_empty() {
            return s;
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_question_that_started_all_this_is_recognised() {
        // Verbatim from the session where the mind promised and never delivered. "The Indian
        // market" names no ticker but has an obvious answer, and refusing to resolve it is what
        // sent the conversation into deny-promise-fail.
        assert!(is_price_question("let's see how the Indian market is doing"));
        assert_eq!(symbols_in("let's see how the Indian market is doing"), vec!["^NSEI"]);
        assert!(is_price_question("how is the nifty doing"));
        assert_eq!(symbols_in("how is the nifty doing"), vec!["^NSEI"]);
        assert!(is_price_question("grab the Nifty 50 and Sensex"));
        assert_eq!(symbols_in("grab the Nifty 50 and Sensex"), vec!["^NSEI", "^BSESN"]);
    }

    #[test]
    fn a_person_says_the_nifty_not_caret_nsei() {
        assert_eq!(symbols_in("what's reliance trading at"), vec!["RELIANCE.NS"]);
        assert_eq!(symbols_in("how's infosys and tcs doing"), vec!["INFY.NS", "TCS.NS"]);
        // "dow" hides inside "down": this asked about bitcoin and got the Dow Jones as well.
        assert_eq!(symbols_in("is bitcoin up or down"), vec!["BTC-USD"]);
        assert_eq!(symbols_in("is the dow up"), vec!["^DJI"], "the real Dow still resolves");
    }

    #[test]
    fn nifty_fifty_does_not_also_match_nifty() {
        // Both patterns hit the same position; the longer name must win, or one question becomes
        // two quotes for the same index.
        let s = symbols_in("what is nifty 50 at");
        assert_eq!(s, vec!["^NSEI"], "one symbol, not two: {s:?}");
    }

    #[test]
    fn a_question_about_something_unquotable_is_left_alone() {
        // A price word with nothing nameable is an ordinary conversation, and hijacking it would be
        // worse than the bug being fixed.
        assert!(!is_price_question("how is your day going"));
        assert!(!is_price_question("what's the market for used cars like"),
                "'the market' was mapped to the S&P and swallowed this — too generic to keep");
        assert!(!is_price_question("open the door"));
        // The weaker signal still exists for callers that want it.
        assert!(has_price_words("how is your day going"), "'how is' is a price WORD; naming is what decides");
    }

    #[test]
    fn yes_please_means_the_thing_that_was_just_offered() {
        // Verbatim. The mind offered to pull the Nifty, the person agreed, and the next turn had no
        // ticker in it — so nothing resolved and the mind said it had no market data, one turn after
        // offering to fetch it.
        let recent = vec![
            "user: what's TCS at".to_string(),
            "assistant: TCS is at 2297.50 INR, down 0.30 percent. Want me to pull the Nifty 50 to compare?".to_string(),
        ];
        assert!(is_agreement("yes please"));
        assert_eq!(symbols_with_context("yes please", &recent), vec!["^NSEI"]);
    }

    #[test]
    fn a_new_question_ignores_the_old_referent() {
        // Context is a fallback, never an override: naming something new must win.
        let recent = vec!["assistant: want me to pull the Nifty 50?".to_string()];
        assert_eq!(symbols_with_context("what's reliance at", &recent), vec!["RELIANCE.NS"]);
        // And a non-agreement with no symbol resolves to nothing rather than the last thing seen.
        assert_eq!(symbols_with_context("what did you mean by that", &recent), Vec::<String>::new());
    }

    #[test]
    fn agreement_is_short_by_definition() {
        assert!(is_agreement("sure"));
        assert!(is_agreement("go ahead"));
        assert!(!is_agreement("yes but only if the market is open and you can get a real quote"),
                "a long sentence is a statement, not a bare agreement");
    }

    #[test]
    fn a_bare_ticker_spoken_in_caps_is_picked_up() {
        assert_eq!(symbols_in("what's MRVL doing"), vec!["MRVL"]);
    }
}
