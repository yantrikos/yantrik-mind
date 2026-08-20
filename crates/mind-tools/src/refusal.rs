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

/// Phrases in which the mind promises to do the work rather than doing it.
///
/// Catching refusals made the model switch to deferring instead: "I can pull Walmart's earnings for
/// you, just give me a moment to access that data." On a path with no tools there IS no moment —
/// nothing happens after the reply is sent, so the person waits for something that will never
/// arrive. It is the same dead end in a friendlier voice, and worse than a refusal, because a
/// refusal at least tells them to look elsewhere.
const PROMISE_MARKS: &[&str] = &[
    "give me a moment",
    "give me a sec",
    "just a moment",
    "one moment",
    "let me pull",
    "let me grab",
    "let me check",
    "let me look",
    "let me fetch",
    "i'll pull",
    "i'll grab",
    "i'll check",
    "i'll look",
    "i'll fetch",
    "i will pull",
    "i will check",
    "pulling that",
    "pulling the",
    "fetching",
    "hang on while",
    "bear with me",
    "coming right up",
    "on it",
];

/// Is this reply deferring the work instead of doing it?
pub fn sounds_like_promise(reply: &str) -> bool {
    let r = reply.to_lowercase();
    PROMISE_MARKS.iter().any(|m| r.contains(m))
}

/// A reply that does not actually answer: it either refuses, or promises to answer later.
///
/// Both end the same way for the person — nothing they asked for — so both must be caught before
/// the reply is delivered.
pub fn is_a_dead_end(reply: &str) -> bool {
    sounds_like_refusal(reply) || sounds_like_promise(reply)
}

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
    fn a_promise_is_a_dead_end_too() {
        // What the model said the moment refusals were caught. There is no "moment" — nothing runs
        // after the reply is sent.
        for p in [
            "I can pull Walmart's latest earnings and debt figures for you, just give me a moment to access that data.",
            "Pulling the Nifty 50 and Sensex now. Give me a second to grab those quotes.",
            "Sure, let me check that for you.",
            "I'll grab those numbers now.",
        ] {
            assert!(sounds_like_promise(p), "missed a promise: {p}");
            assert!(is_a_dead_end(p), "{p}");
        }
    }

    #[test]
    fn an_answer_that_happens_to_contain_a_verb_is_not_a_promise() {
        // "I checked" is done; "I'll check" is not. The tense is the whole difference.
        assert!(!is_a_dead_end("I checked and there's nothing new since this morning."));
        assert!(!is_a_dead_end("Nifty's at 24,211, up a hair — 0.08% so far today."));
        assert!(!is_a_dead_end("Walmart doesn't report until the 21st."));
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
