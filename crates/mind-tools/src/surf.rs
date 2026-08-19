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
    let (a, b) = (tokens(before), tokens(after));
    if a.is_empty() || b.is_empty() {
        return false; // nothing to compare is not a change; it is a failed look.
    }
    a != b
}

/// The stable part of one reading: the set of position states and symbols, uppercased and sorted.
///
/// Everything that made prose comparison useless is dropped here. Words shorter than two characters
/// and the field labels carry no state; numbers are prices and always move. What remains is a closed
/// vocabulary — LONG, SHORT, FLAT, a trader's name, a ticker — which is identical across two
/// readings of an unchanged screen and differs exactly when the screen's MEANING differs.
fn tokens(s: &str) -> std::collections::BTreeSet<String> {
    const LABELS: &[&str] = &["positions", "tickers", "none", "and", "the"];
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2 && !LABELS.contains(w))
        // A token that is all digits is a price or a size, and both move every second.
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .map(|w| w.to_string())
        .collect()
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
    fn the_same_screen_read_twice_is_not_a_transition() {
        // THE BUG THIS EXISTS FOR. The first version diffed the vision model's PROSE, and three
        // consecutive passes over one feed all reported CHANGED — free text is never stable, so the
        // detector fired every time and would have sent the mind to trade on nothing. Only the
        // closed vocabulary is comparable, so trivial reformatting must read as identical.
        let a = "POSITIONS: CHERIF=FLAT, JOE=FLAT\nTICKERS: SPY, MRNA, AMD";
        let b = "POSITIONS:  joe=flat,  cherif=flat\nTICKERS:  amd, spy, mrna";
        assert!(!changed(a, b), "same state, different order and case — not a transition");

        // And a price appearing or moving inside the reading must not trip it either.
        let c = "POSITIONS: CHERIF=FLAT, JOE=FLAT\nTICKERS: SPY 771.60, MRNA 137.11, AMD";
        assert!(!changed(a, c), "prices are not state");
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
