//! redact — what the SCREEN may show, as opposed to what the mind may know.
//!
//! The live token tail, the reasoning fold and the step details put model internals on the
//! operator's display — visible to a glance, a screenshot, a screen share. A stored phone number
//! or an API key riding through a thinking block is a leak with no upside: the diagnostics are
//! read for SHAPE ("it recalled the email, it called the tool"), never for the value itself.
//!
//! The rule, by surface:
//! - STREAMS (thinking, step details, live tokens): always shape-masked. Even when the user asked
//!   for a value, the ANSWER is where they receive it — progress lines never need to carry it.
//! - THE FINAL ANSWER: personal values pass (asking for them is what the mind is for);
//!   CREDENTIAL-shaped values are masked unconditionally, the config panel's own precedent — two
//!   credential leaks in one month came from printing "just enough" of a value, and the only safe
//!   rendering is none. A key's home is the env file and the masked settings row, never chat.
//! - THE TRANSCRIPT/MEMORY: untouched. Masking what the mind remembers would corrupt recall;
//!   this module runs at the display edge only.

/// Mask personal AND credential values — the STREAM rule.
pub(crate) fn redact_stream(text: &str) -> String {
    redact(text, true)
}

/// Mask credential values only — the FINAL-ANSWER rule.
pub(crate) fn redact_answer(text: &str) -> String {
    redact(text, false)
}

fn redact(text: &str, mask_personal: bool) -> String {
    // Token-wise pass over the text, preserving separators exactly: the streams carry prose and
    // JSON fragments, and a redactor that reflows either would corrupt what it protects.
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    for c in text.chars() {
        if c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '`'
            )
        {
            flush(&mut out, &mut token, mask_personal);
            out.push(c);
        } else {
            token.push(c);
        }
    }
    flush(&mut out, &mut token, mask_personal);
    out
}

fn flush(out: &mut String, token: &mut String, mask_personal: bool) {
    if token.is_empty() {
        return;
    }
    out.push_str(&mask_token(token, mask_personal));
    token.clear();
}

fn mask_token(tok: &str, mask_personal: bool) -> String {
    // Strip surrounding punctuation the splitter kept (a trailing period, a colon).
    let core = tok.trim_matches(|c: char| {
        !c.is_alphanumeric() && c != '@' && c != '.' && c != '-' && c != '_' && c != '+'
    });
    if core.len() < 7 {
        return tok.to_string();
    }
    // E.REDACT1: `core` is a sub-slice of `tok`, so its offset is where it actually sits — never
    // a second predicate's count. Two predicates disagreed on a leading `-`/`.`/`_`/`+` and the
    // slice ran off the end (`-abcdefgh`: prefix 1 + core 9 = 10 of 9), killing the handler task.
    let start = core.as_ptr() as usize - tok.as_ptr() as usize;
    let head = &tok[..start];
    let tail = &tok[start + core.len()..];

    // ── Credentials: masked on EVERY surface. ───────────────────────────────────────────────────
    // Known key prefixes first (highest precision), then the shape rule: long, no spaces, mixes
    // letters and digits, not a plain word — the same class `distinctive_pii` calls a long id.
    const KEY_PREFIXES: &[&str] = &[
        "sk-", "gsk_", "csk-", "nvapi-", "ghp_", "gho_", "xoxb-", "xoxp-", "AKIA", "sk_live_",
        "pk_live_", "Bearer ",
    ];
    let is_prefixed_key = KEY_PREFIXES.iter().any(|p| core.starts_with(p));
    let digits = core.chars().filter(|c| c.is_ascii_digit()).count();
    let has_upper = core.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = core.chars().any(|c| c.is_ascii_lowercase());
    // MIXED CASE required: real keys are base62-ish and mix cases; long lowercase hex is a
    // CHECKSUM (a pack's blake3, a binary's md5) — public by nature, and masking it would gut the
    // very reports that cite digests as evidence.
    let is_long_id = core.len() >= 20
        && digits > 0
        && has_upper
        && has_lower
        && core
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '.'))
        && !core.contains('@');
    if is_prefixed_key || is_long_id {
        return format!(
            "{head}•••{}·masked·{tail}",
            core.chars().take(4).collect::<String>()
        );
    }

    if !mask_personal {
        return tok.to_string();
    }

    // ── Personal shapes: masked on streams only. ────────────────────────────────────────────────
    // An email keeps its first letter and its domain's tld — enough to recognise WHICH address the
    // step touched, never the address.
    if let Some(at) = core.find('@') {
        let (local, domain) = (&core[..at], &core[at + 1..]);
        if at > 0 && domain.contains('.') && !domain.ends_with('.') {
            let first = local.chars().next().unwrap_or('•');
            let tld = domain.rsplit('.').next().unwrap_or("");
            return format!("{head}{first}•••@•••.{tld}{tail}");
        }
    }
    // A contiguous 7–15 digit run (phone / account / id): keep the last two digits.
    if core.chars().all(|c| c.is_ascii_digit()) && (7..=15).contains(&core.len()) {
        let last2: String = core.chars().skip(core.len() - 2).collect();
        return format!("{head}•••{last2}{tail}");
    }
    tok.to_string()
}

#[cfg(test)]
mod tests {
    /// E.REDACT1: the exact shape that panicked on staging (byte index 10 of 9).
    #[test]
    fn a_token_starting_with_a_kept_punctuation_char_does_not_run_off_the_end() {
        for tok in ["-abcdefgh", ".abcdefgh", "_abcdefgh", "+abcdefgh", "-abcdefgh.", "(—endpoint"] {
            let out = super::mask_token(tok, true);
            assert!(!out.is_empty(), "{tok:?} -> {out:?}");
        }
        assert_eq!(super::mask_token("(connection", false), "(connection", "a plain word keeps its punctuation");
    }

    use super::*;

    /// The stream rule: personal values are recognisable in shape, never in value.
    #[test]
    fn streams_mask_personal_values_to_shape() {
        let s =
            redact_stream("recalled brishti.sarkar@gmail.com and called 5551234567 for the order");
        assert!(!s.contains("brishti.sarkar@gmail.com"), "{s}");
        assert!(
            s.contains("b•••@•••.com"),
            "recognisable shape survives: {s}"
        );
        assert!(!s.contains("5551234567"), "{s}");
        assert!(
            s.contains("•••67"),
            "the last two digits anchor recognition: {s}"
        );
        assert!(
            s.contains("for the order"),
            "prose around the values is untouched"
        );
    }

    /// Credentials are masked on EVERY surface — including the final answer, even if asked.
    #[test]
    fn credentials_never_render_anywhere() {
        for surface in [
            redact_stream as fn(&str) -> String,
            redact_answer as fn(&str) -> String,
        ] {
            let s = surface("the key is nvapi-8IwezH3XBe8gBGkAGSZNFaMkS1ugmKc62UmKWFCLU3Yg and gsk_dA7T7qxKjURu3ZE5F7No");
            assert!(!s.contains("8IwezH3XBe"), "{s}");
            assert!(!s.contains("dA7T7qxKjURu"), "{s}");
            assert!(
                s.contains("·masked·"),
                "masking must be visible as masking: {s}"
            );
        }
    }

    /// The answer rule: personal values PASS — asking for them is what the mind is for.
    #[test]
    fn answers_keep_personal_values() {
        let s = redact_answer(
            "Brishti's email is brishti.sarkar@gmail.com and the order line is 5551234567.",
        );
        assert!(s.contains("brishti.sarkar@gmail.com"), "{s}");
        assert!(s.contains("5551234567"), "{s}");
    }

    /// Precision: ordinary prose, dates, years, short numbers, URLs without embedded secrets and
    /// long plain words must survive untouched — an over-eager redactor teaches people to turn it
    /// off, which protects nothing.
    #[test]
    fn ordinary_text_is_untouched() {
        for s in [
            "the meeting is on 2026-08-16 at 19:30",
            "extraordinarily long words like disproportionately stay",
            "see https://packs.yantrikdb.com/getting-started for the guide",
            "the benchmark scored 15/20 across 550 runs in 2026",
            "call it at 6pm; room 402",
        ] {
            assert_eq!(redact_stream(s), s, "false positive on: {s}");
        }
    }

    /// Checksums are evidence, not secrets: a pack's blake3 or a binary's md5 is cited in reports
    /// and must survive every surface. Mixed-case is what separates a key from a digest.
    #[test]
    fn hex_digests_are_not_credentials() {
        let d = "daf7920844e30d5add1db90f9ea7bcf7350ce3f9da03f7f8d01997af7467da89";
        assert_eq!(redact_answer(&format!("blake3 {d}")), format!("blake3 {d}"));
        assert_eq!(
            redact_stream(&format!("digest {d} verified")),
            format!("digest {d} verified")
        );
    }

    /// JSON fragments (the live tail often carries them) keep their structure — only values mask.
    #[test]
    fn json_fragments_keep_their_shape() {
        let s = redact_stream(r#"{"email":"a.b@example.org","note":"send the invite"}"#);
        assert!(s.contains(r#""email":"#), "{s}");
        assert!(!s.contains("a.b@example.org"), "{s}");
        assert!(s.contains("send the invite"), "{s}");
    }
}
