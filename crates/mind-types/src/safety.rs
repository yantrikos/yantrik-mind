//! Shared safety primitives used by BOTH the harm-gate (outward actions) and the memory write-gate
//! (inward writes to the typed moat). One source of truth so a secret can neither LEAVE via an
//! action nor ENTER the cognitive graph.

/// Substrings that mark a secret/credential. Matched case-insensitively against any text that would
/// cross a trust boundary (an outward payload OR a write into the cognitive moat).
pub const SECRET_MARKERS: &[&str] = &[
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_", // GitHub tokens
    "glpat-",      // GitLab
    "akia",
    "asia", // AWS access keys
    "-----begin",
    "private key", // PEM private keys
    "app password",
    "app-password", // mail app passwords
    "xoxb-",
    "xoxp-", // Slack
    "sk-",   // OpenAI-style
];

// ── SENSITIVITY DETECTION (E.SEC1) ──────────────────────────────────────────────────────────────
//
// `SECRET_MARKERS` above is kept for reference and for anything that wants the raw list. It is NOT
// the detector any more, because a bare `contains` over it failed in both directions at once:
//
//   MISSED  `my password is hunter2` · `the api key is abc123xyz` · `bearer eyJ…` ·
//           `client secret: …` · `ssn 123-45-6789` · a card PIN
//   FIRED   inside **task-**, **risk-**, **desk-**, **ask-** (from `sk-`)
//           and inside **Asia**, **Malaysia**, **asian** (from `asia`)
//
// The second half is the worse one: `gate_write` REFUSES on a hit, so the mind could not remember
// "asian food recipes" while it would happily remember a password. A detector that refuses ordinary
// life and admits credentials is not conservative in either direction.
//
// What replaces it is SHAPE-AWARE and TYPED. Token markers must begin a token and carry a
// credential-shaped tail; credential phrases must have a plausible assigned VALUE near them, so
// "how do passwords work?" is discussion and `password: hunter2` is not; card numbers are found by
// Luhn AND a payment-card industry digit, so a 13-digit epoch that happens to satisfy Luhn is not
// a card; and card/PIN/CVV wording near a number is caught even when Luhn fails, which is the
// reported `4471-9302-1122-8890` case.

/// What kind of sensitive thing was found. Deliberately carries no value — see [`SensitiveFinding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SensitiveKind {
    /// A token whose prefix and shape match a known credential format (GitHub, AWS, Slack, …).
    TokenPrefix,
    /// A PEM private-key block.
    PemPrivateKey,
    /// A credential word with a plausible assigned value beside it.
    CredentialPhrase,
    /// A payment card number: 13–19 digits, Luhn-valid, card industry digit.
    PaymentPan,
    /// Card/PIN/CVV wording next to a number — caught even when Luhn does not pass.
    CardContextNumber,
    /// A national identity number in a context that names it.
    NationalId,
}

impl SensitiveKind {
    pub fn label(self) -> &'static str {
        match self {
            SensitiveKind::TokenPrefix => "token",
            SensitiveKind::PemPrivateKey => "pem-private-key",
            SensitiveKind::CredentialPhrase => "credential-phrase",
            SensitiveKind::PaymentPan => "payment-card",
            SensitiveKind::CardContextNumber => "card-context-number",
            SensitiveKind::NationalId => "national-id",
        }
    }
}

/// WHERE something sensitive is and WHAT KIND it is — never WHAT it says.
///
/// The value is not carried, and there is nowhere to put it: every derived `Debug`, every log line
/// and every error message built from this can only ever name a kind and a span. A refusal that
/// quotes what it refused is the leak it was meant to prevent (E.SEC1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensitiveFinding {
    pub kind: SensitiveKind,
    /// Byte offset of the match in the text that was scanned.
    pub start: usize,
    /// Byte length of the match.
    pub len: usize,
}

impl std::fmt::Display for SensitiveFinding {
    /// Kind and span only. There is no formatting path that can print the matched text.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at {}..{}",
            self.kind.label(),
            self.start,
            self.start + self.len
        )
    }
}

/// A credential token's prefix and the tail it must carry to be one.
struct TokenShape {
    prefix: &'static str,
    /// Characters required AFTER the prefix. `Asia` fails because it has none.
    ///
    /// Set WELL BELOW the real formats (a GitHub PAT carries 36, an OpenAI key 48) so that a
    /// shortened stand-in or a truncated paste is still treated as a credential: erring long here
    /// turns a partial leak into a clean bill of health. Two PRE-EXISTING tests calibrated this —
    /// they carried `ghp_SECRET12345` and `sk-abc123`, and a stricter threshold silently stopped
    /// catching both. What keeps false positives out is the TOKEN BOUNDARY, not the length.
    min_tail: usize,
    /// AWS key ids are upper-case; requiring it is what keeps `asia`/`Malaysia` out.
    upper_only: bool,
}

const TOKEN_SHAPES: &[TokenShape] = &[
    TokenShape {
        prefix: "ghp_",
        min_tail: 8,
        upper_only: false,
    },
    TokenShape {
        prefix: "gho_",
        min_tail: 8,
        upper_only: false,
    },
    TokenShape {
        prefix: "ghu_",
        min_tail: 8,
        upper_only: false,
    },
    TokenShape {
        prefix: "ghs_",
        min_tail: 8,
        upper_only: false,
    },
    TokenShape {
        prefix: "github_pat_",
        min_tail: 8,
        upper_only: false,
    },
    TokenShape {
        prefix: "glpat-",
        min_tail: 8,
        upper_only: false,
    },
    TokenShape {
        prefix: "xoxb-",
        min_tail: 8,
        upper_only: false,
    },
    TokenShape {
        prefix: "xoxp-",
        min_tail: 8,
        upper_only: false,
    },
    TokenShape {
        prefix: "sk-",
        min_tail: 6,
        upper_only: false,
    },
    TokenShape {
        prefix: "akia",
        min_tail: 16,
        upper_only: true,
    },
    TokenShape {
        prefix: "asia",
        min_tail: 16,
        upper_only: true,
    },
];

/// Words that make a nearby number an identity number rather than a quantity.
const CARD_CONTEXT: &[&str] = &["card", "cards", "pin", "pins", "cvv", "cvc", "iban", "pan"];
const SSN_CONTEXT: &[&str] = &["ssn", "social"];

/// Credential words. On their own they are conversation; with a value beside them they are not.
const CREDENTIAL_PHRASES: &[&str] = &[
    "password",
    "passcode",
    "passphrase",
    "api key",
    "apikey",
    "api-key",
    "secret key",
    "access token",
    "auth token",
    "refresh token",
    "client secret",
    "bearer",
    "private key",
];

/// Is this byte offset the start of a token (rather than the middle of a word)?
fn at_token_start(text: &str, at: usize) -> bool {
    text[..at]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
}

/// The maximal token beginning at `at`.
fn token_at(text: &str, at: usize) -> &str {
    let rest = &text[at..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// Is this run buried inside a longer alphanumeric token (a hash, an id, a base64 blob)?
///
/// A payment card stands on its own or is grouped with spaces and hyphens. Anything with a letter
/// pressed up against either end is part of some other string that merely contains digits — and a
/// 64-character hex hash contains card-shaped substrings often enough to matter: 28 of 11,866
/// read-receipt lines on the box, every one a false positive, which is how this rule was found.
fn is_embedded(text: &str, start: usize, len: usize) -> bool {
    let mut lead = text[..start].chars().rev();
    let mut trail = text[start + len..].chars();
    let (b1, b2) = (lead.next(), lead.next());
    let (a1, a2) = (trail.next(), trail.next());
    let before = b1.is_some_and(|c| c.is_ascii_alphabetic());
    let after = a1.is_some_and(|c| c.is_ascii_alphabetic());

    // THE FRACTIONAL PART OF A DECIMAL is not a card. `0.5500005555555559` carries a 16-digit run
    // that starts with 5 and satisfies Luhn, so the shape rules alone call a high-precision float a
    // payment card — and the audit found three of them sitting in `event.confidence` on the box.
    // Reachable from `gate_write`, so a memory carrying a precise number would have been refused as
    // a credit card, which is the "asian food recipes" failure wearing arithmetic.
    //
    // Only a decimal POINT WITH A DIGIT ON ITS FAR SIDE counts: `4111111111111111.` at the end of a
    // sentence is still a card, because the period is followed by nothing (E.SEC1b).
    let fractional = b1 == Some('.') && b2.is_some_and(|c| c.is_ascii_digit());
    let truncated = a1 == Some('.') && a2.is_some_and(|c| c.is_ascii_digit());

    before || after || fractional || truncated
}

fn luhn_ok(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.chars().rev().enumerate() {
        let mut n = c.to_digit(10).unwrap_or(0);
        if i % 2 == 1 {
            n *= 2;
            if n > 9 {
                n -= 9;
            }
        }
        sum += n;
    }
    sum.is_multiple_of(10)
}

/// Runs of digits that may be grouped with spaces or hyphens, as `(start, byte_len, digit_count)`.
fn digit_runs(text: &str) -> Vec<(usize, usize, String)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut n) = (0usize, b.len());
    while i < n {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        let mut digits = String::new();
        let mut last_digit_end = i;
        while i < n && (b[i].is_ascii_digit() || b[i] == b' ' || b[i] == b'-') {
            if b[i].is_ascii_digit() {
                digits.push(b[i] as char);
                last_digit_end = i + 1;
            }
            i += 1;
        }
        out.push((start, last_digit_end - start, digits));
        n = b.len();
    }
    out
}

/// The first sensitive thing in `text`, or `None`.
///
/// Order is deliberate: the cheapest and most certain shapes first, so a finding names the most
/// specific reason it could.
pub fn first_sensitive(text: &str) -> Option<SensitiveFinding> {
    // ASCII-only lowering, and it MUST stay that way. Every offset below is found in `lower` and
    // then used to slice `text`, so the two must agree byte for byte. `to_lowercase()` is not
    // length-preserving — `İ` (U+0130, 2 bytes) lowers to 3 — which shifts every later offset and
    // lets one land inside a multibyte character: `first_sensitive("İpassword日本")` panicked with
    // `byte index 11 is not a char boundary`. Reachable from `gate_write`, which scans arbitrary
    // user text on every memory write. `to_ascii_lowercase` touches only A-Z, so lengths and char
    // boundaries are identical by construction, and every marker here is ASCII anyway (E.SEC1b).
    let lower = text.to_ascii_lowercase();
    debug_assert_eq!(
        lower.len(),
        text.len(),
        "offsets are shared between the two"
    );
    let find = |kind, start: usize, len: usize| Some(SensitiveFinding { kind, start, len });

    // 1. PEM private keys — a shape, not a phrase.
    if let Some(at) = lower.find("-----begin") {
        if lower[at..].contains("private key") {
            return find(SensitiveKind::PemPrivateKey, at, "-----begin".len());
        }
    }

    // 2. Credential TOKENS: the prefix must START a token and carry a credential-shaped tail.
    for shape in TOKEN_SHAPES {
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(shape.prefix) {
            let at = from + rel;
            if at_token_start(&lower, at) {
                let tok = token_at(text, at);
                let tail = tok.len().saturating_sub(shape.prefix.len());
                let shape_ok = tail >= shape.min_tail
                    && (!shape.upper_only
                        || tok
                            .chars()
                            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
                if shape_ok {
                    return find(SensitiveKind::TokenPrefix, at, tok.len());
                }
            }
            from = at + shape.prefix.len();
        }
    }

    // 3. Payment cards by SHAPE: Luhn, a card industry digit (3–6), and STANDING ALONE.
    //
    //    Two ways to be fooled here, both found rather than imagined. A 13-digit epoch can satisfy
    //    Luhn by chance — 1756170000000 does — but starts with 1, so the industry digit excludes
    //    it. And a digit run EMBEDDED in a longer alphanumeric token satisfies everything by pure
    //    chance: the read-receipt audit on the box flagged 28 lines, and every one of them was a
    //    SHA-256 `chain` value with a card-shaped substring inside it. A real card is its own
    //    token, optionally grouped with spaces or hyphens — it is never buried in hex.
    for (start, len, digits) in digit_runs(text) {
        let n = digits.len();
        if !(13..=19).contains(&n)
            || !matches!(digits.as_bytes()[0], b'3'..=b'6')
            || !luhn_ok(&digits)
        {
            continue;
        }
        if is_embedded(text, start, len) {
            continue;
        }
        return find(SensitiveKind::PaymentPan, start, len);
    }

    // 4. Card/PIN/CVV wording beside a number — catches what Luhn does not, which is the reported
    //    `4471-9302-1122-8890`. Requires a CONTEXT WORD as its own token, so "pinned" is not "pin".
    let has_ctx = |words: &[&str]| -> bool {
        words.iter().any(|w| {
            let mut from = 0usize;
            while let Some(rel) = lower[from..].find(w) {
                let at = from + rel;
                if at_token_start(&lower, at) && token_at(&lower, at) == *w {
                    return true;
                }
                from = at + w.len();
            }
            false
        })
    };
    if has_ctx(CARD_CONTEXT) {
        for (start, len, digits) in digit_runs(text) {
            if digits.len() >= 4 {
                return find(SensitiveKind::CardContextNumber, start, len);
            }
        }
    }

    // 5. National identity numbers, only where the text says that is what they are.
    if has_ctx(SSN_CONTEXT) {
        for (start, len, digits) in digit_runs(text) {
            if digits.len() == 9 {
                return find(SensitiveKind::NationalId, start, len);
            }
        }
    }

    // 6. Credential PHRASES — but only with a plausible assigned value beside them, so that
    //    "how do passwords work?" stays discussion.
    for phrase in CREDENTIAL_PHRASES {
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(phrase) {
            let at = from + rel;
            if at_token_start(&lower, at) && value_follows(&text[at + phrase.len()..]) {
                return find(SensitiveKind::CredentialPhrase, at, phrase.len());
            }
            from = at + phrase.len();
        }
    }
    None
}

/// Does a plausible assigned VALUE follow, close by?
///
/// Looks at the first few tokens after a credential word, within a short window. A value is a token
/// of at least six characters that either contains a digit or is long enough to be a secret rather
/// than a sentence: `hunter2`, `abc123xyz`, `eyJhbGciOiJIUzI1NiJ9…` qualify; `work`, `policy`,
/// `soon` and `requires` do not.
fn value_follows(after: &str) -> bool {
    const WINDOW: usize = 48;
    const MAX_TOKENS: usize = 3;
    // Truncating at a fixed byte count cuts multibyte characters in half.
    let mut end = after.len().min(WINDOW);
    while end > 0 && !after.is_char_boundary(end) {
        end -= 1;
    }
    let window = &after[..end];
    let mut seen = 0usize;
    for tok in window.split(|c: char| !(c.is_ascii_alphanumeric() || "_-./+=".contains(c))) {
        let tok = tok.trim_matches(|c| c == '=' || c == '.' || c == '/');
        if tok.is_empty() {
            continue;
        }
        // A bare plural or linking word is not a value; skip a couple before giving up.
        if tok.len() >= 6 && (tok.chars().any(|c| c.is_ascii_digit()) || tok.len() >= 12) {
            return true;
        }
        seen += 1;
        if seen >= MAX_TOKENS {
            break;
        }
    }
    false
}

/// Does this text carry a secret/credential? Compatibility wrapper over [`first_sensitive`], kept so
/// every existing caller upgrades at once rather than each deciding for itself (E.SEC1).
pub fn contains_secret(text: &str) -> bool {
    first_sensitive(text).is_some()
}

/// EVERY sensitive thing in `text`, not just the first.
///
/// The audit needs the full set to report kinds; the boundaries only ever need to know whether
/// there is one. Both read the same rules, because an audit that classifies for itself can certify
/// a policy production does not run (E.SEC1b).
pub fn sensitive_findings(text: &str) -> Vec<SensitiveFinding> {
    // A generous ceiling. It exists so a pathological input cannot spin, not as a real limit.
    const MAX: usize = 4096;
    let mut out = Vec::new();
    let mut base = 0usize;
    while base < text.len() && out.len() < MAX {
        let Some(f) = first_sensitive(&text[base..]) else {
            break;
        };
        out.push(SensitiveFinding {
            kind: f.kind,
            start: base + f.start,
            len: f.len,
        });
        // Always advance, even on a zero-length match, and always onto a char boundary.
        let mut next = base + f.start + f.len.max(1);
        while next < text.len() && !text.is_char_boundary(next) {
            next += 1;
        }
        base = next;
    }
    out
}

/// Is this a credential-shaped KEY beside a secret-shaped VALUE?
///
/// JSON splits the context from the content: in `{"api_key": "9f2b1c4d8e"}` neither half is
/// sensitive alone — the key is a word, the value is a hex-ish token — and a walk that scans only
/// values will report the file clean. Judging the pair is the only way to see it, and it belongs
/// in this crate rather than in the walker, so the audit and the boundaries share one policy.
///
/// The returned span is into `value`, and as everywhere here it carries no part of it (E.SEC1b).
pub fn sensitive_pair(key: &str, value: &str) -> Option<SensitiveFinding> {
    // `api_key` and `api-key` and `apiKey` are the same key wearing three coats.
    let flat: String = key
        .chars()
        .map(|c| {
            if c == '_' || c == '-' {
                ' '
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let named = CREDENTIAL_PHRASES.iter().any(|p| {
        let mut from = 0usize;
        while let Some(rel) = flat[from..].find(p) {
            let at = from + rel;
            if at_token_start(&flat, at) {
                return true;
            }
            from = at + p.len();
        }
        false
    });
    if !named {
        return None;
    }
    // A named key still needs a value that looks like a secret rather than a sentence about one:
    // `{"password_policy": "requires 12 characters"}` is documentation.
    let v = value.trim();
    let single_token = !v.chars().any(|c| c.is_whitespace());
    if single_token && v.len() >= 6 {
        return Some(SensitiveFinding {
            kind: SensitiveKind::CredentialPhrase,
            start: 0,
            len: v.len(),
        });
    }
    None
}

/// Where a memory write came from — the trust category. Human/system intent is trusted; everything
/// machine-derived is not. Stored on every Observation so belief revision can weight by independence
/// (e.g. never promote to high confidence from a single human-independent source category).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProvenanceCategory {
    /// The operator or a trusted system turn — may carry intent/policy.
    Human,
    /// Output of a sandboxed code skill.
    SandboxedSkill,
    /// Output of a tool (email/github/etc.).
    ToolResult,
    /// A sub-agent's synthesized claim.
    SubAgent,
    /// Fetched web content (attacker-controllable).
    WebContent,
    /// Raw LLM generation with no external grounding.
    LlmInference,
}

impl ProvenanceCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::SandboxedSkill => "sandboxed_skill",
            Self::ToolResult => "tool_result",
            Self::SubAgent => "sub_agent",
            Self::WebContent => "web_content",
            Self::LlmInference => "llm_inference",
        }
    }

    /// Human/system intent is the only trusted category — only it may author skill intent/policy.
    pub fn is_trusted(&self) -> bool {
        matches!(self, Self::Human)
    }

    /// True for machine-derived sources (none of which alone may raise a belief to high confidence).
    pub fn is_human_independent(&self) -> bool {
        !matches!(self, Self::Human)
    }
}

impl std::str::FromStr for ProvenanceCategory {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "human" => Ok(Self::Human),
            "sandboxed_skill" => Ok(Self::SandboxedSkill),
            "tool_result" => Ok(Self::ToolResult),
            "sub_agent" => Ok(Self::SubAgent),
            "web_content" => Ok(Self::WebContent),
            "llm_inference" => Ok(Self::LlmInference),
            _ => Err("unknown provenance category"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_secret_markers_case_insensitive() {
        assert!(contains_secret("here is ghp_ABCDEFG1234567890"));
        assert!(contains_secret("My App Password is hunter2"));
        assert!(contains_secret("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(!contains_secret("a perfectly normal sentence about github"));
    }

    #[test]
    fn provenance_trust_and_independence() {
        assert!(ProvenanceCategory::Human.is_trusted());
        assert!(!ProvenanceCategory::SandboxedSkill.is_trusted());
        assert!(ProvenanceCategory::SubAgent.is_human_independent());
        assert!(!ProvenanceCategory::Human.is_human_independent());
        assert_eq!(
            ProvenanceCategory::WebContent
                .as_str()
                .parse::<ProvenanceCategory>(),
            Ok(ProvenanceCategory::WebContent)
        );
        assert!("typoed_machine_source"
            .parse::<ProvenanceCategory>()
            .is_err());
    }
}

#[cfg(test)]
mod sensitivity_tests {
    use super::*;

    /// E.SEC1, the FALSE NEGATIVES the old substring detector let through. Every one of these was
    /// verified as passing clean before this slice.
    #[test]
    fn credentials_that_used_to_pass_are_caught() {
        let cases: &[(&str, SensitiveKind)] = &[
            ("my password is hunter2", SensitiveKind::CredentialPhrase),
            ("the api key is abc123xyz", SensitiveKind::CredentialPhrase),
            (
                "bearer eyJhbGciOiJIUzI1NiJ9.abcdefghij",
                SensitiveKind::CredentialPhrase,
            ),
            (
                "client secret: s3cr3t-value-here",
                SensitiveKind::CredentialPhrase,
            ),
            (
                "access token = abcd1234efgh",
                SensitiveKind::CredentialPhrase,
            ),
            ("passcode: 9f3a2b1c8d7e", SensitiveKind::CredentialPhrase),
            // The one that opened the whole finding. Luhn FAILS on it; the card/PIN context is why
            // it is caught anyway.
            (
                "my card pin is 4471-9302-1122-8890",
                SensitiveKind::CardContextNumber,
            ),
            // A real payment card shape: Luhn passes and it starts with a card industry digit.
            (
                "charge 4111 1111 1111 1111 today",
                SensitiveKind::PaymentPan,
            ),
            ("ssn 123-45-6789 on file", SensitiveKind::NationalId),
        ];
        for (text, want) in cases {
            let got = first_sensitive(text).unwrap_or_else(|| panic!("MISSED: {text:?}"));
            assert_eq!(got.kind, *want, "{text:?} -> {got:?}");
            assert!(contains_secret(text), "the wrapper must agree: {text:?}");
        }
    }

    /// E.SEC1, the FALSE POSITIVES — the half that mattered more, because `gate_write` REFUSES on a
    /// hit, so this mind could not remember "asian food recipes" while it would remember a password.
    #[test]
    fn ordinary_life_is_not_a_credential() {
        let clean = [
            // The exact words the old `sk-` and `asia` substrings caught.
            "remind me about the task-list tomorrow",
            "our trip to Asia in March",
            "she works in Malaysia",
            "asian food recipes for friday",
            "the risk-register needs updating",
            "book a desk-space for friday",
            "ask-me-later on that one",
            // Codex's required negative fixtures.
            "order 9876543210987 shipped",
            "tracking 1Z999AA10123456784",
            "the timestamp was 1756170000000",
            "call me on 555-0142 later",
            "uuid 550e8400-e29b-41d4-a716-446655440000",
            "the box is at 192.168.4.90",
            "engine version 0.18.0 is pinned",
            // A SHA-256 chain value. 28 of these were flagged as payment cards by the box audit
            // before `is_embedded` existed — the digits are real, the card is not.
            "chain 8ff1ffe72dc393998df748020b12134eaaa1a1fb85d653c632f28c6fb06bbb42",
            "chain c8fb17f2ac95d3dfb5489f4a595c173276f5b51b9165c9971a436dc9c6d7bc8e",
            "blob YWJjZGVmZ2hpams0MTExMTExMTExMTExMTEx",
            "use 80-100 ms of coyote time",
            // Discussion of credentials is not a credential.
            "how do passwords work?",
            "we should rotate the api key soon",
            "the password policy requires twelve characters",
            "remind me to pin the tab",
            "pin 3 items to the board",
        ];
        for text in clean {
            assert_eq!(
                first_sensitive(text),
                None,
                "FALSE POSITIVE on {text:?}: {:?}",
                first_sensitive(text)
            );
            assert!(!contains_secret(text), "the wrapper must agree: {text:?}");
        }
    }

    /// E.SEC1: token markers must BEGIN a token and carry a credential-shaped tail. The prefixes
    /// alone are ordinary text.
    #[test]
    fn token_markers_are_shape_aware_not_substrings() {
        // Real shapes are caught.
        for text in [
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "glpat-abcdefghijklmnopqrst",
            "xoxb-1234567890-abcdefghijk",
            "sk-proj-abcdefghijklmnopqrstuvwxyz012345",
            "AKIAIOSFODNN7EXAMPLE",
            "ASIAY34FZKBOKMUTVV7A",
        ] {
            let got = first_sensitive(text).unwrap_or_else(|| panic!("MISSED token: {text:?}"));
            assert_eq!(got.kind, SensitiveKind::TokenPrefix, "{text:?}");
        }
        // The same prefixes as ordinary words are not.
        for text in [
            "ghp_ is a prefix",
            "sk- alone",
            "AKIA",
            "ASIA",
            "asia",
            "Asia Pacific review",
        ] {
            assert_eq!(first_sensitive(text), None, "FALSE POSITIVE: {text:?}");
        }
        // PEM is a shape too.
        assert_eq!(
            first_sensitive("-----BEGIN RSA PRIVATE KEY-----\nMIIEvg==").map(|f| f.kind),
            Some(SensitiveKind::PemPrivateKey)
        );
        // ...and talking about one is not.
        assert_eq!(first_sensitive("how do I rotate a private key?"), None);
    }

    /// E.SEC1 condition 2: a finding names a KIND and a SPAN. There is no path — Debug, Display or
    /// otherwise — by which it can print what it matched. A refusal that quotes what it refused is
    /// the leak it was meant to prevent.
    #[test]
    fn a_finding_can_never_carry_the_value() {
        let secret = "my password is hunter2swordfish";
        let f = first_sensitive(secret).expect("caught");
        let debug = format!("{f:?}");
        let display = format!("{f}");
        for rendered in [&debug, &display] {
            assert!(
                !rendered.contains("hunter2swordfish"),
                "the value leaked: {rendered}"
            );
            assert!(!rendered.contains("hunter"), "even partly: {rendered}");
        }
        assert!(display.contains("credential-phrase"), "{display}");
        assert!(debug.contains("CredentialPhrase"), "{debug}");
        // The span points at the WORD, and the caller may use it to redact — but nothing in the
        // finding hands over the text.
        assert_eq!(&secret[f.start..f.start + f.len], "password");
    }
}

/// E.SEC1b — Codex's review points, as tests.
#[cfg(test)]
mod sec1b {
    use super::*;

    /// Characters chosen for the ways they break byte arithmetic: a length-CHANGING lowercase, a
    /// dotless pair, a combining mark that can stand alone, 3- and 4-byte scalars, and a BOM.
    const NASTY: &[&str] = &["İ", "ı", "ẞ", "\u{0307}", "日", "🔑", "\u{feff}", "é", "Ⱥ"];

    #[test]
    fn no_offset_arithmetic_can_panic_or_leave_a_span_mid_character() {
        // THE BUG THIS EXISTS FOR: `first_sensitive` finds offsets in a lowered copy and slices the
        // ORIGINAL with them. `to_lowercase` is not length-preserving (`İ` is 2 bytes, its
        // lowercase is 3), so offsets shifted and one landed inside a multibyte character:
        // `first_sensitive("İpassword日本")` panicked with `byte index 11 is not a char boundary`.
        // Reachable from `gate_write`, which scans arbitrary user text on every memory write.
        // It must not PANIC. The correct answer is None — no value is assigned after the word —
        // and the assertion says so rather than assuming a finding, which is what I assumed first.
        assert!(
            first_sensitive("İpassword日本").is_none(),
            "no value follows it, so it is not a credential"
        );
        // The same multibyte prefix with a real value is still caught, so the fix did not buy
        // boundary-safety by going blind.
        assert!(
            first_sensitive("İpassword: hunter2").is_some(),
            "a shifted offset must not hide a secret"
        );
        assert!(first_sensitive("日本語 ghp_SECRET12345").is_some());

        // Every nasty character woven through a body at EVERY byte boundary, against every rule.
        let bodies = [
            "password: hunter2",
            "ghp_SECRET12345",
            "-----BEGIN RSA PRIVATE KEY-----",
            "card 4471 9302 1122 8890",
            "ssn 123456789",
            "AKIAIOSFODNN7EXAMPLE",
            "nothing sensitive here at all",
            "",
        ];
        for body in bodies {
            for nasty in NASTY {
                for cut in 0..=body.len() {
                    if !body.is_char_boundary(cut) {
                        continue;
                    }
                    let text = format!("{}{}{}", &body[..cut], nasty, &body[cut..]);
                    // 1. It must not panic.
                    let found = first_sensitive(&text);
                    // 2. And any span it reports must be a real slice of the text it scanned —
                    //    a span that cannot be sliced is a span that was computed against
                    //    something else.
                    if let Some(f) = found {
                        assert!(
                            f.start <= text.len() && f.start + f.len <= text.len(),
                            "span inside the text: {f} of {text:?}"
                        );
                        assert!(
                            text.is_char_boundary(f.start),
                            "start on a boundary: {f} of {text:?}"
                        );
                        assert!(
                            text.is_char_boundary(f.start + f.len),
                            "end on a boundary: {f} of {text:?}"
                        );
                    }
                    // 3. And the all-findings scan must terminate and agree about the first one.
                    let all = sensitive_findings(&text);
                    assert_eq!(
                        all.first().copied(),
                        found,
                        "the two entry points agree: {text:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_value_window_is_cut_on_a_character_not_a_byte() {
        // `value_follows` truncates what follows a credential word at a FIXED 48 bytes. A multibyte
        // character straddling that edge is sliced in half. Padding is swept across the edge so the
        // straddle is hit whatever the exact offset.
        for pad in 36..64 {
            for nasty in NASTY {
                let text = format!("password: {}{}", "a".repeat(pad), nasty);
                let _ = first_sensitive(&text);
                let text = format!("api key = {}{}9", "b".repeat(pad), nasty);
                let _ = first_sensitive(&text);
            }
        }
    }

    #[test]
    fn a_credential_key_and_its_value_are_judged_together() {
        // POINT 2. JSON splits the context from the content. Neither half of
        // `{"api_key": "9f2b1c4d8e"}` is sensitive alone — the key is a word, the value is a
        // hex-ish token — so a walk that scans only values reports the file clean.
        assert!(
            sensitive_pair("api_key", "9f2b1c4d8e").is_some(),
            "the case a value-only walk misses"
        );
        assert!(sensitive_pair("password", "hunter2").is_some());
        assert!(
            sensitive_pair("apiKey", "9f2b1c4d8e").is_some(),
            "camelCase is the same key"
        );
        assert!(sensitive_pair("API-KEY", "9f2b1c4d8e").is_some());
        assert!(sensitive_pair("client_secret", "abcdef123456").is_some());
        assert!(sensitive_pair("auth_token", "eyJhbGciOiJIUzI1NiJ9").is_some());

        // And the documentation that must stay writable.
        assert!(
            sensitive_pair("password_policy", "requires 12 characters").is_none(),
            "a sentence is not a secret"
        );
        assert!(
            sensitive_pair("note", "9f2b1c4d8e").is_none(),
            "an unnamed key is not context"
        );
        assert!(
            sensitive_pair("password", "").is_none(),
            "an empty value is not a secret"
        );
        assert!(
            sensitive_pair("password", "unset").is_none(),
            "too short to be one"
        );
        assert!(sensitive_pair("passwords_are_hard", "why is that").is_none());

        // The whole-text detector already catches the phrase form across JSON punctuation; the pair
        // rule is the ADDITION, not a replacement.
        assert!(
            first_sensitive(r#"{"password":"hunter2"}"#).is_some(),
            "already caught before E.SEC1b"
        );
        assert!(
            first_sensitive(r#"{"api_key":"sk-abc123"}"#).is_some(),
            "caught by token shape"
        );
        assert!(
            first_sensitive(r#"{"api_key":"9f2b1c4d8e"}"#).is_none(),
            "and THIS is the gap the pair rule closes"
        );
    }

    #[test]
    fn a_finding_has_nowhere_to_put_the_value_it_found() {
        // POINT 4's other half: a refusal that quotes what it refused is the leak it prevents. This
        // asserts over the DERIVED formatting, which is what error messages and log lines are built
        // from — there is no path from a finding back to the text.
        let secret = "hunter2";
        let text = format!("my password is {secret}");
        let f = first_sensitive(&text).expect("caught");
        for rendered in [
            format!("{f}"),
            format!("{f:?}"),
            format!("{:?}", f.kind),
            f.kind.label().to_string(),
        ] {
            assert!(
                !rendered.contains(secret),
                "a finding must not carry its value: {rendered}"
            );
            assert!(
                !rendered.contains("my password is"),
                "nor the text around it: {rendered}"
            );
        }
        // The same for every kind, against a battery of real-shaped secrets.
        for probe in [
            "ghp_SECRET12345",
            "AKIAIOSFODNN7EXAMPLE",
            "-----BEGIN RSA PRIVATE KEY-----",
            "card 4471 9302 1122 8890",
            "my ssn is 123456789",
        ] {
            let f = first_sensitive(probe).expect("caught");
            let rendered = format!("{f} / {f:?}");
            for word in probe.split_whitespace().filter(|w| w.len() >= 5) {
                assert!(!rendered.contains(word), "{rendered} leaked {word}");
            }
        }
    }

    #[test]
    fn a_high_precision_decimal_is_not_a_payment_card() {
        // FOUND BY THE AUDIT, on the box: three `event.confidence` values in the decision log were
        // being reported as payment cards. A confidence float's fractional part is a long digit run
        // that can start with 3-6 and satisfy Luhn by chance, and nothing about the shape rules
        // says otherwise. `gate_write` scans every memory write, so this refused precise numbers.
        assert!(
            first_sensitive("0.5500005555555559").is_none(),
            "a fraction is not a card"
        );
        assert!(first_sensitive("confidence 0.5500005555555559 recorded").is_none());
        assert!(first_sensitive("ratio 12.5500005555555559").is_none());
        // The far side of the point must be a DIGIT for it to be part of a number.
        assert!(
            first_sensitive("I paid with 5500005555555559.").is_some(),
            "a sentence-ending period is not a decimal"
        );
        assert!(first_sensitive("I paid with 5500005555555559").is_some());
        assert!(first_sensitive("5500005555555559 is the number").is_some());
        // And the control from the audit's own evidence: the same digits buried in hex stay out.
        assert!(first_sensitive("deadbeef4111111111111111cafebabe").is_none());
        assert!(first_sensitive("paid with 4111111111111111 today").is_some());
    }

    #[test]
    fn the_all_findings_scan_terminates_and_finds_more_than_one() {
        let text = "ghp_SECRET12345 and also AKIAIOSFODNN7EXAMPLE";
        let all = sensitive_findings(text);
        assert!(all.len() >= 2, "both tokens: {all:?}");
        for f in &all {
            assert!(
                text.is_char_boundary(f.start) && text.is_char_boundary(f.start + f.len),
                "{f}"
            );
        }
        assert!(sensitive_findings("nothing here").is_empty());
        assert!(sensitive_findings("").is_empty());
        // Termination on a degenerate input, not just a clean one.
        let _ = sensitive_findings(&"sk-abc123 ".repeat(500));
    }
}
