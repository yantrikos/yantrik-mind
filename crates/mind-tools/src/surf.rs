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

/// HOW to look at a feed, and what counts as a change.
///
/// Surfing is not a trading feature. Watching sources and noticing when one of them changes is a
/// general capability; "a trader went from flat to long NVAX" is one domain's idea of a change, and
/// baking it in here would have made the module useless for a news channel, a status page, or a
/// scoreboard — and would have forced every future domain to fork it.
///
/// A lens supplies the two domain-specific pieces and nothing else: the QUESTION put to the vision
/// model, and the REDUCER that turns a free-text reading into a comparable state. Everything around
/// them — the roster, what is live, glancing, storing, diffing — stays generic.
///
/// The reducer returns None for an unreadable answer, which is deliberately different from an empty
/// set: "I could not read this" must never compare unequal to a previous look and manufacture a
/// transition out of a failed glance.
#[derive(Clone, Copy)]
pub struct Lens {
    pub name: &'static str,
    /// What to ask the vision model about a frame.
    pub prompt: &'static str,
    /// Reading → comparable state, or None if the reading is unusable.
    pub reduce: fn(&str) -> Option<std::collections::BTreeSet<String>>,
}

impl std::fmt::Debug for Lens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Lens({})", self.name)
    }
}

/// A channel worth checking, named the way a person would name it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Feed {
    /// YouTube handle, e.g. "@TraderTVLive".
    pub handle: String,
    /// Why this feed is in the rotation — kept so a stale roster explains itself.
    pub why: String,
    /// Which lens to read it through. A trading desk and a news channel are watched for different
    /// things, so the roster says which rather than one lens pretending to suit both.
    pub lens: String,
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
        Feed { handle: "@TraderTVLive".into(), why: "live trading desk; on-screen position badges".into(), lens: "desk".into() },
        Feed { handle: "@BearBullTraders".into(), why: "live trading desk; on-screen watchlist".into(), lens: "desk".into() },
        Feed { handle: "@business".into(), why: "Bloomberg; macro headlines".into(), lens: "headlines".into() },
        Feed { handle: "@CNBCtelevision".into(), why: "US market news".into(), lens: "headlines".into() },
        Feed { handle: "@NDTVProfitIndia".into(), why: "Indian market session".into(), lens: "desk".into() },
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
            // Inherit the lens the ROSTER gives this handle. Naming a feed on the command line used
            // to force the generic headline lens onto it, so `surf @TraderTVLive` diffed scrolling
            // news copy on a trading desk and reported a change every single pass. Four fixes to the
            // desk reducer all failed for the same reason: the desk lens was never running.
            let lens = default_feeds()
                .into_iter()
                .find(|f| f.handle.eq_ignore_ascii_case(&h))
                .map(|f| f.lens)
                .unwrap_or_else(|| "headlines".into());
            Feed { handle: h, why: "named by the operator".into(), lens }
        })
        .collect()
}

/// The live URL for a handle. YouTube resolves `/live` to whatever that channel is streaming now,
/// which is why the roster is handles and not video ids: a video id is a broadcast, a handle is a
/// SOURCE, and the mind should follow sources.
///
/// This distinction is not theoretical. TraderTV ran "Moderna Goes Parabolic" and then, the same
/// afternoon with the same traders, "Wall Street Bounces" under a new id — desks end a stream and
/// start another when the shift changes. Anything holding the id was left watching a finished
/// recording while the desk carried on trading.
pub fn live_url(handle: &str) -> String {
    format!("https://www.youtube.com/{}/live", handle.trim_start_matches('@').trim())
}

/// A YouTube search URL for live broadcasts matching a query.
///
/// The roster is a starting point, not the world. When every desk on it is dark the mind should go
/// LOOK for one rather than report that nothing is happening — a hand-written list of five handles
/// is exactly the kind of constant that quietly becomes wrong, and "my roster is empty" is not the
/// same fact as "no desk is trading".
pub fn search_live_url(query: &str) -> String {
    // sp=EgJAAQ%253D%253D is YouTube's "live" search filter.
    format!(
        "https://www.youtube.com/results?search_query={}&sp=EgJAAQ%253D%253D",
        urlencoding::encode(query.trim())
    )
}

/// Handles worth searching for when the roster is dark, in priority order.
pub fn discovery_queries() -> Vec<&'static str> {
    vec![
        "live day trading",
        "stock market live trading",
        "live trading desk",
        "market open live",
    ]
}

/// Did this feed's state change between two readings, as THIS lens defines state?
pub fn changed_by(lens: &Lens, before: &str, after: &str) -> bool {
    match ((lens.reduce)(before), (lens.reduce)(after)) {
        // A failed look is not a transition. Treating an unreadable frame as "everything changed"
        // would manufacture a signal out of a broken frame grab.
        (None, _) | (_, None) => false,
        (Some(x), Some(y)) => x != y,
    }
}

/// The GENERIC lens: what is written on the screen, minus everything that always moves.
///
/// Suits a news channel or a status page, where the state is "which stories are up" rather than any
/// structured record. Numbers are dropped because a clock, a price and a viewer count change every
/// second and none of them is news.
pub const HEADLINE_LENS: Lens = Lens {
    name: "headlines",
    prompt: "List the headlines and any large on-screen text, one per line, exactly as printed. \
             No prose, no explanation, no description of images. If nothing readable, reply NONE.",
    reduce: reduce_headlines,
};

fn reduce_headlines(reading: &str) -> Option<std::collections::BTreeSet<String>> {
    let up = reading.trim().to_uppercase();
    if up.is_empty() || up == "NONE" {
        return None;
    }
    let set: std::collections::BTreeSet<String> = up
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 4)
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .map(|w| w.to_string())
        .collect();
    if set.is_empty() {
        None
    } else {
        Some(set)
    }
}

/// Resolve a lens by name, falling back to the generic one.
pub fn lens_named(name: &str) -> Lens {
    match name.trim().to_lowercase().as_str() {
        "desk" | "trading" | "positions" => crate::desk::DESK_LENS,
        _ => HEADLINE_LENS,
    }
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
    fn a_lens_decides_what_counts_as_a_change() {
        // The generic surfer must not know what a trader is. It applies whatever lens the roster
        // names, and the SAME pair of readings can be a change under one lens and not the other —
        // which is the whole reason the lens is a parameter rather than a hardcoded rule.
        let a = "US SET TO HALVE TARIFFS ON CANADIAN STEEL
SPY 769.61";
        let b = "US SET TO HALVE TARIFFS ON CANADIAN STEEL
SPY 771.02";
        assert!(!changed_by(&HEADLINE_LENS, a, b), "the same headline at a new price is not news");

        let c = "SQM RISES AS Q2 ROUTS EXPECTATIONS
SPY 771.02";
        assert!(changed_by(&HEADLINE_LENS, b, c), "a new headline is the signal for this lens");
    }

    #[test]
    fn an_unreadable_screen_is_never_a_transition_under_any_lens() {
        // A reducer returning None means "I could not read this", which must never compare unequal
        // to a previous look and manufacture a signal out of a failed glance.
        assert_eq!((HEADLINE_LENS.reduce)("NONE"), None);
        assert!(!changed_by(&HEADLINE_LENS, "TARIFFS HALVED ON STEEL", "NONE"));
    }

    #[test]
    fn a_roster_names_its_lens_and_unknown_names_fall_back_to_the_generic_one() {
        assert_eq!(lens_named("desk").name, "desk");
        assert_eq!(lens_named("headlines").name, "headlines");
        assert_eq!(lens_named("something-nobody-wrote-yet").name, "headlines");
        // Desks and news channels in one rotation, each read for what it actually shows.
        let f = default_feeds();
        assert!(f.iter().any(|x| x.lens == "desk"));
        assert!(f.iter().any(|x| x.lens == "headlines"));
    }

    #[test]
    fn an_empty_reading_is_a_failed_look_not_a_transition() {
        // The eyes failed. Treating that as "everything changed" would manufacture a signal out of
        // a broken frame grab — the copy-trade equivalent of trading a gap in the data.
        assert!(!changed_by(&HEADLINE_LENS, "TARIFFS HALVED ON STEEL", ""));
        assert!(!changed_by(&HEADLINE_LENS, "", "TARIFFS HALVED ON STEEL"));
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
