//! SAY — the mouth, held open for the life of the process.
//!
//! The synthesiser costs 1.8s to load and 200ms to speak, so where it lives decides whether the
//! thing can hold a conversation. A session started per turn pays the load every time and the mind
//! answers a second and a half late, every single turn, which reads as a fault rather than a pause.
//!
//! So the voice is a process-global, started on first use and kept. That is not a shortcut around
//! the engine's ownership — it is the honest model of the thing: the box has one speaker, and two
//! `VoiceSession`s would be two mouths talking over each other.
//!
//! ## Why speaking is separate from answering
//!
//! `speak_turn` runs the ordinary turn and then speaks the result, rather than being a special
//! voice-only path. The reply comes from the same loop, the same tools and the same memory as a
//! typed one; only the RENDERING differs, declared through `TurnIdentity::speaking`. A parallel
//! voice pipeline would drift — it would gain its own prompt, then its own tool set, and eventually
//! answer differently to the same question depending on whether it was typed or spoken.

use std::sync::{Arc, OnceLock};

use mind_tools::voice::{Spoken, VoiceSession};

static VOICE: OnceLock<Option<Arc<VoiceSession>>> = OnceLock::new();

/// The process's voice, started once. None when no synthesiser is installed — in which case the
/// mind says so plainly rather than pretending to have spoken.
pub fn voice() -> Option<Arc<VoiceSession>> {
    VOICE
        .get_or_init(|| match VoiceSession::start() {
            Ok(v) => Some(Arc::new(v)),
            Err(_) => None,
        })
        .clone()
}

impl super::ConversationEngine {
    /// Render text as speech without running a turn. `ym say <text>`.
    pub async fn say_aloud(&self, text: &str) -> String {
        let Some(v) = voice() else {
            return "I have no voice on this host — the synthesiser is not installed.".to_string();
        };
        let spoken = mind_tools::speech::to_spoken(text);
        let t = spoken.clone();
        let res = tokio::task::spawn_blocking(move || {
            let turn = v.begin_turn();
            let mut total = 0.0f64;
            let mut chunks = 0usize;
            let said = v.speak_reply(&t, turn, |s: &Spoken| {
                total += s.secs;
                chunks += 1;
            });
            (said, total, chunks)
        })
        .await;
        match res {
            Ok((said, secs, chunks)) => format!(
                "🔊 spoke {chunks} chunk(s), {secs:.1}s of audio\n   {}",
                said.chars().take(160).collect::<String>()
            ),
            Err(e) => format!("the voice task failed: {e}"),
        }
    }

    /// A full spoken turn: answer as always, then say it.
    ///
    /// The identity carries `speaking`, so the SAME loop composes for the ear — short, answer-first,
    /// no markup — instead of a written reply being flattened afterwards. Flattening produces a
    /// briefing with its bullets removed; asking for speech produces speech.
    pub async fn speak_turn(self: &Arc<Self>, user_text: &str, person: &str) -> String {
        let id = super::TurnIdentity::new(person.to_string(), false).speaking(true);
        let answer = match self.turn(user_text, id).await {
            Ok(a) => a,
            Err(e) => format!("Something went wrong there: {e}"),
        };
        // The shape is checked against the flow rules and REPORTED, never silently rewritten. A
        // second model pass to "fix" a long answer costs another second of the listener's time and
        // sometimes changes the facts; naming the fault is how the register gets better instead.
        let faults = mind_tools::flow::faults(&answer, 1);
        let spoken = self.say_aloud(&answer).await;
        if faults.is_empty() {
            spoken
        } else {
            format!("{spoken}\n   (shape: {})", faults.join("; "))
        }
    }

    /// A hold line, ready instantly — what to play while the answer is still coming.
    pub fn hold_line(&self, nth: usize) -> Option<(String, Vec<u8>)> {
        voice().and_then(|v| v.hold(nth))
    }

    /// Stop talking now.
    pub fn interrupt_speech(&self) {
        if let Some(v) = voice() {
            v.interrupt();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_spoken_turn_declares_the_channel_rather_than_flattening_afterwards() {
        // The register has to reach the COMPOSER. Rewriting a written answer afterwards yields a
        // briefing with its bullets stripped — the sentences are still built for a reader.
        let id = crate::TurnIdentity::new("primary", false).speaking(true);
        assert!(id.voice);
        assert!(!id.rich, "a listener cannot see a table, so the licence is withdrawn");
        let note = id.format_note().expect("a spoken turn carries an instruction");
        assert!(note.contains("read aloud"));
        assert!(note.contains("FIRST sentence"));
        assert!(note.contains("Most turns should simply stop"));
    }

    #[test]
    fn speech_and_rendering_can_never_both_be_in_force() {
        // They are contradictory instructions — one grants tables and fenced code, the other says
        // none of it can be seen. A model handed both reads its own markup aloud.
        let id = crate::TurnIdentity::new("primary", false).rendering_rich(true).speaking(true);
        assert!(!id.rich);
        assert!(id.format_note().unwrap().contains("SPOKEN CHANNEL"));
    }
}
