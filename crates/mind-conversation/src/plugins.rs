//! plugins — a DECLARATIVE registry so capabilities are configured, not code-edited. Every plugin
//! (native or MCP) is an entry with a security level + an enabled flag, overlaid from a JSON manifest
//! (`plugins.json`). Toggling, securing, or listing a plugin needs ZERO code change — the agent's
//! tool catalog is generated from the ENABLED entries, so disabling one removes it everywhere.
//!
//! Honest scope: a native plugin's *behavior* is compiled Rust (you can't conjure new native logic
//! from JSON). What the manifest controls is registration/enable/security/presentation. For a
//! genuinely-new capability with no code at all, add an MCP server — which is itself a manifest.

use serde_json::{json, Value};

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
        }
    }
    fn matches(&self, name: &str) -> bool {
        let n = name.trim().to_lowercase();
        self.id == n || self.aliases.iter().any(|a| a == &n)
    }
}

/// The single source of truth for which capabilities exist, are on, and how risky they are.
pub struct PluginRegistry {
    plugins: Vec<PluginSpec>,
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
                "- search {query}: web SEARCH (find pages/answers) — use to DISCOVER URLs/facts, then web_fetch to read one"),
            PluginSpec::new("web_fetch", "Web fetch", "Web", ReadOnly, &["web_fetch"], &["web", "fetch"],
                "- web_fetch {url}: read a web page (fast — use for real, current info instead of guessing)"),
            PluginSpec::new("news", "News", "Web", ReadOnly, &["news", "track_news"], &["news", "headlines"],
                "- news {topic}: latest news headlines on a topic (or top stories) — keyless, works for geopolitics/anything\n\
                 - track_news {topic}: TRACK a topic + proactively surface fresh headlines"),
            PluginSpec::new("weather", "Weather", "Web", ReadOnly, &["weather"], &["weather", "wx"],
                "- weather {place}: current conditions + today's forecast for a city/town"),
            PluginSpec::new("wikipedia", "Wikipedia", "Web", ReadOnly, &["wikipedia", "wiki"], &["wiki", "wikipedia"],
                "- wikipedia {query}: a factual summary from Wikipedia (what/who is X)"),
            PluginSpec::new("calculator", "Calculator", "Utility", ReadOnly, &["calc", "calculate", "math"], &["calc", "calculate", "math"],
                "- calc {expression}: do arithmetic locally (e.g. 12*7+3, (1500*0.18))"),
            PluginSpec::new("translate", "Translate", "Web", ReadOnly, &["translate"], &["translate", "tr"],
                "- translate {to, text}: translate text into a language ('to' like french/hi/es; source auto-detected)"),
            PluginSpec::new("markets", "Market quotes", "Finance", ReadOnly, &["crypto", "coin", "stock", "ticker"], &["crypto", "coin", "stock", "ticker"],
                "- crypto {coin}: a cryptocurrency price + 24h change (e.g. btc, ethereum)\n\
                 - stock {symbol}: a stock quote (US ticker, e.g. AAPL)"),
            PluginSpec::new("portfolio", "Portfolio & analysis", "Finance", Personal, &["portfolio", "holdings", "my_stocks", "analyze", "analyze_stock", "stock_analysis", "add_holding", "track_holding"], &["portfolio", "holding", "holdings", "analyze"],
                "- portfolio {}: the user's investment portfolio — their holdings valued LIVE (price, P&L, allocation)\n\
                 - analyze {ticker}: a DEEP multi-source analysis of a stock/crypto (quote+profile+news+web → balanced briefing w/ risks). ANALYSIS, never a buy/sell tip\n\
                 - add_holding {ticker, shares, cost?}: record a position the user says they own"),
            PluginSpec::new("finance", "Finance (subs/bills/budget)", "Finance", Personal, &["money", "subscriptions", "finance", "discover_subscriptions", "find_subscriptions", "bills", "budget", "budget_overview"], &["money", "finance", "subs", "sub", "bills", "bill", "budget", "spent", "discover"],
                "- money {}: the user's finances overview — subscriptions + monthly total\n\
                 - bills {}: tracked recurring bills + when they're due\n\
                 - budget {}: budget vs spend this month, by category\n\
                 - discover_subscriptions {}: scan the user's EMAIL to find recurring subscriptions"),
            PluginSpec::new("home", "Smart home", "Home", Personal, &["home", "home_status", "house", "smart_home"], &["home", "house"],
                "- home {}: check the smart home (Home Assistant) — who's home, climate, what's on"),
            PluginSpec::new("github", "GitHub", "Dev", Personal, &["github_repo_items", "github_notifications"], &["github", "gh"],
                "- github_repo_items {repo}: list open issues+PRs on \"owner/name\"\n\
                 - github_notifications {}: your GitHub notifications"),
            PluginSpec::new("research", "Deep research", "Web", ReadOnly, &["research"], &["research"],
                "- research {query}: kick off a DEEP background research job (multi-source) — for big questions, delivers when done"),
            PluginSpec::new("coder", "Code sandbox", "Dev", GatedWrite, &["code"], &["code"],
                "- code {task}: kick off a background coding job (writes+runs a script in an isolated sandbox)"),
            PluginSpec::new("dashboards", "Dashboards & pages", "Utility", ReadOnly, &["make_dashboard", "publish_page"], &["dashboard"],
                "- make_dashboard {title, sections}: render + host a styled dashboard/list/comparison page, return a URL\n\
                 - publish_page {name, html}: host a raw HTML page you wrote + return a URL"),
            PluginSpec::new("monitors", "Monitors", "Utility", ReadOnly, &["set_monitor"], &["monitor"],
                "- set_monitor {source, target, url?}: watch a source (github|web|inbox) + ping on a match"),
        ];
        Self { plugins }
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
        self.plugins.iter().filter(|p| p.enabled).map(|p| p.catalog.as_str()).collect::<Vec<_>>().join("\n")
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
        let mut out = String::from("🔌 Plugins (toggle: `ym plugin enable|disable <name>`):\n");
        for cat in cats {
            out.push_str(&format!("\n{cat}\n"));
            for p in self.plugins.iter().filter(|p| p.category == cat) {
                let state = if p.enabled { "on " } else { "OFF" };
                out.push_str(&format!("  [{state}] {:<12} {}  — {}\n", p.id, p.security.badge(), p.title));
            }
        }
        out.push_str("\nNew capability with zero code → add an MCP server (`ym mcp list`).");
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
}
