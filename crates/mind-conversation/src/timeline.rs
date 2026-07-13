//! Memory timeline -- events + trips build/list, on-this-day, ask-arming. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) async fn load_events(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("events")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn save_events(&self, v: &[serde_json::Value]) {
        let _ = self.memory.profile_set("events", &serde_json::to_string(v).unwrap_or_default()).await;
    }

    /// Mine + relate event candidates (detached). Burst = ≥25 photos in a day AND ≥3× the median
    /// active day. Relations tried in order: people-layer date match (high confidence), trip
    /// membership (context), vision occasion-guess (labeled "guessed" — ask-to-learn fuel).
    pub async fn events_build(&self) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some(idx) = sources.iter().position(|s| s.knows_people()) else {
            return "No photo source connected.".to_string();
        };
        let guard = "events".to_string();
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return "Already building the event ledger — it lands here shortly.".to_string();
        }
        let src_name = sources[idx].name().to_string();
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let profiles = self.load_people_profiles().await;
        let trips = self.load_trips().await;
        let prior: std::collections::HashMap<String, serde_json::Value> = self
            .load_events()
            .await
            .into_iter()
            .filter_map(|e| e.get("date").and_then(|d| d.as_str()).map(String::from).map(|d| (d, e)))
            .collect();
        tokio::spawn(async move {
            use chrono::Datelike;
            let sources = mind_tools::PhotoSource::all_from_env();
            let Some(src) = sources.into_iter().find(|s| s.name() == src_name) else {
                studies.lock().unwrap().remove(&guard);
                return;
            };
            // Day histogram across the archive (metadata only; screenshots excluded from counts).
            let mut day_count: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
            let mut day_place: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            let mut day_assets: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
            let this_year = chrono::Utc::now().year();
            for year in 2014..=this_year {
                for q in 0..4 {
                    let m0 = q * 3 + 1;
                    let from = format!("{year}-{m0:02}-01T00:00:00.000Z");
                    let to = if m0 + 3 > 12 {
                        format!("{}-01-01T00:00:00.000Z", year + 1)
                    } else {
                        format!("{year}-{:02}-01T00:00:00.000Z", m0 + 3)
                    };
                    for a in src.taken_between(&from, &to, &[], 1000).await {
                        if a.date.len() < 10 || mind_tools::is_screenish(&a) {
                            continue;
                        }
                        *day_count.entry(a.date.clone()).or_insert(0) += 1;
                        if !a.place.is_empty() {
                            day_place.entry(a.date.clone()).or_insert(a.place.clone());
                        }
                        let e = day_assets.entry(a.date.clone()).or_default();
                        if e.len() < 5 {
                            e.push(a.id.clone());
                        }
                    }
                }
            }
            let mut counts: Vec<u32> = day_count.values().cloned().filter(|c| *c >= 3).collect();
            counts.sort();
            let median = counts.get(counts.len() / 2).cloned().unwrap_or(5).max(3);
            let vc = mind_tools::VisionClient::from_env();
            let mut events: Vec<serde_json::Value> = Vec::new();
            let mut vision_budget = 30usize; // occasion-guessing is bounded per build
            for (day, n) in day_count.iter().rev() {
                if *n < 25 || *n < median * 3 {
                    continue;
                }
                if events.len() >= 150 {
                    break;
                }
                // Keep prior labels (told > inferred > guessed) — a taught label is never overwritten.
                if let Some(p) = prior.get(day) {
                    if p.get("src").and_then(|x| x.as_str()) == Some("told") {
                        events.push(p.clone());
                        continue;
                    }
                }
                let place = day_place.get(day).cloned().unwrap_or_default();
                // WHO from sample assets.
                let mut who: Vec<String> = Vec::new();
                for aid in day_assets.get(day).cloned().unwrap_or_default().iter().take(4) {
                    let (names, _) = src.people_in(aid).await;
                    for nm in names {
                        if !who.contains(&nm) {
                            who.push(nm);
                        }
                    }
                }
                who.truncate(6);
                // Relation 1: someone's people-layer date (±1 day).
                let mmdd: String = day[5..].to_string();
                let mut label = String::new();
                let mut src_tag = "";
                'rel: for p in &profiles {
                    let Some(pname) = p.get("name").and_then(|x| x.as_str()) else { continue };
                    for d in p.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                        let (Some(dm), Some(dl)) = (d.get("mmdd").and_then(|x| x.as_str()), d.get("label").and_then(|x| x.as_str())) else {
                            continue;
                        };
                        let close = dm == mmdd
                            || chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
                                .ok()
                                .and_then(|dd| {
                                    chrono::NaiveDate::from_ymd_opt(dd.year(), dm[..2].parse().ok()?, dm[3..].parse().ok()?)
                                        .map(|target| (dd - target).num_days().abs() <= 1)
                                })
                                .unwrap_or(false);
                        if close {
                            label = format!("{pname}'s {dl} celebration");
                            src_tag = "related";
                            break 'rel;
                        }
                    }
                }
                // Relation 2: inside a trip chapter → context.
                let trip_ctx = trips
                    .iter()
                    .find(|t| {
                        t["start"].as_str().map(|s| s <= day.as_str()).unwrap_or(false)
                            && t["end"].as_str().map(|e| e >= day.as_str()).unwrap_or(false)
                    })
                    .and_then(|t| t["dest"].as_str())
                    .map(|d| d.split(',').next().unwrap_or(d).to_string());
                // Relation 3: vision occasion guess (bounded budget).
                if label.is_empty() && vision_budget > 0 {
                    if let (Some(vcl), Some(aid)) = (&vc, day_assets.get(day).and_then(|v| v.first())) {
                        let asset = mind_tools::PhotoAsset { id: aid.clone(), ..Default::default() };
                        if let Some(bytes) = src.image_bytes(&asset).await {
                            vision_budget -= 1;
                            if let Ok(g) = vcl
                                .analyze("In 3-6 words, what OCCASION does this photo show (e.g. birthday party, wedding ceremony, puja, picnic, holiday gathering, casual day)? Just the phrase.", bytes, "image/jpeg")
                                .await
                            {
                                let g1: String = g.lines().next().unwrap_or("").trim().trim_matches('"').chars().take(50).collect();
                                if g1.len() > 3 && !g1.to_lowercase().contains("casual") {
                                    label = g1;
                                    src_tag = "guessed";
                                }
                            }
                        }
                    }
                }
                events.push(serde_json::json!({
                    "date": day,
                    "photos": n,
                    "place": place,
                    "people": who,
                    "label": label,
                    "src": if label.is_empty() { "unknown" } else { src_tag },
                    "trip": trip_ctx,
                }));
            }
            let n_events = events.len();
            let related = events.iter().filter(|e| e["src"].as_str() == Some("related")).count();
            let guessed = events.iter().filter(|e| e["src"].as_str() == Some("guessed")).count();
            let unknown = events.iter().filter(|e| e["src"].as_str() == Some("unknown")).count();
            let told = events.iter().filter(|e| e["src"].as_str() == Some("told")).count();
            let _ = mem.profile_set("events", &serde_json::to_string(&events).unwrap_or_default()).await;
            nq.lock().unwrap().push(format!(
                "🎪 Event ledger built — {n_events} heavily-photographed days found: {related} matched to family dates, {guessed} occasion-guessed from the photos, {told} you've taught me, {unknown} mysteries.\n\nI'll ask about the mysteries one at a time (a photo + 'what was this?'). `events` lists them; `event <date or word>` for one.",
            ));
            studies.lock().unwrap().remove(&guard);
        });
        "🎪 Mining the archive for EVENTS (photo-burst days) and relating them to what I know — the ledger lands here in a few minutes.".to_string()
    }

    /// List events, optionally filtered.
    pub async fn events_list(&self, filter: &str) -> String {
        let events = self.load_events().await;
        if events.is_empty() {
            return "🎪 No event ledger yet — say `events build`.".to_string();
        }
        let f = filter.trim().to_lowercase();
        let mut lines = Vec::new();
        for e in &events {
            let date = e["date"].as_str().unwrap_or("");
            let label = e["label"].as_str().unwrap_or("");
            if !f.is_empty() && !date.starts_with(&f) && !label.to_lowercase().contains(&f) {
                continue;
            }
            let who = e["people"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            let tag = match e["src"].as_str().unwrap_or("") {
                "told" => "✅",
                "related" => "🔗",
                "guessed" => "🤔",
                _ => "❓",
            };
            lines.push(format!(
                "{tag} {date} — {} ({} photos){}{}",
                if label.is_empty() { "unknown occasion" } else { label },
                e["photos"],
                if who.is_empty() { String::new() } else { format!(" — {who}") },
                e["trip"].as_str().map(|t| format!(" [{t} trip]")).unwrap_or_default()
            ));
            if lines.len() >= 20 {
                break;
            }
        }
        if lines.is_empty() {
            format!("No events matching \"{}\".", filter.trim())
        } else {
            format!("🎪 Life events ({} total — ✅ taught, 🔗 related, 🤔 guessed, ❓ unknown):\n{}", events.len(), lines.join("\n"))
        }
    }

    /// Ask-to-learn: pick the biggest UNKNOWN event not yet asked, send a sample photo + question.
    /// Returns (caption, photo bytes, slot) like whois; the caller sends then arms.
    pub async fn event_ask_next(&self) -> Option<(String, Vec<u8>, String)> {
        let events = self.load_events().await;
        let asked: Vec<String> = self
            .memory
            .profile_get("events_asked")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let sources = mind_tools::PhotoSource::all_from_env();
        let src = sources.iter().find(|s| s.knows_people())?;
        let mut cands: Vec<&serde_json::Value> = events
            .iter()
            .filter(|e| {
                let src_tag = e["src"].as_str().unwrap_or("");
                (src_tag == "unknown" || src_tag == "guessed")
                    && e["date"].as_str().map(|d| !asked.contains(&d.to_string())).unwrap_or(false)
            })
            .collect();
        cands.sort_by_key(|e| std::cmp::Reverse(e["photos"].as_u64().unwrap_or(0)));
        let e = cands.first()?;
        let date = e["date"].as_str()?;
        // A sample photo from that day.
        let hits = src
            .taken_between(&format!("{date}T00:00:00.000Z"), &format!("{date}T23:59:59.000Z"), &[], 8)
            .await;
        let photo = hits.iter().find(|a| !mind_tools::is_screenish(a))?;
        let bytes = src.image_bytes(photo).await?;
        let who = e["people"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        let guess = e["label"].as_str().unwrap_or("");
        let caption = format!(
            "🎪 Help me understand this day — {date}: {} photos{}{}. What was the occasion?{}",
            e["photos"],
            if e["place"].as_str().unwrap_or("").is_empty() { String::new() } else { format!(" in {}", e["place"].as_str().unwrap_or("")) },
            if who.is_empty() { String::new() } else { format!(", with {who}") },
            if guess.is_empty() { String::new() } else { format!(" (my guess: {guess} — correct me!)") }
        );
        Some((caption, bytes, format!("event:{date}")))
    }

    /// Arm the event question after the photo actually sent.
    pub async fn event_ask_arm(&self, slot: &str) {
        let date = slot.trim_start_matches("event:").to_string();
        let mut asked: Vec<String> = self
            .memory
            .profile_get("events_asked")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if !asked.contains(&date) {
            asked.push(date);
        }
        let _ = self.memory.profile_set("events_asked", &serde_json::to_string(&asked).unwrap_or_default()).await;
        self.set_pending_slot(Some(slot)).await;
        let _ = self.memory.profile_set("event_ask_last", &chrono::Utc::now().timestamp_millis().to_string()).await;
        self.note_proactive_sent().await;
        self.ledger_sent("events", "asked what a heavily-photographed day was").await;
    }

    /// Cadence gate: unknown events exist, no pending question, once per period.
    pub async fn event_ask_due(&self) -> bool {
        if self.pending_slot().await.is_some() {
            return false;
        }
        let period_ms: i64 = std::env::var("YM_EVENTASK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(86_400) * 1000;
        let period_ms = (period_ms as f64 * self.domain_pace("events").await) as i64;
        let last: i64 = self.memory.profile_get("event_ask_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

    pub(crate) async fn load_trips(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("trips")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Build/refresh the ledger (detached — ~50 metadata sweeps + face sampling take minutes).
    pub async fn trips_build(&self) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some(idx) = sources.iter().position(|s| s.knows_people()) else {
            return "No face-aware photo source connected — the trip ledger mines the photo archive.".to_string();
        };
        let guard = "trips".to_string();
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return "Already building the trip ledger — it lands here shortly.".to_string();
        }
        let src_name = sources[idx].name().to_string();
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        tokio::spawn(async move {
            use chrono::Datelike;
            let sources = mind_tools::PhotoSource::all_from_env();
            let Some(src) = sources.into_iter().find(|s| s.name() == src_name) else {
                studies.lock().unwrap().remove(&guard);
                return;
            };
            // 1. Sweep the archive quarterly for (date, place) — metadata only, no vision.
            let mut day_place: std::collections::BTreeMap<String, std::collections::HashMap<String, u32>> = std::collections::BTreeMap::new();
            let mut day_assets: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
            let this_year = chrono::Utc::now().year();
            for year in 2014..=this_year {
                for q in 0..4 {
                    let m0 = q * 3 + 1;
                    let from = format!("{year}-{m0:02}-01T00:00:00.000Z");
                    let to = if m0 + 3 > 12 {
                        format!("{}-01-01T00:00:00.000Z", year + 1)
                    } else {
                        format!("{year}-{:02}-01T00:00:00.000Z", m0 + 3)
                    };
                    for a in src.taken_between(&from, &to, &[], 900).await {
                        if a.date.len() < 10 {
                            continue;
                        }
                        let day = a.date.clone();
                        if !a.place.is_empty() {
                            *day_place.entry(day.clone()).or_default().entry(a.place.clone()).or_insert(0) += 1;
                        }
                        let e = day_assets.entry(day).or_default();
                        if e.len() < 4 {
                            e.push(a.id.clone());
                        }
                    }
                }
            }
            if day_place.len() < 10 {
                nq.lock().unwrap().push("🧳 Trip ledger: too little GPS-tagged history to mine trips from.".to_string());
                studies.lock().unwrap().remove(&guard);
                return;
            }
            // 2. Home per year = that year's modal city.
            let mut year_city: std::collections::HashMap<i32, std::collections::HashMap<String, u32>> = std::collections::HashMap::new();
            for (day, places) in &day_place {
                let y: i32 = day[..4].parse().unwrap_or(0);
                for (p, n) in places {
                    *year_city.entry(y).or_default().entry(p.clone()).or_insert(0) += n;
                }
            }
            let home_of = |y: i32| -> String {
                year_city
                    .get(&y)
                    .and_then(|m| m.iter().max_by_key(|(_, n)| **n).map(|(p, _)| p.clone()))
                    .unwrap_or_default()
            };
            // FAMILIAR SET: a place photographed in >=4 distinct months of a year is home-region
            // (the neighboring suburb, the office town) — not a trip destination. Kills the
            // Bentonville-as-a-trip artifact around a Centerton home.
            let mut year_city_months: std::collections::HashMap<(i32, String), std::collections::HashSet<String>> = std::collections::HashMap::new();
            for (day, places) in &day_place {
                let y: i32 = day[..4].parse().unwrap_or(0);
                let month: String = day[..7].to_string();
                for p in places.keys() {
                    year_city_months.entry((y, p.clone())).or_default().insert(month.clone());
                }
            }
            let familiar = |y: i32, city: &str| -> bool {
                year_city_months.get(&(y, city.to_string())).map(|m| m.len() >= 4).unwrap_or(false)
            };
            // 3. Away-day runs (gap tolerance 2 days) → trip candidates.
            #[derive(Clone)]
            struct Run {
                start: String,
                end: String,
                places: std::collections::HashMap<String, u32>,
                photos: u32,
                sample: Vec<String>,
            }
            let mut runs: Vec<Run> = Vec::new();
            let mut cur: Option<Run> = None;
            let mut last_away: Option<chrono::NaiveDate> = None;
            for (day, places) in &day_place {
                let Ok(d) = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d") else { continue };
                let y: i32 = day[..4].parse().unwrap_or(0);
                let home = home_of(y);
                let (modal, n) = places.iter().max_by_key(|(_, n)| **n).map(|(p, n)| (p.clone(), *n)).unwrap();
                let away = !home.is_empty() && modal != home && !familiar(y, &modal);
                if away {
                    let gap_ok = last_away.map(|la| (d - la).num_days() <= 3).unwrap_or(false);
                    if let (Some(r), true) = (cur.as_mut(), gap_ok) {
                        r.end = day.clone();
                        *r.places.entry(modal.clone()).or_insert(0) += n;
                        r.photos += places.values().sum::<u32>();
                        if r.sample.len() < 8 {
                            r.sample.extend(day_assets.get(day).cloned().unwrap_or_default().into_iter().take(2));
                        }
                    } else {
                        if let Some(r) = cur.take() {
                            runs.push(r);
                        }
                        cur = Some(Run {
                            start: day.clone(),
                            end: day.clone(),
                            places: places.clone(),
                            photos: places.values().sum::<u32>(),
                            sample: day_assets.get(day).cloned().unwrap_or_default(),
                        });
                    }
                    last_away = Some(d);
                }
            }
            if let Some(r) = cur.take() {
                runs.push(r);
            }
            // 4. Keep real trips (≥2 days or a heavy single-day burst), name them, find WHO.
            let mut trips: Vec<serde_json::Value> = Vec::new();
            for r in runs {
                let days = chrono::NaiveDate::parse_from_str(&r.end, "%Y-%m-%d")
                    .and_then(|e| chrono::NaiveDate::parse_from_str(&r.start, "%Y-%m-%d").map(|s| (e - s).num_days() + 1))
                    .unwrap_or(1);
                if days < 2 && r.photos < 12 {
                    continue;
                }
                let dest = r.places.iter().max_by_key(|(_, n)| **n).map(|(p, _)| p.clone()).unwrap_or_default();
                // WHO: named faces across sample assets.
                let mut who_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
                for aid in r.sample.iter().take(6) {
                    let (names, _) = src.people_in(aid).await;
                    for n in names {
                        *who_counts.entry(n).or_insert(0) += 1;
                    }
                }
                let mut who: Vec<(String, u32)> = who_counts.into_iter().collect();
                who.sort_by(|a, b| b.1.cmp(&a.1));
                let people: Vec<String> = who.into_iter().take(6).map(|(n, _)| n).collect();
                trips.push(serde_json::json!({
                    "dest": dest,
                    "start": r.start,
                    "end": r.end,
                    "days": days,
                    "photos": r.photos,
                    "people": people,
                    "provenance": "photos:exif+faces",
                }));
            }
            trips.sort_by(|a, b| b["start"].as_str().cmp(&a["start"].as_str()));
            trips.truncate(80);
            let n_trips = trips.len();
            // Top chapters become typed beliefs (provenance-tagged).
            for t in trips.iter().take(12) {
                let people = t["people"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                let _ = mem.remember_as_belief(BeliefAssertion {
                    statement: format!(
                        "Life chapter (from photos): trip to {} — {} to {} ({} days, {} photos){}",
                        t["dest"].as_str().unwrap_or("?"),
                        t["start"].as_str().unwrap_or("?"),
                        t["end"].as_str().unwrap_or("?"),
                        t["days"],
                        t["photos"],
                        if people.is_empty() { String::new() } else { format!(", with {people}") }
                    ),
                    polarity: 1.0,
                    weight: 0.75,
                    source_event: Some("trip-ledger".into()),
                    provenance: "photos".into(),
                }).await;
            }
            let _ = mem.profile_set("trips", &serde_json::to_string(&trips).unwrap_or_default()).await;
            let preview: Vec<String> = trips
                .iter()
                .take(6)
                .map(|t| {
                    format!(
                        "• {} — {} ({}d, {} photos)",
                        t["dest"].as_str().unwrap_or("?"),
                        t["start"].as_str().unwrap_or("?"),
                        t["days"],
                        t["photos"]
                    )
                })
                .collect();
            nq.lock().unwrap().push(format!(
                "🧳 Trip ledger built — {n_trips} life chapters mined from the photo archive (where + when + who). Most recent:\n{}\n\nAsk `trips`, `trip <place>`, or `trip collage <place>`.",
                preview.join("\n")
            ));
            studies.lock().unwrap().remove(&guard);
        });
        "🧳 Mining your photo archive for life chapters (where + when + who, a few minutes) — the ledger lands here when done.".to_string()
    }

    /// The ledger listing, optionally filtered by year or destination substring.
    pub async fn trips_list(&self, filter: &str) -> String {
        let trips = self.load_trips().await;
        if trips.is_empty() {
            return "🧳 No trip ledger yet — say `trips build` and I'll mine the photo archive.".to_string();
        }
        let f = filter.trim().to_lowercase();
        let mut lines = Vec::new();
        for t in &trips {
            let dest = t["dest"].as_str().unwrap_or("?");
            let start = t["start"].as_str().unwrap_or("");
            if !f.is_empty() && !dest.to_lowercase().contains(&f) && !start.starts_with(&f) {
                continue;
            }
            let people = t["people"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
            lines.push(format!(
                "• {dest} — {start} → {} ({}d, {} photos){}",
                t["end"].as_str().unwrap_or(""),
                t["days"],
                t["photos"],
                if people.is_empty() { String::new() } else { format!(" — with {people}") }
            ));
            if lines.len() >= 20 {
                break;
            }
        }
        if lines.is_empty() {
            format!("No trips matching \"{}\" in the ledger.", filter.trim())
        } else {
            format!("🧳 Life chapters ({} total):\n{}", trips.len(), lines.join("\n"))
        }
    }

    /// A warm one-chapter brief: facts from the ledger + the offer of a collage.
    pub async fn trip_brief(&self, query: &str) -> String {
        let trips = self.load_trips().await;
        let q = query.trim().to_lowercase();
        let Some(t) = trips.iter().find(|t| {
            t["dest"].as_str().map(|d| d.to_lowercase().contains(&q)).unwrap_or(false)
                || t["start"].as_str().map(|s| s.starts_with(&q)).unwrap_or(false)
        }) else {
            return format!("No chapter matching \"{}\" — `trips` lists what I know; `trips build` re-mines.", query.trim());
        };
        let people = t["people"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        format!(
            "🧳 {} — {} to {} ({} days, {} photos{}).\n\nSay `trip collage {}` and I'll compose the album page.",
            t["dest"].as_str().unwrap_or("?"),
            t["start"].as_str().unwrap_or("?"),
            t["end"].as_str().unwrap_or("?"),
            t["days"],
            t["photos"],
            if people.is_empty() { String::new() } else { format!(", with {people}") },
            t["dest"].as_str().unwrap_or("?").split(',').next().unwrap_or("?")
        )
    }

    /// Compose a curated collage for one chapter (the studio, window-scoped to the trip).
    pub async fn trip_collage(&self, query: &str, target: Option<i64>) -> String {
        let trips = self.load_trips().await;
        let q = query.trim().to_lowercase();
        let Some(t) = trips.iter().find(|t| t["dest"].as_str().map(|d| d.to_lowercase().contains(&q)).unwrap_or(false)) else {
            return format!("No chapter matching \"{}\" — `trips` lists them.", query.trim());
        };
        let (dest, start, end) = (
            t["dest"].as_str().unwrap_or("?").to_string(),
            t["start"].as_str().unwrap_or("").to_string(),
            t["end"].as_str().unwrap_or("").to_string(),
        );
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some(idx) = sources.iter().position(|s| s.knows_people()) else {
            return "No photo source connected.".to_string();
        };
        let guard = format!("tripcollage:{q}");
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return "Already composing that one.".to_string();
        }
        let src_name = sources[idx].name().to_string();
        let pq = self.photo_queue.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let people_line = t["people"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        let (dest2, start2, end2) = (dest.clone(), start.clone(), end.clone());
        tokio::spawn(async move {
            let sources = mind_tools::PhotoSource::all_from_env();
            let Some(src) = sources.into_iter().find(|s| s.name() == src_name) else {
                studies.lock().unwrap().remove(&guard);
                return;
            };
            let (dest, start, end) = (dest2, start2, end2);
            let assets = src
                .taken_between(&format!("{start}T00:00:00.000Z"), &format!("{end}T23:59:59.000Z"), &[], 60)
                .await;
            // Day-spread + technical triage, then compose.
            let mut cells: Vec<(Vec<u8>, Option<(f32, f32, f32, f32)>)> = Vec::new();
            let mut used_days: std::collections::HashSet<String> = std::collections::HashSet::new();
            for a in &assets {
                if cells.len() >= 9 {
                    break;
                }
                if mind_tools::is_screenish(a) {
                    continue; // ads/app-screens are not memories
                }
                if !used_days.insert(a.date.clone()) && assets.len() > 12 {
                    continue;
                }
                let Some(bytes) = src.image_bytes(a).await else { continue };
                if let Some((sharp, luma, _)) = mind_tools::photo_quality(&bytes) {
                    if sharp < 30.0 || luma < 35.0 || luma > 220.0 {
                        continue;
                    }
                }
                cells.push((bytes, None));
            }
            let n = cells.len();
            if n < 2 {
                nq.lock().unwrap().push(format!("🧳 Couldn't gather enough good frames for the {dest} collage."));
            } else if let Some(img) = mind_tools::make_collage(cells).await {
                let cap = format!(
                    "🧳 {dest} — {start} → {end}{}",
                    if people_line.is_empty() { String::new() } else { format!(" · {people_line}") }
                );
                pq.lock().unwrap().push((img, cap, target));
            } else {
                nq.lock().unwrap().push(format!("🧳 The {dest} collage composition failed — honest miss."));
            }
            studies.lock().unwrap().remove(&guard);
        });
        format!("🧳 Composing the {dest} chapter ({start} → {end}) — it lands here in a minute or two.")
    }

    /// ON THIS DAY — a real photo from this exact date in a past year, captioned with who's in it
    /// (saved face data) and where (EXIF). Queued onto the photo lane; rides the morning briefing.
    pub async fn queue_on_this_day(&self) -> bool {
        use chrono::Datelike;
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some(src) = sources.iter().find(|s| s.knows_people()) else { return false };
        let today = local_now();
        let sent = self.photos_sent().await;
        // 1. A SIGNIFICANT day first — someone's birthday/anniversary from the people layer means
        //    throwback photos OF THAT PERSON around this date in past years. A cloud gallery
        //    resurfaces dates; this resurfaces the PERSON on their day, because the substrate
        //    knows who they are. Narrated, not just dated.
        let mmdd = today.format("%m-%d").to_string();
        let store = self.load_people_profiles().await;
        for p in &store {
            let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            let dates = p.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            let Some(label) = dates
                .iter()
                .find(|d| d.get("mmdd").and_then(|x| x.as_str()) == Some(mmdd.as_str()))
                .and_then(|d| d.get("label").and_then(|x| x.as_str()))
                .map(String::from)
            else {
                continue;
            };
            let Some((i, pid, disp)) = self.resolve_face(&sources, name).await else { continue };
            for back in 1..=8 {
                let Some(day) = chrono::NaiveDate::from_ymd_opt(today.year() - back, today.month(), today.day()) else {
                    continue;
                };
                let from = day - chrono::Duration::days(3);
                let to = day + chrono::Duration::days(4);
                let hits = sources[i]
                    .taken_between(&format!("{from}T00:00:00.000Z"), &format!("{to}T00:00:00.000Z"), &[pid.clone()], 4)
                    .await;
                for a in hits {
                    if sent.contains(&a.id) || mind_tools::is_screenish(&a) {
                        continue;
                    }
                    let Some(bytes) = sources[i].image_bytes(&a).await else { continue };
                    let (names, _) = sources[i].people_in(&a.id).await;
                    let when = format!("{} ({} years ago, around {disp}'s {label})", a.date, back);
                    let story = self.narrate_memory(&bytes, &names, &when, &a.place).await;
                    let mut cap = format!("🎉 {disp}'s {label} — {}", day.year());
                    match story {
                        Some(st) => cap.push_str(&format!("\n{st}")),
                        None if !names.is_empty() => cap.push_str(&format!(" · {}", names.join(", "))),
                        None => {}
                    }
                    self.note_photo_sent(&a.id).await;
                    self.session_note_photo(sources[i].name(), &a.id, &cap, &a.date);
                    self.photo_queue.lock().unwrap().push((bytes, cap, None));
                    return true;
                }
            }
        }
        // 2a. LABELED EVENT anniversaries first — "Aadrisha's birthday party, 3 years ago,
        //     87 photos" is the strongest memory this system can produce.
        {
            let mmdd = today.format("%m-%d").to_string();
            for e in self.load_events().await.iter().take(150) {
                let Some(date) = e["date"].as_str() else { continue };
                let label = e["label"].as_str().unwrap_or("");
                if label.is_empty() || date.len() != 10 || &date[5..] != mmdd.as_str() {
                    continue;
                }
                let years = today.year() - date[..4].parse::<i32>().unwrap_or(today.year());
                if years < 1 {
                    continue;
                }
                let hits = src
                    .taken_between(&format!("{date}T00:00:00.000Z"), &format!("{date}T23:59:59.000Z"), &[], 6)
                    .await;
                for a in hits {
                    if sent.contains(&a.id) || mind_tools::is_screenish(&a) {
                        continue;
                    }
                    let Some(bytes) = src.image_bytes(&a).await else { continue };
                    let cap = format!("🎪 {years} year(s) ago today — {label} ({} photos that day)", e["photos"]);
                    self.note_photo_sent(&a.id).await;
                    self.photo_queue.lock().unwrap().push((bytes, cap, None));
                    return true;
                }
            }
        }
        // 2. Experience anniversaries: a trip that STARTED on this date in a past year beats a
        //    generic same-day photo ("3 years since Kolkata" is a chapter, not a coincidence).
        {
            let mmdd = today.format("%m-%d").to_string();
            for t in self.load_trips().await.iter().take(60) {
                let Some(start) = t["start"].as_str() else { continue };
                if start.len() == 10 && &start[5..] == mmdd.as_str() {
                    let years = today.year() - start[..4].parse::<i32>().unwrap_or(today.year());
                    if years < 1 {
                        continue;
                    }
                    let dest = t["dest"].as_str().unwrap_or("?");
                    let hits = src
                        .taken_between(&format!("{start}T00:00:00.000Z"), &format!("{}T23:59:59.000Z", t["end"].as_str().unwrap_or(start)), &[], 6)
                        .await;
                    for a in hits {
                        if sent.contains(&a.id) || mind_tools::is_screenish(&a) {
                            continue;
                        }
                        let Some(bytes) = src.image_bytes(&a).await else { continue };
                        let people = t["people"].as_array().map(|x| x.iter().filter_map(|p| p.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                        let cap = format!(
                            "🧳 {years} year(s) ago today, this trip began — {dest}{}",
                            if people.is_empty() { String::new() } else { format!(", with {people}") }
                        );
                        self.note_photo_sent(&a.id).await;
                        self.photo_queue.lock().unwrap().push((bytes, cap, None));
                        return true;
                    }
                }
            }
        }
        // 3. Otherwise this exact day in a past year — still narrated (who they ARE + the scene).
        for back in 1..=10 {
            let Some(day) = chrono::NaiveDate::from_ymd_opt(today.year() - back, today.month(), today.day()) else {
                continue;
            };
            let Some(nxt) = day.succ_opt() else { continue };
            let hits = src.taken_between(&format!("{day}T00:00:00.000Z"), &format!("{nxt}T00:00:00.000Z"), &[], 6).await;
            if hits.is_empty() {
                continue;
            }
            let mut pick: Option<(&mind_tools::PhotoAsset, Vec<String>)> = None;
            for a in hits.iter().take(6) {
                if sent.contains(&a.id) || mind_tools::is_screenish(a) {
                    continue;
                }
                let (names, _) = src.people_in(&a.id).await;
                if !names.is_empty() {
                    pick = Some((a, names));
                    break;
                }
                if pick.is_none() {
                    pick = Some((a, Vec::new()));
                }
            }
            let Some((a, names)) = pick else { continue };
            let Some(bytes) = src.image_bytes(a).await else { continue };
            let years = if back == 1 { "a year ago today".to_string() } else { format!("{back} years ago today") };
            let when = format!("{} ({years})", day.format("%b %d, %Y"));
            let story = self.narrate_memory(&bytes, &names, &when, &a.place).await;
            let mut cap = format!("📸 {years} — {}", day.format("%b %d, %Y"));
            if !names.is_empty() {
                cap.push_str(&format!(" · {}", names.join(", ")));
            }
            if !a.place.is_empty() {
                cap.push_str(&format!(" · {}", a.place));
            }
            if let Some(st) = story {
                cap.push_str(&format!("\n{st}"));
            }
            self.note_photo_sent(&a.id).await;
            self.session_note_photo(src.name(), &a.id, &cap, &a.date);
            self.photo_queue.lock().unwrap().push((bytes, cap, None));
            return true;
        }
        false
    }

}
