//! Deals + price-watch (shopping) -- profile-aware deal search, best-offer, price watches. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// `ym learn <url>` — bounded-recursive profile builder. Follows only same-person links (own domain +
    /// known identity hosts), capped by depth + page budget + dedup (logged, never silently truncated).
    pub async fn learn_profile(&self, seed: &str) -> String {
        let web = match &self.web {
            Some(w) => w.clone(),
            None => return "(web fetch isn't wired, so I can't follow links yet)".to_string(),
        };
        let seed = seed.trim();
        if seed.len() < 4 {
            return "Give me a link and I'll go learn about you. e.g. `ym learn https://pranab.co.in`".to_string();
        }
        let seed_url = if seed.starts_with("http") { seed.to_string() } else { format!("https://{seed}") };
        let seed_host = url_host(&seed_url);
        let max_pages: usize = std::env::var("YM_LEARN_MAX_PAGES").ok().and_then(|s| s.parse().ok()).unwrap_or(6);
        let max_depth: usize = std::env::var("YM_LEARN_MAX_DEPTH").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
        // Total wall-clock budget for the whole crawl. Sites that block scrapers (LinkedIn, etc.) burn the
        // full 3-tier fetch ladder; without a budget one hanging page starves the rest and nothing saves.
        let budget_ms: i64 = std::env::var("YM_LEARN_BUDGET_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(100) * 1000;
        let start_ms = chrono::Utc::now().timestamp_millis();

        let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        queue.push_back((seed_url.clone(), 0));
        seen.insert(norm_url(&seed_url));
        let mut fetched: Vec<String> = Vec::new();
        let mut skipped = 0usize;
        let mut facts: Vec<String> = Vec::new();
        let mut fact_keys: std::collections::HashSet<String> = std::collections::HashSet::new();

        while let Some((url, depth)) = queue.pop_front() {
            if fetched.len() >= max_pages || chrono::Utc::now().timestamp_millis() - start_ms > budget_ms {
                skipped += 1 + queue.len();
                break;
            }
            // Per-page fetch timeout so a single blocking site can't stall the whole crawl on the headless
            // fallback's long timeout — skip it and move on.
            let body = match tokio::time::timeout(std::time::Duration::from_secs(22), web.fetch(&url)).await {
                Ok(Ok(b)) if b.trim().len() > 40 => b,
                _ => continue,
            };
            fetched.push(url.clone());
            let excerpt: String = body.chars().take(6000).collect();
            let prompt = format!(
                "You are learning about a PERSON from their own shared link ({seed_url}). From this page, \
                 extract (1) durable FACTS about the person — third-person, standalone, specific (role, employer, \
                 education, projects, publications, skills, location, interests, achievements); and (2) up to 6 URLs \
                 worth following to learn MORE about the SAME person: their other profiles (GitHub, LinkedIn, ORCID, \
                 Twitter/X, Scholar), project/repo pages, or other sections of their own site. Give ABSOLUTE https \
                 URLs. Do NOT include news articles, ads, or unrelated third-party sites.\n\n\
                 === PAGE ({url}) ===\n{excerpt}\n\n\
                 Output ONLY JSON: {{\"facts\":[\"...\"],\"follow\":[\"https://...\"]}}"
            );
            let cfg = GenerationConfig { max_tokens: 800, ..GenerationConfig::default() };
            let v = match self.inference.chat_grounded(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg).await {
                Ok(r) => parse_json_obj(&r.text),
                Err(_) => continue,
            };
            for f in v.get("facts").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                if let Some(s) = f.as_str() {
                    let s = s.trim();
                    let key: String = s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
                    if s.len() >= 8 && key.len() >= 5 && fact_keys.insert(key) {
                        facts.push(s.to_string());
                    }
                }
            }
            if depth < max_depth {
                for l in v.get("follow").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                    if let Some(u) = l.as_str() {
                        let u = u.trim();
                        if follow_ok(u, &seed_host) && seen.insert(norm_url(u)) {
                            queue.push_back((u.to_string(), depth + 1));
                        }
                    }
                }
            }
        }

        eprintln!("[learn] crawled {} page(s), {} fact(s) from {seed_url}", fetched.len(), facts.len());
        if facts.is_empty() {
            return format!("I fetched {} page(s) from {seed_url} but couldn't extract a clear picture of you.", fetched.len());
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        // Save each fact as a revisable, timestamped belief (contradiction detection engages on updates).
        let mut saved = 0usize;
        for f in &facts {
            if self
                .memory
                .remember_as_belief(BeliefAssertion {
                    statement: f.clone(),
                    polarity: 1.0,
                    weight: 0.85,
                    source_event: Some(format!("profile:{seed_host}")),
                    provenance: "profile".into(),
                })
                .await
                .is_ok()
            {
                saved += 1;
            }
        }
        // Register the crawled sources for periodic re-check (diff-based updates later).
        let sources_json = serde_json::json!(fetched.iter().map(|u| serde_json::json!({"url": u, "last_ms": now_ms})).collect::<Vec<_>>());
        let _ = self.memory.profile_set("profile_sources", &sources_json.to_string()).await;
        let _ = self.memory.profile_set("profile_seed", &seed_url).await;
        // Synthesize a living profile + name the gaps (so the ask-loop can fill them).
        let facts_block = facts.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
        let synth_prompt = format!(
            "From these facts I gathered about the user across their own online presence, write a warm, concise \
             SECOND-PERSON summary of who they are (4–6 sentences, addressed to \"you\"), then list 2–4 specific \
             things I still DON'T know and should ask to round out my picture.\n\n=== FACTS ===\n{facts_block}\n\n\
             Output ONLY JSON: {{\"profile\":\"<second-person summary>\",\"gaps\":[\"<question>\"]}}"
        );
        let cfg = GenerationConfig { max_tokens: 700, ..GenerationConfig::default() };
        let (profile, gaps) = match self.inference.chat_grounded(vec![ChatMessage::system(&self.persona), ChatMessage::user(&synth_prompt)], cfg).await {
            Ok(r) => {
                let v = parse_json_obj(&r.text);
                let p = v.get("profile").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                let g: Vec<String> = v.get("gaps").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_string()).collect()).unwrap_or_default();
                (p, g)
            }
            Err(_) => (String::new(), Vec::new()),
        };
        // Persist the synthesized profile so it survives + can be diffed on the next periodic pass.
        if !profile.is_empty() {
            let _ = self.memory.profile_set("self_profile", &profile).await;
        }
        let src_list = fetched.iter().map(|u| format!("  • {u}")).collect::<Vec<_>>().join("\n");
        let gap_block = if gaps.is_empty() {
            String::new()
        } else {
            format!("\n\nTo round out my picture, tell me:\n{}", gaps.iter().map(|g| format!("  • {g}")).collect::<Vec<_>>().join("\n"))
        };
        let skip_note = if skipped > 0 { format!(" ({skipped} more link(s) left unfollowed — page budget)") } else { String::new() };
        let profile_line = if profile.is_empty() { String::new() } else { format!("\n\n{profile}") };
        format!(
            "🧭 I followed your link and read {} page(s) across your presence{skip_note}, and learned {saved} things about you:\n{src_list}{profile_line}{gap_block}",
            fetched.len()
        )
    }

    /// Periodic profile refresh — re-crawl the registered seed to catch what changed (new paper, repo, role).
    /// Reuses learn_profile (beliefs dedupe/reinforce; genuinely new facts get added). Returns a surface if
    /// it re-learned, else None. Paced by the caller.
    pub async fn refresh_profile(&self) -> Option<String> {
        let seed = self.memory.profile_get("profile_seed").await.ok().flatten()?;
        let out = self.learn_profile(&seed).await;
        if out.starts_with('\u{1f9ed}') { Some(out) } else { None }
    }

    /// `ym deals <what> [max$]` — find + compare deals. Trailing number = hard budget.
    pub async fn find_deals(&self, args: &str) -> String {
        let args = args.trim();
        if args.len() < 2 {
            return "What are you shopping for? e.g. `ym deals gold watch 200` (a trailing number = your max budget).".to_string();
        }
        // Budget = a trailing number (optionally $-prefixed); the rest is the query.
        let mut budget: Option<f64> = None;
        let mut raw_tokens: Vec<String> = Vec::new();
        for t in args.split_whitespace() {
            let c = t.trim_start_matches('$').replace(',', "");
            if let Ok(n) = c.parse::<f64>() {
                if n >= 5.0 {
                    budget = Some(n);
                    continue;
                }
            }
            raw_tokens.push(t.to_string());
        }
        // Resolve the gift target (by name/nickname, or by a relationship word) BEFORE building the search
        // query: a person's name IN the query pollutes it (it hits product brands, not the person — the
        // "Brishti brand kids' watch" failure). The name personalizes the PICK; only the item goes to search.
        let people = self.load_people_profiles().await;
        let ql_full = raw_tokens.join(" ").to_lowercase();
        let rel_words = ["wife", "husband", "daughter", "son", "mom", "dad", "mother", "father", "friend", "partner", "girlfriend", "boyfriend", "kid", "child"];
        let target = people.iter().find(|p| person_matches(p, &ql_full)).or_else(|| {
            people.iter().find(|p| p.get("relationship").and_then(|x| x.as_str()).map(|r| !r.is_empty() && ql_full.contains(r)).unwrap_or(false))
        });
        // Clean product query: drop the target's name/nickname, relationship words, and gift filler.
        let stop = ["for", "gift", "gifts", "to", "my", "a", "an", "the", "present", "buy", "get", "some"];
        let product: Vec<String> = raw_tokens.iter().filter(|t| {
            let tl = t.to_lowercase();
            let is_name = target.map(|p| person_matches(p, &tl)).unwrap_or(false);
            !is_name && !stop.contains(&tl.as_str()) && !rel_words.contains(&tl.as_str())
        }).cloned().collect();
        let query = if product.is_empty() { raw_tokens.join(" ") } else { product.join(" ") };
        // Personalization context from the resolved target.
        let persona_ctx = match target {
            Some(p) => {
                let nm = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
                let rel = p.get("relationship").and_then(|x| x.as_str()).unwrap_or("");
                let facts: Vec<&str> = p.get("facts").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|f| f.as_str()).collect()).unwrap_or_default();
                format!("\nThis is a gift for {nm} ({rel}). What I know about them: {}. Factor their taste in.", if facts.is_empty() { "—".to_string() } else { facts.join("; ") })
            }
            None => String::new(),
        };
        let searcher = match &self.searcher {
            Some(s) => s,
            None => return "(search isn't configured, so I can't shop yet)".to_string(),
        };
        // Gender hint from the target's relationship — so "gold watch for my wife" searches women's, not
        // a generic (men's-defaulted) listing. Only the search terms; the display query stays clean.
        let gender = target
            .and_then(|p| p.get("relationship").and_then(|x| x.as_str()))
            .map(|r| {
                let r = r.to_lowercase();
                if ["wife", "mother", "mom", "daughter", "girlfriend", "sister"].iter().any(|w| r.contains(w)) {
                    "women's"
                } else if ["husband", "father", "dad", "son", "boyfriend", "brother"].iter().any(|w| r.contains(w)) {
                    "men's"
                } else {
                    ""
                }
            })
            .unwrap_or("");
        let sq = if gender.is_empty() { query.clone() } else { format!("{gender} {query}") };
        // 1. Multi-source search — two angles (buy + deal) merged and deduped.
        let mut hits: Vec<mind_tools::SearchHit> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for q in [format!("{sq} best price buy online"), format!("{sq} deal discount review")] {
            if let Ok(rs) = searcher.search(&q, 8).await {
                for h in rs {
                    if !h.url.is_empty() && seen.insert(h.url.clone()) {
                        hits.push(h);
                    }
                }
            }
        }
        if hits.is_empty() {
            return format!("I couldn't pull up shopping results for \"{query}\" right now.");
        }
        // 2. Read a few top pages for real prices/detail (bounded: ≤3 pages, per-page 20s, ~70s total —
        //    many retailers bot-wall, so this is best-effort and we fall back to search snippets).
        let mut excerpts = String::new();
        if let Some(web) = &self.web {
            let start = chrono::Utc::now().timestamp_millis();
            let mut read = 0;
            for h in hits.iter().take(6) {
                if read >= 3 || chrono::Utc::now().timestamp_millis() - start > 70_000 {
                    break;
                }
                if let Ok(Ok(b)) = tokio::time::timeout(std::time::Duration::from_secs(20), web.fetch(&h.url)).await {
                    if b.trim().len() > 60 {
                        read += 1;
                        let ex: String = b.chars().take(2000).collect();
                        excerpts.push_str(&format!("\n[from {}]\n{ex}\n", h.url));
                    }
                }
            }
        }
        // Direct Amazon search via a HEADFUL browser — the real unlock: it returns an actual product grid
        // WITH prices, sidestepping both the category-page problem AND the bot-wall (headful defeats the
        // headless fingerprint; proven on Amazon/Target — Walmart's press-and-hold challenge still blocks,
        // so it's skipped). One retailer keeps the reply timely + resource-light; falls back silently.
        if let Some(web) = &self.web {
            let enc = sq.replace(' ', "+");
            let amz = format!("https://www.amazon.com/s?k={enc}");
            let tgt = format!("https://www.target.com/s?searchTerm={enc}");
            // Both render under headless with consistent headers (fast) — run concurrently. (Walmart is
            // omitted: its PerimeterX press-and-hold challenge blocks headless AND headful.)
            let d = std::time::Duration::from_secs(60);
            let (ra, rt) = tokio::join!(
                tokio::time::timeout(d, web.fetch_rendered(&amz)),
                tokio::time::timeout(d, web.fetch_rendered(&tgt)),
            );
            for (label, u, r) in [("Amazon", amz, ra), ("Target", tgt, rt)] {
                if let Ok(Ok(b)) = r {
                    if b.trim().len() > 200 {
                        let ex: String = b.chars().take(3500).collect();
                        excerpts.push_str(&format!("\n[from {u} — live {label} results]\n{ex}\n"));
                    }
                }
            }
        }
        let snippets: String = hits.iter().take(10).map(|h| format!("- {} — {} [{}]", h.title, h.snippet, h.url)).collect::<Vec<_>>().join("\n");
        let budget_line = budget
            .map(|b| format!("HARD BUDGET: ${b:.0}. Only recommend items at or under this; call out anything over."))
            .unwrap_or_else(|| "No explicit budget given — note prices anyway.".to_string());
        // 4. One grounded synthesis — rank REAL listings, name a best pick. No invented prices/products.
        let prompt = format!(
            "You are a sharp, honest shopping assistant finding great deals on \"{query}\".{persona_ctx}\n{budget_line}\n\n\
             Using ONLY the evidence below (search results + page excerpts), give the best 3–6 REAL options. For each: \
             product name — price in USD (ONLY if it actually appears in the evidence, else 'price not listed') — retailer — the link. \
             Compare them, then name ONE '⭐ Best pick' with a one-line why. Do NOT invent prices or products that aren't in the \
             evidence; if the evidence is thin, say so and suggest the best next search rather than fabricating. Prefer in-budget, \
             well-reviewed, good value.\n\n=== SEARCH RESULTS ===\n{snippets}\n\n=== PAGE EXCERPTS ===\n{}\n\n\
             Format: a scannable shortlist with each option on its OWN line starting with '- ' \
             (name — price — retailer — link), then the '⭐ Best pick', then a one-line \
             '💡 Price read:' saying whether the best pick's price is LOW / FAIR / HIGH versus the typical \
             range you can see in the evidence (say 'not enough data' if you can't tell).",
            if excerpts.trim().is_empty() { "(none readable — retailer bot-walls; rely on the search results)".to_string() } else { excerpts.trim().to_string() }
        );
        let cfg = GenerationConfig { max_tokens: 900, ..GenerationConfig::default() };
        let body = match self.inference.chat_grounded(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg).await {
            Ok(r) => r.text.trim().to_string(),
            Err(e) => return format!("(couldn't complete the deal search: {e})"),
        };
        let cap = budget.map(|b| format!(" · under ${b:.0}")).unwrap_or_default();
        // Split the shortlist so verified (price + link) and unverified listings are never mixed.
        format!("🛍️ Deals — {query}{cap}\n\n{}", sectioned_deals(&body))
    }

    /// Structured "single cheapest real listing" for a query — the price-comparison primitive the watch
    /// loop diffs on. Returns (name, price_usd, retailer, url), or None if no concrete price surfaced.
    pub(crate) async fn best_offer(&self, query: &str, gender: &str) -> Option<(String, f64, String, String)> {
        let searcher = self.searcher.as_ref()?;
        let sq = if gender.is_empty() { query.to_string() } else { format!("{gender} {query}") };
        let mut hits: Vec<mind_tools::SearchHit> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for q in [format!("{sq} best price buy online"), format!("{sq} price")] {
            if let Ok(rs) = searcher.search(&q, 8).await {
                for h in rs {
                    if !h.url.is_empty() && seen.insert(h.url.clone()) {
                        hits.push(h);
                    }
                }
            }
        }
        if hits.is_empty() {
            return None;
        }
        let mut excerpts = String::new();
        if let Some(web) = &self.web {
            let start = chrono::Utc::now().timestamp_millis();
            let mut read = 0;
            for h in hits.iter().take(4) {
                if read >= 2 || chrono::Utc::now().timestamp_millis() - start > 50_000 {
                    break;
                }
                if let Ok(Ok(b)) = tokio::time::timeout(std::time::Duration::from_secs(18), web.fetch(&h.url)).await {
                    if b.trim().len() > 60 {
                        read += 1;
                        let ex: String = b.chars().take(2000).collect();
                        excerpts.push_str(&format!("\n[from {}]\n{ex}\n", h.url));
                    }
                }
            }
        }
        let snippets: String = hits.iter().take(10).map(|h| format!("- {} — {} [{}]", h.title, h.snippet, h.url)).collect::<Vec<_>>().join("\n");
        let prompt = format!(
            "Find the SINGLE cheapest real, in-stock listing for \"{query}\" in the evidence below. Use ONLY a \
             price that actually appears in the evidence — never invent one. Output ONLY JSON: \
             {{\"name\":\"...\",\"price_usd\":0.0,\"retailer\":\"...\",\"url\":\"...\"}} — or {{}} if no concrete \
             priced listing is present.\n\n=== SEARCH RESULTS ===\n{snippets}\n\n=== PAGE EXCERPTS ===\n{}",
            if excerpts.trim().is_empty() { "(none readable)".to_string() } else { excerpts.trim().to_string() }
        );
        let cfg = GenerationConfig { max_tokens: 300, ..GenerationConfig::default() };
        let v = parse_json_obj(&self.inference.chat_grounded(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg).await.ok()?.text);
        let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        let price = v.get("price_usd").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let retailer = v.get("retailer").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        if name.len() >= 4 && price > 1.0 {
            Some((name, price, retailer, url))
        } else {
            None
        }
    }

    pub(crate) async fn load_watches(&self) -> Vec<serde_json::Value> {
        self.memory.profile_get("price_watches").await.ok().flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned()).unwrap_or_default()
    }

    pub(crate) async fn save_watches(&self, w: &[serde_json::Value]) {
        let _ = self.memory.profile_set("price_watches", &serde_json::Value::Array(w.to_vec()).to_string()).await;
    }

    /// `ym watch <item> [target$]` — start tracking an item's price; I baseline the best price now and
    /// ping you when it drops (or hits your target). Personalized like the finder (gender/taste).
    pub async fn watch_price(&self, args: &str) -> String {
        let args = args.trim();
        if args.len() < 2 {
            return "Watch what? e.g. `ym watch sony wh-1000xm5 300` (trailing number = your target price).".to_string();
        }
        let mut target: Option<f64> = None;
        let mut toks: Vec<String> = Vec::new();
        for t in args.split_whitespace() {
            let c = t.trim_start_matches('$').replace(',', "");
            if let Ok(n) = c.parse::<f64>() {
                if n >= 5.0 { target = Some(n); continue; }
            }
            toks.push(t.to_string());
        }
        let query = toks.join(" ");
        // Personalize gender from a named/relationship target (same as the finder).
        let people = self.load_people_profiles().await;
        let ql = query.to_lowercase();
        let gender = people.iter()
            .find(|p| person_matches(p, &ql) || p.get("relationship").and_then(|x| x.as_str()).map(|r| !r.is_empty() && ql.contains(r)).unwrap_or(false))
            .and_then(|p| p.get("relationship").and_then(|x| x.as_str()))
            .map(|r| { let r = r.to_lowercase(); if ["wife","mother","mom","daughter","girlfriend","sister"].iter().any(|w| r.contains(w)) { "women's" } else if ["husband","father","dad","son","boyfriend","brother"].iter().any(|w| r.contains(w)) { "men's" } else { "" } })
            .unwrap_or("");
        let offer = self.best_offer(&query, gender).await;
        let now = chrono::Utc::now().timestamp_millis();
        let mut watches = self.load_watches().await;
        watches.retain(|w| w.get("query").and_then(|x| x.as_str()).map(|q| q.to_lowercase()) != Some(ql.clone()));
        let (base_price, base_retailer, base_url) = match &offer {
            Some((_, p, r, u)) => (*p, r.clone(), u.clone()),
            None => (0.0, String::new(), String::new()),
        };
        watches.push(serde_json::json!({
            "query": query, "gender": gender, "target": target,
            "best_price": base_price, "best_retailer": base_retailer, "best_url": base_url,
            "added_ms": now, "last_ms": now,
        }));
        self.save_watches(&watches).await;
        match offer {
            Some((name, p, r, _)) => format!(
                "👁 Watching \"{query}\" — best right now: ${p:.2} ({name}{}).{} I'll ping you when it drops{}.",
                if r.is_empty() { String::new() } else { format!(" at {r}") },
                target.map(|t| if p <= t { format!(" That's already at/under your ${t:.0} target! 🎯") } else { format!(" It's ${:.2} above your ${t:.0} target.", p - t) }).unwrap_or_default(),
                target.map(|t| format!(" below ${t:.0}")).unwrap_or_else(|| " to a new low".to_string()),
            ),
            None => format!("👁 Watching \"{query}\" — I couldn't pin a price this moment, but I'll keep checking and ping you when I find a good one{}.", target.map(|t| format!(" under ${t:.0}")).unwrap_or_default()),
        }
    }

    /// `ym watches` — active price watches with the best price seen so far.
    pub async fn watches_view(&self) -> String {
        let watches = self.load_watches().await;
        if watches.is_empty() {
            return "No price watches yet. `ym watch <item> [target$]` and I'll track it + ping you on a drop.".to_string();
        }
        let mut lines = vec![format!("👁 Price watches ({}):", watches.len())];
        for w in &watches {
            let q = w.get("query").and_then(|x| x.as_str()).unwrap_or("?");
            let p = w.get("best_price").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let r = w.get("best_retailer").and_then(|x| x.as_str()).unwrap_or("");
            let t = w.get("target").and_then(|x| x.as_f64());
            let price_str = if p > 0.0 { format!("best ${p:.2}{}", if r.is_empty() { String::new() } else { format!(" @ {r}") }) } else { "no price yet".to_string() };
            let tgt = t.map(|t| format!(" · target ${t:.0}")).unwrap_or_default();
            lines.push(format!("• {q} — {price_str}{tgt}"));
        }
        lines.push("\n`ym unwatch <item>` to stop.".to_string());
        lines.join("\n")
    }

    /// `ym unwatch <item>` — stop tracking.
    pub async fn unwatch_price(&self, name: &str) -> String {
        let q = name.trim().to_lowercase();
        let mut watches = self.load_watches().await;
        let before = watches.len();
        watches.retain(|w| !w.get("query").and_then(|x| x.as_str()).map(|s| s.to_lowercase().contains(&q)).unwrap_or(false));
        if watches.len() == before {
            return format!("No watch matching \"{}\".", name.trim());
        }
        self.save_watches(&watches).await;
        "Stopped watching that.".to_string()
    }

    /// Periodic drop-check — re-price each watch, and surface only a GENUINE improvement (a new low, or the
    /// target hit for the first time). Updates the stored best in place (the compare-loop delta). Returns
    /// alert lines for the poll loop.
    pub async fn check_price_watches(&self) -> Vec<String> {
        let mut watches = self.load_watches().await;
        if watches.is_empty() {
            return Vec::new();
        }
        let now = chrono::Utc::now().timestamp_millis();
        let mut out = Vec::new();
        let mut changed = false;
        for w in watches.iter_mut() {
            let query = w.get("query").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if query.is_empty() {
                continue;
            }
            let gender = w.get("gender").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let prev = w.get("best_price").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let target = w.get("target").and_then(|x| x.as_f64());
            if let Some((name, price, retailer, url)) = self.best_offer(&query, &gender).await {
                w["last_ms"] = serde_json::json!(now);
                // A genuine improvement = strictly lower than the best we'd seen (or first price we've found).
                let new_low = prev <= 0.0 || price < prev - 0.01;
                let target_hit = target.map(|t| price <= t).unwrap_or(false);
                let already_hit = w.get("notified_target").and_then(|x| x.as_bool()).unwrap_or(false);
                if new_low {
                    let was = if prev > 0.0 { format!(" (was ${prev:.2})") } else { String::new() };
                    let tgt = if target_hit { " — at/under your target! 🎯".to_string() } else { String::new() };
                    out.push(format!("💰 Price drop — {query}: now ${price:.2}{was} — {name}{}{tgt}\n{url}", if retailer.is_empty() { String::new() } else { format!(" @ {retailer}") }));
                    w["best_price"] = serde_json::json!(price);
                    w["best_retailer"] = serde_json::json!(retailer);
                    w["best_url"] = serde_json::json!(url);
                    changed = true;
                } else if target_hit && !already_hit {
                    out.push(format!("🎯 Target hit — {query}: ${price:.2} (≤ your ${:.0}) — {name}\n{url}", target.unwrap_or(0.0)));
                    changed = true;
                }
                if target_hit {
                    w["notified_target"] = serde_json::json!(true);
                    changed = true;
                }
            }
        }
        if changed {
            self.save_watches(&watches).await;
        }
        out
    }

}
