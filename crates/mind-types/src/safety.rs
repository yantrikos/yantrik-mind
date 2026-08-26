//! Shared safety primitives used by BOTH the harm-gate (outward actions) and the memory write-gate
//! (inward writes to the typed moat). One source of truth so a secret can neither LEAVE via an
//! action nor ENTER the cognitive graph.

/// Substrings that mark a secret/credential. Matched case-insensitively against any text that would
/// cross a trust boundary (an outward payload OR a write into the cognitive moat).
pub const SECRET_MARKERS: &[&str] = &[
    "ghp_", "gho_", "ghu_", "ghs_", "github_pat_", // GitHub tokens
    "glpat-",                                       // GitLab
    "akia", "asia",                                 // AWS access keys
    "-----begin", "private key",                    // PEM private keys
    "app password", "app-password",                 // mail app passwords
    "xoxb-", "xoxp-",                               // Slack
    "sk-",                                          // OpenAI-style
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
        write!(f, "{} at {}..{}", self.kind.label(), self.start, self.start + self.len)
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
    TokenShape { prefix: "ghp_", min_tail: 8, upper_only: false },
    TokenShape { prefix: "gho_", min_tail: 8, upper_only: false },
    TokenShape { prefix: "ghu_", min_tail: 8, upper_only: false },
    TokenShape { prefix: "ghs_", min_tail: 8, upper_only: false },
    TokenShape { prefix: "github_pat_", min_tail: 8, upper_only: false },
    TokenShape { prefix: "glpat-", min_tail: 8, upper_only: false },
    TokenShape { prefix: "xoxb-", min_tail: 8, upper_only: false },
    TokenShape { prefix: "xoxp-", min_tail: 8, upper_only: false },
    TokenShape { prefix: "sk-", min_tail: 6, upper_only: false },
    TokenShape { prefix: "akia", min_tail: 16, upper_only: true },
    TokenShape { prefix: "asia", min_tail: 16, upper_only: true },
];

/// Words that make a nearby number an identity number rather than a quantity.
const CARD_CONTEXT: &[&str] = &["card", "cards", "pin", "pins", "cvv", "cvc", "iban", "pan"];
const SSN_CONTEXT: &[&str] = &["ssn", "social"];

/// Credential words. On their own they are conversation; with a value beside them they are not.
const CREDENTIAL_PHRASES: &[&str] = &[
    "password", "passcode", "passphrase", "api key", "apikey", "api-key", "secret key",
    "access token", "auth token", "refresh token", "client secret", "bearer", "private key",
];

/// Is this byte offset the start of a token (rather than the middle of a word)?
fn at_token_start(text: &str, at: usize) -> bool {
    text[..at].chars().next_back().map_or(true, |c| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
}

/// The maximal token beginning at `at`.
fn token_at(text: &str, at: usize) -> &str {
    let rest = &text[at..];
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')).unwrap_or(rest.len());
    &rest[..end]
}

/// Is this run buried inside a longer alphanumeric token (a hash, an id, a base64 blob)?
///
/// A payment card stands on its own or is grouped with spaces and hyphens. Anything with a letter
/// pressed up against either end is part of some other string that merely contains digits — and a
/// 64-character hex hash contains card-shaped substrings often enough to matter: 28 of 11,866
/// read-receipt lines on the box, every one a false positive, which is how this rule was found.
fn is_embedded(text: &str, start: usize, len: usize) -> bool {
    let before = text[..start].chars().next_back().is_some_and(|c| c.is_ascii_alphabetic());
    let after = text[start + len..].chars().next().is_some_and(|c| c.is_ascii_alphabetic());
    before || after
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
    sum % 10 == 0
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
    let lower = text.to_lowercase();
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
                    && (!shape.upper_only || tok.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
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
        if !(13..=19).contains(&n) || !matches!(digits.as_bytes()[0], b'3'..=b'6') || !luhn_ok(&digits) {
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
    let window = &after[..after.len().min(WINDOW)];
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

    pub fn from_str(s: &str) -> Self {
        match s {
            "sandboxed_skill" => Self::SandboxedSkill,
            "tool_result" => Self::ToolResult,
            "sub_agent" => Self::SubAgent,
            "web_content" => Self::WebContent,
            "llm_inference" => Self::LlmInference,
            _ => Self::Human,
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
        assert_eq!(ProvenanceCategory::from_str(ProvenanceCategory::WebContent.as_str()), ProvenanceCategory::WebContent);
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
            ("bearer eyJhbGciOiJIUzI1NiJ9.abcdefghij", SensitiveKind::CredentialPhrase),
            ("client secret: s3cr3t-value-here", SensitiveKind::CredentialPhrase),
            ("access token = abcd1234efgh", SensitiveKind::CredentialPhrase),
            ("passcode: 9f3a2b1c8d7e", SensitiveKind::CredentialPhrase),
            // The one that opened the whole finding. Luhn FAILS on it; the card/PIN context is why
            // it is caught anyway.
            ("my card pin is 4471-9302-1122-8890", SensitiveKind::CardContextNumber),
            // A real payment card shape: Luhn passes and it starts with a card industry digit.
            ("charge 4111 1111 1111 1111 today", SensitiveKind::PaymentPan),
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
            assert_eq!(first_sensitive(text), None, "FALSE POSITIVE on {text:?}: {:?}", first_sensitive(text));
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
        for text in ["ghp_ is a prefix", "sk- alone", "AKIA", "ASIA", "asia", "Asia Pacific review"] {
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
            assert!(!rendered.contains("hunter2swordfish"), "the value leaked: {rendered}");
            assert!(!rendered.contains("hunter"), "even partly: {rendered}");
        }
        assert!(display.contains("credential-phrase"), "{display}");
        assert!(debug.contains("CredentialPhrase"), "{debug}");
        // The span points at the WORD, and the caller may use it to redact — but nothing in the
        // finding hands over the text.
        assert_eq!(&secret[f.start..f.start + f.len], "password");
    }
}
