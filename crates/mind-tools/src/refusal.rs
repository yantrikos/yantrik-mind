//! REFUSAL — noticing when the mind has just told someone it cannot do something.
//!
//! The fast path exists because speech cannot wait thirty seconds for a tool loop. It reaches no
//! tool by construction, and it has drawn the obvious wrong conclusion from that: it decided it has
//! no capabilities at all. One live conversation, consecutive turns:
//!
//! ```text
//!   "I don't have a live market data feed connected right now."      (it has quote)
//!   "No, I can't actually watch live video streams."                  (it watched one that afternoon)
//!   "I don't have Walmart's live earnings or debt figures."           (it has search and web_fetch)
//!   "I don't have access to Walmart's live financial data."           (same)
//! ```
//!
//! Four refusals in four turns, every one of them false. The person stops asking — which is the real
//! cost. A wrong answer gets corrected; a refusal ends the subject.
//!
//! ## Why this is a classifier and not more grounding
//!
//! The first attempts at this fixed one DOMAIN at a time: fetch prices and put them in the prompt,
//! then tell it that it can watch video. But the next question was about Walmart's debt, and the one
//! after would have been something else. Enumerating capabilities into a prompt cannot keep up with
//! the questions a person actually asks.
//!
//! So the fast path is allowed to fail, and the failure is DETECTED. A reply that refuses is not
//! delivered — the question is re-run on the path that has tools. It costs the wait only in the case
//! that would otherwise have been a dead end, which is exactly the trade worth making.

/// Phrases in which the mind says it lacks a capability or access.
///
/// Written from real refusals rather than imagined ones, after a checker built from remembered
/// phrasings missed the first live example it met.
const REFUSAL_MARKS: &[&str] = &[
    "i don't have access",
    "i do not have access",
    "i don't have a live",
    "i don't have live",
    "i don't have the live",
    "i can't actually",
    "i cannot actually",
    "i can't pull",
    "i cannot pull",
    "i can't check",
    "i can't look up",
    "i can't tell you",
    "i cannot tell you",
    "i don't have that data",
    "i don't have the data",
    "i don't have real-time",
    "i don't have realtime",
    "i'm not able to",
    "i am not able to",
    "i don't have a tool",
    "no market-data tool",
    "no live market data",
    "not wired",
    "isn't wired",
    "in front of me right now",
    "i can't watch",
    "i cannot watch",
    "i can't browse",
    "i can't fetch",
];

/// Did this reply refuse on the grounds of a missing capability?
///
/// Deliberately narrow: it must be about the MIND's ability, not about the world. "The market is
/// closed" and "that company does not report until Tuesday" are facts, and escalating them to a
/// tool loop would spend thirty seconds re-confirming something already true.
pub fn sounds_like_refusal(reply: &str) -> bool {
    let r = reply.to_lowercase();
    REFUSAL_MARKS.iter().any(|m| r.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_consecutive_false_refusals_are_caught() {
        // Verbatim, one conversation, four turns in a row. Every one was untrue.
        for r in [
            "I don't have a live market data feed connected right now, so I can't pull the Nifty 50 movement myself.",
            "No, I can't actually watch live video streams. I can read text, data, and code, but not watch video in real-time.",
            "I don't have Walmart's live earnings or debt figures in front of me right now.",
            "I don't have access to Walmart's live financial data or future earnings dates, so I can't tell you when the next turning point is.",
        ] {
            assert!(sounds_like_refusal(r), "missed a real refusal: {r}");
        }
    }

    #[test]
    fn a_fact_about_the_world_is_not_a_refusal() {
        // These are answers, not dead ends. Escalating them would spend thirty seconds of someone's
        // time re-confirming something the mind already knew.
        for ok in [
            "Nifty's at 24,211, up a hair — 0.08% so far today.",
            "The market's closed right now, so that's yesterday's close.",
            "Walmart doesn't report until the 21st.",
            "You're flat — no open positions in the paper account.",
            "I checked and there's nothing new since this morning.",
        ] {
            assert!(!sounds_like_refusal(ok), "wrongly flagged an answer: {ok}");
        }
    }

    #[test]
    fn a_refusal_about_knowledge_rather_than_capability_still_counts() {
        // "I don't have that data" is a capability claim in disguise — the data is a tool call away.
        assert!(sounds_like_refusal("I don't have that data for this quarter."));
        assert!(sounds_like_refusal("I'm not able to reach that right now."));
    }
}
