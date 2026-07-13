//! Photo studio -- style timeline, then-now, grow-up reels, collages, taste/gift intel. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// Today's pick: (jpeg, caption). Choice cached per day; bytes re-fetched per request.
    pub async fn frame_today(&self) -> Option<(Vec<u8>, String)> {
        let today = local_now().date_naive();
        let dkey = today.format("%Y-%m-%d").to_string();
        let sources = mind_tools::PhotoSource::all_from_env();
        let src = sources.iter().find(|s| s.knows_people())?;
        // Cached pick for today?
        if let Some(p) = self
            .memory
            .profile_get("frame_pick")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            if p["date"].as_str() == Some(dkey.as_str()) {
                if let (Some(id), Some(cap)) = (p["id"].as_str(), p["caption"].as_str()) {
                    let a = mind_tools::PhotoAsset {
                        id: id.to_string(),
                        date: String::new(),
                        place: String::new(),
                        file: String::new(),
                        camera: true,
                    };
                    if let Some(bytes) = src.image_bytes(&a).await {
                        return Some((bytes, cap.to_string()));
                    }
                }
            }
        }
        // Pick fresh. Helper: best-quality real photo from a candidate day/window.
        async fn best_of(src: &mind_tools::PhotoSource, from: &str, to: &str, person: &[String]) -> Option<(String, String, String)> {
            let assets = src.taken_between(from, to, person, 60).await;
            let mut best: Option<(f32, String, String, String)> = None;
            let mut tried = 0;
            for a in assets.iter().filter(|a| !mind_tools::is_screenish(a)) {
                if tried >= 5 {
                    break;
                }
                let Some(bytes) = src.image_bytes(a).await else { continue };
                let Some((sharp, luma, _)) = mind_tools::photo_quality(&bytes) else { continue };
                tried += 1;
                if sharp < 15.0 || luma < 28.0 || luma > 228.0 {
                    continue;
                }
                if best.as_ref().map(|(s, _, _, _)| sharp > *s).unwrap_or(true) {
                    best = Some((sharp, a.id.clone(), a.date.clone(), a.place.clone()));
                }
            }
            best.map(|(_, id, d, p)| (id, d, p))
        }
        let mmdd = today.format("%m-%d").to_string();
        let mut pick: Option<(String, String)> = None; // (asset id, caption)
        // 1. A person's day (birthday/anniversary) — their best recent frame.
        for p in self.load_people_profiles().await {
            let Some(name) = p.get("name").and_then(|x| x.as_str()) else { continue };
            let dates = p.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            let Some(label) = dates.iter().find_map(|d| {
                (d.get("mmdd").and_then(|x| x.as_str()) == Some(mmdd.as_str()))
                    .then(|| d.get("label").and_then(|x| x.as_str()).unwrap_or("day").to_string())
            }) else {
                continue;
            };
            if let Some((_, pid, disp)) = self.resolve_face(&sources, name).await {
                let from = format!("{}T00:00:00.000Z", today - chrono::Duration::days(365));
                let to = format!("{}T23:59:59.000Z", today);
                if let Some((id, _, _)) = best_of(src, &from, &to, &[pid]).await {
                    pick = Some((id, format!("Today is {disp}'s {label} ❤")));
                    break;
                }
            }
        }
        // 2. A labeled event's anniversary — a photo from that very day.
        if pick.is_none() {
            for e in self.load_events().await {
                let (Some(date), Some(label)) = (e["date"].as_str(), e["label"].as_str()) else { continue };
                if label.is_empty() || !date.ends_with(&mmdd) || date.starts_with(&dkey[..4]) {
                    continue;
                }
                let from = format!("{date}T00:00:00.000Z");
                let to = format!("{date}T23:59:59.000Z");
                if let Some((id, _, place)) = best_of(src, &from, &to, &[]).await {
                    let year = &date[..4];
                    let where_ = if place.is_empty() { String::new() } else { format!(" · {}", place.split(',').next().unwrap_or("")) };
                    pick = Some((id, format!("{label} — {year}{where_}")));
                    break;
                }
            }
        }
        // 3. This day in history — the year with the most photos on this date.
        if pick.is_none() {
            use chrono::Datelike;
            for year in (2014..today.year()).rev() {
                let day = format!("{year}-{mmdd}");
                let from = format!("{day}T00:00:00.000Z");
                let to = format!("{day}T23:59:59.000Z");
                if let Some((id, _, place)) = best_of(src, &from, &to, &[]).await {
                    let where_ = if place.is_empty() { String::new() } else { format!(" · {}", place.split(',').next().unwrap_or("")) };
                    pick = Some((id, format!("This day, {year}{where_}")));
                    break;
                }
            }
        }
        // 4. Slow walk: a month window somewhere in the archive, rotated by day-of-year.
        if pick.is_none() {
            use chrono::Datelike;
            let span = (today.year() - 2014).max(1) as u32;
            let year = 2014 + (today.ordinal() % span) as i32;
            let month = 1 + (today.ordinal() * 7 % 12);
            let from = format!("{year}-{month:02}-01T00:00:00.000Z");
            let to = if month == 12 {
                format!("{}-01-01T00:00:00.000Z", year + 1)
            } else {
                format!("{year}-{:02}-01T00:00:00.000Z", month + 1)
            };
            if let Some((id, d, place)) = best_of(src, &from, &to, &[]).await {
                let where_ = if place.is_empty() { String::new() } else { format!(" · {}", place.split(',').next().unwrap_or("")) };
                let ym = if d.len() >= 7 { d[..7].to_string() } else { format!("{year}") };
                pick = Some((id, format!("From the archive · {ym}{where_}")));
            }
        }
        let (id, caption) = pick?;
        let _ = self
            .memory
            .profile_set(
                "frame_pick",
                &serde_json::json!({ "date": dkey, "id": id, "caption": caption }).to_string(),
            )
            .await;
        let a = mind_tools::PhotoAsset { id, date: String::new(), place: String::new(), file: String::new(), camera: true };
        let bytes = src.image_bytes(&a).await?;
        Some((bytes, caption))
    }

    pub async fn style_timeline_build(&self, who: &str) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some((_, pid, display)) = self.resolve_face(&sources, who).await else {
            return format!("I don't know \"{who}\" yet — `whois` teaches me people.");
        };
        let guard = format!("style:{}", display.to_lowercase());
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return format!("Already reading {display}'s visual history — the timeline lands here.");
        }
        let src_name = sources.iter().find(|s| s.knows_people()).map(|s| s.name().to_string()).unwrap_or_default();
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let inf = self.inference.clone();
        let disp2 = display.clone();
        tokio::spawn(async move {
            if let Some(msg) = style_task(src_name, pid, disp2, mem, inf).await {
                nq.lock().unwrap().push(msg);
            }
            studies.lock().unwrap().remove(&guard);
        });
        format!("📈 Reading {display}'s whole visual history year by year — the evolution lands here when done.")
    }

    pub async fn style_view(&self, who: &str) -> String {
        let key = format!("style_timeline:{}", who.trim().to_lowercase());
        let Some(kv) = self
            .memory
            .profile_get(&key)
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        else {
            return format!("No style timeline for {who} yet — `style build {who}` reads their whole visual history.");
        };
        let rows = kv["rows"].as_array().cloned().unwrap_or_default();
        let table = rows
            .iter()
            .map(|r| {
                let j = |k: &str| {
                    r[k].as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("/"))
                        .unwrap_or_default()
                };
                format!(
                    "{} · traditional {}% · {} · {} · vibe {} · jewelry {}",
                    r["year"], r["trad_pct"], j("outfits"), j("colors"), j("vibe"), r["jwl"]
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("📈 {who} — style evolution:\n{table}\n\n{}", kv["trend"].as_str().unwrap_or(""))
    }

    pub async fn find_younger_self(&self, who: &str) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some((idx, pid, display)) = self.resolve_face(&sources, who).await else {
            return format!("I don't know \"{who}\" yet — `whois` teaches me people.");
        };
        let guard = format!("youngerself:{}", display.to_lowercase());
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return "Already searching for that younger self — results land here.".to_string();
        }
        let src_name = sources[idx].name().to_string();
        let display2 = display.clone();
        let family: Vec<String> = self
            .load_people_profiles()
            .await
            .iter()
            .filter_map(|p| p.get("name").and_then(|x| x.as_str()).map(String::from))
            .filter(|n| n.to_lowercase() != display.to_lowercase())
            .collect();
        let nq = self.notify_queue.clone();
        let pq = self.photo_queue.clone();
        let studies = self.studies.clone();
        let mem = self.memory.clone();
        tokio::spawn(async move {
            let display = display2;
            let sources = mind_tools::PhotoSource::all_from_env();
            let Some(src) = sources.into_iter().find(|s| s.name() == src_name) else {
                studies.lock().unwrap().remove(&guard);
                return;
            };
            // Target's era anchor: the first year their cluster is DENSE (a handful of mis-tags
            // must not drag the anchor back years).
            let target_assets = src.assets_of_people(&[pid.clone()], 300, true).await;
            let mut year_counts: std::collections::BTreeMap<i32, u32> = std::collections::BTreeMap::new();
            for a in &target_assets {
                if let Ok(y) = a.date.chars().take(4).collect::<String>().parse::<i32>() {
                    *year_counts.entry(y).or_insert(0) += 1;
                }
            }
            let target_first: i32 = year_counts
                .iter()
                .find(|(_, n)| **n >= 15)
                .map(|(y, _)| *y)
                .or_else(|| year_counts.keys().next().copied())
                .unwrap_or(2019);
            let rejected: Vec<String> = mem
                .profile_get("youngerself_no")
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            // Candidates: big unnamed clusters.
            let mut scored: Vec<(f64, String, u64, i32, i32, f64)> = Vec::new();
            for p in src.list_people().await {
                if !p.name.trim().is_empty() || rejected.contains(&p.id) {
                    continue;
                }
                let count = src.person_photo_count(&p.id).await.unwrap_or(0);
                if count < 60 {
                    continue;
                }
                let assets = src.assets_of_people(&[p.id.clone()], 300, true).await;
                if assets.is_empty() {
                    continue;
                }
                let years: Vec<i32> = assets
                    .iter()
                    .filter_map(|a| a.date.chars().take(4).collect::<String>().parse::<i32>().ok())
                    .collect();
                let (y0, y1) = (
                    years.iter().min().copied().unwrap_or(0),
                    years.iter().max().copied().unwrap_or(0),
                );
                // Must live mostly BEFORE the target's verified era (with 1y overlap tolerance).
                if y0 == 0 || y0 > target_first {
                    continue;
                }
                // Family co-occurrence over a small sample.
                let step = (assets.len() / 6).max(1);
                let mut with_family = 0u32;
                let mut sampled = 0u32;
                for a in assets.iter().step_by(step).take(6) {
                    let (names, _) = src.people_in(&a.id).await;
                    sampled += 1;
                    if names.iter().any(|n| family.iter().any(|f| f.eq_ignore_ascii_case(n))) {
                        with_family += 1;
                    }
                }
                if sampled == 0 {
                    continue;
                }
                let co = with_family as f64 / sampled as f64;
                let adjacency = 1.0 / (1.0 + (target_first - y1).abs() as f64); // ends near target's start
                let size_score = (count as f64).ln() / 10.0;
                let score = co * 0.5 + adjacency * 0.3 + size_score * 0.2;
                scored.push((score, p.id.clone(), count, y0, y1, co));
            }
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            scored.retain(|(sc, _, _, _, _, co)| *sc > 0.25 && *co >= 0.3);
            // Runner-ups wait in a queue so a "no" can offer the next one immediately.
            let runner_ups: Vec<serde_json::Value> = scored
                .iter()
                .skip(1)
                .take(3)
                .map(|(sc, id, n, y0, y1, co)| serde_json::json!({"id": id, "score": sc, "count": n, "y0": y0, "y1": y1, "co": co}))
                .collect();
            let _ = mem
                .profile_set(&format!("youngerself_cands_{}", display.to_lowercase()), &serde_json::Value::Array(runner_ups).to_string())
                .await;
            let Some((score, cand_id, count, y0, y1, co)) = scored.into_iter().next() else {
                nq.lock().unwrap().push(format!(
                    "🕵️ I searched the unnamed clusters for {display}'s younger self and none scored high enough (need family co-occurrence + the right years). Their early photos may simply not be in the library."
                ));
                studies.lock().unwrap().remove(&guard);
                return;
            };
            let Some(thumb) = src.face_thumbnail(&cand_id).await else {
                studies.lock().unwrap().remove(&guard);
                return;
            };
            let caption = format!(
                "🕵️ I think I found {display}'s younger self: an unnamed person in the library with {count} photos spanning {y0}–{y1}, appearing with the family in {:.0}% of my samples (confidence {:.2}). Is this {display} as a baby? (yes to merge her timeline / no)",
                co * 100.0,
                score
            );
            let _ = mem.profile_set("pending_onboard", &format!("mergeface:{display}:{pid}:{cand_id}")).await;
            pq.lock().unwrap().push((thumb, caption, None));
            studies.lock().unwrap().remove(&guard);
        });
        format!("🕵️ Hunting for {display}'s younger self among the unnamed clusters — evidence + one sample photo lands here.")
    }

    /// Compose and queue the pair. Detached; honest notify when the archive is too shallow.
    pub async fn then_now_run(&self, who: &str, occasion: Option<String>, target: Option<i64>) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some((idx, pid, display)) = self.resolve_face(&sources, who).await else {
            return format!("I don't have a face for \"{who}\" yet — `whois` teaches me people.");
        };
        let guard = format!("thennow:{}", display.to_lowercase());
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return format!("Already composing {display}'s then-and-now — it lands in chat shortly.");
        }
        let src_name = sources[idx].name().to_string();
        let display2 = display.clone();
        // OUR eyes, not the source's tags: the person's face-gallery centroid gates every frame.
        let centroid: Option<Vec<f32>> = {
            let g = self.face_gallery().await;
            g["people"][&display]["c"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_f64().map(|x| x as f32)).collect::<Vec<f32>>())
                .filter(|c| !c.is_empty())
        };
        let threshold: f32 = std::env::var("YM_FACE_THRESHOLD").ok().and_then(|s| s.parse().ok()).unwrap_or(0.45);
        let nq = self.notify_queue.clone();
        let pq = self.photo_queue.clone();
        let studies = self.studies.clone();
        let inf = self.inference.clone();
        tokio::spawn(async move {
            let display = display2;
            let done = |studies: &Arc<Mutex<std::collections::HashSet<String>>>, g: &str| {
                studies.lock().unwrap().remove(g);
            };
            let sources = mind_tools::PhotoSource::all_from_env();
            let Some(src) = sources.into_iter().find(|s| s.name() == src_name) else {
                done(&studies, &guard);
                return;
            };
            // Best frame from a candidate list: real photo, sharp, decently lit.
            async fn best_frame(
                src: &mind_tools::PhotoSource,
                cands: &[mind_tools::PhotoAsset],
                centroid: &Option<Vec<f32>>,
                threshold: f32,
            ) -> (Option<(Vec<u8>, String, String)>, u32) {
                let mut best: Option<(f32, Vec<u8>, String, String)> = None;
                let mut rejected = 0u32;
                for a in cands.iter().filter(|a| !mind_tools::is_screenish(a)).take(20) {
                    let Some(bytes) = src.image_bytes(a).await else { continue };
                    let Some((sharp, luma, _)) = mind_tools::photo_quality(&bytes) else { continue };
                    if sharp < 22.0 || luma < 30.0 || luma > 225.0 {
                        continue;
                    }
                    // The source says this photo shows the person; verify with OUR gallery before
                    // trusting it. ML unreachable -> fall back to tag trust rather than stalling.
                    if let Some(c) = centroid {
                        if let Some(eng) = mind_tools::FaceEngine::from_env() {
                            match eng.faces(bytes.clone()).await {
                                Ok(faces) => {
                                    if !faces.iter().any(|f| mind_tools::cosine(&f.embedding, c) >= threshold) {
                                        rejected += 1;
                                        continue;
                                    }
                                }
                                Err(_) => {}
                            }
                        }
                    }
                    if best.as_ref().map(|(s, _, _, _)| sharp > *s).unwrap_or(true) {
                        best = Some((sharp, bytes, a.date.clone(), a.place.clone()));
                    }
                }
                (best.map(|(_, b, d, p)| (b, d, p)), rejected)
            }
            let mut old = src.assets_of_people(&[pid.clone()], 1000, true).await;
            let recent = src.assets_of_people(&[pid.clone()], 1000, false).await;
            let have: std::collections::HashSet<String> = old.iter().map(|a| a.id.clone()).collect();
            old.extend(recent.into_iter().filter(|a| !have.contains(&a.id)));
            let new = src.assets_of_people(&[pid.clone()], 40, false).await;
            // TEMPORAL IDENTITY CHAIN: a child's earliest face can't match their current centroid,
            // so recognition propagates backward — each verified year's faces update a rolling
            // centroid that then verifies the next-older year. Mis-tags still fail; babyhood passes.
            let mut by_year: std::collections::BTreeMap<String, Vec<&mind_tools::PhotoAsset>> = std::collections::BTreeMap::new();
            for a in old.iter().filter(|a| !mind_tools::is_screenish(a) && a.date.len() >= 4) {
                by_year.entry(a.date.chars().take(4).collect()).or_default().push(a);
            }
            let mut then_res: Option<(Vec<u8>, String, String)> = None;
            let mut then_rej = 0u32;
            let mut ml_budget = 80u32;
            let chain_threshold = (threshold - 0.07).max(0.36);
            let mut rolling: Option<Vec<f32>> = centroid.clone();
            if let (Some(_), Some(eng)) = (&rolling, mind_tools::FaceEngine::from_env()) {
                for (_year, assets) in by_year.iter().rev() {
                    // Face-check up to 6 frames that PASS the cheap gates (quality first, so the
                    // ML tries aren't wasted on blurry forwards), oldest-in-year preferred.
                    let mut year_hits: Vec<Vec<f32>> = Vec::new();
                    let mut year_best: Option<(Vec<u8>, String, String)> = None;
                    let mut tried = 0u32;
                    for a in assets.iter() {
                        if ml_budget == 0 || tried >= 6 {
                            break;
                        }
                        let Some(bytes) = src.image_bytes(a).await else { continue };
                        let Some((sharp, luma, _)) = mind_tools::photo_quality(&bytes) else { continue };
                        if sharp < 22.0 || luma < 30.0 || luma > 225.0 {
                            continue;
                        }
                        tried += 1;
                        ml_budget -= 1;
                        let Ok(faces) = eng.faces(bytes.clone()).await else { continue };
                        let cur = rolling.as_ref().unwrap();
                        let hit = faces
                            .iter()
                            .map(|f| (mind_tools::cosine(&f.embedding, cur), &f.embedding))
                            .filter(|(sim, _)| *sim >= chain_threshold)
                            .max_by(|(a2, _), (b2, _)| a2.partial_cmp(b2).unwrap_or(std::cmp::Ordering::Equal));
                        match hit {
                            Some((_, emb)) => {
                                year_hits.push(emb.clone());
                                // oldest verified frame in the year wins (we iterate oldest-first)
                                if year_best.is_none() {
                                    year_best = Some((bytes, a.date.clone(), a.place.clone()));
                                }
                            }
                            None => then_rej += 1,
                        }
                    }
                    if let Some(best) = year_best {
                        then_res = Some(best); // keeps being replaced by ever-older years
                        // EMA-update the rolling centroid with this year's verified faces so the
                        // next-older year is judged by a face closer to its own era.
                        if let Some(cur) = rolling.as_mut() {
                            for emb in &year_hits {
                                for (c1, e1) in cur.iter_mut().zip(emb.iter()) {
                                    *c1 = 0.7 * *c1 + 0.3 * e1;
                                }
                            }
                        }
                    }
                    if ml_budget == 0 {
                        break;
                    }
                }
            } else if let Some(a) = by_year.values().next().and_then(|v| v.first()) {
                // No gallery/engine: fall back to the oldest decent frame on tag trust.
                if let Some(bytes) = src.image_bytes(a).await {
                    then_res = Some((bytes, a.date.clone(), a.place.clone()));
                }
            }
            let Some((then_b, then_d, then_p)) = then_res else {
                let why = if then_rej > 0 {
                    format!("even walking my identity chain back through the years, I couldn't verify a single old frame as {display} ({then_rej} rejected). Most likely the library keeps their younger self as a SEPARATE unnamed person — a `whois` session on the baby photos would let me link the two")
                } else {
                    format!("the old archive around {display} is mostly screenshots")
                };
                nq.lock().unwrap().push(format!("↔ Couldn't build a truthful then-and-now — {why}."));
                done(&studies, &guard);
                return;
            };
            if then_rej > 0 {
                nq.lock().unwrap().push(format!("↔ Note: I walked past {then_rej} old frame(s) tagged as {display} that my own face check says aren't them — the pair uses the earliest one that really is."));
            }
            let (now_res, _) = best_frame(&src, &new, &centroid, threshold).await;
            let Some((now_b, now_d, now_p)) = now_res else {
                nq.lock().unwrap().push(format!("↔ No clean recent frame of {display} to pair with the old one."));
                done(&studies, &guard);
                return;
            };
            let (y_then, y_now) = (then_d.chars().take(4).collect::<String>(), now_d.chars().take(4).collect::<String>());
            let gap: i64 = y_now.parse::<i64>().unwrap_or(0) - y_then.parse::<i64>().unwrap_or(0);
            if gap < 2 {
                nq.lock().unwrap().push(format!("↔ {display}'s archive spans only {gap} year(s) so far — then-and-now needs more distance. It'll get better every year."));
                done(&studies, &guard);
                return;
            }
            let Some(img) = mind_tools::make_collage(vec![(then_b, None), (now_b, None)]).await else {
                nq.lock().unwrap().push("↔ The pair composition failed — honest miss.".to_string());
                done(&studies, &guard);
                return;
            };
            // One warm grounded line; deterministic fallback.
            let mut caption = format!("↔ {display} — {y_then} → {y_now}");
            let places = match (then_p.is_empty(), now_p.is_empty()) {
                (false, false) if then_p != now_p => format!(" · {} → {}", then_p.split(',').next().unwrap_or(""), now_p.split(',').next().unwrap_or("")),
                _ => String::new(),
            };
            caption.push_str(&places);
            let prompt = format!(
                "One warm line (max 14 words) for a side-by-side photo pair of {display}: left from {y_then}, right from {y_now} ({gap} years apart). Use only these facts. No hashtags, no quotes."
            );
            let cfg = GenerationConfig { max_tokens: 50, ..GenerationConfig::default() };
            if let Ok(r) = inf.chat(vec![ChatMessage::user(&prompt)], cfg).await {
                let line = r.text.trim().trim_matches('"').to_string();
                if line.len() > 8 && line.len() < 120 {
                    caption.push_str(&format!("\n{line}"));
                }
            }
            if let Some(occ) = occasion {
                caption = format!("{occ}\n{caption}");
            }
            pq.lock().unwrap().push((img, caption, target));
            done(&studies, &guard);
        });
        format!("↔ Composing {display}'s then-and-now — the years side by side. Landing in chat.")
    }

    /// Birthday mornings: the pair fires itself, once per person per year.
    pub async fn birthday_thennow_due(&self) -> Option<(String, String)> {
        let today = local_now().date_naive();
        let mmdd = today.format("%m-%d").to_string();
        let year = today.format("%Y").to_string();
        let sent: Vec<String> = self
            .memory
            .profile_get("thennow_sent")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        for p in self.load_people_profiles().await {
            let Some(name) = p.get("name").and_then(|x| x.as_str()) else { continue };
            let dates = p.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            let is_bday = dates.iter().any(|d| {
                d.get("mmdd").and_then(|x| x.as_str()) == Some(mmdd.as_str())
                    && d.get("label").and_then(|x| x.as_str()).map(|l| l.to_lowercase().contains("birthday")).unwrap_or(false)
            });
            if !is_bday {
                continue;
            }
            let key = format!("{}:{}", name.to_lowercase(), year);
            if sent.contains(&key) {
                continue;
            }
            return Some((name.to_string(), key));
        }
        None
    }

    pub async fn birthday_thennow_mark(&self, key: &str) {
        let mut sent: Vec<String> = self
            .memory
            .profile_get("thennow_sent")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        sent.push(key.to_string());
        if sent.len() > 60 {
            let cut = sent.len() - 60;
            sent.drain(..cut);
        }
        let _ = self.memory.profile_set("thennow_sent", &serde_json::to_string(&sent).unwrap_or_default()).await;
    }

    /// Build a growing-up time-lapse for a named person. Walks the archive month by month in a
    /// detached task (minutes of work) and delivers the film through the video queue.
    pub async fn build_growup_reel(&self, who: &str) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some((idx, pid, display)) = self.resolve_face(&sources, who).await else {
            return format!(
                "No photo source knows a face named \"{}\" yet — answer my who-is-this questions (or name them in the photo app) and I can build their reel.",
                who.trim()
            );
        };
        let src_name = sources[idx].name().to_string();
        let vq = self.video_queue.clone();
        let nq = self.notify_queue.clone();
        let disp = display.clone();
        tokio::spawn(async move {
            use chrono::Datelike;
            let sources = mind_tools::PhotoSource::all_from_env();
            let Some(src) = sources.into_iter().find(|s| s.name() == src_name) else { return };
            let mut frames: Vec<(Vec<u8>, (f32, f32, f32, f32), String)> = Vec::new();
            let (mut first_year, mut last_year) = (0i32, 0i32);
            let end = chrono::Utc::now().date_naive();
            let mut cur = chrono::NaiveDate::from_ymd_opt(2014, 1, 1).unwrap_or(end);
            // Month by month, oldest → newest: the source's best (largest) face of this person.
            while cur < end && frames.len() < 132 {
                let nxt = if cur.month() == 12 {
                    chrono::NaiveDate::from_ymd_opt(cur.year() + 1, 1, 1)
                } else {
                    chrono::NaiveDate::from_ymd_opt(cur.year(), cur.month() + 1, 1)
                }
                .unwrap_or(end);
                let cands = src
                    .taken_between(&format!("{cur}T00:00:00.000Z"), &format!("{nxt}T00:00:00.000Z"), &[pid.clone()], 3)
                    .await;
                let mut best: Option<(String, (f32, f32, f32, f32), f32)> = None;
                for a in &cands {
                    if let Some((x1, y1, x2, y2, pxw)) = src.face_box(&a.id, &pid).await {
                        if pxw < 48.0 {
                            continue; // too small to carry a frame
                        }
                        if best.as_ref().map_or(true, |b| pxw > b.2) {
                            best = Some((a.id.clone(), (x1, y1, x2, y2), pxw));
                        }
                    }
                }
                if let Some((aid, bbox, _)) = best {
                    let asset = mind_tools::PhotoAsset { id: aid, date: String::new(), place: String::new(), ..Default::default() };
                    if let Some(bytes) = src.image_bytes(&asset).await {
                        frames.push((bytes, bbox, cur.format("%b %Y").to_string()));
                        if first_year == 0 {
                            first_year = cur.year();
                        }
                        last_year = cur.year();
                    }
                }
                cur = nxt;
            }
            let n = frames.len();
            if n < 6 {
                nq.lock().unwrap().push(format!(
                    "🎞️ I couldn't build {disp}'s reel yet — only {n} clear face-months so far. The face backfill is still indexing 18 months of photos; try again once it finishes."
                ));
                return;
            }
            match mind_tools::face_reel_video(frames).await {
                Some(mp4) => {
                    let cap = format!("🎞️ {disp}, {first_year} → {last_year} — {n} months, one frame each. Watch them grow.");
                    vq.lock().unwrap().push((mp4, cap, None));
                }
                None => {
                    nq.lock().unwrap().push(format!("🎞️ I gathered {n} frames of {disp} but the video encode failed — that's the honest state."));
                }
            }
        });
        format!(
            "🎞️ On it — walking your whole library month by month for {display}'s growing-up reel. It'll land here in a few minutes."
        )
    }

    /// One warm line for a photo memory: local vision reads the scene, the people layer knows who
    /// they ARE (relationships), a small pass fuses them. The substrate-grounding is the moat — a
    /// cloud gallery can say "3 years ago"; it can't say who these people are to you.
    pub(crate) async fn narrate_memory(&self, bytes: &[u8], names: &[String], when: &str, place: &str) -> Option<String> {
        let scene = self
            .analyze_image_bytes(
                bytes.to_vec(),
                "image/jpeg",
                "ONE short line: what's happening in this photo — setting, activity, mood. No names.",
            )
            .await;
        let scene: String = scene.lines().next().unwrap_or("").chars().take(140).collect();
        if scene.len() < 5 {
            return None;
        }
        let store = self.load_people_profiles().await;
        let mut who: Vec<String> = Vec::new();
        for n in names.iter().take(4) {
            let rel = store
                .iter()
                .find(|p| p.get("name").and_then(|x| x.as_str()).map(|s| s.eq_ignore_ascii_case(n)).unwrap_or(false))
                .and_then(|p| p.get("relationship").and_then(|x| x.as_str()))
                .unwrap_or("");
            who.push(if rel.is_empty() { n.clone() } else { format!("{n} (his {rel})") });
        }
        let prompt = format!(
            "Write ONE warm, personal sentence (max 22 words) captioning a photo memory for the user. Taken: {when}{}. People in it: {}. Scene: {scene}. Ground it ONLY in these facts — no invented details, no emoji, no preamble.",
            if place.is_empty() { String::new() } else { format!(" in {place}") },
            if who.is_empty() { "not identified".to_string() } else { who.join(", ") },
        );
        let cfg = GenerationConfig { max_tokens: 90, ..GenerationConfig::default() };
        self.inference
            .chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg)
            .await
            .ok()
            .map(|r| r.text.trim().trim_matches('"').chars().take(180).collect::<String>())
            .filter(|t| t.len() > 10)
    }

    /// Drain reel videos queued for delivery: (bytes, caption, target chat — None = the primary).
    pub fn take_outbound_videos(&self) -> Vec<(Vec<u8>, String, Option<i64>)> {
        std::mem::take(&mut *self.video_queue.lock().unwrap())
    }

    /// ---------- CREATIVE STUDIO ----------
    /// Free-form photo creation: "collage of Brishti from different parties and traditional
    /// outfits", "morning vibe picture of us with a unique caption". Intent is parsed, photos are
    /// selected for DIVERSITY (semantic theme + person filters + one-per-month spread), composed
    /// (face-centered collage grid or single), and captioned with grounded, non-generic words.
    pub async fn photo_create(&self, request: &str) -> String {
        self.photo_create_for(request, None, None).await
    }

    /// Studio with explicit DELIVERY TARGET and SPEAKER ("me"/"us" resolve around the speaker).
    pub async fn photo_create_for(&self, request: &str, target: Option<i64>, speaker: Option<&str>) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some(src_idx) = sources.iter().position(|s| s.knows_people()) else {
            return "No face-aware photo source is connected — I can't compose from the library.".to_string();
        };
        // Parse the ask into structured intent.
        let parse_prompt = format!(
            "User request: \"{}\". Extract photo-creation intent. Output ONLY JSON: {{\"people\":[\"<names mentioned; 'me' for the user themself; 'us' for user+partner>\"],\"theme\":\"<subject/style terms like party traditional outfit / morning cozy light / beach>\",\"format\":\"<collage or single>\",\"count\":<4, 6 or 9 for collages; 1 for single>,\"caption_mood\":\"<warm/funny/poetic/romantic>\"}}",
            request.trim()
        );
        let cfg = GenerationConfig { max_tokens: 200, ..GenerationConfig::default() };
        let v = self
            .inference
            .chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&parse_prompt)], cfg)
            .await
            .map(|r| parse_json_obj(&r.text))
            .unwrap_or_default();
        let theme = v.get("theme").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        let format_kind = v.get("format").and_then(|x| x.as_str()).unwrap_or("collage").trim().to_lowercase();
        let count = v.get("count").and_then(|x| x.as_u64()).unwrap_or(6) as usize;
        let caption_mood = v.get("caption_mood").and_then(|x| x.as_str()).unwrap_or("warm").trim().to_string();
        // Resolve "me"/"us"/names to faces via the people layer + face-aware source.
        let self_name = match speaker {
            Some(sp) => sp.to_string(),
            None => self.memory.profile_get("name").await.ok().flatten().unwrap_or_default(),
        };
        let spouse = self
            .load_people_profiles()
            .await
            .iter()
            .find(|p| p.get("relationship").and_then(|x| x.as_str()).map(|r| r.contains("wife") || r.contains("husband")).unwrap_or(false))
            .and_then(|p| p.get("name").and_then(|x| x.as_str()).map(String::from))
            .unwrap_or_default();
        let mut names: Vec<String> = Vec::new();
        for p in v.get("people").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let Some(n) = p.as_str() else { continue };
            match n.trim().to_lowercase().as_str() {
                "me" | "myself" | "i" => {
                    if !self_name.is_empty() {
                        names.push(self_name.clone());
                    }
                }
                "us" | "we" => {
                    if !self_name.is_empty() {
                        names.push(self_name.clone());
                    }
                    if !spouse.is_empty() {
                        names.push(spouse.clone());
                    }
                }
                other if other.len() > 1 => names.push(n.trim().to_string()),
                _ => {}
            }
        }
        names.dedup();
        let mut person_ids: Vec<String> = Vec::new();
        let mut resolved: Vec<String> = Vec::new();
        for n in &names {
            if let Some((_, pid, disp)) = self.resolve_face(&sources, n).await {
                person_ids.push(pid);
                resolved.push(disp);
            }
        }
        if person_ids.is_empty() && !names.is_empty() {
            return format!(
                "I couldn't match {} to any face in the library — name them in the photo app (or answer my who-is-this questions) and I can compose with them.",
                names.join("/")
            );
        }
        let people_desc = if resolved.is_empty() { "the family library".to_string() } else { resolved.join(" and ") };
        // Compose in the background — selection + composition + caption take a minute.
        let guard = format!("create:{}", request.trim().to_lowercase().chars().take(48).collect::<String>());
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return "Already composing that one — it lands here shortly.".to_string();
        }
        let src_name = sources[src_idx].name().to_string();
        let pq = self.photo_queue.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let inference = self.inference.clone();
        let persona = self.persona.clone();
        let theme2 = theme.clone();
        let desc2 = people_desc.clone();
        let fmt2 = format_kind.clone();
        tokio::spawn(async move {
            match studio_task(src_name, person_ids, desc2, theme2, fmt2, count, caption_mood, inference, persona).await {
                Ok((img, caption)) => {
                    pq.lock().unwrap().push((img, caption, target));
                }
                Err(msg) => {
                    nq.lock().unwrap().push(format!("🎨 Couldn't compose it: {msg}"));
                }
            }
            studies.lock().unwrap().remove(&guard);
        });
        format!(
            "🎨 Composing — {} of {people_desc}{} — it lands here in a minute or two.",
            if format_kind == "single" { "a picture".to_string() } else { format!("a collage") },
            if theme.is_empty() { String::new() } else { format!(" ({theme})") }
        )
    }

    /// ---------- TASTE DISTRIBUTIONS ----------
    /// From reads to STATISTICS: incremental batches accumulate categorical counts per person;
    /// frequencies become preference PROBABILITIES with sample-size confidence; milestone beliefs
    /// carry the probability in their weight. Heavy pass runs detached — instant ack here.
    pub async fn taste_study(&self, who: &str, batch: usize) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some((i, pid, disp)) = self.resolve_face(&sources, who).await else {
            return format!("No photo source knows \"{}\" by face yet.", who.trim());
        };
        if mind_tools::VisionClient::from_env().is_none() {
            return "No vision model configured — can't study photos.".to_string();
        }
        let acc: serde_json::Value = self
            .memory
            .profile_get(&format!("tastes:{}", disp.to_lowercase()))
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({ "seen": [], "counts": {}, "total": 0 }));
        let total0 = acc["total"].as_u64().unwrap_or(0);
        let guard = format!("tastes:{}", disp.to_lowercase());
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return format!("A taste study of {disp} is already running — the update lands here shortly.");
        }
        let (src_name, pid2, disp2) = (sources[i].name().to_string(), pid.clone(), disp.clone());
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let batch = batch.clamp(10, 60);
        tokio::spawn(async move {
            if let Some(t) = taste_task(src_name, pid2, disp2, batch, mem).await {
                nq.lock().unwrap().push(t);
            }
            studies.lock().unwrap().remove(&guard);
        });
        if total0 > 0 {
            format!(
                "{}\n\n📈 Studying {batch} more in the background — updated numbers will follow.",
                render_tastes(&acc, &disp)
            )
        } else {
            format!("📊 First taste study of {disp} started ({batch} photos, local vision, background) — the distribution lands here when done.")
        }
    }

    /// ---------- OBJECT INVENTORY ----------
    /// Structured possession detection (watch/handbag/saree/... with counts + variants + a
    /// NEVER-SEEN gap list). Heavy pass runs detached — instant ack, catalog arrives when done.
    pub async fn person_inventory(&self, who: &str) -> String {
        let key = format!("closet:{}", who.trim().to_lowercase());
        if let Ok(Some(prev)) = self.memory.profile_get(&key).await {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&prev) {
                if chrono::Utc::now().timestamp_millis() - v["ts"].as_i64().unwrap_or(0) < 30 * 86_400_000 {
                    if let Some(t) = v["text"].as_str() {
                        return format!("{t}\n\n(cached study — `closet {} fresh` to redo)", who.trim());
                    }
                }
            }
        }
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some((i, pid, disp)) = self.resolve_face(&sources, who).await else {
            return format!("No photo source knows \"{}\" by face yet.", who.trim());
        };
        if mind_tools::VisionClient::from_env().is_none() {
            return "No vision model configured — can't read the photos.".to_string();
        }
        let guard = format!("closet:{}", disp.to_lowercase());
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return format!("Already inventorying {disp}'s photos — the catalog lands here shortly.");
        }
        let (src_name, pid2, disp2) = (sources[i].name().to_string(), pid.clone(), disp.clone());
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        tokio::spawn(async move {
            if let Some(t) = inventory_task(src_name, pid2, disp2, mem).await {
                nq.lock().unwrap().push(t);
            }
            studies.lock().unwrap().remove(&guard);
        });
        format!("👗 Studying {disp}'s photos for a structured object inventory (background, local vision) — the catalog lands here when done.")
    }

    /// ---------- GIFT INTELLIGENCE ----------
    /// The photo lane's practical payoff: study a person's actual photos for what they OWN (never
    /// re-gift), their STYLE (colors, materials, aesthetic), and what's visibly MISSING that would
    /// complement it — fused with people-layer facts, distilled into buyable ideas that chain
    /// straight into the deal-finder. Cached ~30 days per person (12 vision reads aren't free).
    pub async fn gift_intel(&self, who: &str) -> String {
        let name_key = format!("gift_intel:{}", who.trim().to_lowercase());
        if let Ok(Some(prev)) = self.memory.profile_get(&name_key).await {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&prev) {
                let ts = v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0);
                if chrono::Utc::now().timestamp_millis() - ts < 30 * 86_400_000 {
                    if let Some(t) = v.get("text").and_then(|x| x.as_str()) {
                        return format!("{t}\n\n(from my photo study — say `gift {} fresh` to redo it)", who.trim());
                    }
                }
            }
        }
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some((i, pid, disp)) = self.resolve_face(&sources, who).await else {
            return format!("No photo source knows \"{}\" by face yet — I can't study their photos for gift ideas.", who.trim());
        };
        if mind_tools::VisionClient::from_env().is_none() {
            return "No vision model configured — I can't read the photos.".to_string();
        }
        // Substrate context gathered up front (the detached task can't call &self methods).
        let store = self.load_people_profiles().await;
        let known = store
            .iter()
            .find(|p| p.get("name").and_then(|x| x.as_str()).map(|s| s.eq_ignore_ascii_case(&disp)).unwrap_or(false))
            .map(|p| {
                let rel = p.get("relationship").and_then(|x| x.as_str()).unwrap_or("");
                let facts: Vec<String> = p
                    .get("facts")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|f| f.as_str().map(String::from)).take(8).collect())
                    .unwrap_or_default();
                format!("{rel}. {}", facts.join("; "))
            })
            .unwrap_or_default();
        let closet_note = self
            .memory
            .profile_get(&format!("closet:{}", disp.to_lowercase()))
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("summary").and_then(|x| x.as_str()).map(String::from))
            .unwrap_or_default();
        let tastes_note = self
            .memory
            .profile_get(&format!("tastes:{}", disp.to_lowercase()))
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .filter(|v| v["total"].as_u64().unwrap_or(0) >= 20)
            .map(|v| render_tastes(&v, &disp))
            .unwrap_or_default();
        let guard = format!("gift:{}", disp.to_lowercase());
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return format!("Already studying {disp}'s photos for gifts — results land here shortly.");
        }
        let (src_name, pid2, disp2) = (sources[i].name().to_string(), pid.clone(), disp.clone());
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let inference = self.inference.clone();
        let persona = self.persona.clone();
        tokio::spawn(async move {
            if let Some(t) = gift_task(src_name, pid2, disp2, known, closet_note, tastes_note, mem, inference, persona).await {
                nq.lock().unwrap().push(t);
            }
            studies.lock().unwrap().remove(&guard);
        });
        format!("📸 Studying {disp}'s photos for gift intelligence in the background — results land here shortly.")
    }

    /// Proactive gift scout gate: at most one study per period, only when photo sources exist.
    pub async fn gift_scout_due(&self) -> bool {
        if !mind_tools::PhotoSource::all_from_env().iter().any(|s| s.knows_people()) {
            return false;
        }
        let period_ms: i64 = std::env::var("YM_GIFTSCOUT_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(86_400) * 1000;
        let period_ms = (period_ms as f64 * self.domain_pace("gift").await) as i64;
        let last: i64 = self.memory.profile_get("gift_scout_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

    /// Someone's day within 25 days and no fresh study → run gift intelligence unprompted. The
    /// companion move: the ideas arrive BEFORE the user thinks to ask, while there's shipping time.
    pub async fn gift_scout_run(&self) -> Option<String> {
        let _ = self
            .memory
            .profile_set("gift_scout_last", &chrono::Utc::now().timestamp_millis().to_string())
            .await;
        let today = local_now();
        let store = self.load_people_profiles().await;
        for p in &store {
            let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            for d in p.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                let (Some(mmdd), Some(label)) = (
                    d.get("mmdd").and_then(|x| x.as_str()),
                    d.get("label").and_then(|x| x.as_str()),
                ) else {
                    continue;
                };
                let Some(days) = days_until_mmdd(mmdd, &today) else { continue };
                if !(0..=25).contains(&days) {
                    continue;
                }
                // Already studied recently? gift_intel reuses its cache — only surface NEW studies.
                let key = format!("gift_intel:{}", name.to_lowercase());
                if let Ok(Some(prev)) = self.memory.profile_get(&key).await {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&prev) {
                        if chrono::Utc::now().timestamp_millis() - v.get("ts").and_then(|x| x.as_i64()).unwrap_or(0) < 30 * 86_400_000 {
                            continue;
                        }
                    }
                }
                let kick = self.gift_intel(name).await;
                if kick.starts_with("No photo source") || kick.starts_with("No vision") {
                    continue;
                }
                if kick.starts_with("🎁") {
                    self.ledger_sent("gift", &format!("proactive gift intel for {name}")).await;
                return Some(format!("{name}'s {label} is in {days} day(s) — here's what their photos say:\n\n{kick}"));
                }
                return Some(format!("{name}'s {label} is in {days} day(s) — I'm studying their photos now; gift ideas will follow shortly."));
            }
        }
        None
    }

}
