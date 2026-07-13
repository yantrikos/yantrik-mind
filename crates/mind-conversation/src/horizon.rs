//! Life patterns + horizon -- projected annual rhythms and anticipation. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// Detect recurring annual patterns. Returns (key, label, next_date, days_until, years_seen,
    /// last_note) sorted by days_until.
    pub(crate) async fn life_patterns(&self) -> Vec<(String, String, chrono::NaiveDate, i64, usize, String)> {
        use chrono::Datelike;
        let today = local_now().date_naive();
        let mut buckets: std::collections::HashMap<String, Vec<(i32, u32, u64, String)>> = std::collections::HashMap::new();
        const LEXICON: [&str; 18] = [
            "puja", "durga", "holi", "holika", "diwali", "christmas", "thanksgiving", "halloween",
            "eid", "rakhi", "navratri", "saraswati", "lights", "birthday", "anniversary", "wedding",
            "housewarming", "graduation",
        ];
        // Events: normalized label keyword (or first two significant words) is the pattern key.
        for e in self.load_events().await {
            let label = e["label"].as_str().unwrap_or("");
            let date = e["date"].as_str().unwrap_or("");
            if label.is_empty() || date.len() != 10 {
                continue;
            }
            let low = label.to_lowercase();
            let key = LEXICON
                .iter()
                .find(|w| low.contains(*w))
                .map(|w| w.to_string())
                .unwrap_or_else(|| {
                    low.split_whitespace()
                        .filter(|w| w.len() > 3)
                        .take(2)
                        .collect::<Vec<_>>()
                        .join(" ")
                });
            if key.len() < 4 {
                continue;
            }
            let (Ok(y), Ok(d)) = (date[..4].parse::<i32>(), chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")) else {
                continue;
            };
            let note = format!(
                "{label}{}",
                e["trip"].as_str().map(|t| format!(" (traveled: {t})")).unwrap_or_default()
            );
            buckets.entry(key).or_default().push((y, d.ordinal(), e["photos"].as_u64().unwrap_or(0), note));
        }
        // Trips: a destination visited in multiple years around the same season.
        for t in self.load_trips().await {
            let (Some(dest), Some(start)) = (t["dest"].as_str(), t["start"].as_str()) else { continue };
            if start.len() != 10 {
                continue;
            }
            let city = dest.split(',').next().unwrap_or(dest).trim().to_string();
            let (Ok(y), Ok(d)) = (start[..4].parse::<i32>(), chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")) else {
                continue;
            };
            let note = format!("trip to {city} ({} days, {} photos)", t["days"], t["photos"]);
            buckets
                .entry(format!("visit {city}"))
                .or_default()
                .push((y, d.ordinal(), t["photos"].as_u64().unwrap_or(0), note));
        }
        let mut out = Vec::new();
        // Calendar festivals are AUTHORITATIVE for their own dates (lunar — they move every
        // year); the observed ledger enriches them with the family's own history. Buckets that
        // match a calendared festival are consumed here so they don't double-project.
        let mut fest_words: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for e in self.load_festival_dates().await {
            let (Some(name), Some(date)) = (e["name"].as_str(), e["date"].as_str()) else { continue };
            let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else { continue };
            let days_until = (d - today).num_days();
            if !(0..=400).contains(&days_until) {
                continue;
            }
            let Some((_, word, _, _)) = Self::FESTIVALS.iter().find(|(n, _, _, _)| *n == name) else { continue };
            fest_words.insert(*word);
            let observed: Vec<&(i32, u32, u64, String)> = buckets
                .iter()
                .filter(|(k, _)| k.contains(word))
                .flat_map(|(_, v)| v.iter())
                .collect();
            let years: std::collections::HashSet<i32> = observed.iter().map(|(y, _, _, _)| *y).collect();
            let last_note = observed
                .iter()
                .max_by_key(|(y, _, _, _)| *y)
                .map(|(y, _, _, n)| format!("{y}: {n}"))
                .unwrap_or_else(|| "your tradition — first one I'm tracking".to_string());
            out.push((format!("fest:{word}"), name.to_string(), d, days_until, years.len(), last_note));
        }
        for (key, mut hits) in buckets {
            if fest_words.iter().any(|w| key.contains(*w)) || key == "puja" {
                continue; // covered by an authoritative calendar entry above
            }
            let years: std::collections::HashSet<i32> = hits.iter().map(|(y, _, _, _)| *y).collect();
            if years.len() < 2 {
                continue; // a pattern needs at least two years of evidence
            }
            // Day-of-year center with Dec/Jan wraparound handling.
            let mut doys: Vec<i64> = hits.iter().map(|(_, d, _, _)| *d as i64).collect();
            doys.sort();
            if doys.last().unwrap_or(&0) - doys.first().unwrap_or(&0) > 300 {
                for d in doys.iter_mut() {
                    if *d < 60 {
                        *d += 365;
                    }
                }
                doys.sort();
            }
            let spread = doys.last().unwrap_or(&0) - doys.first().unwrap_or(&0);
            if spread > 42 {
                continue; // not seasonal enough to project
            }
            let center = doys[doys.len() / 2] % 365;
            let mut next = chrono::NaiveDate::from_yo_opt(today.year(), center.max(1) as u32)
                .unwrap_or(today);
            if next < today {
                next = chrono::NaiveDate::from_yo_opt(today.year() + 1, center.max(1) as u32).unwrap_or(today);
            }
            let days_until = (next - today).num_days();
            hits.sort_by_key(|(y, _, _, _)| std::cmp::Reverse(*y));
            let last_note = hits.first().map(|(y, _, _, n)| format!("{y}: {n}")).unwrap_or_default();
            let label = hits.first().map(|(_, _, _, n)| n.split(" (").next().unwrap_or(n).to_string()).unwrap_or(key.clone());
            out.push((key, label, next, days_until, years.len(), last_note));
        }
        out.sort_by_key(|(_, _, _, d, _, _)| *d);
        out
    }

    /// The projected life — what's coming, with the evidence behind each projection.
    pub async fn life_horizon(&self) -> String {
        let patterns = self.life_patterns().await;
        if patterns.is_empty() {
            return "🔮 No annual patterns confident enough to project yet — they emerge as the event and trip ledgers grow (answer the day-questions!).".to_string();
        }
        let mut lines = Vec::new();
        for (key, label, next, days, years, last) in patterns.iter().take(14) {
            if *days > 180 {
                continue;
            }
            if key.starts_with("fest:") {
                lines.push(format!(
                    "🪔 {} ({days}d away) — {label} [calendar-resolved{}]",
                    next.format("%b %d"),
                    if *years > 0 { format!("; {years} year(s) in your photos, last: {last}") } else { String::new() }
                ));
            } else {
                lines.push(format!(
                    "• ~{} ({days}d away) — {label} [{years} years of evidence; last: {last}]",
                    next.format("%b %d")
                ));
            }
        }
        if lines.is_empty() {
            return "🔮 Nothing projected inside the next 6 months.".to_string();
        }
        format!("🔮 Your projected horizon (from your own life's rhythms):\n{}", lines.join("\n"))
    }

    /// Anticipation nudge gate: one pattern entering its actionable window (10-75 days), not yet
    /// nudged for this occurrence.
    pub async fn anticipate_due(&self) -> bool {
        let period_ms: i64 = std::env::var("YM_ANTICIPATE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(259_200) * 1000;
        let period_ms = (period_ms as f64 * self.domain_pace("anticipate").await) as i64;
        let last: i64 = self.memory.profile_get("anticipate_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

    /// Pick the soonest un-nudged pattern in the window and compose the anticipation.
    pub async fn anticipate_run(&self) -> Option<String> {
        let _ = self
            .memory
            .profile_set("anticipate_last", &chrono::Utc::now().timestamp_millis().to_string())
            .await;
        let done: Vec<String> = self
            .memory
            .profile_get("anticipated")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Skip pure birthday/anniversary keys — the family-date nudges own those dates; the
        // anticipation engine owns the RHYTHM-based rest (festivals, visits, recurring parties).
        let profiles = self.load_people_profiles().await;
        let person_mmdds: std::collections::HashSet<String> = profiles
            .iter()
            .flat_map(|p| {
                p.get("dates")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|d| d.get("mmdd").and_then(|x| x.as_str()).map(String::from)).collect::<Vec<_>>())
                    .unwrap_or_default()
            })
            .collect();
        // Resolve missing festival dates first (bounded: one attempt per 24h) — anticipation on a
        // stale lunar date is worse than none.
        let last_try: i64 = self.memory.profile_get("festival_refresh_try").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        if !self.festivals_unresolved().await.is_empty()
            && chrono::Utc::now().timestamp_millis() - last_try > 86_400_000
            && !self.studies.lock().unwrap().contains("festivals")
        {
            let _ = self
                .memory
                .profile_set("festival_refresh_try", &chrono::Utc::now().timestamp_millis().to_string())
                .await;
            let _ = self.festivals_refresh().await;
        }
        for (key, label, next, days, years, last) in self.life_patterns().await {
            if !(10..=75).contains(&days) {
                continue;
            }
            let occ_key = format!("{key}:{}", next.format("%Y"));
            if done.contains(&occ_key) {
                continue;
            }
            if person_mmdds.contains(&next.format("%m-%d").to_string()) && (key.contains("birthday") || key.contains("anniversary")) {
                continue;
            }
            let mut done2 = done.clone();
            done2.push(occ_key);
            if done2.len() > 100 {
                let cut = done2.len() - 100;
                done2.drain(..cut);
            }
            let _ = self.memory.profile_set("anticipated", &serde_json::to_string(&done2).unwrap_or_default()).await;
            // Festival: cultural framing + the family's own history + a LOCAL scout + an open
            // question whose answer becomes a stored plan (the ask-to-learn shape).
            if let Some(word) = key.strip_prefix("fest:") {
                let reg = Self::FESTIVALS.iter().find(|(_, w, _, _)| *w == word);
                let hint = reg.map(|(_, _, h, _)| *h).unwrap_or("");
                let dur = reg.map(|(_, _, _, d)| *d).unwrap_or(1);
                let year: i32 = next.format("%Y").to_string().parse().unwrap_or(0);
                let span = if dur > 1 {
                    format!("{} – {}", next.format("%B %d"), (next + chrono::Duration::days(dur as i64 - 1)).format("%B %d"))
                } else {
                    next.format("%B %d").to_string()
                };
                let history = if years > 0 {
                    format!(" I've seen {years} year(s) of it in your photos (last: {last}).")
                } else {
                    String::new()
                };
                let local = match self.festival_local_scout(&label, year).await {
                    Some(l) => format!(" 📍 {l}"),
                    None => String::new(),
                };
                // Stake the formal prediction the archive will grade (the receipt loop).
                let conf = (0.55 + 0.08 * years.min(4) as f64).min(0.85);
                self.life_predict(
                    &format!("life:{label} {year}"),
                    format!("The family will celebrate {label} around {} and the archive will show it", next.format("%B %d")),
                    format!("event-ledger day matching '{word}' or a 25+ photo day inside the window"),
                    next + chrono::Duration::days(10),
                    conf,
                    serde_json::json!({
                        "kind": "event", "word": word,
                        "from": (next - chrono::Duration::days(5)).format("%Y-%m-%d").to_string(),
                        "to": (next + chrono::Duration::days(7)).format("%Y-%m-%d").to_string(),
                    }),
                )
                .await;
                self.set_pending_slot(Some(&format!("plans:{label}:{year}"))).await;
                self.ledger_sent("anticipate", &format!("festival {label} ~{span}, asked plans")).await;
                return Some(format!(
                    "🪔 {label} is coming — {span} this year ({days} days out). {hint}.{history}{local} What are the plans this year?"
                ));
            }
            if let Some(city) = key.strip_prefix("visit ") {
                let conf = (0.5 + 0.08 * years.min(4) as f64).min(0.8);
                self.life_predict(
                    &format!("life:visit {city} {}", next.format("%Y")),
                    format!("The family will travel to {city} around {}", next.format("%B %d")),
                    format!("a trip to {city} in the trip ledger overlapping the window"),
                    next + chrono::Duration::days(28),
                    conf,
                    serde_json::json!({
                        "kind": "trip", "dest": city,
                        "from": (next - chrono::Duration::days(21)).format("%Y-%m-%d").to_string(),
                        "to": (next + chrono::Duration::days(21)).format("%Y-%m-%d").to_string(),
                    }),
                )
                .await;
            }
            let travel = last.contains("traveled") || key.starts_with("visit ");
            let suggestion = if travel {
                "Last time this meant travel — worth planning or booking while it's cheap."
            } else {
                "Planning time, while there's still runway."
            };
            self.ledger_sent("anticipate", &format!("projected {key} ~{}", next.format("%b %d"))).await;
            return Some(format!(
                "🔮 Looking ahead — {label} is coming up around {} ({days} days out), based on {years} year(s) of your own rhythm ({last}). {suggestion}",
                next.format("%B %d")
            ));
        }
        None
    }

}
