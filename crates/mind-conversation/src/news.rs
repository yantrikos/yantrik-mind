//! News + market context -- headlines, briefs, tracked topics, digest scheduling. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) async fn news_cmd(&self, rest: &str) -> String {
        let rest = rest.trim();
        let mut it = rest.splitn(2, char::is_whitespace);
        let first = it.next().unwrap_or("").to_lowercase();
        match first.as_str() {
            "track" | "watch" | "follow" => self.news_track(it.next().unwrap_or("").trim()).await,
            "untrack" | "unwatch" | "unfollow" | "stop" => self.news_untrack(it.next().unwrap_or("").trim()).await,
            "tracking" | "tracked" | "topics" => self.news_tracked_list().await,
            "headlines" | "quick" | "list" => self.news_headlines({ let r = it.next().unwrap_or("").trim(); if r.is_empty() { None } else { Some(r) } }).await,
            // A bare `ym news` = quick top headlines; `ym news <topic>` = the in-depth, multi-source brief.
            _ if rest.is_empty() => self.news_headlines(None).await,
            _ => self.news_brief(rest).await,
        }
    }

    /// In-depth, MULTI-SOURCE news brief — the upgrade from a headline dump. Gathers headlines (which
    /// outlets, recency) + a web-search sweep (real article URLs + snippets) + reads the top few
    /// articles, then SYNTHESIZES: what's happening, why it matters, the key angles (consolidated
    /// across outlets, noting agreement/disagreement), and what to watch — with the real SOURCE LINKS
    /// listed at the end. Fetched content is untrusted reference data (prompt-injection surface).
    /// Live market context for a geopolitics/markets/oil/economy topic — Brent + WTI crude + the user's
    /// holdings — so a news brief can thread the situation through to its market + portfolio impact.
    /// None when the topic isn't market-relevant. (Cross-domain: news × markets × the user's world.)
    pub(crate) async fn market_context(&self, topic: &str) -> Option<String> {
        let t = topic.to_lowercase();
        const KEYS: [&str; 22] = [
            "geopolit", "war", "conflict", "oil", "crude", "energy", "econom", "market", "inflation",
            "fed", "rate", "opec", "middle east", "hormuz", "russia", "ukraine", "iran", "israel",
            "gaza", "trade war", "tariff", "sanction",
        ];
        if !KEYS.iter().any(|k| t.contains(k)) {
            return None;
        }
        let m = self.markets.as_ref()?;
        let mut parts = Vec::new();
        for (sym, name) in [("BZ=F", "Brent"), ("CL=F", "WTI")] {
            if let Ok(q) = m.stock_quote(sym).await {
                let arrow = if q.change_pct >= 0.0 { "▲" } else { "▼" };
                parts.push(format!("{name} crude ${:.2} {arrow}{:.1}%", q.price, q.change_pct.abs()));
            }
        }
        let holdings = self.load_holdings().await;
        if !holdings.is_empty() {
            let tickers: Vec<String> = holdings.iter().filter_map(|h| h.get("ticker").and_then(|x| x.as_str()).map(String::from)).collect();
            if !tickers.is_empty() {
                parts.push(format!("user's holdings: {}", tickers.join(", ")));
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }

    pub async fn news_brief(&self, topic: &str) -> String {
        let topic = topic.trim();
        if topic.len() < 2 {
            return "What's the story? e.g. `ym news AI regulation`".to_string();
        }
        // 1. Headlines — outlet names + recency (Google News indexes paywalled outlets too).
        let headlines: Vec<String> = match &self.news {
            Some(n) => n
                .headlines(Some(topic), 8)
                .await
                .unwrap_or_default()
                .iter()
                .map(|i| format!("- {} ({})", i.title, i.source))
                .collect(),
            None => vec![],
        };
        // 2. NEWS search (SearXNG news category when available) — specific recent ARTICLES with real
        // dated URLs (not topic-portal homepages), which become both the evidence and the source links.
        let hits: Vec<mind_tools::SearchHit> = match &self.searcher {
            Some(se) => se.search_news(topic, 8).await.unwrap_or_default(),
            None => vec![],
        };
        if headlines.is_empty() && hits.is_empty() {
            return format!("I couldn't find current coverage on \"{topic}\" right now.");
        }
        let snippets: String = hits.iter().take(8).map(|h| format!("- {} — {} [{}]", h.title, h.snippet, h.url)).collect::<Vec<_>>().join("\n");
        // 3. Read the top 3 articles for substance beyond snippets.
        let mut excerpts = String::new();
        if let Some(web) = &self.web {
            for h in hits.iter().take(3) {
                if let Ok(body) = web.fetch(&h.url).await {
                    let ex: String = body.chars().take(1400).collect();
                    excerpts.push_str(&format!("\n[from {}]\n{ex}\n", h.url));
                }
            }
        }
        // 3b. CROSS-DOMAIN: for geopolitics/markets/oil/economy topics, pull LIVE market data so the
        // brief connects the SITUATION to its market ripples + the user's own portfolio — the thing a
        // single-domain news app structurally can't do.
        let market = self.market_context(topic).await;
        // 4. Synthesize across sources.
        let evidence = format!(
            "HEADLINES (outlet + title):\n{}\n\nWEB RESULTS (title — snippet — url):\n{}\n\nARTICLE EXCERPTS:\n{}\n\nLIVE MARKET CONTEXT:\n{}",
            if headlines.is_empty() { "(none)".to_string() } else { headlines.join("\n") },
            if snippets.is_empty() { "(none)".to_string() } else { snippets },
            if excerpts.trim().is_empty() { "(none)".to_string() } else { excerpts.trim().to_string() },
            market.as_deref().unwrap_or("(not market-relevant)"),
        );
        let market_instr = if market.is_some() {
            "5. **Market angle** — CONNECT the situation to the LIVE market context above: how it's moving oil/markets, and (if holdings are listed) what it means for the user's portfolio. Cite the live figures."
        } else {
            ""
        };
        let prompt = format!(
            "You are a sharp, neutral news analyst briefing the user on \"{topic}\". Using ONLY the multi-source evidence below, write an IN-DEPTH brief that CONSOLIDATES across sources — do NOT just relay headlines.\n\n=== EVIDENCE ===\n{evidence}\n\n=== WRITE ===\n1. **What's happening** — the core development(s).\n2. **Why it matters** — context / background.\n3. **The angles** — how different outlets/sides frame it; note where they AGREE and where they DIFFER, attributing contested claims to a source.\n4. **What to watch** — what's next / still uncertain.\n{market_instr}\n\nRULES: factual + balanced; attribute contested claims; do NOT invent specifics, numbers, or quotes not in the evidence. Use the live market figures verbatim. Under 300 words. Do NOT list the source URLs yourself (they're appended separately)."
        );
        let cfg = GenerationConfig { max_tokens: 1000, ..GenerationConfig::default() };
        let body = match self.inference.chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg).await {
            Ok(r) => r.text.trim().to_string(),
            Err(e) => return format!("(couldn't complete the brief: {e})"),
        };
        // 5. Append the real source links (deduped clean URLs from the web search).
        let mut seen = std::collections::HashSet::new();
        let sources: Vec<String> = hits
            .iter()
            .filter(|h| !h.url.is_empty() && seen.insert(h.url.clone()))
            .take(6)
            .map(|h| format!("- {} — {}", h.title, h.url))
            .collect();
        let src_block = if sources.is_empty() { String::new() } else { format!("\n\n📎 Sources:\n{}", sources.join("\n")) };
        format!("📰 {topic} — in-depth\n\n{body}{src_block}")
    }

    pub(crate) async fn news_headlines(&self, topic: Option<&str>) -> String {
        let news = match &self.news {
            Some(n) => n,
            None => return "(news isn't configured)".to_string(),
        };
        match news.headlines(topic, 6).await {
            Ok(items) => {
                let head = match topic {
                    Some(t) => format!("📰 {t}:\n"),
                    None => "📰 Top headlines:\n".to_string(),
                };
                format!("{head}{}", render_news(&items))
            }
            Err(e) => format!("(couldn't fetch news: {e})"),
        }
    }

    pub(crate) async fn load_news_topics(&self) -> Vec<String> {
        self.memory.profile_get("news_topics").await.ok().flatten()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn save_news_topics(&self, t: &[String]) {
        let _ = self.memory.profile_set("news_topics", &serde_json::to_string(t).unwrap_or_else(|_| "[]".into())).await;
    }

    pub(crate) async fn news_track(&self, topic: &str) -> String {
        if topic.len() < 2 {
            return "What should I track? e.g. `ym news track geopolitics`".to_string();
        }
        let mut topics = self.load_news_topics().await;
        if topics.iter().any(|t| t.eq_ignore_ascii_case(topic)) {
            return format!("Already tracking \"{topic}\".");
        }
        topics.push(topic.to_string());
        self.save_news_topics(&topics).await;
        format!("Tracking \"{topic}\" — I'll surface fresh headlines as they appear. ({} topic(s) tracked)", topics.len())
    }

    pub(crate) async fn news_untrack(&self, topic: &str) -> String {
        let mut topics = self.load_news_topics().await;
        let before = topics.len();
        topics.retain(|t| !t.eq_ignore_ascii_case(topic));
        if topics.len() == before {
            return format!("Not tracking \"{topic}\".");
        }
        self.save_news_topics(&topics).await;
        format!("Stopped tracking \"{topic}\". ({} left)", topics.len())
    }

    pub(crate) async fn news_tracked_list(&self) -> String {
        let topics = self.load_news_topics().await;
        if topics.is_empty() {
            return "Not tracking any news topics yet. Add one: `ym news track <topic>` (e.g. geopolitics).".to_string();
        }
        format!("📰 Tracking: {}", topics.join(", "))
    }

    /// Proactive news watch: for each tracked topic, detect NEW headlines (deduped, primed silently so
    /// a restart doesn't replay) and return the fresh STORIES to research — `(topic, headline)`. The
    /// poll loop turns each into a full multi-source BRIEF before sending (research-then-send, not a
    /// raw headline). Capped per tick so it's quality, not spam. Sets last_news_topic for "tell me more".
    /// Which tracked topics are DUE for a proactive situation digest. State is PERSISTED (profile
    /// "news_digest_state": per-topic seen-urls + last-sent-ms) so a restart no longer re-primes and
    /// silently swallows every update — the bug that made the proactive watch never fire. Paced per
    /// topic (YM_NEWS_DIGEST_HOURS, default 6h) so it's analytical UPDATES, not a per-headline flood.
    /// The poll loop turns each due topic into a full cross-domain `news_brief` (news × live markets).
    pub async fn news_digests_due(&self) -> Vec<String> {
        let news = match &self.news {
            Some(n) => n,
            None => return Vec::new(),
        };
        let topics = self.load_news_topics().await;
        if topics.is_empty() {
            return Vec::new();
        }
        let pace_ms: u64 = std::env::var("YM_NEWS_DIGEST_HOURS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(6) * 3_600_000;
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let mut state: serde_json::Value = self
            .memory
            .profile_get("news_digest_state")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let mut due = Vec::new();
        for topic in &topics {
            let items = match news.headlines(Some(topic), 6).await {
                Ok(i) => i,
                Err(_) => continue,
            };
            let urls: Vec<String> = items.iter().map(|i| i.url.clone()).filter(|u| !u.is_empty()).collect();
            let entry = state.get(topic);
            let primed = entry.is_some();
            let mut seen: std::collections::HashSet<String> = entry
                .and_then(|e| e.get("seen"))
                .and_then(|s| s.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let last_ms = entry.and_then(|e| e.get("last_ms")).and_then(|x| x.as_u64()).unwrap_or(0);
            let fresh: Vec<String> = urls.iter().filter(|u| !seen.contains(*u)).cloned().collect();
            if !primed {
                // First time we've ever watched this topic → prime silently (don't dump old news).
                seen.extend(urls);
                state[topic] = serde_json::json!({ "seen": seen.into_iter().collect::<Vec<_>>(), "last_ms": now });
            } else if !fresh.is_empty() && now.saturating_sub(last_ms) >= pace_ms {
                // Fresh developments + the pace window has elapsed → a digest is due.
                seen.extend(fresh);
                let mut seen_vec: Vec<String> = seen.into_iter().collect();
                if seen_vec.len() > 200 {
                    let drop = seen_vec.len() - 200;
                    seen_vec.drain(0..drop); // bound growth
                }
                state[topic] = serde_json::json!({ "seen": seen_vec, "last_ms": now });
                *self.last_news_topic.lock().unwrap() = Some(topic.clone());
                due.push(topic.clone());
            }
            // else: fresh stays UNSEEN (so the next pace window still fires) or there's nothing new.
        }
        let _ = self.memory.profile_set("news_digest_state", &state.to_string()).await;
        due.truncate(2); // at most 2 topic-digests per tick
        due
    }

    /// If the user is reacting with INTEREST to a just-surfaced news ping ("tell me more", "go
    /// deeper", "what's the latest"), return that topic (consumed) so we proactively brief it.
    pub(crate) fn interest_in_recent_news(&self, text: &str) -> Option<String> {
        let l = text.trim().to_lowercase();
        const SIGNALS: [&str; 14] = [
            "tell me more", "more on that", "more on this", "more about that", "go deeper", "dig in",
            "dig deeper", "dig into", "what's the latest", "whats the latest", "look into that",
            "research that", "more details", "expand on that",
        ];
        let interested = (l.len() <= 40 && (l == "more" || l == "go on" || l == "details"))
            || SIGNALS.iter().any(|s| l.contains(s));
        if interested {
            self.last_news_topic.lock().unwrap().take()
        } else {
            None
        }
    }

}
