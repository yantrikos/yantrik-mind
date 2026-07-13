//! Photo core -- analysis, face resolve/learn, find+send, sessions, anchors, fb sync. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// A photo turn from Telegram: vision-analyze, and if it's a RECEIPT, auto-log the expense
    /// into the month's budget (the vision lane's first money job). The receipt marker line is
    /// machine-parsed then stripped from what the user sees.
    pub async fn analyze_photo_turn(&self, image: Vec<u8>, caption: &str) -> String {
        // Remember the last photo received — "enhance it" style follow-ups act on it.
        *self.last_photo.lock().unwrap() = Some(image.clone());
        // OUR face recognition first: names from our own gallery ground everything downstream.
        let (who, unknown_faces) = self.identify_faces_in(&image).await;
        let who_line = if who.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = who.iter().map(|(n, _)| n.clone()).collect();
            format!(
                "

(People in this photo, from MY OWN face memory — treat as ground truth: {}{})",
                names.join(", "),
                if unknown_faces > 0 { format!(" + {unknown_faces} I don't recognize") } else { String::new() }
            )
        };
        // ENHANCEMENT LANE: an edit-intent caption returns the edited photo, not a description.
        if let Some(mode) = enhancement_mode(caption) {
            return match mind_tools::enhance_photo(image, mode).await {
                Some(out) => {
                    self.photo_queue.lock().unwrap().push((out, format!("\u{2728} enhanced ({mode})"), None));
                    format!("\u{2728} Done — enhanced it ({mode}), sending it back now.")
                }
                None => "I tried to enhance it but the edit failed on this image — honest miss.".to_string(),
            };
        }
        let q = if caption.trim().is_empty() {
            format!("Describe what's in this image and anything the user would want to know from it. Be concrete.{who_line}")
        } else {
            format!("{}{who_line}", caption.trim())
        };
        let prompt = format!(
            "{q}\n\nIf (and only if) this image is a RECEIPT or BILL, also end your reply with exactly one line:\nRECEIPT: <total amount, digits only> | <merchant> | <one-word category like groceries/dining/gas/shopping>"
        );
        let mut reply = self.analyze_image_bytes(image, "image/jpeg", &prompt).await;
        if !who.is_empty() {
            let names: Vec<String> = who.iter().map(|(n, s)| format!("{n} ({:.0}%)", s * 100.0)).collect();
            reply = format!("👥 I recognize: {}

{reply}", names.join(", "));
        }
        if let Some(pos) = reply.rfind("RECEIPT:") {
            let line = reply[pos..].lines().next().unwrap_or("").to_string();
            let parts: Vec<&str> = line.trim_start_matches("RECEIPT:").split('|').map(str::trim).collect();
            if parts.len() >= 3 {
                let amt: f64 = parts[0].chars().filter(|c| c.is_ascii_digit() || *c == '.').collect::<String>().parse().unwrap_or(0.0);
                let category = parts[2].to_lowercase();
                if amt > 0.0 && !category.is_empty() {
                    let logged = self.expense_log(&format!("{amt} {category}")).await;
                    reply = format!("{}\n\n💰 {} ({})", reply[..pos].trim_end(), logged, parts[1]);
                }
            }
        }
        reply
    }

    /// Analyze an image (a photo the user sent, or a page screenshot) with the vision model.    /// Analyze an image (a photo the user sent, or a page screenshot) with the vision model.
    /// Honest by construction: no configured model or a failed call says so — never a guessed caption.
    pub async fn analyze_image_bytes(&self, image: Vec<u8>, mime: &str, question: &str) -> String {
        let Some(v) = mind_tools::VisionClient::from_env() else {
            return "I can't look at images yet — no vision model is configured (YM_VISION_MODEL + its provider key). That's the honest state.".to_string();
        };
        let q = if question.trim().is_empty() {
            "Describe what's in this image and anything the user would want to know from it. Be concrete."
        } else {
            question.trim()
        };
        let prompt = format!("{q}\n\nBe factual: read text, prices, and numbers exactly as shown; say plainly if something is unreadable.");
        match v.analyze(&prompt, image, mime).await {
            Ok(t) => t,
            Err(e) => format!("I tried to look at it, but the vision call failed ({e}) — so I genuinely haven't seen it."),
        }
    }

    /// SEE a web page: render it in the real browser, screenshot, vision-analyze. Sees what text
    /// extraction can't — layouts, images, JS-only content, some bot-walled pages that still render.
    pub async fn see_page(&self, url: &str, question: &str) -> String {
        let url = url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return "Give me a full URL to look at, e.g. `ym see https://example.com what's the price?`".to_string();
        }
        let Some(shot) = mind_tools::screenshot_page(url).await else {
            return format!("I couldn't render {url} for a screenshot (blocked or the browser failed) — no picture, so no analysis. That's the honest state.");
        };
        let q = if question.trim().is_empty() {
            "What does this page show? Summarize the content, key numbers/prices, and anything notable."
        } else {
            question
        };
        self.analyze_image_bytes(shot, "image/jpeg", q).await
    }

    /// FACEBOOK SYNC — the "know me" lane, read-only over the user's OWN profile. Profile facts +
    /// likes (interest mining) + events (straight onto the calendar spine) become typed beliefs and
    /// profile entries, provenance "facebook". Deterministic extraction; refreshed daily by the
    /// poll loop; warns when the token nears expiry.
    pub async fn fb_sync(&self) -> String {
        let Some(fb) = mind_tools::FbClient::from_env() else {
            return "Facebook isn't connected (FB_USER_TOKEN not set) — that's the honest state.".to_string();
        };
        let mut lines: Vec<String> = Vec::new();
        let mut learned = 0usize;
        // 1. Profile facts → beliefs.
        if let Ok(p) = fb.profile().await {
            if let Some(b) = p.get("birthday").and_then(|x| x.as_str()) {
                let _ = self.memory.remember_as_belief(BeliefAssertion {
                    statement: format!("Pranab's own birthday is {b} (from his Facebook profile)"),
                    polarity: 1.0, weight: 1.0, source_event: Some("facebook".into()), provenance: "facebook".into(),
                }).await;
                learned += 1;
            }
            if let Some(h) = p.get("hometown").and_then(|x| x.get("name")).and_then(|x| x.as_str()) {
                let _ = self.memory.remember_as_belief(BeliefAssertion {
                    statement: format!("Pranab's hometown is {h}"),
                    polarity: 1.0, weight: 0.9, source_event: Some("facebook".into()), provenance: "facebook".into(),
                }).await;
                learned += 1;
            }
        }
        // 2. Likes → interest beliefs (and the Brishti creator-page enrichment).
        if let Ok(l) = fb.likes(25).await {
            let mut names: Vec<String> = Vec::new();
            for like in l.get("data").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                let (Some(name), cat) = (
                    like.get("name").and_then(|x| x.as_str()),
                    like.get("category").and_then(|x| x.as_str()).unwrap_or(""),
                ) else { continue };
                names.push(name.trim().to_string());
                let _ = self.memory.remember_as_belief(BeliefAssertion {
                    statement: format!("Pranab follows \"{}\" on Facebook ({cat})", name.trim()),
                    polarity: 1.0, weight: 0.7, source_event: Some("facebook".into()), provenance: "facebook".into(),
                }).await;
                learned += 1;
            }
            // The wife's creator page, pinned to her profile as a fact.
            if names.iter().any(|n| n.to_lowercase().contains("brishti")) {
                let mut store = self.load_people_profiles().await;
                if let Some(idx) = store.iter().position(|p| {
                    p.get("relationship").and_then(|x| x.as_str()).map(|r| r.contains("wife")).unwrap_or(false)
                }) {
                    let mut facts = store[idx].get("facts").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                    let fact = "Runs the \"World Of Brishti\" digital-creator page on Facebook";
                    if !facts.iter().any(|f| f.as_str().map(|s| s.contains("World Of Brishti")).unwrap_or(false)) {
                        facts.push(serde_json::json!(fact));
                        store[idx]["facts"] = serde_json::json!(facts);
                        self.save_people_profiles(&store).await;
                        lines.push("pinned Brishti's creator page to her profile".to_string());
                    }
                }
            }
            lines.push(format!("{} liked pages mined for interests", names.len()));
        }
        // 3. Events → the calendar spine (source "fb"; replaced wholesale each sync).
        if let Ok(ev) = fb.events(10).await {
            let mut evs: Vec<serde_json::Value> = self
                .load_calendar()
                .await
                .into_iter()
                .filter(|e| e.get("source").and_then(|x| x.as_str()) != Some("fb"))
                .collect();
            let mut n = 0;
            for e in ev.get("data").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                let (Some(name), Some(start)) = (
                    e.get("name").and_then(|x| x.as_str()),
                    e.get("start_time").and_then(|x| x.as_str()),
                ) else { continue };
                if let Ok(t) = chrono::DateTime::parse_from_str(start, "%Y-%m-%dT%H:%M:%S%z") {
                    evs.push(serde_json::json!({
                        "id": t.timestamp_millis(), "title": format!("{name} (FB)"),
                        "when_ms": t.timestamp_millis(), "source": "fb",
                    }));
                    n += 1;
                }
            }
            self.save_calendar(&evs).await;
            if n > 0 {
                lines.push(format!("{n} Facebook event(s) on the calendar"));
            }
        }
        // 4. Post cadence — noted, not over-read (sparse posters are sparse).
        if let Ok(p) = fb.posts(10).await {
            let n = p.get("data").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
            lines.push(format!("{n} recent posts seen"));
        }
        // 5. Token health — warn while there's still time to re-mint.
        if let Some(days) = fb.days_to_expiry().await {
            if days < 7 {
                lines.push(format!("⚠️ Facebook token expires in {days} day(s) — re-mint it soon"));
            }
        }
        let _ = self.memory.profile_set("fb_last_sync", &chrono::Utc::now().timestamp_millis().to_string()).await;
        format!("📘 Facebook synced — {learned} facts learned. {}", lines.join("; "))
    }

    /// Learn preference/pattern beliefs from photos. `who` routes to face-aware sources (Immich's
    /// named faces + our own ask-who-is-who face_names map); None sweeps recent photos across every
    /// configured source. All vision is LOCAL (zero cloud).
    pub async fn photo_patterns(&self, source_filter: Option<&str>, who: Option<&str>, limit: usize) -> String {
        let sources: Vec<mind_tools::PhotoSource> = mind_tools::PhotoSource::all_from_env()
            .into_iter()
            .filter(|s| source_filter.map(|f| s.name() == f).unwrap_or(true))
            .collect();
        if sources.is_empty() {
            return "No photo source is connected (Immich / Facebook) — honest state.".to_string();
        }
        if mind_tools::VisionClient::from_env().is_none() {
            return "No vision model configured (YM_VISION_MODEL) — can't read the photos.".to_string();
        }
        // Gather: per-person via a face-aware source, or a recent sweep across all of them.
        let mut picked: Vec<(usize, mind_tools::PhotoAsset)> = Vec::new();
        let mut subject: Option<String> = None;
        if let Some(w) = who {
            let Some((i, pid, display)) = self.resolve_face(&sources, w).await else {
                let mut known: Vec<String> = Vec::new();
                for src in sources.iter().filter(|s| s.knows_people()) {
                    known.extend(src.list_people().await.into_iter().filter(|p| !p.name.is_empty()).map(|p| p.name));
                }
                known.extend(self.face_names().await.values().cloned());
                known.sort();
                known.dedup();
                return format!(
                    "No photo source knows a face named \"{}\". People I can read: {}.",
                    w.trim(),
                    if known.is_empty() {
                        "none named yet — answer my who-is-this questions to teach me".to_string()
                    } else {
                        known.join(", ")
                    }
                );
            };
            for a in sources[i].assets_of_person(&pid, limit.max(4)).await {
                picked.push((i, a));
            }
            subject = Some(display);
        } else {
            for (i, src) in sources.iter().enumerate() {
                for a in src.recent_assets(limit.max(4)).await {
                    picked.push((i, a));
                }
            }
        }
        if picked.is_empty() {
            return "The photo sources returned nothing to read.".to_string();
        }
        // Describe each (LOCAL vision), folding EXIF places in.
        let q = if subject.is_some() {
            "In ONE short line: the setting, the activity, and the vibe/aesthetic. Do not guess names."
        } else {
            "In ONE line: the setting, the activity, who's in it (solo / couple / family / group — do NOT guess names), and the overall vibe/aesthetic."
        };
        let mut descs: Vec<String> = Vec::new();
        let mut places: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (i, a) in picked.iter().filter(|(_, a)| !mind_tools::is_screenish(a)).take(limit.max(4)) {
            if !a.place.is_empty() {
                places.insert(a.place.clone());
            }
            let Some(bytes) = sources[*i].image_bytes(a).await else { continue };
            let d = self.analyze_image_bytes(bytes, "image/jpeg", q).await;
            let d1: String = d.lines().next().unwrap_or("").chars().take(160).collect();
            if d1.len() > 5 {
                // Ground each line in WHO is in the photo (saved face data) — "Brishti + Aadrisha,
                // porch, autumn" teaches far more than "two people outside".
                let (who, _) = sources[*i].people_in(&a.id).await;
                let mut line = String::new();
                if !a.date.is_empty() {
                    line.push_str(&format!("[{}] ", a.date));
                }
                if !who.is_empty() {
                    line.push_str(&format!("({}) ", who.join(" + ")));
                }
                line.push_str(&d1);
                descs.push(line);
            }
        }
        if descs.is_empty() {
            return "I reached the photo list but couldn't analyze any images.".to_string();
        }
        let place_line = if places.is_empty() {
            String::new()
        } else {
            format!("\nPlaces (from photo GPS): {}", places.iter().take(8).cloned().collect::<Vec<_>>().join("; "))
        };
        let joined = descs.join("\n");
        let prompt = match &subject {
            Some(nm) => format!(
                "These are one-line reads of photos of {nm} (a real person in the user's life). Infer 4-6 concrete, standalone preferences/patterns about them: what they enjoy, settings/activities that recur, their style/aesthetic. One per line, no preamble, no invented names.\n\n=== PHOTOS ===\n{joined}{place_line}"
            ),
            None => format!(
                "These are one-line descriptions of the user's recent photos across their libraries. Infer RECURRING patterns: favorite settings, activities, travel/aesthetic tastes, and the kinds of moments they capture. Where a couple or family recurs, note shared preferences (but never invent identities). Output 4-6 concrete statements, one per line, no preamble.\n\n=== PHOTOS ===\n{joined}{place_line}"
            ),
        };
        let cfg = GenerationConfig { max_tokens: 500, ..GenerationConfig::default() };
        let insights = match self.inference.chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg).await {
            Ok(r) => r.text.trim().to_string(),
            Err(e) => return format!("Read {} photos but couldn't distill patterns ({e}).", descs.len()),
        };
        // Store as beliefs; per-person reads also enrich that person's people-layer profile.
        let mut stored = 0;
        let mut new_facts: Vec<String> = Vec::new();
        for line in insights.lines() {
            let stmt = line.trim().trim_start_matches(['-', '*', '•', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '.', ')']).trim();
            if stmt.len() < 12 {
                continue;
            }
            let statement = match &subject {
                Some(nm) => format!("{nm} (photo pattern): {stmt}"),
                None => format!("{stmt} (pattern from the photo libraries)"),
            };
            let _ = self.memory.remember_as_belief(BeliefAssertion {
                statement,
                polarity: 1.0, weight: 0.6, source_event: Some("photos".into()), provenance: "photos".into(),
            }).await;
            new_facts.push(stmt.to_string());
            stored += 1;
        }
        if let Some(nm) = &subject {
            let ql = nm.to_lowercase();
            let mut store = self.load_people_profiles().await;
            if let Some(p) = store.iter_mut().find(|p| person_matches(p, &ql)) {
                let mut facts = p.get("facts").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                for nf in new_facts.iter().take(2) {
                    if !facts.iter().any(|f| f.as_str().map(|s| s.contains(nf.as_str())).unwrap_or(false)) {
                        facts.push(serde_json::json!(format!("{nf} (from photos)")));
                    }
                }
                p["facts"] = serde_json::json!(facts);
                self.save_people_profiles(&store).await;
            }
        }
        match &subject {
            Some(nm) => format!("📸 {nm} — read {} of their photos → {stored} pattern(s):\n\n{insights}{place_line}", descs.len()),
            None => format!("📸 Read {} recent photos across your libraries → {stored} pattern(s):\n\n{insights}{place_line}", descs.len()),
        }
    }

    /// Resolve a person's name to (source idx, person id, display name). Source-side names first
    /// (Immich's own labels), then our learned face_names map (ask-who-is-who answers) — a face YOU
    /// named in chat is as queryable as one named in Immich.
    pub(crate) async fn resolve_face(&self, sources: &[mind_tools::PhotoSource], who: &str) -> Option<(usize, String, String)> {
        let want = who.trim().to_lowercase();
        if want.is_empty() {
            return None;
        }
        let fm = self.face_names().await;
        for (i, src) in sources.iter().enumerate() {
            if !src.knows_people() {
                continue;
            }
            for p in src.list_people().await {
                let display = if p.name.is_empty() {
                    fm.get(&format!("{}:{}", src.name(), p.id)).cloned().unwrap_or_default()
                } else {
                    p.name.clone()
                };
                if !display.is_empty() && display.to_lowercase() == want {
                    return Some((i, p.id, display));
                }
            }
        }
        None
    }

    /// PHOTO RETRIEVAL — "send me a photo of Brishti in a red saree" → find the actual image and
    /// ship it to the home channel. Person terms resolve via face-aware sources; extra descriptors
    /// are vision-screened (LOCAL) against candidates so the photo genuinely matches. Honest when
    /// nothing matches — never sends a wrong photo claiming it fits.
    pub async fn photo_find_and_send(&self, query: &str) -> String {
        self.photo_find_and_send_for(query, None, None).await
    }

    /// Retrieval with explicit DELIVERY TARGET and SPEAKER ("me" = the speaker, not the primary) —
    /// the member-facing variant. The photo library is a shared family resource; targeting keeps
    /// each person's results in their own chat.
    pub async fn photo_find_and_send_for(&self, query: &str, target: Option<i64>, speaker: Option<&str>) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        if sources.is_empty() {
            return "No photo source is connected (Immich / Facebook) — I have no library to pull from.".to_string();
        }
        let ql = query.trim().trim_end_matches(['?', '!', '.']).to_lowercase();
        // "old/early/childhood/years ago" asks want the ARCHIVE, not the camera roll: widen the
        // pool and walk it oldest-first.
        let wants_old = ["old", "older", "early", "earliest", "childhood", "years ago", "long ago", "back then"]
            .iter()
            .any(|w| ql.contains(w));
        let fm = self.face_names().await;
        // Collect EVERY known person the ask mentions ("me and Brishti at the beach") — and ground
        // our/us/we/my (+ "of me"-style phrases, NOT the "show me" filler) to the user's own face,
        // so "our wedding" filters to photos he's actually in.
        let self_name = match speaker {
            Some(sp) => sp.to_lowercase(),
            None => self.memory.profile_get("name").await.ok().flatten().unwrap_or_default().to_lowercase(),
        };
        let wants_self = ["our", "us", "we", "my"].iter().any(|w| ql.split_whitespace().any(|t| t == *w))
            || ["me and", "and me", "of me", "with me"].iter().any(|p| ql.contains(p));
        let mut src_idx: Option<usize> = None;
        let mut person_ids: Vec<String> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for (i, src) in sources.iter().enumerate() {
            if !src.knows_people() {
                continue;
            }
            for p in src.list_people().await {
                let nm = if p.name.is_empty() {
                    fm.get(&format!("{}:{}", src.name(), p.id)).cloned().unwrap_or_default()
                } else {
                    p.name.clone()
                };
                if nm.len() < 2 {
                    continue;
                }
                let nml = nm.to_lowercase();
                if ql.contains(&nml) {
                    src_idx = Some(i);
                    person_ids.push(p.id.clone());
                    names.push(nm);
                } else if wants_self && !self_name.is_empty() && nml == self_name {
                    src_idx = Some(i);
                    person_ids.push(p.id.clone());
                    names.push(nm);
                }
            }
            if src_idx.is_some() {
                break; // person filters can't span sources
            }
        }
        let label = names.join(" + ");
        // Descriptor = the ask minus person names and filler words.
        let mut restq = ql.clone();
        for nm in &names {
            restq = restq.replace(&nm.to_lowercase(), " ");
        }
        let stop = [
            "photo", "photos", "picture", "pictures", "pic", "pics", "image", "images", "snap",
            "of", "the", "a", "an", "and", "me", "my", "your", "our", "us", "we", "in", "at", "on",
            "with", "from", "send", "show", "share", "find", "get", "pull", "please", "can",
            "could", "you", "some", "any", "one", "wearing", "her", "his", "their",
            "more", "another", "again", "different", "else", "new", "boss", "need", "i",
            "old", "older", "early", "earliest", "childhood", "ago", "long", "back", "then",
        ];
        let desc = restq.split_whitespace().filter(|w| !stop.contains(w)).collect::<Vec<_>>().join(" ");
        // Candidates, best lane first: SEMANTIC search (CLIP over the whole archive) when the source
        // has it and there's a descriptor; person-filtered metadata otherwise; recent sweep as floor.
        let idx = src_idx.unwrap_or(0);
        let searched = !desc.is_empty() && sources[idx].supports_search();
        let cands: Vec<mind_tools::PhotoAsset> = if searched {
            sources[idx].search(&desc, &person_ids, if wants_old { 30 } else { 12 }).await
        } else if !person_ids.is_empty() {
            sources[idx].assets_of_people(&person_ids, if wants_old { 80 } else { 24 }, wants_old).await
        } else {
            sources[idx].recent_assets(if wants_old { 80 } else { 24 }).await
        };
        if cands.is_empty() {
            return format!(
                "I searched the library{}{} and nothing came back — honestly empty-handed.",
                if label.is_empty() { String::new() } else { format!(" for {label}") },
                if desc.is_empty() { String::new() } else { format!(" (\"{desc}\")") }
            );
        }
        // "One more" must mean a DIFFERENT photo: prefer candidates not sent before (rolling 200).
        let sent = self.photos_sent().await;
        // Screenshots/app-captures never belong in a memory answer — real camera photos only
        // (unless literally nothing else matched, in which case honesty wins below).
        let real: Vec<mind_tools::PhotoAsset> = cands.iter().filter(|a| !mind_tools::is_screenish(a)).cloned().collect();
        let cands = if real.is_empty() { cands } else { real };
        let unseen: Vec<mind_tools::PhotoAsset> = cands.iter().filter(|a| !sent.contains(&a.id)).cloned().collect();
        let mut cands = if unseen.is_empty() { cands } else { unseen };
        if wants_old {
            // Oldest first — dated assets ascending, undated last. The no-descriptor pick and the
            // verify walk both start from the archive's far end.
            cands.sort_by(|a, b| match (a.date.is_empty(), b.date.is_empty()) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => a.date.cmp(&b.date),
            });
        }
        // DATE-ANCHOR MEMORY (Pranab's insight): successful finds taught us WHEN things happened
        // ("wedding" → 2016-03). Widen the pool with that date window so "another one" has real
        // neighbors to draw from, not just the same CLIP top hits.
        let mut cands = cands;
        if !desc.is_empty() {
            let anchors = self.photo_anchors().await;
            for w in desc.split_whitespace().filter(|w| w.len() >= 4) {
                if let Some(dt) = anchors.get(w).and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()) {
                    let from = dt - chrono::Duration::days(6);
                    let to = dt + chrono::Duration::days(7);
                    for a in sources[idx].taken_between(&format!("{from}T00:00:00.000Z"), &format!("{to}T00:00:00.000Z"), &person_ids, 12).await {
                        if !cands.iter().any(|c| c.id == a.id) {
                            cands.push(a);
                        }
                    }
                }
            }
        }
        // Pick + verify. Semantic hits are already meaning-ranked — vision-verify the top few so
        // what we send GENUINELY matches; the no-descriptor path just varies the pick.
        let chosen: Option<&mind_tools::PhotoAsset>;
        let mut chosen_bytes: Option<Vec<u8>> = None;
        if !desc.is_empty() && mind_tools::VisionClient::from_env().is_some() {
            let mut hit: Option<&mind_tools::PhotoAsset> = None;
            let look = if searched { 4 } else { 8 };
            for a in cands.iter().take(look) {
                let Some(bytes) = sources[idx].image_bytes(a).await else { continue };
                let verdict = self
                    .analyze_image_bytes(bytes.clone(), "image/jpeg", &format!("Does this photo clearly show: {desc}? Answer only YES or NO."))
                    .await;
                if verdict.trim().to_uppercase().starts_with("YES") {
                    hit = Some(a);
                    chosen_bytes = Some(bytes);
                    break;
                }
            }
            if hit.is_none() && searched {
                // CLIP ranked these best across the whole archive — send the top hit rather than
                // refuse, but say the verifier wasn't sure.
                if let Some(a) = cands.first() {
                    if let Some(bytes) = sources[idx].image_bytes(a).await {
                        let mut cap = format!("📸 closest match for \"{desc}\"");
                        if !a.date.is_empty() {
                            cap.push_str(&format!(" · {}", a.date));
                        }
                        if !a.place.is_empty() {
                            cap.push_str(&format!(" · {}", a.place));
                        }
                        self.note_photo_anchor(&desc, &a.date).await;
                        self.note_photo_sent(&a.id).await;
                        self.session_note_photo(sources[idx].name(), &a.id, &cap, &a.date);
                        self.photo_queue.lock().unwrap().push((bytes, cap, target));
                        return format!(
                            "Sending the library's closest match for \"{desc}\"{} — my eyes weren't 100% sure it's exactly that, so tell me if it's off. 📸",
                            if label.is_empty() { String::new() } else { format!(" with {label}") }
                        );
                    }
                }
                return format!("I searched for \"{desc}\" but couldn't fetch a matching image — honest miss.");
            }
            if hit.is_none() {
                return format!(
                    "I looked through {} recent {} and none clearly shows \"{desc}\" — being straight with you rather than sending a wrong one. Want me to dig further back?",
                    cands.len().min(8),
                    if label.is_empty() { "photos".to_string() } else { format!("photos of {label}") }
                );
            }
            chosen = hit;
        } else {
            let n = cands.len().min(12).max(1);
            let pick = (chrono::Utc::now().timestamp_millis() as usize) % n;
            chosen = cands.get(pick);
            if let Some(a) = chosen {
                chosen_bytes = sources[idx].image_bytes(a).await;
            }
        }
        let (Some(a), Some(bytes)) = (chosen, chosen_bytes) else {
            return "I found a match but couldn't fetch the image bytes.".to_string();
        };
        let (who, unknown) = sources[idx].people_in(&a.id).await;
        let mut cap = String::from("📸");
        if !who.is_empty() {
            cap.push_str(&format!(" {}", who.join(", ")));
            if unknown > 0 {
                cap.push_str(&format!(" +{unknown}"));
            }
        } else if !label.is_empty() {
            cap.push_str(&format!(" {label}"));
        }
        if !a.date.is_empty() {
            cap.push_str(&format!(" · {}", a.date));
        }
        if !a.place.is_empty() {
            cap.push_str(&format!(" · {}", a.place));
        }
        if !desc.is_empty() {
            cap.push_str(&format!(" ({desc})"));
        }
        if !desc.is_empty() {
            self.note_photo_anchor(&desc, &a.date).await;
        }
        self.note_photo_sent(&a.id).await;
        self.session_note_photo(sources[idx].name(), &a.id, &cap, &a.date);
        self.photo_queue.lock().unwrap().push((bytes, cap, target));
        let what = if label.is_empty() { "one from your library".to_string() } else { format!("one of {label}") };
        format!(
            "Found it — sending {what}{} 📸",
            if a.date.is_empty() { String::new() } else { format!(" from {}", a.date) }
        )
    }

    /// Drain images queued for delivery: (bytes, caption, target chat — None = the primary).
    pub fn take_outbound_photos(&self) -> Vec<(Vec<u8>, String, Option<i64>)> {
        std::mem::take(&mut *self.photo_queue.lock().unwrap())
    }

    pub(crate) async fn face_gallery(&self) -> serde_json::Value {
        self.memory
            .profile_get("facegallery")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_else(|| serde_json::json!({ "people": {} }))
    }

    /// Build/refresh the gallery (detached — dozens of embeds take minutes). For every named
    /// identity (source-named + our face_names), embed up to 6 photos, match OUR detected face to
    /// the source's box for that person (center-in-box), average into a normalized centroid.
    pub async fn faces_learn(&self) -> String {
        if mind_tools::FaceEngine::from_env().is_none() {
            return "No face embedder configured (YM_FACE_ML_URL) — honest state.".to_string();
        }
        let guard = "faces".to_string();
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return "Already learning faces — results land here shortly.".to_string();
        }
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let fm = self.face_names().await;
        tokio::spawn(async move {
            let sources = mind_tools::PhotoSource::all_from_env();
            let Some(src) = sources.iter().find(|s| s.knows_people()) else {
                nq.lock().unwrap().push("🧠 Face learning needs a face-aware photo source connected.".to_string());
                studies.lock().unwrap().remove(&guard);
                return;
            };
            let Some(engine) = mind_tools::FaceEngine::from_env() else {
                studies.lock().unwrap().remove(&guard);
                return;
            };
            // Named identities: source names + our own taught names.
            let mut identities: Vec<(String, String)> = Vec::new(); // (person_id, name)
            for p in src.list_people().await {
                let nm = if p.name.is_empty() {
                    fm.get(&format!("{}:{}", src.name(), p.id)).cloned().unwrap_or_default()
                } else {
                    p.name.clone()
                };
                if nm.len() > 1 {
                    identities.push((p.id, nm));
                }
            }
            let mut gallery = serde_json::json!({ "people": {} });
            let mut lines: Vec<String> = Vec::new();
            for (pid, name) in &identities {
                let assets = src.assets_of_person(pid, 8).await;
                let mut sum: Vec<f32> = Vec::new();
                let mut n = 0usize;
                for a in assets.iter().take(6) {
                    let Some((bx1, by1, bx2, by2, _)) = src.face_box(&a.id, pid).await else { continue };
                    let Some(bytes) = src.image_bytes(a).await else { continue };
                    let Ok(faces) = engine.faces(bytes).await else { continue };
                    // Our detected face whose center sits inside the source's labeled box = them.
                    let (cx_lo, cy_lo, cx_hi, cy_hi) = (bx1, by1, bx2, by2);
                    for f in faces {
                        let (fx, fy) = ((f.bbox.0 + f.bbox.2) / 2.0, (f.bbox.1 + f.bbox.3) / 2.0);
                        if fx >= cx_lo && fx <= cx_hi && fy >= cy_lo && fy <= cy_hi {
                            let norm: f32 = f.embedding.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                            if sum.is_empty() {
                                sum = f.embedding.iter().map(|x| x / norm).collect();
                            } else {
                                for (i, x) in f.embedding.iter().enumerate() {
                                    sum[i] += x / norm;
                                }
                            }
                            n += 1;
                            break;
                        }
                    }
                }
                if n >= 2 {
                    let cn: f32 = sum.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                    let centroid: Vec<f32> = sum.iter().map(|x| x / cn).collect();
                    gallery["people"][name] = serde_json::json!({ "c": centroid, "n": n });
                    lines.push(format!("{name} ({n} faces)"));
                }
            }
            let _ = mem.profile_set("facegallery", &gallery.to_string()).await;
            nq.lock().unwrap().push(format!(
                "🧠 Face gallery learned — I can now recognize {} people in ANY photo, with my own eyes: {}",
                lines.len(),
                lines.join(", ")
            ));
            studies.lock().unwrap().remove(&guard);
        });
        "🧠 Learning the family's faces into my own memory (a few minutes) — I'll confirm when I can recognize everyone myself.".to_string()
    }

    /// Recognize people in ANY image using OUR gallery. Returns (name, similarity) per face
    /// above threshold, plus a count of unknown faces.
    pub async fn identify_faces_in(&self, image: &[u8]) -> (Vec<(String, f32)>, usize) {
        let Some(engine) = mind_tools::FaceEngine::from_env() else {
            return (Vec::new(), 0);
        };
        let gallery = self.face_gallery().await;
        let Some(people) = gallery["people"].as_object() else {
            return (Vec::new(), 0);
        };
        if people.is_empty() {
            return (Vec::new(), 0);
        }
        let threshold: f32 = std::env::var("YM_FACE_THRESHOLD").ok().and_then(|s| s.parse().ok()).unwrap_or(0.45);
        let Ok(faces) = engine.faces(image.to_vec()).await else {
            return (Vec::new(), 0);
        };
        let mut named: Vec<(String, f32)> = Vec::new();
        let mut unknown = 0usize;
        for f in faces {
            let mut best: Option<(String, f32)> = None;
            for (name, entry) in people {
                let c: Vec<f32> = entry["c"].as_array().map(|a| a.iter().filter_map(|v| v.as_f64().map(|x| x as f32)).collect()).unwrap_or_default();
                let sim = mind_tools::cosine(&f.embedding, &c);
                if sim > best.as_ref().map_or(0.0, |b| b.1) {
                    best = Some((name.clone(), sim));
                }
            }
            match best {
                Some((name, sim)) if sim >= threshold => {
                    if !named.iter().any(|(n, _)| n == &name) {
                        named.push((name, sim));
                    }
                }
                _ => unknown += 1,
            }
        }
        (named, unknown)
    }

    /// ---------- PHOTO SESSION (working set) ----------
    /// Pin a just-surfaced photo into the session buffer (last 12 kept).
    pub(crate) fn session_note_photo(&self, source: &str, id: &str, cap: &str, date: &str) {
        let mut sess = self.photo_session.lock().unwrap();
        sess.push(serde_json::json!({
            "src": source,
            "id": id,
            "cap": cap.chars().take(120).collect::<String>(),
            "date": date,
            "ts": chrono::Utc::now().timestamp_millis(),
        }));
        if sess.len() > 12 {
            let cut = sess.len() - 12;
            sess.drain(..cut);
        }
    }

    /// Resolve a follow-up against the photos currently "in view" (surfaced within 2h):
    /// ordinals pick by position, descriptors are vision-matched, and visual QUESTIONS are
    /// answered by looking at the actual photo. Honest when nothing is in view or nothing matches.
    /// Is a photo working set currently in view (non-empty, < 2h old)?
    pub fn photo_session_active(&self) -> bool {
        let now = chrono::Utc::now().timestamp_millis();
        self.photo_session
            .lock()
            .unwrap()
            .iter()
            .any(|e| now - e["ts"].as_i64().unwrap_or(0) < 2 * 3_600_000)
    }

    pub async fn photo_followup_turn(&self, text: &str, target: Option<i64>) -> String {
        let now = chrono::Utc::now().timestamp_millis();
        let fresh: Vec<serde_json::Value> = {
            self.photo_session
                .lock()
                .unwrap()
                .iter()
                .filter(|e| now - e["ts"].as_i64().unwrap_or(0) < 2 * 3_600_000)
                .cloned()
                .collect()
        };
        if fresh.is_empty() {
            return "I don't have any photos in view right now — the ones from earlier have left my working set. Ask me to find them again and I'll keep the thread this time.".to_string();
        }
        let sources = mind_tools::PhotoSource::all_from_env();
        let fetch = |e: &serde_json::Value| {
            let src_name = e["src"].as_str().unwrap_or("").to_string();
            let id = e["id"].as_str().unwrap_or("").to_string();
            let sources = &sources;
            async move {
                let src = sources.iter().find(|s| s.name() == src_name)?;
                src.image_bytes(&mind_tools::PhotoAsset { id, date: String::new(), place: String::new(), ..Default::default() }).await
            }
        };
        let l = text.to_lowercase();
        let is_question = text.trim().ends_with('?')
            || ["is ", "are ", "does ", "do ", "who ", "what ", "which ", "how "].iter().any(|q| l.starts_with(q));
        // Ordinal reference → a specific buffered photo.
        let ord: Option<usize> = [
            ("first", 0usize), ("1st", 0), ("second", 1), ("2nd", 1), ("third", 2), ("3rd", 2),
            ("fourth", 3), ("4th", 3), ("fifth", 4), ("5th", 4),
        ]
        .iter()
        .find(|(w, _)| l.contains(w))
        .map(|(_, i)| *i)
        .or_else(|| if l.contains("last one") || l.contains("latest one") { Some(fresh.len() - 1) } else { None });
        // Singular demonstrative ('that one', 'this photo') → the most recent photo in view.
        let demonstrative = ["that one", "this one", "that photo", "this photo", "that pic", "this pic", "it again"]
            .iter()
            .any(|r| l.contains(r));
        let ord = ord.or(if demonstrative { Some(fresh.len() - 1) } else { None });
        if let Some(i) = ord {
            let Some(e) = fresh.get(i) else {
                return format!("Only {} photo(s) are in view — no #{}.", fresh.len(), i + 1);
            };
            let Some(bytes) = fetch(e).await else {
                return "I know which one you mean but couldn't re-fetch it.".to_string();
            };
            if is_question {
                let ans = self.analyze_image_bytes(bytes, "image/jpeg", text).await;
                return format!("Looking at that photo ({}): {}", e["cap"].as_str().unwrap_or("?"), ans);
            }
            self.photo_queue.lock().unwrap().push((bytes, e["cap"].as_str().unwrap_or("📸").to_string(), target));
            return "Sending that one. 📸".to_string();
        }
        // Descriptor reference ("the cake one", "which one has the blue dress"): vision-match the set.
        let stopish = [
            "that", "this", "the", "one", "photo", "pic", "picture", "which", "is", "in", "with",
            "has", "have", "she", "he", "her", "his", "there", "of", "a", "an", "you", "sent",
            "showed", "me", "was", "were", "it", "send", "show", "again", "resend", "please",
            "can", "could", "ones",
        ];
        let desc: String = l
            .trim_end_matches(['?', '!', '.'])
            .split_whitespace()
            .filter(|w| !stopish.contains(w))
            .collect::<Vec<_>>()
            .join(" ");
        if desc.len() >= 3 {
            for (i, e) in fresh.iter().enumerate().take(6) {
                let Some(bytes) = fetch(e).await else { continue };
                let verdict = self
                    .analyze_image_bytes(bytes.clone(), "image/jpeg", &format!("Does this photo clearly show: {desc}? Answer only YES or NO."))
                    .await;
                if verdict.trim().to_uppercase().starts_with("YES") {
                    if is_question && !l.starts_with("which") {
                        let ans = self.analyze_image_bytes(bytes, "image/jpeg", text).await;
                        return format!("That's #{} ({}): {}", i + 1, e["cap"].as_str().unwrap_or("?"), ans);
                    }
                    self.photo_queue.lock().unwrap().push((bytes, e["cap"].as_str().unwrap_or("📸").to_string(), target));
                    return format!("That's #{} — sending it. 📸", i + 1);
                }
            }
            return format!(
                "I looked at the {} photo(s) in view and none clearly shows \"{desc}\" — honest miss. Want me to search the whole library for it instead?",
                fresh.len().min(6)
            );
        }
        // Bare visual question with exactly one photo in view → just look at it.
        if is_question {
            if let Some(e) = fresh.last() {
                if let Some(bytes) = fetch(e).await {
                    let ans = self.analyze_image_bytes(bytes, "image/jpeg", text).await;
                    return format!("Looking at the latest one ({}): {}", e["cap"].as_str().unwrap_or("?"), ans);
                }
            }
        }
        "Tell me which of the photos in view you mean — 'the first one', 'the last one', or describe it ('the cake one').".to_string()
    }

    /// ---------- LIBRARY CLEANUP ----------
    /// Organize the photo library ITSELF: sweep the archive, classify screenshots and forwards/
    /// no-camera saves, file them into auto-albums — and, as an explicit second step, archive
    /// them out of the main timeline (reversible; nothing is ever deleted).
    pub async fn photo_cleanup(&self, mode: &str) -> String {
        let sources = mind_tools::PhotoSource::all_from_env();
        let Some(idx) = sources.iter().position(|s| s.knows_people()) else {
            return "No photo source connected.".to_string();
        };
        let guard = "cleanup".to_string();
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return "A library cleanup is already running — results land here shortly.".to_string();
        }
        let src_name = sources[idx].name().to_string();
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let mode = mode.to_string();
        let mode2 = mode.clone();
        tokio::spawn(async move {
            let mode = mode2;
            use chrono::Datelike;
            let sources = mind_tools::PhotoSource::all_from_env();
            let Some(found) = sources.into_iter().find(|s| s.name() == src_name) else {
                studies.lock().unwrap().remove(&guard);
                return;
            };
            let mind_tools::PhotoSource::Immich(im) = &found else {
                studies.lock().unwrap().remove(&guard);
                return;
            };
            // Sweep + classify the whole archive (metadata only).
            let (mut shots, mut fwds) = (Vec::new(), Vec::new());
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
                    for (id, _d, _p, file, camera) in im.taken_between(&from, &to, &[], 1000).await.unwrap_or_default() {
                        let asset = mind_tools::PhotoAsset { id, file, camera, ..Default::default() };
                        match mind_tools::junk_class(&asset) {
                            Some("screenshot") => shots.push(asset.id),
                            Some("forward") => fwds.push(asset.id),
                            _ => {}
                        }
                    }
                }
            }
            let keep_set: std::collections::HashSet<String> = mem
                .profile_get("cleanup_keep")
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
                .unwrap_or_default()
                .into_iter()
                .collect();
            match mode.as_str() {
                // TRIAGE: which forwards contain KNOWN FAMILY FACES? Those are keepers.
                "triage" => {
                    let engine = mind_tools::FaceEngine::from_env();
                    let mut keeps: Vec<String> = Vec::new();
                    let mut gray: Vec<String> = Vec::new();
                    let mut junk = 0usize;
                    let total = fwds.len();
                    for (i, id) in fwds.iter().enumerate() {
                        if i % 2000 == 0 && i > 0 {
                            eprintln!("[cleanup] triage {i}/{total} — {} keepers so far", keeps.len());
                        }
                        // Tier 1 (free): Immich's own face assignments.
                        let (named, unknown) = found.people_in(id).await;
                        if !named.is_empty() {
                            keeps.push(id.clone());
                        } else if unknown > 0 {
                            gray.push(id.clone());
                        } else {
                            junk += 1;
                        }
                    }
                    // Tier 2: face-bearing but unnamed — check against OUR gallery (bounded).
                    let mut rescued = 0usize;
                    if let Some(engine) = engine {
                        let gallery = mem
                            .profile_get("facegallery")
                            .await
                            .ok()
                            .flatten()
                            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                            .unwrap_or_else(|| serde_json::json!({ "people": {} }));
                        let people: Vec<(String, Vec<f32>)> = gallery["people"]
                            .as_object()
                            .map(|m| {
                                m.iter()
                                    .filter_map(|(n, e)| {
                                        let c: Vec<f32> = e["c"].as_array()?.iter().filter_map(|v| v.as_f64().map(|x| x as f32)).collect();
                                        Some((n.clone(), c))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        let threshold: f32 = std::env::var("YM_FACE_THRESHOLD").ok().and_then(|s| s.parse().ok()).unwrap_or(0.45);
                        if !people.is_empty() {
                            let gray_total = gray.len().min(1500);
                            for (gi, id) in gray.iter().take(1500).enumerate() {
                                if gi % 250 == 0 && gi > 0 {
                                    eprintln!("[cleanup] tier-2 gallery check {gi}/{gray_total} — {rescued} rescued");
                                }
                                let asset = mind_tools::PhotoAsset { id: id.clone(), ..Default::default() };
                                let Some(bytes) = found.image_bytes(&asset).await else { continue };
                                let Ok(faces) = engine.faces(bytes).await else { continue };
                                let hit = faces.iter().any(|f| {
                                    people.iter().any(|(_, c)| mind_tools::cosine(&f.embedding, c) >= threshold)
                                });
                                if hit {
                                    keeps.push(id.clone());
                                    rescued += 1;
                                }
                            }
                        }
                        let _ = gray_totals_note(rescued);
                    }
                    // Protect the keepers + file them into a Family album.
                    let _ = mem.profile_set("cleanup_keep", &serde_json::to_string(&keeps).unwrap_or_default()).await;
                    let albums = im.list_albums().await.unwrap_or_default();
                    let fam_album = albums
                        .iter()
                        .find(|(_, n)| n == "❤️ Family (rescued from forwards)")
                        .map(|(i, _)| i.clone());
                    let fam_album = match fam_album {
                        Some(a) => Some(a),
                        None => im.create_album("❤️ Family (rescued from forwards)").await,
                    };
                    if let Some(aid) = &fam_album {
                        for chunk in keeps.chunks(400) {
                            let _ = im.add_to_album(aid, chunk).await;
                        }
                    }
                    nq.lock().unwrap().push(format!(
                        "🔎 Forwards triage done — of {total} forwards/saves: {} contain KNOWN family faces (kept + filed into ❤️ Family album; {rescued} of those recognized by MY own gallery where the library had no name), {} have unfamiliar faces only, {junk} have no faces at all.\n\n`photos cleanup archive` now archives ONLY the junk — every family keeper stays in your timeline.",
                        keeps.len(),
                        gray.len().saturating_sub(rescued),
                    ));
                }
                // MEMES: classify non-keeper forwards with the local graphic detector — the
                // "no known face + text/graphics" bucket Pranab asked for. Photo-like faceless
                // saves are deliberately NOT flagged (scenery/food someone sent may be a keeper).
                "memes" => {
                    let mut memes: Vec<String> = Vec::new();
                    let mut photo_like = 0usize;
                    let mut unreadable = 0usize;
                    let total = fwds.len();
                    for (i, id) in fwds.iter().enumerate() {
                        if keep_set.contains(id) {
                            continue;
                        }
                        if i % 1500 == 0 && i > 0 {
                            eprintln!("[cleanup] memes {i}/{total} — {} flagged", memes.len());
                        }
                        let asset = mind_tools::PhotoAsset { id: id.clone(), ..Default::default() };
                        let Some(bytes) = found.image_bytes(&asset).await else {
                            unreadable += 1;
                            continue;
                        };
                        match mind_tools::looks_graphic(&bytes) {
                            Some(true) => memes.push(id.clone()),
                            Some(false) => photo_like += 1,
                            None => unreadable += 1,
                        }
                    }
                    // Honesty audit: vision-check a small sample of flagged memes (think-off, fast).
                    let mut audit = String::new();
                    if !memes.is_empty() {
                        if let Some(vc) = mind_tools::VisionClient::from_env() {
                            let (mut agree, mut checked) = (0usize, 0usize);
                            let step = (memes.len() / 12).max(1);
                            for id in memes.iter().step_by(step).take(12) {
                                let asset = mind_tools::PhotoAsset { id: id.clone(), ..Default::default() };
                                let Some(bytes) = found.image_bytes(&asset).await else { continue };
                                if let Ok(v) = vc
                                    .analyze("Is this a meme, advertisement, poster, or text/graphic image (not a camera photo of real life)? Answer only YES or NO.", bytes, "image/jpeg")
                                    .await
                                {
                                    checked += 1;
                                    if v.trim().to_uppercase().starts_with("YES") {
                                        agree += 1;
                                    }
                                }
                            }
                            if checked > 0 {
                                audit = format!(" Vision audit: {agree}/{checked} sampled flags confirmed.");
                            }
                        }
                    }
                    let _ = mem.profile_set("cleanup_memes", &serde_json::to_string(&memes).unwrap_or_default()).await;
                    let albums = im.list_albums().await.unwrap_or_default();
                    let meme_album = albums
                        .iter()
                        .find(|(_, n)| n == "🗑 Memes & Ads (auto)")
                        .map(|(i, _)| i.clone());
                    let meme_album = match meme_album {
                        Some(a) => Some(a),
                        None => im.create_album("🗑 Memes & Ads (auto)").await,
                    };
                    if let Some(aid) = &meme_album {
                        for chunk in memes.chunks(400) {
                            let _ = im.add_to_album(aid, chunk).await;
                        }
                    }
                    nq.lock().unwrap().push(format!(
                        "🗑 Meme/ad detection done — of the non-keeper forwards: {} flagged as memes/ads/graphics (filed into 🗑 Memes & Ads), {photo_like} look like real photos (left alone), {unreadable} unreadable.{audit}

`photos cleanup archive` now sweeps screenshots + flagged memes only — photo-like saves and every family keeper stay in your timeline.",
                        memes.len()
                    ));
                }
                // ARCHIVE: sweep junk out of the timeline — keepers are protected.
                "archive" => {
                    // SAFER SCOPE: screenshots + vision-classified memes/ads only. Faceless but
                    // photo-like saves are NOT swept — run `photos cleanup memes` first to grow
                    // the meme list; keepers are always protected.
                    let memes: Vec<String> = mem
                        .profile_get("cleanup_memes")
                        .await
                        .ok()
                        .flatten()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();
                    let mut archived = 0usize;
                    let protected = |id: &String| keep_set.contains(id);
                    let shots_go: Vec<String> = shots.into_iter().filter(|i| !protected(i)).collect();
                    let memes_go: Vec<String> = memes.into_iter().filter(|i| !protected(i)).collect();
                    for chunk in shots_go.chunks(400).chain(memes_go.chunks(400)) {
                        if im.set_archived(chunk, true).await {
                            archived += chunk.len();
                        }
                    }
                    let _ = fwds; // full forwards deliberately untouched — photo-like saves stay
                    nq.lock().unwrap().push(format!(
                        "🧹 Archived {archived} out of your main timeline ({} screenshots + {} flagged memes/ads) — {} family keepers protected; photo-like faceless saves untouched. Fully reversible.",
                        shots_go.len(),
                        memes_go.len(),
                        keep_set.len()
                    ));
                }
                // ORGANIZE (default): file into auto-albums.
                _ => {
                    let albums = im.list_albums().await.unwrap_or_default();
                    let mut get_album = |name: &str| albums.iter().find(|(_, n)| n == name).map(|(i, _)| i.clone());
                    let shots_album = match get_album("📱 Screenshots (auto)") {
                        Some(a) => Some(a),
                        None => im.create_album("📱 Screenshots (auto)").await,
                    };
                    let fwd_album = match get_album("📨 Forwards & Saves (auto)") {
                        Some(a) => Some(a),
                        None => im.create_album("📨 Forwards & Saves (auto)").await,
                    };
                    let mut filed = 0usize;
                    if let Some(aid) = &shots_album {
                        for chunk in shots.chunks(400) {
                            if im.add_to_album(aid, chunk).await {
                                filed += chunk.len();
                            }
                        }
                    }
                    if let Some(aid) = &fwd_album {
                        for chunk in fwds.chunks(400) {
                            if im.add_to_album(aid, chunk).await {
                                filed += chunk.len();
                            }
                        }
                    }
                    nq.lock().unwrap().push(format!(
                        "🧹 Library organized — {} screenshots and {} forwards/no-camera saves classified; {filed} filed into the auto-albums.\n\nNext: `photos cleanup triage` checks every forward for KNOWN FAMILY FACES (those get kept), then `photos cleanup archive` sweeps only the true junk.",
                        shots.len(),
                        fwds.len()
                    ));
                }
            }
            studies.lock().unwrap().remove(&guard);
        });
        match mode {
            m if m == "triage" => "🔎 Checking every forward for known family faces (the library's assignments + my own gallery) — keepers get protected and filed into a ❤️ Family album. This takes a while; counts land here.".to_string(),
            m if m == "memes" => "🗑 Running the meme/ad detector over the non-keeper forwards (local + a vision audit sample) — counts land here when done.".to_string(),
            m if m == "archive" => "🧹 Sweeping screenshots + flagged memes out of your main timeline (family keepers protected, photo-like saves untouched, fully reversible) — counts land here shortly.".to_string(),
            _ => "🧹 Sweeping the whole archive to classify screenshots and forwards — filing into auto-albums.".to_string(),
        }
    }

    /// Learned "when things happened" map (desc word → YYYY-MM-DD) from successful finds — the
    /// metadata a find surfaces becomes searchable knowledge for the next ask.
    pub(crate) async fn photo_anchors(&self) -> std::collections::HashMap<String, String> {
        self.memory
            .profile_get("photo_anchors")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn note_photo_anchor(&self, desc: &str, date: &str) {
        if date.len() < 8 {
            return;
        }
        let mut m = self.photo_anchors().await;
        for w in desc.split_whitespace().filter(|w| w.len() >= 4).take(4) {
            m.insert(w.to_string(), date.to_string());
        }
        if m.len() <= 300 {
            let _ = self.memory.profile_set("photo_anchors", &serde_json::to_string(&m).unwrap_or_default()).await;
        }
    }

    /// Rolling memory of photos already sent to the chat — "show me another" excludes these.
    pub(crate) async fn photos_sent(&self) -> Vec<String> {
        self.memory
            .profile_get("photos_sent")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn note_photo_sent(&self, id: &str) {
        let mut v = self.photos_sent().await;
        v.retain(|x| x != id);
        v.push(id.to_string());
        if v.len() > 200 {
            let cut = v.len() - 200;
            v.drain(..cut);
        }
        let _ = self.memory.profile_set("photos_sent", &serde_json::to_string(&v).unwrap_or_default()).await;
    }

    /// Our own face-id → name map, learned from who-is-this answers (the source stays read-only).
    pub(crate) async fn face_names(&self) -> std::collections::HashMap<String, String> {
        self.memory
            .profile_get("face_names")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn save_face_names(&self, m: &std::collections::HashMap<String, String>) {
        let _ = self.memory.profile_set("face_names", &serde_json::to_string(m).unwrap_or_default()).await;
    }

    /// Daily FB refresh gate for the poll loop (data-only, no user-facing send).
    pub async fn fb_sync_due(&self) -> bool {
        if mind_tools::FbClient::from_env().is_none() {
            return false;
        }
        let period_ms: i64 = std::env::var("YM_FB_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(86_400) * 1000;
        let last: i64 = self
            .memory
            .profile_get("fb_last_sync")
            .await
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

}
