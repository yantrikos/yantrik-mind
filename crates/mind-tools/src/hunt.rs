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
    /// Price x shares. The only honest measure of whether a position can be got out of.
    pub dollar_volume: f64,
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
    /// Too little money changing hands to get back out of.
    TooThin { dollars: f64 },
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
            Self::TooThin { dollars } => write!(f, "only ${:.0}m traded — too thin to leave a position in", dollars / 1e6),
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
    // Zero means the caller had no volume figure, not that nothing traded — only judge when known.
    if m.dollar_volume > 0.0 && m.dollar_volume < MIN_DOLLAR_VOLUME {
        return Err(Reject::TooThin { dollars: m.dollar_volume });
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
                dollar_volume: 0.0,
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

/// How old a story is, in the words a person would use.
///
/// STORY TIME AND READING TIME ARE DIFFERENT FACTS, and conflating them is how stale news gets
/// traded as fresh. Live examples: TNON carried "Stock Surges on Key Patent News" while it was down
/// 41% — the surge was a previous day's story. MARA's move was "attributed to a 'Friday' event",
/// which the model only distrusted because the word Friday happened to be in the text.
///
/// A catalyst is only a catalyst if it is NEW. Without an age beside it, a six-hour-old headline and
/// a six-minute-old one look identical on the page, and only one of them explains a move happening
/// now.
pub fn age_phrase(at: &str, now_ms: i64) -> String {
    let Some(t) = crate::shadow::parse_rfc3339_ms(at) else {
        return "undated".to_string();
    };
    let mins = (now_ms - t) / 60_000;
    if mins < 0 {
        // A future timestamp is a clock or feed problem, and saying so beats printing "-3m ago".
        return "timestamped in the future".to_string();
    }
    match mins {
        0 => "just now".to_string(),
        1..=59 => format!("{mins}m ago"),
        60..=1439 => format!("{}h ago", mins / 60),
        _ => format!("{}d ago", mins / 1440),
    }
}

/// Is this story fresh enough to explain a move happening NOW?
///
/// Four hours is the working line for a same-day thesis: within one session, and outside it the
/// story has been available long enough that the move it caused has already happened.
pub const FRESH_CATALYST_MINS: i64 = 240;

pub fn is_fresh(at: &str, now_ms: i64) -> bool {
    crate::shadow::parse_rfc3339_ms(at)
        .map(|t| (now_ms - t) / 60_000 <= FRESH_CATALYST_MINS && t <= now_ms)
        .unwrap_or(false)
}

/// Headlines about this symbol published SINCE a given moment, newest first.
///
/// A position is entered on a catalyst and then left alone until price moves. But the reason for
/// holding can be refuted while the price has not caught up yet — an earnings correction, a pulled
/// guidance, a denied deal. Watching only the price means finding out last.
///
/// Roundups are excluded here for the same reason they are excluded from entry: a piece tagging a
/// dozen tickers says nothing about this one, and a wall of market wallpaper would bury the single
/// headline that actually matters.
pub fn news_since<'a>(sym: &str, since_ms: i64, news: &'a [Headline]) -> Vec<&'a Headline> {
    let mut out: Vec<&Headline> = news_for(sym, news)
        .into_iter()
        .filter(|h| is_specific(h))
        .filter(|h| crate::shadow::parse_rfc3339_ms(&h.at).map(|t| t > since_ms).unwrap_or(false))
        .collect();
    out.sort_by_key(|h| std::cmp::Reverse(crate::shadow::parse_rfc3339_ms(&h.at).unwrap_or(0)));
    out
}

/// The best explanation for this symbol's move, or None if only roundups mention it.
pub fn catalyst_for<'a>(sym: &str, news: &'a [Headline]) -> Option<&'a Headline> {
    news_for(sym, news).into_iter().find(|h| is_specific(h))
}

/// The LIQUID universe: what is actually being traded in size today.
///
/// The movers endpoint ranks by percentage, which on any given day means microcaps on binary news —
/// live, mid-session, it returned TNON -41%, MRNX -37%, RDAC -32%, none of them tradeable and the
/// mind rightly declined all four. Ranking by share VOLUME is no better: a 64-cent stock trading 76
/// million shares is $49m of flow, while Apple trading 30 million is $9 billion.
///
/// Dollar volume is the measure that means anything, because it is the one that decides whether a
/// position can be got back out of. The same market, the same minute, filtered this way: MRNA -18%
/// on $3.4bn, WMT -9% on $2.4bn, MSTR +7% on $1.5bn. That is a day-trading universe; the other was
/// a lottery.
pub fn actives_url(top: usize) -> String {
    format!(
        "https://data.alpaca.markets/v1beta1/screener/stocks/most-actives?by=volume&top={}",
        top.clamp(1, 100)
    )
}

/// Snapshots carry the last price and the previous close, so today's move needs no extra call.
pub fn snapshots_url(symbols: &[String]) -> String {
    format!("https://data.alpaca.markets/v2/stocks/snapshots?symbols={}", symbols.join(","))
}

/// Turn a snapshot batch into movers, given the share volumes from the actives call.
pub fn parse_snapshots(v: &serde_json::Value, volumes: &std::collections::BTreeMap<String, f64>) -> Vec<Mover> {
    let mut out = Vec::new();
    let Some(obj) = v.as_object() else { return out };
    for (sym, d) in obj {
        let day = d.get("dailyBar");
        let prev = d.get("prevDailyBar");
        let price = day.and_then(|x| x.get("c")).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let prev_close = prev.and_then(|x| x.get("c")).and_then(|x| x.as_f64()).unwrap_or(0.0);
        if price <= 0.0 || prev_close <= 0.0 {
            continue;
        }
        let vol = volumes.get(sym).copied().unwrap_or(0.0);
        out.push(Mover {
            symbol: sym.clone(),
            price,
            percent_change: (price / prev_close - 1.0) * 100.0,
            dollar_volume: price * vol,
        });
    }
    // Most traded first — the deepest book is the easiest to leave.
    out.sort_by(|a, b| b.dollar_volume.partial_cmp(&a.dollar_volume).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Least dollar volume worth considering. Below this a position moves the price on the way out.
pub const MIN_DOLLAR_VOLUME: f64 = 50_000_000.0;

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
    // The LIQUID universe first. Percentage movers are microcaps on binary news — four consecutive
    // live hunts drew from that list and the mind declined every one, correctly. Ranking by dollar
    // volume the same minute gave MRNA -18% on $3.4bn and WMT -9% on $2.4bn: things a person could
    // actually trade.
    if let Ok(actives) = fetch_actives(top.max(40)) {
        if !actives.is_empty() {
            return Ok(actives);
        }
    }
    // Falls back to the percentage list rather than returning nothing — a thin universe beats no
    // universe, and the filters will still reject what cannot be traded.
    Ok(parse_movers(&alpaca_get(movers_url(top))?))
}

/// The most-traded names, with today's move and dollar volume attached.
pub fn fetch_actives(top: usize) -> anyhow::Result<Vec<Mover>> {
    let list = alpaca_get(actives_url(top))?;
    let mut volumes = std::collections::BTreeMap::new();
    let mut syms: Vec<String> = Vec::new();
    for a in list.get("most_actives").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
        let Some(sym) = a.get("symbol").and_then(|x| x.as_str()) else { continue };
        volumes.insert(sym.to_string(), a.get("volume").and_then(|x| x.as_f64()).unwrap_or(0.0));
        syms.push(sym.to_string());
    }
    if syms.is_empty() {
        return Ok(Vec::new());
    }
    syms.truncate(60);
    Ok(parse_snapshots(&alpaca_get(snapshots_url(&syms))?, &volumes))
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
        // Zero volume = "not known", which the filter deliberately does not judge.
        Mover { symbol: sym.into(), price, percent_change: pct, dollar_volume: 0.0 }
    }

    #[test]
    fn share_volume_is_not_liquidity_but_dollar_volume_is() {
        // Both were in the live most-actives list at the same minute. HUIZ traded 65 MILLION shares
        // and MMA 76 million — more than Walmart — and neither is a position anyone could leave.
        let huiz = Mover { symbol: "HUIZ".into(), price: 2.24, percent_change: 30.0, dollar_volume: 2.24 * 65_036_594.0 };
        let mma = Mover { symbol: "MMA".into(), price: 0.64, percent_change: 25.0, dollar_volume: 0.64 * 76_228_431.0 };
        let wmt = Mover { symbol: "WMT".into(), price: 104.16, percent_change: -8.96, dollar_volume: 2.4e9 };
        let b = Bounds::default();
        assert!(matches!(tradeable(&huiz, &b), Err(Reject::TooCheap { .. })));
        assert!(matches!(tradeable(&mma, &b), Err(Reject::TooCheap { .. })));
        assert!(tradeable(&wmt, &b).is_ok(), "Walmart down 9 percent on 2.4 billion dollars is the trade");
    }

    #[test]
    fn a_liquid_name_on_a_thin_day_is_still_refused() {
        // Price alone does not make a position exitable — a $40 stock with $8m of flow will move on
        // the way out.
        let thin = Mover { symbol: "QUIET".into(), price: 40.0, percent_change: 6.0, dollar_volume: 8_000_000.0 };
        assert!(matches!(tradeable(&thin, &Bounds::default()), Err(Reject::TooThin { .. })));
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
    fn a_story_carries_its_age_because_stale_news_reads_as_fresh() {
        // The real failures: TNON showed "Stock Surges on Key Patent News" while down 41% (a
        // previous day's story), and MARA's catalyst was a Friday event read on a Tuesday. On the
        // page a six-hour-old headline and a six-minute-old one look identical.
        let now = crate::shadow::parse_rfc3339_ms("2026-08-25T19:30:00Z").unwrap();
        assert_eq!(age_phrase("2026-08-25T19:30:00Z", now), "just now");
        assert_eq!(age_phrase("2026-08-25T19:16:00Z", now), "14m ago");
        assert_eq!(age_phrase("2026-08-25T13:30:00Z", now), "6h ago");
        assert_eq!(age_phrase("2026-08-21T19:30:00Z", now), "4d ago");
        assert_eq!(age_phrase("not a date", now), "undated");

        assert!(is_fresh("2026-08-25T19:00:00Z", now), "half an hour old explains a move now");
        assert!(!is_fresh("2026-08-25T13:00:00Z", now), "six hours old has already been traded");
        assert!(!is_fresh("2026-08-21T19:30:00Z", now), "Friday's story is not Tuesday's catalyst");
        // A future timestamp is a feed problem, not a fresh story.
        assert!(!is_fresh("2026-08-26T19:30:00Z", now));
    }

    #[test]
    fn news_after_entry_is_what_can_refute_a_thesis() {
        // A position is entered on a catalyst and then watched only by price. The reason for
        // holding can be refuted long before the price reflects it, and watching price alone means
        // finding out last.
        let old = Headline { symbols: vec!["BMNR".into()], headline: "Bitmine buys $81M of Ethereum".into(), source: "b".into(), at: "2026-08-25T18:00:00Z".into() };
        let fresh = Headline { symbols: vec!["BMNR".into()], headline: "Bitmine says the purchase was misreported".into(), source: "b".into(), at: "2026-08-25T19:40:00Z".into() };
        let roundup = Headline { symbols: "A B C D E F".split(' ').map(String::from).collect(), headline: "12 crypto stocks moving today".into(), source: "b".into(), at: "2026-08-25T19:50:00Z".into() };
        let news = vec![old, fresh, roundup];
        let entry = crate::shadow::parse_rfc3339_ms("2026-08-25T19:00:00Z").unwrap();
        let since = news_since("BMNR", entry, &news);
        assert_eq!(since.len(), 1, "only the post-entry, company-specific one: {since:?}");
        assert!(since[0].headline.contains("misreported"));
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
