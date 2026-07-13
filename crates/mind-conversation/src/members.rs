//! Household members -- per-member tasks, briefs, photo sharing, member turn handling. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) async fn member_tasks(&self, owner: &str) -> Vec<serde_json::Value> {
        self.memory
            .profile_get(&format!("m:{owner}:tasks"))
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn save_member_tasks(&self, owner: &str, v: &[serde_json::Value]) {
        let _ = self
            .memory
            .profile_set(&format!("m:{owner}:tasks"), &serde_json::to_string(v).unwrap_or_default())
            .await;
    }

    /// Deterministic member intents: reminders, task list, done, daily-brief opt-in/out.
    /// Returns None when the message is plain conversation.
    pub(crate) async fn member_task_turn(&self, owner: &str, name: &str, text: &str) -> Option<String> {
        let low = text.trim().to_lowercase();
        // "remind me to pick up the cake tomorrow" / "add task ..." / "reminder: ..."
        for pat in ["remind me to ", "remind me ", "add task ", "add a task ", "reminder: ", "task: "] {
            if let Some(rest) = low.strip_prefix(pat) {
                let body = text.trim()[text.trim().len() - rest.len()..].trim().to_string();
                if body.len() < 2 {
                    return Some("What should I remind you about?".to_string());
                }
                let due = parse_due(&low);
                let mut tasks = self.member_tasks(owner).await;
                tasks.push(serde_json::json!({
                    "id": chrono::Utc::now().timestamp_millis(),
                    "text": body,
                    "due": due,
                    "done": false,
                    "notified": false,
                }));
                self.save_member_tasks(owner, &tasks).await;
                let when = match due {
                    Some(ms) => chrono::DateTime::from_timestamp_millis(ms as i64)
                        .map(|t| format!(" — I'll nudge you around {}", t.with_timezone(local_now().offset()).format("%a %b %d, %H:%M")))
                        .unwrap_or_default(),
                    None => " — no time attached; it'll sit on your list (say `my tasks`)".to_string(),
                };
                return Some(format!("Got it, {name}: “{body}”{when}. ✅"));
            }
        }
        if ["my tasks", "my reminders", "show tasks", "show reminders", "list tasks", "list reminders"].iter().any(|p| low.contains(p)) {
            let tasks = self.member_tasks(owner).await;
            let open: Vec<(usize, &serde_json::Value)> = tasks.iter().enumerate().filter(|(_, t)| !t["done"].as_bool().unwrap_or(false)).collect();
            if open.is_empty() {
                return Some("Your list is clear — nothing pending. 🎉".to_string());
            }
            let mut out = format!("📝 Your reminders, {name}:");
            for (n, (_, t)) in open.iter().enumerate() {
                let due = t["due"].as_u64().and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
                    .map(|d| format!(" ({})", d.with_timezone(local_now().offset()).format("%b %d %H:%M")))
                    .unwrap_or_default();
                out.push_str(&format!("\n{}. {}{}", n + 1, t["text"].as_str().unwrap_or("?"), due));
            }
            out.push_str("\n\n(say `done <number>` to clear one)");
            return Some(out);
        }
        if let Some(nstr) = low.strip_prefix("done ") {
            if let Ok(n) = nstr.trim().parse::<usize>() {
                let mut tasks = self.member_tasks(owner).await;
                let open_ids: Vec<usize> = tasks.iter().enumerate().filter(|(_, t)| !t["done"].as_bool().unwrap_or(false)).map(|(i, _)| i).collect();
                if let Some(&idx) = open_ids.get(n.saturating_sub(1)) {
                    tasks[idx]["done"] = serde_json::json!(true);
                    let txt = tasks[idx]["text"].as_str().unwrap_or("?").to_string();
                    self.save_member_tasks(owner, &tasks).await;
                    return Some(format!("Done — “{txt}” cleared. ✔️"));
                }
                return Some("I couldn't find that number on your list — say `my tasks` to see it.".to_string());
            }
        }
        if low.contains("brief me daily") || low.contains("daily brief") || low.contains("morning brief") {
            if low.contains("stop") || low.contains("off") || low.contains("no more") {
                let _ = self.memory.profile_set(&format!("m:{owner}:brief"), "").await;
                return Some("Okay — daily briefs off. Say `brief me daily` anytime to restart.".to_string());
            }
            let _ = self.memory.profile_set(&format!("m:{owner}:brief"), "on").await;
            let sample = self.compose_member_brief(owner, name).await;
            return Some(format!("☀️ Daily brief is ON — every morning, just for you. Here's today's:\n\n{sample}"));
        }
        None
    }

    /// A member's short morning brief: THEIR reminders + the household's upcoming dates (the
    /// deliberately-shared layer). No primary-private data by construction.
    pub(crate) async fn compose_member_brief(&self, owner: &str, name: &str) -> String {
        use chrono::Datelike;
        let now = local_now();
        let mut out = format!("☀️ Morning, {name} — {}", now.format("%A, %b %d"));
        let tasks = self.member_tasks(owner).await;
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let soon: Vec<String> = tasks
            .iter()
            .filter(|t| !t["done"].as_bool().unwrap_or(false))
            .filter(|t| t["due"].as_u64().map(|d| d <= now_ms + 48 * 3_600_000).unwrap_or(false))
            .map(|t| {
                let due = t["due"].as_u64().and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
                    .map(|d| format!(" ({})", d.with_timezone(now.offset()).format("%a %H:%M")))
                    .unwrap_or_default();
                format!("• {}{due}", t["text"].as_str().unwrap_or("?"))
            })
            .collect();
        if !soon.is_empty() {
            out.push_str("\n\n⏰ Coming up on your list:\n");
            out.push_str(&soon.join("\n"));
        }
        // Household dates (shared by design): birthdays/anniversaries within 14 days.
        let mut fam: Vec<String> = Vec::new();
        for p in &self.load_people_profiles().await {
            let pname = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if pname.is_empty() || pname.eq_ignore_ascii_case(name) {
                continue;
            }
            for d in p.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                let (Some(mmdd), Some(label)) = (d.get("mmdd").and_then(|x| x.as_str()), d.get("label").and_then(|x| x.as_str())) else { continue };
                if let Some(days) = days_until_mmdd(mmdd, &now) {
                    if (0..=14).contains(&days) {
                        fam.push(format!("• {pname}'s {label} in {days} day(s)"));
                    }
                }
            }
        }
        if !fam.is_empty() {
            out.push_str("\n\n🏠 Family:\n");
            out.push_str(&fam.join("\n"));
        }
        if soon.is_empty() && fam.is_empty() {
            out.push_str("\n\nNothing pressing on your list — enjoy the day. 🌼");
        }
        out
    }

    /// Poll-loop beat for ALL registered members: due-reminder nudges + opt-in morning briefs,
    /// each delivered to that member's own chat. Returns (chat_id, message) pairs.
    pub async fn member_beats(&self) -> Vec<(i64, String)> {
        use chrono::Timelike;
        let mut out: Vec<(i64, String)> = Vec::new();
        let people = self.load_people().await;
        let now = local_now();
        let now_ms = chrono::Utc::now().timestamp_millis() as u64;
        let today = now.format("%Y-%m-%d").to_string();
        for p in &people {
            let (Some(slug), Some(tg)) = (p.get("slug").and_then(|x| x.as_str()), p.get("tg_id").and_then(|x| x.as_i64())) else {
                continue;
            };
            if tg == 0 {
                continue;
            }
            let name = p.get("name").and_then(|x| x.as_str()).unwrap_or(slug).to_string();
            // Due reminders (mark notified so each fires once).
            let mut tasks = self.member_tasks(slug).await;
            let mut dirty = false;
            for t in tasks.iter_mut() {
                let due = t["due"].as_u64().unwrap_or(0);
                if due > 0
                    && due <= now_ms
                    && !t["done"].as_bool().unwrap_or(false)
                    && !t["notified"].as_bool().unwrap_or(false)
                {
                    out.push((tg, format!("⏰ {name} — reminder: {}", t["text"].as_str().unwrap_or("?"))));
                    t["notified"] = serde_json::json!(true);
                    dirty = true;
                }
            }
            if dirty {
                self.save_member_tasks(slug, &tasks).await;
            }
            // Opt-in morning brief, once per date, morning window 7-11 local.
            let brief_on = self
                .memory
                .profile_get(&format!("m:{slug}:brief"))
                .await
                .ok()
                .flatten()
                .map(|v| v == "on")
                .unwrap_or(false);
            if brief_on && (7..=11).contains(&now.hour()) {
                let sent = self.memory.profile_get(&format!("m:{slug}:brief_date")).await.ok().flatten().unwrap_or_default();
                if sent != today {
                    let _ = self.memory.profile_set(&format!("m:{slug}:brief_date"), &today).await;
                    out.push((tg, self.compose_member_brief(slug, &name).await));
                }
            }
        }
        out
    }

    /// The DM chat id for a member slug (registry tg_id; Telegram private chat id == user id).
    pub async fn note_last_photo(&self, jpeg: Vec<u8>, caption: &str) {
        *self.last_sent_photo.lock().unwrap() = Some((jpeg, caption.to_string()));
    }

    /// Share the most recent photo with a household member and relay their take back.
    pub async fn share_with_member(&self, member: &str, note: &str) -> String {
        let want = member.trim().trim_start_matches('@').to_lowercase();
        if want.is_empty() {
            return "Share with whom? (a household member's name, slug, or relationship)".to_string();
        }
        let people = self.load_people().await;
        let Some(p) = people.iter().find(|p| {
            ["slug", "name", "relationship"].iter().any(|f| {
                p.get(*f).and_then(|x| x.as_str()).map(|v| v.to_lowercase() == want).unwrap_or(false)
            })
        }) else {
            return format!("I don't have \"{member}\" in the household registry yet — `person add <slug> <name> <tg_id> <relationship>` first.");
        };
        let slug = p.get("slug").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let name = p.get("name").and_then(|x| x.as_str()).unwrap_or(&slug).to_string();
        let Some(chat) = self.chat_of_member(&slug).await else {
            return format!("{name} is registered but I don't have their Telegram chat yet.");
        };
        let Some((jpeg, caption)) = self.last_sent_photo.lock().unwrap().clone() else {
            return "I haven't sent you a photo recently — nothing to share yet.".to_string();
        };
        let primary = self
            .memory
            .profile_get("name")
            .await
            .ok()
            .flatten()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "The family".to_string());
        let extra = if note.trim().is_empty() { String::new() } else { format!("\n{}", note.trim()) };
        let cap = format!("📨 {primary} shared this with you.{extra}\n{caption}\n\nReply here and I'll pass your take along.");
        self.photo_queue.lock().unwrap().push((jpeg, cap, Some(chat)));
        let take = serde_json::json!({
            "slug": slug, "name": name,
            "until": chrono::Utc::now().timestamp_millis() + 3 * 3_600_000,
            "about": caption.chars().take(120).collect::<String>(),
        });
        let _ = self.memory.profile_set("member_take", &take.to_string()).await;
        format!("📨 Sent to {name} with the note — I'll relay whatever they say back to you.")
    }

    pub(crate) async fn chat_of_member(&self, owner: &str) -> Option<i64> {
        if let Some(rest) = owner.strip_prefix("guest:") {
            return rest.parse().ok();
        }
        self.load_people()
            .await
            .iter()
            .find(|p| p.get("slug").and_then(|x| x.as_str()) == Some(owner))
            .and_then(|p| p.get("tg_id").and_then(|x| x.as_i64()))
    }

    /// A REGISTERED MEMBER's conversational turn (wife/kids — anyone but the primary). Their own
    /// voice, their own scoped transcript, and a hard wall by construction: none of the primary's
    /// memory, beliefs, plans, or surprises is fetched on this path, and the speaker context makes
    /// the identity explicit so the companion never confuses who it's talking to.
    pub(crate) async fn member_turn(&self, user_text: &str, id: &TurnIdentity) -> String {
        // A shared-photo take we're waiting on? Relay the reply to the primary, then continue.
        if let Some(t) = self
            .memory
            .profile_get("member_take")
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        {
            if t["slug"].as_str() == Some(id.owner.as_str())
                && chrono::Utc::now().timestamp_millis() < t["until"].as_i64().unwrap_or(0)
            {
                let _ = self.memory.profile_set("member_take", "").await;
                let who = t["name"].as_str().unwrap_or("They").to_string();
                self.notify_queue.lock().unwrap().push(format!(
                    "💬 {who}'s take on what you shared: \"{}\"",
                    user_text.trim().chars().take(300).collect::<String>()
                ));
            }
        }
        let people = self.load_people().await;
        let (name, rel) = people
            .iter()
            .find(|p| p.get("slug").and_then(|x| x.as_str()) == Some(id.owner.as_str()))
            .map(|p| {
                (
                    p.get("name").and_then(|x| x.as_str()).unwrap_or(id.owner.as_str()).to_string(),
                    p.get("relationship").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                )
            })
            .unwrap_or_else(|| (id.owner.clone(), String::new()));
        let primary_name = self
            .memory
            .profile_get("name")
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| "the primary".to_string());
        // FAMILY PHOTO ACCESS: the library is a shared family resource — retrieval and the studio
        // work for members, DELIVERED TO THEIR OWN CHAT, with "me"/"us" resolving around them.
        // (Analysis about people — gift/tastes/closet studies — stays on the primary's path.)
        let member_chat = self.chat_of_member(&id.owner).await;
        if let Some(req) = creative_request(user_text) {
            return self.photo_create_for(&req, member_chat, Some(&name)).await;
        }
        if photo_followup(user_text) && (self.photo_session_active() || photo_followup_strong(user_text)) {
            return self.photo_followup_turn(user_text, member_chat).await;
        }
        if let Some(q) = photo_request(user_text) {
            return self.photo_find_and_send_for(&q, member_chat, Some(&name)).await;
        }
        // Their reminders, tasks, and daily-brief switch — owner-keyed, delivered to their chat.
        if let Some(reply) = self.member_task_turn(&id.owner, &name, user_text).await {
            return reply;
        }
        // Looser photo intent for members: event phrasings without a photo-noun ("get one from
        // Aadrisha's last birthday") still reach retrieval instead of the chat model.
        if let Some(q) = member_photo_intent(user_text) {
            return self.photo_find_and_send_for(&q, member_chat, Some(&name)).await;
        }
        let recent = self.memory.recent_messages(12, &mind_types::AccessContext::Principal(id.viewer())).await.unwrap_or_default();
        let convo = if recent.is_empty() {
            "(first conversation — greet them warmly by name, and in ONE short line mention what you can do for them: find family photos ('show me photos of…'), make collages, set reminders ('remind me to…'), and a daily morning brief ('brief me daily'))".to_string()
        } else {
            recent.iter().map(|(role, text)| format!("{role}: {text}")).collect::<Vec<_>>().join("\n")
        };
        let sys = format!(
            "{}\n\nSPEAKER CONTEXT (hard rules):\n- You are talking with {name}{} — a registered family member, on THEIR own private channel.\n- Address {name} by name. NEVER address or confuse them with {primary_name} (the primary user).\n- {primary_name}'s private information, notes, plans, purchases and surprises are OFF-LIMITS here. If asked about them, say warmly that it's private.\n- What {name} shares with you is THEIR private space — treasure it for them.\n- Capabilities like photo finding, collages, reminders and the daily brief are handled AUTOMATICALLY before your reply. If such a request still reached you, the handler missed it — ask them to rephrase (e.g. 'show me photos of Aadrisha'). You CANNOT attach or send files yourself: NEVER say you are sending/attaching a photo and NEVER claim one was sent.",
            self.persona,
            if rel.is_empty() { String::new() } else { format!(" ({primary_name}'s {rel})") },
        );
        let prompt = format!(
            "Recent conversation with {name}:\n{convo}\n\n{name}: {user_text}\n\nReply as the companion — warm, natural, concise. No preamble."
        );
        let cfg = GenerationConfig { max_tokens: 700, ..GenerationConfig::default() };
        match self.inference.chat(vec![ChatMessage::system(&sys), ChatMessage::user(&prompt)], cfg).await {
            Ok(r) => r.text.trim().to_string(),
            Err(e) => format!("(I hit a snag thinking just now: {e})"),
        }
    }

}
