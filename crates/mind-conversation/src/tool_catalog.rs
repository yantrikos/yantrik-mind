//! tool_catalog — HYBRID retrieval-gating of the agent tool catalog (Tier's design: a small
//! DETAILED working set + a NAME-ONLY tail + the discover_tools escape hatch). The full ~150-line
//! catalog on every loop step was the classic anti-pattern: it buries the relevant tool and burns
//! tokens. But a tool that's simply ABSENT from the catalog makes the model confabulate the
//! capability in chat (the deal-tracker scar) — so nothing is ever removed, only abbreviated:
//!
//! - ALWAYS detailed: core tools, the skill meta-tools (discover_tools / run_skill /
//!   build_capability / answer), and a PINNED set the loop's system prompt names by name.
//! - TOP-K relevant to THIS message: detailed (deterministic keyword-overlap scoring, no model).
//! - Everything else: listed by NAME in a tail line — still visible, still callable.
//!
//! Gating is PROMPT-PRESENTATION ONLY. Dispatch (`run_agent_tool_as`) accepts every enabled tool
//! regardless of how it was rendered, so "every tool reachable" is structural, not statistical.

use std::collections::HashSet;

/// Core tools + the header for the gated (relevance-ranked) section that follows.
pub(crate) const CORE_HEAD: &str = "CORE TOOLS (always available; use ONE per step):\n\
- recall {query}: search your typed memory\n\
- remember {text}: store a durable fact about the user/world (do this when they tell you something lasting)\n\
- add_reminder {text, when}: mark a date/commitment for the future (a birthday, a deadline) so you ping them when due — 'when' like tomorrow / next week / in 3 days / July 23\n\
- now {}: the current date and time\n\
MOST-RELEVANT TOOLS for this message (native — prefer these; do NOT build a skill for a task they cover):";

/// The native life/shopping tool lines (one tool per line; no header — gated by relevance).
pub(crate) const LIFE_LINES: &str = "- deals {query, budget?}: find + compare REAL deals on something (great for gifts — I factor in who it's for + budget)\n\
- watch_price {query, target?}: start tracking an item's price and ping on a real drop / when it hits a target\n\
- watches {}: list what I'm currently price-watching\n\
- learn_about {url}: follow a link and learn about a person/thing (recursive: their profiles too)\n\
- track_subject {subject}: keep a living, evolving understanding of an ongoing topic (re-run → what changed)\n\
- patterns {}: surface non-obvious patterns across what I know about the user\n\
- family {}: the people I keep track of + their upcoming key dates\n\
- about_person {name}: what I know about someone in the user's life\n\
- calendar {}: the unified upcoming view · calendar_add {text}: add an event (Dinner on July 4 at 7pm)\n\
- calendar_remove {title}: remove a calendar event by (partial) title — USE THIS when the user says an event/date is wrong or should go\n\
- forget_date {name, label}: remove one dated entry (e.g. open house) from a person's profile — the other place a wrong date can live\n\
- see_page {url, question?}: render a page in the real browser, screenshot it, and ANALYZE the image — use when text extraction fails or layout/visuals matter\n\
- photo_send {query}: find a REAL photo in the user's own libraries (face-matched people + semantic search over the whole archive) and SEND it to the chat — use for ANY 'show/send me a photo/pic of X', including events like 'our wedding'\n\
- photo_patterns {name?}: read someone's photos and learn their style/preferences (no name = recent across libraries)\n\
- ask_whois {}: send the next unknown-face 'who is this?' question to the chat\n\
- growup_reel {name}: build a time-lapse FILM of a person growing up (best face per month across the whole photo archive) and send it — pure magic for family\n\
- on_this_day {}: send a real photo memory from this exact day in a past year (who + where captioned)\n\
- enhance_photo {}: enhance the last photo the user sent (light/color/sharpen) and send it back — for photo-editing asks\n\
- gift_intel {name}: study a person's photos for gift intelligence — what they OWN (never re-gift), their style, what's MISSING that complements it, 3 buyable ideas; chain into `deals` for real listings\n\
- inbox_analytics {}: cross-account email digest over ALL connected inboxes — needs-action / from-people / money-in-motion / purchases / noise, with body-peek state verification (read-only)\n\
- mail_rule {rule}: permanently teach a mail categorization rule when the user corrects the digest ('amazon receipts are noise')\n\
- mail_report {}: DEEP mail analysis over hundreds of emails — recurring charges w/ est monthly total, bills, shopping volume, real humans, account surface, renewal radar; auto-tracks found subscriptions\n\
- self_report {}: my weekly self-review — per-domain scoreboard of my proactive predictions vs your reactions, corrections I absorbed, what I'm changing\n\
- bill_autopay {name}: when the user says a bill is on autopay, mark it so reminders stop\n\
- trip_ledger {query?}: LIFE CHAPTERS mined from the photo archive (where+when+who) — list trips, or brief one ('kolkata', '2019'); trip collages available\n\
- event_ledger {query?}: heavily-photographed DAYS related to family dates and occasions (birthday parties, pujas, ceremonies) — list or look one up; unknown days get asked about\n\
- life_horizon {}: the PROJECTED life — annual patterns from the family's own rhythms (festivals, recurring visits) with next dates and evidence\n\
- festival_calendar {}: the Bengali Hindu festival year — per-year resolved dates (lunar calendar) + what each festival is\n\
- traditions {}: the family's per-festival traditions (photoshoots, feasts) — weather-dependent ones get forecast-planned day suggestions\n\
- family_book {year?}: the family's living biography compiled from the archive — chapters per year, open questions, exportable volume\n\
- then_and_now {person}: side-by-side of the same person years apart (earliest good frame vs latest) with the years labeled\n\
- find_younger_self {person}: hunt the unnamed clusters for a person's earlier years (babies get split by face clustering) — evidence + confirm + merge\n\
- share_with_member {member, note?}: send the LAST photo I delivered to a household member (wife/kids) with a note — their reply gets relayed back\n\
- style_timeline {person}: how a person's style is EVOLVING year over year from their own photos, and where it's heading\n\
- family_frame {}: today's wall-frame photo pick (anniversary-aware daily photo for the home tablet) — returns the caption + URL\n\
- nightly_dream {}: one verified cross-domain connection from everything known about the family (or honest silence)\n\
- self_limits {}: my honest capabilities/limitations/frustrations analysis, grounded in my own telemetry (tool reliability, tensions, ledger traction, failure log)\n\
- plugin_registry {query?}: the plugin store in the substrate — search connectors (live/gated/parked/planned) or browse all\n\
- mail_search {query}: search the FULL mailboxes of every configured account (all folders incl. archive) — bookings, receipts, confirmation numbers, senders. Results ARE the answer — never fetch links or sign-in pages from email bodies\n\
- onedrive {action}: read the family's OLDER photo years from OneDrive (pre-Immich) — status/auth/find <date-range>/onthisday. Read-only\n\
- photo_cleanup {}: organize the photo LIBRARY itself — classify screenshots + WhatsApp forwards across the whole archive into auto-albums (archive step available on request)\n\
- person_items {name}: structured OBJECT INVENTORY from their photos — every watch/bag/dress/jewelry item seen (counts + variants) and what was NEVER seen (gift gaps); use for 'does she have a…' questions\n\
- taste_profile {name}: preference PROBABILITIES from studying many photos — outfit/color/jewelry/setting/vibe distributions with confidence that grows per batch; use for 'what does she like' questions\n\
- photo_create {request}: CREATIVE studio — collages (a person across occasions/outfits, 'us' across years) and mood/vibe pictures, composed from the library with a unique grounded caption; pass the user's ask verbatim";

/// Standing rule appended after the detailed section, never gated.
pub(crate) const NEVER_RULE: &str = "- NEVER claim you removed/changed a date unless one of these tools confirmed it — if no tool fits, say so plainly\n\
- an mcp.* integration write always pauses for the user's ok; read-only integrations run instantly";

/// The skill meta-tools — the escape hatch of the gated catalog; never gated.
pub(crate) const SKILL_SECTION: &str = "SKILL LIBRARY (your growing, reusable capabilities — beyond the core):\n\
- discover_tools {query}: SEARCH your tools + skill library for a capability that fits the task — ALWAYS try this before assuming you can't do something (it also finds the name-only tools below)\n\
- run_skill {name, target, url?}: run a skill you found via discover_tools\n\
- build_capability {name, summary, recipe}: create a NEW reusable skill when discover_tools finds nothing — then run_skill it\n\
- answer {text}: give the user your final reply";

/// Tools the loop's system prompt names explicitly — always rendered in full (when enabled) so the
/// prompt's own guidance ("for SHOPPING use `deals`…") never points at an abbreviated entry.
const PINNED: &[&str] = &[
    "search", "web_fetch", "research", "deals", "watch_price", "learn_about", "family",
    "about_person", "github_repo_items",
];

/// How many relevance-matched (non-pinned) tool lines stay detailed.
const TOP_K: usize = 10;

/// The tool name of a catalog line ("- deals {query}: …" → "deals"), or None for headers/rules.
pub(crate) fn tool_name_of_line(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("- ")?;
    let name = rest.split([' ', '{', ':']).next().unwrap_or("");
    // A rule line ("NEVER claim…") isn't a tool; neither is an empty remainder.
    if name.is_empty() || name.chars().all(|c| !c.is_lowercase()) {
        return None;
    }
    Some(name)
}

/// Lowercased content words (stopwords + short tokens dropped) — the same tokenizer on both the
/// user text and the catalog line keeps scoring symmetric and deterministic.
fn tokenize(text: &str) -> HashSet<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "you", "your", "are", "can", "this", "that", "its", "was",
        "does", "from", "into", "has", "have", "what", "when", "where", "who", "how", "why",
        "she", "him", "her", "his", "they", "them", "our", "one", "get", "use", "any", "all",
        "not", "but", "about", "over", "per", "via",
    ];
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 && !STOP.contains(w))
        .map(|w| w.to_string())
        .collect()
}

/// Deterministic relevance of one catalog line to the user's message: content-word overlap, with
/// naming the tool itself worth more than any description match.
fn score(query: &HashSet<String>, line: &str, name: &str) -> usize {
    let overlap = tokenize(line).intersection(query).count();
    let named = if query.contains(&name.to_lowercase()) { 5 } else { 0 };
    overlap + named
}

/// Split the gated catalog into (detailed section, name-only tail) for this message.
/// Input is any mix of tool lines ("- name {…}: …"); header/rule lines are dropped (the caller
/// re-adds the standing rules). Pinned tools and the TOP_K scored > 0 stay detailed; the rest
/// become a single name-only line so every tool remains visible + callable.
pub(crate) fn gate_catalog(user_text: &str, gated_lines: &str) -> (String, String) {
    let q = tokenize(user_text);
    let mut detailed: Vec<&str> = Vec::new();
    let mut scored: Vec<(usize, &str, &str)> = Vec::new();
    let mut tail: Vec<&str> = Vec::new();
    for line in gated_lines.lines().filter(|l| !l.trim().is_empty()) {
        let Some(name) = tool_name_of_line(line) else { continue };
        if PINNED.contains(&name) {
            detailed.push(line);
        } else {
            let s = score(&q, line, name);
            if s > 0 {
                scored.push((s, name, line));
            } else {
                tail.push(name);
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0)); // stable: catalog order breaks ties
    for (i, (_, name, line)) in scored.iter().enumerate() {
        if i < TOP_K {
            detailed.push(line);
        } else {
            tail.push(name);
        }
    }
    tail.sort_unstable();
    let tail_line = if tail.is_empty() {
        String::new()
    } else {
        format!(
            "OTHER TOOLS (abbreviated to names only, but ALL callable directly by name with JSON args — nothing here is missing; discover_tools {{query}} shows how one works): {}",
            tail.join(", ")
        )
    };
    (detailed.join("\n"), tail_line)
}

/// Top catalog lines matching a discover_tools query — the escape hatch that turns a name-only
/// (or forgotten) tool back into a fully-described one on demand.
pub(crate) fn search_lines(query: &str, catalog: &str, top_n: usize) -> Vec<String> {
    let q = tokenize(query);
    let mut scored: Vec<(usize, &str)> = catalog
        .lines()
        .filter_map(|line| {
            let name = tool_name_of_line(line)?;
            let s = score(&q, line, name);
            (s > 0).then_some((s, line))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(top_n).map(|(_, l)| l.trim().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_extraction_skips_rules_and_headers() {
        assert_eq!(tool_name_of_line("- deals {query, budget?}: find deals"), Some("deals"));
        assert_eq!(tool_name_of_line("- mcp.gmail.search — search mail"), Some("mcp.gmail.search"));
        assert_eq!(tool_name_of_line("- NEVER claim you removed a date"), None);
        assert_eq!(tool_name_of_line("LIFE & SHOPPING TOOLS (native):"), None);
    }

    #[test]
    fn relevant_tool_is_detailed_and_irrelevant_moves_to_tail() {
        let (detailed, tail) = gate_catalog("what's the weather in pune?", LIFE_LINES);
        // zero-overlap tools lose their detail line but keep their name in the tail
        assert!(!detailed.contains("growup_reel {name}"), "irrelevant tool should not be detailed");
        assert!(tail.contains("growup_reel"), "gated tool must stay visible by name");
        // pinned tools always keep their full line
        assert!(detailed.contains("deals {query, budget?}"), "pinned tool must stay detailed");
    }

    #[test]
    fn every_tool_appears_exactly_once() {
        let (detailed, tail) = gate_catalog("show me a photo of the wedding", LIFE_LINES);
        for line in LIFE_LINES.lines() {
            let name = tool_name_of_line(line).unwrap();
            let in_detail = detailed.lines().any(|l| tool_name_of_line(l) == Some(name));
            let in_tail = tail.contains(name);
            assert!(in_detail || in_tail, "{name} vanished from the catalog");
        }
        // and the photo ask surfaced the photo tool in full
        assert!(detailed.contains("photo_send {query}"));
    }

    #[test]
    fn gating_cuts_the_catalog_substantially() {
        // Measure over the REAL gated surface: every enabled plugin line + every life line.
        let plugin_catalog = crate::plugins::PluginRegistry::builtin().enabled_catalog();
        let full = format!("{plugin_catalog}\n{LIFE_LINES}");
        for turn in ["hey, good morning!", "what's the weather in pune?", "find me a gift for my wife"] {
            let (detailed, tail) = gate_catalog(turn, &full);
            let gated_len = detailed.len() + tail.len();
            println!("catalog cut for {turn:?}: {} -> {gated_len} chars", full.len());
            assert!(
                gated_len < full.len() / 2,
                "hybrid catalog should be less than half the full catalog for {turn:?} ({gated_len} vs {})",
                full.len()
            );
        }
    }

    #[test]
    fn search_finds_a_gated_tool_by_description() {
        let hits = search_lines("track a price drop", LIFE_LINES, 6);
        assert!(
            hits.iter().any(|l| l.contains("watch_price")),
            "discover_tools must surface watch_price for a price-drop ask: {hits:?}"
        );
    }
}
