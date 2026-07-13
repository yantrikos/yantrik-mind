//! Emissary -- festival/birthday/trip readiness packets + the ops board. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// FestivalOps: compile the festival node's unmet criteria into packets. Returns titles built.
    pub async fn emissary_festival(&self, node: &serde_json::Value) -> Vec<String> {
        let node_id = node.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let title = node.get("title").and_then(|x| x.as_str()).unwrap_or("the festival").to_string();
        let when = node.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        let unmet = |c: &str| {
            node.get("readiness").and_then(|r| r.get(c)).and_then(|v| v.as_bool()) != Some(true)
        };
        if !Self::treasury_try_draw("emissary") {
            return vec![];
        }
        let today = local_now();
        let date_str = chrono::DateTime::from_timestamp_millis(when)
            .map(|t| t.with_timezone(today.offset()).format("%A, %B %-d").to_string())
            .unwrap_or_default();
        let expiry = when + 86_400_000;
        let evidence: Vec<String> = self
            .memory
            .beliefs_matching(&title, &mind_types::AccessContext::Operator)
            .await
            .unwrap_or_default()
            .iter()
            .take(5)
            .map(|b| format!("{} ({:.2})", b.statement, b.confidence))
            .collect();
        let mut built: Vec<String> = Vec::new();
        let cfg = GenerationConfig { max_tokens: 500, ..GenerationConfig::default() };

        // ---- supplies: PUBLIC lane, generic by construction ----
        if unmet("supplies") {
            let prompt = format!(
                "A Bengali-Hindu family will attend a local {title} celebration on {date_str}. \
                 Write a practical READINESS CHECKLIST (8-12 short lines, grouped: bring / prepare at home / on the day). \
                 Concrete items only (prasad/offerings, water, cash for donation, comfortable footwear, phone charged for photos, etc). No preamble."
            );
            if let Ok(r) = self
                .inference
                .chat_scoped(vec![ChatMessage::user(&prompt)], cfg.clone(), mind_inference::PrivacyScope::Public)
                .await
            {
                let id = self
                    .packet_add(
                        &node_id,
                        Some("supplies"),
                        "checklist",
                        &format!("{title} — readiness checklist"),
                        r.text.trim(),
                        "festival within 9 days; supplies criterion unmet",
                        evidence.clone(),
                        0.8,
                        false,
                        expiry,
                    )
                    .await;
                let _ = id;
                built.push("supplies checklist".into());
            }
        }

        // ---- logistics + weather + fallback: deterministic forecast, HOUSEHOLD-lane fuse ----
        if unmet("logistics+weather") {
            let mut fc_line = String::from("(forecast unavailable)");
            if let Some(w) = &self.weather {
                let city = self.home_city_now().await.unwrap_or_else(|| "Bentonville".to_string());
                if let Ok(days) = w.daily_outlook(&city, 16).await {
                    let fdate = chrono::DateTime::from_timestamp_millis(when)
                        .map(|t| t.with_timezone(today.offset()).format("%Y-%m-%d").to_string())
                        .unwrap_or_default();
                    if let Some(d) = days.iter().find(|d| d.date == fdate) {
                        fc_line = format!(
                            "{} {}: {}, {:.0}-{:.0}°F, {:.0}% rain, wind {:.0} mph, sunset {}",
                            d.weekday, d.date, d.desc, d.lo_f, d.hi_f, d.precip_prob, d.wind_mph, d.sunset
                        );
                    }
                }
            }
            let prompt = format!(
                "Event: {title} on {date_str} (local community association event).\nForecast: {fc_line}\n\n\
                 Write a short LOGISTICS PLAN: when to leave, what the weather means for the plan, \
                 and ONE concrete fallback if weather or crowd turns bad. 5-7 lines, no preamble."
            );
            if let Ok(r) = self
                .inference
                .chat_scoped(vec![ChatMessage::user(&prompt)], cfg.clone(), mind_inference::PrivacyScope::Household)
                .await
            {
                self.packet_add(
                    &node_id,
                    Some("logistics+weather"),
                    "plan",
                    &format!("{title} — logistics, weather & fallback"),
                    &format!("FORECAST: {fc_line}\n\n{}", r.text.trim()),
                    "festival within 9 days; logistics criterion unmet",
                    evidence.clone(),
                    0.8,
                    false,
                    expiry,
                )
                .await;
                built.push("logistics+weather plan".into());
            }
        }

        // ---- story + family message: SCAFFOLD/FILL (public prompt, local names) ----
        if unmet("story+message") {
            let prompt = format!(
                "Write two things about {title} (a Hindu festival), for a family observing it on {date_str}:\n\
                 1) STORY: a warm, age-appropriate 120-word telling of the festival's story for a 7-year-old child. Simple words, wonder, no fear.\n\
                 2) MESSAGE: a 2-sentence warm festival greeting a husband could send his wife.\n\
                 Use NO personal names anywhere — write 'little one' and 'dear'. Label the sections STORY: and MESSAGE:."
            );
            if let Ok(r) = self
                .inference
                .chat_scoped(vec![ChatMessage::user(&prompt)], cfg, mind_inference::PrivacyScope::Public)
                .await
            {
                // deterministic FILL: swap the placeholders for the real names, locally.
                let people = self.load_people().await;
                let profiles = self.load_people_profiles().await;
                let by_rel = |want: &str| -> Option<String> {
                    // EXACT relationship first across both stores — "daughter" must resolve to the
                    // daughter, never to a "friend's daughter" that merely contains the word.
                    let scan = |rows: &[serde_json::Value], exact: bool| -> Option<String> {
                        rows.iter()
                            .find(|p| {
                                p.get("relationship")
                                    .and_then(|x| x.as_str())
                                    .map(|r| {
                                        let rl = r.trim().to_lowercase();
                                        if exact { rl == want } else { rl.contains(want) }
                                    })
                                    .unwrap_or(false)
                            })
                            .and_then(|p| p.get("name").and_then(|x| x.as_str()).map(String::from))
                    };
                    scan(&people, true)
                        .or_else(|| scan(&profiles, true))
                        .or_else(|| scan(&people, false))
                };
                let daughter = by_rel("daughter").unwrap_or_else(|| "little one".into());
                let wife = by_rel("wife").unwrap_or_else(|| "dear".into());
                let filled = r
                    .text
                    .trim()
                    .replace("little one", &daughter)
                    .replace("Little one", &daughter)
                    .replace("dear", &wife)
                    .replace("Dear", &wife);
                self.packet_add(
                    &node_id,
                    Some("story+message"),
                    "draft",
                    &format!("{title} — {daughter}'s story + family message"),
                    &format!("{filled}\n\n(Story composed on the public lane with no names; names filled locally — nothing family-identifying left this house.)"),
                    "festival within 9 days; story+message criterion unmet",
                    evidence,
                    0.8,
                    false,
                    expiry,
                )
                .await;
                built.push("story + family message".into());
            }
        }
        built
    }

    /// BirthdayOps: gift/card/plan/collision-check packets for a birthday node (<=14d out).
    pub async fn emissary_birthday(&self, node: &serde_json::Value) -> Vec<String> {
        let node_id = node.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let title = node.get("title").and_then(|x| x.as_str()).unwrap_or("the birthday").to_string();
        let when = node.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        let who = title.split('\u{2019}').next().unwrap_or(&title).split('\'').next().unwrap_or(&title).trim().to_string();
        let unmet = |c: &str| node.get("readiness").and_then(|r| r.get(c)).and_then(|v| v.as_bool()) != Some(true);
        if !Self::treasury_try_draw("emissary") {
            return vec![];
        }
        let today = local_now();
        let date_str = chrono::DateTime::from_timestamp_millis(when)
            .map(|t| t.with_timezone(today.offset()).format("%A, %B %-d").to_string())
            .unwrap_or_default();
        let expiry = when + 86_400_000;
        let evidence: Vec<String> = self
            .memory
            .beliefs_matching(&format!("{who} birthday gift"), &mind_types::AccessContext::Operator)
            .await
            .unwrap_or_default()
            .iter()
            .take(6)
            .map(|b| format!("{} ({:.2})", b.statement, b.confidence))
            .collect();
        let mut built: Vec<String> = Vec::new();
        let cfg = GenerationConfig { max_tokens: 500, ..GenerationConfig::default() };

        // collision-check FIRST — deterministic, and its finding feeds the plan prompt.
        let mut collision_note = String::new();
        if unmet("collision-check") {
            let siblings = self.future_scan(21).await;
            let mut hits: Vec<String> = Vec::new();
            for sib in &siblings {
                let sid = sib.get("id").and_then(|x| x.as_str()).unwrap_or("");
                if sid == node_id {
                    continue;
                }
                let skind = sib.get("kind").and_then(|x| x.as_str()).unwrap_or("");
                if !matches!(skind, "trip" | "festival" | "event") {
                    continue;
                }
                let swhen = sib.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0);
                let gap_days = (swhen - when) / 86_400_000;
                if gap_days.abs() <= 3 {
                    let stitle = sib.get("title").and_then(|x| x.as_str()).unwrap_or("?");
                    hits.push(format!(
                        "[{skind}] {stitle} — {}",
                        if gap_days == 0 { "SAME DAY".to_string() } else if gap_days > 0 { format!("{gap_days} day(s) AFTER") } else { format!("{} day(s) BEFORE", -gap_days) }
                    ));
                }
            }
            let body = if hits.is_empty() {
                format!("Checked every known event within ±3 days of {who}'s birthday ({date_str}): no collisions. The day is clear.")
            } else {
                collision_note = format!(
                    "CONSTRAINT: adjacent events — {}. Front-load anything heavy (gift, card, errands); avoid plans that create packing or travel stress.",
                    hits.join("; ")
                );
                format!(
                    "⚠️ {who}'s birthday ({date_str}) collides with:\n{}\n\nGuidance: front-load the gift and card BEFORE the birthday; pick a low-logistics celebration; don't schedule anything that competes with packing or departure prep.",
                    hits.iter().map(|h| format!("  • {h}")).collect::<Vec<_>>().join("\n")
                )
            };
            self.packet_add(&node_id, Some("collision-check"), "info", &format!("{who}'s birthday — collision check"), &body, "deterministic cross-event scan (±3 days)", vec![], 0.95, false, expiry).await;
            built.push("collision-check".into());
        }

        if unmet("gift") {
            let facts = if evidence.is_empty() { "(no stored gift facts)".to_string() } else { evidence.join("\n") };
            let prompt = format!(
                "Facts about a birthday gift decision (from stored, dated notes):\n{facts}\n\nBirthday: {date_str}. {collision_note}\n\n\
                 Write a GIFT STATUS packet: what's already decided, what still needs an action (order dates, budget), and ONE concrete recommendation for what to do in the next 48h. Use ONLY the facts above — never invent products, prices, or preferences. 6-9 lines, no preamble."
            );
            if let Ok(r) = self.inference.chat_scoped(vec![ChatMessage::user(&prompt)], cfg.clone(), mind_inference::PrivacyScope::Household).await {
                self.packet_add(&node_id, Some("gift"), "plan", &format!("{who}'s birthday — gift status & next action"), r.text.trim(), "birthday within 14 days; gift criterion unmet", evidence.clone(), 0.8, false, expiry).await;
                built.push("gift".into());
            }
        }

        if unmet("card") {
            let prompt = "Write a warm, personal 4-sentence birthday card message a husband writes to his wife. Heartfelt, specific to a shared life (home, laughter, growing together), no clichés about age. Use 'dear' as the only name placeholder. No preamble.".to_string();
            if let Ok(r) = self.inference.chat_scoped(vec![ChatMessage::user(&prompt)], cfg.clone(), mind_inference::PrivacyScope::Public).await {
                let filled = r.text.trim().replace("dear", &who).replace("Dear", &who);
                self.packet_add(&node_id, Some("card"), "draft", &format!("{who}'s birthday — card draft"), &format!("{filled}\n\n(Composed on the public lane with no names; name filled locally.)"), "birthday within 14 days; card criterion unmet", vec![], 0.75, false, expiry).await;
                built.push("card".into());
            }
        }

        if unmet("plan") {
            let mut fc = String::new();
            if let Some(w) = &self.weather {
                let city = self.home_city_now().await.unwrap_or_else(|| "Bentonville".to_string());
                if let Ok(days) = w.daily_outlook(&city, 16).await {
                    let fdate = chrono::DateTime::from_timestamp_millis(when)
                        .map(|t| t.with_timezone(today.offset()).format("%Y-%m-%d").to_string())
                        .unwrap_or_default();
                    if let Some(d) = days.iter().find(|d| d.date == fdate) {
                        fc = format!("Forecast that day: {}, {:.0}-{:.0}°F, {:.0}% rain.", d.desc, d.lo_f, d.hi_f, d.precip_prob);
                    }
                }
            }
            let prompt = format!(
                "Plan a birthday evening for a spouse. Date: {date_str}. {fc} {collision_note}\n\
                 Household: two adults, one 7-year-old daughter who will want to be part of it.\n\
                 Give TWO options — one quiet/at-home, one out-but-low-logistics — each 3 lines (what, prep needed, why it fits the constraints). No preamble."
            );
            if let Ok(r) = self.inference.chat_scoped(vec![ChatMessage::user(&prompt)], cfg, mind_inference::PrivacyScope::Household).await {
                self.packet_add(&node_id, Some("plan"), "plan", &format!("{who}'s birthday — two celebration options"), r.text.trim(), "birthday within 14 days; plan criterion unmet", vec![], 0.75, false, expiry).await;
                built.push("plan".into());
            }
        }
        built
    }

    /// TripOps: packing/documents/weather+fallback/route+timing packets for a trip node (<=10d).
    pub async fn emissary_trip(&self, node: &serde_json::Value) -> Vec<String> {
        let node_id = node.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let title = node.get("title").and_then(|x| x.as_str()).unwrap_or("the trip").to_string();
        let when = node.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0);
        let end = node.get("end_ms").and_then(|x| x.as_i64()).unwrap_or(when);
        let unmet = |c: &str| node.get("readiness").and_then(|r| r.get(c)).and_then(|v| v.as_bool()) != Some(true);
        if !Self::treasury_try_draw("emissary") {
            return vec![];
        }
        let today = local_now();
        let date_str = chrono::DateTime::from_timestamp_millis(when)
            .map(|t| t.with_timezone(today.offset()).format("%A, %B %-d").to_string())
            .unwrap_or_default();
        let nights = ((end - when) / 86_400_000).max(1);
        // Destination heuristic: first capitalized word of the title, else home.
        let dest = title
            .split_whitespace()
            .find(|w| w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false))
            .unwrap_or("home")
            .to_string();
        let expiry = end + 86_400_000;
        let evidence: Vec<String> = self
            .memory
            .beliefs_matching(&title, &mind_types::AccessContext::Operator)
            .await
            .unwrap_or_default()
            .iter()
            .take(6)
            .map(|b| format!("{} ({:.2})", b.statement, b.confidence))
            .collect();
        let mut built: Vec<String> = Vec::new();
        let cfg = GenerationConfig { max_tokens: 550, ..GenerationConfig::default() };

        // weather across the trip window — deterministic, feeds packing + fallback.
        let mut fc_lines: Vec<String> = Vec::new();
        if let Some(w) = &self.weather {
            if let Ok(days) = w.daily_outlook(&dest, 16).await {
                let d0 = chrono::DateTime::from_timestamp_millis(when).map(|t| t.with_timezone(today.offset()).date_naive());
                let d1 = chrono::DateTime::from_timestamp_millis(end).map(|t| t.with_timezone(today.offset()).date_naive());
                if let (Some(d0), Some(d1)) = (d0, d1) {
                    for d in &days {
                        if let Ok(nd) = chrono::NaiveDate::parse_from_str(&d.date, "%Y-%m-%d") {
                            if nd >= d0 && nd <= d1 {
                                fc_lines.push(format!("{} {}: {}, {:.0}-{:.0}°F, {:.0}% rain", d.weekday, d.date, d.desc, d.lo_f, d.hi_f, d.precip_prob));
                            }
                        }
                    }
                }
            }
        }
        let fc_block = if fc_lines.is_empty() { "(forecast unavailable)".to_string() } else { fc_lines.join("\n") };

        if unmet("packing") {
            let prompt = format!(
                "Packing list for a {nights}-night family trip to {dest} starting {date_str}. Family: two adults, one 7-year-old daughter.\n\
                 Forecast:\n{fc_block}\n\nGroup: adults / child / shared (chargers, meds, documents, snacks-for-the-drive). Weather-appropriate. 14-20 lines total, no preamble."
            );
            if let Ok(r) = self.inference.chat_scoped(vec![ChatMessage::user(&prompt)], cfg.clone(), mind_inference::PrivacyScope::Household).await {
                self.packet_add(&node_id, Some("packing"), "checklist", &format!("{dest} trip — packing list"), r.text.trim(), "trip within 10 days; packing criterion unmet", evidence.clone(), 0.8, false, expiry).await;
                built.push("packing".into());
            }
        }

        if unmet("documents") {
            let facts = if evidence.is_empty() { "(no stored booking facts)".to_string() } else { evidence.join("\n") };
            let body = format!(
                "DOCUMENTS & CONFIRMATIONS — everything the substrate holds:\n{facts}\n\n\
                 Standard set: IDs, insurance cards, booking confirmation (screenshot it offline), payment cards, any tickets.\n\
                 One check to run the night before: confirmation email accessible offline + hotel phone number saved."
            );
            self.packet_add(&node_id, Some("documents"), "checklist", &format!("{dest} trip — documents & confirmations"), &body, "deterministic composition from stored booking facts", evidence.clone(), 0.9, false, expiry).await;
            built.push("documents".into());
        }

        if unmet("weather+fallback") {
            let prompt = format!(
                "A family trip to {dest}, {nights} nights from {date_str}. Forecast:\n{fc_block}\n\n\
                 Write: (1) what the weather means for the trip in one line per day; (2) TWO indoor fallback activities in {dest} suitable for a 7-year-old, if rain hits. 8-10 lines, no preamble."
            );
            if let Ok(r) = self.inference.chat_scoped(vec![ChatMessage::user(&prompt)], cfg.clone(), mind_inference::PrivacyScope::Household).await {
                self.packet_add(&node_id, Some("weather+fallback"), "plan", &format!("{dest} trip — weather & fallbacks"), &format!("FORECAST:\n{fc_block}\n\n{}", r.text.trim()), "trip within 10 days; weather criterion unmet", vec![], 0.8, false, expiry).await;
                built.push("weather+fallback".into());
            }
        }

        if unmet("route+timing") {
            let facts = if evidence.is_empty() { String::new() } else { format!("Known constraints:\n{}\n", evidence.join("\n")) };
            let prompt = format!(
                "Drive plan: Centerton/Bentonville AR to {dest}, family car trip with a 7-year-old, arriving {date_str}.\n{facts}\
                 Write: departure window recommendation, drive time estimate, ONE good rest stop pattern for a child, and arrival-day sequencing around any known appointment. 6-8 lines, no preamble."
            );
            if let Ok(r) = self.inference.chat_scoped(vec![ChatMessage::user(&prompt)], cfg, mind_inference::PrivacyScope::Household).await {
                self.packet_add(&node_id, Some("route+timing"), "plan", &format!("{dest} trip — route & timing"), r.text.trim(), "trip within 10 days; route criterion unmet", evidence, 0.75, false, expiry).await;
                built.push("route+timing".into());
            }
        }
        built
    }

    /// `ym board` — what am I carrying? The pull-side operations view (the charter's cockpit).
    pub async fn ops_board(&self) -> String {
        let mut out = String::from("🗂 OPERATIONS BOARD\n");
        let live = self.live_packets().await;
        let needs_word: Vec<&serde_json::Value> = live
            .iter()
            .filter(|p| {
                p.get("confirmation_required").and_then(|x| x.as_bool()).unwrap_or(false)
                    && p.get("status").and_then(|x| x.as_str()) == Some("proposed")
            })
            .collect();
        out.push_str(&format!(
            "Packets: {} live ({} awaiting your word — `packets`)\n",
            live.len(),
            needs_word.len()
        ));
        let ranked = self.future_fragile(14).await;
        if let Some((score, n)) = ranked.first() {
            out.push_str(&format!(
                "Most fragile: {} (fragility {score:.1})\n",
                n.get("title").and_then(|x| x.as_str()).unwrap_or("?")
            ));
        }
        if let Some(rep) = self.memory.profile_get("nightshift_report").await.ok().flatten() {
            out.push_str(&format!("Last shift: {}\n", rep.replace('\n', " · ")));
        }
        out.push_str(&format!("\n{}\n", Self::treasury_report().lines().take(7).collect::<Vec<_>>().join("\n")));
        // Self-accountability lines: calibration + immunology. The board is
        // where the family SEES that the mind measures itself.
        out.push_str(&format!("\n{}\n", self.judgment_report().await));
        out.push_str(&format!("{}\n", Self::immune_board_line()));
        out.push_str("\nDetail: `packets` · `future` · `regrets` · `providers` · `nightshift` · `immune` · `judgment`");
        out
    }

}
