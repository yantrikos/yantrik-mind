//! ARCH-3 slice 2 — egress-clean tool planning + the exact-value exfil guard.
//!
//! Extracted from the (very large) `lib.rs` so the egress-confidentiality logic lives on its own.
//! These are methods on [`super::ConversationEngine`] (a child module may impl a parent type and
//! reach its private fields) plus one free helper. See `docs/ARCH3_SLICE2_EGRESS_CLEAN_PLANNING.md`
//! for the full mechanism and the honest scope/residual-leak notes.

use yantrik_ml::{ChatMessage, GenerationConfig};

use super::{ConversationEngine, TurnIdentity};

/// E.EGRESS3/3b (Pranab's decisions, 5 Oct: "Your words + named files", then "Exact files only"):
/// what a person handed over in ONE conversation -- the files whose own path they wrote, and the text
/// the Mind read from exactly those files -- which an outbound web query may draw on. Kept per
/// person and per desktop conversation (the review's H3), so a household member's turn neither sees
/// nor names into another's, and a new desktop chat starts empty.
#[derive(Default)]
pub(crate) struct HandedOver {
    stores: std::collections::HashMap<String, Store>,
}

#[derive(Default)]
struct Store {
    /// Absolute file paths the person named, and when (the lapse is fixed then; it never slides).
    named: Vec<(String, u64)>,
    /// Named files the Mind itself saved to in this conversation: no longer the person's text.
    mind_written: std::collections::HashSet<String>,
    /// Text read from a named file: (path, text). Expires with its path.
    texts: Vec<(String, String)>,
}

/// E.EGRESS3: how long a named file, and the text read from it, stays a permitted source.
pub(crate) const HANDED_OVER_MS: u64 = 12 * 3600 * 1000;
/// E.EGRESS3b: the most text kept per file, the most files, and what the planner is shown.
const HANDED_TEXT_CAP: usize = 20_000;
const HANDED_FILES_MAX: usize = 8;
const PLANNER_HANDED_CAP: usize = 8_000;
const PLANNER_WEB_CAP: usize = 4_000;

/// E.EGRESS3b: words that make a mention of a path a refusal, not a hand-over ("don't touch ~/.ssh").
const NEGATIONS: [&str; 8] = ["don't", "dont", "do not", "never", "not ", "without", "except", "avoid"];

impl Store {
    fn fresh(&mut self, now: u64) {
        self.named.retain(|(_, at)| now.saturating_sub(*at) < HANDED_OVER_MS);
        let live: Vec<String> = self.named.iter().map(|(p, _)| p.clone()).collect();
        self.texts.retain(|(p, _)| live.contains(p));
    }
}

impl HandedOver {
    /// The files a person's message names: their own exact paths, not folders (a path ending in `/`
    /// names a folder), never one holding `..`, never one after a negation.
    pub(crate) fn note_named(&mut self, key: &str, user_text: &str, home: Option<&str>, now: u64) {
        let store = self.stores.entry(key.to_string()).or_default();
        store.fresh(now);
        let lower = user_text.to_lowercase();
        for (at, p) in crate::desktop::path_mentions(user_text) {
            if p.ends_with('/') || p.split('/').any(|seg| seg == "..") {
                continue;
            }
            let before: String = lower[..at.min(lower.len())].chars().rev().take(24).collect::<Vec<_>>().into_iter().rev().collect();
            if NEGATIONS.iter().any(|n| before.contains(n)) {
                continue;
            }
            let abs = crate::desktop::absolute(p, home);
            if !store.named.iter().any(|(n, _)| *n == abs) {
                store.named.push((abs, now));
            }
        }
    }

    /// The Mind saved to `path`: if it was a named file, it is no longer the person's text.
    pub(crate) fn note_written(&mut self, key: &str, path: &str, home: Option<&str>) {
        let abs = crate::desktop::absolute(path, home);
        let store = self.stores.entry(key.to_string()).or_default();
        store.texts.retain(|(p, _)| *p != abs);
        store.mind_written.insert(abs);
    }

    /// Text the EDITOR reported reading from `editor_path`: kept only when that exact file was named
    /// in this conversation and the Mind has not written to it. Pages of one file accumulate.
    pub(crate) fn note_read(&mut self, key: &str, editor_path: &str, text: &str, home: Option<&str>, now: u64) -> bool {
        let abs = crate::desktop::absolute(editor_path, home);
        if editor_path.split('/').any(|seg| seg == "..") {
            return false;
        }
        let store = self.stores.entry(key.to_string()).or_default();
        store.fresh(now);
        if store.mind_written.contains(&abs) || !store.named.iter().any(|(n, _)| *n == abs) {
            return false;
        }
        match store.texts.iter_mut().find(|(p, _)| *p == abs) {
            Some((_, kept)) => {
                if !kept.contains(text) {
                    kept.push('\n');
                    kept.push_str(text);
                    *kept = kept.chars().take(HANDED_TEXT_CAP).collect();
                }
            }
            None => {
                if store.texts.len() >= HANDED_FILES_MAX {
                    return false;
                }
                store.texts.push((abs, text.chars().take(HANDED_TEXT_CAP).collect()));
            }
        }
        true
    }

    /// Each handed-over file's text, as separate sources.
    pub(crate) fn texts(&mut self, key: &str, now: u64) -> Vec<String> {
        match self.stores.get_mut(key) {
            Some(store) => {
                store.fresh(now);
                store.texts.iter().map(|(_, t)| t.clone()).collect()
            }
            None => Vec::new(),
        }
    }

    /// The file names and paths named in this conversation -- never to leave inside a query.
    pub(crate) fn named_names(&self, key: &str) -> Vec<String> {
        self.stores.get(key).map_or_else(Vec::new, |s| {
            s.named
                .iter()
                .flat_map(|(p, _)| [p.clone(), p.rsplit('/').next().unwrap_or(p).to_string()])
                .collect()
        })
    }

    pub(crate) fn clear(&mut self, key: &str) {
        self.stores.remove(key);
    }
}

/// E.EGRESS3b: text as the span check compares it -- lower case, every run of characters that are
/// not letters or digits made one space.
fn norm(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// E.EGRESS3b (the review's H1): may this query leave as the model wrote it? Only when, normalised, it
/// is ONE contiguous run of whole words inside ONE source -- the person's message, one handed-over
/// file, or one web observation -- and holds no path. Every word counts, short ones and digits
/// included. A query of words gathered from here and there is the planner's to write.
pub(crate) fn query_is_a_span_of_one(query: &str, sources: &[&str], named: &[String]) -> bool {
    if has_path(query, named) {
        return false;
    }
    let q = norm(query);
    if q.is_empty() {
        return false;
    }
    let needle = format!(" {q} ");
    sources.iter().any(|s| format!(" {} ", norm(s)).contains(&needle))
}

/// E.EGRESS3b (the review's M1): is this word a path on this machine, or a named file?
fn is_path_word(w: &str, named: &[String]) -> bool {
    let w = w.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';'));
    let w = w.trim_end_matches(['.', ':']);
    if w.is_empty() {
        return false;
    }
    let lower = w.to_ascii_lowercase();
    let web = lower.starts_with("http://") || lower.starts_with("https://");
    let drive = w.len() >= 3 && w.as_bytes()[0].is_ascii_alphabetic() && &w[1..3] == ":\\";
    lower.starts_with('~')
        || lower.starts_with("./")
        || lower.starts_with("../")
        || lower.starts_with("file:")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || drive
        || (!web && (w.contains('/') || w.contains('\\')))
        || named.iter().any(|n| n.eq_ignore_ascii_case(w))
}

/// E.EGRESS3b (M1): does a url carry a path on this machine anywhere in it -- `file:`, `~/`, a home
/// or system folder, an encoded slash, or a named file? A web address is checked as a whole.
fn url_holds_path(url: &str, named: &[String]) -> bool {
    let lower = url.to_ascii_lowercase();
    ["file:", "~/", "/home/", "/root/", "/etc/", "/var/", "/users/", "/tmp/", "%2f", "%5c", ":\\"]
        .iter()
        .any(|m| lower.contains(m))
        || named.iter().any(|n| !n.is_empty() && lower.contains(&n.to_ascii_lowercase()))
}

/// E.EGRESS3b: does a text carry a path, or a named file's path or name, in any of its words?
fn has_path(q: &str, named: &[String]) -> bool {
    q.split_whitespace().any(|w| is_path_word(w, named))
}

/// E.EGRESS3: the web tools -- search and fetch -- whose output a web query may draw its words from.
pub(crate) fn is_web_tool(tool: &str) -> bool {
    matches!(tool, "search" | "web_search" | "google" | "ddg" | "wikipedia" | "wiki" | "web_fetch" | "fetch" | "web")
}

/// E.EGRESS3b: the text without its path words.
pub(crate) fn strip_local_paths(q: &str, named: &[String]) -> String {
    q.split_whitespace().filter(|w| !is_path_word(w, named)).collect::<Vec<_>>().join(" ")
}

/// Extract DISTINCTIVE, high-precision PII-shaped values from text — the only class the exact-value
/// exfil guard acts on (near-zero false positives). Catches: email addresses (`local@domain.tld`),
/// contiguous 7–15 digit numbers (phone / account / card, unseparated), and long (≥16-char)
/// alphanumeric tokens that mix letters and digits (ids/keys). Deliberately misses separated phone
/// numbers, names, and dates — those are low precision and are clean planning's job.
pub(crate) fn distinctive_pii(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | ','
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | ';'
                    | '/'
                    | '\\'
                    | ':'
                    | '='
                    | '&'
                    | '?'
                    | '|'
            )
    }) {
        let tok = raw.trim_matches(|c: char| {
            !c.is_alphanumeric() && c != '@' && c != '.' && c != '-' && c != '_' && c != '+'
        });
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
        let is_phone_like =
            tok.chars().all(|c| c.is_ascii_digit()) && (7..=15).contains(&tok.len());
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

/// Why the clean planner produced no outbound arguments (E.EGRESSMSG1). It never sees private
/// context, so it never refuses FOR privacy: every failure is the planner's own, and the refusal
/// the person reads names which one (VM 561 was told "without pulling in private context" when the
/// model had simply not answered).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CleanArgsFailure {
    /// The model call that authors the arguments errored.
    NoAnswer,
    /// It answered, but with no JSON argument object.
    NoUsableArgs,
}

impl CleanArgsFailure {
    pub(crate) fn why(self) -> &'static str {
        match self {
            CleanArgsFailure::NoAnswer => "the model that prepares outbound requests did not answer",
            CleanArgsFailure::NoUsableArgs => {
                "the model that prepares outbound requests returned no usable arguments"
            }
        }
    }
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
    ///
    /// `external_provenance` is what EXTERNAL services have already returned THIS turn (search
    /// results, fetched pages) — never private-tool output. An arg value found verbatim there, or in
    /// the user's literal request, passes through unchanged: the outside world already has it, so
    /// re-authoring it cannot protect anything — and measurably destroys it. Observed live
    /// 2026-08-16: a research turn's `web_fetch` of a URL taken from its own search results was
    /// re-authored by the clean planner (which, by design, never sees the work log) into invented
    /// search-engine URLs and twice into unfetchable garbage — six fetches of one article, none of
    /// them the article. The pass-through also restores DETERMINISM, which the loop's repeat-guard
    /// depends on: a re-authoring model call gives the same tool call a different signature each
    /// time, so the guard never fires on exactly the repeats this failure produces.
    /// E.EGRESS3b: whose hand-over this turn reads and writes -- the person, in this desktop chat.
    pub(crate) fn handed_key(id: &TurnIdentity) -> String {
        let chat = crate::TURN_CONVERSATION.try_with(|c| c.clone()).unwrap_or_default();
        format!("{}|{chat}", id.owner)
    }

    /// E.EGRESS3: keep text the editor read from a file the person named, as a permitted query source.
    pub(crate) fn note_handed_over(&self, key: &str, editor_path: &str, text: &str) {
        let home = self.person_home();
        if let Ok(mut h) = self.handed_over.lock() {
            if h.note_read(key, editor_path, text, home.as_deref(), Self::now_ms()) {
                eprintln!("[egress] {editor_path}: handed over by the person, {} chars kept as a query source", text.chars().count());
            }
        }
    }

    /// E.EGRESS3b: the Mind saved to `path` -- a named file it wrote is no longer the person's text.
    pub(crate) fn note_mind_written(&self, key: &str, path: &str) {
        let home = self.person_home();
        if let Ok(mut h) = self.handed_over.lock() {
            h.note_written(key, path, home.as_deref());
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn egress_clean_args(
        &self,
        tool: &str,
        user_text: &str,
        grounded: serde_json::Value,
        external_provenance: &str,
    ) -> Result<serde_json::Value, CleanArgsFailure> {
        self.egress_clean_args_with(tool, user_text, grounded, external_provenance, &[], "").await
    }

    /// E.EGRESS3: [`Self::egress_clean_args`] with what the WEB tools returned this turn (each
    /// observation separately), the only outside text a web query may take its words from, and whose
    /// hand-over (`key`, [`Self::handed_key`]) it may use.
    pub(crate) async fn egress_clean_args_with(
        &self,
        tool: &str,
        user_text: &str,
        grounded: serde_json::Value,
        external_provenance: &str,
        web_obs: &[String],
        key: &str,
    ) -> Result<serde_json::Value, CleanArgsFailure> {
        // Only active when the egress kernel is wired (keeps legacy/test paths unchanged).
        if self.egress.is_none() {
            return Ok(grounded);
        }
        // Eligible = external tools whose arg is a self-contained query/url/text authored from the
        // literal request. Contextual tools ("more like that") are deliberately excluded for now —
        // widen this set only as each connector's isolation boundary is proven with a test.
        let eligible = matches!(
            tool,
            "search"
                | "web_search"
                | "google"
                | "ddg"
                | "mail_search"
                | "mailsearch"
                | "search_mail"
                | "findmail"
                | "web_fetch"
                | "fetch"
                | "web"
                | "wikipedia"
                | "wiki"
                | "translate"
                | "tr"
        );
        if !eligible {
            return Ok(grounded);
        }
        if !matches!(
            mind_governance::egress::classify(tool),
            Some(mind_governance::egress::EgressClass::External(_))
        ) {
            return Ok(grounded);
        }
        // PROVENANCE PASS-THROUGH (scoped to the url-bearing fetch tools, where the breakage is
        // total): a URL the user typed, or that an external service returned this turn, is not a
        // private fact — dispatch it exactly as chosen. Queries stay clean-authored: they are built
        // from the user's request, which the clean planner CAN see, so it does its job there.
        if matches!(tool, "web_fetch" | "fetch" | "web") {
            if let Some(url) = grounded.get("url").and_then(|u| u.as_str()) {
                if !url.is_empty() && (user_text.contains(url) || external_provenance.contains(url))
                {
                    return Ok(grounded);
                }
            }
        }
        // E.EGRESS3 (Pranab, 5 Oct): a web search may also draw on the text of files the person named
        // in this conversation, and on what the outside world returned this turn. A query whose every
        // content word is already there leaves as the model wrote it -- deterministic, so the repeat
        // guard works -- and anything else is authored with that text in view.
        // E.EGRESS3b (the review's H1): it leaves as written only as ONE contiguous run of whole words
        // of ONE source; anything assembled from several is the planner's to write.
        let web_query = matches!(tool, "search" | "web_search" | "google" | "ddg" | "wikipedia" | "wiki");
        let (handed, named) = if key.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            let (texts, named) = self
                .handed_over
                .lock()
                .map(|mut h| (h.texts(key, Self::now_ms()), h.named_names(key)))
                .unwrap_or_default();
            // Only a web search reads the handed-over text; every tool keeps the named paths out.
            (if web_query { texts } else { Vec::new() }, named)
        };
        if web_query {
            let q = ["query", "q", "topic"].iter().find_map(|k| grounded.get(*k).and_then(|v| v.as_str()));
            if let Some(q) = q {
                let mut sources: Vec<&str> = vec![user_text];
                sources.extend(handed.iter().map(String::as_str));
                sources.extend(web_obs.iter().map(String::as_str));
                if query_is_a_span_of_one(q, &sources, &named) {
                    // The review's L3: the query, and nothing else the model put beside it.
                    return Ok(serde_json::json!({ "query": q }));
                }
            }
        }
        let schema = match tool {
            "web_fetch" | "fetch" | "web" => "{\"url\": \"<the URL, taken ONLY from the user's literal request>\"}",
            "translate" | "tr" => "{\"to\": \"<target language>\", \"text\": \"<the text to translate, from the literal request>\"}",
            // E.EGRESS3b: keywords -- R1c's planner wrote sentence-long queries that a search engine
            // answered with Wikipedia "Machine".
            _ => "{\"query\": \"<3 to 8 search keywords built ONLY from the sources shown>\"}",
        };
        let sys = "You author the ARGUMENTS for an OUTBOUND tool call that will LEAVE this device and \
            reach an external service. You have NO access to the user's private memory, notes, files, or \
            prior conversation. You MUST NOT invent or add any personal detail (names, dates, health, \
            finances, addresses, account numbers) that is not present VERBATIM in the user's literal \
            request below. Build the argument ONLY from the literal request (and, for a web search, the \
            text the person handed over, when it is shown). Never put a file path from this machine \
            (~/... or /home/...) in a query. Output ONLY one JSON object.";
        let handed_all = handed.join("\n\n");
        let handed_part = if handed_all.trim().is_empty() {
            String::new()
        } else {
            let excerpt: String = handed_all.chars().take(PLANNER_HANDED_CAP).collect();
            format!("\nText of the files the person handed over (you MAY take query terms from it):\n{excerpt}\n")
        };
        // E.EGRESS3b (the review's H1): what the web returned this turn, newest last, capped -- so the
        // planner can follow a lead the results gave (a name, a paper) without the model's own words.
        let web_part = if web_query && !web_obs.is_empty() {
            let all = web_obs.join("\n");
            let tail: String = all.chars().rev().take(PLANNER_WEB_CAP).collect::<Vec<_>>().into_iter().rev().collect();
            format!("\nWhat web searches returned this turn (you MAY take query terms from it):\n{tail}\n")
        } else {
            String::new()
        };
        let user = format!("Tool: {tool}\nArgument shape: {schema}\nUser's literal request: {user_text}\n{handed_part}{web_part}\nOutput ONLY the JSON args.");
        let cfg = GenerationConfig {
            max_tokens: 300,
            ..GenerationConfig::default()
        };
        // A plain (ungrounded) call — no private lane, no grounding, a FRESH message list with no
        // shared state carrying private context.
        let text = self
            .inference
            .chat_household_attributed(
                vec![ChatMessage::system(sys), ChatMessage::user(&user)],
                cfg,
                concat!(module_path!(), ":egress-clean"),
            )
            .await
            .map_err(|e| {
                eprintln!("[egress] clean planner for {tool} did not answer: {e}");
                CleanArgsFailure::NoAnswer
            })?
            .text;
        let body_owned = crate::strip_reasoning(&text);
        let body = body_owned.as_str();
        let obj = match (body.find('{'), body.rfind('}')) {
            (Some(a), Some(b)) if b > a => &body[a..=b],
            _ => {
                eprintln!("[egress] clean planner for {tool} returned no JSON ({} chars)", body.len());
                return Err(CleanArgsFailure::NoUsableArgs); // fail closed
            }
        };
        match serde_json::from_str::<serde_json::Value>(obj) {
            Ok(mut parsed) if parsed.is_object() => {
                let _ = grounded; // grounded args are intentionally DISCARDED for eligible egress tools
                // E.EGRESS3b (the review's M1): no path on this machine, and no named file's path or
                // name, leaves in ANY argument. Taken out of text; a url carrying one is refused.
                if let Some(obj) = parsed.as_object_mut() {
                    for (k, v) in obj.iter_mut() {
                        let Some(text) = v.as_str().map(str::to_string) else { continue };
                        if k == "url" {
                            if url_holds_path(&text, &named) {
                                eprintln!("[egress] clean planner for {tool} wrote a url holding a local path \u{2014} refused");
                                return Err(CleanArgsFailure::NoUsableArgs);
                            }
                            continue;
                        }
                        if has_path(&text, &named) {
                            *v = serde_json::Value::String(strip_local_paths(&text, &named));
                        }
                    }
                }
                Ok(parsed)
            }
            _ => {
                eprintln!("[egress] clean planner for {tool} returned JSON that is not an argument object");
                Err(CleanArgsFailure::NoUsableArgs)
            }
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
    pub(crate) async fn model_injected_private_value(
        &self,
        tool: &str,
        args: &serde_json::Value,
        user_text: &str,
        id: &TurnIdentity,
    ) -> Option<String> {
        if !matches!(
            mind_governance::egress::classify(tool),
            Some(mind_governance::egress::EgressClass::External(_))
        ) {
            return None;
        }
        let canon = mind_governance::egress::canonicalize(args);
        let user_lc = user_text.to_lowercase();
        let ctx = mind_types::AccessContext::principal(
            id.viewer(),
            mind_types::Purpose::conversation(&id.owner),
        );
        for value in distinctive_pii(&canon) {
            let vlc = value.to_lowercase();
            if user_lc.contains(&vlc) {
                continue; // the user typed it themselves — their call, not a model exfil
            }
            // Is this exact value a stored fact the speaker's memory holds? (substring-confirm, not
            // just word-overlap, so we don't false-positive on a shared token.)
            if let Ok(hits) = self.memory.beliefs_matching(&value, &ctx).await {
                if hits
                    .iter()
                    .any(|b| b.statement.to_lowercase().contains(&vlc))
                {
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
        assert!(
            vals.iter().any(|v| v == "ABC123DEF456GHI7"),
            "long mixed id"
        );
        assert!(!vals.iter().any(|v| v == "word"), "plain words are not PII");
        assert!(
            distinctive_pii("meet at 7 on July 4 in Pune").is_empty(),
            "dates/short numbers are not distinctive PII"
        );
    }
}
