//! HUNT — the mind trading on its own account of the world, not on someone else's screen.
//!
//! Copying a broadcast is a way to LEARN: it borrows a stranger's judgment and, at best, arrives a
//! few minutes late to it. An independent hunt is the other thing entirely — find what is moving,
//! find out why, decide, and follow. This module is the first half: what moved, and is it something
//! a person could actually trade.
//!
//! ## The filters are the strategy
//!
//! The first live pull of Alpaca's movers endpoint returned, in order: FIXX +1378%, ZSTK +443%,
//! MRNX +282%, TNONW at two cents, AACBR at one cent. That is what "biggest movers" means in a
//! market of eleven thousand listings — the top of the list is warrants, rights, and microcaps that
//! either cannot be exited or move on a single print.
//!
//! So the interesting code here is not the ranking, it is the REJECTING, and every rejection carries
//! its reason. A shortlist that silently dropped the junk would look identical to one that never saw
//! it, and the difference between those two is whether anyone can tell that the universe filter is
//! doing its job or quietly excluding everything.
//!
//! ## Why an extreme move is a reason to stay out
//!
//! Instinct says the biggest mover is the best opportunity. For a same-day trade the opposite holds:
//! a stock already up 300% on a binary announcement has had its news, and what remains is a
//! coin-flip on the fade with a spread to match. MRNA doubling on trial data was tradeable; the
//! thing up 1378% is a lottery ticket that has already been drawn. The cap is therefore an upper
//! bound as well as a lower one.

use serde::{Deserialize, Serialize};

/// One symbol that moved today.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mover {
    pub symbol: String,
    pub price: f64,
    pub percent_change: f64,
}

/// A headline attached to a symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Headline {
    pub symbols: Vec<String>,
    pub headline: String,
    pub source: String,
    pub at: String,
}

/// Why a mover is not a candidate. Kept as a typed reason so a shortlist can SHOW its rejections
/// rather than present a filtered list as if it were the whole market.
#[derive(Debug, Clone, PartialEq)]
pub enum Reject {
    /// Under the price floor: spreads on these are a larger edge than any thesis.
    TooCheap { price: f64 },
    /// A warrant, right or unit — not the common stock, and usually barely traded.
    NotCommonStock,
    /// The move already happened. What is left is a coin flip on the fade.
    MoveExhausted { pct: f64 },
    /// Barely moved; nothing to explain and nothing to trade.
    TooQuiet { pct: f64 },
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooCheap { price } => write!(f, "${price:.2} — under the price floor; the spread would be the trade"),
            Self::NotCommonStock => write!(f, "warrant/right/unit, not common stock"),
            Self::MoveExhausted { pct } => write!(f, "{pct:+.0}% — the news already happened; what is left is a coin flip on the fade"),
            Self::TooQuiet { pct } => write!(f, "{pct:+.1}% — nothing actually moved"),
        }
    }
}

/// Bounds for what counts as tradeable. Defaults chosen against a real movers pull, not from taste.
#[derive(Debug, Clone, Copy)]
pub struct Bounds {
    pub min_price: f64,
    pub min_move_pct: f64,
    pub max_move_pct: f64,
}

impl Default for Bounds {
    fn default() -> Self {
        // $5 floor clears the penny tier where the spread dominates any edge; 3% is the smallest
        // move worth explaining; 60% is where a move stops being a trend and becomes an event that
        // has already resolved.
        Self { min_price: 5.0, min_move_pct: 3.0, max_move_pct: 60.0 }
    }
}

/// A five-letter symbol ending in W, R or U is a warrant, right or unit — not the common stock.
///
/// Four-letter symbols are ordinary (`INTC`, `AMAT`), so the length test matters: rejecting any
/// symbol ending in W would throw away real companies.
pub fn is_derivative_symbol(sym: &str) -> bool {
    let s = sym.trim().to_uppercase();
    s.len() == 5 && matches!(s.chars().last(), Some('W') | Some('R') | Some('U'))
}

/// Is this mover worth a look? `Ok(())` or the reason it is not.
pub fn tradeable(m: &Mover, b: &Bounds) -> Result<(), Reject> {
    if is_derivative_symbol(&m.symbol) {
        return Err(Reject::NotCommonStock);
    }
    if m.price < b.min_price {
        return Err(Reject::TooCheap { price: m.price });
    }
    let mag = m.percent_change.abs();
    if mag < b.min_move_pct {
        return Err(Reject::TooQuiet { pct: m.percent_change });
    }
    if mag > b.max_move_pct {
        return Err(Reject::MoveExhausted { pct: m.percent_change });
    }
    Ok(())
}

/// Split a movers list into candidates and rejections-with-reasons.
pub fn shortlist(movers: &[Mover], b: &Bounds) -> (Vec<Mover>, Vec<(String, Reject)>) {
    let mut keep = Vec::new();
    let mut drop = Vec::new();
    for m in movers {
        match tradeable(m, b) {
            Ok(()) => keep.push(m.clone()),
            Err(r) => drop.push((m.symbol.clone(), r)),
        }
    }
    // Biggest surviving move first — among things that are actually tradeable.
    keep.sort_by(|a, c| c.percent_change.abs().partial_cmp(&a.percent_change.abs()).unwrap_or(std::cmp::Ordering::Equal));
    (keep, drop)
}

/// Parse Alpaca's movers envelope. Both directions are kept: a hunt that only looked at gainers
/// would be a long-only strategy by accident of parsing rather than by decision.
pub fn parse_movers(v: &serde_json::Value) -> Vec<Mover> {
    let mut out = Vec::new();
    for key in ["gainers", "losers"] {
        for q in v.get(key).and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let symbol = q.get("symbol").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if symbol.is_empty() {
                continue;
            }
            out.push(Mover {
                symbol,
                price: q.get("price").and_then(|x| x.as_f64()).unwrap_or(0.0),
                percent_change: q.get("percent_change").and_then(|x| x.as_f64()).unwrap_or(0.0),
            });
        }
    }
    out
}

/// Parse Alpaca's news envelope.
pub fn parse_news(v: &serde_json::Value) -> Vec<Headline> {
    v.get("news")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|n| Headline {
            symbols: n
                .get("symbols")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default(),
            headline: n.get("headline").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            source: n.get("source").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            at: n.get("created_at").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        })
        .filter(|h| !h.headline.is_empty())
        .collect()
}

/// A headline tagging more than this many symbols is a market roundup, not a catalyst.
///
/// Scoping the news query to the candidates fixed the blank column and revealed a subtler problem:
/// what came back was "Dow Gains Over 100 Points; Target Posts Upbeat Q2 Earnings" against a stock
/// down 51%, and "11 Industrials Stocks Moving In Wednesday's Intraday Session" against another
/// down 33%. Those articles tag a dozen tickers and explain none of them.
///
/// This is worse than the blank column was. An empty field is visibly missing information; a
/// roundup headline sitting next to a candidate READS as the catalyst and gets reasoned about as
/// one. A genuine company event — trial data, guidance, a deal — names one or two symbols.
pub const MAX_SYMBOLS_FOR_SPECIFIC: usize = 3;

/// Is this headline ABOUT the stock, rather than a list the stock appears in?
pub fn is_specific(h: &Headline) -> bool {
    h.symbols.len() <= MAX_SYMBOLS_FOR_SPECIFIC
}

/// Headlines that mention this symbol. A mover WITHOUT a headline is not disqualified — it is
/// flagged, because an unexplained move is a different (and worse-understood) thing than a move with
/// a known cause, and the difference belongs in the thesis rather than hidden in a filter.
pub fn news_for<'a>(sym: &str, news: &'a [Headline]) -> Vec<&'a Headline> {
    let s = sym.trim().to_uppercase();
    let mut hits: Vec<&Headline> =
        news.iter().filter(|h| h.symbols.iter().any(|x| x.trim().to_uppercase() == s)).collect();
    // Specific first, so a caller taking `.first()` gets the real catalyst when one exists rather
    // than whichever roundup happened to be most recent.
    hits.sort_by_key(|h| h.symbols.len());
    hits
}

/// The best explanation for this symbol's move, or None if only roundups mention it.
pub fn catalyst_for<'a>(sym: &str, news: &'a [Headline]) -> Option<&'a Headline> {
    news_for(sym, news).into_iter().find(|h| is_specific(h))
}

/// The movers URL on the DATA host. Split out so the shape is testable without a network call.
pub fn movers_url(top: usize) -> String {
    format!("https://data.alpaca.markets/v1beta1/screener/stocks/movers?top={}", top.clamp(1, 50))
}

/// The news URL for a set of symbols (empty = the whole firehose).
pub fn news_url(symbols: &[String], limit: usize) -> String {
    let base = "https://data.alpaca.markets/v1beta1/news";
    if symbols.is_empty() {
        format!("{base}?limit={}", limit.clamp(1, 50))
    } else {
        format!("{base}?symbols={}&limit={}", symbols.join(","), limit.clamp(1, 50))
    }
}

fn alpaca_get(url: String) -> anyhow::Result<serde_json::Value> {
    let key = std::env::var("ALPACA_KEY_ID").map_err(|_| anyhow::anyhow!("ALPACA_KEY_ID is not set"))?;
    let sec = std::env::var("ALPACA_SECRET_KEY").map_err(|_| anyhow::anyhow!("ALPACA_SECRET_KEY is not set"))?;
    Ok(ureq::get(&url)
        .set("APCA-API-KEY-ID", &key)
        .set("APCA-API-SECRET-KEY", &sec)
        .timeout(std::time::Duration::from_secs(30))
        .call()?
        .into_json()?)
}

/// Today's movers.
///
/// The HTTP lives HERE rather than in the caller: this crate already owns the Alpaca credentials
/// and the data-host rule, and letting the conversation layer make its own requests would put a
/// second place in the codebase that could address the wrong host.
pub fn fetch_movers(top: usize) -> anyhow::Result<Vec<Mover>> {
    Ok(parse_movers(&alpaca_get(movers_url(top))?))
}

/// News for SPECIFIC symbols — never the general firehose.
///
/// The first run asked for the fifty most recent headlines market-wide and then matched the
/// shortlist against them. Every candidate came back "no headline", and that was an artefact of the
/// sample rather than a fact about the stock: a general feed is dominated by large caps, so a
/// $12 name that fell 54% will never appear in it. The pipeline was therefore reporting every
/// small-cap move as unexplained, which is precisely the input the thesis leans on hardest — a
/// filter that quietly answers "no catalyst" to every question is worse than no filter, because it
/// looks like evidence.
pub fn fetch_news_for(symbols: &[String], limit: usize) -> anyhow::Result<Vec<Headline>> {
    if symbols.is_empty() {
        return Ok(Vec::new());
    }
    Ok(parse_news(&alpaca_get(news_url(symbols, limit))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(sym: &str, price: f64, pct: f64) -> Mover {
        Mover { symbol: sym.into(), price, percent_change: pct }
    }

    #[test]
    fn the_real_movers_list_is_mostly_untradeable_and_says_so() {
        // These are the ACTUAL top movers from the first live pull. If the filters ever start
        // passing this list, the hunt has become a lottery.
        let live = vec![
            m("FIXX", 13.82, 1378.55),
            m("ZSTK", 10.00, 443.45),
            m("MRNX", 119.74, 282.19),
            m("TNONW", 0.02, 185.71),
            m("AACBR", 0.01, 181.82),
            m("XOSWW", 0.004, -57.50),
            m("PFSA", 12.95, -52.88),
            m("LGCL", 0.14, -40.55),
        ];
        let (keep, drop) = shortlist(&live, &Bounds::default());
        // Exactly ONE survives, and that is the filter working rather than merely refusing: PFSA is
        // real common stock at a real price, down hard but not so far that the event has already
        // resolved. Whether to trade it is a question for a thesis; whether it is even eligible is
        // this function's whole job. (The first draft of this test asserted an empty list — that
        // was the test being wrong, not the filter.)
        assert_eq!(keep.len(), 1, "expected only PFSA to survive, got {keep:?}");
        assert_eq!(keep[0].symbol, "PFSA");
        assert_eq!(drop.len(), live.len() - 1);
        // And each rejection must explain ITSELF — a silent filter is indistinguishable from a
        // broken one.
        let why: Vec<String> = drop.iter().map(|(s, r)| format!("{s}: {r}")).collect();
        assert!(why.iter().any(|w| w.contains("warrant/right/unit")), "{why:?}");
        assert!(why.iter().any(|w| w.contains("price floor")), "{why:?}");
        assert!(why.iter().any(|w| w.contains("coin flip")), "{why:?}");
    }

    #[test]
    fn a_normal_days_move_survives() {
        // The point of the filters is to leave real candidates standing, not to reject everything.
        let day = vec![m("MRVL", 233.40, 8.05), m("NBIS", 228.64, -7.97), m("AVGO", 362.82, -4.48)];
        let (keep, drop) = shortlist(&day, &Bounds::default());
        assert_eq!(keep.len(), 3, "dropped: {drop:?}");
        assert_eq!(keep[0].symbol, "MRVL", "biggest tradeable move ranks first");
    }

    #[test]
    fn losers_are_hunted_too_or_the_strategy_is_long_only_by_accident() {
        let v = serde_json::json!({
            "gainers": [{"symbol":"AAA","price":10.0,"percent_change":9.0}],
            "losers":  [{"symbol":"BBB","price":20.0,"percent_change":-11.0}]
        });
        let ms = parse_movers(&v);
        assert_eq!(ms.len(), 2);
        let (keep, _) = shortlist(&ms, &Bounds::default());
        assert_eq!(keep[0].symbol, "BBB", "the larger move is the down one");
    }

    #[test]
    fn four_letter_symbols_ending_in_w_are_real_companies() {
        // Rejecting anything ending in W would throw away ordinary listings.
        assert!(!is_derivative_symbol("SNOW"));
        assert!(!is_derivative_symbol("LOW"));
        assert!(is_derivative_symbol("TNONW"));
        assert!(is_derivative_symbol("AACBR"));
        assert!(is_derivative_symbol("XOSWW"));
    }

    #[test]
    fn a_headline_is_matched_to_its_symbol_not_to_a_substring() {
        let news = vec![
            Headline { symbols: vec!["SNOW".into()], headline: "BofA raises Snowflake target".into(), source: "b".into(), at: "".into() },
            Headline { symbols: vec!["WDC".into()], headline: "Western Digital lab".into(), source: "b".into(), at: "".into() },
        ];
        assert_eq!(news_for("SNOW", &news).len(), 1);
        assert_eq!(news_for("NOW", &news).len(), 0, "SNOW must not match NOW");
    }

    #[test]
    fn a_market_roundup_is_not_a_catalyst() {
        // Real headlines returned against real candidates on the first scoped run. Both tag a long
        // list of tickers and explain none of them; presenting either as the reason a stock fell
        // 51% would hand the model a false premise that reads exactly like evidence.
        let roundup = Headline {
            symbols: "AAPL MSFT PFSA TGT DOW AMZN NVDA F GM BA".split(' ').map(String::from).collect(),
            headline: "Dow Gains Over 100 Points; Target Posts Upbeat Q2 Earnings".into(),
            source: "benzinga".into(),
            at: "".into(),
        };
        let real = Headline {
            symbols: vec!["PFSA".into()],
            headline: "PFSA halts trial after safety signal".into(),
            source: "benzinga".into(),
            at: "".into(),
        };
        assert!(!is_specific(&roundup));
        assert!(is_specific(&real));
        // Both mention PFSA, but only one explains it — and the specific one must win even though
        // the roundup was listed first.
        let news = vec![roundup, real];
        assert_eq!(catalyst_for("PFSA", &news).map(|h| h.headline.as_str()), Some("PFSA halts trial after safety signal"));
        assert_eq!(catalyst_for("TGT", &news), None, "a stock that only appears in a roundup has no catalyst");
    }

    #[test]
    fn an_unexplained_move_is_flagged_rather_than_filtered_away() {
        // "No headline" is information for the thesis, not grounds for silent exclusion — a move
        // nobody can explain is a different animal from one with a known cause.
        let news: Vec<Headline> = vec![];
        assert!(news_for("MRVL", &news).is_empty());
        let (keep, _) = shortlist(&[m("MRVL", 233.40, 8.05)], &Bounds::default());
        assert_eq!(keep.len(), 1, "a mover with no news is still a candidate");
    }

    #[test]
    fn urls_address_the_data_host_only() {
        // This module reads. Placing an order is broker.rs's job, on the paper host, and these two
        // must never blur.
        assert!(movers_url(10).starts_with("https://data.alpaca.markets/"));
        assert!(news_url(&[], 5).starts_with("https://data.alpaca.markets/"));
        assert!(!movers_url(10).contains("paper-api"));
        assert_eq!(news_url(&["AAPL".into(), "MSFT".into()], 5).contains("symbols=AAPL,MSFT"), true);
    }
}
