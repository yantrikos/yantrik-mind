//! BANTER — what to say while the answer is still being fetched.
//!
//! A spoken conversation has a property a chat window does not: silence is not neutral. Four seconds
//! of nothing while a quote is pulled reads as a hang, so the person repeats themselves, and now two
//! turns are in flight. Filling that gap is not decoration — it is what makes voice usable at all.
//!
//! But filler is also the easiest place in this whole system to lie, and the lie would be a
//! *pleasant* one. The model is mid-fetch, it knows what the question was, and the most natural
//! thing to say while waiting is a guess at the answer: "looks like it's up today". That sentence
//! costs nothing to produce, sounds attentive, and is a fabrication about the exact fact being
//! looked up — spoken aloud, in a medium where nobody can scroll back.
//!
//! ## The rule
//!
//! **A filler may talk about the PAST or the PROCESS. It may never talk about the PENDING ANSWER.**
//!
//! "Pulling that up now" is about the process. "You asked me this on Tuesday" is about the past.
//! "Should be up on the day" is about the answer, and is forbidden even when it later turns out to
//! be correct — a guess that happens to be right is still a guess, and the listener cannot tell the
//! difference between the two, which is precisely the problem.
//!
//! ## Why not generate it with the model
//!
//! Because the point is to cover a wait, and a 27B model takes seconds to answer — the filler would
//! need its own filler. These are composed from material already in hand (what tool is running, what
//! the memory actually holds) at microsecond cost. That constraint is also a safety property: text
//! assembled from real rows cannot hallucinate a market move.

use serde::{Deserialize, Serialize};

/// What kind of thing the mind is saying while it waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// About the work in flight: "still pulling that".
    Process,
    /// About something real from memory: "you asked me this on Tuesday".
    Recall,
    /// A remark with no factual payload at all.
    Aside,
}

/// One thing that could be said into a gap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filler {
    pub text: String,
    pub kind: Kind,
}

/// Words that make a sentence a claim about a market or a value. If a filler contains one of these
/// it is asserting something about the pending answer, which is the one thing filler may never do.
///
/// Deliberately over-broad. A false positive costs one unspoken pleasantry; a false negative is the
/// mind guessing a price out loud, in a medium with no scrollback, in a confident voice.
const CLAIMY: &[&str] = &[
    " up ", " down ", "higher", "lower", "rising", "falling", "rally", "selling off", "green",
    "red", "gain", "loss", "beat", "miss", "should be", "probably", "looks like", "i think it",
    "my guess", "likely", "seems to be", "must be", "bet it", "expect it",
];

/// Would speaking this while the answer is unknown assert something about that answer?
pub fn asserts_the_pending_answer(text: &str) -> bool {
    let t = format!(" {} ", text.to_lowercase().replace(['.', ',', '!', '?'], " "));
    CLAIMY.iter().any(|c| t.contains(c))
}

/// Is this safe to say into the gap?
///
/// A Recall filler is held to the same rule as any other. "You said last week it would bounce" is
/// about the past AND about the pending answer, and speaking it plants a number in the listener's
/// head that the mind has not verified.
pub fn is_speakable(f: &Filler) -> bool {
    !f.text.trim().is_empty() && !asserts_the_pending_answer(&f.text)
}

/// How long to wait before saying anything at all.
///
/// A person does not narrate a half-second pause, and a mind that fills every gap sounds anxious.
/// Below this, silence is simply the natural rhythm of a conversation.
pub const QUIET_GRACE_MS: u64 = 1200;

/// Never fill more often than this. Two fillers in quick succession is not company, it is a machine
/// covering for itself.
pub const MIN_GAP_MS: u64 = 4000;

/// Should the mind say something now?
pub fn should_speak(elapsed_ms: u64, since_last_filler_ms: Option<u64>) -> bool {
    if elapsed_ms < QUIET_GRACE_MS {
        return false;
    }
    match since_last_filler_ms {
        Some(ms) => ms >= MIN_GAP_MS,
        None => true,
    }
}

/// Process fillers phrased from the tool actually running, so the words are true by construction.
pub fn process_filler(tool: &str) -> Filler {
    let text = match tool {
        "quote" | "price" => "Pulling the live price now.",
        "watch" | "copy_trade" => "Watching a bit of it — this takes a moment.",
        "hunt" | "surf" => "Scanning what is moving.",
        "browse" => "Opening the page.",
        "recall" | "sources" => "Checking what I have on that.",
        _ => "Working on it.",
    };
    Filler { text: text.to_string(), kind: Kind::Process }
}

/// A recall filler built from a memory row. Returns None when the row would assert something about
/// the pending answer, because a memory ABOUT the subject is exactly the tempting, forbidden case.
pub fn recall_filler(memory_line: &str) -> Option<Filler> {
    let line = memory_line.trim();
    if line.len() < 12 {
        return None;
    }
    let f = Filler { text: format!("While that runs — {line}"), kind: Kind::Recall };
    is_speakable(&f).then_some(f)
}

/// Pick the next filler without repeating one already used this turn.
///
/// Repetition is what makes a voice assistant unbearable: the same three phrases on rotation stop
/// being conversation and start being a progress bar that talks.
pub fn next_filler<'a>(candidates: &'a [Filler], used: &[String]) -> Option<&'a Filler> {
    candidates
        .iter()
        .find(|f| is_speakable(f) && !used.iter().any(|u| u.eq_ignore_ascii_case(&f.text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guess_at_the_pending_answer_is_never_spoken() {
        // THE failure this module exists to prevent. Each of these is what a helpful assistant says
        // while waiting, and each is a fabrication about the very number being fetched — delivered
        // aloud, where there is no scrollback to check it against.
        for bad in [
            "Looks like it's up today.",
            "Should be higher after that news.",
            "Probably a small loss on the day.",
            "I think it rallied this morning.",
            "Seems to be selling off.",
        ] {
            assert!(asserts_the_pending_answer(bad), "must be caught: {bad}");
            assert!(!is_speakable(&Filler { text: bad.into(), kind: Kind::Aside }), "{bad}");
        }
    }

    #[test]
    fn talking_about_the_process_or_the_past_is_fine() {
        for good in [
            "Pulling the live price now.",
            "Still fetching — the feed is slow today.",
            "You asked me this on Tuesday as well.",
            "Give me a second, the stream takes a moment to attach.",
        ] {
            assert!(!asserts_the_pending_answer(good), "wrongly blocked: {good}");
            assert!(is_speakable(&Filler { text: good.into(), kind: Kind::Process }));
        }
    }

    #[test]
    fn a_memory_that_prejudges_the_answer_is_refused_even_though_it_is_true() {
        // The subtlest case. This memory is real, and saying it aloud while the quote is still in
        // flight plants a number the mind has not verified — the listener hears a forecast.
        assert_eq!(recall_filler("you said last week it would probably bounce off 24,000"), None);
        // Whereas a genuine, answer-neutral callback is good company.
        let ok = recall_filler("you were tracking this one on Tuesday too").unwrap();
        assert_eq!(ok.kind, Kind::Recall);
        assert!(ok.text.contains("Tuesday"));
    }

    #[test]
    fn silence_is_allowed_and_chatter_is_rationed() {
        // A person does not narrate a half-second pause.
        assert!(!should_speak(300, None), "a short gap needs no filling");
        assert!(should_speak(2_000, None));
        // And having just spoken, it waits.
        assert!(!should_speak(5_000, Some(1_000)), "two fillers in a row is a machine covering itself");
        assert!(should_speak(9_000, Some(5_000)));
    }

    #[test]
    fn the_same_line_is_not_repeated_in_one_turn() {
        let cands = vec![
            Filler { text: "Pulling the live price now.".into(), kind: Kind::Process },
            Filler { text: "Still going — the feed is slow today.".into(), kind: Kind::Process },
        ];
        let used = vec!["Pulling the live price now.".to_string()];
        let n = next_filler(&cands, &used).unwrap();
        assert_eq!(n.text, "Still going — the feed is slow today.");
    }

    #[test]
    fn process_filler_describes_the_tool_that_is_actually_running() {
        // True by construction: the sentence is chosen FROM the running tool, so it cannot describe
        // work that is not happening.
        assert!(process_filler("quote").text.contains("price"));
        assert!(process_filler("watch").text.contains("Watching"));
        assert!(!asserts_the_pending_answer(&process_filler("hunt").text));
    }
}
