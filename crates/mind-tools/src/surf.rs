//! SURF — many feeds at once, and none of them configured by hand.
//!
//! The first version of continuous watching was aimed at ONE broadcast: a crop rectangle tuned,
//! pixel by pixel, to where two particular traders' position badges sit in one particular studio's
//! lower-third. It worked, and it was a dead end. Every constant in it is a fact about that
//! channel's graphic design, so a second feed needs a second tuning session and a redesign silently
//! invalidates the first — the watcher keeps reporting healthy while watching a decorative pixel.
//!
//! A mind that surfs cannot be tuned per channel. What generalises is already in hand and was
//! demonstrated on two unrelated broadcasts on the same afternoon: the VISION model reads a whole
//! uncropped frame and says what is on it — a watchlist from one studio, a position banner from
//! another, neither one configured. Cropping was a cost optimisation, and it bought cheapness at
//! the price of the only property that matters here, which is working somewhere it has not been.
//!
//! So this module holds the part that is genuinely channel-independent: finding what is live right
//! now, and keeping a per-feed record of what was last seen so that a CHANGE can be recognised.
//! The interesting signal was never a snapshot — a trader who is flat tells you nothing. It is the
//! TRANSITION, and a transition needs two observations of the same feed and no knowledge whatsoever
//! of where on the screen it happened.

use serde::{Deserialize, Serialize};

/// A channel worth checking, named the way a person would name it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feed {
    /// YouTube handle, e.g. "@TraderTVLive".
    pub handle: String,
    /// Why this feed is in the rotation — kept so a stale roster explains itself.
    pub why: String,
}

/// What one look at one feed found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sighting {
    pub handle: String,
    pub video_id: String,
    pub title: String,
    /// Whatever the vision model said about the frame — deliberately free text, because the whole
    /// point is that no schema is imposed on a channel we have never seen.
    pub seen: String,
    pub at_ms: i64,
}

/// The default rotation: live desks and live business news. A starting roster, not a fixed one —
/// the mind is expected to add and drop handles as it learns which ones repay the attention.
pub fn default_feeds() -> Vec<Feed> {
    vec![
        Feed { handle: "@TraderTVLive".into(), why: "live trading desk; on-screen position badges".into() },
        Feed { handle: "@BearBullTraders".into(), why: "live trading desk; on-screen watchlist".into() },
        Feed { handle: "@business".into(), why: "Bloomberg; macro headlines".into() },
        Feed { handle: "@CNBCtelevision".into(), why: "US market news".into() },
        Feed { handle: "@NDTVProfitIndia".into(), why: "Indian market session".into() },
    ]
}

/// Parse a handle list from free text ("@a, @b @c"), so a roster can come from a command or config
/// without a format anyone has to learn.
pub fn parse_feeds(spec: &str) -> Vec<Feed> {
    spec.split(|c: char| c == ',' || c.is_whitespace())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            let h = if s.starts_with('@') { s.to_string() } else { format!("@{s}") };
            Feed { handle: h, why: "named by the operator".into() }
        })
        .collect()
}

/// The live URL for a handle. YouTube resolves `/live` to whatever that channel is streaming now,
/// which is why the roster is handles and not video ids: a video id is a broadcast, a handle is a
/// SOURCE, and the mind should follow sources.
pub fn live_url(handle: &str) -> String {
    format!("https://www.youtube.com/{}/live", handle.trim_start_matches('@').trim())
}

/// Did the feed's state actually change between two looks?
///
/// Deliberately crude, and crude in a specific direction: it compares the SUBSTANCE of two vision
/// readings after stripping the noise that always differs (digits, punctuation, case). Prices tick
/// every second and a clock never repeats, so a strict comparison would call every pair of frames a
/// change and be exactly as useless as the scene-detector that fired 776 times in 25 seconds.
///
/// What survives the stripping is words — LONG, SHORT, flat, a ticker symbol, a trader's name. A
/// position opening changes those. A price ticking does not.
pub fn changed(before: &str, after: &str) -> bool {
    let (a, b) = (exposure(before), exposure(after));
    match (a, b) {
        // A failed look is not a transition. Treating an unreadable frame as "everything changed"
        // would manufacture a signal out of a broken frame grab.
        (None, _) | (_, None) => false,
        (Some(x), Some(y)) => x != y,
    }
}

/// What the screen says anyone is HOLDING — the only part of it that is a signal.
///
/// Two readings of one unchanged broadcast, captured to settle this rather than guessed at:
///
///   POSITIONS: CHERIF=FLAT, JOE=FLAT
///   TICKERS: TSM, TCHN, AAPL, PENT, UNH, DOGEUSDT, DXY, PLTR, DELL, BRK-B, COST, HOLO, ZS
///
///   POSITIONS: LONG=FLAT:CHERIF, SHORT=FLAT:CHERIF, LONG=FLAT:JOE, SHORT=FLAT:JOE
///   TICKERS: BRK-B, COST, HOLO, ZS, RDDT, HIMS, SLV
///
/// Nothing happened between them, and almost everything differs. The TICKER list differs because
/// the tape SCROLLS — those symbols are a conveyor belt, not a state, and no amount of care in the
/// comparison can make them stable. The POSITIONS line differs because the model read the LONG and
/// SHORT button labels as trader names the second time; the wording of a reading is not reliable
/// even when the format is pinned.
///
/// What both readings agree on is that nobody holds anything. So the comparison is reduced to
/// exactly that: the set of non-flat exposures. Everyone flat is one state; CHERIF long AMD is
/// another. Names, labels, ordering, and the whole scrolling tape are discarded, because a change in
/// any of them is not news and this detector exists to fire only on news.
pub fn exposure(reading: &str) -> Option<std::collections::BTreeSet<String>> {
    let line = reading
        .lines()
        .find(|l| l.trim().to_uppercase().starts_with("POSITIONS"))
        .unwrap_or("");
    if line.trim().is_empty() {
        return None; // no positions field at all — a failed or malformed look.
    }
    let up = line.to_uppercase();
    let mut held = std::collections::BTreeSet::new();
    // Each comma-separated entry may name a state and, if held, a symbol.
    for part in up.split(',') {
        let has_long = part.contains("LONG");
        let has_short = part.contains("SHORT");
        // FLAT anywhere in the entry means this entry reports no exposure, whatever else it says —
        // which is what makes "LONG=FLAT:CHERIF" read correctly as flat rather than as a long.
        if part.contains("FLAT") || part.contains("NO POSITION") || part.contains("NONE") {
            continue;
        }
        if !has_long && !has_short {
            continue;
        }
        // The symbol is the token that is not a state word and not a name we can identify; take
        // any alphabetic run of 1-5 chars that is not a state word.
        let sym = part
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
            .map(|t| t.trim())
            .find(|t| {
                !t.is_empty()
                    && t.len() <= 5
                    && !matches!(*t, "LONG" | "SHORT" | "FLAT" | "POSITIONS" | "NONE")
                    && t.chars().any(|c| c.is_ascii_alphabetic())
            })
            .unwrap_or("?");
        held.insert(format!("{}:{}", if has_long { "LONG" } else { "SHORT" }, sym));
    }
    Some(held)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_is_a_source_and_resolves_to_whatever_is_live_now() {
        // The roster holds handles, never video ids: a video id is one broadcast that ends, a
        // handle is a source the mind can keep following tomorrow.
        assert_eq!(live_url("@TraderTVLive"), "https://www.youtube.com/TraderTVLive/live");
        assert_eq!(live_url("TraderTVLive"), "https://www.youtube.com/TraderTVLive/live");
    }

    #[test]
    fn a_ticking_price_is_not_a_change_but_a_position_is() {
        // The lesson from the scene-detector that fired every frame: everything on a trading screen
        // moves, so "different pixels" is not a signal.
        let flat = "POSITIONS: CHERIF=FLAT, JOE=FLAT\nTICKERS: SPY, QQQ, MRNA";
        let ticked = "POSITIONS: CHERIF=FLAT, JOE=FLAT\nTICKERS: SPY, QQQ, MRNA";
        assert!(!changed(flat, ticked), "an unchanged screen must not read as a change");

        let opened = "POSITIONS: CHERIF=LONG:AMD, JOE=FLAT\nTICKERS: SPY, QQQ, MRNA";
        assert!(changed(flat, opened), "a trader taking a position IS the signal");
    }

    #[test]
    fn two_real_readings_of_one_unchanged_broadcast_are_not_a_transition() {
        // THE BUG THIS EXISTS FOR, pinned with the ACTUAL readings that exposed it. Nothing
        // happened on that stream between these two looks, and three consecutive passes all
        // reported CHANGED — which would have sent the mind to trade on nothing.
        //
        // Note how little the two agree on: the tape scrolled to an entirely different set of
        // symbols, and the model read the LONG/SHORT button labels as trader names the second time.
        // Only one thing is common to both, and it happens to be the only thing that matters —
        // nobody is holding anything.
        let r1 = "POSITIONS: CHERIF=FLAT, JOE=FLAT\n\
                  TICKERS: TSM, TCHN, AAPL, PENT, UNH, DOGEUSDT, DXY, PLTR, DELL, BRK-B, COST, HOLO, ZS";
        let r2 = "POSITIONS: LONG=FLAT:CHERIF, SHORT=FLAT:CHERIF, LONG=FLAT:JOE, SHORT=FLAT:JOE\n\
                  TICKERS: BRK-B, COST, HOLO, ZS, RDDT, HIMS, SLV";
        assert_eq!(exposure(r1), Some(Default::default()), "everyone flat is no exposure");
        assert_eq!(exposure(r2), Some(Default::default()), "'LONG=FLAT:CHERIF' is flat, not a long");
        assert!(!changed(r1, r2), "the scrolling tape and a mislabelled name are not a transition");
    }

    #[test]
    fn someone_actually_taking_a_position_still_fires() {
        // The detector must not have been made deaf by being made quiet.
        let flat = "POSITIONS: CHERIF=FLAT, JOE=FLAT\nTICKERS: SPY, MRNA";
        let long_amd = "POSITIONS: CHERIF=LONG:AMD, JOE=FLAT\nTICKERS: SPY, MRNA, AMD";
        assert!(changed(flat, long_amd), "flat -> long AMD is the whole point");
        assert!(changed(long_amd, flat), "and closing it is a transition too");

        // A different trader taking the same side of a DIFFERENT name is also news.
        let short_zs = "POSITIONS: CHERIF=LONG:AMD, JOE=SHORT:ZS\nTICKERS: SPY";
        assert!(changed(long_amd, short_zs));
    }

    #[test]
    fn a_malformed_reading_is_a_failed_look_not_a_transition() {
        // An unreadable frame must never manufacture a signal.
        assert_eq!(exposure("I cannot make out this image."), None);
        assert!(!changed("POSITIONS: CHERIF=FLAT", "I cannot make out this image."));
    }

    #[test]
    fn a_failed_look_is_not_a_transition() {
        // An empty reading means the eyes failed, and treating that as "everything changed" would
        // manufacture a signal out of a broken frame grab — the copy-trade equivalent of trading a
        // gap in the data.
        assert!(!changed("CHERIF LONG no positions", ""));
        assert!(!changed("", "CHERIF LONG no positions"));
    }

    #[test]
    fn a_roster_can_be_named_in_the_way_a_person_would_write_it() {
        let f = parse_feeds("@TraderTVLive, BearBullTraders  @business");
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].handle, "@TraderTVLive");
        assert_eq!(f[1].handle, "@BearBullTraders", "a missing @ is a typo, not a different channel");
        assert_eq!(f[2].handle, "@business");
    }

    #[test]
    fn the_default_rotation_spans_desks_and_news_in_two_markets() {
        let f = default_feeds();
        assert!(f.len() >= 4);
        assert!(f.iter().any(|x| x.why.contains("trading desk")));
        assert!(f.iter().any(|x| x.handle.contains("NDTV")), "an Indian session is a different clock, not a duplicate");
    }
}
