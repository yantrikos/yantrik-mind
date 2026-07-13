//! Plugin registry + self-report -- manifests, seeding, search, weekly self-review. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) fn plugin_manifests() -> Vec<serde_json::Value> {
        let m = |name: &str, kind: &str, status: &str, does: &str, needs: &str| {
            serde_json::json!({ "name": name, "kind": kind, "status": status, "does": does, "needs": needs })
        };
        vec![
            m("immich-photos", "photo_source", "live", "self-hosted family photo archive: people/faces, CLIP search, EXIF dates+places, albums, archive curation", "YM_IMMICH_URL, YM_IMMICH_KEY"),
            m("facebook-photos", "photo_source", "parked", "FB tagged-photo read (album crawl)", "FB_USER_TOKEN (long-lived)"),
            m("onedrive-photos", "photo_source", "gated", "OneDrive photo-year read (Graph, device-code, read-only) — pre-Immich years; find by date + on-this-day. Needs one Azure app id + phone sign-in", "YM_OD_CLIENT_ID + phone approval"),
            m("google-photos", "photo_source", "gated", "Google Photos pick-based import (Picker API, device-code, read-only) — you tap-pick photos, I pull them home + caption with LOCAL vision. NOT a faces source (Google exposes no people API; Immich covers that). For photos that live only in Google Photos", "YM_GPHOTOS_CLIENT_ID + YM_GPHOTOS_CLIENT_SECRET + phone approval"),
            m("google-photos", "photo_source", "planned", "Google Photos read (API access is restricted post-2025 — feasibility check first)", "Google OAuth"),
            m("dropbox-files", "file_source", "planned", "Dropbox file/photo read", "Dropbox OAuth"),
            m("gdrive-files", "file_source", "planned", "Google Drive docs/files read for household paperwork", "Google OAuth"),
            m("own-faces", "vision", "live", "own face gallery: 40 learned people, temporal identity chain, verification of source tags", "YM_FACE_ML_URL"),
            m("vision-analyze", "vision", "live", "photo understanding: occasions, outfits, quality, memes, style timelines", "ollama vision model"),
            m("weather", "info", "live", "current + 16-day outlook (open-meteo) with NWS fallback; photo-day scoring", "none (keyless)"),
            m("mail-inboxes", "comm_read", "live", "multi-account IMAP: digests, taxonomy, teachable rules, subscriptions, renewals", "YM_SCAN_EMAIL[_n] + app passwords"),
            m("email-send", "comm_write", "gated", "outbound email drafts (harm-gate + confirm)", "SMTP creds"),
            m("github", "dev", "live", "repo/notification read; gated commenting; self-build PRs", "GITHUB token"),
            m("home-assistant", "home", "live", "device states + presence for briefings and alerts", "HOME_ASSISTANT_TOKEN"),
            m("telegram", "channel", "live", "primary channel: family multi-user, photos, proactive delivery", "bot token"),
            m("whatsapp-channel", "channel", "planned", "WhatsApp Business/bridge channel — where the wider family actually is", "provider decision + number"),
            m("slack-channel", "channel", "planned", "Slack workspace channel (work-life surface)", "Slack app token"),
            m("discord-channel", "channel", "planned", "Discord channel", "bot token"),
            m("family-frame", "surface", "live", "daily-photo wall tablet page, token-guarded LAN listener", "YM_FRAME_TOKEN"),
            m("web-research", "info", "live", "keyless search + SSRF-guarded fetch; deals; link learning", "none"),
            m("markets", "info", "live", "stocks/crypto quotes + portfolio view", "none"),
            m("sandbox-coder", "dev", "live", "code sandbox + skill authoring loop (self-extension)", "none"),
        ]
    }

    /// Write the registry: KV for listing + one semantic memory line per plugin for discovery.
    pub async fn plugins_seed(&self) -> String {
        let manifests = Self::plugin_manifests();
        let _ = self
            .memory
            .profile_set("plugin_registry", &serde_json::Value::Array(manifests.clone()).to_string())
            .await;
        let seeded = self.memory.profile_get("plugin_seed_ver").await.ok().flatten().unwrap_or_default();
        let mut wrote = 0usize;
        if seeded != "v2" {
            for p in &manifests {
                let line = format!(
                    "[plugin] {} ({}, {}) — {}; needs: {}",
                    p["name"].as_str().unwrap_or(""),
                    p["kind"].as_str().unwrap_or(""),
                    p["status"].as_str().unwrap_or(""),
                    p["does"].as_str().unwrap_or(""),
                    p["needs"].as_str().unwrap_or("")
                );
                if self.memory.remember_observation(&line, mind_types::safety::ProvenanceCategory::Human).await.is_ok() {
                    wrote += 1;
                }
            }
            let _ = self.memory.profile_set("plugin_seed_ver", "v2").await;
        }
        format!(
            "🧩 Plugin registry written: {} manifests in the substrate ({wrote} memory lines seeded). `plugin all` to browse, `plugin search <what>` to discover.",
            Self::plugin_manifests().len()
        )
    }

    pub async fn plugins_all(&self) -> String {
        let reg: Vec<serde_json::Value> = self
            .memory
            .profile_get("plugin_registry")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_else(|| Self::plugin_manifests());
        let mut by_kind: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for p in &reg {
            let icon = match p["status"].as_str().unwrap_or("") {
                "live" => "🟢",
                "gated" => "🔐",
                "parked" => "⏸",
                _ => "🔵",
            };
            by_kind
                .entry(p["kind"].as_str().unwrap_or("other").to_string())
                .or_default()
                .push(format!("{icon} {} — {}", p["name"].as_str().unwrap_or(""), p["does"].as_str().unwrap_or("")));
        }
        let mut out = String::from("🧩 PLUGIN STORE (substrate registry)\n");
        for (kind, items) in by_kind {
            out.push_str(&format!("\n{}:\n{}\n", kind.to_uppercase(), items.join("\n")));
        }
        out.push_str("\n🟢 live · 🔐 gated · ⏸ parked · 🔵 planned — `plugin search <what>` to discover");
        out
    }

    pub async fn plugins_search(&self, q: &str) -> String {
        // Operator read is safe HERE ONLY because of the output filter below: nothing leaves this
        // function unless it starts with "[plugin]" (system catalog metadata, not personal data).
        // Catalog entries are untagged, so a Principal read would wrongly hide them from members.
        // Semantic first (the memory lane), substring safety net second (the KV).
        let mut hits: Vec<String> = Vec::new();
        if let Ok(rs) = self
            .memory
            .recall_typed(mind_types::RecallQuery { text: format!("plugin connector {q}"), top_k: 12, kind: None }, &mind_types::AccessContext::Operator)
            .await
        {
            for r in rs {
                if r.item.text.starts_with("[plugin]") && !hits.iter().any(|h| h == &r.item.text) {
                    hits.push(r.item.text.clone());
                }
            }
        }
        if hits.len() < 3 {
            let ql = q.to_lowercase();
            let words: Vec<String> = ql.split_whitespace().filter(|w| w.len() >= 3).map(String::from).collect();
            let reg: Vec<serde_json::Value> = self
                .memory
                .profile_get("plugin_registry")
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default();
            for p in &reg {
                let line = format!(
                    "[plugin] {} ({}, {}) — {}",
                    p["name"].as_str().unwrap_or(""),
                    p["kind"].as_str().unwrap_or(""),
                    p["status"].as_str().unwrap_or(""),
                    p["does"].as_str().unwrap_or("")
                );
                let key: String = line.chars().take(30).collect();
                let ll = line.to_lowercase();
                let word_hit = words.iter().any(|w| ll.contains(w.as_str()));
                if (ll.contains(&ql) || word_hit) && !hits.iter().any(|h| h.starts_with(&key)) {
                    hits.push(line);
                }
            }
        }
        hits.truncate(6);
        if hits.is_empty() {
            format!("🧩 Nothing in the registry matches \"{q}\" — `plugin all` to browse, and planned plugins count too.")
        } else {
            format!("🧩 Registry matches for \"{q}\":\n{}", hits.join("\n"))
        }
    }

    /// THE WEEKLY SELF-REPORT — the mind reviews its own week: scoreboard per domain, the
    /// corrections it absorbed (with lessons), what it learned (beliefs formed, studies deepened,
    /// rules taught), and the PACING POLICIES it is changing as a result. Deterministic core;
    /// one small LLM pass turns it into honest first-person prose. `ym report` anytime.
    pub async fn self_report(&self, apply_policy: bool) -> String {
        use chrono::Datelike;
        let now = chrono::Utc::now().timestamp_millis();
        let week_ago = now - 7 * 86_400_000;
        let l = self.ledger().await;
        let stats = Self::ledger_stats(&l, week_ago);
        // Corrections + lessons this week (verbatim — these are the gold).
        let lessons: Vec<String> = l
            .iter()
            .filter(|e| e["ts"].as_i64().unwrap_or(0) >= week_ago && e["outcome"].as_str() == Some("corrected"))
            .filter_map(|e| {
                let what = e["what"].as_str().unwrap_or("?");
                e["lesson"].as_str().map(|le| format!("{what} → {le}"))
            })
            .collect();
        // Growth counters: beliefs, taught mail rules, face names learned.
        let beliefs_now = self.memory.belief_count().await.unwrap_or(0) as i64;
        let beliefs_prev: i64 = self.memory.profile_get("report_beliefs").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(beliefs_now);
        let rules_n = self.mail_rules().await.len();
        let faces_n = self.face_names().await.len();
        // MOVE 5 — visible self-extension: what the mind built/changed in ITSELF this week
        // (the self-build loop's evolution log + deploy count). The awe-tier line, made routine.
        let evo_path = std::env::var("YM_EVOLUTION_LOG").unwrap_or_else(|_| "/var/lib/yantrik-mind/evolution.log".to_string());
        let mut built: Vec<String> = Vec::new();
        let mut deploys = 0u32;
        if let Ok(txt) = std::fs::read_to_string(&evo_path) {
            let cutoff = chrono::Utc::now() - chrono::Duration::days(7);
            for line in txt.lines().rev().take(500) {
                let parts: Vec<&str> = line.splitn(4, " | ").collect();
                if parts.len() < 4 {
                    continue;
                }
                let Ok(ts) = chrono::DateTime::parse_from_rfc3339(parts[0].trim()) else { continue };
                if ts.with_timezone(&chrono::Utc) < cutoff {
                    break;
                }
                if parts[1].trim() == "deploy" {
                    deploys += 1;
                } else {
                    built.push(format!(
                        "{} {} — {}",
                        parts[1].trim(),
                        parts[2].trim(),
                        parts[3].trim().chars().take(90).collect::<String>()
                    ));
                }
            }
        }
        built.reverse();
        built.truncate(8);
        // Policy pass: domains ignored hard get slower; domains loved speed back up. Logged.
        let mut policy_notes: Vec<String> = Vec::new();
        if apply_policy {
            for (d, (sends, eng, ign, _)) in &stats {
                if *sends >= 4 {
                    let cur = self.domain_pace(d).await;
                    if *ign as f64 >= *sends as f64 * 0.75 && cur < 4.0 {
                        let new = (cur * 1.5).min(4.0);
                        let _ = self.memory.profile_set(&format!("pace:{d}"), &format!("{new:.2}")).await;
                        policy_notes.push(format!("{d}: mostly ignored ({ign}/{sends}) — slowing myself down ({cur:.1}x → {new:.1}x)"));
                    } else if *eng as f64 >= *sends as f64 * 0.6 && cur > 1.0 {
                        let new = (cur / 1.5).max(1.0);
                        let _ = self.memory.profile_set(&format!("pace:{d}"), &format!("{new:.2}")).await;
                        policy_notes.push(format!("{d}: engaging again ({eng}/{sends}) — speeding back up ({cur:.1}x → {new:.1}x)"));
                    }
                }
            }
            let _ = self.memory.profile_set("report_beliefs", &beliefs_now.to_string()).await;
            let _ = self.memory.profile_set("report_last", &now.to_string()).await;
        }
        let scoreboard: String = if stats.is_empty() {
            "(no proactive acts logged yet — the ledger just opened)".to_string()
        } else {
            stats
                .iter()
                .map(|(d, (s, e, ig, c))| {
                    let pct = if *s > 0 { *e as f64 * 100.0 / *s as f64 } else { 0.0 };
                    format!("- {d}: {s} sent, {e} engaged ({pct:.0}%), {ig} ignored, {c} corrected")
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let self_built = if built.is_empty() {
            format!("{deploys} self-deployments; no separate self-build entries logged")
        } else {
            format!("{deploys} self-deployments; self-built: {}", built.join(" · "))
        };
        let facts = format!(
            "WEEK SCOREBOARD (my proactive predictions vs your reactions):\n{scoreboard}\n\nCORRECTIONS I ABSORBED ({}):\n{}\n\nGROWTH: {} durable beliefs ({}); {} taught mail rules; {} faces I can name.\n\nWHAT I BUILT/CHANGED IN MYSELF THIS WEEK:\n{self_built}\n\nPOLICY CHANGES THIS REVIEW:\n{}",
            lessons.len(),
            if lessons.is_empty() { "(none — either I was right or you were patient)".to_string() } else { lessons.join("\n") },
            beliefs_now,
            if beliefs_now >= beliefs_prev { format!("+{}", beliefs_now - beliefs_prev) } else { format!("{}", beliefs_now - beliefs_prev) },
            rules_n,
            faces_n,
            if policy_notes.is_empty() { "(none needed)".to_string() } else { policy_notes.join("\n") },
        );
        let week = format!("{}", chrono::Utc::now().iso_week().week());
        let prompt = format!(
            "You are writing your OWN weekly self-review to the person you serve — first person, honest, warm, terse (max 220 words), plain text no markdown. Use ONLY these facts (never invent): \n\n{facts}\n\nStructure: what I learned about you this week; where I was wrong (own the misses concretely); what I built in myself (from the BUILT section — mention it with quiet pride); what I'm changing. If the ledger is thin, say plainly this is week one and the numbers start now."
        );
        let cfg = GenerationConfig { max_tokens: 500, ..GenerationConfig::default() };
        match self
            .inference
            .chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg)
            .await
        {
            Ok(r) => format!("🪞 Week {week} self-report\n\n{}", r.text.trim()),
            Err(_) => format!("🪞 Week {week} self-report (raw)\n\n{facts}"),
        }
    }

    /// Weekly gate for the poll loop: 7 days since the last applied review, morning window.
    pub async fn report_due(&self) -> bool {
        use chrono::Timelike;
        let now = local_now();
        if !(8..=11).contains(&now.hour()) {
            return false;
        }
        let last: i64 = self.memory.profile_get("report_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= 7 * 86_400_000
    }

    /// Study-all continuation: names with an unmet taste target and a free guard — the poll loop
    /// chains the next batch (deploy-proof: accumulator + target persist).
    pub async fn taste_continues(&self) -> Vec<String> {
        let mut out = Vec::new();
        for p in &self.load_people_profiles().await {
            let Some(name) = p.get("name").and_then(|x| x.as_str()) else { continue };
            let key = format!("taste_target:{}", name.to_lowercase());
            let target: i64 = self.memory.profile_get(&key).await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
            if target <= 0 {
                continue;
            }
            let total: i64 = self
                .memory
                .profile_get(&format!("tastes:{}", name.to_lowercase()))
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v["total"].as_i64())
                .unwrap_or(0);
            if total < target && !self.studies.lock().unwrap().contains(&format!("tastes:{}", name.to_lowercase())) {
                out.push(name.to_string());
            }
        }
        out
    }

    /// What's running in the background right now — the "is it happening or broke" answer.
    pub fn running_studies(&self) -> String {
        let s = self.studies.lock().unwrap();
        if s.is_empty() {
            "Nothing running in the background right now — all studies idle.".to_string()
        } else {
            format!(
                "⚙️ Running now: {}. (Progress lines go to the journal; results land here when done.)",
                s.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        }
    }

}
