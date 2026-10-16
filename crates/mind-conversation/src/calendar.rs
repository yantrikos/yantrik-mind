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
        evs.sort_by_key(|e| {
            e.get("when_ms")
                .and_then(|x| x.as_i64())
                .unwrap_or(i64::MAX)
        });
        if evs.len() > 400 {
            evs.truncate(400); // bound growth (soonest-first, far future trimmed)
        }
        let _ = self
            .memory
            .profile_set(
                "calendar_events",
                &serde_json::to_string(&evs).unwrap_or_else(|_| "[]".into()),
            )
            .await;
    }

    /// `ym calendar add Dinner with Brishti on July 4` — title before the last " on ", date after
    /// (falls back to any date found in the whole text). Stored in the substrate.
    pub async fn calendar_add(&self, text: &str) -> String {
        let text = text.trim();
        let today = local_now();
        let (mut title, when) = match text.to_lowercase().rfind(" on ") {
            Some(i) if parse_text_date_ms(&text[i + 4..], &today).is_some() => (
                text[..i].trim().to_string(),
                parse_text_date_ms(&text[i + 4..], &today),
            ),
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
                    if d.and_local_timezone(*today.offset())
                        .single()
                        .is_some_and(|t| t.timestamp_millis() <= today.timestamp_millis())
                    {
                        dt = (today.date_naive() + chrono::Duration::days(1)).and_hms_opt(h, m, 0);
                    }
                }
                dt.and_then(|d| d.and_local_timezone(*today.offset()).single())
                    .map(|t| t.timestamp_millis())
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
            .map(|t| {
                t.with_timezone(today.offset())
                    .format("%A, %B %-d")
                    .to_string()
            })
            .unwrap_or_default();
        format!("📅 Added: {title} — {date}. It'll show in the morning briefing and I'll bring it up around the day.")
    }

    /// Everything time-shaped in the next `days`, unified: calendar events (yours + external),
    /// people key dates, and reminder deadlines. Returns (ms, line) sorted soonest-first.
    /// Rows for the next-N-days panel: `(when_ms, label, dismissable_task_id)`.
    ///
    /// The third element is what makes a row ACTIONABLE. Only reminders carry one — a calendar
    /// event or a birthday is a fact about the world, not a commitment the mind is holding, so
    /// there is nothing to dismiss. A `None` row must render without a dismiss control rather
    /// than with one that fails.
    /// E.SPINE1: the `upcoming` tool's answer -- every item in the next `days`, then the open
    /// reminders that carry no date. No cap: a partial list reads as the whole.
    pub(crate) async fn upcoming_list(&self, days: i64) -> String {
        // E.CAL1 (541: the desktop calendar's trip reminder missed 8 of 8): the calendar the person
        // keeps on this desktop belongs to calendar-service -- read it, never copy it.
        let desk = if self.desktop_attached() { Some(self.desktop_calendar_lines(days).await) } else { None };
        let spine = self.upcoming_spine(days).await;
        let dated: std::collections::HashSet<String> = spine.iter().filter_map(|(_, _, id)| id.clone()).collect();
        let (reminders, _) = self.split_tasks().await;
        let undated: Vec<&Task> = reminders.iter().filter(|t| !dated.contains(&t.id) && t.due_ms.is_none()).collect();
        // E.REM2: "tomorrow" needs today -- 541 called today's items tomorrow's.
        let today = format!("Today is {}.", local_now().format("%a %-d %b"));
        let calendar = match &desk {
            Some(Ok(lines)) if !lines.is_empty() => format!("\nOn your calendar:{}", lines.iter().map(|l| format!("\n- {l}")).collect::<String>()),
            Some(Ok(_)) => "\nOn your calendar: nothing.".to_string(),
            // E.CAL1b (541: "Nothing is scheduled for this week", then the calendar went unread):
            // what could not be read comes first, and nothing below may be called the whole week.
            Some(Err(())) => "\nThe desktop calendar could not be read, so what is on it is unknown -- say that first. \
Only the reminders I keep are below."
                .to_string(),
            None => String::new(),
        };
        let unread = matches!(desk, Some(Err(())));
        if spine.is_empty() && undated.is_empty() {
            if unread {
                return format!("{today}{calendar}\nNo reminders of mine in the next {days} days, and none open.");
            }
            let other = if desk.is_some() { "Nothing else" } else { "Nothing" };
            return format!("{today}{calendar}\n{other} in the next {days} days, and no open reminders.");
        }
        let mut out = if unread {
            format!("{today}{calendar}\nMy reminders, next {days} days:")
        } else {
            format!("{today}{calendar}\nNext {days} days:")
        };
        if spine.is_empty() {
            out.push_str(" nothing dated.");
        }
        for (_, line, _) in &spine {
            out.push_str(&format!("\n- {line}"));
        }
        if !undated.is_empty() {
            out.push_str("\nOpen reminders with no date:");
            for t in undated {
                out.push_str(&format!("\n- {}", t.description));
            }
        }
        out
    }

    /// E.CAL1: the desktop calendar's events in the next `days`, as lines -- `Err` when it cannot be
    /// read. calendar-service starts on demand: on "no socket" it is started ONCE, then asked again.
    async fn desktop_calendar_lines(&self, days: i64) -> std::result::Result<Vec<String>, ()> {
        let today = local_now().date_naive();
        let end = today + chrono::Duration::days(days);
        let ask = serde_json::json!({
            "app": "calendar",
            "action": "list_events",
            "args": {"start_date": today.format("%Y-%m-%d").to_string(), "end_date": end.format("%Y-%m-%d").to_string()},
        });
        let mut obs = Box::pin(self.run_agent_tool(crate::desktop::ACT, &ask)).await;
        if obs.to_lowercase().contains("no socket") {
            let start = serde_json::json!({"app": "shell", "action": "start_service", "args": {"name": "calendar"}});
            let _ = Box::pin(self.run_agent_tool(crate::desktop::ACT, &start)).await;
            obs = Box::pin(self.run_agent_tool(crate::desktop::ACT, &ask)).await;
        }
        calendar_event_lines(&obs)
    }

    pub(crate) async fn upcoming_spine(&self, days: i64) -> Vec<(i64, String, Option<String>)> {
        let today = local_now();
        let now = today.timestamp_millis();
        let horizon = now + days * 86_400_000;
        let mut out: Vec<(i64, String, Option<String>)> = Vec::new();
        for e in self.load_calendar().await {
            let ms = e.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0);
            if ms >= now - 86_400_000 / 2 && ms <= horizon {
                let title = e.get("title").and_then(|x| x.as_str()).unwrap_or("?");
                let src = if e.get("source").and_then(|x| x.as_str()) == Some("ics") {
                    " (ext)"
                } else {
                    ""
                };
                let when = chrono::DateTime::from_timestamp_millis(ms)
                    .map(|t| {
                        t.with_timezone(today.offset())
                            .format("%a %b %-d")
                            .to_string()
                    })
                    .unwrap_or_default();
                out.push((ms, format!("{when}: {title}{src}"), None));
            }
        }
        for (name, label, d, mmdd) in self.upcoming_people_dates(days).await {
            out.push((
                now + d * 86_400_000,
                format!("{mmdd}: {name}'s {label}"),
                None,
            ));
        }
        let (reminders, _) = self.split_tasks().await;
        for t in &reminders {
            if let Some(ms) = t
                .due_ms
                .map(|m| m as i64)
                .or_else(|| parse_text_date_ms(&t.description, &today))
            {
                // A reminder whose date has PASSED is not upcoming. This branch had only the upper
                // bound while the calendar branch above had both, so reminders from any distance in
                // the past sat under "Next 7 days" — live on 2026-08-13 the panel showed six items
                // dated 8–17 July, in_days -36 to -28. The client compounded it by rendering every
                // non-positive in_days as "today", so month-old commitments read as due now. Same
                // half-day floor as events: something due this morning is still worth seeing tonight.
                if ms >= now - 86_400_000 / 2 && ms <= horizon {
                    let mut short: String = t.description.chars().take(90).collect();
                    if t.description.chars().count() > 90 {
                        short.push('…'); // a silent mid-phrase cut ("…by July 1") reads as wrong data
                    }
                    let when = chrono::DateTime::from_timestamp_millis(ms)
                        .map(|x| {
                            x.with_timezone(today.offset())
                                .format("%a %b %-d")
                                .to_string()
                        })
                        .unwrap_or_default();
                    out.push((ms, format!("{when}: ⏰ {short}"), Some(t.id.clone())));
                }
            }
        }
        out.sort_by_key(|(ms, _, _)| *ms);
        // ONE OCCASION, ONE ROW. The three sources overlap by design — a birthday is in the people
        // registry AND usually has a reminder attached — so the panel showed "Pranab's Mom's
        // birthday" twice, once as "08-13: …" and once as "Thu Aug 13: ⏰ …". Two rows for one fact
        // is the panel disagreeing with itself, which costs more trust than the row is worth.
        //
        // Same day + same subject = same occasion. The date prefix differs by source, so it is
        // stripped before comparing, and the remainder is matched on content words rather than
        // exactly — the two spellings of one birthday are never character-identical.
        // Every source writes "<date>: <subject>", so the subject is whatever follows the first
        // colon, minus the reminder bell.
        fn subject(line: &str) -> String {
            line.split_once(": ")
                .map_or(line, |(_, rest)| rest)
                .trim()
                .trim_start_matches('⏰')
                .trim()
                .to_string()
        }
        // Bucket by the LOCAL calendar date, not by UTC milliseconds. Dividing epoch ms by a day
        // splits the evening from the morning of the same local date: these two rows for one
        // birthday sat at 22:25 and 00:32 UTC — both local 13 August — and a UTC bucket called them
        // different days, so the duplicate survived. Same trap as `in_days` in surface.rs, made
        // again an hour after fixing it there; "which day is this" is never a division.
        let local_day = |ms: i64| -> Option<chrono::NaiveDate> {
            chrono::DateTime::from_timestamp_millis(ms)
                .map(|t| t.with_timezone(today.offset()).date_naive())
        };
        let mut deduped: Vec<(i64, String, Option<String>)> = Vec::new();
        for (ms, line, id) in out {
            let day = local_day(ms);
            let subj = subject(&line);
            if let Some(existing) = deduped
                .iter_mut()
                .find(|(m, l, _)| local_day(*m) == day && crate::task_similar(&subject(l), &subj))
            {
                // MERGE THE ID, don't just drop the duplicate. One occasion often arrives twice —
                // once as a people-date or calendar event (nothing to dismiss) and once as the
                // reminder the mind is actually carrying (dismissable). Which one sorts first is an
                // accident of milliseconds, so keeping only the first would randomly decide whether
                // the row can be dismissed at all. Whichever copy carries the id wins that field.
                if existing.2.is_none() {
                    existing.2 = id;
                }
                continue;
            }
            deduped.push((ms, line, id));
        }
        deduped
    }

    /// `ym calendar` — the unified 14-day view.
    pub async fn calendar_view(&self) -> String {
        let spine = self.upcoming_spine(14).await;
        if spine.is_empty() {
            return "📅 Nothing on the spine for the next 14 days. `ym calendar add <what> on <date>` — or `ym calendar connect <ics-url>` to bring your external calendar in.".to_string();
        }
        let mut out = String::from("📅 Next 14 days:");
        for (_, line, _) in spine.iter().take(14) {
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
        let Some(url) = self
            .memory
            .profile_get("calendar_ics_url")
            .await
            .ok()
            .flatten()
        else {
            return 0;
        };
        let Some(web) = &self.web else { return 0 };
        let Ok(body) = web.fetch(&url).await else {
            return 0;
        };
        let today = local_now();
        let now = today.timestamp_millis();
        let fresh = parse_ics_events(
            &body,
            *today.offset(),
            now - 86_400_000,
            now + 60 * 86_400_000,
        );
        let n = fresh.len();
        let mut evs: Vec<serde_json::Value> = self
            .load_calendar()
            .await
            .into_iter()
            .filter(|e| e.get("source").and_then(|x| x.as_str()) != Some("ics"))
            .collect();
        for (title, ms) in fresh {
            evs.push(
                serde_json::json!({ "id": ms, "title": title, "when_ms": ms, "source": "ics" }),
            );
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
            e.get("title")
                .and_then(|x| x.as_str())
                .is_some_and(|t| t.to_lowercase().contains(&q))
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
    /// L1d: the loop ledger's key for one event-prep opportunity — the EXACT persisted key
    /// (`title|start ms`, the one `events_prepped` stores), digested: two events starting at
    /// the same instant stay two opportunities.
    pub fn event_prep_key(title: &str, when_ms: i64) -> u64 {
        mind_observability::opportunity_key_digest(&format!("{title}|{when_ms}"))
    }

    /// L1d: the events that WOULD need prep now, read without marking anything — the ledger's
    /// opportunities (one per event, keyed by its start). `events_needing_prep` stays the act.
    pub async fn events_needing_prep_preview(&self) -> Vec<(String, i64)> {
        let lead_min: i64 = std::env::var("YM_PREP_LEAD_MIN")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(90);
        let now = local_now().timestamp_millis();
        let evs = self.load_calendar().await;
        let prepped: Vec<String> = self
            .memory
            .profile_get("events_prepped")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let mut due = Vec::new();
        let mut listed: std::collections::HashSet<String> = Default::default();
        for e in &evs {
            let Some(ms) = e.get("when_ms").and_then(|x| x.as_i64()) else {
                continue;
            };
            let Some(title) = e.get("title").and_then(|x| x.as_str()) else {
                continue;
            };
            let key = format!("{title}|{ms}");
            let locally_done = self.prepped_local.lock().unwrap().contains(&key);
            if ms > now
                && ms - now <= lead_min * 60_000
                && !prepped.contains(&key)
                && !locally_done
                && listed.insert(key)
            {
                due.push((title.to_string(), ms));
            }
        }
        due
    }

    pub async fn events_needing_prep(&self) -> Vec<(String, i64)> {
        let lead_min: i64 = std::env::var("YM_PREP_LEAD_MIN")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(90);
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
            let Some(ms) = e.get("when_ms").and_then(|x| x.as_i64()) else {
                continue;
            };
            let Some(title) = e.get("title").and_then(|x| x.as_str()) else {
                continue;
            };
            let key = format!("{title}|{ms}");
            let locally_done = self.prepped_local.lock().unwrap().contains(&key);
            if ms > now && ms - now <= lead_min * 60_000 && !prepped.contains(&key) && !locally_done
            {
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
                .profile_set(
                    "events_prepped",
                    &serde_json::to_string(&prepped).unwrap_or_else(|_| "[]".into()),
                )
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
            .recall_typed(
                mind_types::RecallQuery {
                    text: title.to_string(),
                    top_k: 6,
                    kind: None,
                },
                &mind_types::AccessContext::operator(mind_types::Purpose::serving_primary(
                    mind_types::Activity::Conversation,
                )),
            )
            .await
            .unwrap_or_default();
        let mut ctx = String::new();
        for r in recalled.iter().take(6) {
            ctx.push_str(&format!(
                "- {}\n",
                r.item.text.chars().take(200).collect::<String>()
            ));
        }
        // People layer: anyone named in the event title brings their living profile along.
        let tl = title.to_lowercase();
        for p in self.load_people_profiles().await {
            let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let hit = !name.is_empty() && tl.contains(&name.to_lowercase())
                || p.get("aliases")
                    .and_then(|x| x.as_array())
                    .is_some_and(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str())
                            .any(|al| al.len() > 2 && tl.contains(&al.to_lowercase()))
                    });
            if hit {
                let rel = p.get("relationship").and_then(|x| x.as_str()).unwrap_or("");
                let facts: Vec<&str> = p
                    .get("facts")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|f| f.as_str()).collect())
                    .unwrap_or_default();
                ctx.push_str(&format!("About {name} ({rel}): {}\n", facts.join("; ")));
            }
        }
        // Practicals: today's weather, for the leave-early / bring-an-umbrella note.
        if let Some(w) = &self.weather {
            let place =
                std::env::var("YM_WEATHER_PLACE").unwrap_or_else(|_| "Bentonville".to_string());
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
        let cfg = GenerationConfig {
            max_tokens: 260,
            ..GenerationConfig::default()
        };
        let body = self
            .inference
            .chat_grounded(
                vec![
                    ChatMessage::system(&self.persona()),
                    ChatMessage::user(&prompt),
                ],
                cfg,
            )
            .await
            .ok()?
            .text
            .trim()
            .to_string();
        Some(format!("🎗 {title} — in {in_min} min.\n\n{body}"))
    }
}

/// E.CAL1: the JSON array `calendar list_events` returns, as "Ddd D Mon HH:MM: title" lines (all-day
/// events without a time). `Err` when no array of events can be read from the result.
///
/// E.CAL1c (yantrik-os #833's `yos act` print): the summary, "accepted: …", the result as indented
/// JSON -- printed only when it is not empty -- then a `state: {…}` line whose `upcoming` array may
/// run past the asked range. So only the text before the state line is read, one JSON value at a
/// time from each `[`; with no array of titled events, an accepted act is an empty calendar.
pub(crate) fn calendar_event_lines(obs: &str) -> std::result::Result<Vec<String>, ()> {
    calendar_event_lines_on(obs, local_now().date_naive())
}

/// E.CAL2 (541: "Trip reminder -- Dallas tomorrow" at 18:00 TODAY was told as tomorrow, three times
/// of three): each day relative to `today`, and the title in quotes, so the title's own date words
/// are not read as the event's date.
pub(crate) fn calendar_event_lines_on(obs: &str, today: chrono::NaiveDate) -> std::result::Result<Vec<String>, ()> {
    let head = obs.split("state: {").next().unwrap_or(obs);
    let mut found: Option<Vec<serde_json::Value>> = None;
    let mut untitled = false;
    for (i, _) in head.match_indices('[') {
        let mut values = serde_json::Deserializer::from_str(&head[i..]).into_iter::<serde_json::Value>();
        let Some(Ok(serde_json::Value::Array(items))) = values.next() else { continue };
        if items.is_empty() {
            found.get_or_insert_with(Vec::new);
            continue;
        }
        if !items.iter().all(|e| e.is_object()) {
            continue;
        }
        if items.iter().all(|e| e.get("title").and_then(|t| t.as_str()).is_some()) {
            found = Some(items);
            break;
        }
        untitled = true;
    }
    let events = match found {
        Some(events) if !events.is_empty() || !untitled => events,
        None if !untitled && head.contains("accepted: True") => Vec::new(),
        _ => return Err(()),
    };
    let mut out = Vec::new();
    for e in events {
        let Some(title) = e.get("title").and_then(|t| t.as_str()) else { return Err(()) };
        let start = e.get("start").and_then(|t| t.as_str()).unwrap_or("");
        let all_day = e.get("is_all_day").and_then(|v| v.as_bool()).unwrap_or(false);
        let when = chrono::NaiveDateTime::parse_from_str(start, "%Y-%m-%dT%H:%M:%S")
            .map(|t| {
                let day = match (t.date() - today).num_days() {
                    0 => "today".to_string(),
                    1 => "tomorrow".to_string(),
                    _ => t.format("%a %-d %b").to_string(),
                };
                if all_day { day } else { format!("{day} {}", t.format("%H:%M")) }
            })
            .unwrap_or_else(|_| start.to_string());
        out.push(format!("{when}: \"{title}\""));
    }
    Ok(out)
}

#[cfg(test)]
mod cal1_tests {
    use super::calendar_event_lines_on;

    /// The absolute cases read from 1 Oct, so no event below is today or tomorrow.
    fn calendar_event_lines(obs: &str) -> Result<Vec<String>, ()> {
        calendar_event_lines_on(obs, chrono::NaiveDate::from_ymd_opt(2026, 10, 1).unwrap())
    }

    #[test]
    fn list_events_json_becomes_lines_and_garbage_is_an_error() {
        let ok = r#"Done -- Calendar -- [{"id":"e1","title":"Trip reminder -- Dallas tomorrow","start":"2026-10-16T18:00:00","end":"2026-10-16T18:30:00","is_all_day":false},{"id":"e2","title":"Holiday","start":"2026-10-19T00:00:00","is_all_day":true}]"#;
        assert_eq!(
            calendar_event_lines(ok).unwrap(),
            vec!["Fri 16 Oct 18:00: \"Trip reminder -- Dallas tomorrow\"".to_string(), "Mon 19 Oct: \"Holiday\"".to_string()]
        );
        assert_eq!(calendar_event_lines("[]").unwrap(), Vec::<String>::new());
        assert!(calendar_event_lines("calendar is closed (no socket)").is_err());
        assert!(calendar_event_lines("[{\"no\": \"title\"}]").is_err());

        // E.CAL1c: as `yos act calendar list_events` prints it (yantrik-os #833).
        let printed = "Done \u{2014} Calendar \u{2014} 1 event in [16 Oct \u{2013} 23 Oct]\naccepted: True, settled: True\nrevision: 7\n[\n  {\n    \"id\": \"e1\",\n    \"title\": \"Trip reminder - Dallas tomorrow\",\n    \"start\": \"2026-10-16T18:00:00\",\n    \"end\": \"2026-10-16T18:30:00\",\n    \"is_all_day\": false\n  }\n]\nstate: {\"events\":2,\"upcoming\":[{\"id\":\"e9\",\"title\":\"Dentist\",\"start\":\"2026-11-30T09:00:00\"}]}\n";
        assert_eq!(calendar_event_lines(printed).unwrap(), vec!["Fri 16 Oct 18:00: \"Trip reminder - Dallas tomorrow\"".to_string()]);
        // Nothing in range: yos prints no result, and the state's `upcoming` is not this week.
        let empty = "Done \u{2014} Calendar \u{2014} no events in range\naccepted: True, settled: True\nstate: {\"events\":1,\"upcoming\":[{\"id\":\"e9\",\"title\":\"Dentist\",\"start\":\"2026-11-30T09:00:00\"}]}\n";
        assert_eq!(calendar_event_lines(empty).unwrap(), Vec::<String>::new());
        assert!(calendar_event_lines("Done \u{2014} Calendar\naccepted: False, settled: True\n").is_err(), "a refused act is not an empty calendar");

        // E.CAL2: on 16 Oct the 18:00 event is today's, whatever its title says.
        let on_16 = chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
        let three = r#"[{"title":"Trip reminder - Dallas tomorrow","start":"2026-10-16T18:00:00","is_all_day":false},{"title":"Dentist","start":"2026-10-17T09:00:00","is_all_day":false},{"title":"Holiday","start":"2026-10-19T00:00:00","is_all_day":true}]"#;
        assert_eq!(
            calendar_event_lines_on(three, on_16).unwrap(),
            vec![
                "today 18:00: \"Trip reminder - Dallas tomorrow\"".to_string(),
                "tomorrow 09:00: \"Dentist\"".to_string(),
                "Mon 19 Oct: \"Holiday\"".to_string(),
            ]
        );
    }
}
