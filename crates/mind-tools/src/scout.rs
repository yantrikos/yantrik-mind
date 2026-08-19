//! SCOUT — find sources, judge them, and spend attention on the ones that earn it.
//!
//! Everything before this took the roster as given: five handles someone typed. That is not how a
//! person learns a field. They go looking, watch a bit of several, form a quick opinion about which
//! are worth more time, and then — the part that matters — revise that opinion as the sources turn
//! out to be right or wrong. Attention is the scarce thing, and deciding where to spend it is a
//! judgment the mind should be making rather than inheriting.
//!
//! Three stages, deliberately separate because they fail differently:
//!
//! 1. **DISCOVER** — what is live right now that claims to be about trading.
//! 2. **APPRAISE** — from ONE glance, is this the kind of source that could ever be useful? A desk
//!    showing real positions is checkable; a person talking over a chart is not. This is cheap and
//!    shallow on purpose: it is a filter on FORM, not a verdict on skill.
//! 3. **TRUST** — earned only from graded outcomes. Whether someone is any good is not visible in a
//!    frame, and a confident presentation is evidence of confidence, not of edge.
//!
//! ## Why a new source is neither trusted nor ignored
//!
//! A source with no record cannot be rated, and the two obvious answers are both wrong. Trust it,
//! and the mind copies strangers. Ignore it, and nothing new is ever learned — the roster ossifies
//! into whatever was typed first, which is exactly the limitation this module exists to remove.
//!
//! So an unproven source gets PROVISIONAL standing: watched enough to generate gradeable claims,
//! never enough to act on. It earns its way up or falls off. That is the explore/exploit trade made
//! explicit rather than smuggled in as a default.

use serde::{Deserialize, Serialize};

/// A live broadcast that might be worth watching.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub video_id: String,
    pub title: String,
    pub channel: String,
    /// Concurrent viewers, when the platform reports them.
    pub viewers: Option<u64>,
}

/// What one glance says about the FORM of a source — not its skill.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Appraisal {
    /// A position bar, holdings, or explicit LONG/SHORT state is on screen.
    pub shows_positions: bool,
    /// Specific tickers are named, so claims can be attached to something checkable.
    pub names_tickers: bool,
    /// Price levels are visible, so a claim can be graded against a number.
    pub shows_levels: bool,
    /// Reads as a sales pitch — a course, a signal group, a discount code.
    pub selling_something: bool,
}

impl Appraisal {
    /// Could this source ever produce a checkable claim?
    ///
    /// Deliberately about FORM. A desk that prints its positions can be graded whether the traders
    /// are brilliant or hopeless; a commentator with no tickers and no levels cannot be graded at
    /// all, and an ungradeable source is worthless to a mind that learns from being scored — however
    /// insightful it sounds.
    pub fn is_checkable(&self) -> bool {
        (self.shows_positions || self.names_tickers) && !self.selling_something
    }

    /// Read an appraisal out of a vision reading. Keyword-based and shallow on purpose: this is a
    /// cheap first filter over many candidates, and anything expensive here defeats the point of
    /// looking at many.
    pub fn from_reading(reading: &str) -> Appraisal {
        let up = reading.to_uppercase();
        let has = |needles: &[&str]| needles.iter().any(|n| up.contains(n));
        Appraisal {
            shows_positions: has(&["LONG=", "SHORT=", "NO POSITIONS", "POSITION", "ENTRY", "AVG PRICE"]),
            // Ticker detection reads the ORIGINAL text, never the uppercased copy. Checking the
            // uppercased one destroys the only signal that separates AMD from "and": every word is
            // caps by then, so a plain English sentence scores as a screen full of tickers and any
            // talking head is admitted as a checkable source.
            names_tickers: has(&["TICKER", "$"]) || reading.split_whitespace().any(is_ticker_shaped),
            shows_levels: has(&["SUPPORT", "RESISTANCE", "TARGET", "STOP", "LEVEL"]),
            selling_something: has(&[
                "DISCORD", "PROMO CODE", "SIGN UP", "COURSE", "MENTORSHIP", "SUBSCRIBE FOR", "JOIN NOW", "DM ME",
            ]),
        }
    }
}

/// A bare word that looks like a US ticker.
fn is_ticker_shaped(w: &str) -> bool {
    let t = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    !t.is_empty()
        && t.len() <= 5
        && t.chars().all(|c| c.is_ascii_uppercase())
        // Words that are shaped like tickers but are not: the vocabulary of trading commentary.
        && !matches!(t, "LONG" | "SHORT" | "FLAT" | "BUY" | "SELL" | "THE" | "AND" | "LIVE" | "NEWS" | "USD" | "NONE")
}

/// How much a source has earned. Derived from GRADED claims, never from how it looks.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Record {
    pub graded: u32,
    pub correct: u32,
}

/// What the mind should do with a source right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// No record yet: watch and log, never act.
    Provisional,
    /// Beaten a coin flip over enough calls to be worth acting on.
    Trusted,
    /// Graded often enough, and wrong often enough, to stop spending attention on.
    Dropped,
}

/// Calls needed before a record means anything.
///
/// Ten is not a statistically comfortable number, and using it to ACT would be reckless. It is the
/// threshold at which a source stops being unknown and starts being suspected, which is a different
/// and much cheaper decision — the cost of being wrong here is wasted attention, not a wrong trade.
pub const MIN_GRADED: u32 = 10;

/// Where a source stands, from its record alone.
pub fn standing(r: &Record) -> Standing {
    if r.graded < MIN_GRADED {
        return Standing::Provisional;
    }
    let hit = r.correct as f64 / r.graded as f64;
    // A coin flip is not an edge, and a source needs to beat it by enough that noise is an unlikely
    // explanation before the mind hands it real weight.
    if hit >= 0.60 {
        Standing::Trusted
    } else if hit < 0.45 {
        Standing::Dropped
    } else {
        Standing::Provisional
    }
}

/// A stable, human-readable name for whatever produced a claim.
///
/// Records are only useful if two claims from the same desk land under the same key. A raw watch URL
/// does not do that: a desk restarts its stream several times a day, so `watch?v=…` would file every
/// shift as a brand-new source that never accumulates a record and is therefore never trusted or
/// dropped. The CHANNEL is the thing with a reputation.
pub fn source_label(url: &str) -> String {
    let u = url.trim();
    if let Some(rest) = u.split("youtube.com/").nth(1) {
        let seg = rest.trim_start_matches('/');
        if let Some(handle) = seg.strip_prefix('@') {
            let name = handle.split(['/', '?', '&']).next().unwrap_or(handle);
            return format!("@{name}");
        }
    }
    // Not a channel URL — fall back to the host, which at least groups by publisher.
    u.split("://")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .map(|h| h.trim_start_matches("www.").to_string())
        .unwrap_or_else(|| "(unknown source)".into())
}

/// Roll graded claims up into a record per source.
///
/// This is the join that makes the whole thing a learning loop rather than a set of types. Claims
/// already go to the judgment ledger stamped with where they came from; grading already marks them
/// right or wrong. Without this step those two facts never meet, and "trust" stays a struct that is
/// constructed and never updated — the shape of learning with none of it happening.
///
/// UNGRADED claims are excluded rather than counted as failures. A prediction whose deadline has not
/// arrived is not a wrong prediction, and treating pending as wrong would punish exactly the sources
/// that make long-horizon calls.
pub fn tally<'a>(graded: impl Iterator<Item = (&'a str, bool)>) -> std::collections::BTreeMap<String, Record> {
    let mut out: std::collections::BTreeMap<String, Record> = Default::default();
    for (source, correct) in graded {
        let e = out.entry(source.trim().to_string()).or_default();
        e.graded += 1;
        if correct {
            e.correct += 1;
        }
    }
    out
}

/// Rank candidates for attention: checkable form first, then audience as a weak prior.
///
/// Viewers are used ONLY to break ties. A large audience is evidence about production values and
/// nothing else — the most-watched desk and the most-profitable desk are different questions, and
/// conflating them would have the mind learning from whoever is best at being watched.
pub fn rank<'a>(cands: &'a [(Candidate, Appraisal)]) -> Vec<&'a Candidate> {
    let mut worth: Vec<&(Candidate, Appraisal)> = cands.iter().filter(|(_, a)| a.is_checkable()).collect();
    worth.sort_by(|x, y| {
        let score = |a: &Appraisal| {
            (a.shows_positions as u8) * 2 + (a.names_tickers as u8) + (a.shows_levels as u8)
        };
        score(&y.1)
            .cmp(&score(&x.1))
            .then(y.0.viewers.unwrap_or(0).cmp(&x.0.viewers.unwrap_or(0)))
    });
    worth.into_iter().map(|(c, _)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, viewers: u64) -> Candidate {
        Candidate { video_id: id.into(), title: "t".into(), channel: "c".into(), viewers: Some(viewers) }
    }

    #[test]
    fn a_source_that_cannot_be_graded_is_worthless_however_good_it_sounds() {
        // A commentator with no tickers, no positions and no levels may be the most insightful
        // person on the platform, and the mind still cannot learn anything from them: nothing they
        // say can be scored. Gradeability is not a proxy for quality, it is a precondition for it.
        let talker = Appraisal::from_reading("Let me explain my macro view on the economy this quarter");
        assert!(!talker.is_checkable());

        let desk = Appraisal::from_reading("CHERIF | LONG=NONE | SHORT=NVAX");
        assert!(desk.is_checkable(), "{desk:?}");
        assert!(desk.shows_positions);
    }

    #[test]
    fn a_sales_pitch_is_rejected_even_when_it_names_tickers() {
        // Naming tickers next to a promo code is marketing wearing the costume of analysis, and it
        // is the single most common shape of bad trading content.
        let pitch = Appraisal::from_reading("AMD TSLA to the moon! JOIN NOW - use PROMO CODE for my course");
        assert!(pitch.names_tickers, "it does name tickers");
        assert!(pitch.selling_something);
        assert!(!pitch.is_checkable(), "but it must not be watched");
    }

    #[test]
    fn a_new_source_is_neither_trusted_nor_ignored() {
        // Both obvious answers are wrong: trusting strangers copies anyone, ignoring them freezes
        // the roster at whatever was typed first.
        assert_eq!(standing(&Record { graded: 0, correct: 0 }), Standing::Provisional);
        assert_eq!(standing(&Record { graded: 9, correct: 9 }), Standing::Provisional,
                   "nine out of nine is still not a record");
    }

    #[test]
    fn trust_is_earned_from_being_right_and_lost_from_being_wrong() {
        assert_eq!(standing(&Record { graded: 20, correct: 14 }), Standing::Trusted);
        assert_eq!(standing(&Record { graded: 20, correct: 10 }), Standing::Provisional, "a coin flip is not an edge");
        assert_eq!(standing(&Record { graded: 20, correct: 6 }), Standing::Dropped);
    }

    #[test]
    fn a_reputation_belongs_to_the_channel_not_to_one_broadcast() {
        // A desk restarts its stream several times a day. Filing claims under the video URL would
        // give every shift a fresh, empty record — so no source would ever accumulate enough graded
        // calls to be trusted OR dropped, and the whole ledger would stay permanently provisional.
        assert_eq!(source_label("https://www.youtube.com/@TraderTVLive/live"), "@TraderTVLive");
        assert_eq!(source_label("https://youtube.com/@BearBullTraders/live?x=1"), "@BearBullTraders");
        // A bare video URL has no channel in it; grouping by host is the honest fallback.
        assert_eq!(source_label("https://www.youtube.com/watch?v=NpZf5vWGVw8"), "youtube.com");
    }

    #[test]
    fn trust_is_computed_from_the_ledger_the_mind_already_keeps() {
        // The join that turns types into learning: claims are logged with their source and graded
        // later; this is where those two facts finally meet.
        let rows = vec![
            ("@TraderTVLive", true),
            ("@TraderTVLive", true),
            ("@TraderTVLive", false),
            ("@SomeGuru", false),
            ("@SomeGuru", false),
        ];
        let t = tally(rows.into_iter());
        assert_eq!(t["@TraderTVLive"], Record { graded: 3, correct: 2 });
        assert_eq!(t["@SomeGuru"], Record { graded: 2, correct: 0 });
        // Neither has enough calls to be rated yet, and saying so is the correct answer.
        assert_eq!(standing(&t["@SomeGuru"]), Standing::Provisional);
    }

    #[test]
    fn a_pending_claim_is_not_a_failed_one() {
        // Only GRADED claims reach tally. Counting pending predictions as wrong would punish the
        // sources that make longer-horizon calls, which is precisely backwards.
        let graded_only = vec![("@A", true)];
        let t = tally(graded_only.into_iter());
        assert_eq!(t["@A"].graded, 1, "a source with one graded call and nine pending has ONE record");
    }

    #[test]
    fn audience_size_only_breaks_ties() {
        // The most-watched desk and the most-profitable desk are different questions. If viewers
        // outranked form, the mind would learn from whoever is best at being watched.
        let big_talker = (cand("a", 50_000), Appraisal::from_reading("my macro view on the economy"));
        let small_desk = (cand("b", 12), Appraisal::from_reading("JOE | LONG=TQQQ | SHORT=NONE"));
        let pool = [big_talker, small_desk];
        let ranked = rank(&pool);
        assert_eq!(ranked.len(), 1, "the talker is not rankable at all");
        assert_eq!(ranked[0].video_id, "b");
    }

    #[test]
    fn among_checkable_sources_the_one_showing_positions_wins() {
        let names_only = (cand("a", 9_000), Appraisal::from_reading("watching $AMD and $NVDA today"));
        let with_book = (cand("b", 30), Appraisal::from_reading("CHERIF | LONG=AMD | SHORT=NONE"));
        let pool = [names_only, with_book];
        let ranked = rank(&pool);
        assert_eq!(ranked[0].video_id, "b", "a printed position beats a mentioned ticker");
    }
}
