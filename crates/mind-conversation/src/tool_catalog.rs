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

use serde_json::{json, Value};
use std::collections::HashSet;

/// Core tools + the header for the gated (relevance-ranked) section that follows.
pub(crate) const CORE_HEAD: &str = "CORE TOOLS (always available; use ONE per step):\n\
- recall {query}: search your typed memory\n\
- remember {text}: store a durable fact about the user/world (do this when they tell you something lasting)\n\
- add_reminder {text, when}: mark a date/commitment for the future (a birthday, a deadline) so you ping them when due — 'when' like tomorrow / next week / in 3 days / July 23\n\
- drop_reminder {words}: when they say to drop/cancel/stop tracking something, CLOSE it for real — this clears matching reminders, threads, watches, and planned items everywhere. Never just SAY something is dropped: call this, then report what it closed\n\
- now {}: the current date and time\n\
- myself {}: your LIVE setup — providers, model lanes, keys present, mounted packs. ANY question about your own configuration is answered from THIS, never from memory: your memories about your own code are history, not state\n\
MOST-RELEVANT TOOLS for this message (native — prefer these; do NOT build a skill for a task they cover):";


/// Standing rule appended after the detailed section, never gated.
pub(crate) const NEVER_RULE: &str = "- NEVER claim you removed/changed a date unless one of these tools confirmed it — if no tool fits, say so plainly\n\
- NEVER say a capability is missing, unwired, unavailable or 'not connected this turn' when a tool listed above covers it, and NEVER tell the user to go run a `ym` command themselves — that tool is YOURS and calling it is your job. If a listed tool fits the question, CALL IT; you may only report an inability after a call actually failed, and then say what failed\n\
- an mcp.* integration write always pauses for the user's ok; read-only integrations run instantly";

/// The skill meta-tools — the escape hatch of the gated catalog; never gated.
pub(crate) const SKILL_SECTION: &str = "SKILL LIBRARY (your growing, reusable capabilities — beyond the core):\n\
- discover_tools {query}: SEARCH your tools + skill library for a capability that fits the task — ALWAYS try this before assuming you can't do something (it also finds the name-only tools below)\n\
- run_skill {name, target, url?}: run a skill you found via discover_tools\n\
- build_capability {name, summary, recipe}: create a NEW reusable skill when discover_tools finds nothing — then run_skill it\n\
- answer {text}: give the user your final reply";

/// Tools the loop's system prompt names explicitly — always rendered in full (when enabled) so the
/// prompt's own guidance ("for SHOPPING use `deals`…") never points at an abbreviated entry.
///
/// The SENSES are pinned for a different reason. `quote`, `watch` and `browse` are how the mind
/// perceives the world outside its own memory, and a sense that competes for a top-K slot loses:
/// on a box with MCP servers attached the catalog runs to well over a hundred lines, so a handful
/// of matched words decides whether the mind can see. It lost exactly that way — asked "what is
/// the Nifty trading at", it answered "I don't have a live market-data tool wired up" and offered
/// to BUILD one, while holding `quote` the whole time. Rewriting the line in the asking vocabulary
/// was not enough, because relevance is relative: the competition grows every time a server
/// connects, so any line can be evicted by tools that have nothing to do with the question.
///
/// Perception is not a specialist capability to be retrieved on a keyword. It is the precondition
/// for answering honestly at all — the difference between "I looked" and "I can't". So it does not
/// compete.
const PINNED: &[&str] = &[
    "search", "web_fetch", "research", "deals", "watch_price", "learn_about", "family",
    "about_person", "github_repo_items",
    // the senses
    "quote", "watch", "browse",
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

/// The tool name of one `·`-separated FRAGMENT of a catalog line.
///
/// `tool_name_of_line` requires the leading "- " and so only ever sees the first tool on a line;
/// a fragment after a `·` has no dash. Both readings are needed: the gate scores whole lines, while
/// schema generation and any "is this tool advertised?" question must see every name.
pub(crate) fn tool_name_of_line_in_fragment(fragment: &str) -> Option<&str> {
    let body = fragment.trim().strip_prefix("- ").unwrap_or(fragment.trim());
    let name = body.split([' ', '{', ':']).next().unwrap_or("");
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

// ── NATIVE FUNCTION-CALLING schemas ─────────────────────────────────────────────────────────────
// The agent loop passes OpenAI-format tool schemas to the backend (which yantrik-ml adapts to the
// Anthropic/Ollama shapes). A tool-capable model then returns STRUCTURED tool_calls instead of a
// free-text JSON blob — killing the parse-fragility + publish_page-salvage hacks. The schema set is
// derived from the SAME gated catalog lines the prose surface shows, so the two never drift; the
// free-text catalog + parser stay as the fallback for backends that ignore the `tools` param.

/// One catalog line → OpenAI function schema(s). A line can pack two tools with a `·` separator
/// ("- calendar {}: … · calendar_add {text}: …") — each half becomes its own schema.
fn line_schemas(line: &str) -> Vec<Value> {
    line.split('·').filter_map(|piece| one_schema(piece)).collect()
}

/// Parse a single "name {arg, arg2?}: description" fragment into a function schema. Returns None for
/// header/rule lines (no lowercase tool name).
fn one_schema(fragment: &str) -> Option<Value> {
    let body = fragment.trim().trim_start_matches("- ").trim();
    let name = body.split([' ', '{', ':']).next().unwrap_or("").trim();
    if name.is_empty() || name.chars().all(|c| !c.is_lowercase()) {
        return None;
    }
    // Args inside {...}: each becomes a property; a trailing `?` marks it optional.
    //
    // TYPED, because untyped invites invention. These properties used to carry only a description,
    // on the reasoning that an empty schema accepts any JSON and so would not wrongly force a
    // numeric arg to string. The cost was measured on qwen3.8:27b, 2026-08-15: asked for the
    // weather in Kyoto it emitted native calls of
    //
    //     {"place": 35.0116}    {"place": 127.002783}    {"place": 15}
    //
    // — Kyoto's LATITUDE, then a longitude. With nothing saying `place` is text, the model picked a
    // plausible shape and the tool answered "which place?" three times. The same model returns
    // {"place":"Bergen"} flawlessly against a schema that says `"type":"string"`, so this was never
    // the model being weak; it was the schema declining to say what it wanted.
    //
    // String is the right default because nearly every catalog arg is free text (query, place, url,
    // topic, text, to, symbol). The genuinely numeric names stay untyped so they are not forced.
    const NUMERIC_ARGS: &[&str] =
        &["limit", "count", "n", "top_k", "shares", "quantity", "amount", "price", "days", "hours", "minutes", "sections", "year"];
    let mut props = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();
    if let (Some(a), Some(b)) = (body.find('{'), body.find('}')) {
        if b > a {
            for raw in body[a + 1..b].split(',') {
                let arg = raw.trim();
                if arg.is_empty() {
                    continue;
                }
                let optional = arg.ends_with('?');
                let key = arg.trim_end_matches('?').trim();
                if key.is_empty() {
                    continue;
                }
                let prop = if NUMERIC_ARGS.contains(&key) {
                    json!({ "description": key })
                } else {
                    json!({ "type": "string", "description": key })
                };
                props.insert(key.to_string(), prop);
                if !optional {
                    required.push(key.to_string());
                }
            }
        }
    }
    // Description = whatever follows the `}` (or the name), stripped of the joining punctuation.
    let after = match body.find('}') {
        Some(b) => &body[b + 1..],
        None => body.strip_prefix(name).unwrap_or(body),
    };
    let desc = after.trim_start_matches([':', ' ', '—', '-']).trim();
    Some(function_schema(name, desc, props, required))
}

/// Assemble the OpenAI `{type:function, function:{…}}` envelope.
fn function_schema(name: &str, desc: &str, props: serde_json::Map<String, Value>, required: Vec<String>) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": name,
            "description": desc,
            "parameters": {
                "type": "object",
                "properties": props,
                "required": required,
                "additionalProperties": true,
            }
        }
    })
}

/// Build a schema from an explicit (arg, required) list — for the core + meta tools that live in the
/// prompt scaffolding rather than the plugin/life catalog constants.
fn arg_schema(name: &str, desc: &str, args: &[(&str, bool)]) -> Value {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for (a, req) in args {
        props.insert((*a).to_string(), json!({ "description": a }));
        if *req {
            required.push((*a).to_string());
        }
    }
    function_schema(name, desc, props, required)
}

/// The always-present tools that aren't in the plugin/life catalog: the CORE four + the skill
/// meta-tools. `answer` is deliberately absent — under native calling the model answers by returning
/// TEXT with no tool call (empty `tool_calls`), which is the standard convention.
fn core_meta_schemas() -> Vec<Value> {
    vec![
        arg_schema("recall", "search your typed memory", &[("query", true)]),
        arg_schema(
            "home_control",
            "operate an allowlisted home device: service like light.turn_off / switch.turn_on / media_player.media_pause, entity_id the EXACT id (resolve the friendly name via the `home` tool first). Security devices (locks, covers, alarms, cameras) are never operable.",
            &[("service", true), ("entity_id", true)],
        ),
        arg_schema("remember", "store a durable fact about the user or the world", &[("text", true)]),
        arg_schema(
            "add_reminder",
            "mark a date/commitment (birthday, deadline) so you follow up when it's due",
            &[("text", true), ("when", true)],
        ),
        arg_schema(
            "draft_email",
            "when a reply needs writing, LEAVE IT IN THEIR DRAFTS instead of only showing it in chat — it lands in their mailbox unsent, one click from done. This cannot send; the send stays theirs",
            &[("to", true), ("subject", false), ("body", true)],
        ),
        arg_schema(
            "quote",
            "live market price for one or more symbols — US equities via Alpaca, Indian listings via the .NS/.BO suffix (e.g. RELIANCE.NS) and indices like ^NSEI. Use this for ANY price question rather than answering from memory",
            &[("symbols", true)],
        ),
        arg_schema(
            "browse",
            "drive a real web page toward a goal (navigate, read, fill forms). It stops before anything irreversible — it cannot buy, send or delete",
            &[("url", true), ("goal", true)],
        ),
        arg_schema(
            "watch",
            "WATCH or LISTEN to a video/audio URL (YouTube, podcast, stream): reads published captions, hears it with the local speech model, and looks at sampled frames with the local vision model. Use this for ANY media link — never say you cannot watch video without calling it first",
            &[("url", true), ("question", false)],
        ),
        arg_schema(
            "drop_reminder",
            "when they say to drop/cancel/stop tracking something: CLOSE it for real in every store (reminders, threads, watches, planned items) — never just acknowledge a drop in words",
            &[("words", true)],
        ),
        arg_schema("now", "the current date and time", &[]),
        arg_schema(
            "myself",
            "your LIVE configuration, measured from the running process: model lanes and providers, which keys are present, mounted knowledge packs. Answer ANY question about your own setup from THIS — never from memory",
            &[],
        ),
        arg_schema(
            "discover_tools",
            "search your tools + skill library for a capability that fits the task",
            &[("query", true)],
        ),
        arg_schema("run_skill", "run a skill you found via discover_tools", &[("name", true), ("target", false), ("url", false)]),
        arg_schema(
            "build_capability",
            "create a NEW reusable skill when discover_tools finds nothing",
            &[("name", true), ("summary", true), ("recipe", true)],
        ),
    ]
}

/// The native tool-schema list for this message: core + meta + a schema for every DETAILED
/// (relevance-selected) catalog tool. Deliberately mirrors `gate_catalog`'s detailed set, so the
/// structured surface and the prose surface list the SAME tools; the name-only tail stays
/// prose-and-fallback-only (kept lightweight — a tail tool is reached via discover_tools + the
/// retained free-text path). Deduped by function name (first wins).
pub(crate) fn tool_schemas(user_text: &str, gated_src: &str) -> Vec<Value> {
    let (detailed, _tail) = gate_catalog(user_text, gated_src);
    let mut out = core_meta_schemas();
    for line in detailed.lines() {
        out.extend(line_schemas(line));
    }
    let mut seen: HashSet<String> = HashSet::new();
    out.retain(|s| {
        let name = s["function"]["name"].as_str().unwrap_or("").to_string();
        !name.is_empty() && seen.insert(name)
    });
    out
}

// ── RECENT-CONTEXT COMPACTION ──────────────────────────────────────────────────────────────────
//
// Measured on the live box 2026-08-04 (`ym prompt_audit`): the recent-message block was 16,899 B =
// 53.3% of every agent-loop prompt, dwarfing the tool catalog (5.6%) and the schemas (13.5%). And
// the split was 14 assistant messages at 15,650 B against 6 user messages at 735 B — the mind's OWN
// long outputs (briefings, research reports, photo captions; the largest single one 6,253 B) were
// eating its context, re-read on all five steps of every turn.
//
// The asymmetry is the whole insight: what the USER said is the signal and is tiny; what the MIND
// said is bulk it already knows it produced. So user turns are never touched, the latest assistant
// turn keeps enough to answer a follow-up about it, and older assistant turns keep their opening —
// which carries the point — plus an explicit elision marker so the model knows text was removed
// rather than silently inventing what filled the gap.

/// Bytes kept from the MOST RECENT assistant message (a follow-up usually refers to this one).
pub(crate) const KEEP_LATEST: usize = 1200;
/// Bytes kept from older assistant messages — enough for the gist, not the whole briefing.
pub(crate) const KEEP_OLDER: usize = 360;

/// Compact a recent-conversation window for the prompt. `msgs` is oldest-first `(role, text)`.
pub(crate) fn compact_recent(msgs: &[(String, String)]) -> String {
    let last_assistant = msgs.iter().rposition(|(r, _)| r == "assistant");
    msgs.iter()
        .enumerate()
        .map(|(i, (role, text))| {
            // The user's own words are never abridged: they are the smallest slice and the most
            // load-bearing one.
            if role != "assistant" {
                return format!("{role}: {text}");
            }
            let keep = if Some(i) == last_assistant { KEEP_LATEST } else { KEEP_OLDER };
            if text.chars().count() <= keep {
                return format!("{role}: {text}");
            }
            let head: String = text.chars().take(keep).collect();
            let dropped = text.chars().count() - keep;
            format!("{role}: {head}… [{dropped} chars of my earlier reply elided]")
        })
        .collect::<Vec<_>>()
        .join("
")
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

    /// The real agent-visible catalog: every enabled builtin capability's lines.
    ///
    /// These tests used to run against a hand-written `LIFE_LINES` const. Now that the household
    /// tools are registry specs like everything else, the tests read the same generated source the
    /// loop does — so a spec whose catalog line is malformed fails here instead of silently
    /// degrading tool selection in production.
    fn catalog() -> String {
        crate::plugins::PluginRegistry::builtin().enabled_catalog()
    }

    /// MIGRATION PARITY. The ~44 household tool lines used to live in a hand-written
    /// `const LIFE_LINES: &str` appended to the generated catalog. They are now registry specs, and
    /// this asserts nothing was lost in the move — because a tool silently missing from the catalog
    /// is the failure this codebase already has a scar from: an absent tool makes the model
    /// confabulate the capability in chat rather than say it cannot do the thing.
    ///
    /// The list is the tool names extracted from the deleted const, frozen here. It is deliberately
    /// literal rather than derived: a test that regenerated its own expectation from the registry
    /// would pass no matter what the registry said.
    #[test]
    fn every_migrated_household_tool_is_still_in_the_catalog() {
        const MIGRATED: &[&str] = &[
            "about_person", "ask_whois", "bill_autopay", "calendar", "calendar_add",
            "calendar_remove", "deals", "enhance_photo", "event_ledger", "family", "family_book",
            "family_frame", "festival_calendar", "find_younger_self", "forget_date", "gift_intel",
            "growup_reel", "inbox_analytics", "learn_about", "life_horizon", "mail_report",
            "mail_rule", "mail_search", "nightly_dream", "on_this_day", "onedrive", "patterns",
            "person_items", "photo_cleanup", "photo_create", "photo_patterns", "photo_send",
            "plugin_registry", "see_page", "self_limits", "self_report", "share_with_member",
            "style_timeline", "taste_profile", "then_and_now", "track_subject", "traditions",
            "trip_ledger", "watch_price", "watches",
        ];
        let src = catalog();
        // A line may pack two tools with a `·` separator ("- calendar {}: … · calendar_add {…}: …"),
        // which is why this splits the way the schema generator does rather than using
        // `tool_name_of_line` — that returns only the first name, and the second tool is real and
        // callable. (Getting this wrong is how the first run of this test reported `calendar_add`
        // missing when it was present in both the spec and the catalog.)
        let present: HashSet<&str> = src
            .lines()
            .flat_map(|l| l.split('·'))
            .filter_map(tool_name_of_line_in_fragment)
            .collect();
        let missing: Vec<&&str> = MIGRATED.iter().filter(|t| !present.contains(**t)).collect();
        assert!(missing.is_empty(), "these tools vanished in the registry migration: {missing:?}");

        // Cross-check against the registry's own declarations: a tool advertised in the catalog but
        // not listed on its spec would be ungoverned by the toggle.
        let reg = crate::plugins::PluginRegistry::builtin();
        let declared: HashSet<&str> = reg.all_specs().iter().flat_map(|s| s.tools.iter().map(|t| t.as_str())).collect();
        let undeclared: Vec<&&str> = MIGRATED.iter().filter(|t| !declared.contains(**t)).collect();
        assert!(undeclared.is_empty(), "advertised but not declared on any spec: {undeclared:?}");
    }

    /// Every catalog line must carry exactly one tool and be parseable — the whole surface is
    /// generated now, so a malformed `catalog:` on one spec would quietly drop that tool from both
    /// the prose catalog and the function schemas.
    #[test]
    fn every_generated_catalog_line_is_well_formed() {
        let src = catalog();
        for line in src.lines().filter(|l| !l.trim().is_empty()) {
            assert!(line.starts_with("- "), "catalog lines start with '- ': {line:?}");
            assert!(tool_name_of_line(line).is_some(), "unparseable tool line: {line:?}");
            assert!(line.contains(':'), "a tool line needs a description after ':': {line:?}");
            // A `\n` that survived as text means a spec's catalog string was escaped wrong — the
            // lines would concatenate and every tool but the first would disappear.
            assert!(!line.contains("\\n"), "literal \\n in a catalog line: {line:?}");
        }
    }

    /// Every registered HANDLER must have a spec.
    ///
    /// A handler without one is a capability whose behaviour is compiled in but which the registry
    /// does not know about: its tools get no catalog line, so the agent never learns they exist —
    /// and an absent tool is the case where the model confabulates the capability instead of saying
    /// it cannot. This caught a real regression: refactoring the spec table dropped `monitors` while
    /// leaving `MonitorsCapability` registered, which would have silently removed `set_monitor` from
    /// the agent's world.
    #[test]
    fn every_registered_handler_has_a_spec() {
        let reg = crate::plugins::PluginRegistry::builtin();
        let ids: HashSet<&str> = reg.all_specs().iter().map(|s| s.id.as_str()).collect();
        let orphans: Vec<&str> = reg.handler_ids().into_iter().filter(|h| !ids.contains(h)).collect();
        assert!(orphans.is_empty(), "handlers with no PluginSpec (their tools would be invisible): {orphans:?}");
    }

    /// Every spec must contribute at least one catalog line, or its tools are undiscoverable.
    #[test]
    fn every_enabled_spec_contributes_a_catalog_line() {
        let reg = crate::plugins::PluginRegistry::builtin();
        let src = reg.enabled_catalog();
        let advertised: HashSet<&str> = src
            .lines()
            .flat_map(|l| l.split('·'))
            .filter_map(tool_name_of_line_in_fragment)
            .collect();
        for spec in reg.all_specs().iter().filter(|s| s.enabled) {
            assert!(!spec.catalog.trim().is_empty(), "spec `{}` has no catalog line", spec.id);
            // At least one — not every — declared tool must be advertised. A spec may list ALIASES
            // the dispatch accepts (`web_search` alongside the canonical `search`) while the catalog
            // names only the canonical one, which is right: offering the model two names for one
            // tool wastes prompt and invites it to think they differ. What must never happen is a
            // spec advertising NOTHING, which is how a refactor silently removed `set_monitor`.
            assert!(
                spec.tools.iter().any(|t| advertised.contains(t.as_str())),
                "`{}` declares {:?} but advertises none of them — the agent cannot discover this capability",
                spec.id,
                spec.tools
            );
        }
    }

    /// No two capabilities may claim the same tool name: `plugin_for_tool` returns the FIRST match,
    /// so a duplicate would make one capability's toggle silently govern another's tool.
    #[test]
    fn no_tool_is_claimed_by_two_capabilities() {
        let reg = crate::plugins::PluginRegistry::builtin();
        let mut owner: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
        for spec in reg.all_specs() {
            for tool in &spec.tools {
                if let Some(prev) = owner.insert(tool, &spec.id) {
                    panic!("tool `{tool}` is claimed by both `{prev}` and `{}`", spec.id);
                }
            }
        }
    }

    #[test]
    fn name_extraction_skips_rules_and_headers() {
        assert_eq!(tool_name_of_line("- deals {query, budget?}: find deals"), Some("deals"));
        assert_eq!(tool_name_of_line("- mcp.gmail.search — search mail"), Some("mcp.gmail.search"));
        assert_eq!(tool_name_of_line("- NEVER claim you removed a date"), None);
        assert_eq!(tool_name_of_line("LIFE & SHOPPING TOOLS (native):"), None);
    }

    #[test]
    fn relevant_tool_is_detailed_and_irrelevant_moves_to_tail() {
        let (detailed, tail) = gate_catalog("what's the weather in pune?", &catalog());
        // zero-overlap tools lose their detail line but keep their name in the tail
        assert!(!detailed.contains("growup_reel {name}"), "irrelevant tool should not be detailed");
        assert!(tail.contains("growup_reel"), "gated tool must stay visible by name");
        // pinned tools always keep their full line
        assert!(detailed.contains("deals {query, budget?}"), "pinned tool must stay detailed");
    }

    #[test]
    fn every_tool_appears_exactly_once() {
        let (detailed, tail) = gate_catalog("show me a photo of the wedding", &catalog());
        let src = catalog();
        for line in src.lines() {
            let Some(name) = tool_name_of_line(line) else { continue };
            let in_detail = detailed.lines().any(|l| tool_name_of_line(l) == Some(name));
            let in_tail = tail.contains(name);
            assert!(in_detail || in_tail, "{name} vanished from the catalog");
        }
        // and the photo ask surfaced the photo tool in full
        assert!(detailed.contains("photo_send {query}"));
    }

    #[test]
    fn gating_cuts_the_catalog_substantially() {
        // Measured over the REAL gated surface — every enabled capability's catalog lines.
        let full = catalog();
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
    fn schema_parses_name_args_and_required() {
        let s = one_schema("- deals {query, budget?}: find + compare REAL deals on something").unwrap();
        assert_eq!(s["function"]["name"], "deals");
        assert!(s["function"]["description"].as_str().unwrap().contains("compare REAL deals"));
        let props = &s["function"]["parameters"]["properties"];
        assert!(props.get("query").is_some() && props.get("budget").is_some());
        let req: Vec<&str> = s["function"]["parameters"]["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        assert_eq!(req, vec!["query"], "budget is optional (trailing ?), query is required");
    }

    #[test]
    fn schema_splits_dot_joined_line_into_two_tools() {
        let two = line_schemas("- calendar {}: the unified upcoming view · calendar_add {text}: add an event");
        let names: Vec<&str> = two.iter().map(|s| s["function"]["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["calendar", "calendar_add"], "the `·`-joined line yields both tools");
    }

    #[test]
    fn schema_skips_rule_lines() {
        assert!(one_schema("- NEVER claim you removed/changed a date unless a tool confirmed it").is_none());
        assert!(line_schemas("LIFE & SHOPPING TOOLS (native):").is_empty());
    }

    #[test]
    fn tool_schemas_always_include_core_and_exclude_answer() {
        let schemas = tool_schemas("what's the weather in pune?", &catalog());
        let names: HashSet<&str> = schemas.iter().map(|s| s["function"]["name"].as_str().unwrap()).collect();
        for core in ["recall", "remember", "add_reminder", "drop_reminder", "now", "myself", "discover_tools", "run_skill", "build_capability"] {
            assert!(names.contains(core), "core/meta schema '{core}' must always be present");
        }
        assert!(!names.contains("answer"), "answer is not a native tool — text with no call IS the answer");
        // the message-relevant tool got a schema; an irrelevant one did not (mirrors the prose gate)
        assert!(names.contains("family"), "pinned tool schema present");
        // every schema is well-formed OpenAI shape
        for s in &schemas {
            assert_eq!(s["type"], "function");
            assert!(s["function"]["parameters"]["type"] == "object");
        }
    }

    #[test]
    fn schema_set_stays_compact_and_relevant() {
        let full = catalog();
        for turn in ["what's the weather in pune?", "find me a gift for my wife", "hey, good morning!"] {
            let schemas = tool_schemas(turn, &full);
            let json = serde_json::to_string(&schemas).unwrap();
            println!("{turn:?}: {} schemas, {} chars", schemas.len(), json.len());
            // core+meta (7) plus the pinned set plus a bounded relevant set — never the whole
            // catalog, which is now past 40 tools.
            //
            // The ceiling moved from 30 to 32 for one reason, stated so the next person can judge
            // whether it was earned: the three SENSES (quote, watch, browse) are pinned, because a
            // sense evicted to the name-only tail reads to the model as a capability it does not
            // have, and it then reports that it cannot do the thing. Three permanent slots is the
            // price of that, and it is a bound on the always-present set rather than a licence for
            // the gate to leak.
            assert!(schemas.len() >= 7, "core+meta always present");
            assert!(schemas.len() <= 32, "schema set is gated, not the full catalog ({} tools)", schemas.len());
        }
    }

    #[test]
    fn search_finds_a_gated_tool_by_description() {
        let hits = search_lines("track a price drop", &catalog(), 6);
        assert!(
            hits.iter().any(|l| l.contains("watch_price")),
            "discover_tools must surface watch_price for a price-drop ask: {hits:?}"
        );
    }
}

#[cfg(test)]
mod compaction_tests {
    use super::*;

    fn m(role: &str, n: usize) -> (String, String) {
        (role.to_string(), "x".repeat(n))
    }

    /// The measured shape: the user's words are tiny and load-bearing; the mind's own replies are
    /// bulk. Compaction must therefore be ASYMMETRIC — never touch the user.
    #[test]
    fn user_messages_are_never_abridged() {
        let msgs = vec![m("user", 4000), m("assistant", 100)];
        let out = compact_recent(&msgs);
        assert!(out.contains(&"x".repeat(4000)), "a long USER message must survive intact");
        assert!(!out.contains("elided"), "nothing of the user's is elided");
    }

    #[test]
    fn old_assistant_replies_shrink_but_the_latest_keeps_more() {
        let msgs = vec![m("assistant", 6253), m("user", 50), m("assistant", 6253)];
        let out = compact_recent(&msgs);
        // Both are abridged, but the LATEST keeps materially more — a follow-up usually refers to it.
        assert_eq!(out.matches("elided").count(), 2, "both long replies abridged: {}", out.len());
        assert!(out.len() < 2 * KEEP_LATEST + 500, "total is bounded, not 12.5k: {}", out.len());
        assert!(KEEP_LATEST > KEEP_OLDER * 3, "the latest reply is deliberately far more generous");
    }

    #[test]
    fn elision_is_announced_so_the_model_never_invents_the_gap() {
        let out = compact_recent(&[m("assistant", 5000)]);
        assert!(out.contains("chars of my earlier reply elided"), "{out:.120}");
        assert!(out.contains('…'), "the cut point is visible");
    }

    #[test]
    fn short_conversations_are_untouched() {
        let msgs = vec![m("user", 30), m("assistant", 200)];
        let out = compact_recent(&msgs);
        assert!(!out.contains("elided"), "nothing under the cap is abridged: {out:.80}");
    }

    /// The whole point, in numbers: the live window measured 16,899 B.
    #[test]
    fn the_live_window_shape_shrinks_substantially() {
        // 14 assistant / 6 user, assistant bytes dominated by a few long replies (the real profile).
        let mut msgs: Vec<(String, String)> = Vec::new();
        for _ in 0..6 {
            msgs.push(m("user", 122));
        }
        for n in [6253, 3603, 2072, 849, 701, 462, 350, 300, 250, 200, 180, 150, 140, 140] {
            msgs.push(m("assistant", n));
        }
        let before: usize = msgs.iter().map(|(r, t)| r.len() + t.len() + 2).sum();
        let after = compact_recent(&msgs).len();
        assert!(before > 16_000, "setup mirrors the measured window: {before}");
        assert!(
            after < before / 2,
            "compaction must at least halve the biggest slice of the prompt ({before} -> {after})"
        );
    }
}

