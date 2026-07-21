//! The family life-story book -- chapter build/redraft, table of contents, read/export, gap questions. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) async fn load_book_chapters(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("book_chapters")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    pub(crate) async fn load_book_lore(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("book_lore")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    /// The strict grounded-drafting prompt. The model gets ONLY evidence; thin years stay thin.
    pub(crate) fn book_chapter_prompt(year: i64, ev: &serde_json::Value, lore: &[serde_json::Value]) -> String {
        let lore_block = if lore.is_empty() {
            "(none yet)".to_string()
        } else {
            lore.iter()
                .filter_map(|l| {
                    let a = l["a"].as_str()?;
                    let by = l["by"].as_str().unwrap_or("the family");
                    Some(format!("- {by} said: \"{a}\""))
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let label = if year == 0 { "the years before the photographs".to_string() } else { year.to_string() };
        format!(
            "You are writing one chapter of a family's private book. Chapter: {label}.\n\nEVIDENCE — the ONLY facts you may use (do not invent events, places, feelings, or people):\nPhotos taken: {}\nPlaces in the photos: {}\nTrips: {}\nNamed occasions: {}\nMost often in frame: {}\nIn the family's own words:\n{lore_block}\n\nWrite 130-210 words, warm and concrete, literary but honest. HARD RULES: use ONLY names that appear in the evidence above — if no names appear, use no names at all; never invent people, relatives, speakers, or scenes; a quote belongs to exactly the person marked as its teller; do not reference outside world events (news, pandemics) unless they are in the evidence; do not write imagined or hypothetical scenes — write what is present, and write honestly about what is absent. If the evidence is thin, the chapter is short. NEVER use bullet points.\nFirst line must be: TITLE: <3-6 word chapter title>\nThen a blank line, then the chapter text.",
            ev["photos"].as_u64().unwrap_or(0),
            ev["places"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "(unknown)".into()),
            ev["trips"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "(none recorded)".into()),
            ev["events"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("; ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "(none named yet)".into()),
            ev["people"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).filter(|s| !s.is_empty()).unwrap_or_else(|| "(faces not sampled)".into()),
        )
    }

    /// Compile the book: per-year evidence -> grounded chapter drafts. Detached (guard "book").
    pub async fn book_build(&self) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some(idx) = sources.iter().position(|s| s.knows_people()) else {
            return "No photo source connected.".to_string();
        };
        let guard = "book".to_string();
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return "Already compiling the book — the table of contents lands here when it's done.".to_string();
        }
        let src_name = sources[idx].name().to_string();
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let inf = self.inference.clone();
        let trips = self.load_trips().await;
        let events = self.load_events().await;
        let lore_all = self.load_book_lore().await;
        let existing = self.load_book_chapters().await;
        tokio::spawn(async move {
            let sources = mind_tools::PhotoSource::all_from_env();
            let Some(src) = sources.into_iter().find(|s| s.name() == src_name) else {
                studies.lock().unwrap().remove(&guard);
                return;
            };
            use chrono::Datelike;
            let this_year = chrono::Utc::now().year() as i64;
            let mut chapters: Vec<serde_json::Value> = existing;
            let mut drafted = 0usize;
            let mut first_year: Option<i64> = None;
            for year in 2012..=this_year {
                // --- evidence: photos, places, people ---
                let mut photos = 0u64;
                let mut place_tally: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
                let mut sample_ids: Vec<String> = Vec::new();
                for q in 0..4 {
                    let m0 = q * 3 + 1;
                    let from = format!("{year}-{m0:02}-01T00:00:00.000Z");
                    let to = if m0 + 3 > 12 {
                        format!("{}-01-01T00:00:00.000Z", year + 1)
                    } else {
                        format!("{year}-{:02}-01T00:00:00.000Z", m0 + 3)
                    };
                    let batch = src.taken_between(&from, &to, &[], 400).await;
                    for a in batch.iter().filter(|a| !mind_tools::is_screenish(a)) {
                        photos += 1;
                        let city = a.place.split(',').next().unwrap_or("").trim().to_string();
                        if city.len() > 2 {
                            *place_tally.entry(city).or_insert(0) += 1;
                        }
                        if sample_ids.len() < 2 * (q as usize + 1) {
                            sample_ids.push(a.id.clone());
                        }
                    }
                }
                if photos == 0 {
                    continue;
                }
                first_year.get_or_insert(year);
                let mut places: Vec<(String, u32)> = place_tally.into_iter().collect();
                places.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                let places: Vec<String> = places.into_iter().take(4).map(|(c, _)| c).collect();
                let mut people_tally: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
                for id in sample_ids.iter().take(8) {
                    let (names, _) = src.people_in(id).await;
                    for n in names {
                        *people_tally.entry(n).or_insert(0) += 1;
                    }
                }
                let mut people: Vec<(String, u32)> = people_tally.into_iter().collect();
                people.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                let people: Vec<String> = people.into_iter().take(5).map(|(n, _)| n).collect();
                let trips_y: Vec<String> = trips
                    .iter()
                    .filter(|t| t["start"].as_str().map(|d| d.starts_with(&year.to_string())).unwrap_or(false))
                    .filter_map(|t| {
                        Some(format!("{} ({} days)", t["dest"].as_str()?, t["days"].as_u64().unwrap_or(0)))
                    })
                    .take(8)
                    .collect();
                let mut unknown_n = 0u32;
                let events_y: Vec<String> = events
                    .iter()
                    .filter(|e| e["date"].as_str().map(|d| d.starts_with(&year.to_string())).unwrap_or(false))
                    .filter_map(|e| {
                        let label = e["label"].as_str().unwrap_or("");
                        if label.is_empty() {
                            unknown_n += 1;
                            return None;
                        }
                        Some(format!("{}: {}", e["date"].as_str().unwrap_or(""), label))
                    })
                    .take(10)
                    .collect();
                let ev = serde_json::json!({
                    "photos": photos, "places": places, "trips": trips_y, "events": events_y,
                    "people": people, "unknown_events": unknown_n,
                });
                let lore_y: Vec<serde_json::Value> =
                    lore_all.iter().filter(|l| l["year"].as_i64() == Some(year)).cloned().collect();
                // Re-draft only when the chapter is missing or marked stale.
                let cur = chapters.iter().find(|c| c["year"].as_i64() == Some(year));
                let needs = cur.map(|c| c["stale"].as_bool().unwrap_or(false)).unwrap_or(true);
                if !needs {
                    // refresh evidence on the existing chapter, keep the prose
                    if let Some(c) = chapters.iter_mut().find(|c| c["year"].as_i64() == Some(year)) {
                        c["evidence"] = ev;
                    }
                    continue;
                }
                let prompt = Self::book_chapter_prompt(year, &ev, &lore_y);
                let cfg = GenerationConfig { max_tokens: 500, ..GenerationConfig::default() };
                let Ok(resp) = inf.chat(vec![ChatMessage::user(&prompt)], cfg).await else { continue };
                let txt = resp.text.trim().to_string();
                let (title, body) = match txt.split_once('\n') {
                    Some((t, b)) => (t.trim().trim_start_matches("TITLE:").trim().to_string(), b.trim().to_string()),
                    None => (year.to_string(), txt),
                };
                chapters.retain(|c| c["year"].as_i64() != Some(year));
                chapters.push(serde_json::json!({
                    "year": year, "title": title, "text": body, "stale": false,
                    "drafted": chrono::Utc::now().timestamp_millis(), "evidence": ev,
                }));
                drafted += 1;
            }
            chapters.sort_by_key(|c| c["year"].as_i64().unwrap_or(0));
            let n = chapters.len();
            let span = format!(
                "{}-{}",
                chapters.first().and_then(|c| c["year"].as_i64()).unwrap_or(0),
                chapters.last().and_then(|c| c["year"].as_i64()).unwrap_or(0)
            );
            let _ = mem.profile_set("book_chapters", &serde_json::Value::Array(chapters).to_string()).await;
            studies.lock().unwrap().remove(&guard);
            nq.lock().unwrap().push(format!(
                "📖 The Family Book: {n} chapters ({span}), {drafted} freshly drafted. `book` for the table of contents — and I'll start asking about the years the archive can't explain."
            ));
        });
        "📖 Compiling the Family Book — reading the whole archive year by year. The table of contents lands here when it's done.".to_string()
    }

    /// Redraft one chapter from its stored evidence + all lore for that year.
    pub async fn book_redraft(&self, year: i64) -> String {
        let mut chapters = self.load_book_chapters().await;
        let ev = chapters
            .iter()
            .find(|c| c["year"].as_i64() == Some(year))
            .map(|c| c["evidence"].clone())
            .unwrap_or_else(|| serde_json::json!({"photos": 0, "places": [], "trips": [], "events": [], "people": [], "unknown_events": 0}));
        let lore_y: Vec<serde_json::Value> = self
            .load_book_lore()
            .await
            .into_iter()
            .filter(|l| l["year"].as_i64() == Some(year))
            .collect();
        let prompt = Self::book_chapter_prompt(year, &ev, &lore_y);
        let cfg = GenerationConfig { max_tokens: 500, ..GenerationConfig::default() };
        let Ok(resp) = self.inference.chat_grounded(vec![ChatMessage::user(&prompt)], cfg).await else {
            return "Couldn't redraft the chapter right now.".to_string();
        };
        let txt = resp.text.trim().to_string();
        let (title, body) = match txt.split_once('\n') {
            Some((t, b)) => (t.trim().trim_start_matches("TITLE:").trim().to_string(), b.trim().to_string()),
            None => (year.to_string(), txt),
        };
        chapters.retain(|c| c["year"].as_i64() != Some(year));
        chapters.push(serde_json::json!({
            "year": year, "title": title, "text": body, "stale": false,
            "drafted": chrono::Utc::now().timestamp_millis(), "evidence": ev,
        }));
        chapters.sort_by_key(|c| c["year"].as_i64().unwrap_or(0));
        let _ = self.memory.profile_set("book_chapters", &serde_json::Value::Array(chapters).to_string()).await;
        let ylabel = if year == 0 { "the prologue".to_string() } else { format!("chapter {year}") };
        format!("📖 Rewritten — {ylabel} now reflects everything told. `book {}` to read it.", if year == 0 { "origin".to_string() } else { year.to_string() })
    }

    pub async fn book_toc(&self) -> String {
        let chapters = self.load_book_chapters().await;
        if chapters.is_empty() {
            return "📖 The book hasn't been compiled yet — `book build` starts it.".to_string();
        }
        let lines: Vec<String> = chapters
            .iter()
            .map(|c| {
                let y = c["year"].as_i64().unwrap_or(0);
                let ylabel = if y == 0 { "Prologue".to_string() } else { y.to_string() };
                let gaps = c["evidence"]["unknown_events"].as_u64().unwrap_or(0);
                format!(
                    "{ylabel} — {}{}",
                    c["title"].as_str().unwrap_or("Untitled"),
                    if gaps > 0 { format!("  ✍️{gaps}") } else { String::new() }
                )
            })
            .collect();
        format!(
            "📖 THE FAMILY BOOK\n{}\n\n`book <year>` to read · ✍️ = days that year I still can't explain · `book export` for the full volume",
            lines.join("\n")
        )
    }

    pub async fn book_read(&self, year: i64) -> String {
        let chapters = self.load_book_chapters().await;
        let Some(c) = chapters.iter().find(|c| c["year"].as_i64() == Some(year)) else {
            return format!("No chapter for {year} yet — `book build` compiles, or the archive may be silent that year.");
        };
        let ev = &c["evidence"];
        let ylabel = if year == 0 { "Prologue".to_string() } else { year.to_string() };
        format!(
            "📖 {ylabel} — {}\n\n{}\n\n— drawn from {} photos{}{}",
            c["title"].as_str().unwrap_or(""),
            c["text"].as_str().unwrap_or(""),
            ev["photos"].as_u64().unwrap_or(0),
            ev["trips"].as_array().map(|a| if a.is_empty() { String::new() } else { format!(", {} trips", a.len()) }).unwrap_or_default(),
            ev["events"].as_array().map(|a| if a.is_empty() { String::new() } else { format!(", {} named occasions", a.len()) }).unwrap_or_default(),
        )
    }

    /// Print-ready markdown volume on disk.
    pub async fn book_export(&self) -> String {
        let chapters = self.load_book_chapters().await;
        if chapters.is_empty() {
            return "Nothing to export yet — `book build` first.".to_string();
        }
        let lore = self.load_book_lore().await;
        let mut md = String::from("# The Family Book\n\n*Compiled by yantrik-mind from the family's own archive — photographs, journeys, occasions, and words.*\n\n");
        for c in &chapters {
            let y = c["year"].as_i64().unwrap_or(0);
            let ylabel = if y == 0 { "Prologue".to_string() } else { y.to_string() };
            md.push_str(&format!(
                "## {ylabel} — {}\n\n{}\n\n",
                c["title"].as_str().unwrap_or(""),
                c["text"].as_str().unwrap_or("")
            ));
        }
        if !lore.is_empty() {
            md.push_str("---\n\n## In the family's own words\n\n");
            for l in &lore {
                if let Some(a) = l["a"].as_str() {
                    let y = l["year"].as_i64().unwrap_or(0);
                    md.push_str(&format!("**{}** — \"{a}\"\n\n", if y == 0 { "Before the photographs".to_string() } else { y.to_string() }));
                }
            }
        }
        let dir = "/var/lib/yantrik-mind/book";
        let _ = std::fs::create_dir_all(dir);
        let path = format!("{dir}/family-book.md");
        let words = md.split_whitespace().count();
        match std::fs::write(&path, &md) {
            Ok(_) => format!("📖 Exported — {} chapters, {words} words → {path}", chapters.len()),
            Err(e) => format!("Export failed: {e}"),
        }
    }

    /// The open questions the book wants to ask.
    pub async fn book_gaps(&self) -> String {
        match self.book_ask_candidates().await {
            v if v.is_empty() => "📖 No open questions right now — the chapters hold together.".to_string(),
            v => format!(
                "📖 What the book still wants to know:\n{}",
                v.iter().take(8).map(|(_, q)| format!("• {q}")).collect::<Vec<_>>().join("\n")
            ),
        }
    }

    /// Gap candidates: (slot, question), most valuable first.
    pub(crate) async fn book_ask_candidates(&self) -> Vec<(String, String)> {
        let chapters = self.load_book_chapters().await;
        if chapters.is_empty() {
            return Vec::new();
        }
        let asked: Vec<String> = self
            .memory
            .profile_get("book_asked")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let lore = self.load_book_lore().await;
        let mut out: Vec<(String, String)> = Vec::new();
        // Origin: the story before the archive begins.
        let first_year = chapters.iter().filter_map(|c| c["year"].as_i64()).filter(|y| *y > 0).min().unwrap_or(0);
        if first_year > 0
            && !asked.contains(&"origin".to_string())
            && !lore.iter().any(|l| l["year"].as_i64() == Some(0))
        {
            out.push((
                "book:origin".to_string(),
                format!("📖 For the book's prologue — the archive begins in {first_year}, but the story starts earlier. How did you and Brishti's story begin, and where was home before the photos?"),
            ));
        }
        // Thin or unexplained years, oldest first.
        let mut years: Vec<&serde_json::Value> = chapters.iter().filter(|c| c["year"].as_i64().unwrap_or(0) > 0).collect();
        years.sort_by_key(|c| c["year"].as_i64().unwrap_or(0));
        for c in years {
            let y = c["year"].as_i64().unwrap_or(0);
            if asked.contains(&y.to_string()) || lore.iter().any(|l| l["year"].as_i64() == Some(y)) {
                continue;
            }
            let photos = c["evidence"]["photos"].as_u64().unwrap_or(0);
            let unknowns = c["evidence"]["unknown_events"].as_u64().unwrap_or(0);
            if photos < 120 {
                out.push((
                    format!("book:{y}"),
                    format!("📖 For the family book — {y} is nearly silent in the archive ({photos} photos). Where was life then, and what changed that year?"),
                ));
            } else if unknowns >= 3 {
                out.push((
                    format!("book:{y}"),
                    format!("📖 For the family book — {y} is full of photographs but the occasions are unnamed. What defined that year for the family?"),
                ));
            }
        }
        out
    }

    pub async fn book_ask_due(&self) -> bool {
        if self.pending_slot().await.is_some() {
            return false;
        }
        let period_ms: i64 = std::env::var("YM_BOOKASK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(172_800) * 1000;
        let period_ms = (period_ms as f64 * self.domain_pace("book").await) as i64;
        let last: i64 = self.memory.profile_get("book_ask_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

    pub async fn book_ask_next(&self) -> Option<(String, String)> {
        self.book_ask_candidates().await.into_iter().next()
    }

    pub async fn book_ask_arm(&self, slot: &str) {
        let key = slot.trim_start_matches("book:").to_string();
        let mut asked: Vec<String> = self
            .memory
            .profile_get("book_asked")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if !asked.contains(&key) {
            asked.push(key);
        }
        let _ = self.memory.profile_set("book_asked", &serde_json::to_string(&asked).unwrap_or_default()).await;
        self.set_pending_slot(Some(slot)).await;
        let _ = self.memory.profile_set("book_ask_last", &chrono::Utc::now().timestamp_millis().to_string()).await;
        self.note_proactive_sent().await;
        self.ledger_sent("book", "asked the family about a chapter gap").await;
    }

}
