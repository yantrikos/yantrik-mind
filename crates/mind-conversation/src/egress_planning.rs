//! ARCH-3 slice 2 — egress-clean tool planning + the exact-value exfil guard.
//!
//! Extracted from the (very large) `lib.rs` so the egress-confidentiality logic lives on its own.
//! These are methods on [`super::ConversationEngine`] (a child module may impl a parent type and
//! reach its private fields) plus one free helper. See `docs/ARCH3_SLICE2_EGRESS_CLEAN_PLANNING.md`
//! for the full mechanism and the honest scope/residual-leak notes.

use yantrik_ml::{ChatMessage, GenerationConfig};

use super::{ConversationEngine, TurnIdentity};

/// Extract DISTINCTIVE, high-precision PII-shaped values from text — the only class the exact-value
/// exfil guard acts on (near-zero false positives). Catches: email addresses (`local@domain.tld`),
/// contiguous 7–15 digit numbers (phone / account / card, unseparated), and long (≥16-char)
/// alphanumeric tokens that mix letters and digits (ids/keys). Deliberately misses separated phone
/// numbers, names, and dates — those are low precision and are clean planning's job.
pub(crate) fn distinctive_pii(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| c.is_whitespace() || matches!(c, '"' | ',' | '{' | '}' | '[' | ']' | '(' | ')' | '<' | '>' | ';' | '/' | '\\' | ':' | '=' | '&' | '?' | '|')) {
        let tok = raw.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.' && c != '-' && c != '_' && c != '+');
        if tok.len() < 7 {
            continue;
        }
        let is_email = {
            if let Some(at) = tok.find('@') {
                at > 0 && tok[at + 1..].contains('.') && !tok[at + 1..].ends_with('.')
            } else {
                false
            }
        };
        let digits = tok.chars().filter(|c| c.is_ascii_digit()).count();
        let is_phone_like = tok.chars().all(|c| c.is_ascii_digit()) && (7..=15).contains(&tok.len());
        let is_long_id = tok.len() >= 16
            && tok.chars().all(|c| c.is_ascii_alphanumeric())
            && digits > 0
            && tok.chars().any(|c| c.is_ascii_alphabetic());
        if is_email || is_phone_like || is_long_id {
            let v = tok.to_string();
            if !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
}

impl ConversationEngine {
    /// ARCH-3 slice 2 — EGRESS-CLEAN TOOL PLANNING. For an outbound tool whose argument is a
    /// self-contained query/url the model can build from the LITERAL request, RE-AUTHOR the argument
    /// in a SEPARATE, STATELESS model call that never saw the private grounding, working-set, people
    /// layer, rolling summary, or work-log — so a private fact the grounded model saw can never be
    /// written into what actually leaves the device. The grounded model still chooses WHICH tool; a
    /// clean model authors the ARG that reaches the connector; the broker then mediates the result.
    ///
    /// Returns the args to dispatch. `None` = fail-closed refusal (an eligible egress tool whose clean
    /// authoring could not produce usable args — better to refuse than fall back to grounded args that
    /// might carry a private fact). A non-eligible tool returns its grounded args unchanged (these are
    /// documented as NOT-yet-egress-clean-planned; widen `eligible` as each connector is proven).
    ///
    /// HONEST RESIDUAL LEAKS (per the slice-2 spec, NOT covered here): the user's literal request may
    /// itself contain a private detail; the clean model's pretraining; values a later local tool-result
    /// introduces. Clean planning is complementary to the credential/value tripwire, not a total guard.
    pub(crate) async fn egress_clean_args(&self, tool: &str, user_text: &str, grounded: serde_json::Value) -> Option<serde_json::Value> {
        // Only active when the egress kernel is wired (keeps legacy/test paths unchanged).
        if self.egress.is_none() {
            return Some(grounded);
        }
        // Eligible = external tools whose arg is a self-contained query/url/text authored from the
        // literal request. Contextual tools ("more like that") are deliberately excluded for now —
        // widen this set only as each connector's isolation boundary is proven with a test.
        let eligible = matches!(
            tool,
            "search" | "web_search" | "google" | "ddg" | "mail_search" | "mailsearch" | "search_mail"
                | "findmail" | "web_fetch" | "fetch" | "web" | "wikipedia" | "wiki" | "translate" | "tr"
        );
        if !eligible {
            return Some(grounded);
        }
        if !matches!(mind_governance::egress::classify(tool), Some(mind_governance::egress::EgressClass::External(_))) {
            return Some(grounded);
        }
        let schema = match tool {
            "web_fetch" | "fetch" | "web" => "{\"url\": \"<the URL, taken ONLY from the user's literal request>\"}",
            "translate" | "tr" => "{\"to\": \"<target language>\", \"text\": \"<the text to translate, from the literal request>\"}",
            _ => "{\"query\": \"<a concise query built ONLY from the user's literal request>\"}",
        };
        let sys = "You author the ARGUMENTS for an OUTBOUND tool call that will LEAVE this device and \
            reach an external service. You have NO access to the user's private memory, notes, files, or \
            prior conversation. You MUST NOT invent or add any personal detail (names, dates, health, \
            finances, addresses, account numbers) that is not present VERBATIM in the user's literal \
            request below. Build the argument ONLY from the literal request. Output ONLY one JSON object.";
        let user = format!("Tool: {tool}\nArgument shape: {schema}\nUser's literal request: {user_text}\n\nOutput ONLY the JSON args.");
        let cfg = GenerationConfig { max_tokens: 300, ..GenerationConfig::default() };
        // A plain (ungrounded) call — no private lane, no grounding, a FRESH message list with no
        // shared state carrying private context.
        let text = self.inference.chat(vec![ChatMessage::system(sys), ChatMessage::user(&user)], cfg).await.ok()?.text;
        let body = text.rsplit("</think>").next().unwrap_or(&text);
        let obj = match (body.find('{'), body.rfind('}')) {
            (Some(a), Some(b)) if b > a => &body[a..=b],
            _ => return None, // fail closed: no usable JSON from the clean planner
        };
        let parsed: serde_json::Value = serde_json::from_str(obj).ok()?;
        if parsed.is_object() {
            let _ = grounded; // grounded args are intentionally DISCARDED for eligible egress tools
            Some(parsed)
        } else {
            None
        }
    }

    /// ARCH-3 slice 2 — the high-precision EXACT-VALUE exfil guard (sol's complementary layer to
    /// egress-clean planning). It catches the leak clean planning can't: a distinctive stored value
    /// the GROUNDED model injected into a NON-clean-planned external tool's args (an MCP read, a
    /// github/translate/coder call) that the user did NOT type. Precise signal, near-zero false
    /// positives: a value that is (1) PII-shaped (email / 7–15-digit number / long alphanumeric id),
    /// (2) present verbatim in the outbound args, (3) present verbatim in the speaker's typed memory,
    /// and (4) NOT in the user's literal request = the model reproducing a stored private value the
    /// user never asked to send. Returns a GENERIC reason (never names the value — no oracle) or None.
    ///
    /// Deliberately narrow (high precision over recall, per sol): separated phone numbers, paraphrase,
    /// encoding, and non-PII-shaped facts are NOT caught here — that residue is clean planning's job
    /// (for eligible tools) and remains open for the rest (documented).
    pub(crate) async fn model_injected_private_value(&self, tool: &str, args: &serde_json::Value, user_text: &str, id: &TurnIdentity) -> Option<String> {
        if !matches!(mind_governance::egress::classify(tool), Some(mind_governance::egress::EgressClass::External(_))) {
            return None;
        }
        let canon = mind_governance::egress::canonicalize(args);
        let user_lc = user_text.to_lowercase();
        let ctx = mind_types::AccessContext::Principal(id.viewer());
        for value in distinctive_pii(&canon) {
            let vlc = value.to_lowercase();
            if user_lc.contains(&vlc) {
                continue; // the user typed it themselves — their call, not a model exfil
            }
            // Is this exact value a stored fact the speaker's memory holds? (substring-confirm, not
            // just word-overlap, so we don't false-positive on a shared token.)
            if let Ok(hits) = self.memory.beliefs_matching(&value, &ctx).await {
                if hits.iter().any(|b| b.statement.to_lowercase().contains(&vlc)) {
                    return Some(format!(
                        "(that would send what looks like a stored private detail out through `{tool}`, and you didn't include it in your request — I'll hold off. Tell me the exact terms to send if that's intended.)"
                    ));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::distinctive_pii;

    #[test]
    fn distinctive_pii_extracts_only_high_precision_values() {
        let vals = distinctive_pii("email a.b@ex.com call 5551234567 id ABC123DEF456GHI7 word");
        assert!(vals.iter().any(|v| v == "a.b@ex.com"), "email");
        assert!(vals.iter().any(|v| v == "5551234567"), "unseparated phone");
        assert!(vals.iter().any(|v| v == "ABC123DEF456GHI7"), "long mixed id");
        assert!(!vals.iter().any(|v| v == "word"), "plain words are not PII");
        assert!(distinctive_pii("meet at 7 on July 4 in Pune").is_empty(), "dates/short numbers are not distinctive PII");
    }
}
