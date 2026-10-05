//! E.ERASE4 — the Mind's side of the desktop's `redact` (yantrik-os #649, docs/harness.md
//! "Forgetting: `redact`").
//!
//! When the person asks the Mind to forget a text, the Mind erases its own memory; the desktop keeps
//! copies of its own (the pane transcript, the run store), and `redact` asks it to erase those too.
//! Only digests travel. The desktop applies one only for a Keep/Erase question the person PRESSED
//! Erase on, and only for a text that is one whole double-quoted span of that question as shown --
//! so the question quotes the text, and the needle is exactly that quote.
//!
//! Quoting the text reverses E.ERASE1/2's "never repeat it back". That is safe only because an
//! accepted redact also erases the question's own prompt (4c, #649).

use sha2::{Digest, Sha256};

/// The desktop's floor and ceiling on a needle's length, in scalars after canon.
pub(crate) const NEEDLE_MIN: usize = 4;
pub(crate) const NEEDLE_MAX: usize = 4096;
/// How much of a question's prompt the person is shown: this many characters, or the 1999 before
/// the card's ellipsis when it is longer. A quote must close inside it.
pub(crate) const QUESTION_CHARS: usize = 2000;

/// The form texts are hashed and matched in: NFC, then Unicode default lowercasing -- in that order.
pub(crate) fn canon(text: &str) -> String {
    icu_normalizer::ComposingNormalizerBorrowed::new_nfc().normalize(text).to_lowercase()
}

/// A needle: the SHA-256 (lowercase hex) of the canonical form as UTF-8, and its length in scalars
/// counted after lowercasing ("İ" becomes two).
pub(crate) fn needle(text: &str) -> (String, usize) {
    let c = canon(text);
    let hex = Sha256::digest(c.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    (hex, c.chars().count())
}

/// The quoted spans of a question as the person is shown it, each in canonical form -- the
/// desktop's rule, held to the shared fixture `redact_spans.json`:
/// - only double quotes delimit; a straight `"` opens only at the start or after whitespace or one
///   of `( [ {`, and closes at the NEXT `"` only if that one is followed by whitespace, one of
///   `. , ; : ! ? ) ] }`, or the end; inside it, “ and ” are ordinary;
/// - “ opens and ” closes, at the next ”; backwards is never a span; inside, `"` is ordinary;
/// - a span whose text starts or ends with whitespace is no span;
/// - left to right, no nesting; where an opener makes no span the scan goes on after it.
pub(crate) fn quoted_spans(question: &str) -> Vec<String> {
    let all: Vec<char> = question.chars().collect();
    let shown: &[char] = if all.len() <= QUESTION_CHARS { &all } else { &all[..QUESTION_CHARS - 1] };
    let mut spans = Vec::new();
    let mut i = 0;
    while i < shown.len() {
        let c = shown[i];
        let close = if c == '"' && (i == 0 || shown[i - 1].is_whitespace() || matches!(shown[i - 1], '(' | '[' | '{')) {
            Some('"')
        } else if c == '\u{201C}' {
            Some('\u{201D}')
        } else {
            None
        };
        if let Some(close) = close {
            if let Some(j) = shown[i + 1..].iter().position(|&x| x == close).map(|p| i + 1 + p) {
                let after = shown.get(j + 1).copied();
                let closes = close != '"'
                    || after.is_none_or(|a| a.is_whitespace() || matches!(a, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}'));
                let text = &shown[i + 1..j];
                let trimmed = text.is_empty() || (!text[0].is_whitespace() && !text[text.len() - 1].is_whitespace());
                if closes && trimmed {
                    spans.push(canon(&text.iter().collect::<String>()));
                    i = j + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    spans
}

/// The Keep/Erase question for `text` (the forget tool's `what`, trimmed), and the needle to send
/// on Erase -- None when the desktop could not be asked to erase it: too short or too long once
/// canonical, holding both kinds of double quote, or a quote that would not close where the person
/// can see it. Then the question does not quote it, and only the Mind's memory is erased.
///
/// `in_conversation`: the person put these words in THIS conversation themselves (the redact review's
/// note 2). Otherwise quoting would ADD a copy -- in the card, the run store and the pane -- of text the
/// Mind recalled, which stays there on Keep, no answer or a refusal; so it is not quoted.
pub(crate) fn forget_question(text: &str, places: usize, asked_at: &str, in_conversation: bool) -> (String, Option<(String, usize)>) {
    if !in_conversation {
        return (
            format!("Erase the text you asked me to forget? It is in {places} place(s) in my memory, and this can't be undone. (Asked at {asked_at}.)"),
            None,
        );
    }
    // The redact review's note 1: the desktop decides, so the question only says the Mind will ask.
    let tail = format!("It is in {places} place(s) in my memory. Erase removes it from my memory and asks the desktop to remove it from this conversation; this can't be undone. (Asked at {asked_at}.)");
    let quote = if !text.contains('"') {
        Some(('"', '"'))
    } else if !text.contains(['\u{201C}', '\u{201D}']) {
        Some(('\u{201C}', '\u{201D}'))
    } else {
        None
    };
    let (hex, len) = needle(text);
    let sendable = (NEEDLE_MIN..=NEEDLE_MAX).contains(&len) && text == text.trim();
    if let (Some((open, close)), true) = (quote, sendable) {
        let question = format!("Forget {open}{text}{close}? {tail}");
        // The Mind's own check: the desktop's parser must find exactly this text, and nothing else.
        if quoted_spans(&question) == vec![canon(text)] {
            return (question, Some((hex, len)));
        }
    }
    (
        format!("Erase the text you asked me to forget? It is in {places} place(s) in my memory, and this can't be undone. This conversation keeps its own copy. (Asked at {asked_at}.)"),
        None,
    )
}

/// Said after an Erase whose text could not be quoted for the desktop (see [`forget_question`]).
pub(crate) const NOT_QUOTABLE: &str =
    " This conversation still holds it: the text could not be quoted in the question, so the desktop could not be asked to erase it.";

/// What the person is told about this conversation's copy, from the desktop's answer to `redact`
/// (None: not sent, or no answer). Never more than the answer says.
pub(crate) fn conversation_outcome(reply: Option<&serde_json::Value>) -> String {
    match reply {
        Some(r) if r.get("redacted").and_then(|n| n.as_u64()).is_some() => {
            let n = r["redacted"].as_u64().unwrap_or(0);
            let mut s = format!(" It is also gone from this conversation ({n} place(s)); earlier conversations keep their words.");
            if let Some(w) = r.get("warning").and_then(|w| w.as_str()) {
                s.push_str(&format!(" The desktop noted: {w}."));
            }
            s
        }
        Some(r) => {
            let why = r
                .get("refused")
                .or_else(|| r.get("unsent"))
                .and_then(|w| w.as_str())
                .unwrap_or("the desktop gave no reason");
            format!(" This conversation still holds it: {why}.")
        }
        None => " This conversation still holds it: the desktop was not asked, or did not answer.".to_string(),
    }
}

/// E.ERASE4: a `redact` for the desktop, sent on the turn whose question the person pressed Erase on.
pub struct Redact {
    pub request_id: String,
    /// (sha256 hex, scalars) -- never the words.
    pub needles: Vec<(String, usize)>,
    pub reply: tokio::sync::oneshot::Sender<Option<serde_json::Value>>,
}

tokio::task_local! {
    /// E.ERASE4: set by a channel that can carry `redact` -- the desktop harness. Unset elsewhere.
    pub static TURN_REDACT: tokio::sync::mpsc::UnboundedSender<Redact>;
}

/// E.ERASE4: send a `redact` and wait for the desktop's answer. None when it could not be sent.
pub(crate) async fn send_redact(request_id: String, needles: Vec<(String, usize)>) -> Option<serde_json::Value> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    let sent = TURN_REDACT
        .try_with(|tx| tx.send(Redact { request_id, needles, reply }).is_ok())
        .unwrap_or(false);
    if !sent {
        return None;
    }
    answer.await.ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared fixtures, copied verbatim from yantrik-os 4f50b24a.
    #[test]
    fn needles_match_the_desktops() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!("../fixtures/redact/redact_needles_4f50b24a.json")).unwrap();
        assert_eq!(cases.len(), 5);
        for c in &cases {
            let (hex, len) = needle(c["text"].as_str().unwrap());
            assert_eq!(hex, c["sha256"].as_str().unwrap(), "{c}");
            assert_eq!(len as u64, c["len"].as_u64().unwrap(), "{c}");
        }
        // Where the order shows: "J" + caron has no composed capital, so NFC-then-lowercase keeps it
        // decomposed; lowercasing first would compose "ǰ" (U+01F0). Digest from Python's
        // `unicodedata.normalize('NFC', t).lower()`, as the desktop's reference computes it.
        assert_eq!(needle("J\u{30C}ones"), ("48f8994903a2b1f0b7083d0d3c0b7ebb05cbca9feb07ab5a5fc485f02abc109a".to_string(), 6));
    }

    #[test]
    fn spans_match_the_desktops() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!("../fixtures/redact/redact_spans_4f50b24a.json")).unwrap();
        assert_eq!(cases.len(), 18);
        for c in &cases {
            let want: Vec<String> = c["spans"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect();
            assert_eq!(quoted_spans(c["question"].as_str().unwrap()), want, "{}", c["case"]);
        }
    }

    /// The question quotes the text so the desktop's parser finds exactly it; the needle is its digest.
    #[test]
    fn the_question_quotes_exactly_the_text() {
        for text in ["throwaway-erase2", "don't share 'x' anywhere", "Priya lives at 12 Elm Street", "\u{130}stanbul flat", "say \"hello\" now"] {
            let (q, n) = forget_question(text, 2, "10:05", true);
            assert_eq!(quoted_spans(&q), vec![canon(text)], "{q}");
            assert_eq!(n, Some(needle(text)), "{q}");
            assert!(q.starts_with("Forget "), "{q}");
        }
        // A text with a straight quote is quoted with curly ones.
        assert!(forget_question("say \"hello\" now", 1, "10:05", true).0.starts_with("Forget \u{201C}"));
        // Words the person never put in this conversation are not quoted: that would add a copy.
        let (q, n) = forget_question("throwaway-erase2", 1, "10:05", false);
        assert!(n.is_none() && !q.contains("throwaway-erase2"), "{q}");
        assert!(q.contains("Erase the text you asked me to forget?"), "{q}");
    }

    /// Not quoted, and no needle: both kinds of double quote, under 4 scalars, over 4096, or a quote
    /// that would close past what the person sees.
    #[test]
    fn what_cannot_be_redacted_is_not_quoted() {
        let long = "x".repeat(2100);
        for (text, why) in [
            ("a \"b\" \u{201C}c\u{201D} d", "both kinds of quote"),
            ("abc", "under four scalars"),
            (long.as_str(), "past the visible prefix"),
        ] {
            let (q, n) = forget_question(text, 1, "10:05", true);
            assert!(n.is_none(), "{why}");
            assert!(!q.contains(text), "{why}: the question quoted it");
            assert!(q.contains("This conversation keeps its own copy"), "{why}");
        }
        let huge = "y".repeat(4097);
        assert!(forget_question(&huge, 1, "10:05", true).1.is_none(), "over 4096");
    }

    #[test]
    fn the_outcome_says_only_what_the_desktop_said() {
        let ok = conversation_outcome(Some(&serde_json::json!({"redacted": 3, "where": ["transcript", "runs"]})));
        assert!(ok.contains("gone from this conversation (3 place(s))") && ok.contains("earlier conversations keep their words"), "{ok}");
        let no = conversation_outcome(Some(&serde_json::json!({"refused": "a needle is not in the question the person answered"})));
        assert!(no.contains("still holds it: a needle is not in the question") && !no.contains("gone"), "{no}");
        assert!(conversation_outcome(None).contains("still holds it"));
        let warned = conversation_outcome(Some(&serde_json::json!({"redacted": 1, "where": ["runs"], "warning": "secure_delete could not be restored on this connection"})));
        assert!(warned.contains("secure_delete could not be restored"), "{warned}");
    }
}
