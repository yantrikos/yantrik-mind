//! Personal finance -- subscriptions, holdings/portfolio, bills, budgets/expenses. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) async fn load_subs(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("subscriptions")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    pub(crate) async fn save_subs(&self, subs: &[serde_json::Value]) {
        let _ = self.memory.profile_set("subscriptions", &serde_json::Value::Array(subs.to_vec()).to_string()).await;
    }

    /// The finance command router (used by `ym money`/`ym sub(s)` and the chat tool).
    pub(crate) async fn finance_cmd(&self, cmd: &str, rest: &str) -> String {
        match cmd {
            "subs" | "subscriptions" => self.subs_list().await,
            "sub" | "subscription" => {
                let mut p = rest.trim().splitn(2, char::is_whitespace);
                let action = p.next().unwrap_or("").to_lowercase();
                let arg = p.next().unwrap_or("").trim();
                match action.as_str() {
                    "add" | "+" => self.sub_add(arg).await,
                    "rm" | "remove" | "cancel" | "del" | "-" => self.sub_remove(arg).await,
                    "discover" | "scan" | "find" => self.discover_subscriptions().await,
                    "" | "list" | "ls" => self.subs_list().await,
                    _ => "Usage: ym sub add <name> <amount> [monthly|yearly|weekly] · ym sub rm <name> · ym sub discover · ym subs".to_string(),
                }
            }
            _ => self.money_overview().await, // "money" / "finance"
        }
    }

    pub(crate) async fn sub_add(&self, arg: &str) -> String {
        let toks: Vec<&str> = arg.split_whitespace().collect();
        let amt_idx = toks.iter().position(|t| strip_currency(t).replace(',', "").parse::<f64>().is_ok());
        let Some(i) = amt_idx else {
            return "Usage: ym sub add <name> <amount> [monthly|yearly|weekly]".to_string();
        };
        let name = toks[..i].join(" ");
        if name.is_empty() {
            return "Need a name — ym sub add <name> <amount> [cycle]".to_string();
        }
        let amount: f64 = strip_currency(toks[i]).replace(',', "").parse().unwrap_or(0.0);
        let currency = if toks[i].starts_with('₹') { "₹" } else if toks[i].starts_with('€') { "€" } else if toks[i].starts_with('£') { "£" } else { "$" };
        let cycle = toks.get(i + 1).map(|s| s.to_lowercase()).unwrap_or_else(|| "monthly".to_string());
        let mut subs = self.load_subs().await;
        subs.retain(|s| !s.get("name").and_then(|n| n.as_str()).map(|n| n.eq_ignore_ascii_case(&name)).unwrap_or(false));
        subs.push(serde_json::json!({ "name": name, "amount": amount, "cycle": cycle, "currency": currency }));
        self.save_subs(&subs).await;
        format!("Added {name} — {currency}{amount} {cycle} (~{currency}{:.2}/mo). Tracking {} subscription(s) now.", sub_monthly(amount, &cycle), subs.len())
    }

    pub(crate) async fn sub_remove(&self, name: &str) -> String {
        if name.is_empty() {
            return "Which one? ym sub rm <name>".to_string();
        }
        let mut subs = self.load_subs().await;
        let before = subs.len();
        subs.retain(|s| !s.get("name").and_then(|n| n.as_str()).map(|n| n.eq_ignore_ascii_case(name)).unwrap_or(false));
        if subs.len() == before {
            return format!("No subscription named '{name}'. `ym subs` to see them.");
        }
        self.save_subs(&subs).await;
        format!("Removed {name}. {} left.", subs.len())
    }

    pub(crate) async fn subs_list(&self) -> String {
        let subs = self.load_subs().await;
        if subs.is_empty() {
            return "No subscriptions tracked yet — add one: `ym sub add Netflix 15.99 monthly`".to_string();
        }
        let get_str = |s: &serde_json::Value, k: &str, d: &str| s.get(k).and_then(|x| x.as_str()).unwrap_or(d).to_string();
        let cur = get_str(&subs[0], "currency", "$");
        let mut total = 0.0;
        let mut lines = Vec::new();
        for s in &subs {
            let name = get_str(s, "name", "?");
            let amount = s.get("amount").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let cycle = get_str(s, "cycle", "monthly");
            let c = get_str(s, "currency", "$");
            let m = sub_monthly(amount, &cycle);
            total += m;
            lines.push(format!("• {name} — {c}{amount} {cycle} (~{c}{m:.2}/mo)"));
        }
        format!("{}\n— {} subscriptions, ~{cur}{total:.2}/mo (~{cur}{:.0}/yr)", lines.join("\n"), subs.len(), total * 12.0)
    }

    pub(crate) async fn money_overview(&self) -> String {
        let subs = self.load_subs().await;
        if subs.is_empty() {
            return "💸 Money: nothing tracked yet. Start with subscriptions — `ym sub add <name> <amount> [cycle]`, or `ym discover` to find them in your email.".to_string();
        }
        let total: f64 = subs
            .iter()
            .map(|s| sub_monthly(s.get("amount").and_then(|x| x.as_f64()).unwrap_or(0.0), s.get("cycle").and_then(|x| x.as_str()).unwrap_or("monthly")))
            .sum();
        let cur = subs[0].get("currency").and_then(|x| x.as_str()).unwrap_or("$");
        format!("💸 Tracking {} subscription(s), ~{cur}{total:.2}/mo (~{cur}{:.0}/yr). `ym subs` for the breakdown.", subs.len(), total * 12.0)
    }

    pub(crate) async fn load_holdings(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("holdings")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    pub(crate) async fn save_holdings(&self, h: &[serde_json::Value]) {
        let _ = self.memory.profile_set("holdings", &serde_json::Value::Array(h.to_vec()).to_string()).await;
    }

    /// `ym holding ...` router.
    pub(crate) async fn holding_cmd(&self, action: &str, arg: &str) -> String {
        match action {
            "add" | "+" | "buy" => self.holding_add(arg).await,
            "rm" | "remove" | "del" | "sell" | "-" => self.holding_remove(arg).await,
            "" | "list" | "ls" => self.portfolio_overview().await,
            _ => "Usage: ym holding add <ticker> <shares> [cost] [crypto] · ym holding rm <ticker> · ym portfolio".to_string(),
        }
    }

    /// Record a position. `<ticker> <shares> [cost-basis] [crypto|stock]`; kind auto-detected for
    /// common coins. Cost basis is optional (without it we show value but not P&L).
    pub(crate) async fn holding_add(&self, arg: &str) -> String {
        let toks: Vec<&str> = arg.split_whitespace().collect();
        if toks.len() < 2 {
            return "Usage: ym holding add <ticker> <shares> [cost-basis] [crypto]  (e.g. ym holding add AAPL 10 175.50)".to_string();
        }
        let ticker = toks[0].to_uppercase();
        let shares: f64 = toks[1].replace(',', "").parse().unwrap_or(0.0);
        if shares <= 0.0 {
            return format!("How many {ticker}? Give a positive number of shares/units.");
        }
        let mut cost: Option<f64> = None;
        let mut kind = if is_crypto_symbol(&ticker) { "crypto" } else { "stock" };
        for t in &toks[2..] {
            let tl = t.to_lowercase();
            if tl == "crypto" || tl == "coin" {
                kind = "crypto";
            } else if tl == "stock" || tl == "equity" {
                kind = "stock";
            } else if let Ok(c) = strip_currency(t).replace(',', "").parse::<f64>() {
                cost = Some(c);
            }
        }
        let mut holdings = self.load_holdings().await;
        holdings.retain(|h| !h.get("ticker").and_then(|x| x.as_str()).map(|x| x.eq_ignore_ascii_case(&ticker)).unwrap_or(false));
        holdings.push(serde_json::json!({ "ticker": ticker, "shares": shares, "cost": cost, "kind": kind }));
        self.save_holdings(&holdings).await;
        let costnote = cost.map(|c| format!(" @ ${}", money(c))).unwrap_or_default();
        format!("Added {} {ticker}{costnote} ({kind}). Tracking {} position(s) — `ym portfolio` to value them.", fmt_shares(shares), holdings.len())
    }

    pub(crate) async fn holding_remove(&self, ticker: &str) -> String {
        let ticker = ticker.trim();
        if ticker.is_empty() {
            return "Which one? ym holding rm <ticker>".to_string();
        }
        let mut holdings = self.load_holdings().await;
        let before = holdings.len();
        holdings.retain(|h| !h.get("ticker").and_then(|x| x.as_str()).map(|x| x.eq_ignore_ascii_case(ticker)).unwrap_or(false));
        if holdings.len() == before {
            return format!("No holding '{}'. `ym portfolio` to see them.", ticker.to_uppercase());
        }
        self.save_holdings(&holdings).await;
        format!("Removed {}. {} position(s) left.", ticker.to_uppercase(), holdings.len())
    }

    /// Live valuation: each position's price, value, P&L vs cost, allocation %, + a concentration
    /// flag. Factual — the moat is that it PERSISTS and reasons across sessions, not a hot tip.
    pub(crate) async fn portfolio_overview(&self) -> String {
        let holdings = self.load_holdings().await;
        if holdings.is_empty() {
            return "📊 No holdings tracked yet. Add one: `ym holding add AAPL 10 175.50` (shares + optional cost basis). Crypto too: `ym holding add BTC 0.5 crypto`.".to_string();
        }
        let markets = match &self.markets {
            Some(m) => m,
            None => return "(markets aren't configured — can't value the portfolio)".to_string(),
        };
        struct Row {
            ticker: String,
            shares: f64,
            cost: Option<f64>,
            value: Option<f64>,
            chg: f64,
        }
        let mut rows: Vec<Row> = Vec::new();
        // Sequential — small N, and gentle on the free quote APIs (no concurrent rate-limit hit).
        for h in &holdings {
            let ticker = h.get("ticker").and_then(|x| x.as_str()).unwrap_or("?").to_string();
            let shares = h.get("shares").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let cost = h.get("cost").and_then(|x| x.as_f64());
            let kind = h.get("kind").and_then(|x| x.as_str()).unwrap_or("stock");
            let q = if kind == "crypto" { markets.crypto_quote(&ticker).await } else { markets.stock_quote(&ticker).await };
            match q {
                Ok(quote) => rows.push(Row { ticker, shares, cost, value: Some(shares * quote.price), chg: quote.change_pct }),
                Err(_) => rows.push(Row { ticker, shares, cost, value: None, chg: 0.0 }),
            }
        }
        let total: f64 = rows.iter().filter_map(|r| r.value).sum();
        // P&L must compare like-for-like: only positions that HAVE a cost basis, current-value vs cost.
        // (Mixing all-positions' value against the cost-basis subset's cost gives a nonsense %.)
        let mut cost_basis_value = 0.0;
        let mut total_cost = 0.0;
        let mut priced = 0usize; // positions counted in the P&L
        for r in &rows {
            if let (Some(v), Some(c)) = (r.value, r.cost) {
                if c > 0.0 {
                    cost_basis_value += v;
                    total_cost += c * r.shares;
                    priced += 1;
                }
            }
        }
        let mut lines = Vec::new();
        for r in &rows {
            let Some(value) = r.value else {
                lines.push(format!("• {} {} — (no live quote)", fmt_shares(r.shares), r.ticker));
                continue;
            };
            let alloc = if total > 0.0 { value / total * 100.0 } else { 0.0 };
            let arrow = if r.chg >= 0.0 { "▲" } else { "▼" };
            let pl = match r.cost {
                Some(c) if c > 0.0 => {
                    let plpct = (value - c * r.shares) / (c * r.shares) * 100.0;
                    format!("  {}{:.1}% P&L", if plpct >= 0.0 { "+" } else { "" }, plpct)
                }
                _ => String::new(),
            };
            lines.push(format!("• {} {} → ${}  {arrow}{:.1}%{pl}   ({alloc:.0}%)", fmt_shares(r.shares), r.ticker, money(value), r.chg.abs()));
        }
        let mut header = format!("📊 Portfolio — ${}", money(total));
        if total_cost > 0.0 {
            let pl = cost_basis_value - total_cost;
            let plpct = pl / total_cost * 100.0;
            let arrow = if pl >= 0.0 { "▲" } else { "▼" };
            let sign = if pl >= 0.0 { "+" } else { "-" };
            // Note when the P&L only covers some positions (the rest have no cost basis recorded).
            let scope = if priced < rows.len() { format!(" on {priced} of {} positions", rows.len()) } else { String::new() };
            header.push_str(&format!("  ({arrow} {sign}${}, {sign}{:.1}%{scope})", money(pl.abs()), plpct.abs()));
        }
        // Concentration observation (factual, not advice): the biggest single position.
        let mut note = String::new();
        if total > 0.0 {
            if let Some(top) = rows.iter().filter(|r| r.value.is_some()).max_by(|a, b| {
                a.value.unwrap_or(0.0).partial_cmp(&b.value.unwrap_or(0.0)).unwrap_or(std::cmp::Ordering::Equal)
            }) {
                let alloc = top.value.unwrap_or(0.0) / total * 100.0;
                if alloc >= 40.0 {
                    note = format!("\n⚠ {} is {:.0}% of the portfolio — that's concentrated (an observation, not advice).", top.ticker, alloc);
                }
            }
        }
        format!("{header}\n{}{note}", lines.join("\n"))
    }

    /// Deep, MULTI-SOURCE ticker analysis — the honest answer to "stock tips". Gathers several
    /// INDEPENDENT sources (a live quote, a Wikipedia profile, recent news, and a web-search sweep
    /// with the top results actually read), then SYNTHESIZES a balanced briefing: what it is, recent
    /// action, what the sources collectively say (agreement/disagreement), the bull case AND the bear
    /// case, and key risks — cited, no price targets, no buy/sell, framed as analysis not advice.
    /// Cross-references the user's own portfolio (the moat). Tool output is untrusted reference data.
    pub(crate) async fn analyze_ticker(&self, raw: &str) -> String {
        let toks: Vec<&str> = raw.split_whitespace().collect();
        if toks.is_empty() {
            return "Analyze what? e.g. `ym analyze AAPL` (or `ym analyze BTC crypto`).".to_string();
        }
        let ticker = toks[0].to_uppercase();
        let kind = if toks.iter().any(|t| t.eq_ignore_ascii_case("crypto")) || is_crypto_symbol(&ticker) { "crypto" } else { "stock" };
        let markets = match &self.markets {
            Some(m) => m,
            None => return "(markets aren't configured)".to_string(),
        };
        // 1. The live quote (and the proper name to search the other sources by).
        let quote = if kind == "crypto" { markets.crypto_quote(&ticker).await } else { markets.stock_quote(&ticker).await };
        let quote = match quote {
            Ok(q) => q,
            Err(e) => return format!("Couldn't get a quote for {ticker}: {e}. Check the symbol?"),
        };
        let name = quote.name.clone();
        let qline = if kind == "crypto" { quote.render_crypto() } else { quote.render_stock() };

        // 2. Gather INDEPENDENT sources (bounded). Each is untrusted reference data.
        let wiki = match &self.wiki {
            Some(w) => w.lookup(&name).await.unwrap_or_default(),
            None => String::new(),
        };
        let news: Vec<String> = match &self.news {
            Some(n) => n
                .headlines(Some(&format!("{name} {ticker} stock")), 6)
                .await
                .unwrap_or_default()
                .iter()
                .map(|i| format!("- {} ({})", i.title, i.source))
                .collect(),
            None => vec![],
        };
        let mut web_text = String::new();
        if let Some(se) = &self.searcher {
            if let Ok(hits) = se.search(&format!("{name} {ticker} stock analysis outlook risks"), 6).await {
                for h in hits.iter().take(6) {
                    web_text.push_str(&format!("- {} — {} [{}]\n", h.title, h.snippet, h.url));
                }
                // Read the top 2 pages for substance beyond snippets.
                if let Some(web) = &self.web {
                    for h in hits.iter().take(2) {
                        if let Ok(body) = web.fetch(&h.url).await {
                            let excerpt: String = body.chars().take(1400).collect();
                            web_text.push_str(&format!("\n[excerpt from {}]\n{excerpt}\n", h.url));
                        }
                    }
                }
            }
        }

        // 3. Portfolio cross-reference — personalized but still factual (the moat).
        let holdings = self.load_holdings().await;
        let portfolio_note = holdings
            .iter()
            .find(|h| h.get("ticker").and_then(|x| x.as_str()).map(|t| t.eq_ignore_ascii_case(&ticker)).unwrap_or(false))
            .map(|h| {
                let shares = h.get("shares").and_then(|x| x.as_f64()).unwrap_or(0.0);
                format!("\n\nNOTE: the user HOLDS this — {} {} (~${} now). Work that in, including any concentration consideration.", fmt_shares(shares), ticker, money(shares * quote.price))
            })
            .unwrap_or_default();

        // 4. Synthesize across sources. Strict: no invented numbers, no buy/sell, mandatory disclaimer.
        let evidence = format!(
            "LIVE QUOTE: {qline}\n\nWIKIPEDIA PROFILE:\n{}\n\nRECENT HEADLINES:\n{}\n\nWEB SOURCES (titles, snippets, and excerpts read from the top pages):\n{}",
            if wiki.trim().is_empty() { "(none)" } else { wiki.trim() },
            if news.is_empty() { "(none)".to_string() } else { news.join("\n") },
            if web_text.trim().is_empty() { "(none)".to_string() } else { web_text.trim().to_string() },
        );
        let prompt = format!(
            "You are a careful financial ANALYST (NOT an advisor) briefing the user on {name} ({ticker}). Use ONLY the multi-source evidence below, and CONSOLIDATE across the sources — note where they agree and where they disagree, don't just relay headlines.\n\n=== EVIDENCE ===\n{evidence}{portfolio_note}\n\n=== WRITE ===\n1. What {name} is/does — one line, from the profile.\n2. Recent price action — cite the live-quote figure.\n3. What the sources collectively say (consolidated; flag any disagreement).\n4. The BULL case and the BEAR case — both, balanced.\n5. Key RISKS / what to watch.\n\nHARD RULES: Do NOT invent any number, price, ratio, or target not present in the evidence. Do NOT say buy/sell/hold and do NOT predict the price. Stay balanced (always include the bear case). Under 230 words. End with exactly this line: 'This is analysis to consider — not financial advice. You decide.'"
        );
        let cfg = GenerationConfig { max_tokens: 900, ..GenerationConfig::default() };
        match self.inference.chat_grounded(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg).await {
            Ok(r) => format!("📊 {name} ({ticker}) — {qline}\n\n{}", r.text.trim()),
            Err(e) => format!("(couldn't complete the analysis: {e})"),
        }
    }

    /// Email auto-discovery: scan the inbox (sender + subject headers), LLM-extract recurring
    /// subscriptions, auto-track the ones with a clear price, and list the rest for the user to
    /// confirm an amount. "JARVIS already knows your money" — turns manual entry into discovery.
    /// Headers-only (no bodies), so prices are often absent → those become add-prompts, not guesses.
    pub(crate) async fn discover_subscriptions(&self) -> String {
        // Prefer the dedicated personal scan-inbox (where the user's subscription receipts live); the
        // bot's own mailbox is usually empty of personal subscriptions.
        let inboxes = self.scan_inboxes();
        if inboxes.is_empty() {
            return "I don't have an inbox to scan yet. Point me at your personal email (YM_SCAN_EMAIL + an app password; _2.._6 for more accounts) and I'll find your subscriptions.".to_string();
        }
        let mut lines: Vec<String> = Vec::new();
        for (label, m) in &inboxes {
            if let Ok(msgs) = m.inbox(80).await {
                for msg in msgs {
                    lines.push(format!("- [{label}] {} | {}", msg.from, msg.subject));
                }
            }
        }
        if lines.is_empty() {
            return "No email to scan right now (none of the connected inboxes returned mail).".to_string();
        }
        let block: String = lines.join("\n");
        let prompt = format!(
            "These are recent emails (sender | subject). Identify the user's RECURRING paid subscriptions/services \
             (streaming, SaaS, gym, insurance, cloud, memberships). IGNORE one-off purchases, shipping/delivery, \
             OTP/login codes, newsletters, and promotions. For each subscription give name, amount (a number if it \
             actually appears, else null), and cycle (\"monthly\" or \"yearly\" if known, else null). Output ONLY a \
             JSON array, e.g. [{{\"name\":\"Netflix\",\"amount\":15.99,\"cycle\":\"monthly\"}}].\n\nEMAILS:\n{block}"
        );
        let cfg = GenerationConfig { max_tokens: 1500, ..GenerationConfig::default() };
        let text = match self
            .inference
            .chat(vec![ChatMessage::system("You extract recurring subscriptions from email metadata. Output only a JSON array."), ChatMessage::user(&prompt)], cfg)
            .await
        {
            Ok(r) => r.text,
            Err(e) => return format!("Couldn't analyze the email: {e}"),
        };
        let body = text.rsplit("</think>").next().unwrap_or(&text);
        let arr: Vec<serde_json::Value> = match (body.find('['), body.rfind(']')) {
            (Some(a), Some(b)) if b > a => serde_json::from_str(&body[a..=b]).unwrap_or_default(),
            _ => Vec::new(),
        };
        if arr.is_empty() {
            return "I scanned your inbox but didn't spot any clear subscriptions.".to_string();
        }
        let mut tracked = self.load_subs().await;
        let already: std::collections::HashSet<String> =
            tracked.iter().filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(|n| n.to_lowercase())).collect();
        let (mut added, mut no_amount) = (Vec::new(), Vec::new());
        let mut changed = false;
        for item in &arr {
            let name = item.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if name.len() < 2 || already.contains(&name.to_lowercase()) {
                continue;
            }
            let cycle = item.get("cycle").and_then(|x| x.as_str()).unwrap_or("monthly").to_string();
            match item.get("amount").and_then(|x| x.as_f64()) {
                Some(a) if a > 0.0 => {
                    tracked.push(serde_json::json!({ "name": name, "amount": a, "cycle": cycle, "currency": "$" }));
                    added.push(format!("{name} (${a} {cycle})"));
                    changed = true;
                }
                _ => no_amount.push(name),
            }
        }
        if changed {
            self.save_subs(&tracked).await;
        }
        let mut out = String::new();
        if !added.is_empty() {
            out.push_str(&format!("📬 Found + tracked {} subscription(s) from your mail: {}.\n", added.len(), added.join(", ")));
        }
        if !no_amount.is_empty() {
            out.push_str(&format!("I also see these but couldn't read a price — add with `ym sub add <name> <amount>`: {}.\n", no_amount.join(", ")));
        }
        if out.is_empty() {
            out = "I scanned your inbox — nothing new beyond what you already track.".to_string();
        }
        out.trim().to_string()
    }

    pub(crate) async fn load_bills(&self) -> Vec<serde_json::Value> {
        self.memory.profile_get("bills").await.ok().flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    pub(crate) async fn save_bills(&self, bills: &[serde_json::Value]) {
        let _ = self.memory.profile_set("bills", &serde_json::Value::Array(bills.to_vec()).to_string()).await;
    }

    pub(crate) async fn bill_cmd(&self, action: &str, arg: &str) -> String {
        match action {
            "add" | "+" => self.bill_add(arg).await,
            "rm" | "remove" | "del" | "-" => self.bill_remove(arg).await,
            "autopay" | "auto" => self.bill_autopay(arg).await,
            "" | "list" | "ls" => self.bills_list().await,
            _ => "Usage: ym bill add <name> <amount> <due-day> [monthly|yearly] · ym bills · ym bill rm <name>".to_string(),
        }
    }

    pub(crate) async fn bill_add(&self, arg: &str) -> String {
        let toks: Vec<&str> = arg.split_whitespace().collect();
        let amt_idx = toks.iter().position(|t| strip_currency(t).replace(',', "").parse::<f64>().is_ok());
        let Some(i) = amt_idx else {
            return "Usage: ym bill add <name> <amount> <due-day> [monthly|yearly]".to_string();
        };
        let name = toks[..i].join(" ");
        if name.is_empty() {
            return "Need a name — ym bill add <name> <amount> <due-day>".to_string();
        }
        let amount: f64 = strip_currency(toks[i]).replace(',', "").parse().unwrap_or(0.0);
        let currency = if toks[i].starts_with('₹') { "₹" } else if toks[i].starts_with('€') { "€" } else if toks[i].starts_with('£') { "£" } else { "$" };
        let (mut due_day, mut cycle) = (1u32, "monthly".to_string());
        for t in &toks[i + 1..] {
            let tl = t.trim_end_matches(|c: char| c.is_alphabetic()).to_lowercase(); // "23rd" → "23"
            if let Ok(d) = tl.parse::<u32>() {
                if (1..=31).contains(&d) {
                    due_day = d;
                }
            } else if ["monthly", "yearly", "annual", "annually", "weekly", "quarterly"].contains(&t.to_lowercase().as_str()) {
                cycle = t.to_lowercase();
            }
        }
        let mut bills = self.load_bills().await;
        bills.retain(|b| !b.get("name").and_then(|n| n.as_str()).map(|n| n.eq_ignore_ascii_case(&name)).unwrap_or(false));
        bills.push(serde_json::json!({
            "name": name, "amount": amount, "due_day": due_day, "cycle": cycle, "currency": currency,
            "src": "told", "added": local_now().format("%b %d, %Y").to_string(),
        }));
        self.save_bills(&bills).await;
        format!("Got it — {name} {currency}{amount}, due the {due_day}{} ({cycle}). I'll remind you before it's due.", ordinal(due_day))
    }

    pub(crate) async fn bill_remove(&self, name: &str) -> String {
        if name.is_empty() {
            return "Which one? ym bill rm <name>".to_string();
        }
        let mut bills = self.load_bills().await;
        let before = bills.len();
        bills.retain(|b| !b.get("name").and_then(|n| n.as_str()).map(|n| n.eq_ignore_ascii_case(name)).unwrap_or(false));
        if bills.len() == before {
            return format!("No bill named '{name}'. `ym bills` to see them.");
        }
        self.save_bills(&bills).await;
        format!("Removed {name}. {} bill(s) left.", bills.len())
    }

    pub(crate) async fn bills_list(&self) -> String {
        let bills = self.load_bills().await;
        if bills.is_empty() {
            return "No bills tracked — add one: `ym bill add electric 120 23 monthly`".to_string();
        }
        let cur = bills[0].get("currency").and_then(|x| x.as_str()).unwrap_or("$").to_string();
        let mut total = 0.0;
        let mut lines = Vec::new();
        for b in &bills {
            let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("?");
            let amount = b.get("amount").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let cycle = b.get("cycle").and_then(|x| x.as_str()).unwrap_or("monthly");
            let due_day = b.get("due_day").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
            let c = b.get("currency").and_then(|x| x.as_str()).unwrap_or("$");
            total += sub_monthly(amount, cycle);
            let d = bill_days_until(due_day);
            let due = if d == 0 { " — due TODAY".to_string() } else if d > 0 && d <= 5 { format!(" — due in {d}d") } else { String::new() };
            let ap = if b.get("autopay").and_then(|x| x.as_bool()).unwrap_or(false) { " · autopay" } else { "" };
            let ap = format!("{ap}{}", b.get("added").and_then(|x| x.as_str()).map(|d| format!(" · added {d}")).unwrap_or_default());
            let ap = ap.as_str();
            lines.push(format!("• {name} — {c}{amount}, the {due_day}{} ({cycle}){due}{ap}", ordinal(due_day)));
        }
        format!("{}\n— {} bills, ~{cur}{total:.2}/mo", lines.join("\n"), bills.len())
    }

    /// Proactive bill reminder: any bill due within ~2 days that hasn't been flagged this month.
    /// Deduped by "name:YYYY-MM" so it fires once per cycle. Pushed to the chat by the poll loop.
    pub async fn bill_watch(&self) -> Vec<String> {
        let bills = self.load_bills().await;
        if bills.is_empty() {
            return Vec::new();
        }
        let ym = current_ym();
        // PERSISTED dedup ("name:YYYY-MM") — the in-memory set reset on every restart and re-fired
        // reminders after each deploy (live bug: three pings for the same bill in one day).
        let mut reminded: Vec<String> = self
            .memory
            .profile_get("bills_reminded")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut out = Vec::new();
        let mut dirty = false;
        for b in &bills {
            if b.get("autopay").and_then(|x| x.as_bool()).unwrap_or(false) {
                continue; // autopay — the money moves itself; no ping needed
            }
            let due_day = b.get("due_day").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
            let d = bill_days_until(due_day);
            if !(0..=2).contains(&d) {
                continue;
            }
            let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("a bill").to_string();
            let key = format!("{name}:{ym}");
            if reminded.contains(&key) {
                continue;
            }
            reminded.push(key);
            dirty = true;
            let amount = b.get("amount").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let cur = b.get("currency").and_then(|x| x.as_str()).unwrap_or("$");
            let when = if d == 0 { "today".to_string() } else { format!("in {d} day(s)") };
            let prov = match (b.get("src").and_then(|x| x.as_str()), b.get("added").and_then(|x| x.as_str())) {
                (Some("told"), Some(d)) => format!(" (you added this {d})"),
                (Some(sr), Some(d)) => format!(" (from {sr}, {d})"),
                _ => String::new(),
            };
            out.push(format!("🧾 Heads up — {name} ({cur}{amount}) is due {when} (the {due_day}{}).{prov}", ordinal(due_day)));
        }
        if dirty {
            if reminded.len() > 60 {
                let cut = reminded.len() - 60;
                reminded.drain(..cut);
            }
            let _ = self
                .memory
                .profile_set("bills_reminded", &serde_json::to_string(&reminded).unwrap_or_default())
                .await;
        }
        if !out.is_empty() {
            self.ledger_sent("bills", &format!("{} bill reminder(s)", out.len())).await;
        }
        out
    }

    /// Mark a bill as autopay — reminders stop; the ledger learns the lesson.
    pub async fn bill_autopay(&self, name: &str) -> String {
        let name = name.trim();
        if name.is_empty() {
            return "Which bill? `bill autopay <name>`".to_string();
        }
        let mut bills = self.load_bills().await;
        let mut hit = false;
        for b in bills.iter_mut() {
            if b.get("name").and_then(|n| n.as_str()).map(|n| n.eq_ignore_ascii_case(name)).unwrap_or(false) {
                b["autopay"] = serde_json::json!(true);
                hit = true;
            }
        }
        if !hit {
            return format!("No bill named '{name}'. `ym bills` to see them.");
        }
        self.save_bills(&bills).await;
        self.ledger_correction("bills", name, "on autopay — stop reminding").await;
        format!("✅ {name} marked autopay — I'll stop reminding you (it'll still show in `ym bills`).")
    }

    pub(crate) async fn load_budgets(&self) -> serde_json::Map<String, serde_json::Value> {
        self.memory.profile_get("budgets").await.ok().flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    pub(crate) async fn load_expenses(&self) -> Vec<serde_json::Value> {
        self.memory.profile_get("expenses").await.ok().flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    /// Set a monthly budget for a category, or (no args) show the overview.
    pub(crate) async fn budget_set(&self, arg: &str) -> String {
        let arg = arg.trim();
        if arg.is_empty() {
            return self.budget_overview().await;
        }
        let toks: Vec<&str> = arg.split_whitespace().collect();
        let Some(i) = toks.iter().position(|t| strip_currency(t).replace(',', "").parse::<f64>().is_ok()) else {
            return "Usage: ym budget <category> <amount>  (or just `ym budget` for the overview)".to_string();
        };
        let category = toks.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, t)| *t).collect::<Vec<_>>().join(" ").to_lowercase();
        if category.is_empty() {
            return "Which category? ym budget <category> <amount>".to_string();
        }
        let amount: f64 = strip_currency(toks[i]).replace(',', "").parse().unwrap_or(0.0);
        let mut budgets = self.load_budgets().await;
        budgets.insert(category.clone(), serde_json::json!(amount));
        let _ = self.memory.profile_set("budgets", &serde_json::Value::Object(budgets).to_string()).await;
        format!("Budget set: {category} ${amount:.0}/mo. Log spend with `ym spent <amount> {category}`.")
    }

    /// Log an expense ("45 dining" or "dining 45") into the current month.
    pub(crate) async fn expense_log(&self, arg: &str) -> String {
        let toks: Vec<&str> = arg.split_whitespace().collect();
        let Some(i) = toks.iter().position(|t| strip_currency(t).replace(',', "").parse::<f64>().is_ok()) else {
            return "Usage: ym spent <amount> <category>".to_string();
        };
        let amount: f64 = strip_currency(toks[i]).replace(',', "").parse().unwrap_or(0.0);
        let category = toks.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, t)| *t).collect::<Vec<_>>().join(" ").to_lowercase();
        if category.is_empty() {
            return "What category? ym spent <amount> <category>".to_string();
        }
        let ym = current_ym();
        let mut exp = self.load_expenses().await;
        exp.push(serde_json::json!({ "amount": amount, "category": category, "ym": ym }));
        let _ = self.memory.profile_set("expenses", &serde_json::Value::Array(exp.clone()).to_string()).await;
        // show the category's status after logging
        let spent: f64 = exp.iter().filter(|e| e.get("ym").and_then(|x| x.as_str()) == Some(ym.as_str()) && e.get("category").and_then(|x| x.as_str()) == Some(category.as_str())).filter_map(|e| e.get("amount").and_then(|x| x.as_f64())).sum();
        let budgets = self.load_budgets().await;
        match budgets.get(&category).and_then(|x| x.as_f64()) {
            Some(b) => format!("Logged ${amount:.2} on {category}. This month: ${spent:.2} / ${b:.0} ({}).", if spent > b { format!("${:.0} OVER", spent - b) } else { format!("${:.0} left", b - spent) }),
            None => format!("Logged ${amount:.2} on {category}. (${spent:.2} this month; set a budget with `ym budget {category} <amount>`.)"),
        }
    }

    pub(crate) async fn budget_overview(&self) -> String {
        let budgets = self.load_budgets().await;
        let exp = self.load_expenses().await;
        let ym = current_ym();
        if budgets.is_empty() && exp.iter().all(|e| e.get("ym").and_then(|x| x.as_str()) != Some(ym.as_str())) {
            return "No budgets or spend tracked this month. Set one: `ym budget dining 400`, log: `ym spent 45 dining`.".to_string();
        }
        let mut lines = Vec::new();
        let mut cats: Vec<String> = budgets.keys().cloned().collect();
        for e in &exp {
            if let Some(c) = e.get("category").and_then(|x| x.as_str()) {
                if !cats.contains(&c.to_string()) {
                    cats.push(c.to_string());
                }
            }
        }
        for cat in &cats {
            let spent: f64 = exp.iter().filter(|e| e.get("ym").and_then(|x| x.as_str()) == Some(ym.as_str()) && e.get("category").and_then(|x| x.as_str()) == Some(cat.as_str())).filter_map(|e| e.get("amount").and_then(|x| x.as_f64())).sum();
            match budgets.get(cat).and_then(|x| x.as_f64()) {
                Some(b) => lines.push(format!("• {cat}: ${spent:.0} / ${b:.0} {}", if spent > b { format!("⚠ ${:.0} OVER", spent - b) } else { format!("(${:.0} left)", b - spent) })),
                None => lines.push(format!("• {cat}: ${spent:.0} spent (no budget set)")),
            }
        }
        format!("📊 This month:\n{}", lines.join("\n"))
    }

}
