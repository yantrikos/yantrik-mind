//! FLOW — the shape of a spoken turn, which is mostly restraint.
//!
//! Measured on this box, so the constraint is real rather than assumed: the model's FIRST token
//! arrives at ~1000ms and a full short reply at ~3600ms. Whisper adds ~300ms after the speaker
//! stops, synthesis ~200ms. So the natural gap before any sound is one and a half to two seconds —
//! and a phone call tolerates about one. That single fact drives everything here.
//!
//! ## Cover the gap the way a person does
//!
//! A person says "mm" or "hang on" while they think, and it costs them nothing because they are not
//! composing it. The equivalent is a PRE-RENDERED clip: synthesise a small set at boot and the wait
//! is covered in ~0ms instead of 200. Generating filler on demand would put the filler behind the
//! same queue as the answer, which is the one place it must never be.
//!
//! ## The habit that ruins a phone call
//!
//! The mind's real messages end like this: "Want me to attempt the fetch now, or do you have the
//! numbers in front of you?" — every single time — and open with an agenda nobody asked for: "Two
//! things I'm carrying for you right now". In a chat window that is thorough. Spoken, it means every
//! answer arrives wrapped in a menu, and the person has to wait through the wrapper to hear the
//! thing they asked. A friend answers the question and stops.
//!
//! So: answer first, keep it to a breath, and offer something only occasionally. Not never — an
//! offer is useful sometimes — but a closing question EVERY turn stops being helpfulness and becomes
//! a tic that has to be talked over.

/// Roughly how many spoken words fit in a second.
pub const WORDS_PER_SEC: f64 = 2.6;

/// Longest a single spoken turn should run before it becomes a monologue. Twelve seconds is about
/// thirty words — a couple of sentences. Past this on a phone, the listener stops tracking and
/// starts waiting for a gap to interrupt.
pub const MAX_TURN_SECS: f64 = 12.0;

/// A closing offer is welcome sometimes and grating every time. One turn in four.
pub const OFFER_EVERY_N_TURNS: u32 = 4;

/// How long the listener will sit in silence before the exchange feels broken.
pub const PATIENCE_MS: u64 = 900;
const _: () = assert!(PATIENCE_MS < 1000);

/// Estimated seconds of speech.
pub fn spoken_secs(text: &str) -> f64 {
    text.split_whitespace().count() as f64 / WORDS_PER_SEC
}

/// Is this turn short enough to say to someone?
pub fn is_a_breath(text: &str) -> bool {
    spoken_secs(text) <= MAX_TURN_SECS
}

/// Does the reply end by handing the conversation back with a question or an offer?
pub fn ends_with_an_offer(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    let tail: String = t
        .chars()
        .rev()
        .take(140)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    // The list is what real replies actually end with, extended each time one slips through. It
    // missed "Want to start there?" on the very first live sample — the checker was written from
    // the forms I remembered rather than the forms the mind uses, which is the same mistake as
    // testing a filter against invented data.
    tail.contains("want me to")
        || tail.contains("want to start")
        || tail.contains("shall i")
        || tail.contains("would you like")
        || tail.contains("do you want")
        || tail.contains("should i")
        || tail.contains("let me know")
        || tail.contains("if you want")
        // Any trailing question that hands the turn back. A genuine clarifying question is rare;
        // a habitual sign-off question is what this exists to catch, and both end the same way.
        || tail.trim_end().ends_with('?')
}

/// Does it open by restating the question or announcing an agenda?
///
/// "Two things I'm carrying for you right now" is a header. Spoken, the listener waits through it to
/// find out whether any of it is the answer they asked for.
pub fn opens_with_preamble(text: &str) -> bool {
    let head = text.trim().to_lowercase();
    let head: String = head.chars().take(80).collect();
    [
        "two things",
        "three things",
        "here's the state",
        "here is the state",
        "a few things",
        "quick update",
        "one thing i should",
        "so, to answer",
        "great question",
        "let me break",
    ]
    .iter()
    .any(|p| head.contains(p))
}

/// Should this turn be allowed to end with an offer?
pub fn may_offer(turn_index: u32) -> bool {
    turn_index > 0 && turn_index.is_multiple_of(OFFER_EVERY_N_TURNS)
}

/// What is wrong with this turn, spoken aloud. Empty means it is fine to say.
pub fn faults(text: &str, turn_index: u32) -> Vec<&'static str> {
    let mut f = Vec::new();
    if !is_a_breath(text) {
        f.push("too long to say in one turn — answer, then stop");
    }
    if opens_with_preamble(text) {
        f.push("opens with an agenda instead of the answer");
    }
    if ends_with_an_offer(text) && !may_offer(turn_index) {
        f.push("ends with an offer again — not every turn");
    }
    f
}

/// The instruction that produces a turn like this, given to the model when it will be heard.
pub const FLOW_RULES: &str =
    "This is a spoken conversation, so answer like a person on the phone. \
Give the answer in the first sentence. Keep the whole turn to a couple of sentences — if there is \
more, say the important part and let them ask. Do not open with an agenda or a summary of what you \
are carrying. Do not end every turn by offering to do something; most turns should simply end. Use \
'it' and 'that' for things already mentioned instead of naming them again.";

/// Pre-rendered lines, spoken while the answer is still coming.
///
/// Short and content-free by design. These are synthesised at boot so the gap costs ~0ms, and they
/// must be safe to say before anything is known — which rules out anything about the answer.
pub const HOLD_LINES: &[&str] = &[
    "Mm.",
    "One sec.",
    "Hang on.",
    "Let me look.",
    "Right, checking.",
    "Give me a moment.",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The mind's real Telegram reply, verbatim.
    const REAL_REPLY: &str = "I don't trade. I have no positions, no broker, and no capital — so \
\"trading skills\" don't apply to me. What I do have is the discipline we built: I only give you \
measured data, never a guess, and I flag when I can't see the number. That said, I know you're the \
one trading — day/swing, and you've been pulling Nifty 50 and RELIANCE quotes. You've still got a \
sparse belief on that (0.68) and no recorded strategy on file. If you want me to be genuinely useful \
here, give me your primary trading rule — the one thing that decides when you enter or exit — and \
I'll anchor my alerts and context around it instead of guessing. Want to drop your core trading rule \
now, or shall I re-pull a live RELIANCE quote first?";

    #[test]
    fn the_minds_real_reply_would_be_a_monologue_on_a_phone() {
        // Excellent in a chat window. Spoken, it runs most of a minute before the listener learns
        // whether any of it was the answer.
        let secs = spoken_secs(REAL_REPLY);
        assert!(secs > 30.0, "that reply is {secs:.0}s of talking");
        assert!(!is_a_breath(REAL_REPLY));
        let f = faults(REAL_REPLY, 1);
        assert!(f.iter().any(|x| x.contains("too long")), "{f:?}");
        assert!(
            f.iter().any(|x| x.contains("offer")),
            "it ends with a two-part offer: {f:?}"
        );
    }

    #[test]
    fn the_sign_off_question_that_slipped_through_is_caught() {
        // From the first live voice reply. The checker knew "want me to" and "do you want" and had
        // never met "Want to start there?" — written from the forms I remembered instead of the
        // forms the mind actually uses.
        assert!(ends_with_an_offer("If you give me your one entry rule, I can frame the first test trade. Want to start there?"));
        assert!(ends_with_an_offer(
            "It's flat. Let me know if you want Reliance too."
        ));
        // A turn that simply stops is the target.
        assert!(!ends_with_an_offer(
            "Twenty four thousand and fifty three, down about a quarter percent."
        ));
    }

    #[test]
    fn an_agenda_opening_is_caught() {
        // Real opening from the same conversation.
        assert!(opens_with_preamble(
            "Two things I'm carrying for you right now:"
        ));
        assert!(opens_with_preamble("Here's the state: nothing changed."));
        // A plain answer is not a preamble.
        assert!(!opens_with_preamble(
            "It's at twenty four thousand fifty three, down a bit."
        ));
    }

    #[test]
    fn a_short_answer_that_just_stops_is_the_target() {
        let good = "Twenty four thousand and fifty three, down about a quarter percent.";
        assert!(is_a_breath(good));
        assert!(faults(good, 1).is_empty(), "{:?}", faults(good, 1));
    }

    #[test]
    fn offering_is_rationed_not_banned() {
        // Sometimes an offer is the right thing; every single turn is a tic.
        assert!(!may_offer(1));
        assert!(!may_offer(3));
        assert!(may_offer(4));
        let with_offer = "It's flat. Want me to pull Reliance too?";
        assert!(ends_with_an_offer(with_offer));
        assert!(
            faults(with_offer, 4).is_empty(),
            "allowed on the fourth turn"
        );
        assert!(!faults(with_offer, 5).is_empty(), "not on the fifth");
    }

    #[test]
    fn hold_lines_say_nothing_about_the_answer() {
        // They are spoken BEFORE anything is known, so any content would be invention. They are also
        // pre-rendered, so they cost no time when the wait is already the problem.
        for l in HOLD_LINES {
            assert!(
                l.split_whitespace().count() <= 4,
                "a hold line is short: {l}"
            );
            assert!(!crate::banter::asserts_the_pending_answer(l), "{l}");
        }
    }
}
