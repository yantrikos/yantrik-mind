//! Calendar + event prep -- add/remove events, ICS sync, upcoming spine, prep packets. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) async fn load_calendar(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("calendar_events")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn save_calendar(&self, evs: &[serde_json::Value]) {
        let mut evs: Vec<serde_json::Value> = evs.to_vec();
        evs.sort_by_key(|e| e.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(i64::MAX));
        if evs.len() > 400 {
            evs.truncate(400); // bound growth (soonest-first, far future trimmed)
        }
        let _ = self
            .memory
            .profile_set("calendar_events", &serde_json::to_string(&evs).unwrap_or_else(|_| "[]".into()))
            .await;
    }

    /// `ym calendar add Dinner with Brishti on July 4` — title before the last " on ", date after
    /// (falls back to any date found in the whole text). Stored in the substrate.
    pub async fn calendar_add(&self, text: &str) -> String {
        let text = text.trim();
        let today = local_now();
        let (mut title, when) = match text.to_lowercase().rfind(" on ") {
            Some(i) if parse_text_date_ms(&text[i + 4..], &today).is_some() => {
                (text[..i].trim().to_string(), parse_text_date_ms(&text[i + 4..], &today))
            }
            _ => (text.to_string(), parse_text_date_ms(text, &today)),
        };
        // Optional clock time ("at 6pm" / "at 18:30"): applied to the parsed date, or to today when
        // only a time is given (rolling to tomorrow if that moment already passed).
        let time = parse_time_hm(text);
        let when = match (when, time) {
            (Some(ms), Some((h, m))) => chrono::DateTime::from_timestamp_millis(ms)
                .map(|t| t.with_timezone(today.offset()).date_naive())
                .and_then(|d| d.and_hms_opt(h, m, 0))
                .and_then(|dt| dt.and_local_timezone(*today.offset()).single())
                .map(|t| t.timestamp_millis())
                .or(Some(ms)),
            (None, Some((h, m))) => {
                let mut dt = today.date_naive().and_hms_opt(h, m, 0);
                if let Some(d) = dt {
                    if d.and_local_timezone(*today.offset()).single().map(|t| t.timestamp_millis() <= today.timestamp_millis()).unwrap_or(false) {
                        dt = (today.date_naive() + chrono::Duration::days(1)).and_hms_opt(h, m, 0);
                    }
                }
                dt.and_then(|d| d.and_local_timezone(*today.offset()).single()).map(|t| t.timestamp_millis())
            }
            (ms, None) => ms,
        };
        if time.is_some() {
            if let Some(j) = title.to_lowercase().rfind(" at ") {
                if parse_time_hm(&title).is_some() {
                    title = title[..j].trim().to_string();
                }
            }
        }
        let Some(ms) = when else {
            return "I couldn't find a date or time in that — try `ym calendar add Dinner with Brishti on July 4 at 7pm`.".to_string();
        };
        if title.len() < 2 {
            return "What's the event? e.g. `ym calendar add Dentist on July 9`.".to_string();
        }
        let mut evs = self.load_calendar().await;
        evs.push(serde_json::json!({
            "id": chrono::Utc::now().timestamp_millis(),
            "title": title,
            "when_ms": ms,
            "source": "user",
        }));
        self.save_calendar(&evs).await;
        let date = chrono::DateTime::from_timestamp_millis(ms)
            .map(|t| t.with_timezone(today.offset()).format("%A, %B %-d").to_string())
            .unwrap_or_default();
        format!("📅 Added: {title} — {date}. It'll show in the morning briefing and I'll bring it up around the day.")
    }

    /// Everything time-shaped in the next `days`, unified: calendar events (yours + external),
    /// people key dates, and reminder deadlines. Returns (ms, line) sorted soonest-first.
    pub(crate) async fn upcoming_spine(&self, days: i64) -> Vec<(i64, String)> {
        let today = local_now();
        let now = today.timestamp_millis();
        let horizon = now + days * 86_400_000;
        let mut out: Vec<(i64, String)> = Vec::new();
        for e in self.load_calendar().await {
            let ms = e.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0);
            if ms >= now - 86_400_000 / 2 && ms <= horizon {
                let title = e.get("title").and_then(|x| x.as_str()).unwrap_or("?");
                let src = if e.get("source").and_then(|x| x.as_str()) == Some("ics") { " (ext)" } else { "" };
                let when = chrono::DateTime::from_timestamp_millis(ms)
                    .map(|t| t.with_timezone(today.offset()).format("%a %b %-d").to_string())
                    .unwrap_or_default();
                out.push((ms, format!("{when}: {title}{src}")));
            }
        }
        for (name, label, d, mmdd) in self.upcoming_people_dates(days).await {
            out.push((now + d * 86_400_000, format!("{mmdd}: {name}'s {label}")));
        }
        let (reminders, _) = self.split_tasks().await;
        for t in &reminders {
            if let Some(ms) = t.due_ms.map(|m| m as i64).or_else(|| parse_text_date_ms(&t.description, &today)) {
                if ms <= horizon {
                    let mut short: String = t.description.chars().take(90).collect();
                    if t.description.chars().count() > 90 {
                        short.push('…'); // a silent mid-phrase cut ("…by July 1") reads as wrong data
                    }
                    let when = chrono::DateTime::from_timestamp_millis(ms)
                        .map(|x| x.with_timezone(today.offset()).format("%a %b %-d").to_string())
                        .unwrap_or_default();
                    out.push((ms, format!("{when}: ⏰ {short}")));
                }
            }
        }
        out.sort_by_key(|(ms, _)| *ms);
        out
    }

    /// `ym calendar` — the unified 14-day view.
    pub async fn calendar_view(&self) -> String {
        let spine = self.upcoming_spine(14).await;
        if spine.is_empty() {
            return "📅 Nothing on the spine for the next 14 days. `ym calendar add <what> on <date>` — or `ym calendar connect <ics-url>` to bring your external calendar in.".to_string();
        }
        let mut out = String::from("📅 Next 14 days:");
        for (_, line) in spine.iter().take(14) {
            out.push_str(&format!("\n  • {line}"));
        }
        out
    }

    /// `ym calendar connect <ics-url>` — read-only external feed (Google Calendar's "secret iCal
    /// address" etc.). One URL, no OAuth. Refreshed periodically by the poll loop.
    pub async fn calendar_connect(&self, url: &str) -> String {
        let url = url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return "That doesn't look like a URL — paste your calendar's secret iCal address (Google Calendar → Settings → 'Secret address in iCal format').".to_string();
        }
        let _ = self.memory.profile_set("calendar_ics_url", url).await;
        let n = self.refresh_ics().await;
        format!("🔗 Connected. Pulled {n} upcoming event(s) from your external calendar — they'll show in `ym calendar` and the briefing, refreshed every few hours.")
    }

    /// Re-pull the external ICS feed (if connected): replaces all `source:"ics"` events with the
    /// fresh window. Read-only — we never write to the external calendar.
    pub async fn refresh_ics(&self) -> usize {
        let Some(url) = self.memory.profile_get("calendar_ics_url").await.ok().flatten() else {
            return 0;
        };
        let Some(web) = &self.web else { return 0 };
        let Ok(body) = web.fetch(&url).await else { return 0 };
        let today = local_now();
        let now = today.timestamp_millis();
        let fresh = parse_ics_events(&body, *today.offset(), now - 86_400_000, now + 60 * 86_400_000);
        let n = fresh.len();
        let mut evs: Vec<serde_json::Value> = self
            .load_calendar()
            .await
            .into_iter()
            .filter(|e| e.get("source").and_then(|x| x.as_str()) != Some("ics"))
            .collect();
        for (title, ms) in fresh {
            evs.push(serde_json::json!({ "id": ms, "title": title, "when_ms": ms, "source": "ics" }));
        }
        self.save_calendar(&evs).await;
        n
    }

    /// Remove calendar events whose title matches (case-insensitive substring). The tool the
    /// open-house incident proved we owed the user: "please remove that date" must WORK from chat —
    /// the agent had no removal tool, so it confabulated success while the nudge engine kept firing.
    pub async fn calendar_remove(&self, text: &str) -> String {
        let q = text.trim().to_lowercase();
        if q.len() < 3 {
            return "Remove which event? e.g. `ym calendar remove open house`".to_string();
        }
        let evs = self.load_calendar().await;
        let (gone, keep): (Vec<_>, Vec<_>) = evs.into_iter().partition(|e| {
            e.get("title").and_then(|x| x.as_str()).map(|t| t.to_lowercase().contains(&q)).unwrap_or(false)
        });
        if gone.is_empty() {
            return format!("No calendar event matches \"{}\".", text.trim());
        }
        self.save_calendar(&keep).await;
        let names: Vec<String> = gone
            .iter()
            .filter_map(|e| e.get("title").and_then(|x| x.as_str()).map(String::from))
            .collect();
        format!("🗑 Removed from the calendar: {}.", names.join(", "))
    }

    /// PRE-EVENT PREP, part 1 (cheap): calendar events starting within the lead window
    /// (YM_PREP_LEAD_MIN, default 90) that haven't been prepped — marked immediately (persisted)
    /// so each event preps exactly once, restart-safe. The poll loop composes+sends detached.
    pub async fn events_needing_prep(&self) -> Vec<(String, i64)> {
        let lead_min: i64 = std::env::var("YM_PREP_LEAD_MIN").ok().and_then(|s| s.parse().ok()).unwrap_or(90);
        let now = local_now().timestamp_millis();
        let evs = self.load_calendar().await;
        let mut prepped: Vec<String> = self
            .memory
            .profile_get("events_prepped")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut due = Vec::new();
        for e in &evs {
            let Some(ms) = e.get("when_ms").and_then(|x| x.as_i64()) else { continue };
            let Some(title) = e.get("title").and_then(|x| x.as_str()) else { continue };
            let key = format!("{title}|{ms}");
            let locally_done = self.prepped_local.lock().unwrap().contains(&key);
            if ms > now && ms - now <= lead_min * 60_000 && !prepped.contains(&key) && !locally_done {
                self.prepped_local.lock().unwrap().insert(key.clone());
                prepped.push(key);
                let _ = self.memory.record_episode("calendar-event").await;
                due.push((title.to_string(), ms));
            }
        }
        if !due.is_empty() {
            if prepped.len() > 200 {
                let cut = prepped.len() - 200;
                prepped.drain(0..cut);
            }
            let _ = self
                .memory
                .profile_set("events_prepped", &serde_json::to_string(&prepped).unwrap_or_else(|_| "[]".into()))
                .await;
        }
        due
    }

    /// PRE-EVENT PREP, part 2 (the JARVIS move): shortly before an event, pull what I know about
    /// the people/context involved — the small personal things a great assistant remembers — plus
    /// today's weather, and compose a short heads-up. The note nobody else can write, because
    /// nobody else holds the memories.
    pub async fn compose_event_prep(&self, title: &str, when_ms: i64) -> Option<String> {
        let now = local_now();
        let in_min = ((when_ms - now.timestamp_millis()) / 60_000).max(1);
        // Memories relevant to the event text (semantic recall over the typed store).
        let recalled = self
            .memory
            .recall_typed(mind_types::RecallQuery { text: title.to_string(), top_k: 6, kind: None }, &mind_types::AccessContext::Operator)
            .await
            .unwrap_or_default();
        let mut ctx = String::new();
        for r in recalled.iter().take(6) {
            ctx.push_str(&format!("- {}\n", r.item.text.chars().take(200).collect::<String>()));
        }
        // People layer: anyone named in the event title brings their living profile along.
        let tl = title.to_lowercase();
        for p in self.load_people_profiles().await {
            let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let hit = !name.is_empty() && tl.contains(&name.to_lowercase())
                || p.get("aliases").and_then(|x| x.as_array()).map(|a| {
                    a.iter().filter_map(|x| x.as_str()).any(|al| al.len() > 2 && tl.contains(&al.to_lowercase()))
                }).unwrap_or(false);
            if hit {
                let rel = p.get("relationship").and_then(|x| x.as_str()).unwrap_or("");
                let facts: Vec<&str> = p.get("facts").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|f| f.as_str()).collect()).unwrap_or_default();
                ctx.push_str(&format!("About {name} ({rel}): {}\n", facts.join("; ")));
            }
        }
        // Practicals: today's weather, for the leave-early / bring-an-umbrella note.
        if let Some(w) = &self.weather {
            let place = std::env::var("YM_WEATHER_PLACE").unwrap_or_else(|_| "Bentonville".to_string());
            if let Ok(rep) = w.report(&place).await {
                ctx.push_str(&format!("Weather now: {}\n", rep));
            }
        }
        if ctx.trim().is_empty() {
            // Nothing personal to add — a bare reminder beats an LLM inventing substance.
            return Some(format!("🎗 Heads-up: {title} in {in_min} min."));
        }
        let prompt = format!(
            "Write a SHORT prep note (2-4 sentences, warm, no preamble) for the user's upcoming event \
             \"{title}\" starting in {in_min} minutes. Use ONLY the context below: weave in at most TWO \
             genuinely relevant personal details a great assistant would remember, and one practical note \
             (weather/timing) only if actually relevant. Do not invent anything.\n\n=== CONTEXT ===\n{ctx}"
        );
        let cfg = GenerationConfig { max_tokens: 260, ..GenerationConfig::default() };
        let body = self
            .inference
            .chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg)
            .await
            .ok()?
            .text
            .trim()
            .to_string();
        Some(format!("🎗 {title} — in {in_min} min.\n\n{body}"))
    }

}
