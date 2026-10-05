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
    /// E.ERASE4 (the redact review's note 2): the person's own messages in this conversation, newest
    /// last, canonical -- a forget question quotes only words the person already put here.
    said: Vec<String>,
}

/// E.ERASE4: how many of the person's messages a conversation keeps for that check.
const SAID_MAX: usize = 50;

/// E.EGRESS3: how long a named file, and the text read from it, stays a permitted source.
pub(crate) const HANDED_OVER_MS: u64 = 12 * 3600 * 1000;
/// E.EGRESS3b: the most text kept per file, the most files, and what the planner is shown.
const HANDED_TEXT_CAP: usize = 20_000;
const HANDED_FILES_MAX: usize = 8;
const PLANNER_HANDED_CAP: usize = 8_000;
const PLANNER_WEB_CAP: usize = 4_000;

/// E.EGRESS3d (the re-review's N5): a path is handed over only by a sentence that asks for it -- a
/// positive verb before it -- and that negates nothing. Matched as whole words of normalised text
/// (apostrophes of either kind, every other punctuation mark a space).
const HAND_OVER_VERBS: [&str; 15] = [
    "read", "open", "use", "see", "here s", "here is", "attached", "review", "study", "summarise", "summarize",
    "analyse", "analyze", "check", "look at",
];
const NEGATIONS: [&str; 21] = [
    "don t", "dont", "do not", "never", "not", "no", "without", "except", "avoid", "ignore", "skip",
    "shouldn t", "should not", "stay out", "off limits", "can t", "cannot", "won t", "mustn t", "doesn t",
    "isn t",
];

/// E.EGRESS3d: the sentence around byte `at`, bounded by ". ", "!", "?" or a newline -- and the part
/// of it before the path. E.EGRESS3e (A1): every slice goes through `get`; an offset that is not a
/// character boundary gives None, and None hands nothing over.
pub(crate) fn sentence_around(text: &str, at: usize, len: usize) -> Option<(&str, &str)> {
    let is_end = |i: usize| {
        text.get(i..).is_some_and(|rest| {
            rest.starts_with(". ") || rest.starts_with('!') || rest.starts_with('?') || rest.starts_with('\n')
        })
    };
    let head = text.get(..at)?;
    let start = head.char_indices().rev().find(|(i, _)| is_end(*i)).map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
    let from = (at + len).min(text.len());
    let tail = text.get(from..)?;
    let end = tail.char_indices().find(|(i, _)| is_end(from + i)).map(|(i, _)| from + i).unwrap_or(text.len());
    Some((text.get(start..end)?, text.get(start..at)?))
}

/// E.EGRESS3d: text as whole words, lower case, every apostrophe (' and ’ alike) and all other
/// punctuation a space -- "don’t" and "don't" are both "don t".
fn words_of(s: &str) -> String {
    format!(" {} ", norm(s))
}

/// E.EGRESS3d (N5, N2): does this mention of `path` hand it over? Lowercasing happens after the
/// slicing, never before (N2: "ẞ" lowercases to a different byte length).
fn hands_over(text: &str, at: usize, path: &str) -> bool {
    let Some((sentence, before)) = sentence_around(text, at, path.len()) else { return false };
    let (sentence, before) = (words_of(sentence), words_of(before));
    HAND_OVER_VERBS.iter().any(|v| before.contains(&format!(" {v} ")))
        && !NEGATIONS.iter().any(|n| sentence.contains(&format!(" {n} ")))
}

/// E.EGRESS3e: is byte `at` inside text the person quoted rather than wrote -- a line starting with
/// `>`, a ``` fence, or anything after "Forwarded message" / "Original Message"? A file named there
/// was named by someone else.
fn in_quoted_text(text: &str, at: usize) -> bool {
    let mut fenced = false;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let lower = line.to_lowercase();
        let lower_trim = lower.trim();
        // E.EGRESS3f: a mail client's reply header ("On Mon, … wrote:", "From: …") starts quoted text too.
        if lower.contains("forwarded message")
            || lower.contains("original message")
            || (lower_trim.starts_with("on ") && lower_trim.ends_with("wrote:"))
            || lower_trim.starts_with("from:")
        {
            return at >= offset;
        }
        let fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        // E.EGRESS3f: an indented block (4 spaces or a tab) is pasted text, as markdown reads it.
        let indented = line.starts_with("    ") || line.starts_with('\t');
        if at >= offset && at < offset + line.len() {
            return fenced || fence || trimmed.starts_with('>') || indented;
        }
        if fence {
            fenced = !fenced;
        }
        offset += line.len();
    }
    false
}

/// E.EGRESS3d (N5): a dotfile, a dot-folder, or a key-like name is never handed over.
fn is_secret_like(p: &str) -> bool {
    let name = p.rsplit('/').next().unwrap_or(p).to_ascii_lowercase();
    p.split('/').any(|seg| seg.starts_with('.') && seg != "." && seg != "..")
        || name.starts_with("id_")
        || [".pem", ".key", ".p12", ".pfx", ".kdbx"].iter().any(|e| name.ends_with(e))
}

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
        // E.ERASE4: the person's message, once (both loops note the same turn).
        let said = crate::erase_redact::canon(user_text);
        if store.said.last() != Some(&said) {
            store.said.push(said);
            let over = store.said.len().saturating_sub(SAID_MAX);
            store.said.drain(..over);
        }
        for (at, p) in crate::desktop::path_mentions(user_text) {
            if p.ends_with('/') || p.split('/').any(|seg| seg == "..") || is_secret_like(p) {
                continue;
            }
            if in_quoted_text(user_text, at) || !hands_over(user_text, at, p) {
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

    /// E.EGRESS3f (S2): every file named in this conversation is no longer the person's text.
    pub(crate) fn note_all_written(&mut self, key: &str) {
        if let Some(store) = self.stores.get_mut(key) {
            store.texts.clear();
            let named: Vec<String> = store.named.iter().map(|(p, _)| p.clone()).collect();
            store.mind_written.extend(named);
        }
    }

    /// E.EGRESS3d (N4): a terminal command or Files action whose text mentions a named file -- its
    /// path or its name -- is taken to have written it. Returns the files so marked.
    pub(crate) fn note_written_if_mentioned(&mut self, key: &str, text: &str) -> Vec<String> {
        let Some(store) = self.stores.get_mut(key) else { return Vec::new() };
        let lower = text.to_lowercase();
        let hit: Vec<String> = store
            .named
            .iter()
            .map(|(p, _)| p.clone())
            .filter(|p| {
                let name = p.rsplit('/').next().unwrap_or(p);
                lower.contains(&p.to_lowercase()) || (!name.is_empty() && lower.contains(&name.to_lowercase()))
            })
            .collect();
        for p in &hit {
            store.texts.retain(|(t, _)| t != p);
            store.mind_written.insert(p.clone());
        }
        hit
    }

    /// E.EGRESS3e (A3): when the person named this file (ms), if they did, in this conversation.
    pub(crate) fn named_at(&self, key: &str, abs: &str) -> Option<u64> {
        self.stores.get(key)?.named.iter().find(|(n, _)| n == abs).map(|(_, at)| *at)
    }

    /// E.EGRESS3e (A3): the files whose text is kept, with when each was named.
    pub(crate) fn kept_files(&self, key: &str) -> Vec<(String, u64)> {
        self.stores.get(key).map_or_else(Vec::new, |s| {
            s.texts.iter().filter_map(|(p, _)| s.named.iter().find(|(n, _)| n == p).cloned()).collect()
        })
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

    /// E.ERASE4: has the person put these exact words (canonical) in this conversation?
    pub(crate) fn person_said(&self, key: &str, text: &str) -> bool {
        let needle = crate::erase_redact::canon(text);
        !needle.is_empty() && self.stores.get(key).is_some_and(|s| s.said.iter().any(|m| m.contains(&needle)))
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
    // E.EGRESS3d (N1): by characters -- a byte slice panicked on "A–Z".
    let drive = w.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && w.get(1..3) == Some(":\\");
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
pub(crate) fn url_holds_path(url: &str, named: &[String]) -> bool {
    // E.EGRESS3d (N6): decoded until it stops changing -- %252F is %2F is /.
    let mut decoded = url.to_string();
    for _ in 0..4 {
        let next = percent_decode(&decoded);
        if next == decoded {
            break;
        }
        decoded = next;
    }
    // E.EGRESS3e (A2): still encoded after the last round is refused -- five rounds of /home/p/x
    // read as "%2Fhome%2Fp%2Fx" after four and went out.
    if percent_decode(&decoded) != decoded {
        return true;
    }
    let lower = decoded.to_lowercase();
    let marker = [
        "file:", "~/", "~\\", "/home/", "/root/", "/etc/", "/var/", "/users/", "/tmp/", "/mnt/", "/opt/", "/srv/",
        "/media/", "/run/", ":\\", "%2f", "%5c", "%7e",
    ]
    .iter()
    .any(|m| lower.contains(m));
    // A query or fragment value that is itself a path ("?f=./notes.md", "#../x").
    let (before, after) = lower.split_once(['?', '#']).unwrap_or((&lower, ""));
    let value_is_path = after.split(['&', '=', '#']).any(|v| !v.is_empty() && is_path_word(v, &[]));
    // E.EGRESS3e (A2): and each segment of the url's own path ("/~x/", "/c:\\…").
    let path = before.split_once("://").map_or(before, |(_, rest)| rest.split_once('/').map_or("", |(_, p)| p));
    let segment_is_path = path.split('/').any(|seg| !seg.is_empty() && is_path_word(seg, &[]));
    marker || value_is_path || segment_is_path || named.iter().any(|n| !n.is_empty() && lower.contains(&n.to_lowercase()))
}

/// E.EGRESS3d (N6): one round of percent-decoding (bytes, then UTF-8, lossily).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16)) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// E.EGRESS3b: does a text carry a path, or a named file's path or name, in any of its words?
fn has_path(q: &str, named: &[String]) -> bool {
    q.split_whitespace().any(|w| is_path_word(w, named))
}

/// E.EGRESS3e (A3): the file's ctime (unix seconds, `files_stat`'s `changed`) is before the second
/// the person named it (ms). Strictly before: a write in that same second would pass otherwise.
pub(crate) fn unchanged_since(changed: u64, named_at_ms: Option<u64>) -> bool {
    // E.EGRESS3f (S1): and a ctime of 0 is not a time.
    changed > 0 && named_at_ms.is_some_and(|at| changed < at / 1000)
}

/// E.EGRESS3e: the tools whose query may be planned from the handed-over text.
pub(crate) fn plans_from_handed(tool: &str) -> bool {
    matches!(tool, "search" | "web_search" | "google" | "ddg" | "wikipedia" | "wiki")
}

/// E.EGRESS4: the http(s) addresses in a text, in order, without trailing punctuation.
pub(crate) fn urls_in(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '`'))
        .map(|w| w.trim_end_matches(['.', ',', ';', ':', '!', '?']))
        .filter(|w| {
            let lower = w.to_ascii_lowercase();
            (lower.starts_with("http://") || lower.starts_with("https://")) && w.split_once("://").is_some_and(|(_, rest)| !rest.is_empty())
        })
        .map(str::to_string)
        .collect()
}

/// E.EGRESS4: an address without its query and fragment -- what `?v=alice` and `?v=bob` share.
fn without_query(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// E.EGRESS4: an address up to its last path segment -- what `/p/alice` and `/p/bob` share.
fn variant_group(url: &str) -> String {
    let base = without_query(url).trim_end_matches('/');
    match base.split_once("://") {
        Some((scheme, rest)) => match rest.rsplit_once('/') {
            Some((head, _)) => format!("{}://{}/", scheme.to_ascii_lowercase(), head.to_ascii_lowercase()),
            None => format!("{}://{}/", scheme.to_ascii_lowercase(), rest.to_ascii_lowercase()),
        },
        None => base.to_ascii_lowercase(),
    }
}

/// E.EGRESS4: a page offering more variants of one address than this is a menu, not a source.
const VARIANT_MENU: usize = 20;
/// E.EGRESS4: the most candidates the planner is shown (the newest), and fetches per query-variant.
const FETCH_CANDIDATES: usize = 40;
const FETCHES_PER_PAGE: usize = 2;

/// E.EGRESS4: the addresses a fetch may go to, newest last -- those in the person's words and in what
/// the WEB tools returned this turn, never another tool's. From each result page, a group of more
/// than [`VARIANT_MENU`] variants is dropped whole; an address holding a local path is dropped.
pub(crate) fn fetch_candidates(user_text: &str, web_obs: &[String], named: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |u: String| {
        if !url_holds_path(&u, named) && !out.contains(&u) {
            out.push(u);
        }
    };
    for u in urls_in(user_text) {
        push(u);
    }
    for page in web_obs {
        let urls = urls_in(page);
        let mut groups: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for u in &urls {
            *groups.entry(variant_group(u)).or_default() += 1;
        }
        for u in urls {
            if groups.get(&variant_group(&u)).copied().unwrap_or(0) <= VARIANT_MENU {
                push(u);
            }
        }
    }
    let skip = out.len().saturating_sub(FETCH_CANDIDATES);
    out.into_iter().skip(skip).collect()
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
    /// E.EGRESS3g (R1d on VM 520): a fetch whose address came from neither the person's words nor
    /// this turn's outside results -- the planner cannot write one, and wrote "" three times running.
    UrlNotFromSources,
    /// E.EGRESS4: a third fetch this turn of one page that differs only in its query or fragment.
    FetchCapReached,
}

impl CleanArgsFailure {
    pub(crate) fn why(self) -> &'static str {
        match self {
            CleanArgsFailure::NoAnswer => "the model that prepares outbound requests did not answer",
            CleanArgsFailure::NoUsableArgs => {
                "the model that prepares outbound requests returned no usable arguments"
            }
            CleanArgsFailure::FetchCapReached => {
                "two pages that differ only in their query were already fetched from that address this turn; use what they gave"
            }
            CleanArgsFailure::UrlNotFromSources => {
                "that address is not in the person's words or in what a search returned this turn; search first, then fetch an address the results give"
            }
        }
    }
}

impl ConversationEngine {
    /// E.EGRESS3b: whose hand-over this turn reads and writes -- the person, in this desktop chat.
    pub(crate) fn handed_key(id: &TurnIdentity) -> String {
        let chat = crate::TURN_CONVERSATION.try_with(|c| c.clone()).unwrap_or_default();
        // E.EGRESS3e: the scope and the shared flag too -- a member-scoped turn under the primary's
        // name (YM_HARNESS_SCOPE=member) must not read the primary's hand-over.
        format!("{}|{:?}|{}|{chat}", id.owner, id.output_scope, id.shared)
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

    /// E.EGRESS3d (N4): every way the Mind writes takes a named file out of the hand-over -- an editor
    /// edit or save (the tab's file as the editor reports it, and a save_as target), and a terminal
    /// command or Files action that mentions a named file. Both loops call this after `guards::post`.
    pub(crate) fn note_writes(&self, id: &TurnIdentity, tool: &str, args: &serde_json::Value, obs: &str) {
        // E.EGRESS3g (R1d on VM 520): a call the desktop refused ran nothing and wrote nothing -- the
        // model's `editor.open_path` on the spec took it out of the hand-over. Only the desktop's own
        // refusal line, at the very start, counts: a command's output could print the words.
        // E.EGRESS4 (the sixth pass's note 1): and only the DESKTOP's own -- any other MCP server's
        // output passes through and could print the line first, then write -- in the gate's full form.
        if tool == crate::desktop::ACT && obs.trim_start().starts_with("REFUSED \u{2014} nothing was run. refused:") {
            return;
        }
        let key = Self::handed_key(id);
        if crate::desktop::changes_editor_text(tool, args) {
            let reported = crate::desktop::reported_path(obs);
            let saved_as = crate::desktop::save_path(tool, args);
            if let Some(path) = &reported {
                self.note_mind_written(&key, path);
            }
            if let Some(p) = &saved_as {
                self.note_mind_written(&key, p);
            }
            // E.EGRESS3f (S2): an edit that does not say which file it changed is placed by the tab its
            // answer's header names -- an untitled tab holds no named file; a named tab takes back the
            // files of that name; no header, and it may have changed any of them.
            if reported.is_none() && saved_as.is_none() {
                if let Ok(mut h) = self.handed_over.lock() {
                    match crate::desktop::editor_tab_name(obs) {
                        Some(name) if name == crate::desktop::UNTITLED_TAB => {}
                        Some(name) => {
                            for p in h.note_written_if_mentioned(&key, &name) {
                                eprintln!("[egress] {p}: its tab was edited -- no longer the person's text");
                            }
                        }
                        None => {
                            h.note_all_written(&key);
                            eprintln!("[egress] an editor {tool} named no tab -- every named file is taken back");
                        }
                    }
                }
            }
        }
        // E.EGRESS3e (A3): the backstop to the ctime check -- any call but a look whose arguments
        // mention a named file (a terminal's agent_run or agent_input, Files, blender's run_python...).
        // E.EGRESS3f (S3): a look is an explicit (app, action) pair.
        let a_look = tool == crate::desktop::DESCRIBE || is_web_tool(tool) || crate::desktop::only_looks(tool, args);
        if !a_look {
            if let Ok(mut h) = self.handed_over.lock() {
                for p in h.note_written_if_mentioned(&key, &args.to_string()) {
                    eprintln!("[egress] {p}: touched by a {tool} write -- no longer the person's text");
                }
            }
        }
    }

    /// E.EGRESS3c (4c's #654): keep an editor's text of a named file only when the desktop vouches
    /// that the editor's path IS the file -- it exists, is reached through no link, and its real path
    /// is that path. Anything else, an unreachable desktop included, keeps nothing.
    pub(crate) async fn hand_over_checked(&self, key: &str, editor_path: &str, text: &str, id: &TurnIdentity) {
        let args = serde_json::json!({"app": crate::desktop::TWIN_HOST, "action": "files_stat", "args": {"path": editor_path}});
        let answer = self.run_agent_tool_as(crate::desktop::ACT, &args, id).await;
        let home = self.person_home();
        let same = |real: &str| crate::desktop::absolute(real, home.as_deref()) == crate::desktop::absolute(editor_path, home.as_deref());
        let abs = crate::desktop::absolute(editor_path, home.as_deref());
        let named_at = self.handed_over.lock().ok().and_then(|h| h.named_at(key, &abs));
        match crate::desktop::files_stat_real(&answer) {
            Some((true, false, real, changed)) if same(&real) && unchanged_since(changed, named_at) => {
                self.note_handed_over(key, editor_path, text)
            }
            other => eprintln!("[egress] {editor_path}: not handed over -- the desktop did not vouch for it ({other:?})"),
        }
    }

    /// E.EGRESS3e (A3): before a web search plans from handed-over text, ask the desktop again about
    /// each file; one written since it was named (its ctime moved), or no longer vouched for, is
    /// dropped and never comes back. Nothing is asked when nothing is kept.
    pub(crate) async fn recheck_handed(&self, key: &str, id: &TurnIdentity) {
        let kept = self.handed_over.lock().map(|h| h.kept_files(key)).unwrap_or_default();
        for (abs, named_at) in kept {
            let args = serde_json::json!({"app": crate::desktop::TWIN_HOST, "action": "files_stat", "args": {"path": abs}});
            let answer = self.run_agent_tool_as(crate::desktop::ACT, &args, id).await;
            let still = matches!(crate::desktop::files_stat_real(&answer),
                Some((true, false, real, changed)) if real == abs && unchanged_since(changed, Some(named_at)));
            if !still {
                eprintln!("[egress] {abs}: changed or unvouched since it was named -- no longer a query source");
                self.note_mind_written(key, &abs);
            }
        }
    }

    /// E.EGRESS4: where a fetch goes. An address in the person's own words of this turn leaves as
    /// written; anything else is the CLEAN planner's pick among [`fetch_candidates`], made at
    /// temperature 0 from the person's request, the files they handed over, each candidate's result
    /// line and what was fetched already -- the grounded model's choice is discarded, so it carries
    /// no private bits. A third fetch of one page differing only in its query is refused.
    async fn plan_fetch(
        &self,
        tool: &str,
        user_text: &str,
        grounded: &serde_json::Value,
        web_obs: &[String],
        fetched: &[String],
        key: &str,
    ) -> Result<serde_json::Value, CleanArgsFailure> {
        let (handed, named) = if key.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            self.handed_over
                .lock()
                .map(|mut h| (h.texts(key, Self::now_ms()), h.named_names(key)))
                .unwrap_or_default()
        };
        let capped = |url: &str| {
            let page = without_query(url);
            fetched.iter().filter(|f| without_query(f) == page).count() >= FETCHES_PER_PAGE
        };
        if let Some(url) = grounded.get("url").and_then(|u| u.as_str()) {
            if !url.is_empty() && user_text.contains(url) {
                if capped(url) {
                    return Err(CleanArgsFailure::FetchCapReached);
                }
                return Ok(serde_json::json!({ "url": url }));
            }
        }
        let candidates = fetch_candidates(user_text, web_obs, &named);
        if candidates.is_empty() {
            eprintln!("[egress] {tool}: no address from the person's words or a web result \u{2014} refused");
            return Err(CleanArgsFailure::UrlNotFromSources);
        }
        let all_web = web_obs.join("\n");
        let line_of = |u: &str| -> String {
            all_web.lines().chain(user_text.lines()).find(|l| l.contains(u)).unwrap_or("").chars().take(200).collect()
        };
        let list: String = candidates.iter().enumerate().map(|(i, u)| format!("{}. {u}\n   {}\n", i + 1, line_of(u))).collect();
        let handed_all = handed.join("\n\n");
        let handed_part = if handed_all.trim().is_empty() {
            String::new()
        } else {
            let excerpt: String = handed_all.chars().take(PLANNER_HANDED_CAP).collect();
            format!("\nText of the files the person handed over:\n{excerpt}\n")
        };
        let done = if fetched.is_empty() { "(none)".to_string() } else { fetched.join("\n") };
        let sys = "You choose which ONE web page to fetch next for the person's request, from a numbered \
            list of addresses that web searches returned. Prefer a page not fetched yet that best serves \
            the request. You know nothing else about the person. Output ONLY {\"pick\": <number>}.";
        let user = format!(
            "Person's request: {user_text}\n{handed_part}\nAlready fetched this turn:\n{done}\n\nAddresses:\n{list}\nOutput ONLY {{\"pick\": <number>}}."
        );
        let cfg = GenerationConfig { max_tokens: 40, temperature: 0.0, seed: 0, ..GenerationConfig::default() };
        let text = self
            .inference
            .chat_household_attributed(vec![ChatMessage::system(sys), ChatMessage::user(&user)], cfg, concat!(module_path!(), ":fetch-pick"))
            .await
            .map_err(|e| {
                eprintln!("[egress] fetch planner for {tool} did not answer: {e}");
                CleanArgsFailure::NoAnswer
            })?
            .text;
        let body = crate::strip_reasoning(&text);
        let pick = match (body.find('{'), body.rfind('}')) {
            (Some(a), Some(b)) if b > a => serde_json::from_str::<serde_json::Value>(&body[a..=b])
                .ok()
                .and_then(|v| v.get("pick").and_then(|p| p.as_u64())),
            _ => None,
        };
        let Some(url) = pick.and_then(|n| usize::try_from(n).ok()).and_then(|n| n.checked_sub(1)).and_then(|i| candidates.get(i)) else {
            eprintln!("[egress] fetch planner for {tool} picked no listed address \u{2014} refused");
            return Err(CleanArgsFailure::NoUsableArgs);
        };
        if capped(url) {
            return Err(CleanArgsFailure::FetchCapReached);
        }
        Ok(serde_json::json!({ "url": url }))
    }

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
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn egress_clean_args(
        &self,
        tool: &str,
        user_text: &str,
        grounded: serde_json::Value,
        external_provenance: &str,
    ) -> Result<serde_json::Value, CleanArgsFailure> {
        self.egress_clean_args_with(tool, user_text, grounded, external_provenance, &[], &[], "").await
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
        fetched: &[String],
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
        // E.EGRESS4 (the sixth pass): a fetch never leaves as the grounded model chose it from what
        // came back this turn -- a page listing ?v=alice, ?v=bob... let it pick private bits, and a
        // mail result let it follow a reset link. Only the person's own words pass through; anything
        // else is the clean planner's pick among the web results.
        if matches!(tool, "web_fetch" | "fetch" | "web") {
            let _ = external_provenance; // mail and every non-web tool: never a fetch source
            return self.plan_fetch(tool, user_text, &grounded, web_obs, fetched, key).await;
        }
        // E.EGRESS3 (Pranab, 5 Oct): a web search may also draw on the text of files the person named
        // in this conversation, and on what the outside world returned this turn. A query whose every
        // content word is already there leaves as the model wrote it -- deterministic, so the repeat
        // guard works -- and anything else is authored with that text in view.
        // E.EGRESS3b (the review's H1): it leaves as written only as ONE contiguous run of whole words
        // of ONE source; anything assembled from several is the planner's to write.
        let web_query = plans_from_handed(tool);
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
