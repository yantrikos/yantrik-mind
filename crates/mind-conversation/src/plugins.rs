//! plugins — a DECLARATIVE registry so capabilities are configured, not code-edited. Every plugin
//! (native or MCP) is an entry with a security level + an enabled flag, overlaid from a JSON manifest
//! (`plugins.json`). Toggling, securing, or listing a plugin needs ZERO code change — the agent's
//! tool catalog is generated from the ENABLED entries, so disabling one removes it everywhere.
//!
//! Honest scope: a native plugin's *behavior* is compiled Rust (you can't conjure new native logic
//! from JSON). What the manifest controls is registration/enable/security/presentation. For a
//! genuinely-new capability with no code at all, add an MCP server — which is itself a manifest.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::ConversationEngine;

/// How risky a plugin is — drives presentation and (for writes) gating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecurityLevel {
    /// Public data, no side effects — runs freely.
    ReadOnly,
    /// Reads the user's PERSONAL data (inbox, home, finances). Runs, but flagged so it's visible.
    Personal,
    /// Outward / mutating effect — always routed through the harm-gate + a confirmation handshake.
    GatedWrite,
}

impl SecurityLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::Personal => "personal",
            Self::GatedWrite => "gated_write",
        }
    }
    pub fn badge(&self) -> &'static str {
        match self {
            Self::ReadOnly => "🟢 read-only",
            Self::Personal => "🔒 personal",
            Self::GatedWrite => "⚠ gated-write",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().replace('-', "_").as_str() {
            "read_only" | "readonly" | "read" => Some(Self::ReadOnly),
            "personal" | "private" => Some(Self::Personal),
            "gated_write" | "gated" | "write" => Some(Self::GatedWrite),
            _ => None,
        }
    }
}

/// Where a capability's behavior came from — the seed of pack provenance. Builtin = compiled into
/// this crate; Imported = brought in from a document/manifest (SKILL.md, MCP); SelfAuthored = the
/// mind wrote it for itself. Only Builtin ships today; this field is the hook the pack ladder
/// hangs promotion + certification on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    Builtin,
    Imported,
    SelfAuthored,
}

impl Provenance {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Imported => "imported",
            Self::SelfAuthored => "self_authored",
        }
    }
}

/// A runtime dependency a capability cannot work without.
///
/// This exists because the first version of the availability probe was a hand-written `match` on
/// capability ids — and it was wrong in exactly the way such a thing always goes wrong. It checked
/// `"wiki"` while the registry's id is `wikipedia`, so that capability was never probed at all; and
/// five ids matched nothing, so they reported READY unconditionally. Everything looked green,
/// including things that could not have worked.
///
/// The fix is to make the dependency part of the DECLARATION rather than a lookup table that has to
/// be kept in sync by hand. A new capability now states what it needs, and the probe is a loop over
/// that list — so the failure mode becomes "someone forgot to declare a requirement" (visible in one
/// place, next to the spec) instead of "the id in the match arm has a typo" (invisible, and silently
/// green). Each variant maps to one concrete `Option<Arc<dyn …>>` on the engine, so a variant cannot
/// exist without something real to check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requirement {
    WebSearch,
    WebFetch,
    News,
    Weather,
    Wiki,
    Markets,
    Translator,
    HomeAssistant,
    Github,
    Coder,
    /// Any readable mailbox — the bot's own or one of the personal scan inboxes.
    Mailbox,
    /// The bounded research sub-agent.
    Researcher,
}

impl Requirement {
    /// What the operator has to do about it, in their terms — not the field name that is missing.
    pub fn unmet_reason(self) -> &'static str {
        match self {
            Self::WebSearch => "no web search backend is configured",
            Self::WebFetch => "no web fetcher is configured",
            Self::News => "no news client is configured",
            Self::Weather => "no weather client is configured",
            Self::Wiki => "no Wikipedia client is configured",
            Self::Markets => "no market-data client is configured",
            Self::Translator => "no translator is configured",
            Self::HomeAssistant => "Home Assistant is not connected — set YM_HA_URL and YM_HA_TOKEN",
            Self::Github => "no GitHub token — set YM_GITHUB_TOKEN",
            Self::Coder => {
                "the agentic coder needs the claude CLI plus MINIMAX_API_KEY or CLAUDE_CODE_OAUTH_TOKEN"
            }
            Self::Mailbox => "no mailbox is connected — set YM_EMAIL with an app password (or a YM_SCAN_EMAIL account)",
            Self::Researcher => "the research sub-agent is not wired",
        }
    }
}

/// The BEHAVIOR half of a plugin. PluginSpec DECLARES a capability (identity, security, enabled,
/// catalog); a CapabilityHandler IMPLEMENTS it, and the registry — not a hardcoded match arm —
/// routes to it. A handler returns None for a name it doesn't own, which falls through to the
/// legacy match in lib.rs: the strangler seam that lets domains leave that match one at a time.
#[async_trait]
pub trait CapabilityHandler: Send + Sync {
    /// The PluginSpec id this handler implements.
    fn id(&self) -> &str;
    /// Does this capability serve `ym` commands at all? Tool-only capabilities return false so a
    /// shared alias word (e.g. coder's "code" vs the repo browser's `ym code`) never gets
    /// short-circuited by the disabled-plugin gate on the CLI path.
    fn handles_commands(&self) -> bool {
        true
    }
    /// Answer a `ym` command word this plugin's aliases own. None = not mine, fall through.
    async fn handle_command(&self, host: &ConversationEngine, cmd: &str, rest: &str) -> Option<String>;
    /// Answer a run_agent_tool name this plugin's tools own. None = not mine, fall through.
    async fn handle_tool(&self, host: &ConversationEngine, tool: &str, args: &Value) -> Option<String>;
}

/// One declared plugin.
#[derive(Clone, Debug)]
pub struct PluginSpec {
    pub id: String,
    pub title: String,
    pub category: String,
    pub security: SecurityLevel,
    pub enabled: bool,
    /// run_agent_tool tool-names this plugin owns (disabling the plugin disables these).
    pub tools: Vec<String>,
    /// `ym` command aliases this plugin answers to.
    pub aliases: Vec<String>,
    /// The catalog line(s) shown to the agent when the plugin is enabled.
    pub catalog: String,
    /// Where this capability's behavior came from (builtin / imported / self-authored).
    pub provenance: Provenance,
    /// What this capability cannot work without. Empty means pure compute (a calculator needs
    /// nothing) or that its dependencies are internal — either way, always available when enabled.
    pub requires: Vec<Requirement>,
}

impl PluginSpec {
    fn new(
        id: &str,
        title: &str,
        category: &str,
        security: SecurityLevel,
        tools: &[&str],
        aliases: &[&str],
        catalog: &str,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            category: category.into(),
            security,
            enabled: true,
            tools: tools.iter().map(|s| s.to_string()).collect(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            catalog: catalog.into(),
            provenance: Provenance::Builtin,
            requires: Vec::new(),
        }
    }

    /// Declare what this capability needs at runtime. Builder form so the builtin table below stays
    /// one readable line per capability with its requirements right beside it.
    fn requiring(mut self, reqs: &[Requirement]) -> Self {
        self.requires = reqs.to_vec();
        self
    }

    fn matches(&self, name: &str) -> bool {
        let n = name.trim().to_lowercase();
        self.id == n || self.aliases.iter().any(|a| a == &n)
    }

    /// A dynamically-declared plugin (installed pack, imported capability) — same shape as a
    /// builtin, but with caller-chosen provenance. Dynamic specs start DISABLED: certification
    /// (their evals passing) is what turns them on.
    pub fn dynamic(
        id: &str,
        title: &str,
        category: &str,
        security: SecurityLevel,
        tools: &[String],
        aliases: &[String],
        catalog: &str,
        provenance: Provenance,
    ) -> Self {
        Self {
            id: id.trim().to_lowercase(),
            title: title.into(),
            category: category.into(),
            security,
            enabled: false,
            tools: tools.to_vec(),
            aliases: aliases.to_vec(),
            catalog: catalog.into(),
            provenance,
            // A dynamically-installed capability declares no native requirement: its behaviour is a
            // skill/pack recipe, so what it needs is whatever tools that recipe calls — checked when
            // it runs, not here. Certification is the gate that decides whether it may run at all.
            requires: Vec::new(),
        }
    }
}

/// The single source of truth for which capabilities exist, are on, and how risky they are —
/// and, for plugins with a registered CapabilityHandler, HOW they dispatch.
pub struct PluginRegistry {
    plugins: Vec<PluginSpec>,
    handlers: Vec<Arc<dyn CapabilityHandler>>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

impl PluginRegistry {
    /// The built-in native plugins (defaults; the manifest overlays enabled/security). Catalog text
    /// matches what the agent saw before — moving it here just makes the catalog registry-driven.
    pub fn builtin() -> Self {
        use SecurityLevel::*;
        let plugins = vec![
            PluginSpec::new("web_search", "Web search", "Web", ReadOnly, &["search", "web_search"], &["search", "google", "ddg"],
                "- search {query}: web SEARCH (find pages/answers) — use to DISCOVER URLs/facts, then web_fetch to read one")
                .requiring(&[Requirement::WebSearch]),
            PluginSpec::new("web_fetch", "Web fetch", "Web", ReadOnly, &["web_fetch"], &["web", "fetch"],
                "- web_fetch {url}: read a web page (fast — use for real, current info instead of guessing)")
                .requiring(&[Requirement::WebFetch]),
            PluginSpec::new("news", "News", "Web", ReadOnly, &["news", "headlines", "track_news", "follow_news"], &["news", "headlines"],
                "- news {topic}: latest news headlines on a topic (or top stories) — keyless, works for geopolitics/anything
\
                 - track_news {topic}: TRACK a topic + proactively surface fresh headlines")
                .requiring(&[Requirement::News]),
            PluginSpec::new("weather", "Weather", "Web", ReadOnly, &["weather"], &["weather", "wx"],
                "- weather {place}: current conditions + today's forecast for a city/town")
                .requiring(&[Requirement::Weather]),
            PluginSpec::new("wikipedia", "Wikipedia", "Web", ReadOnly, &["wikipedia", "wiki"], &["wiki", "wikipedia"],
                "- wikipedia {query}: a factual summary from Wikipedia (what/who is X)")
                .requiring(&[Requirement::Wiki]),
            PluginSpec::new("calculator", "Calculator", "Utility", ReadOnly, &["calc", "calculate", "math"], &["calc", "calculate", "math"],
                "- calc {expression}: do arithmetic locally (e.g. 12*7+3, (1500*0.18))"),
            PluginSpec::new("translate", "Translate", "Web", ReadOnly, &["translate"], &["translate", "tr"],
                "- translate {to, text}: translate text into a language ('to' like french/hi/es; source auto-detected)")
                .requiring(&[Requirement::Translator]),
            PluginSpec::new("markets", "Market quotes", "Finance", ReadOnly, &["crypto", "coin", "stock", "ticker"], &["crypto", "coin", "stock", "ticker"],
                "- crypto {coin}: a cryptocurrency price + 24h change (e.g. btc, ethereum)
\
                 - stock {symbol}: a stock quote (US ticker, e.g. AAPL)")
                .requiring(&[Requirement::Markets]),
            PluginSpec::new("portfolio", "Portfolio & analysis", "Finance", Personal,
                &["portfolio", "holdings", "my_stocks", "analyze", "analyze_stock", "stock_analysis", "add_holding", "track_holding"],
                &["portfolio", "holding", "holdings", "analyze", "stocks", "position", "analyse", "analysis"],
                "- portfolio {}: the user's investment portfolio — their holdings valued LIVE (price, P&L, allocation)
\
                 - analyze {ticker}: a DEEP multi-source analysis of a stock/crypto (quote+profile+news+web → balanced briefing w/ risks). ANALYSIS, never a buy/sell tip
\
                 - add_holding {ticker, shares, cost?}: record a position the user says they own")
                // Holdings are valued LIVE, so without market data the portfolio cannot be shown.
                .requiring(&[Requirement::Markets]),
            PluginSpec::new("finance", "Finance (subs/bills/budget)", "Finance", Personal,
                &["money", "subscriptions", "finance", "discover_subscriptions", "find_subscriptions", "scan_email_subscriptions", "bills", "budget", "budget_overview"],
                &["money", "finance", "subs", "sub", "subscriptions", "subscription", "bills", "bill", "budget", "budgets", "spent", "spend", "expense", "discover", "scan"],
                "- money {}: the user's finances overview — subscriptions + monthly total
\
                 - bills {}: tracked recurring bills + when they're due
\
                 - budget {}: budget vs spend this month, by category
\
                 - discover_subscriptions {}: scan the user's EMAIL to find recurring subscriptions"),
            PluginSpec::new("home", "Smart home", "Home", Personal, &["home", "home_status", "house", "smart_home"], &["home", "house"],
                "- home {}: check the smart home (Home Assistant) — who's home, climate, what's on")
                .requiring(&[Requirement::HomeAssistant]),
            PluginSpec::new("github", "GitHub", "Dev", Personal, &["github_repo_items", "github_notifications"], &["github", "gh"],
                "- github_repo_items {repo}: list open issues+PRs on \"owner/name\"
\
                 - github_notifications {}: your GitHub notifications")
                .requiring(&[Requirement::Github]),
            PluginSpec::new("research", "Deep research", "Web", ReadOnly, &["research"], &["research"],
                "- research {query}: kick off a DEEP background research job (multi-source) — for big questions, delivers when done")
                // The sub-agent reaches for search and fetch; without either it has nothing to research WITH.
                .requiring(&[Requirement::Researcher, Requirement::WebSearch, Requirement::WebFetch]),
            // SAY WHEN TO CALL IT, not just what it does. The old line — "kick off a background
            // coding job (writes+runs a script in an isolated sandbox)" — describes the mechanism
            // and never the occasion, so a request to AUTHOR something ("create a pack: a markdown
            // corpus and a toml config") does not read as belonging here. It reads as "execute
            // code", which that request is not.
            //
            // Measured on the live dispatch model (qwen3.6:35b, think:false, temperature 0), same
            // catalog, same user turn — "Create a YantrikDB pack … for svg generation":
            //   old line -> EMPTY response, which is exactly the observed live failure
            //               ("[agent] dispatch produced no tool/answer"), ending in the generic
            //               "Sorry — I had trouble putting that together."
            //   this line -> parses, tool = "code", sensible args, first try.
            // The capability was never missing; nothing in the catalog pointed at it. The model
            // then invented `create_yantrikdb_pack`, which is what a model does when the tool it
            // needs is not described as the tool it needs.
            PluginSpec::new("coder", "Code sandbox", "Dev", GatedWrite, &["code"], &["code"],
                "- code {task}: AUTHOR FILES or run code in an isolated sandbox. Use this whenever the user asks you to CREATE, WRITE, BUILD or GENERATE an artifact — a document, config, dataset, corpus, pack, script, page or app. Returns the files it produced.")
                .requiring(&[Requirement::Coder]),
            PluginSpec::new("dashboards", "Dashboards & pages", "Utility", ReadOnly, &["make_dashboard", "publish_page"], &["dashboard"],
                "- make_dashboard {title, sections}: render + host a styled dashboard/list/comparison page, return a URL
\
                 - publish_page {name, html}: host a raw HTML page you wrote + return a URL"),

            PluginSpec::new("monitors", "Monitors", "Utility", ReadOnly, &["set_monitor"], &["monitor"],
                "- set_monitor {source, target, url?}: watch a source (github|web|inbox) + ping on a match"),
            // ── The household surface ─────────────────────────────────────────────────────────
            // These were ~44 lines of hand-written English in a `const LIFE_LINES: &str`, appended
            // to the generated catalog at prompt time. Moving them here makes the registry the ONE
            // place that knows what this mind can do: the catalog is generated, the toggle governs
            // them like everything else, and the agent compiler can resolve a required capability
            // against them instead of grepping prose.
            //
            // The catalog text is moved VERBATIM — those descriptions are what the model reads to
            // choose a tool, so a paraphrase would be a behaviour change dressed as a refactor.
            //
            // No aliases and no handlers, deliberately. Aliases would re-route `ym` command words
            // that the legacy match already answers; a handler is what MOVES behaviour, and this
            // commit moves declarations only. Dispatch still falls through to the match in lib.rs,
            // which is exactly what the strangler seam is for.
            PluginSpec::new("shopping", "Shopping & deals", "Shopping", Personal, &["deals", "watch_price", "watches"], &[],
                "- deals {query, budget?}: find + compare REAL deals on something (great for gifts — I factor in who it's for + budget)\n\
                 - watch_price {query, target?}: start tracking an item's price and ping on a real drop / when it hits a target\n\
                 - watches {}: list what I'm currently price-watching")
                .requiring(&[Requirement::WebSearch]),
            // Markets + media. These shipped as dispatch arms and console commands but were never
            // DECLARED here, so the prose catalog never advertised them — and this module's own
            // header says what happens then: the model confabulates the capability instead of
            // calling it. It did exactly that, quoting `ym quote ^NSEI` back as something the
            // user should run while insisting it had no market-data tool.
            PluginSpec::new("market_data", "Market data", "Research", ReadOnly, &["quote"], &["price"],
                "- quote {symbols}: LIVE price, quote, level or move for a stock, share, index, ticker or crypto — how a market/sector is trading today, up or down, at what level. US equities via Alpaca; Indian listings with the .NS/.BO suffix (RELIANCE.NS, TCS.NS) and indices like ^NSEI (Nifty), ^BSESN (Sensex). CALL THIS for ANY question about what something is worth or how it is doing — never answer a price from memory and never tell the user to run a command instead"),
            PluginSpec::new("media_watch", "Watch media", "Research", ReadOnly, &["watch"], &["listen"],
                "- watch {url, question?}: WATCH, SEE, HEAR or LISTEN to a video, audio, YouTube link, livestream, broadcast, podcast, talk, interview or recording — reads published captions, hears the speech with the local model, and looks at sampled frames, aligned on one timeline. Works on LIVE streams (samples a window of what is airing NOW). CALL THIS for any media link — never say you cannot watch or hear video"),
            PluginSpec::new("web_drive", "Browse", "Research", ReadOnly, &["browse"], &[],
                "- browse {url, goal}: OPEN and drive a real website, web page, portal or app in a real browser toward a goal — navigate, click, read, search, sign in, fill forms, check a dashboard. CALL THIS to look something up on a site rather than saying you have no browser. It stops before anything irreversible — it cannot buy, send, pay or delete"),
            PluginSpec::new("gifting", "Gift intelligence", "Shopping", Personal, &["gift_intel"], &[],
                "- gift_intel {name}: study a person's photos for gift intelligence — what they OWN (never re-gift), their style, what's MISSING that complements it, 3 buyable ideas; chain into `deals` for real listings"),
            PluginSpec::new("people", "People", "People", Personal, &["learn_about", "family", "about_person"], &[],
                "- learn_about {url}: follow a link and learn about a person/thing (recursive: their profiles too)\n\
                 - family {}: the people I keep track of + their upcoming key dates\n\
                 - about_person {name}: what I know about someone in the user's life")
                .requiring(&[Requirement::WebFetch]),
            PluginSpec::new("subject_tracking", "Subject tracking", "Research", ReadOnly, &["track_subject"], &[],
                "- track_subject {subject}: keep a living, evolving understanding of an ongoing topic (re-run → what changed)")
                .requiring(&[Requirement::WebSearch]),
            PluginSpec::new("memory_patterns", "Memory patterns", "Memory", Personal, &["patterns"], &[],
                "- patterns {}: surface non-obvious patterns across what I know about the user"),
            PluginSpec::new("calendar_tools", "Calendar", "Calendar", Personal, &["calendar", "calendar_add", "calendar_remove", "forget_date"], &[],
                "- calendar {}: the unified upcoming view · calendar_add {text}: add an event (Dinner on July 4 at 7pm)\n\
                 - calendar_remove {title}: remove a calendar event by (partial) title — USE THIS when the user says an event/date is wrong or should go\n\
                 - forget_date {name, label}: remove one dated entry (e.g. open house) from a person's profile — the other place a wrong date can live"),
            PluginSpec::new("browser", "Real browser", "Web", ReadOnly, &["see_page"], &[],
                "- see_page {url, question?}: render a page in the real browser, screenshot it, and ANALYZE the image — use when text extraction fails or layout/visuals matter"),
            PluginSpec::new("photo_library", "Photo library", "Photos", Personal, &["photo_send", "photo_patterns", "ask_whois", "on_this_day", "then_and_now", "find_younger_self", "style_timeline", "family_frame", "photo_cleanup", "person_items", "taste_profile"], &[],
                "- photo_send {query}: find a REAL photo in the user's own libraries (face-matched people + semantic search over the whole archive) and SEND it to the chat — use for ANY 'show/send me a photo/pic of X', including events like 'our wedding'\n\
                 - photo_patterns {name?}: read someone's photos and learn their style/preferences (no name = recent across libraries)\n\
                 - ask_whois {}: send the next unknown-face 'who is this?' question to the chat\n\
                 - on_this_day {}: send a real photo memory from this exact day in a past year (who + where captioned)\n\
                 - then_and_now {person}: side-by-side of the same person years apart (earliest good frame vs latest) with the years labeled\n\
                 - find_younger_self {person}: hunt the unnamed clusters for a person's earlier years (babies get split by face clustering) — evidence + confirm + merge\n\
                 - style_timeline {person}: how a person's style is EVOLVING year over year from their own photos, and where it's heading\n\
                 - family_frame {}: today's wall-frame photo pick (anniversary-aware daily photo for the home tablet) — returns the caption + URL\n\
                 - photo_cleanup {}: organize the photo LIBRARY itself — classify screenshots + WhatsApp forwards across the whole archive into auto-albums (archive step available on request)\n\
                 - person_items {name}: structured OBJECT INVENTORY from their photos — every watch/bag/dress/jewelry item seen (counts + variants) and what was NEVER seen (gift gaps); use for 'does she have a…' questions\n\
                 - taste_profile {name}: preference PROBABILITIES from studying many photos — outfit/color/jewelry/setting/vibe distributions with confidence that grows per batch; use for 'what does she like' questions"),
            PluginSpec::new("photo_studio", "Photo studio", "Photos", Personal, &["growup_reel", "enhance_photo", "photo_create"], &[],
                "- growup_reel {name}: build a time-lapse FILM of a person growing up (best face per month across the whole photo archive) and send it — pure magic for family\n\
                 - enhance_photo {}: enhance the last photo the user sent (light/color/sharpen) and send it back — for photo-editing asks\n\
                 - photo_create {request}: CREATIVE studio — collages (a person across occasions/outfits, 'us' across years) and mood/vibe pictures, composed from the library with a unique grounded caption; pass the user's ask verbatim"),
            PluginSpec::new("photo_sharing", "Photo sharing", "Photos", GatedWrite, &["share_with_member"], &[],
                "- share_with_member {member, note?}: send the LAST photo I delivered to a household member (wife/kids) with a note — their reply gets relayed back"),
            PluginSpec::new("cloud_photos", "Cloud photo archive", "Photos", Personal, &["onedrive"], &[],
                "- onedrive {action}: read the family's OLDER photo years from OneDrive (pre-Immich) — status/auth/find <date-range>/onthisday. Read-only"),
            PluginSpec::new("mail_intel", "Mail intelligence", "Mail", Personal, &["inbox_analytics", "mail_rule", "mail_report", "mail_search"], &[],
                "- inbox_analytics {}: cross-account email digest over ALL connected inboxes — needs-action / from-people / money-in-motion / purchases / noise, with body-peek state verification (read-only)\n\
                 - mail_rule {rule}: permanently teach a mail categorization rule when the user corrects the digest ('amazon receipts are noise')\n\
                 - mail_report {}: DEEP mail analysis over hundreds of emails — recurring charges w/ est monthly total, bills, shopping volume, real humans, account surface, renewal radar; auto-tracks found subscriptions\n\
                 - mail_search {query}: search the FULL mailboxes of every configured account (all folders incl. archive) — bookings, receipts, confirmation numbers, senders. Results ARE the answer — never fetch links or sign-in pages from email bodies")
                .requiring(&[Requirement::Mailbox]),
            PluginSpec::new("life_ledger", "Life chapters", "Life", Personal, &["trip_ledger", "event_ledger", "family_book"], &[],
                "- trip_ledger {query?}: LIFE CHAPTERS mined from the photo archive (where+when+who) — list trips, or brief one ('kolkata', '2019'); trip collages available\n\
                 - event_ledger {query?}: heavily-photographed DAYS related to family dates and occasions (birthday parties, pujas, ceremonies) — list or look one up; unknown days get asked about\n\
                 - family_book {year?}: the family's living biography compiled from the archive — chapters per year, open questions, exportable volume"),
            PluginSpec::new("life_rhythms", "Life rhythms", "Life", Personal, &["life_horizon", "festival_calendar", "traditions"], &[],
                "- life_horizon {}: the PROJECTED life — annual patterns from the family's own rhythms (festivals, recurring visits) with next dates and evidence\n\
                 - festival_calendar {}: the Bengali Hindu festival year — per-year resolved dates (lunar calendar) + what each festival is\n\
                 - traditions {}: the family's per-festival traditions (photoshoots, feasts) — weather-dependent ones get forecast-planned day suggestions"),
            PluginSpec::new("introspection", "Self-assessment", "Self", ReadOnly, &["self_report", "nightly_dream", "self_limits"], &[],
                "- self_report {}: my weekly self-review — per-domain scoreboard of my proactive predictions vs your reactions, corrections I absorbed, what I'm changing\n\
                 - nightly_dream {}: one verified cross-domain connection from everything known about the family (or honest silence)\n\
                 - self_limits {}: my honest capabilities/limitations/frustrations analysis, grounded in my own telemetry (tool reliability, tensions, ledger traction, failure log)"),
            PluginSpec::new("bills", "Bill tracking", "Finance", Personal, &["bill_autopay"], &[],
                "- bill_autopay {name}: when the user says a bill is on autopay, mark it so reminders stop"),
            PluginSpec::new("plugin_store", "Plugin store", "System", ReadOnly, &["plugin_registry"], &[],
                "- plugin_registry {query?}: the plugin store in the substrate — search connectors (live/gated/parked/planned) or browse all"),
        ];
        // Builtin handlers — dispatchable behavior paired to the specs above. Domains leave the
        // lib.rs match tables one at a time and land here.
        let handlers: Vec<Arc<dyn CapabilityHandler>> = vec![
            Arc::new(crate::finance::FinanceCapability),
            Arc::new(crate::finance::PortfolioCapability),
            Arc::new(crate::home::HomeCapability),
            Arc::new(crate::news::NewsCapability),
            Arc::new(crate::capabilities::WebSearchCapability),
            Arc::new(crate::capabilities::WebFetchCapability),
            Arc::new(crate::capabilities::WeatherCapability),
            Arc::new(crate::capabilities::WikipediaCapability),
            Arc::new(crate::capabilities::CalculatorCapability),
            Arc::new(crate::capabilities::MarketsCapability),
            Arc::new(crate::capabilities::TranslateCapability),
            Arc::new(crate::capabilities::GithubCapability),
            Arc::new(crate::capabilities::ResearchCapability),
            Arc::new(crate::capabilities::CoderCapability),
            Arc::new(crate::capabilities::MonitorsCapability),
            Arc::new(crate::capabilities::DashboardsCapability),
        ];
        Self { plugins, handlers }
    }

    /// Overlay a JSON manifest: `{ "plugins": { "<id>": { "enabled": bool, "security": "..." } } }`.
    /// Only listed plugins are touched; unknown ids are ignored.
    pub fn apply_manifest(&mut self, json: &str) {
        let v: Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return,
        };
        let map = v.get("plugins").and_then(|p| p.as_object());
        if let Some(map) = map {
            for (id, over) in map {
                if let Some(p) = self.plugins.iter_mut().find(|p| &p.id == id) {
                    if let Some(en) = over.get("enabled").and_then(|x| x.as_bool()) {
                        p.enabled = en;
                    }
                    if let Some(sec) = over.get("security").and_then(|x| x.as_str()).and_then(SecurityLevel::parse) {
                        p.security = sec;
                    }
                }
            }
        }
    }

    /// Build a complete, human-editable manifest snapshot of every plugin's current state.
    pub fn to_manifest(&self) -> String {
        let mut map = serde_json::Map::new();
        for p in &self.plugins {
            map.insert(p.id.clone(), json!({ "enabled": p.enabled, "security": p.security.as_str() }));
        }
        let doc = json!({
            "_comment": "Toggle/secure plugins here — no code change. enabled: true/false; security: read_only|personal|gated_write. (New native behavior still needs Rust; for zero-code capabilities add an MCP server in mcp.json.)",
            "plugins": Value::Object(map),
        });
        serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into())
    }

    /// The plugin that owns a run_agent_tool name, if any.
    pub fn plugin_for_tool(&self, tool: &str) -> Option<&PluginSpec> {
        self.plugins.iter().find(|p| p.tools.iter().any(|t| t == tool))
    }

    /// The plugin (enabled or not) whose id/aliases own a `ym` command word, if any.
    pub fn plugin_for_command(&self, cmd: &str) -> Option<&PluginSpec> {
        self.plugins.iter().find(|p| p.matches(cmd))
    }

    /// The registered handler for a plugin id, if one exists.
    pub fn handler_for_id(&self, id: &str) -> Option<Arc<dyn CapabilityHandler>> {
        self.handlers.iter().find(|h| h.id() == id).cloned()
    }

    /// The handler for a run_agent_tool name — only if the owning plugin is ENABLED.
    pub fn handler_for_tool(&self, tool: &str) -> Option<Arc<dyn CapabilityHandler>> {
        let p = self.plugin_for_tool(tool)?;
        if !p.enabled {
            return None;
        }
        self.handler_for_id(&p.id)
    }

    /// Register (or replace) a capability handler — the door imported/self-authored capabilities
    /// enter by. Pair with a PluginSpec entry so enable/security governance covers the new arrival.
    pub fn register_handler(&mut self, h: Arc<dyn CapabilityHandler>) {
        self.handlers.retain(|x| x.id() != h.id());
        self.handlers.push(h);
    }

    /// Register a DYNAMIC spec (installed pack). Refuses to shadow a builtin id, and refuses tool
    /// names another plugin already owns — a pack must not silently capture builtin dispatch.
    /// Replacing a previous dynamic spec with the same id is fine (reinstall/upgrade).
    pub fn register_spec(&mut self, spec: PluginSpec) -> Result<(), String> {
        if let Some(existing) = self.plugins.iter().find(|p| p.id == spec.id) {
            if existing.provenance == Provenance::Builtin {
                return Err(format!("'{}' is a builtin plugin — a pack can't replace it", spec.id));
            }
        }
        for t in &spec.tools {
            if let Some(owner) = self.plugin_for_tool(t) {
                if owner.id != spec.id {
                    return Err(format!("tool '{t}' already belongs to plugin '{}'", owner.id));
                }
            }
        }
        self.plugins.retain(|p| p.id != spec.id);
        self.plugins.push(spec);
        Ok(())
    }

    /// Remove a DYNAMIC spec + its handler. Builtins are untouchable.
    pub fn remove_spec(&mut self, id: &str) -> Result<(), String> {
        match self.plugins.iter().find(|p| p.id == id) {
            None => Err(format!("no plugin '{id}'")),
            Some(p) if p.provenance == Provenance::Builtin => Err(format!("'{id}' is builtin — it can't be removed")),
            Some(_) => {
                self.plugins.retain(|p| p.id != id);
                self.handlers.retain(|h| h.id() != id);
                Ok(())
            }
        }
    }

    /// The spec for an id, if present.
    pub fn spec(&self, id: &str) -> Option<&PluginSpec> {
        self.plugins.iter().find(|p| p.id == id)
    }

    /// All non-builtin specs (installed packs) — for listing + persistence.
    pub fn dynamic_specs(&self) -> Vec<&PluginSpec> {
        self.plugins.iter().filter(|p| p.provenance != Provenance::Builtin).collect()
    }

    /// Every registered spec, builtin and dynamic alike. The registry is the single source of truth
    /// for what capabilities EXIST; whether one is currently usable is a separate question answered
    /// by probing its backing client (see `ConversationEngine::capability_report`).
    pub fn all_specs(&self) -> &[PluginSpec] {
        &self.plugins
    }

    /// The ids of every registered handler. Pairs with `all_specs` to check the two halves agree —
    /// a handler without a spec is behaviour the agent is never told about.
    pub fn handler_ids(&self) -> Vec<&str> {
        self.handlers.iter().map(|h| h.id()).collect()
    }

    /// Is this tool runnable? Core tools (owned by no plugin) are always on; a plugin-owned tool is
    /// on only if its plugin is enabled.
    pub fn is_tool_enabled(&self, tool: &str) -> bool {
        self.plugin_for_tool(tool).map(|p| p.enabled).unwrap_or(true)
    }

    pub fn security_for_tool(&self, tool: &str) -> Option<SecurityLevel> {
        self.plugin_for_tool(tool).map(|p| p.security)
    }

    /// The catalog lines for the ENABLED plugins (what the agent is told it can use).
    pub fn enabled_catalog(&self) -> String {
        self.plugins.iter().filter(|p| p.enabled).map(|p| p.catalog.as_str()).collect::<Vec<_>>().join("
")
    }

    /// Flip a plugin (by id or alias) on/off; returns the resolved id, or None if not found.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Option<String> {
        let p = self.plugins.iter_mut().find(|p| p.matches(name))?;
        p.enabled = enabled;
        Some(p.id.clone())
    }

    /// Render the full plugin list, grouped by category, with security badge + on/off.
    pub fn render_list(&self) -> String {
        let mut cats: Vec<&str> = self.plugins.iter().map(|p| p.category.as_str()).collect();
        cats.sort();
        cats.dedup();
        let mut out = String::from("🔌 Plugins (toggle: `ym plugin enable|disable <name>`):
");
        for cat in cats {
            out.push_str(&format!("
{cat}
"));
            for p in self.plugins.iter().filter(|p| p.category == cat) {
                let state = if p.enabled { "on " } else { "OFF" };
                let prov = match p.provenance {
                    Provenance::Builtin => String::new(),
                    other => format!(" · {}", other.as_str()),
                };
                out.push_str(&format!("  [{state}] {:<12} {}  — {}{}
", p.id, p.security.badge(), p.title, prov));
            }
        }
        out.push_str("
New capability with zero code → add an MCP server (`ym mcp list`).");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_has_core_natives_and_owns_tools() {
        let r = PluginRegistry::builtin();
        assert!(r.plugin_for_tool("weather").is_some());
        assert!(r.plugin_for_tool("analyze").map(|p| p.id == "portfolio").unwrap_or(false));
        // a core (unowned) tool is always enabled
        assert!(r.is_tool_enabled("recall"));
        assert!(r.is_tool_enabled("weather"));
    }

    #[test]
    fn a_sense_survives_the_gate_when_the_question_is_asked_in_plain_words() {
        // The scar: the mind held `quote` and still answered "I don't have a market-data tool
        // wired in this session" — then quoted `ym quote ^NSEI` back as something to run. It
        // was not missing the tool; the tool had fallen to the NAME-ONLY tail, and a bare name
        // with no description reads to the model as a capability it does not have.
        //
        // The gate scores literal word overlap, so a line written in the vocabulary of its
        // implementation ("symbols", "equities") ranks near zero against the words people
        // actually use ("Nifty", "trading", "watch this video"). Naming the tool rescued it —
        // which is exactly the phrasing a user never uses. So these lines must carry the
        // ASKING vocabulary, and this test is the thing that notices when an edit drops it.
        // Against the builtin catalog alone this passes even with the senses unpinned — which is
        // exactly why the first version of this test proved nothing. The box runs MCP servers, so
        // the real catalog is a hundred-odd lines and a sense has to out-rank all of them. The
        // decoys reproduce that pressure: they deliberately carry the query's own words, because a
        // competitor that matches nothing cannot evict anything.
        let mut src = PluginRegistry::builtin().enabled_catalog();
        for i in 0..80 {
            src.push_str(&format!(
                "\n- mcp.decoy.tool_{i} {{q}}: today's live market stock trading news for Nifty and \
                 Reliance right now — watch this video, open that site, listen to the stream"
            ));
        }
        for (asked, tool) in [
            ("what is the Nifty trading at right now", "quote"),
            ("how is this stock doing today", "quote"),
            ("can you watch this YouTube video for me", "watch"),
            ("listen to this livestream and tell me what they say", "watch"),
            ("open that website and look it up", "browse"),
        ] {
            let (detailed, _tail) = crate::tool_catalog::gate_catalog(asked, &src);
            assert!(
                detailed.lines().any(|l| crate::tool_catalog::tool_name_of_line(l) == Some(tool)),
                "asked in plain words {asked:?} — `{tool}` fell out of the detailed catalog, so the \
                 mind will report it cannot do this. Put the asking words back in its catalog line."
            );
        }
    }

    #[test]
    fn disabling_removes_from_catalog_and_gates_tools() {
        let mut r = PluginRegistry::builtin();
        assert!(r.enabled_catalog().contains("weather {place}"));
        let id = r.set_enabled("weather", false).unwrap();
        assert_eq!(id, "weather");
        assert!(!r.is_tool_enabled("weather"), "disabled tool must be gated");
        assert!(!r.enabled_catalog().contains("weather {place}"), "disabled plugin must leave the catalog");
        // toggling by alias works too
        assert_eq!(r.set_enabled("wx", true), Some("weather".into()));
        assert!(r.is_tool_enabled("weather"));
    }

    #[test]
    fn manifest_overlay_roundtrips() {
        let mut r = PluginRegistry::builtin();
        r.apply_manifest(r#"{"plugins":{"github":{"enabled":false},"home":{"security":"gated_write"}}}"#);
        assert!(!r.is_tool_enabled("github_repo_items"), "github disabled by manifest");
        assert_eq!(r.security_for_tool("home"), Some(SecurityLevel::GatedWrite), "home security overridden");
        // a full snapshot round-trips through apply_manifest
        let snap = r.to_manifest();
        let mut r2 = PluginRegistry::builtin();
        r2.apply_manifest(&snap);
        assert!(!r2.is_tool_enabled("github_repo_items"));
        assert_eq!(r2.security_for_tool("home"), Some(SecurityLevel::GatedWrite));
    }

    #[test]
    fn unknown_plugin_toggle_returns_none() {
        let mut r = PluginRegistry::builtin();
        assert_eq!(r.set_enabled("nonsense", false), None);
    }

    #[test]
    fn finance_dispatches_through_registry_and_respects_enable() {
        let mut r = PluginRegistry::builtin();
        // spec + handler pair up: commands and tools resolve to the finance capability
        assert!(r.handler_for_id("finance").is_some(), "finance handler must be registered");
        assert_eq!(r.plugin_for_command("money").map(|p| p.id.clone()), Some("finance".into()));
        assert_eq!(r.plugin_for_command("spent").map(|p| p.id.clone()), Some("finance".into()));
        assert!(r.handler_for_tool("bills").is_some(), "finance owns the bills tool");
        assert_eq!(r.plugin_for_command("money").unwrap().provenance, Provenance::Builtin);
        // disabling the plugin severs tool dispatch (commands are gated in cli_dispatch)
        r.set_enabled("finance", false);
        assert!(r.handler_for_tool("bills").is_none(), "disabled plugin must not dispatch");
        // the other ported domains resolve too
        assert!(r.handler_for_id("portfolio").is_some(), "portfolio handler must be registered");
        assert_eq!(r.plugin_for_command("stocks").map(|p| p.id.clone()), Some("portfolio".into()));
        assert!(r.handler_for_tool("add_holding").is_some(), "portfolio owns add_holding");
        assert!(r.handler_for_id("home").is_some(), "home handler must be registered");
        assert!(r.handler_for_tool("smart_home").is_some(), "home owns smart_home");
    }
}

