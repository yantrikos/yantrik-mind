//! SPEECH — turning a written reply into something a person can listen to.
//!
//! The mind's replies are shaped for a chat window: bold headers, bullet lists, ticker symbols,
//! percentages, emoji. That shape is good on a screen and unbearable in the ear. Its actual Telegram
//! message read aloud begins "Two things I'm carrying for you right now, colon, dash, asterisk
//! asterisk RELIANCE price is still unconfirmed" — and no quality of synthesiser rescues it, because
//! the problem is the text and not the voice.
//!
//! Two jobs here, and they are different.
//!
//! **REGISTER** is what the model should write when it knows it will be heard: short sentences, one
//! idea at a time, contractions, no lists, no markup. That is a composition instruction, and it
//! belongs in the prompt.
//!
//! **NORMALISATION** is what has to happen to text that was written anyway — because a tool result
//! is `^NSEI: 24102.85 INR (-0.27%)` no matter how the reply is phrased around it. Symbols get
//! spoken names, markup is stripped, emoji are dropped.
//!
//! ## Why sentences are cut early
//!
//! A conversation dies in the gap before the first sound. Synthesis runs at up to 19x realtime, so
//! the first sentence can be speaking while the rest is still being turned into audio — the listener
//! hears a reply beginning ~200ms after it exists rather than after the whole paragraph is rendered.
//! That single change is most of what "natural" means; the rest is manners.

/// What to tell the model when the reply will be HEARD rather than read.
///
/// Written to describe speech rather than to forbid markdown, because "no bullet points" produces a
/// paragraph that is still a list with the bullets removed.
pub const SPOKEN_REGISTER: &str = "You are SPEAKING this reply aloud, not writing it. Say it the way \
you would to someone sitting next to you: short sentences, one idea at a time, contractions, no \
lists, no headings, no markdown, no emoji. Lead with the answer — a person cannot skim you, so the \
thing they asked for goes in the first sentence. Numbers are spoken naturally: 'twenty four thousand \
fifty three' not '24,053.30', 'down about a quarter of a percent' not '-0.27%'. If you have three \
things to say, say the most important one and offer the rest. Never read a table.";

/// Spoken names for the symbols that appear in market output.
const SYMBOL_SPEECH: &[(&str, &str)] = &[
    ("^NSEI", "the Nifty"),
    ("^BSESN", "the Sensex"),
    ("^GSPC", "the S and P"),
    ("^IXIC", "the Nasdaq"),
    ("^DJI", "the Dow"),
    ("RELIANCE.NS", "Reliance"),
    ("INFY.NS", "Infosys"),
    ("TCS.NS", "T C S"),
];

/// Turn written text into something worth hearing.
pub fn to_spoken(text: &str) -> String {
    let mut s = text.to_string();
    for (sym, said) in SYMBOL_SPEECH {
        s = s.replace(sym, said);
    }
    // An Indian listing that has no explicit mapping still must not be spelled out suffix and all.
    s = strip_exchange_suffixes(&s);
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        let mut l = line.trim();
        // Bullets and headers become ordinary sentences; the pause between them does the work the
        // bullet did on screen.
        for p in ["- ", "* ", "• ", "#### ", "### ", "## ", "# "] {
            if let Some(rest) = l.strip_prefix(p) {
                l = rest.trim();
                break;
            }
        }
        if l.is_empty() {
            continue;
        }
        out.push_str(l);
        if !l.ends_with(['.', '!', '?', ':', ',']) {
            out.push('.');
        }
        out.push(' ');
    }
    let mut s = out;
    for m in ["**", "__", "`", "*", "_", "#"] {
        s = s.replace(m, "");
    }
    s = spoken_percentages(&s);
    s = s.replace(" & ", " and ");
    // Emoji and other pictographs carry no sound.
    s = s.chars().filter(|c| !is_pictograph(*c)).collect();
    s.split_whitespace().collect::<Vec<_>>().join(" ").trim().to_string()
}

/// `.NS` / `.BO` are how a data feed names an exchange, not how a person names a company.
///
/// Line by line, because an earlier version split the whole text on whitespace and rejoined it —
/// which collapsed every newline, so the bullet-stripping that runs afterwards saw one long line and
/// removed nothing. The text came out with its dashes intact and would have been read aloud as
/// "dash RELIANCE price is still unconfirmed".
fn strip_exchange_suffixes(s: &str) -> String {
    s.lines().map(strip_suffixes_in_line).collect::<Vec<_>>().join("\n")
}

fn strip_suffixes_in_line(s: &str) -> String {
    s.split_whitespace()
        .map(|w| {
            let core = w.trim_end_matches([',', '.', ':', ';', ')']);
            for suf in [".NS", ".BO"] {
                if let Some(base) = core.strip_suffix(suf) {
                    if !base.is_empty() {
                        return w.replace(&format!("{base}{suf}"), base);
                    }
                }
            }
            w.to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "-0.27%" is read as punctuation unless it is turned into words.
fn spoken_percentages(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        // A minus sign directly before a digit is a direction, not a dash.
        if (bytes[i] == '-' || bytes[i] == '+') && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
            let prev_is_word = i > 0 && bytes[i - 1].is_alphanumeric();
            if !prev_is_word {
                out.push_str(if bytes[i] == '-' { "down " } else { "up " });
                i += 1;
                continue;
            }
        }
        if bytes[i] == '%' {
            out.push_str(" percent");
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn is_pictograph(c: char) -> bool {
    matches!(c as u32,
        0x1F300..=0x1FAFF | 0x2600..=0x27BF | 0xFE00..=0xFE0F | 0x1F000..=0x1F2FF | 0x2190..=0x21FF)
}

/// Split into chunks that can be spoken as soon as they exist.
///
/// The FIRST chunk is deliberately allowed to be short: it is the difference between a reply that
/// starts immediately and one that starts after the whole paragraph has been rendered, and a
/// listener forgives a brief opening clause far more readily than a silence.
pub fn speakable_chunks(text: &str, first_max: usize, rest_max: usize) -> Vec<String> {
    let spoken = to_spoken(text);
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let limit = |n: usize| if n == 0 { first_max } else { rest_max };
    for word in spoken.split_whitespace() {
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
        let ends_sentence = word.ends_with(['.', '!', '?']) && !word.ends_with("..");
        if ends_sentence && cur.len() >= 12 {
            chunks.push(std::mem::take(&mut cur));
            continue;
        }
        if cur.len() >= limit(chunks.len()) {
            // No sentence end in sight — break at a comma if there is one, so the seam falls where
            // a speaker would breathe.
            if let Some(p) = cur.rfind(", ") {
                let (head, tail) = cur.split_at(p + 1);
                let (head, tail) = (head.trim().to_string(), tail.trim().to_string());
                chunks.push(head);
                cur = tail;
            } else {
                chunks.push(std::mem::take(&mut cur));
            }
        }
    }
    if !cur.trim().is_empty() {
        chunks.push(cur.trim().to_string());
    }
    chunks.retain(|c| !c.trim().is_empty());
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_minds_own_telegram_reply_becomes_speakable() {
        // This is REAL output, sent to Pranab today. Read aloud verbatim it opens with "asterisk
        // asterisk RELIANCE price" and reads a caret and a percent sign as words.
        let written = "Two things I'm carrying for you right now:\n\
                       - **RELIANCE price is still unconfirmed** this session — I only got a clean read on ^NSEI (24,053.30, -0.27%).\n\
                       - 📉 The U.S.-Iran ceasefire call is overdue.";
        let said = to_spoken(written);
        assert!(!said.contains('*'), "markup must not be spoken: {said}");
        assert!(!said.contains('^'), "a caret is not a word: {said}");
        assert!(!said.contains('%'), "a percent sign is not a word: {said}");
        assert!(!said.contains("📉"), "emoji make no sound: {said}");
        assert!(said.contains("the Nifty"), "^NSEI is 'the Nifty' to a person: {said}");
        assert!(said.contains("down 0.27 percent"), "a leading minus is a direction: {said}");
        // Case-insensitive on purpose: this occurrence is the bare word RELIANCE with no exchange
        // suffix, so no mapping applies and it stays as written. Whether an all-caps word should be
        // lower-cased for the synthesiser is a real open question — some voices spell out capitals —
        // and it is not settled here, so the test asserts what this function actually promises.
        assert!(said.to_lowercase().contains("reliance"), "{said}");
    }

    #[test]
    fn an_exchange_suffix_is_not_part_of_a_company_name() {
        // A feed says RELIANCE.NS; a person says Reliance. Spelled out, it becomes "dot N S".
        assert_eq!(to_spoken("HDFCBANK.NS is flat"), "HDFCBANK is flat.");
        assert!(to_spoken("watching TATAMOTORS.BO today").contains("TATAMOTORS "));
    }

    #[test]
    fn the_first_chunk_is_short_so_the_reply_starts_immediately() {
        // The gap before the first sound is where a conversation dies. A short opening clause is
        // forgiven; two seconds of silence is not.
        let text = "The Nifty is at 24,053, down about a quarter of a percent on the session. \
                    Reliance never came back cleanly, so I won't guess it. Want me to re-pull?";
        let chunks = speakable_chunks(text, 60, 160);
        assert!(chunks.len() >= 2, "{chunks:?}");
        assert!(chunks[0].len() <= 70, "first chunk must be quick to synthesise: {:?}", chunks[0]);
        // And nothing may be lost in the chunking — a dropped clause is a changed answer.
        let rejoined = chunks.join(" ");
        assert!(rejoined.contains("Reliance"), "{rejoined}");
        assert!(rejoined.contains("re-pull"), "{rejoined}");
    }

    #[test]
    fn a_bullet_list_becomes_sentences_rather_than_a_paragraph_of_dashes() {
        let said = to_spoken("Here's the state:\n- one thing\n- another thing");
        assert!(!said.contains(" - "), "{said}");
        assert!(said.contains("one thing."), "each bullet ends as its own sentence: {said}");
        assert!(said.contains("another thing."), "{said}");
    }

    #[test]
    fn a_hyphenated_word_is_not_read_as_a_direction() {
        // "down" is only correct for a minus attached to a number.
        let said = to_spoken("a well-known re-pull of the U.S.-Iran call");
        assert!(said.contains("well-known"), "{said}");
        assert!(!said.contains("down"), "{said}");
    }

    #[test]
    fn the_register_tells_the_model_to_speak_not_to_avoid_markdown() {
        // "No bullet points" yields a paragraph that is still a list with the bullets removed, so
        // the instruction describes speech instead.
        assert!(SPOKEN_REGISTER.contains("SPEAKING"));
        assert!(SPOKEN_REGISTER.contains("Lead with the answer"));
        assert!(SPOKEN_REGISTER.contains("cannot skim"));
    }
}
