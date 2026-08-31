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
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// `.NS` / `.BO` are how a data feed names an exchange, not how a person names a company.
///
/// Line by line, because an earlier version split the whole text on whitespace and rejoined it —
/// which collapsed every newline, so the bullet-stripping that runs afterwards saw one long line and
/// removed nothing. The text came out with its dashes intact and would have been read aloud as
/// "dash RELIANCE price is still unconfirmed".
fn strip_exchange_suffixes(s: &str) -> String {
    s.lines()
        .map(strip_suffixes_in_line)
        .collect::<Vec<_>>()
        .join("\n")
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
        if (bytes[i] == '-' || bytes[i] == '+')
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
        {
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

/// The opening of a reply, cut at a whole sentence, within a word budget.
///
/// The instruction alone does not hold. Measured after a hard forty-word limit was added to the
/// spoken register: "what is the Nifty at" came back as thirteen words that simply stop, and "what
/// is in my paper account" came back at seventy with a closing offer. The rule binds when the answer
/// is a fact and dissolves when the question invites elaboration.
///
/// So the mouth enforces what the prompt requests. This is NOT a rewrite — no second model pass, no
/// paraphrase, nothing that could change a number — and NOT a truncation mid-word. It is whole
/// sentences up to the budget, then silence. The full text still reaches the transcript, so nothing
/// is lost; it simply is not monologued at someone who asked a one-line question.
///
/// A single sentence longer than the budget is spoken in full rather than cut: half a sentence is
/// worse than a long one, because the listener is left waiting for a verb.
pub fn within_budget(text: &str, max_words: usize) -> String {
    let spoken = to_spoken(text);
    let mut out: Vec<&str> = Vec::new();
    let mut words = 0usize;
    for sentence in split_sentences(&spoken) {
        let n = sentence.split_whitespace().count();
        if !out.is_empty() && words + n > max_words {
            break;
        }
        words += n;
        out.push(sentence);
    }
    if out.is_empty() {
        return spoken;
    }
    // `join` builds the owned string before `out`'s slices of `spoken` are dropped.
    out.join(" ")
}

/// Sentence boundaries, kept simple — an abbreviation is a smaller error than a wrong split.
fn split_sentences(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = s.as_bytes();
    for i in 0..b.len() {
        if matches!(b[i], b'.' | b'!' | b'?') {
            let end = i + 1;
            let is_end = end >= b.len() || b[end] == b' ';
            if is_end && end - start > 1 {
                out.push(s[start..end].trim());
                start = end;
            }
        }
    }
    if start < s.len() && !s[start..].trim().is_empty() {
        out.push(s[start..].trim());
    }
    out.into_iter().filter(|x| !x.is_empty()).collect()
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
        assert!(
            said.contains("the Nifty"),
            "^NSEI is 'the Nifty' to a person: {said}"
        );
        assert!(
            said.contains("down 0.27 percent"),
            "a leading minus is a direction: {said}"
        );
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
    fn the_budget_stops_at_a_whole_thought_not_mid_word() {
        // The real over-long reply, verbatim. The register asked for forty words and got seventy,
        // so the mouth honours what the prompt requested.
        let long = "You're sitting on cash — $10,000 in the account, all of it uninvested, with no open positions. Buying power is $40,000, so you've got room for up to 4x leverage if you want to deploy. Given the RELIANCE and INFY threads you've been tracking, the account is ready for a first entry whenever you set a thesis. Want me to pull a fresh RELIANCE quote and sketch a position size?";
        let said = within_budget(long, 45);
        assert!(
            said.split_whitespace().count() <= 50,
            "{} words: {said}",
            said.split_whitespace().count()
        );
        assert!(
            said.ends_with('.'),
            "stops at a full stop, never mid-word: {said}"
        );
        assert!(
            said.contains("10,000"),
            "the ANSWER survives — it is the first sentence: {said}"
        );
        assert!(
            !said.contains("Want me to"),
            "the trailing offer is not reached: {said}"
        );
    }

    #[test]
    fn a_short_answer_is_left_exactly_as_it_is() {
        // The measured good case: thirteen words that simply stop. The budget must not touch it.
        let good = "Nifty is at twenty-four thousand two hundred fifteen, flat on the session.";
        assert_eq!(within_budget(good, 45), to_spoken(good));
    }

    #[test]
    fn one_very_long_sentence_is_spoken_whole_rather_than_halved() {
        // Half a sentence is worse than a long one — the listener is left waiting for a verb.
        let one = "The reason the account shows ten thousand dollars of buying power despite having no positions at all is that the broker extends four times leverage on a cash balance of that size.";
        let said = within_budget(one, 10);
        assert!(
            said.split_whitespace().count() > 10,
            "a lone long sentence is not cut: {said}"
        );
        assert!(said.ends_with('.'));
    }

    #[test]
    fn the_first_chunk_is_short_so_the_reply_starts_immediately() {
        // The gap before the first sound is where a conversation dies. A short opening clause is
        // forgiven; two seconds of silence is not.
        let text = "The Nifty is at 24,053, down about a quarter of a percent on the session. \
                    Reliance never came back cleanly, so I won't guess it. Want me to re-pull?";
        let chunks = speakable_chunks(text, 60, 160);
        assert!(chunks.len() >= 2, "{chunks:?}");
        assert!(
            chunks[0].len() <= 70,
            "first chunk must be quick to synthesise: {:?}",
            chunks[0]
        );
        // And nothing may be lost in the chunking — a dropped clause is a changed answer.
        let rejoined = chunks.join(" ");
        assert!(rejoined.contains("Reliance"), "{rejoined}");
        assert!(rejoined.contains("re-pull"), "{rejoined}");
    }

    #[test]
    fn a_bullet_list_becomes_sentences_rather_than_a_paragraph_of_dashes() {
        let said = to_spoken("Here's the state:\n- one thing\n- another thing");
        assert!(!said.contains(" - "), "{said}");
        assert!(
            said.contains("one thing."),
            "each bullet ends as its own sentence: {said}"
        );
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
