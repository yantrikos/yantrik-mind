//! E.VOICE2 — the answer, streamed to the desktop while the model writes it.
//!
//! The rule: NOTHING IS SENT THAT THE DELIVERED ANSWER MIGHT NOT CONTAIN. A spoken sentence cannot
//! be taken back and the Lens appends chunks, while the agent loop's checks can still throw a
//! direct reply away (a nudge asks again), replace it (HTML becomes a page link) or read it as a
//! tool call (JSON). So a step's words are forwarded only when its pre-step state rules every
//! retracting check out, only in whole sentences, and never past the first `{` or `<`. The last,
//! unfinished sentence is never sent from here: it arrives with the answer.

use tokio::sync::mpsc::UnboundedSender;

/// What the harness receives while a turn runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerDelta {
    /// A model step begins; what the previous step sent stays sent.
    Step,
    /// Committed words of this step: whole sentences, credential-masked like the answer.
    Text(String),
}

tokio::task_local! {
    /// Set by a surface that shows the answer as it is written (the desktop harness).
    pub static TURN_ANSWER: UnboundedSender<AnswerDelta>;
}

/// `YM_STREAM_ANSWER=off` restores the old path: one plain call per step, the answer sent whole.
fn switched_off() -> bool {
    std::env::var("YM_STREAM_ANSWER").is_ok_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

/// Whether a step's words may be forwarded: none of the checks that discard or replace a direct
/// reply can fire on it. Each input is what that check reads, known before the step runs.
pub(crate) fn forward_allowed(unsaved_doc: bool, saved_any: bool, goal_asked: bool, unattempted: bool) -> bool {
    !unsaved_doc && !saved_any && !goal_asked && !unattempted
}

/// Tokens in, committed segments out.
#[derive(Default)]
pub(crate) struct AnswerCommitter {
    pending: String,
    started: bool,
    closed: bool,
}

impl AnswerCommitter {
    pub(crate) fn push(&mut self, tok: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.closed {
            return out;
        }
        for c in tok.chars() {
            // JSON is read as a tool call or an `answer`; `<` opens HTML or a reasoning tag.
            if c == '{' || c == '<' {
                self.closed = true;
                self.pending.clear();
                return out;
            }
            // The answer is trimmed, so its first word is the first thing sent.
            if !self.started {
                if c.is_whitespace() {
                    continue;
                }
                self.started = true;
            }
            if c == '\n' || (c.is_whitespace() && self.pending.ends_with(['.', '!', '?', '…'])) {
                self.commit(&mut out);
            }
            self.pending.push(c);
        }
        out
    }

    /// Send what is pending up to its last non-space character; the whitespace stays to begin the
    /// next segment, so segments split only where the answer has whitespace, and the joined
    /// segments are a prefix of the trimmed answer (and of its credential-masked form).
    fn commit(&mut self, out: &mut Vec<String>) {
        let body = self.pending.trim_end().len();
        if self.pending[..body].trim().is_empty() {
            return;
        }
        out.push(crate::redact::redact_answer(&self.pending[..body]));
        self.pending.drain(..body);
    }
}

/// Run one agent-loop step. On a surface that streams, the step's model call streams (so a cancel
/// can stop it mid-call) and, when `forward`, its committed sentences go to the surface; elsewhere
/// it is `f` unchanged.
pub(crate) async fn streamed_step<F: std::future::Future>(forward: bool, f: F) -> F::Output {
    let Ok(out) = TURN_ANSWER.try_with(|t| t.clone()) else {
        return f.await;
    };
    if switched_off() {
        return f.await;
    }
    let _ = out.send(AnswerDelta::Step);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let drain = tokio::spawn(async move {
        let mut c = AnswerCommitter::default();
        while let Some(t) = rx.recv().await {
            if forward {
                for s in c.push(&t) {
                    let _ = out.send(AnswerDelta::Text(s));
                }
            }
        }
    });
    let r = mind_inference::with_step_tokens(tx, f).await;
    // Every sender is gone with the call, so this ends once the last token is through: what
    // the step committed is queued before the step's answer exists.
    let _ = drain.await;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(tokens: &[&str]) -> Vec<String> {
        let mut c = AnswerCommitter::default();
        tokens.iter().flat_map(|t| c.push(t)).collect()
    }

    #[test]
    fn sentences_and_lines_commit_and_the_unfinished_tail_does_not() {
        assert_eq!(run(&["Pasta is ", "quick. Rice", " works too! Or ", "soup."]), vec!["Pasta is quick.", " Rice works too!"]);
        assert_eq!(run(&["Here is the plan:\n- Monday", "\n- Tuesday"]), vec!["Here is the plan:", "\n- Monday"]);
        assert_eq!(run(&["It costs 3.50 today"]), Vec::<String>::new(), "a decimal point is not a sentence end");
    }

    #[test]
    fn leading_whitespace_is_skipped_and_trailing_whitespace_is_held() {
        assert_eq!(run(&["  \n Hi.   ", "There.\n\n", "More"]), vec!["Hi.", "   There."]);
        assert_eq!(run(&["Last line  \n"]), vec!["Last line"], "spaces before the newline wait for what follows");
    }

    #[test]
    fn nothing_is_committed_after_the_first_brace_or_angle_bracket() {
        assert_eq!(run(&["Sure. ", "{\"answer\": \"x. y. \"}", " More. Words. "]), vec!["Sure."]);
        assert_eq!(run(&["Done. ", "<table><tr>. ", "a. b. "]), vec!["Done."]);
        assert_eq!(run(&["<think>plan. go. </think>Hi. there. "]), Vec::<String>::new());
    }

    /// The property the harness relies on: joined, the segments are a prefix of the answer as it
    /// is delivered (trimmed, credentials masked), whatever the chunking.
    #[test]
    fn the_joined_segments_are_a_prefix_of_the_delivered_answer() {
        let corpus = [
            "  Your key is sk-proj-abcdefghijklmnop1234. Keep it safe! Done.",
            "**Monday**: the dentist.\n- `Tuesday`: lunch with Asha.\n\nThat is all.  ",
            "Tokens AKIAABCDEFGHIJKLMNOP and ghp_0123456789abcdefABCDEF. Rotate both? Yes.",
            "One.  Two!\tThree?\nFour… five",
        ];
        for text in corpus {
            let delivered = crate::redact::redact_answer(text.trim());
            for size in [1usize, 2, 3, 5, 8, 13, 64] {
                let chars: Vec<char> = text.chars().collect();
                let tokens: Vec<String> = chars.chunks(size).map(|c| c.iter().collect()).collect();
                let refs: Vec<&str> = tokens.iter().map(String::as_str).collect();
                let joined: String = run(&refs).concat();
                assert!(!joined.is_empty(), "{text:?} committed nothing at size {size}");
                assert!(delivered.starts_with(&joined), "size {size}: {joined:?} is not a prefix of {delivered:?}");
                assert!(!joined.contains("sk-proj-abcdefghijklmnop1234") && !joined.contains("AKIAABCDEFGHIJKLMNOP"), "a key was streamed: {joined:?}");
            }
        }
    }

    #[test]
    fn each_retracting_check_alone_stops_forwarding() {
        assert!(forward_allowed(false, false, false, false));
        assert!(!forward_allowed(true, false, false, false), "an unsaved document");
        assert!(!forward_allowed(false, true, false, false), "saved pages (dead-link nudge)");
        assert!(!forward_allowed(false, false, true, false), "a goal path in the request");
        assert!(!forward_allowed(false, false, false, true), "an unattempted side effect");
    }
}
