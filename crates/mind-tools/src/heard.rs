//! HEARD — deciding whether the transcriber heard a PERSON, or heard the room.
//!
//! Whisper does not only transcribe speech. When there is nothing to transcribe it narrates the
//! audio instead, in brackets: `[BLANK_AUDIO]`, `[MUSIC PLAYING]`, `(metal clanging)`. Those are
//! notes ABOUT the recording, not words anyone said, and handing them to a conversation makes the
//! mind reply to them. From a real session:
//!
//! ```text
//!   you  : (metal clanging)
//!   mind : Sounds like you're at the workshop or garage — what's clanging?
//!   you  : [BLANK_AUDIO]
//!   mind : That was a blank audio clip — did you mean to say something?
//!   you  : [MUSIC PLAYING]
//!   mind : Sounds like you're listening to something — what's playing?
//! ```
//!
//! Three turns spent answering the transcriber's description of a room. Worse than wasted: each one
//! is stored as something the person SAID, so the memory of the conversation is now partly a memory
//! of clanging metal.
//!
//! ## The rule
//!
//! Strip every bracketed and parenthesised span. If nothing is left but punctuation, the microphone
//! caught noise and there is no turn to take. Silence is the correct response to silence.
//!
//! Annotations are stripped rather than the whole utterance dropped, because a real sentence can
//! carry one: "(door slams) sorry, what were you saying" is a person talking, and the words survive
//! while the stage direction does not.

/// What the transcriber produced, once the stage directions are removed.
pub fn spoken_words(transcript: &str) -> String {
    let mut out = String::with_capacity(transcript.len());
    let mut depth_sq = 0i32;
    let mut depth_par = 0i32;
    let mut depth_star = false;
    for c in transcript.chars() {
        match c {
            '[' => depth_sq += 1,
            ']' => depth_sq = (depth_sq - 1).max(0),
            '(' => depth_par += 1,
            ')' => depth_par = (depth_par - 1).max(0),
            // *laughs* / *sighs* are the same thing in a different costume.
            '*' => depth_star = !depth_star,
            _ if depth_sq == 0 && depth_par == 0 && !depth_star => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ").trim().to_string()
}

/// Did a person actually say something?
///
/// A bare "no" or "stop" is a real and important turn, so the test is whether any WORD survives —
/// never a length threshold. Requiring two words would discard exactly the interruptions that
/// matter most.
pub fn is_speech(transcript: &str) -> bool {
    let words = spoken_words(transcript);
    words.chars().any(|c| c.is_alphanumeric())
}

/// Utterances whose whole content is a transcriber artefact, even without brackets.
const BARE_ARTEFACTS: &[&str] = &["blank_audio", "silence", "inaudible", "music", "applause", "no speech"];

/// A last check for artefacts that arrive unbracketed.
pub fn is_artefact(transcript: &str) -> bool {
    let w = spoken_words(transcript).to_lowercase();
    let w = w.trim_matches(|c: char| !c.is_alphanumeric()).to_string();
    BARE_ARTEFACTS.contains(&w.as_str())
}

/// The turn to take, or None if the microphone heard the room rather than a person.
pub fn as_turn(transcript: &str) -> Option<String> {
    let words = spoken_words(transcript);
    if !is_speech(&words) || is_artefact(&words) {
        return None;
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_that_actually_happened_are_not_turns() {
        // Verbatim from a live session. Each of these was answered as if it were the person talking.
        assert_eq!(as_turn("(metal clanging)"), None);
        assert_eq!(as_turn("[BLANK_AUDIO]"), None);
        assert_eq!(as_turn("[MUSIC PLAYING]"), None);
    }

    #[test]
    fn the_usual_suspects_are_caught_too() {
        for noise in ["[SILENCE]", "( upbeat music )", "*laughs*", "[ Inaudible ]", "[APPLAUSE]", "   ", "..."] {
            assert_eq!(as_turn(noise), None, "should not be a turn: {noise:?}");
        }
    }

    #[test]
    fn a_real_sentence_survives_a_stage_direction_inside_it() {
        // The annotation is stripped, not the sentence. Dropping the whole utterance would lose a
        // person speaking over a noise, which is exactly when they are most likely to be interrupting.
        assert_eq!(as_turn("(door slams) sorry, what were you saying?").as_deref(), Some("sorry, what were you saying?"));
        assert_eq!(as_turn("what's the nifty at [BLANK_AUDIO]").as_deref(), Some("what's the nifty at"));
    }

    #[test]
    fn a_single_word_is_a_real_turn() {
        // "no" and "stop" are the most important things a person says to a machine that is talking.
        // A length threshold would discard precisely those.
        assert_eq!(as_turn("No.").as_deref(), Some("No."));
        assert_eq!(as_turn("stop").as_deref(), Some("stop"));
        assert_eq!(as_turn("sure").as_deref(), Some("sure"));
    }

    #[test]
    fn ordinary_speech_is_untouched() {
        let s = "what is the nifty at right now";
        assert_eq!(as_turn(s).as_deref(), Some(s));
    }
}
