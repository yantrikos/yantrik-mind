//! Bengali festival calendar + family traditions -- resolve dates, list, local scout, tradition prep. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) async fn load_festival_dates(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("festival_dates")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    pub(crate) async fn save_festival_dates(&self, v: &[serde_json::Value]) {
        let _ = self
            .memory
            .profile_set("festival_dates", &serde_json::Value::Array(v.to_vec()).to_string())
            .await;
    }

    /// Which registry festivals have no resolved date covering [today, today+120d]?
    pub(crate) async fn festivals_unresolved(&self) -> Vec<(&'static str, i32)> {
        use chrono::Datelike;
        let today = local_now().date_naive();
        let have: std::collections::HashSet<(String, i32)> = self
            .load_festival_dates()
            .await
            .iter()
            .filter_map(|e| {
                let n = e["name"].as_str()?.to_string();
                let y = e["year"].as_i64()? as i32;
                Some((n, y))
            })
            .collect();
        let dates = self.load_festival_dates().await;
        let date_of = |name: &str, year: i32| -> Option<chrono::NaiveDate> {
            dates
                .iter()
                .find(|e| e["name"].as_str() == Some(name) && e["year"].as_i64() == Some(year as i64))
                .and_then(|e| e["date"].as_str())
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        };
        let mut want: Vec<(&'static str, i32)> = Vec::new();
        for (name, _, _, dur) in Self::FESTIVALS.iter() {
            let y = today.year();
            if !have.contains(&(name.to_string(), y)) {
                want.push((name, y));
            } else if let Some(d) = date_of(name, y) {
                // This year's celebration is over → the horizon needs NEXT year's date.
                if d + chrono::Duration::days(*dur as i64 + 7) < today && !have.contains(&(name.to_string(), y + 1)) {
                    want.push((name, y + 1));
                }
            }
        }
        want
    }

    /// Lunar-structure consistency guard: the Bengali festival cluster has FIXED internal
    /// arithmetic — Kojagori Lakshmi Puja = Vijayadashami + 5, Kali Puja = Dashami + ~19,
    /// Bhai Phonta = Kali Puja + 2. A web-extracted Durga Puja date that disagrees with the
    /// other anchors is wrong; re-derive it from them instead of trusting a bad snippet.
    pub(crate) async fn festival_consistency_fix(&self) -> usize {
        let mut entries = self.load_festival_dates().await;
        let get = |entries: &[serde_json::Value], name: &str, year: i64| -> Option<chrono::NaiveDate> {
            entries
                .iter()
                .find(|e| e["name"].as_str() == Some(name) && e["year"].as_i64() == Some(year))
                .and_then(|e| e["date"].as_str())
                .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        };
        let years: std::collections::HashSet<i64> = entries.iter().filter_map(|e| e["year"].as_i64()).collect();
        let mut fixed = 0usize;
        for y in years {
            let Some(durga) = get(&entries, "Durga Puja", y) else { continue };
            let dashami = durga + chrono::Duration::days(4);
            let mut anchors = 0u32;
            let mut bad = 0u32;
            if let Some(l) = get(&entries, "Lakshmi Puja", y) {
                anchors += 1;
                if !(3..=7).contains(&(l - dashami).num_days()) {
                    bad += 1;
                }
            }
            if let Some(k) = get(&entries, "Kali Puja", y) {
                anchors += 1;
                if !(16..=22).contains(&(k - dashami).num_days()) {
                    bad += 1;
                }
            }
            if anchors > 0 && bad == anchors {
                let derived = get(&entries, "Lakshmi Puja", y)
                    .map(|l| l - chrono::Duration::days(9))
                    .or_else(|| get(&entries, "Kali Puja", y).map(|k| k - chrono::Duration::days(23)));
                if let Some(nd) = derived {
                    entries.retain(|e| !(e["name"].as_str() == Some("Durga Puja") && e["year"].as_i64() == Some(y)));
                    entries.push(serde_json::json!({
                        "name": "Durga Puja", "year": y, "date": nd.format("%Y-%m-%d").to_string(), "src": "derived-lunar",
                    }));
                    fixed += 1;
                }
            }
        }
        // Mahalaya is EXACTLY the Amavasya six days before Shashthi — derive it from Durga Puja
        // rather than trusting (or waiting for) a web extraction.
        let years2: std::collections::HashSet<i64> = entries.iter().filter_map(|e| e["year"].as_i64()).collect();
        for y in years2 {
            let Some(durga) = get(&entries, "Durga Puja", y) else { continue };
            let want = durga - chrono::Duration::days(6);
            let ok = get(&entries, "Mahalaya", y).map(|m| (durga - m).num_days().abs() <= 8 && m < durga).unwrap_or(false);
            if !ok {
                entries.retain(|e| !(e["name"].as_str() == Some("Mahalaya") && e["year"].as_i64() == Some(y)));
                entries.push(serde_json::json!({
                    "name": "Mahalaya", "year": y, "date": want.format("%Y-%m-%d").to_string(), "src": "derived-lunar",
                }));
                fixed += 1;
            }
        }
        if fixed > 0 {
            self.save_festival_dates(&entries).await;
        }
        fixed
    }

    /// Detached task: resolve missing festival dates for the horizon via web search + extraction.
    pub async fn festivals_refresh(&self) -> String {
        let fixed = self.festival_consistency_fix().await;
        if fixed > 0 {
            self.notify_queue
                .lock()
                .unwrap()
                .push(format!("📅 Corrected {fixed} festival date(s) that disagreed with the lunar arithmetic (Dashami→Kojagori→Kali Puja spacing)."));
        }
        let Some(searcher) = self.searcher.clone() else {
            return "Web search isn't configured — can't resolve festival dates.".to_string();
        };
        let want = self.festivals_unresolved().await;
        if want.is_empty() {
            return "All festival dates for the horizon are already resolved — `festivals` to see them.".to_string();
        }
        let guard = "festivals".to_string();
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return "Already resolving festival dates — results land here shortly.".to_string();
        }
        // One-time identity grounding: WHY this calendar exists.
        if self.memory.profile_get("festival_identity_noted").await.ok().flatten().is_none() {
            let _ = self
                .memory
                .remember_as_belief(BeliefAssertion {
                    statement: "Pranab is Hindu and Bengali, from West Bengal — the family's year follows the Bengali Hindu festival calendar (Durga Puja above all, Kali Puja, Saraswati Puja, Poila Boishakh, Bhai Phonta...)".to_string(),
                    polarity: 1.0,
                    weight: 0.95,
                    source_event: Some("festival-calendar".into()),
                    provenance: "told".into(),
                })
                .await;
            let _ = self.memory.profile_set("festival_identity_noted", "1").await;
        }
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let inf = self.inference.clone();
        let n_want = want.len();
        tokio::spawn(async move {
            let mut resolved = 0usize;
            let mut entries: Vec<serde_json::Value> = mem
                .profile_get("festival_dates")
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            for (name, year) in want {
                let hits = match searcher.search(&format!("{name} {year} date hindu bengali calendar"), 5).await {
                    Ok(h) if !h.is_empty() => h,
                    _ => continue,
                };
                let listing = mind_tools::render_search(&hits);
                let prompt = format!(
                    "From these search results, find the {year} Gregorian START date of {name} (the Hindu/Bengali festival).\n\n{listing}\n\nOutput ONLY JSON: {{\"date\":\"YYYY-MM-DD\",\"confidence\":0.0-1.0}}. If the results don't clearly show the {year} date, use confidence 0."
                );
                let cfg = GenerationConfig { max_tokens: 80, ..GenerationConfig::default() };
                let Ok(resp) = inf.chat(vec![ChatMessage::user(&prompt)], cfg).await else { continue };
                let txt = resp.text;
                let json = txt
                    .find('{')
                    .and_then(|a| txt.rfind('}').map(|b| &txt[a..=b]))
                    .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok());
                let Some(j) = json else { continue };
                let conf = j["confidence"].as_f64().unwrap_or(0.0);
                let date = j["date"].as_str().unwrap_or("");
                let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok();
                if conf >= 0.5 && parsed.map(|d| d.format("%Y").to_string() == year.to_string()).unwrap_or(false) {
                    entries.retain(|e| !(e["name"].as_str() == Some(name) && e["year"].as_i64() == Some(year as i64)));
                    entries.push(serde_json::json!({ "name": name, "year": year, "date": date, "src": "web" }));
                    resolved += 1;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            let _ = mem.profile_set("festival_dates", &serde_json::Value::Array(entries).to_string()).await;
            studies.lock().unwrap().remove(&guard);
            nq.lock()
                .unwrap()
                .push(format!("📅 Festival calendar updated — resolved {resolved} of {n_want} pending dates. `festivals` to see the year."));
        });
        format!("📅 Resolving {n_want} festival date(s) from the calendar sources — I'll post here when done.")
    }

    /// The family's festival year — resolved dates + what each festival is.
    pub async fn festivals_list(&self) -> String {
        let today = local_now().date_naive();
        let entries = self.load_festival_dates().await;
        let mut lines: Vec<(i64, String)> = Vec::new();
        for e in &entries {
            let (Some(name), Some(date)) = (e["name"].as_str(), e["date"].as_str()) else { continue };
            let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else { continue };
            let days = (d - today).num_days();
            if !(-30..=420).contains(&days) {
                continue;
            }
            let reg = Self::FESTIVALS.iter().find(|(n, _, _, _)| *n == name);
            let hint = reg.map(|(_, _, h, _)| *h).unwrap_or("");
            let dur = reg.map(|(_, _, _, d)| *d).unwrap_or(1);
            let span = if dur > 1 {
                format!("{} – {}", d.format("%b %d"), (d + chrono::Duration::days(dur as i64 - 1)).format("%b %d"))
            } else {
                d.format("%b %d, %Y").to_string()
            };
            let when = if days < 0 { format!("{} days ago", -days) } else { format!("in {days}d") };
            lines.push((days, format!("🪔 {name} — {span} ({when}) — {hint}")));
        }
        lines.sort_by_key(|(d, _)| *d);
        let unresolved = self.festivals_unresolved().await;
        let mut out = if lines.is_empty() {
            "🪔 No festival dates resolved yet.".to_string()
        } else {
            format!(
                "🪔 The festival year (Bengali Hindu calendar):\n{}",
                lines.into_iter().map(|(_, l)| l).collect::<Vec<_>>().join("\n")
            )
        };
        if !unresolved.is_empty() {
            out.push_str(&format!(
                "\n({} date(s) not yet resolved — `festivals refresh` to look them up)",
                unresolved.len()
            ));
        }
        out
    }

    /// Modal city of the last ~4 months of camera photos — where home is right now. Cached 30d.
    pub(crate) async fn home_city_now(&self) -> Option<String> {
        if let Some(cached) = self.memory.profile_get("home_city").await.ok().flatten() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cached) {
                let age = chrono::Utc::now().timestamp_millis() - v["ts"].as_i64().unwrap_or(0);
                if age < 30 * 86_400_000 && v["v"].as_i64() == Some(2) {
                    if let Some(c) = v["city"].as_str() {
                        return Some(c.to_string());
                    }
                }
            }
        }
        let sources = mind_tools::PhotoSource::all_from_env();
        let src = sources.iter().find(|s| s.knows_people())?;
        let today = local_now().date_naive();
        let after = format!("{}T00:00:00.000Z", today - chrono::Duration::days(120));
        let before = format!("{}T23:59:59.000Z", today);
        let assets = src.taken_between(&after, &before, &[], 300).await;
        let mut counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for a in assets.iter().filter(|a| !mind_tools::is_screenish(a)) {
            // Keep "City, State" — a bare small-town name under-scopes the local search.
            let city = a.place.split(',').take(2).map(str::trim).filter(|p| !p.is_empty()).collect::<Vec<_>>().join(", ");
            if city.len() > 2 {
                *counts.entry(city).or_insert(0) += 1;
            }
        }
        let city = counts.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c)?;
        let _ = self
            .memory
            .profile_set(
                "home_city",
                &serde_json::json!({ "city": city, "ts": chrono::Utc::now().timestamp_millis(), "v": 2 }).to_string(),
            )
            .await;
        Some(city)
    }

    /// Local-celebration scout: is this festival being celebrated near home this year?
    pub(crate) async fn festival_local_scout(&self, name: &str, year: i32) -> Option<String> {
        let searcher = self.searcher.clone()?;
        let city = self.home_city_now().await?;
        let hits = searcher
            .search(&format!("{name} {year} {city} bengali association celebration event"), 4)
            .await
            .ok()
            .filter(|h| !h.is_empty())?;
        let listing = mind_tools::render_search(&hits);
        let prompt = format!(
            "From these search results, write ONE short sentence about where/when {name} {year} is being celebrated near {city} — ONLY if a result actually shows a local celebration (association, temple, community event). If nothing local and concrete, output exactly NONE.\n\n{listing}"
        );
        let cfg = GenerationConfig { max_tokens: 90, ..GenerationConfig::default() };
        let resp = self.inference.chat(vec![ChatMessage::user(&prompt)], cfg).await.ok()?;
        let line = resp.text.trim().to_string();
        if line.len() < 12 || line.to_uppercase().contains("NONE") {
            return None;
        }
        Some(line)
    }

    pub(crate) async fn load_traditions(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("festival_traditions")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
    }

    /// Add a tradition: `tradition <festival>: <what the family does>`.
    pub async fn tradition_add(&self, arg: &str) -> String {
        let Some((fest_raw, text)) = arg.split_once(':') else {
            return "Usage: tradition <festival>: <what the family does>  (e.g. tradition Mahalaya: dress-up photoshoot)".to_string();
        };
        let fest_raw = fest_raw.trim().to_lowercase();
        let text = text.trim().to_string();
        if text.len() < 4 {
            return "Tell me what the tradition actually is.".to_string();
        }
        let Some((name, _, _, _)) = Self::FESTIVALS.iter().find(|(n, w, _, _)| {
            n.to_lowercase().contains(&fest_raw) || fest_raw.contains(*w)
        }) else {
            let known = Self::FESTIVALS.iter().map(|(n, _, _, _)| *n).collect::<Vec<_>>().join(", ");
            return format!("I don't know that festival. Ones I track: {known}");
        };
        const OUTDOOR: [&str; 9] = ["photo", "shoot", "picture", "pic", "outdoor", "picnic", "park", "garden", "walk"];
        let low = text.to_lowercase();
        let weather = OUTDOOR.iter().any(|w| low.contains(w));
        let mut all = self.load_traditions().await;
        all.retain(|t| !(t["festival"].as_str() == Some(name) && t["tradition"].as_str().map(|x| x.to_lowercase()) == Some(low.clone())));
        all.push(serde_json::json!({
            "festival": name, "tradition": text, "weather": weather, "src": "told",
            "added": local_now().format("%Y-%m-%d").to_string(),
        }));
        let _ = self.memory.profile_set("festival_traditions", &serde_json::Value::Array(all).to_string()).await;
        let _ = self
            .memory
            .remember_as_belief(BeliefAssertion {
                statement: format!("Family tradition around {name}: {text}"),
                polarity: 1.0,
                weight: 0.9,
                source_event: Some("festival-tradition".into()),
                provenance: "told".into(),
            })
            .await;
        format!(
            "🪔 Remembered — around {name}: {text}.{}",
            if weather { " I'll watch the forecast and suggest the best days when it's close." } else { "" }
        )
    }

    pub async fn traditions_list(&self) -> String {
        let all = self.load_traditions().await;
        if all.is_empty() {
            return "No family traditions taught yet — `tradition <festival>: <what you do>` and I'll plan around them.".to_string();
        }
        let lines: Vec<String> = all
            .iter()
            .filter_map(|t| {
                let f = t["festival"].as_str()?;
                let tr = t["tradition"].as_str()?;
                let w = if t["weather"].as_bool().unwrap_or(false) { " 🌤" } else { "" };
                Some(format!("🪔 {f}{w} — {tr}"))
            })
            .collect();
        format!("Family traditions I plan around:\n{}", lines.join("\n"))
    }

    /// Score a forecast day for an outdoor dress-up-and-photos plan. 0-100.
    pub(crate) fn photo_day_score(d: &mind_tools::DayForecast, delta_from_fest: i64) -> i64 {
        let mut score: i64 = 100;
        if d.precip_prob >= 60.0 {
            score -= 60;
        } else if d.precip_prob >= 30.0 {
            score -= 25;
        }
        let dl = d.desc.to_lowercase();
        if ["rain", "thunder", "storm", "snow", "sleet", "freezing"].iter().any(|w| dl.contains(w)) {
            score -= 40;
        } else if ["drizzle", "overcast", "fog"].iter().any(|w| dl.contains(w)) {
            score -= 12;
        }
        if d.hi_f < 45.0 {
            score -= 25;
        } else if d.hi_f < 55.0 {
            score -= 10;
        } else if d.hi_f > 95.0 {
            score -= 20;
        }
        if d.wind_mph > 25.0 {
            score -= 15;
        }
        if d.weekday == "Sat" || d.weekday == "Sun" {
            score += 8;
        }
        score - 2 * delta_from_fest.abs()
    }

    /// Compose the best-days suggestion for one weather-dependent tradition, if the festival's
    /// window [-4, +3] overlaps the forecast. Returns None when out of range or weather missing.
    pub(crate) async fn tradition_days_suggestion(&self, fest: &str, tradition: &str) -> Option<String> {
        let weather = self.weather.clone()?;
        let today = local_now().date_naive();
        let fdate = self
            .load_festival_dates()
            .await
            .iter()
            .filter_map(|e| {
                if e["name"].as_str() != Some(fest) {
                    return None;
                }
                let d = chrono::NaiveDate::parse_from_str(e["date"].as_str()?, "%Y-%m-%d").ok()?;
                if d >= today { Some(d) } else { None }
            })
            .min()?;
        let days_until = (fdate - today).num_days();
        if days_until > 14 {
            return None; // beyond a trustworthy forecast
        }
        let city = self.home_city_now().await.unwrap_or_else(|| "home".to_string());
        let outlook = weather.daily_outlook(&city, 16).await.ok()?;
        let w_start = fdate - chrono::Duration::days(4);
        let w_end = fdate + chrono::Duration::days(3);
        let mut scored: Vec<(i64, &mind_tools::DayForecast)> = outlook
            .iter()
            .filter_map(|d| {
                let nd = chrono::NaiveDate::parse_from_str(&d.date, "%Y-%m-%d").ok()?;
                if nd < w_start || nd > w_end || nd < today {
                    return None;
                }
                Some((Self::photo_day_score(d, (nd - fdate).num_days()), d))
            })
            .collect();
        if scored.is_empty() {
            return None;
        }
        scored.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        let best: Vec<String> = scored
            .iter()
            .take(3)
            .filter(|(s, _)| *s >= 40)
            .map(|(_, d)| {
                let sunset = if d.sunset.is_empty() { String::new() } else { format!(", sunset {}", d.sunset) };
                format!("**{} {}** — {}, {:.0}°F, rain {:.0}%{sunset}", d.weekday, &d.date[5..], d.desc, d.hi_f, d.precip_prob)
            })
            .collect();
        let worst = scored
            .iter()
            .rev()
            .find(|(s, _)| *s < 20)
            .map(|(_, d)| format!(" Skip {} {} ({}, rain {:.0}%).", d.weekday, &d.date[5..], d.desc, d.precip_prob))
            .unwrap_or_default();
        let day_word = if days_until == 0 { "TODAY".to_string() } else { format!("{} ({days_until}d out)", fdate.format("%A %b %d")) };
        if best.is_empty() {
            return Some(format!(
                "🪔📸 {fest} is {day_word} — {tradition}. The forecast around it looks rough in {city}; best of it: {}, {:.0}°F, rain {:.0}%. I'd keep plans flexible.{worst}",
                scored[0].1.desc, scored[0].1.hi_f, scored[0].1.precip_prob
            ));
        }
        Some(format!(
            "🪔📸 {fest} is {day_word} — and that's {tradition}. Best days by the {city} forecast:\n{}\n{}Golden hour is the last hour before sunset.",
            best.join("\n"),
            worst.trim_start().to_string() + if worst.is_empty() { "" } else { "\n" }
        ))
    }

    /// Daily gate for tradition prep.
    pub async fn tradition_prep_due(&self) -> bool {
        let period_ms: i64 = std::env::var("YM_TRADPREP_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(86_400) * 1000;
        let last: i64 = self.memory.profile_get("tradprep_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

    /// One prep suggestion per firing: the soonest weather-dependent tradition whose festival is
    /// within forecast range and not yet prepped this occurrence.
    pub async fn tradition_prep_run(&self) -> Option<String> {
        let _ = self
            .memory
            .profile_set("tradprep_last", &chrono::Utc::now().timestamp_millis().to_string())
            .await;
        let done: Vec<String> = self
            .memory
            .profile_get("trad_prepped")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let today = local_now().date_naive();
        for t in self.load_traditions().await {
            if !t["weather"].as_bool().unwrap_or(false) {
                continue;
            }
            let (Some(fest), Some(tr)) = (t["festival"].as_str(), t["tradition"].as_str()) else { continue };
            let occ = format!("{fest}:{}", today.format("%Y"));
            if done.contains(&occ) {
                continue;
            }
            if let Some(msg) = self.tradition_days_suggestion(fest, tr).await {
                let mut done2 = done.clone();
                done2.push(occ);
                if done2.len() > 60 {
                    let cut = done2.len() - 60;
                    done2.drain(..cut);
                }
                let _ = self.memory.profile_set("trad_prepped", &serde_json::to_string(&done2).unwrap_or_default()).await;
                self.ledger_sent("anticipate", &format!("weather-planned days for {fest} tradition")).await;
                return Some(msg);
            }
        }
        None
    }

}
