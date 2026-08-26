//! Named delegations — "create an agent, assign it a task", made honest.
//!
//! The runtime already existed (researcher + coder executors, bg-job cap, notify queue); what was
//! missing was ACCOUNTABILITY: a job you kick off should be visible while it runs and findable
//! after it finishes, not just a message that scrolls by in chat. So a delegation is a LEDGER ROW
//! first and a background task second: `ym delegate <name>: <task>` records it, runs it, updates
//! it, and `ym jobs` shows the board. The desktop app's Tasks pane renders this.
//!
//! Deliberately NOT a persistent-agent framework: a "named agent" here is a labeled job, not a
//! second mind with its own memory. One mind, many hands — the moment delegations get their own
//! beliefs is the moment the household has two sources of truth.

use super::*;

const LEDGER_KEY: &str = "delegations";
const LEDGER_CAP: usize = 50;
/// Result stored in the ledger row. Was 1200 — enough for a board glance, but the desktop's
/// channel view renders the WHOLE result as the agent's message, and a truncated answer in a
/// channel reads as a broken agent (it did, live: "…/ / Several sea"). 8000 chars × 50-row cap
/// keeps the blob bounded.
const RESULT_HEAD: usize = 8000;

/// `<name>: <task>` (explicit) or just `<task>` (name derived from its first words). Kind is
/// routed by verb keywords — code-shaped work goes to the sandboxed coder, everything else to the
/// researcher.
pub(crate) fn parse_delegation(rest: &str) -> Option<(String, String, &'static str)> {
    let rest = rest.trim();
    if rest.len() < 3 {
        return None;
    }
    let (name, task) = match rest.split_once(':') {
        // A colon names the job — but "https://..." is not a name:task split.
        Some((n, t)) if !n.contains("http") && n.split_whitespace().count() <= 5 && !t.trim().is_empty() => {
            (n.trim().to_string(), t.trim().to_string())
        }
        _ => {
            // Derived label: the first few PLAIN words — URLs stay in the task, not the label.
            let name: String =
                rest.split_whitespace().filter(|w| !w.contains("://")).take(4).collect::<Vec<_>>().join(" ");
            let name = if name.is_empty() { "job".to_string() } else { name };
            (name, rest.to_string())
        }
    };
    Some((name, task.clone(), classify(&task)))
}

/// Verbs that ask for something to be PRODUCED — including produced by CHANGING what exists.
/// The improvement verbs were missing at first, so "improve the dashboard" routed to research:
/// a delegation asking for work was answered with reading.
const MAKE_VERBS: &[&str] = &[
    "build", "create", "make", "write", "design", "generate", "draft", "produce", "implement",
    "code", "develop", "set up", "put together", "publish", "prototype", "mock up", "scaffold",
    "refactor", "patch", "fix", "add", "port", "convert", "rewrite", "improve", "redesign",
    "restyle", "rework", "revamp", "polish", "modernize", "enhance", "extend", "upgrade",
];

/// Signals that the task points at an EXISTING body of source, which makes it code work no matter
/// what the artifact looks like. "Redesign the dashboard in /srv/app" is editing files, not
/// publishing a page — the page/code boundary is where routing kept going wrong, because page
/// nouns appear constantly in descriptions of code to change. A brief once had to be worded to
/// AVOID the words "site", "page" and "dashboard" to reach the coder; the router should never
/// make the caller do that.
fn mentions_codebase(tl: &str) -> bool {
    // Location phrasings only. A bare "repo" is NOT a marker: "a dashboard showing my repos" is a
    // page ABOUT repositories, not an edit to one — the noun has to be where the work happens
    // ("in the repo"), not what the artifact displays.
    const MARKERS: &[&str] = &[
        "codebase", "source tree", "source files", "the source", "existing code", "our code",
        "crates/", "in the repo", "in our repo", "in this repo", "in my repo",
    ];
    if MARKERS.iter().any(|m| tl.contains(m)) {
        return true;
    }
    // A concrete source-file extension is as decisive as naming the repo. Word-boundary on the
    // right so ".jsx" or ".rsync" can't false-positive off a shorter suffix.
    const EXTS: &[&str] = &[".rs", ".js", ".ts", ".py", ".css", ".html", ".go", ".c", ".cpp", ".java", ".sh", ".toml", ".yaml", ".yml"];
    EXTS.iter().any(|e| {
        tl.match_indices(e).any(|(at, _)| {
            tl[at + e.len()..]
                .chars()
                .next()
                .map(|c| !c.is_alphanumeric())
                .unwrap_or(true)
        })
    })
}

/// Things that are produced as a FILE or a PAGE — the artifact nouns.
const ARTIFACT_NOUNS: &[&str] = &[
    "website", "web site", "site", "page", "landing", "portfolio", "resume", "cv", "app",
    "application", "dashboard", "script", "program", "tool", "cli", "api", "server", "bot",
    "component", "form", "game", "chart", "diagram", "slide", "deck", "template", "prototype",
    "mockup", "readme", "doc site", "blog",
];

/// Verbs that ask for INFORMATION, which beat an artifact noun when they lead the sentence —
/// "research portfolio websites" wants reading, not a website.
const FIND_VERBS: &[&str] =
    &["research", "find", "look up", "search", "compare", "review", "summarize", "check", "list", "investigate", "explain", "analyze"];

/// The DETERMINISTIC FLOOR of routing — not the routing itself.
///
/// A keyword table is what caused the bug this replaces: the old one held seven substrings (`build`,
/// `write a script`, `code`, `implement`, `fix the`, `refactor`, `patch`) and "create a stunning
/// portfolio website for me" matched none of them, so it went to the research agent, which has read
/// tools only, and came back with six links.
///
/// A LONGER table is the same defect with a further-away boundary — it would miss the next phrasing
/// instead of this one. So this function is no longer the decision: [`route`] asks the model, which
/// has no vocabulary limit, and falls back here only when there is no model or its answer is
/// unusable. What a table CAN do well is be instant, free, and predictable, which is exactly what a
/// fallback should be.
pub fn classify(task: &str) -> &'static str {
    let tl = task.to_lowercase();
    // Strip the polite wrapper so the leading verb is the real one.
    let head = tl
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim_start_matches("please ")
        .trim_start_matches("can you ")
        .trim_start_matches("could you ")
        .trim_start_matches("i want you to ")
        .trim_start_matches("i need you to ")
        .trim_start_matches("i'd like you to ")
        .trim_start_matches("help me ")
        .trim_start_matches("please ");

    let leads_with = |set: &[&str]| set.iter().any(|v| head.starts_with(v));
    let mentions = |set: &[&str]| set.iter().any(|v| tl.contains(v));

    // A find-verb in the lead position is decisive: "compare the best portfolio sites" is reading.
    if leads_with(FIND_VERBS) && !leads_with(MAKE_VERBS) {
        return "research";
    }
    if leads_with(MAKE_VERBS) || (mentions(MAKE_VERBS) && mentions(ARTIFACT_NOUNS)) {
        // A PAGE is its own kind. The coder builds it in a sandbox workdir, which is right for a
        // script or a CLI but wrong for a web page: what the person wants is a URL they can open, and
        // the recipe chain (research → author → publish) delivers exactly that in one pass.
        // UNLESS the task points at existing source: then the page noun is describing the thing
        // being edited, not the deliverable, and the coder is the right executor.
        return if mentions(PAGE_NOUNS) && !mentions_codebase(&tl) { "page" } else { "code" };
    }
    "research"
}

/// The kinds a delegation can be routed to, with the one-line description the router shows the model.
/// Adding an executor means adding a row here — the router's vocabulary comes from this list, not
/// from anything written into a prompt by hand.
pub const KINDS: &[(&str, &str)] = &[
    ("page", "produce a NEW standalone web page and publish it at a URL the user can open (portfolio, landing page, resume, one-pager) — the deliverable is the link"),
    ("code", "write or change software — a script, CLI, program, library, or improving EXISTING source files; if the task points at existing code, a repo, or files to modify, it is code even when the thing being changed looks like a page or dashboard"),
    ("research", "find things out and report: search, read sources, compare, summarize, answer a question"),
];

/// Parse the router's reply into a kind that is ACTUALLY RUNNABLE on this box.
///
/// Separate from the model call so it can be tested without inference — this is where a hallucinated
/// kind, a wrapped answer, or a kind whose executor is not configured gets rejected.
pub fn parse_route(reply: &str, available: &[&str]) -> Option<&'static str> {
    let low = reply.to_lowercase();
    // Longest name first, so "research" is not shadowed by a substring of another kind.
    let mut names: Vec<&(&str, &str)> = KINDS.iter().collect();
    names.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    // First mention wins: a model that says "page — because …" means page.
    let mut best: Option<(usize, &'static str)> = None;
    for (k, _) in names {
        if let Some(at) = low.find(k) {
            let kind: &'static str = KINDS.iter().find(|(n, _)| n == k).map(|(n, _)| *n)?;
            if available.contains(&kind) && best.map(|(b, _)| at < b).unwrap_or(true) {
                best = Some((at, kind));
            }
        }
    }
    best.map(|(_, k)| k)
}

/// Artifact nouns that are a hosted PAGE rather than a codebase.
const PAGE_NOUNS: &[&str] = &[
    "website", "web site", "site", "page", "landing", "portfolio", "resume", "cv", "dashboard",
    "blog", "deck", "slides", "one-pager", "onepager",
];

/// The chain that turns "make me a portfolio site" into a URL.
///
/// This is the recipe engine doing what it was built for, and delegation was not using it — it picked
/// ONE executor up front with no handoff, so a research job that discovered it needed to build
/// something just reported that it couldn't. Four steps, each feeding the next:
///
///   research (references) → author a complete document → publish it → hand back the link
///
/// The research step is `Skip`-on-error on purpose: no network is a reason to design from first
/// principles, not a reason to produce nothing.
pub fn page_recipe(name: &str, task: &str, pack_rules: Option<&str>) -> Recipe {
    // Mounted-pack rules have to be threaded in HERE. The page chain runs on the RecipeEngine, which
    // builds its own messages and never sees the ConversationEngine's prompt — so injecting the pack
    // block into build_prompt and the agent loop covered two of the three paths, and the one that
    // actually writes pages was the one left out. Verified live: a page built with web-craft mounted
    // contained none of its markers.
    let rules = pack_rules
        .map(|r| format!("HOUSE RULES from a mounted knowledge pack — follow them:
{r}

"))
        .unwrap_or_default();
    Recipe {
        id: "delegate-page".into(),
        name: format!("page: {name}"),
        steps: vec![
            RecipeStep::Tool {
                tool_name: "research".into(),
                args: serde_json::json!({ "query": format!("{task} — layout, structure and visual design references") }),
                store_as: "refs".into(),
                on_error: ErrorAction::Skip,
            },
            RecipeStep::Think {
                prompt: format!(
                    "{rules}Build this page: {task}\n\n\
                     REFERENCES (inspiration only — never copy their text or claim their content as ours):\n{{{{refs}}}}\n\n\
                     Output ONE complete, self-contained HTML document and NOTHING else — no commentary \
                     before or after, no markdown fence. Start with <!doctype html> and END with \
                     </html>; a document that stops early is worthless, so if you are running long, \
                     write fewer sections rather than leaving the last one unfinished.\n\n\
                     SUBSTANCE — the page must be FINISHED, not a hero with nothing under it:\n\
                     - A hero, then at least three more real sections with real content in them \
                     (selected work as cards, an about paragraph with actual sentences, and contact).\n\
                     - 4-6 project cards, each with a title, a one-line description, and 2-3 tags. \
                     Invent plausible project names and descriptions that fit the brief; never lorem \
                     ipsum, and never invent facts about a real person. Use [Your Name] for the person.\n\
                     - A footer. Nothing may be an empty placeholder box.\n\n\
                     CRAFT:\n\
                     - Everything inline: one <style> block, one <script> only if it does something. \
                     It must render with NO network access — no CDN, no webfont links, no remote \
                     images. Use CSS gradients, shapes or inline SVG instead of photographs.\n\
                     - A deliberate palette of 4-6 colours and one accent, a real type scale, and \
                     generous whitespace. System font stack, styled well.\n\
                     - Responsive from 360px to desktop with CSS grid/flex.\n\
                     - Light and dark via prefers-color-scheme, with an explicit background on body in \
                     both — never a transparent body.\n\
                     - Semantic HTML, a real <title>, visible keyboard focus states, and hover states \
                     on anything interactive.\n\
                     - Avoid the generic AI look: no purple-to-blue gradient hero on white, no emoji \
                     as section markers, no everything-centered. Commit to one clear visual idea."
                ),
                store_as: "page".into(),
                on_error: ErrorAction::Fail,
                // A DOCUMENT budget, not a reply budget. The default 2048 produced a page that stopped
                // mid-tag, and the chain published the fragment and announced it was live.
                max_tokens: Some(16_000usize),
                // Thinking OFF. On a thinking model the budget is shared between the reasoning
                // preamble and the answer, and this step needs OUTPUT tokens: measured, the same
                // prompt gave a complete 9-10k-character page with thinking off and ~900 characters
                // of non-document with it left to the backend default. The bigger the rule block, the
                // longer it reasons and the less document survives.
                think: Some(false),
            },
            RecipeStep::Tool {
                tool_name: "publish_page".into(),
                args: serde_json::json!({ "name": name, "html": "{{page}}" }),
                store_as: "url".into(),
                on_error: ErrorAction::Fail,
            },
            RecipeStep::Notify { message: format!("🌐 [{name}] it's live — {{{{url}}}}\n\nOpen it, and tell me what to change.") },
        ],
    }
}

/// Read-modify-write one ledger row. Free function on the memory handle so the detached task can
/// update the board without holding the engine.
/// Record where a code job's artifacts live, so a reader can OPEN them. "Done, at
/// /opt/.../run-178.../index.html" is a dead end for anyone not sitting on the box; the desktop
/// reads these fields to preview the actual file.
pub(crate) async fn ledger_artifacts(
    memory: &Arc<dyn MemoryFacade>,
    id: &str,
    workdir: &str,
    files: &[String],
) {
    let mut rows: Vec<serde_json::Value> = memory
        .profile_get(LEDGER_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if let Some(r) = rows.iter_mut().find(|r| r.get("id").and_then(|x| x.as_str()) == Some(id)) {
        r["workdir"] = serde_json::json!(workdir);
        r["files"] = serde_json::json!(files);
    }
    let _ = memory.profile_set(LEDGER_KEY, &serde_json::to_string(&rows).unwrap_or_default()).await;
}

pub(crate) async fn ledger_update(
    memory: &Arc<dyn MemoryFacade>,
    id: &str,
    status: &str,
    result_head: Option<String>,
) {
    let mut rows: Vec<serde_json::Value> = memory
        .profile_get(LEDGER_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if let Some(r) = rows.iter_mut().find(|r| r.get("id").and_then(|x| x.as_str()) == Some(id)) {
        r["status"] = serde_json::json!(status);
        r["finished_ms"] = serde_json::json!(chrono::Utc::now().timestamp_millis());
        if let Some(head) = result_head {
            r["result"] = serde_json::json!(head.chars().take(RESULT_HEAD).collect::<String>());
        }
    }
    let _ = memory.profile_set(LEDGER_KEY, &serde_json::to_string(&rows).unwrap_or_default()).await;
}

/// Does a critique verdict mean "good enough, stop iterating"? Tolerant of critics that dress the
/// word up ("SHIP.", "ship — looks solid"), strict about critics that merely MENTION shipping
/// mid-critique ("fix X before you ship").
pub(crate) fn verdict_ships(v: &str) -> bool {
    v.trim().to_uppercase().starts_with("SHIP")
}

/// A peer-strength judge for the iterate loop, when one is named.
///
/// THE CRITIC IS THE QUALITY CEILING, and by default it ran on the mind's own household route —
/// which starts at the local brain pool, so a small local model was grading a frontier builder's
/// work and rubber-stamped it: three consecutive cockpit runs shipped on ROUND 1, and raising the
/// round cap 3 → 50 changed nothing, because the cap was never the binding constraint. A judge
/// weaker than the builder cannot drive iteration.
///
/// `YM_CRITIC_MODEL` names the judge as a `provider:model` spec — e.g.
/// `nanogpt:deepseek/deepseek-v4-pro`. Unset keeps the previous behaviour exactly. Household lane:
/// a delegated build is household work, so a cloud judge is in bounds here (a Private turn never
/// reaches this code path).
pub(crate) fn critic_from_env() -> Option<(InferencePool, String)> {
    let spec = std::env::var("YM_CRITIC_MODEL").ok()?.trim().to_string();
    if spec.is_empty() {
        return None;
    }
    let backend = mind_inference::backend_from_spec(&spec)?;
    Some((InferencePool::new(backend, 2).with_provider(&spec), spec))
}

/// Fingerprint of a workdir's visible files: name → (size, mtime-seconds). Two equal snapshots
/// mean a round changed NOTHING — which, for a round that also reported failure, is the signature
/// of a provider-level outage (an exhausted quota once burned four rounds in under a minute, the
/// critic re-reviewing the same artifact each time). Content hashing would be sturdier but costs
/// a full tree read per round; size+mtime is enough to tell "did work happen" from "did nothing".
pub(crate) fn workdir_snapshot(wd: &str) -> std::collections::BTreeMap<String, (u64, i64)> {
    let mut snap = std::collections::BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(wd) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if let Ok(md) = e.metadata() {
                let mtime = md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                snap.insert(name, (md.len(), mtime));
            }
        }
    }
    snap
}

/// Is this text actually a REVIEW, or did the judge answer in the wrong voice?
///
/// An empty verdict was the first failure; this is the second, and it is worse because it looks
/// like content. In job bf77af the critic (thinking off) replied *"I'll start by reading the
/// current state of the files and understanding what we're working with, then write IDEAS.md
/// before making any changes."* — the BUILDER's opening line, not a review. It is not SHIP, so it
/// was fed forward as findings, and round 4 dutifully built against it.
///
/// Usable = SHIP, or something shaped like findings. Deliberately shape-based rather than
/// semantic: the loop cannot judge the judge, but it can insist a review look like one.
pub(crate) fn verdict_is_usable(v: &str) -> bool {
    let t = v.trim();
    if t.is_empty() {
        return false;
    }
    if verdict_ships(t) {
        return true;
    }
    // First-person agent voice at the START = role confusion. Only the opening is checked, so a
    // legitimate finding that happens to say "I would tighten this" still passes.
    let low = t.to_lowercase();
    const AGENT_VOICE: &[&str] = &[
        "i'll start", "i will start", "i'll begin", "i will begin", "let me start", "let me begin",
        "let me first", "let me read", "i'll read", "i will read", "i need to read", "i'll look",
        "first, let me", "first i'll", "i'll examine",
    ];
    if AGENT_VOICE.iter().any(|p| low.starts_with(p)) {
        return false;
    }
    // At least one enumerated or bulleted line — the format the prompt asks for.
    t.lines().any(|line| {
        let l = line.trim_start();
        l.starts_with("- ")
            || l.starts_with("* ")
            || l.starts_with("• ")
            || l.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
    })
}

/// Append one round's spend to the SHARED token ledger — the same file `ym-record-spend` writes
/// for the nightly self-build and the same one the `tokens` verb already reads. Only the lane label
/// differs ("delegate" vs "builder"), so delegated spend appears in the existing report with no
/// reader change.
///
/// This exists because delegation was invisible. The nightly tick logged every run; the iterate
/// loop logged nothing, so the ledger showed $1.71 across six builds while a single day of
/// delegation quietly moved 42.7M tokens and exhausted a week's quota. The expensive path was the
/// unmeasured one.
///
/// UNMEASURED IS NOT FREE. A round whose CLI returned no usage block is recorded as UNMEASURED
/// rather than skipped or zeroed: a silent gap reads as "nothing was spent", which is the exact
/// misreading that let this go unnoticed.
pub(crate) fn record_round_spend(spend: Option<&mind_tools::coder::RoundSpend>, job: &str, round: usize) {
    let dir = std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".to_string());
    let when = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let lane = format!("delegate:{job}#{round}");
    let line = match spend {
        Some(s) => s.ledger_line(&lane, &when),
        None => format!("{when} | {lane} | unknown | tokens=UNMEASURED | usd=UNMEASURED"),
    };
    // Best effort by design: a delegation must not fail because its meter could not write. The
    // loss is visible either way — a missing line is a gap in the ledger, not a silent zero.
    if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(format!("{dir}/token_ledger.log")) {
        use std::io::Write;
        let _ = writeln!(fh, "{line}");
    }
}

/// What the NEXT round is told about the rounds before it — the mind's memory of the job, handed
/// to a builder that has none.
///
/// This exists because the alternative was `--continue`, which resumed the builder's whole session
/// so round N re-sent rounds 1..N-1's transcript on EVERY ONE of its tool calls. That makes cost
/// roughly quadratic in total turns: one 5-round job reached 405 API turns against a 14MB
/// transcript and helped exhaust a week's token quota in a day. It was also the wrong layer — the
/// delegation design is "one mind, many hands", and `--continue` gave the hand beliefs. The hand
/// should be stateless; the mind remembers and hands down only what the next round needs.
///
/// Newest LAST (the builder reads forward into its current instructions), and the oldest entries
/// are dropped first when the budget binds — a recent failed approach is what stops a re-try, an
/// ancient one rarely is. Deliberately carries NEGATIVE knowledge ("tried X, critic still says Y"):
/// knowing what already failed is what a cold builder cannot re-derive from the tree.
pub(crate) fn history_block(trail: &[String], budget: usize) -> String {
    if trail.is_empty() {
        return String::new();
    }
    // Walk newest-first to decide what fits, then emit oldest-first.
    let mut keep: Vec<&String> = Vec::new();
    let mut used = 0usize;
    for entry in trail.iter().rev() {
        if used + entry.len() + 1 > budget && !keep.is_empty() {
            break;
        }
        used += entry.len() + 1;
        keep.push(entry);
    }
    keep.reverse();
    let dropped = trail.len() - keep.len();
    let mut out = String::from("\nWHAT EARLIER ROUNDS ALREADY TRIED (do not repeat a failed approach; do not undo work that passed):\n");
    if dropped > 0 {
        out.push_str(&format!("({dropped} earlier round(s) omitted)\n"));
    }
    for entry in keep {
        out.push_str(entry);
        out.push('\n');
    }
    out
}

/// What the critic gets to SEE. A critic that receives only file names and the builder's own
/// summary is grading a self-report — the first real run shipped on exactly that. So: excerpts of
/// the actual text files, newest thinking included, bounded so a big tree can't blow the context.
/// Binary and oversized files are named but not quoted; that they exist is still signal.
pub(crate) fn artifact_excerpt(workdir: &str, files: &[String], budget: usize) -> String {
    const PER_FILE: usize = 3_000;
    let mut out = String::new();
    for name in files {
        if out.len() >= budget {
            out.push_str("(further files omitted — excerpt budget reached)\n");
            break;
        }
        let path = format!("{}/{}", workdir.trim_end_matches('/'), name);
        match std::fs::read(&path) {
            Ok(bytes) if bytes.iter().take(512).any(|b| *b == 0) => {
                out.push_str(&format!("=== {name} ({} bytes, binary — not shown)\n", bytes.len()));
            }
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let take = PER_FILE.min(budget.saturating_sub(out.len()));
                let mut cut = take.min(text.len());
                // Cut on a char boundary; a split UTF-8 sequence would corrupt the prompt.
                while cut > 0 && !text.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.push_str(&format!(
                    "=== {name} ({} bytes{})\n{}\n",
                    bytes.len(),
                    if cut < text.len() { ", excerpt" } else { "" },
                    &text[..cut]
                ));
            }
            Err(_) => out.push_str(&format!("=== {name} (unreadable)\n")),
        }
    }
    out
}

/// One delegation, TYPED.
///
/// The ledger is stored as loose JSON (it predates any consumer that needed shape), and every
/// reader so far has re-derived the fields with `get("status").and_then(as_str)` chains. That is
/// fine for a text renderer and wrong for anything that has to make a decision — a typo in a key
/// silently becomes `None`, which reads as "not running" rather than as a bug. The typed view is
/// lenient on the way in (any missing field takes its default) and exact from then on.
#[derive(Debug, Clone, serde::Deserialize, Default)]
pub(crate) struct JobRow {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub kind: String,
    /// "running" | "done" | "failed". Defaulted rather than optional so callers can match on it.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub started_ms: i64,
    #[serde(default)]
    pub finished_ms: Option<i64>,
    #[serde(default)]
    pub result: Option<String>,
}

impl super::ConversationEngine {
    /// The delegation ledger as typed rows, newest activity last. Shares the ledger with
    /// `jobs_report_cmd` so the board, the desktop channel view, and the pulse can never disagree
    /// about what is running.
    pub(crate) async fn job_rows(&self) -> Vec<JobRow> {
        let raw: Vec<serde_json::Value> = self
            .memory
            .profile_get(LEDGER_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // A single malformed row must not blank the whole board — skip it and keep the rest.
        raw.into_iter().filter_map(|v| serde_json::from_value::<JobRow>(v).ok()).collect()
    }

    /// Close out jobs that were running when the service last stopped. Call ONCE at startup.
    ///
    /// A delegation is an in-process task, so a restart kills every one of them — but the ledger
    /// row keeps saying "running", and the panel keeps drawing a live spinner with a growing
    /// elapsed. Observed at 1070m against a job a redeploy had killed the day before: seventeen
    /// hours of the cockpit insisting work was in flight when nothing was executing.
    ///
    /// Startup is the honest place for this. Doing it inside `job_rows` looked tempting — one
    /// accessor, every reader fixed — but that makes a read silently write, and it cannot tell a
    /// genuinely-running job from a stale one without a process-start timestamp that is only
    /// accurate if something forces it early. At startup the answer needs no timestamp at all:
    /// nothing is running yet, so every row claiming otherwise is stale by definition.
    pub async fn reconcile_orphaned_jobs(&self) -> usize {
        let orphans: Vec<String> = self
            .job_rows()
            .await
            .into_iter()
            .filter(|r| r.status == "running")
            .map(|r| r.id)
            .collect();
        for id in &orphans {
            ledger_update(
                &self.memory,
                id,
                "failed",
                Some("(interrupted — the service restarted while this was running, so it never finished)".to_string()),
            )
            .await;
        }
        orphans.len()
    }
}

/// Scratch memory for one job — Pranab's design (2026-08-05): a long job gets its own quarantined
/// workspace; at completion what's worth keeping is PROMOTED into real memory and the scratch is
/// destroyed. Nothing a job writes touches the mind's memory without passing the promotion gate,
/// so a delegation can think freely without becoming a second source of truth.
///
/// Substrate note: stored as a per-job profile blob today (the engine controls purge completely);
/// the surface (note → promote → purge) is shaped so it can move onto real yantrikdb namespaces
/// once the core grows delete-by-namespace — the caller-visible lifecycle won't change.
fn scratch_key(id: &str) -> String {
    format!("job_scratch:{id}")
}
const SCRATCH_NOTE_CAP: usize = 200;
/// Un-promoted scratch older than this is junk by definition — purged by the board on render.
const SCRATCH_STALE_MS: i64 = 7 * 24 * 3600 * 1000;

pub(crate) async fn scratch_note(memory: &Arc<dyn MemoryFacade>, id: &str, text: &str) {
    let key = scratch_key(id);
    let mut notes: Vec<serde_json::Value> = memory
        .profile_get(&key)
        .await
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if notes.len() >= SCRATCH_NOTE_CAP {
        return; // a runaway job must not grow an unbounded blob
    }
    notes.push(serde_json::json!({ "t": chrono::Utc::now().timestamp_millis(), "note": text }));
    let _ = memory.profile_set(&key, &serde_json::to_string(&notes).unwrap_or_default()).await;
}

pub(crate) fn render_board(rows: &[serde_json::Value], now_ms: i64) -> String {
    if rows.is_empty() {
        return "🧰 No delegations yet. `ym delegate <name>: <task>` starts one — research by default, \
                the sandboxed coder when the task reads like code."
            .to_string();
    }
    let mut out = String::from("🧰 DELEGATIONS — the job board\n");
    let mut sorted: Vec<&serde_json::Value> = rows.iter().collect();
    // Running first, then newest finished.
    sorted.sort_by_key(|r| {
        let running = r.get("status").and_then(|x| x.as_str()) == Some("running");
        let t = r.get("started_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        (if running { 0 } else { 1 }, -t)
    });
    for r in sorted.iter().take(15) {
        let g = |k: &str| r.get(k).and_then(|x| x.as_str()).unwrap_or("?");
        let status = g("status");
        let started = r.get("started_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        let mins = ((now_ms - started).max(0)) / 60_000;
        let badge = match status {
            "running" => format!("⏳ running {mins}m"),
            "done" => "✅ done".to_string(),
            _ => "❌ failed".to_string(),
        };
        out.push_str(&format!("\n[{}] {} · {} · {}\n    task: {}\n", g("id"), g("name"), g("kind"), badge, g("task")));
        if status != "running" {
            let head = g("result");
            if head != "?" && !head.is_empty() {
                let first: String = head.lines().take(3).collect::<Vec<_>>().join(" / ");
                out.push_str(&format!("    {}\n", first.chars().take(180).collect::<String>()));
            }
        }
    }
    out.push_str(
        "\nA finished job's scratch waits 7 days: `ym jobs keep <id>` promotes it into memory \
         (as a sub-agent observation), `ym jobs drop <id>` destroys it unkept.",
    );
    out
}

impl super::ConversationEngine {
    /// Which executors this box can actually run right now.
    fn available_kinds(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.recipes.is_some() {
            v.push("page");
        }
        if self.coder.is_some() {
            v.push("code");
        }
        if self.researcher.is_some() {
            v.push("research");
        }
        v
    }

    /// Pick the executor for a task — model first, keyword table only as a floor.
    ///
    /// The model gets the SAME list of kinds the runtime dispatches on, filtered to what is
    /// configured, so it cannot route to something that does not exist here. One short call; if it
    /// fails, times out, or answers with something unusable, [`classify`] decides and the delegation
    /// still runs. Routing must never be the reason nothing happens.
    async fn route(&self, task: &str) -> &'static str {
        let available = self.available_kinds();
        let floor = classify(task);
        if available.len() < 2 {
            return available.first().copied().unwrap_or(floor);
        }
        let menu: String = KINDS
            .iter()
            .filter(|(k, _)| available.contains(k))
            .map(|(k, d)| format!("- {k}: {d}\n"))
            .collect();
        let prompt = format!(
            "A user asked for this to be done:\n\n{task}\n\nWhich ONE of these does it need?\n\n{menu}\n\
             Answer with the single word only. If they want something MADE, pick the kind that makes \
             it — never `research` merely because making it well would involve looking things up. \
             The page/code tie-break: what matters is whether the task starts from EXISTING files or \
             a codebase (code) or wants a new standalone page whose deliverable is a link (page) — \
             not whether words like site, page or dashboard appear in it."
        );
        let cfg = GenerationConfig { max_tokens: 12, think: mind_inference::think_for("dispatch", Some(false)), ..GenerationConfig::default() };
        // GROUNDED, not a bare chat(): the prompt carries the user's own words verbatim, which is
        // household content, so it takes the private lane first and any escalation is audited. The
        // privacy audit caught this as an unscoped call — correctly; a one-word routing answer is not
        // a reason to send someone's request to a cloud provider unrecorded.
        let reply = match self.inference.chat_grounded(vec![ChatMessage::user(&prompt)], cfg).await {
            Ok(r) => r.text,
            Err(_) => return floor,
        };
        match parse_route(&reply, &available) {
            Some(k) => k,
            None => floor,
        }
    }

    /// `ym delegate <name>: <task>` — ledger row + background execution + chat delivery.
    /// Run a banked INSTRUCTION DOCUMENT — a skill whose `code` is prose, not a capability spec.
    ///
    /// Lives here rather than in the dispatch because everything it needs is the delegation
    /// machinery: the job-board slot, the ledger row the cockpit renders, the scratch notes that
    /// make a long run watchable, and the notify queue. A document run IS a delegation; the only
    /// difference is that the instructions were banked earlier instead of typed now (E.SK1).
    ///
    /// Returns a RECEIPT immediately. A research document takes minutes and a tool call must not
    /// hold the turn open while it thinks.
    /// Run a banked CAPABILITY SPEC -- JSON naming a tool to poll until it matches a target.
    ///
    /// Lifted out of the `run_skill` tool arm unchanged so the phrase path can reach it too: a
    /// user who types `run skill web-monitor: bitcoin` should get a monitor, not a lecture about
    /// how to ask for one (E.SK2).
    pub(crate) async fn run_capability_skill(
        &self,
        sk: &mind_types::Skill,
        tool_name: &str,
        spec: &serde_json::Value,
        target: &str,
        url: &str,
    ) -> String {
        let Some(recipes) = self.recipes.clone() else {
            return "(the recipe engine isn't configured on this box)".to_string();
        };
        let var = spec.get("var").and_then(|x| x.as_str()).unwrap_or("out").to_string();
        let label = spec.get("label").and_then(|x| x.as_str()).unwrap_or(&sk.name).to_string();
        if target.len() < 2 {
            return format!(
                "(\"{}\" watches the {label} for something you name -- say \"run skill {}: <target>\")",
                sk.name, sk.name
            );
        }
        let mut targs = spec.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));
        if spec.get("needs_url").and_then(|x| x.as_bool()).unwrap_or(false) {
            targs = serde_json::json!({ "url": url });
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let rec = Recipe {
            id: "skill".into(),
            name: format!("run {}: {target}", sk.name),
            steps: vec![
                RecipeStep::WaitForCondition { tool_name: tool_name.into(), args: targs, store_as: var.clone(), condition: Condition::VarContains { var, substring: target.to_string() }, poll_secs: 120, expire_ms: now + 24 * 3600 * 1000 },
                RecipeStep::Notify { message: format!("📡 the {label} now matches \"{target}\".") },
            ],
        };
        // The outcome is recorded AFTER the run and reports what happened (E.SK1).
        let out = recipes.run_with(&rec, std::collections::HashMap::new()).await;
        let _ = self.memory.record_skill_outcome(&sk.name, out.ok).await;
        if out.sleeping_until.is_some() {
            format!("Running skill '{}' — watching {label} for \"{target}\".", sk.name)
        } else if !out.notifications.is_empty() {
            out.notifications.join("\n")
        } else {
            format!("(skill '{}' ran but produced nothing)", sk.name)
        }
    }

    pub(crate) async fn run_instruction_skill(&self, sk: &mind_types::Skill, instructions: &str, target: &str) -> String {
        // EITHER executor will do, but one of them must exist before a row is written -- the
        // executor-presence rule `delegate_cmd` keeps: a ledger row for a job that cannot run is a
        // lie on the board.
        let researcher = self.researcher.clone();
        let recipes = self.recipes.clone();
        if researcher.is_none() && recipes.is_none() {
            return "(the recipe engine isn't configured on this box)".to_string();
        }
        if !self.try_acquire_bg(3) {
            return "(the job board is full — a few jobs are already running; `ym jobs` to see them)".to_string();
        }
        let id = format!("{:x}", chrono::Utc::now().timestamp_millis() & 0xffffff);
        let now = chrono::Utc::now().timestamp_millis();
        let task = if target.trim().is_empty() { sk.summary.clone() } else { target.trim().to_string() };
        let mut rows: Vec<serde_json::Value> = self
            .memory
            .profile_get(LEDGER_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        rows.push(serde_json::json!({
            "id": id, "name": sk.name, "task": task, "kind": "skill",
            "status": "running", "started_ms": now,
        }));
        if rows.len() > LEDGER_CAP {
            let cut = rows.len() - LEDGER_CAP;
            rows.drain(..cut);
        }
        let _ = self.memory.profile_set(LEDGER_KEY, &serde_json::to_string(&rows).unwrap_or_default()).await;

        let prompt = crate::import_skill::instruction_prompt(instructions, Some(target));
        let (q, jobs, mem) = (self.notify_queue.clone(), self.bg_jobs.clone(), self.memory.clone());
        let (id2, name2, task2) = (id.clone(), sk.name.clone(), task.clone());
        let trace = format!("skill:{}:{}", sk.name, id);
        tokio::spawn(async move {
            // The flight trace first, so a run that dies still says what it was.
            scratch_note(&mem, &id2, &format!("trace: {trace}")).await;
            scratch_note(&mem, &id2, &format!("task: {task2}")).await;
            let (ok, msg) = match (researcher, recipes) {
                // A document that says "use live data only" needs live data. E.SK1 ran documents
                // through a bare `Think` step because the SCHEDULED path used one, and never asked
                // whether that executor could do the work the documents describe -- so
                // `test-market` ran, followed its instructions correctly, and reported that it
                // could not comply. The research executor two feet away has the tools, and the
                // same document through it returned real quotes (E.SK3).
                (Some(r), _) => {
                    scratch_note(&mem, &id2, "following banked instructions (research)").await;
                    let res = r.run(&prompt).await;
                    // Findings land in SCRATCH, not memory -- the promotion gate (`jobs keep`) is
                    // the only door from a job's output into the mind's real memory.
                    for u in res.sources.iter().take(10) {
                        scratch_note(&mem, &id2, &format!("source: {u}")).await;
                    }
                    scratch_note(&mem, &id2, &res.answer).await;
                    // ASK THE FIELD. `!answer.is_empty()` was a guess, and a synthesis failure
                    // arrives AS text — `(sub-agent synthesis error: …)` is not empty — so an API
                    // error went on the board as a finished deliverable and the skill was credited
                    // with a success. Reported live by Pranab on an NVDA run (E.SK5).
                    let ok = res.ok();
                    let mut m = if ok {
                        format!("📥 [{name2}] {}", res.answer)
                    } else {
                        let why = res.error.clone().unwrap_or_else(|| "the instructions produced nothing".into());
                        scratch_note(&mem, &id2, &format!("failed: {why}")).await;
                        format!("📥 [{name2}] I couldn't finish it: {why}")
                    };
                    // The sources go out either way: they are what the run DID accomplish, and on a
                    // failure they are the difference between "it broke" and "it broke after
                    // reading six pages, here they are".
                    if !res.sources.is_empty() {
                        m.push_str("\n\nSources:\n");
                        for u in res.sources.iter().take(6) {
                            m.push_str(&format!("- {u}\n"));
                        }
                    }
                    (ok, m)
                }
                // No researcher: the bare recipe, so a box without one keeps the executor it had.
                (None, Some(recipes)) => {
                    scratch_note(&mem, &id2, "following banked instructions").await;
                    let steps = crate::import_skill::instruction_steps_from_prompt(&name2, prompt);
                    let rec = Recipe { id: format!("skill:{name2}"), name: format!("run {name2}: {task2}"), steps };
                    let out = recipes.run_with(&rec, std::collections::HashMap::new()).await;
                    if out.ok {
                        let m = out
                            .notifications
                            .last()
                            .cloned()
                            .unwrap_or_else(|| format!("📥 [{name2}] done."));
                        (true, m)
                    } else {
                        let why = out.error.clone().unwrap_or_else(|| "the instructions produced nothing".into());
                        scratch_note(&mem, &id2, &format!("failed: {why}")).await;
                        (false, format!("📥 [{name2}] I couldn't finish it: {why}"))
                    }
                }
                // Refused before the row was written, above.
                (None, None) => (false, format!("📥 [{name2}] no executor.")),
            };
            // AFTER the run, and reporting what actually happened. NOTE: `ok` means the executor
            // completed, not that the task was accomplished -- a document that correctly refuses
            // still counts here. Named as a residual in E.SK3; separating them needs a judge on
            // the deliverable.
            let _ = mem.record_skill_outcome(&name2, ok).await;
            ledger_update(&mem, &id2, if ok { "done" } else { "failed" }, Some(msg.clone())).await;
            q.lock().unwrap().push(msg);
            jobs.fetch_sub(1, Ordering::Relaxed);
        });
        format!(
            "📥 Running \"{}\" on the board (job {id}) — {}. I'll bring back the deliverable; `ym jobs` shows it working.",
            sk.name,
            if target.trim().is_empty() { sk.summary.clone() } else { format!("input: {}", target.trim()) }
        )
    }

    pub async fn delegate_cmd(&self, rest: &str) -> String {
        let Some((name, task, _floor)) = parse_delegation(rest) else {
            return "Usage: `ym delegate <name>: <task>` (e.g. `ym delegate quant-check: compare DeepSeek IQ2 vs Q3 quality claims`).".to_string();
        };
        // A DELEGATION NAMED AFTER A BANKED SKILL RUNS THAT SKILL, with the task as its input.
        //
        // Without this the name was only a label: `delegate test-market: check WMT` routed on the
        // TASK alone, started a generic research job, and never opened the 6,149-byte document the
        // name refers to. Every prior `test-market · research` row on the board is that — a decent
        // answer that owes nothing to the instructions it was supposed to follow.
        //
        // EXACT match only. A fuzzy one would hijack delegations that merely resemble a skill name.
        // Classification comes from the shared `classify_skill` and dispatches to the same three
        // runners the phrase path and the tool arm use, so this adds a caller, not an executor
        // (E.SK4).
        if let Ok(Some(sk)) = self.memory.get_skill(&name).await {
            return match crate::skills::classify_skill(&sk) {
                crate::skills::SkillBody::Code { lang, source } => self.run_code_skill(&sk, lang, &source).await,
                crate::skills::SkillBody::Instructions { text } => self.run_instruction_skill(&sk, &text, &task).await,
                crate::skills::SkillBody::Capability { tool, spec } => {
                    self.run_capability_skill(&sk, &tool, &spec, &task, "").await
                }
            };
        }
        let kind = self.route(&task).await;
        // Executor presence FIRST — a ledger row for a job that can't run is a lie on the board.
        let runnable = match kind {
            "code" => self.coder.is_some(),
            "page" => self.recipes.is_some(),
            _ => self.researcher.is_some(),
        };
        if !runnable {
            return format!("(the {kind} executor isn't configured on this box)");
        }
        if !self.try_acquire_bg(3) {
            return "(the job board is full — a few delegations are already running; `ym jobs` to see them)".to_string();
        }
        let id = format!("{:x}", chrono::Utc::now().timestamp_millis() & 0xffffff);
        let now = chrono::Utc::now().timestamp_millis();
        let mut rows: Vec<serde_json::Value> = self
            .memory
            .profile_get(LEDGER_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        rows.push(serde_json::json!({
            "id": id, "name": name, "task": task, "kind": kind,
            "status": "running", "started_ms": now,
        }));
        if rows.len() > LEDGER_CAP {
            let cut = rows.len() - LEDGER_CAP;
            rows.drain(..cut);
        }
        let _ = self.memory.profile_set(LEDGER_KEY, &serde_json::to_string(&rows).unwrap_or_default()).await;

        let (q, jobs, mem) = (self.notify_queue.clone(), self.bg_jobs.clone(), self.memory.clone());
        let (id2, name2, task2) = (id.clone(), name.clone(), task.clone());
        if kind == "page" {
            let engine = self.recipes.clone().unwrap();
            let pack_rules = self.memory.pack_context().await.ok().flatten();
            tokio::spawn(async move {
                scratch_note(&mem, &id2, &format!("task: {task2}")).await;
                scratch_note(&mem, &id2, "chain: research → author → publish").await;
                let out = engine.run_with(&page_recipe(&name2, &task2, pack_rules.as_deref()), std::collections::HashMap::new()).await;
                // The URL is the deliverable. A chain that "succeeded" without one has not built
                // anything, so that is reported as a failure rather than as a cheerful empty result.
                let url = out.vars.get("url").and_then(|v| v.as_str()).unwrap_or_default().to_string();
                let msg = if out.ok && !url.is_empty() {
                    scratch_note(&mem, &id2, &format!("published: {url}")).await;
                    out.notifications
                        .last()
                        .cloned()
                        .unwrap_or_else(|| format!("🌐 [{name2}] it's live — {url}"))
                } else {
                    let why = out.error.unwrap_or_else(|| "the page step produced no document".into());
                    scratch_note(&mem, &id2, &format!("failed: {why}")).await;
                    format!("🌐 [{name2}] I couldn't finish the page: {why}")
                };
                ledger_update(&mem, &id2, if out.ok && !url.is_empty() { "done" } else { "failed" }, Some(msg.clone())).await;
                q.lock().unwrap().push(msg);
                jobs.fetch_sub(1, Ordering::Relaxed);
            });
        } else if kind == "code" {
            // ITERATE-UNTIL-GOOD (the Hermes pattern, 2026-08-06): one coder pass produces a first
            // draft; real artifacts need build → critique → improve until a bar is met. Same shape
            // as the nightly self-improve loop, generalized from "the mind's codebase" to "whatever
            // was delegated". Every round narrates into scratch, so the channel thread shows the
            // loop working — and the critique trail survives for the promotion gate to keep.
            let c = self.coder.clone().unwrap();
            let house = self.inference.clone();
            let named_critic = critic_from_env();
            // 50, not 3. The cap is a RUNAWAY BACKSTOP, not the quality bar: the loop's real exits
            // are the critic's SHIP, the dead-provider guard, and the fail-closed critic path. With
            // those in place a low cap only truncates honest iteration — the whole point is to keep
            // going until the work is good, and a ~15-min round on a cached subscription makes 50
            // affordable. (When the cap DOES fire, the result says "round limit" and carries the
            // last review, so a stuck loop is visible, not silent.)
            let rounds: usize = std::env::var("YM_DELEGATE_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(50);
            // COLD by default. Warm `--continue` resume was measured to be the loop's dominant cost:
            // it replays the accumulated transcript on every tool call of every later round, so spend
            // grows with the SQUARE of total turns. The trade it bought — a builder that remembers its
            // own intent — is now bought instead by history_block(), at kilobytes instead of megabytes.
            // Kept switchable because the honest comparison needs a live run: if a cold builder is
            // seen re-exploring or undoing its own work, YM_DELEGATE_RESUME=warm restores the old
            // behaviour without a deploy.
            let warm_resume = std::env::var("YM_DELEGATE_RESUME")
                .map(|v| v.trim().eq_ignore_ascii_case("warm"))
                .unwrap_or(false);
            tokio::spawn(async move {
                // Who is judging is part of the record: a run reviewed by the local pool and one
                // reviewed by a peer-strength model are not the same evidence.
                let (critic, critic_label) = match &named_critic {
                    Some((pool, spec)) => (pool, spec.clone()),
                    None => (&house, format!("{} (household route)", house.provider())),
                };
                scratch_note(&mem, &id2, &format!("critic: {critic_label}")).await;
                let mut wd: Option<String> = None;
                let mut last: Option<mind_tools::coder::CoderResult> = None;
                let mut verdict = String::new();
                // Reviewed means A CRITIC ACTUALLY RAN. It used to be inferred from the verdict,
                // and a dead critic was papered over with a literal "SHIP" — inference failure was
                // scored as passing review. Quality must fail CLOSED: unreviewed work goes out
                // labelled unreviewed.
                let mut reviewed = false;
                // The final round's review came back empty even on retry. Distinct from `!reviewed`
                // (no critic ever answered) and from a round-limit stop (a real review is standing),
                // because the honest report differs: earlier rounds may have been reviewed while the
                // CURRENT artifact was not.
                let mut inconclusive = false;
                // Post-round fingerprint of the workdir, for the dead-provider guard below.
                let mut prev_snap: Option<std::collections::BTreeMap<String, (u64, i64)>> = None;
                let mut brief = task2.clone();
                // The mind's memory of this job: one line per finished round, "what it touched →
                // what the critic still said". This is what a cold builder gets instead of the
                // previous round's session, and it is the whole reason a cold builder is viable.
                let mut trail: Vec<String> = Vec::new();
                for round in 1..=rounds {
                    scratch_note(&mem, &id2, &format!("round {round}: building — {}", brief.chars().take(160).collect::<String>())).await;
                    // The builder is told where it stands: what round, how many remain, and that
                    // the clock is real. It cannot triage what it does not know — the first run
                    // spent its whole budget reading because nothing told it there was a budget.
                    let situated = format!(
                        "(Round {round} of at most {rounds}. Your wall clock is limited and expires WITHOUT WARNING — whatever is on disk at that moment is what gets judged. Land one change fully before starting the next, and keep notes of intent in the files themselves.)\n\n{brief}"
                    );
                    let res = match &wd {
                        // Rounds after the first run COLD in the same workdir: same tree, same files,
                        // fresh context. What the resumed session used to supply — why the builder
                        // made its choices, what it already tried — now arrives as text in the brief
                        // (see history_block). The tree itself is the other half of that memory: the
                        // files on disk ARE the state, and re-reading them is cheap next to replaying
                        // a transcript that grows every turn.
                        Some(w) if warm_resume => c.continue_in(&situated, w.clone()).await,
                        Some(w) => c.run_in(&situated, w.clone()).await,
                        None => c.run(&situated).await,
                    };
                    let r = match res {
                        Ok(r) => r,
                        Err(e) => {
                            // A spawn/IO error means there is genuinely nothing to salvage — this
                            // is the only path that still fails the round outright. If an earlier
                            // round produced work, fall through and let it be judged instead of
                            // discarding it.
                            scratch_note(&mem, &id2, &format!("round {round}: build error — {e}")).await;
                            if last.is_some() {
                                break;
                            }
                            let msg = format!("🛠️ [{name2}] failed in round {round}: {e}");
                            ledger_update(&mem, &id2, "failed", Some(msg.clone())).await;
                            q.lock().unwrap().push(msg);
                            jobs.fetch_sub(1, Ordering::Relaxed);
                            return;
                        }
                    };
                    wd = Some(r.workdir.clone());
                    // Meter EVERY round, before any of the early-exit paths below — a round that
                    // failed still burned whatever it burned getting there, and those are exactly
                    // the rounds a cost review wants to see.
                    record_round_spend(r.spend.as_ref(), &id2, round);
                    if let Some(s) = r.spend.as_ref() {
                        // Also on the job's own trail, so `ym jobs` shows the cost next to the work
                        // instead of making you cross-reference a separate ledger.
                        scratch_note(&mem, &id2, &format!(
                            "round {round}: spend — {} tokens (cache_r={}), ${:.4}",
                            s.total_tokens(), s.cache_read, s.usd
                        )).await;
                    }
                    // A round that FAILED and touched NOTHING is a dead provider, not a bad build:
                    // iterating cannot help, and each retry costs a full-session replay. Keep the
                    // previous round's work as the result (do not let the dead round become `last`)
                    // and stop here. First round is exempt — there is nothing to compare against,
                    // and a round-1 failure already has its own paths below.
                    let snap = workdir_snapshot(&r.workdir);
                    let round_changed_nothing = prev_snap.as_ref() == Some(&snap);
                    prev_snap = Some(snap);
                    if !r.ok && !r.timed_out && round_changed_nothing && last.is_some() {
                        scratch_note(&mem, &id2, &format!(
                            "round {round}: builder failed without touching a file ({}) — provider-level failure, stopping with round {}'s work",
                            r.summary.chars().take(160).collect::<String>(),
                            round - 1
                        )).await;
                        break;
                    }
                    // STALEMATE: the builder ran fine but wrote nothing — it considers the critique
                    // addressed or won't act on it. Re-critiquing an identical tree can only repeat
                    // itself; with a 50-round cap that is hours of perfectly circular work. Stop and
                    // let the standing review speak. (A high cap only works because the loop can
                    // tell "still improving" from "going in circles" — this is the second half of
                    // that, the dead-provider guard being the first.)
                    if r.ok && round_changed_nothing && last.is_some() {
                        scratch_note(&mem, &id2, &format!(
                            "round {round}: builder made no changes — stalemate with the critic, stopping with the standing review"
                        )).await;
                        break;
                    }
                    if r.timed_out {
                        // The wall clock cut the agent off, but its work up to the cutoff is on
                        // disk. A timed-out round with files is a PARTIAL ROUND to judge — the
                        // previous behaviour discarded a complete, gate-passing redesign because
                        // the agent overran while double-checking it.
                        scratch_note(&mem, &id2, &format!("round {round}: wall clock expired — salvaging {} file(s) from the cutoff", r.files.len())).await;
                        if r.files.is_empty() {
                            let msg = format!("🛠️ [{name2}] failed in round {round}: timed out before producing anything");
                            ledger_update(&mem, &id2, "failed", Some(msg.clone())).await;
                            q.lock().unwrap().push(msg);
                            jobs.fetch_sub(1, Ordering::Relaxed);
                            return;
                        }
                    } else {
                        scratch_note(&mem, &id2, &format!("round {round}: built {} file(s) — {}", r.files.len(), r.summary.chars().take(200).collect::<String>())).await;
                    }
                    // CRITIQUE — a separate set of eyes, judging against the ORIGINAL task (not the
                    // round brief, which narrows every iteration). The critic reads the ARTIFACT
                    // ITSELF, not the builder's account of it: excerpts of the real files, plus the
                    // summary as a claim to check rather than the evidence. "SHIP" ends the loop.
                    // Captured before `r` is moved into `last` below, so the trail entry can be
                    // written once the critic has spoken. File names, not the summary: the summary
                    // is the builder's own claim, and the trail's job is to record what actually
                    // happened to the tree.
                    let round_touched = if r.files.is_empty() {
                        "touched no files".to_string()
                    } else if r.files.len() > 8 {
                        format!("touched {} (+{} more)", r.files[..8].join(", "), r.files.len() - 8)
                    } else {
                        format!("touched {}", r.files.join(", "))
                    };
                    let round_cut = r.timed_out;
                    let excerpt = artifact_excerpt(&r.workdir, &r.files, 12_000);
                    // THE BAR IS THE TASK'S OWN BAR. The old prompt shipped anything that "plausibly
                    // satisfies the task and has no obvious defects" — a smoke test, not a standard,
                    // and it passed round 1 every time even when the task said clean-but-unremarkable
                    // is a FAILURE. A critic that only looks for breakage cannot drive a loop whose
                    // purpose is to make something good.
                    let critique_prompt = format!(
                        "You are the QUALITY BAR for a delegated build, not a smoke test.\n\nTASK AS GIVEN:\n{task2}\n\n\
                         THE ARTIFACT (file excerpts — this is the evidence; the builder's summary below is only a claim to check against it):\n{excerpt}\n\
                         BUILDER'S SUMMARY: {}\n{}\n\
                         Hold the artifact to the standard THE TASK ITSELF sets. If the task asks for excellence, \
                         then \"it works and nothing is broken\" is a FAILING result, not a passing one — absence of \
                         defects is not a reason to ship.\n\n\
                         Reply exactly SHIP only if you would defend this work as FINISHED to the person who asked for it. \
                         Otherwise list up to 4 concrete, specific, imperative findings (one line each). Name what is \
                         merely adequate, not only what is broken: a missed opportunity is a finding. Do not repeat a \
                         finding the artifact already addresses.",
                        r.summary.chars().take(1200).collect::<String>(),
                        if r.timed_out { "NOTE: the builder was stopped by the wall clock mid-run, so the artifact may be mid-edit. Judge what is there.\n" } else { "" },
                    );
                    // BUDGET GENEROUSLY: every model in this fleet is a thinking model, and a thinking
                    // model shares max_tokens with its own reasoning — the answer is whatever is left.
                    // Measured: deepseek-v4-pro spends 57 tokens reasoning about a ONE-WORD probe and
                    // returns an EMPTY string at max_tokens=16 (finish_reason "length"). At 2000 a real
                    // critique over a 12KB excerpt still came back empty on round 2 of job 701106 while
                    // round 1 succeeded — so the failure is variance under a tight ceiling, which is
                    // the worst kind: it looks like an approving judge. 15k leaves room to reason AND
                    // to say four specific things. A judge that cannot afford to explain itself
                    // defaults to approving, and this loop's whole quality bar is the judge.
                    let cfg = GenerationConfig {
                        max_tokens: 15_000,
                        think: mind_inference::think_for("critique", None),
                        prefer_reasoner: true,
                        ..GenerationConfig::default()
                    };
                    let mut critic_says = critic
                        .chat_scoped(vec![ChatMessage::user(&critique_prompt)], cfg, mind_inference::PrivacyScope::Household)
                        .await
                        .map(|x| x.text.trim().to_string());
                    // AN EMPTY VERDICT IS NOT A REVIEW. It is neither SHIP nor findings, and feeding
                    // it forward hands the next round a brief reading "Fix these review findings:"
                    // followed by nothing — observed live in job 701106, where round 3 was dispatched
                    // with no guidance at all. The generous budget above is the real fix; this is the
                    // backstop for the case it does not cover: retry with thinking OFF, so the entire
                    // budget goes to the answer and no reasoning can crowd it out.
                    if critic_says.as_deref().map(|v| !verdict_is_usable(v)).unwrap_or(false) {
                        scratch_note(&mem, &id2, &format!(
                            "round {round}: critic did not return a review (got {:?}) — retrying",
                            critic_says.as_deref().unwrap_or("").chars().take(120).collect::<String>()
                        )).await;
                        // Thinking stays ON for the retry. Turning it off is what produced the
                        // builder-voice reply in the first place: with no room to reason the judge
                        // pattern-matched the task text and continued it instead of reviewing it.
                        let retry = GenerationConfig { max_tokens: 15_000, think: Some(true), prefer_reasoner: true, ..GenerationConfig::default() };
                        critic_says = critic
                            .chat_scoped(vec![ChatMessage::user(&critique_prompt)], retry, mind_inference::PrivacyScope::Household)
                            .await
                            .map(|x| x.text.trim().to_string());
                    }
                    last = Some(r);
                    match critic_says {
                        // Still not a review after the retry: the judge is not judging. Stop rather
                        // than spend the remaining rounds editing against noise, and do NOT let the
                        // result claim it passed a review that never happened.
                        Ok(v) if !verdict_is_usable(&v) => {
                            scratch_note(&mem, &id2, &format!("round {round}: critic failed to review twice — stopping; the artifact stands unreviewed at this round")).await;
                            inconclusive = true;
                            verdict.clear();
                            break;
                        }
                        Ok(v) => {
                            reviewed = true;
                            verdict = v;
                        }
                        Err(e) => {
                            // Fail CLOSED: no review happened, so nothing may claim to have passed
                            // one. Keep the artifact, end the loop, and say so out loud.
                            scratch_note(&mem, &id2, &format!("round {round}: critic unavailable — {e}")).await;
                            reviewed = false;
                            verdict.clear();
                            break;
                        }
                    }
                    if verdict_ships(&verdict) {
                        scratch_note(&mem, &id2, &format!("round {round}: critique — SHIP")).await;
                        break;
                    }
                    scratch_note(&mem, &id2, &format!("round {round}: critique — {}", verdict.chars().take(400).collect::<String>())).await;
                    // Record the round before briefing the next one. Flattened to a single line so
                    // twenty rounds of history stay readable and bounded — the next builder needs the
                    // SHAPE of what was tried, not a replay of it.
                    trail.push(format!(
                        "round {round}: {round_touched}{} → critic still said: {}",
                        if round_cut { " (cut off mid-edit by the wall clock)" } else { "" },
                        verdict
                            .lines()
                            .map(|l| l.trim())
                            .filter(|l| !l.is_empty())
                            .collect::<Vec<_>>()
                            .join(" | ")
                            .chars()
                            .take(400)
                            .collect::<String>()
                    ));
                    brief = format!(
                        "Improve the existing build in this directory. Fix these review findings:\n{verdict}\nDo not start over; edit in place.\n{}",
                        history_block(&trail, 4_000)
                    );
                }
                let r = last.expect("at least one round ran");
                ledger_artifacts(&mem, &id2, &r.workdir, &r.files).await;
                let shipped = reviewed && verdict_ships(&verdict) && !inconclusive;
                let status_line = if shipped {
                    "done (passed review)"
                } else if !reviewed {
                    "done (UNREVIEWED — the critic was unavailable; treat this as a draft)"
                } else if inconclusive {
                    "done (review INCONCLUSIVE — the critic returned nothing on the final round; treat this as a draft)"
                } else {
                    "done (round limit — last review below)"
                };
                let msg = format!(
                    "🛠️ [{name2}] {status_line}:\n\n{}{}",
                    mind_tools::render_coder(&r),
                    if shipped || verdict.is_empty() { String::new() } else { format!("\n\nOutstanding review notes:\n{verdict}") }
                );
                ledger_update(&mem, &id2, "done", Some(msg.clone())).await;
                q.lock().unwrap().push(msg);
                jobs.fetch_sub(1, Ordering::Relaxed);
            });
        } else {
            let r = self.researcher.clone().unwrap();
            tokio::spawn(async move {
                scratch_note(&mem, &id2, &format!("task: {task2}")).await;
                let res = r.run(&task2).await;
                // Findings land in SCRATCH, not memory — the promotion gate (`jobs keep`) is the
                // only door from a job's output into the mind's real memory.
                for u in res.sources.iter().take(10) {
                    scratch_note(&mem, &id2, &format!("source: {u}")).await;
                }
                scratch_note(&mem, &id2, &res.answer).await;
                // The same rule as the skill path: a delegation that failed says so on the board
                // rather than showing its error message under a green tick (E.SK5).
                let ok = res.ok();
                let mut msg = if ok {
                    format!("🔎 [{name2}] {}", res.answer)
                } else {
                    let why = res.error.clone().unwrap_or_else(|| "the research produced nothing".into());
                    scratch_note(&mem, &id2, &format!("failed: {why}")).await;
                    format!("🔎 [{name2}] I couldn't finish it: {why}")
                };
                if !res.sources.is_empty() {
                    msg.push_str("\n\nSources:\n");
                    for u in res.sources.iter().take(6) {
                        msg.push_str(&format!("- {u}\n"));
                    }
                }
                ledger_update(&mem, &id2, if ok { "done" } else { "failed" }, Some(msg.clone())).await;
                q.lock().unwrap().push(msg);
                jobs.fetch_sub(1, Ordering::Relaxed);
            });
        }
        format!("🧰 Delegated [{id}] \"{name}\" ({kind}). It's on the board — `ym jobs` — and the result lands in chat.")
    }

    /// `ym jobs [json | keep <id> | drop <id>]` — the board, its machine-readable form (the
    /// desktop's channel view), and the two ends of a job's scratch memory.
    pub async fn jobs_report_cmd(&self, rest: &str) -> String {
        let rest = rest.trim();
        if rest == "json" {
            let rows: Vec<serde_json::Value> = self
                .memory
                .profile_get(LEDGER_KEY)
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            // Attach each job's scratch notes — the thread timeline between task and result.
            let mut out = Vec::with_capacity(rows.len());
            for mut r in rows {
                if let Some(id) = r.get("id").and_then(|x| x.as_str()).map(String::from) {
                    let notes: Vec<serde_json::Value> = self
                        .memory
                        .profile_get(&scratch_key(&id))
                        .await
                        .ok()
                        .flatten()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();
                    r["notes"] = serde_json::Value::Array(notes);
                }
                out.push(r);
            }
            return serde_json::json!({ "jobs": out }).to_string();
        }
        if let Some(id) = rest.strip_prefix("keep ") {
            return self.job_promote(id.trim()).await;
        }
        if let Some(id) = rest.strip_prefix("drop ") {
            let n = self.job_purge_scratch(id.trim()).await;
            return format!("🗑 Dropped [{}] scratch ({n} note(s)) — nothing entered memory.", id.trim());
        }
        // `drop` only ever purged the SCRATCH; the ledger row survived, so a finished or failed
        // agent stayed on the board with no way to remove it. Deleting the row is a separate,
        // louder verb because it is the destructive one: the scratch is a working note, the row is
        // the record that the job happened.
        if let Some(id) = rest.strip_prefix("delete ").or_else(|| rest.strip_prefix("forget ")) {
            let id = id.trim();
            let mut rows: Vec<serde_json::Value> = self
                .memory
                .profile_get(LEDGER_KEY)
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            let before = rows.len();
            let running = rows.iter().any(|r| {
                r.get("id").and_then(|x| x.as_str()) == Some(id)
                    && r.get("status").and_then(|x| x.as_str()) == Some("running")
            });
            if running {
                return format!(
                    "[{id}] is still running — deleting its row would leave the work with no record. Let it finish, or restart the service to interrupt it, then delete."
                );
            }
            rows.retain(|r| r.get("id").and_then(|x| x.as_str()) != Some(id));
            if rows.len() == before {
                return format!("No job [{id}] on the board.");
            }
            let n = self.job_purge_scratch(id).await;
            let _ = self
                .memory
                .profile_set(LEDGER_KEY, &serde_json::to_string(&rows).unwrap_or_default())
                .await;
            return format!("🗑 Deleted [{id}] from the board, with its {n} scratch note(s).");
        }
        let rows: Vec<serde_json::Value> = self
            .memory
            .profile_get(LEDGER_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Stale-scratch hygiene rides board renders: un-promoted scratch of long-finished jobs is
        // junk by definition (the promotion window has clearly passed).
        let now = chrono::Utc::now().timestamp_millis();
        for r in rows.iter() {
            let finished = r.get("finished_ms").and_then(|x| x.as_i64()).unwrap_or(i64::MAX);
            if now - finished > SCRATCH_STALE_MS {
                if let Some(id) = r.get("id").and_then(|x| x.as_str()) {
                    let _ = self.job_purge_scratch(id).await;
                }
            }
        }
        render_board(&rows, now)
    }

    /// PROMOTION GATE — the one door from a job's scratch into the mind's real memory. Writes an
    /// OBSERVATION with sub-agent provenance, never a belief: the belief path keeps its stricter
    /// gates (wrong beliefs are recalled confidently forever). Scratch is destroyed after.
    async fn job_promote(&self, id: &str) -> String {
        let notes: Vec<serde_json::Value> = self
            .memory
            .profile_get(&scratch_key(id))
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if notes.is_empty() {
            return format!("[{id}] has no scratch to keep (already promoted, dropped, or never noted).");
        }
        let body: Vec<&str> = notes.iter().filter_map(|n| n.get("note").and_then(|x| x.as_str())).collect();
        let text = format!("Delegated job [{id}] findings: {}", body.join(" | "));
        let text: String = text.chars().take(4000).collect();
        match self.memory.remember_observation(&text, mind_types::safety::ProvenanceCategory::SubAgent).await {
            Ok(_) => {
                let n = self.job_purge_scratch(id).await;
                format!("📥 Kept [{id}] — {n} note(s) promoted into memory as a sub-agent observation; scratch destroyed.")
            }
            Err(e) => format!("(couldn't promote [{id}]: {e} — scratch left intact)"),
        }
    }

    async fn job_purge_scratch(&self, id: &str) -> usize {
        let key = scratch_key(id);
        let n = self
            .memory
            .profile_get(&key)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
            .map(|v| v.len())
            .unwrap_or(0);
        let _ = self.memory.profile_set(&key, "[]").await;
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_name_and_task_split_on_colon() {
        let (n, t, k) = parse_delegation("quant-check: compare DeepSeek IQ2 vs Q3 claims").unwrap();
        assert_eq!(n, "quant-check");
        assert!(t.starts_with("compare"));
        assert_eq!(k, "research");
    }

    #[test]
    fn code_shaped_tasks_route_to_the_coder() {
        let (_, _, k) = parse_delegation("log-tool: build a CLI for parsing the tick logs").unwrap();
        assert_eq!(k, "code");
    }

    #[test]
    fn a_url_colon_is_not_a_name_split() {
        let (n, t, _) = parse_delegation("summarize https://example.com/post fully").unwrap();
        assert!(t.contains("https://example.com/post"), "task lost the url: {t}");
        assert!(!n.contains("https"), "name should be derived words, got {n}");
    }

    #[test]
    fn board_shows_running_before_done_and_says_so() {
        let rows = vec![
            serde_json::json!({"id":"a1","name":"old","task":"x","kind":"research","status":"done","started_ms":1000,"result":"found it"}),
            serde_json::json!({"id":"b2","name":"live","task":"y","kind":"code","status":"running","started_ms":2000}),
        ];
        let out = render_board(&rows, 8_000_000);
        let live = out.find("live").unwrap();
        let old = out.find("old").unwrap();
        assert!(live < old, "running job must render first:\n{out}");
        assert!(out.contains("⏳ running") && out.contains("✅ done"));
    }

    #[test]
    fn empty_board_teaches_the_verb() {
        assert!(render_board(&[], 0).contains("ym delegate"));
    }
}

#[cfg(test)]
mod iterate_tests {
    use super::*;

    #[test]
    fn ship_verdicts_end_the_loop_and_critiques_do_not() {
        for v in ["SHIP", "ship", "  Ship.  ", "SHIP — looks solid"] {
            assert!(verdict_ships(v), "{v:?} should ship");
        }
        for v in ["Fix the contrast before you ship", "1. broken nav\n2. SHIP the fixed css", "needs work"] {
            assert!(!verdict_ships(v), "{v:?} must keep iterating");
        }
    }

    /// The page/code boundary: a page noun describes the DELIVERABLE only when the task is not
    /// pointing at existing source. The cockpit brief once had to be worded around the words
    /// "site", "page" and "dashboard" to reach the coder — that contortion is the bug.
    #[test]
    fn existing_source_beats_page_nouns() {
        for t in [
            "redesign the dashboard UI in /srv/cockpit — start from styles.css and index.html",
            "improve the site's styles.css and app.js in our codebase",
            "rewrite the landing page component in crates/mind-web",
            "polish the settings page in the repo",
        ] {
            assert_eq!(classify(t), "code", "existing source must go to the coder: {t}");
        }
        // Without a codebase signal the page nouns still mean a page.
        for t in [
            "create a stunning portfolio website for me",
            "make me a landing page for the product",
        ] {
            assert_eq!(classify(t), "page", "a new standalone page stays a page: {t}");
        }
    }

    /// Improvement verbs are MAKE verbs: asking for something to be made better is asking for
    /// work, not for reading. "improve the dashboard" used to route to research.
    #[test]
    fn improvement_verbs_are_make_verbs() {
        for t in ["improve the dashboard", "redesign the cockpit app", "polish the CLI tool", "modernize the settings form"] {
            assert_ne!(classify(t), "research", "an improvement ask is work: {t}");
        }
    }

    /// The judge must answer in the judge's voice. Shape-checking the verdict is what stops a
    /// builder-preamble reply from being fed forward as findings (job bf77af, round 3 → 4).
    #[test]
    fn a_verdict_must_look_like_a_review() {
        assert!(verdict_is_usable("SHIP"), "ship is always usable");
        assert!(verdict_is_usable("1. Tune line-height to at least 1.6\n2. Differentiate the sub-line"));
        assert!(verdict_is_usable("- the empty state hangs from the top\n- the fade erases data"));
        // The exact reply that broke the loop.
        assert!(!verdict_is_usable(
            "I'll start by reading the current state of the files and understanding what we're working with, then write IDEAS.md before making any changes."
        ));
        assert!(!verdict_is_usable("Let me first look at the stylesheet."));
        assert!(!verdict_is_usable(""));
        assert!(!verdict_is_usable("   \n  "));
        // Prose with no enumerated finding is not the requested format.
        assert!(!verdict_is_usable("This looks generally fine to me overall."));
        // A finding that uses first person mid-text is still a finding.
        assert!(verdict_is_usable("1. I would tighten the rail hairlines; they read as borders."));
    }

    /// The dead-provider guard's sensor: identical snapshots mean no work happened; any size or
    /// mtime movement reads as change.
    #[test]
    fn workdir_snapshots_detect_change_and_stillness() {
        let wd = mind_types::scratch::dir("snap_test_");
        std::fs::create_dir_all(&wd).unwrap();
        std::fs::write(wd.join("a.txt"), "one").unwrap();
        let s1 = workdir_snapshot(&wd.to_string_lossy());
        let s2 = workdir_snapshot(&wd.to_string_lossy());
        assert_eq!(s1, s2, "an untouched tree must fingerprint identically");
        std::fs::write(wd.join("a.txt"), "different length").unwrap();
        let s3 = workdir_snapshot(&wd.to_string_lossy());
        assert_ne!(s1, s3, "an edit must change the fingerprint");
        std::fs::write(wd.join(".hidden"), "x").unwrap();
        let s4 = workdir_snapshot(&wd.to_string_lossy());
        assert_eq!(s3, s4, "dotfiles are the agent's own state, not the artifact");
        std::fs::remove_dir_all(&wd).ok();
    }

    /// The cold builder's only memory of earlier rounds. Must stay bounded (it replaced an
    /// unbounded transcript), keep the NEWEST rounds when it has to choose, emit them oldest-first
    /// so the brief reads forward, and say out loud when it dropped something.
    #[test]
    fn history_block_is_bounded_and_keeps_the_newest_rounds() {
        assert_eq!(history_block(&[], 4_000), "", "no rounds yet means no history section at all");

        let trail: Vec<String> = (1..=6).map(|n| format!("round {n}: touched a.js → critic still said: fix {n}")).collect();

        // Generous budget: every round survives, oldest first.
        let full = history_block(&trail, 4_000);
        let pos1 = full.find("round 1:").expect("oldest round present");
        let pos6 = full.find("round 6:").expect("newest round present");
        assert!(pos1 < pos6, "rounds must read oldest-first so the brief runs forward into the findings");
        assert!(!full.contains("omitted"), "nothing was dropped, so nothing should claim to be");

        // Tight budget: the RECENT rounds are the ones that stop a repeated approach, so they win.
        let tight = history_block(&trail, 120);
        assert!(tight.len() < full.len(), "a tight budget must actually bind");
        assert!(tight.contains("round 6:"), "the newest round is the one a builder most needs");
        assert!(!tight.contains("round 1:"), "the oldest round is dropped first");
        assert!(tight.contains("omitted"), "a silent drop would misrepresent the history as complete");

        // Never drop everything: one oversized entry still comes through rather than vanishing.
        let huge = vec![format!("round 9: {}", "x".repeat(5_000))];
        assert!(history_block(&huge, 100).contains("round 9:"), "a single oversized round must not be silently swallowed");
    }

    /// The critic reads the artifact itself. Excerpts respect the budget, mark binary files
    /// without quoting them, and never split a UTF-8 character.
    #[test]
    fn artifact_excerpts_are_bounded_and_binary_safe() {
        let wd = mind_types::scratch::dir("excerpt_test_");
        std::fs::create_dir_all(&wd).unwrap();
        std::fs::write(wd.join("notes.md"), "café ".repeat(1_000)).unwrap(); // multibyte, > per-file cap
        std::fs::write(wd.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
        let files = vec!["notes.md".to_string(), "blob.bin".to_string()];
        let ex = artifact_excerpt(&wd.to_string_lossy(), &files, 4_000);
        assert!(ex.len() <= 4_600, "budget overshot: {} bytes", ex.len());
        assert!(ex.contains("café"), "text content must be quoted");
        assert!(ex.contains("binary — not shown"), "binary must be named, not quoted");
        assert!(std::str::from_utf8(ex.as_bytes()).is_ok(), "must never split a UTF-8 char");
        std::fs::remove_dir_all(&wd).ok();
    }
}
