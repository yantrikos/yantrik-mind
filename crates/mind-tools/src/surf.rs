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
    fn words(s: &str) -> Vec<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphabetic())
            .filter(|w| w.len() >= 3)
            .map(|w| w.to_string())
            .collect()
    }
    let (a, b) = (words(before), words(after));
    if a.is_empty() || b.is_empty() {
        return false; // nothing to compare is not a change; it is a failed look.
    }
    let sa: std::collections::HashSet<_> = a.iter().collect();
    let sb: std::collections::HashSet<_> = b.iter().collect();
    let shared = sa.intersection(&sb).count();
    let union = sa.union(&sb).count().max(1);
    // Less than 80% overlap in content words = something on that screen is genuinely different.
    (shared as f64 / union as f64) < 0.8
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
        // moves, so "different pixels" is not a signal. Only different WORDS are.
        let flat = "CHERIF LONG SHORT no positions   JOE LONG SHORT no positions   SPY 771.60";
        let ticked = "CHERIF LONG SHORT no positions   JOE LONG SHORT no positions   SPY 771.94";
        assert!(!changed(flat, ticked), "a price tick must not read as a state change");

        let opened = "CHERIF LONG 200 shares AMD   JOE LONG SHORT no positions   SPY 771.94";
        assert!(changed(flat, opened), "a trader taking a position IS the signal");
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
