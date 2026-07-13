//! Decision ledger -- future packets, node ticks, fragility scan, regret classification. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) async fn load_packets(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("action_packets")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn save_packets(&self, v: &[serde_json::Value]) {
        let _ = self
            .memory
            .profile_set("action_packets", &serde_json::Value::Array(v.to_vec()).to_string())
            .await;
    }

    /// Author one packet (emissaries call this). Returns the packet id. If `satisfies` names a
    /// readiness criterion on the linked node, the node is ticked immediately — proposed work
    /// counts as readiness; a rejection un-ticks it.
    #[allow(clippy::too_many_arguments)]
    pub async fn packet_add(
        &self,
        node_id: &str,
        satisfies: Option<&str>,
        kind: &str,          // checklist | plan | draft | cart | info
        title: &str,
        body: &str,
        reason: &str,
        evidence: Vec<String>,
        confidence: f64,
        confirmation_required: bool,
        expiry_ms: i64,
    ) -> String {
        let now = chrono::Utc::now().timestamp_millis();
        let id = format!("pkt:{:x}", now);
        let mut store = self.load_packets().await;
        store.push(serde_json::json!({
            "id": id, "node_id": node_id, "satisfies": satisfies, "kind": kind,
            "title": title, "body": body, "reason": reason, "evidence": evidence,
            "confidence": confidence, "confirmation_required": confirmation_required,
            "expiry_ms": expiry_ms, "status": "proposed", "created_ms": now,
            "alternatives_rejected": [],
        }));
        // keep the store bounded; drop the oldest terminal packets first
        if store.len() > 200 {
            store.retain(|p| {
                matches!(p.get("status").and_then(|x| x.as_str()), Some("proposed") | Some("confirmed"))
            });
        }
        self.save_packets(&store).await;
        if let Some(criterion) = satisfies {
            self.node_tick(node_id, criterion, true).await;
        }
        self.ledger_sent("packet", &format!("prepared: {title}")).await;
        id
    }

    /// Tick (or un-tick) one readiness criterion on a FutureNode.
    pub(crate) async fn node_tick(&self, node_id: &str, criterion: &str, done: bool) {
        let mut nodes: Vec<serde_json::Value> = self
            .memory
            .profile_get("future_nodes")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_default();
        for n in nodes.iter_mut() {
            if n.get("id").and_then(|x| x.as_str()) == Some(node_id) {
                if let Some(obj) = n.as_object_mut() {
                    obj.entry("readiness").or_insert_with(|| serde_json::json!({}));
                    if let Some(r) = obj.get_mut("readiness").and_then(|x| x.as_object_mut()) {
                        r.insert(criterion.to_string(), serde_json::json!(done));
                    }
                }
            }
        }
        let _ = self
            .memory
            .profile_set("future_nodes", &serde_json::Value::Array(nodes).to_string())
            .await;
    }

    /// Lazily expire, then return live packets (proposed first, then confirmed), newest last.
    pub(crate) async fn live_packets(&self) -> Vec<serde_json::Value> {
        let now = chrono::Utc::now().timestamp_millis();
        let mut store = self.load_packets().await;
        let mut changed = false;
        for p in store.iter_mut() {
            let live = matches!(p.get("status").and_then(|x| x.as_str()), Some("proposed"));
            let exp = p.get("expiry_ms").and_then(|x| x.as_i64()).unwrap_or(i64::MAX);
            if live && exp < now {
                p["status"] = serde_json::json!("expired");
                changed = true;
                // an expired packet no longer vouches for readiness
                if let (Some(nid), Some(c)) = (
                    p.get("node_id").and_then(|x| x.as_str()).map(String::from),
                    p.get("satisfies").and_then(|x| x.as_str()).map(String::from),
                ) {
                    self.node_tick(&nid, &c, false).await;
                }
            }
        }
        if changed {
            self.save_packets(&store).await;
        }
        store
            .into_iter()
            .filter(|p| {
                matches!(p.get("status").and_then(|x| x.as_str()), Some("proposed") | Some("confirmed"))
            })
            .collect()
    }

    /// `ym packets` — the live board: what's prepared, what needs a word.
    pub async fn packets_view(&self) -> String {
        let live = self.live_packets().await;
        if live.is_empty() {
            return "📦 No live packets. The Night Shift compiles them against the future nodes (`ym future`).".to_string();
        }
        let mut out = String::from("📦 ACTION PACKETS (live)\n");
        for (i, p) in live.iter().enumerate() {
            let title = p.get("title").and_then(|x| x.as_str()).unwrap_or("?");
            let kind = p.get("kind").and_then(|x| x.as_str()).unwrap_or("?");
            let st = p.get("status").and_then(|x| x.as_str()).unwrap_or("?");
            let conf = p.get("confirmation_required").and_then(|x| x.as_bool()).unwrap_or(false);
            out.push_str(&format!(
                "{}. [{kind}] {title} — {st}{}\n",
                i + 1,
                if conf && st == "proposed" { " · NEEDS YOUR WORD (`approve N` / `reject N`)" } else { "" }
            ));
        }
        out.push_str("`packet N` shows the full proof (reason, evidence, expiry).");
        out
    }

    /// `ym packet N` — the full proof-carrying view.
    pub async fn packet_show(&self, sel: &str) -> String {
        let live = self.live_packets().await;
        let idx = sel.trim().parse::<usize>().ok().and_then(|n| n.checked_sub(1));
        let Some(p) = idx.and_then(|i| live.get(i)) else {
            return "Which packet? `packets` lists them; `packet 2` shows one.".to_string();
        };
        let g = |k: &str| p.get(k).and_then(|x| x.as_str()).unwrap_or("—").to_string();
        let ev: Vec<String> = p
            .get("evidence")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| format!("  · {x}")).collect())
            .unwrap_or_default();
        let exp = p
            .get("expiry_ms")
            .and_then(|x| x.as_i64())
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|t| t.with_timezone(local_now().offset()).format("%a %b %-d %H:%M").to_string())
            .unwrap_or_else(|| "never".into());
        format!(
            "📦 {}\nkind: {} · status: {} · confidence: {:.2} · expires: {exp}\nnode: {} (satisfies: {})\n\nWHY: {}\n\nEVIDENCE:\n{}\n\n{}",
            g("title"),
            g("kind"),
            g("status"),
            p.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0),
            g("node_id"),
            g("satisfies"),
            g("reason"),
            if ev.is_empty() { "  (deterministic composition)".to_string() } else { ev.join("\n") },
            g("body"),
        )
    }

    /// `approve N` / `reject N [why]` — the human word. Rejection un-ticks readiness and records
    /// the why as a correction the replay lab learns from.
    pub async fn packet_decide(&self, sel: &str, approve: bool, why: &str) -> String {
        let live = self.live_packets().await;
        let idx = sel.trim().parse::<usize>().ok().and_then(|n| n.checked_sub(1));
        let Some(target) = idx.and_then(|i| live.get(i)) else {
            return "Which packet? `packets` lists them by number.".to_string();
        };
        let id = target.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let title = target.get("title").and_then(|x| x.as_str()).unwrap_or("?").to_string();
        let mut store = self.load_packets().await;
        for p in store.iter_mut() {
            if p.get("id").and_then(|x| x.as_str()) == Some(id.as_str()) {
                p["status"] = serde_json::json!(if approve { "confirmed" } else { "rejected" });
                if !why.trim().is_empty() {
                    p["decision_why"] = serde_json::json!(why.trim());
                }
            }
        }
        self.save_packets(&store).await;
        if approve {
            self.ledger_resolve(true).await;
            format!("✅ Confirmed: {title}. I'll act within the packet's bounds — nothing beyond it.")
        } else {
            if let (Some(nid), Some(c)) = (
                target.get("node_id").and_then(|x| x.as_str()),
                target.get("satisfies").and_then(|x| x.as_str()),
            ) {
                self.node_tick(nid, c, false).await;
            }
            self.ledger_correction("packet", &title, if why.trim().is_empty() { "rejected" } else { why.trim() }).await;
            format!("🗑 Rejected: {title}{} — noted for the replay lab.", if why.trim().is_empty() { String::new() } else { format!(" ({})", why.trim()) })
        }
    }

    /// Rebuild the forward store for the next `days`, preserving existing node state by id.
    /// Returns the nodes sorted by time. Persisted at KV `future_nodes`.
    pub async fn future_scan(&self, days: i64) -> Vec<serde_json::Value> {
        let today = local_now();
        let now = today.timestamp_millis();
        let horizon = now + days * 86_400_000;
        // Existing state to preserve (readiness ticks, packets, dismissals survive rescans).
        let old: std::collections::HashMap<String, serde_json::Value> = self
            .memory
            .profile_get("future_nodes")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str::<Vec<serde_json::Value>>(&x).ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|n| {
                let id = n.get("id").and_then(|x| x.as_str()).map(String::from)?;
                Some((id, n))
            })
            .collect();
        let slug = |t: &str| -> String {
            t.to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '-' })
                .collect::<String>()
                .split('-')
                .filter(|x| !x.is_empty())
                .take(5)
                .collect::<Vec<_>>()
                .join("-")
        };
        // Per-kind readiness criteria: the packet checklist each node kind demands. These defaults
        // seed the compiler; packets tick them off as they ship.
        let criteria = |kind: &str| -> Vec<&'static str> {
            match kind {
                "festival" => vec!["supplies", "logistics+weather", "story+message"],
                "trip" => vec!["packing", "documents", "weather+fallback", "route+timing"],
                "birthday" => vec!["gift", "card", "plan", "collision-check"],
                "deadline" => vec!["prepared-action"],
                _ => vec!["prepared-note"],
            }
        };
        let mut nodes: Vec<serde_json::Value> = Vec::new();
        let mut push = |title: String, kind: &str, when_ms: i64, end_ms: i64, participants: Vec<String>| {
            let id = format!("{kind}:{}", slug(&title));
            let mut node = old.get(&id).cloned().unwrap_or_else(|| {
                serde_json::json!({
                    "id": id, "readiness": {}, "packets": [], "status": "open",
                })
            });
            node["title"] = serde_json::json!(title);
            node["kind"] = serde_json::json!(kind);
            node["when_ms"] = serde_json::json!(when_ms);
            node["end_ms"] = serde_json::json!(end_ms.max(when_ms));
            node["participants"] = serde_json::json!(participants);
            node["criteria"] = serde_json::json!(criteria(kind));
            nodes.push(node);
        };
        // 1. Calendar entries (festivals carry the fest: prefix; multi-day events keep their window).
        for e in self.load_calendar().await {
            let ms = e.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0);
            if ms < now - 86_400_000 || ms > horizon {
                continue;
            }
            let title = e.get("title").and_then(|x| x.as_str()).unwrap_or("?").to_string();
            let end = e.get("end_ms").and_then(|x| x.as_i64()).unwrap_or(ms);
            let tl = title.to_lowercase();
            // Trip beats festival when both signals appear ("Olathe trip — Puja at cousin's"):
            // the drive is the operational load; the observance rides along.
            let kind = if tl.contains("trip") || tl.contains("travel") || tl.contains("resort") || tl.contains("hotel") || tl.contains("flight") {
                "trip"
            } else if title.starts_with("fest:") || tl.contains("puja") || tl.contains("yatra") || tl.contains("ashtami") {
                "festival"
            } else {
                "event"
            };
            push(title.trim_start_matches("fest:").trim().to_string(), kind, ms, end, vec![]);
        }
        // 1b. The FESTIVALS registry — the authoritative festival dates (they are NOT calendar
        // entries; the twin must read the registry directly or it misses every festival).
        for e in self.load_festival_dates().await {
            let (Some(name), Some(date)) = (e.get("name").and_then(|x| x.as_str()), e.get("date").and_then(|x| x.as_str())) else {
                continue;
            };
            let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else { continue };
            let when_ms = d
                .and_hms_opt(9, 0, 0)
                .and_then(|dt| dt.and_local_timezone(*today.offset()).single())
                .map(|dt| dt.timestamp_millis())
                .unwrap_or(0);
            if when_ms < now - 86_400_000 || when_ms > horizon {
                continue;
            }
            push(name.to_string(), "festival", when_ms, when_ms, vec![]);
        }
        // 2. People dates (birthdays/anniversaries) inside the horizon.
        for (name, label, d, _mmdd) in self.upcoming_people_dates(days).await {
            let kind = if label.to_lowercase().contains("birthday") { "birthday" } else { "event" };
            push(format!("{name}'s {label}"), kind, now + d * 86_400_000, now + d * 86_400_000, vec![name]);
        }
        // 3. Deadlined reminders.
        let (reminders, _) = self.split_tasks().await;
        for t in &reminders {
            if let Some(ms) = t.due_ms.map(|m| m as i64).or_else(|| parse_text_date_ms(&t.description, &today)) {
                if ms >= now && ms <= horizon {
                    push(t.description.chars().take(80).collect(), "deadline", ms, ms, vec![]);
                }
            }
        }
        nodes.sort_by_key(|n| n.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0));
        let _ = self
            .memory
            .profile_set("future_nodes", &serde_json::Value::Array(nodes.clone()).to_string())
            .await;
        nodes
    }

    /// Fragility ranking: deadline proximity × unmet readiness. This is the Night Shift's
    /// dispatch order — the most-imminent, least-ready node gets worked first.
    pub async fn future_fragile(&self, days: i64) -> Vec<(f64, serde_json::Value)> {
        let now = chrono::Utc::now().timestamp_millis();
        let mut out: Vec<(f64, serde_json::Value)> = self
            .future_scan(days)
            .await
            .into_iter()
            .filter(|n| n.get("status").and_then(|x| x.as_str()) != Some("dismissed"))
            .map(|n| {
                let when = n.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(now);
                let days_left = ((when - now).max(0) as f64 / 86_400_000.0).max(0.25);
                let total = n.get("criteria").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(1).max(1);
                let done = n
                    .get("readiness")
                    .and_then(|x| x.as_object())
                    .map(|m| m.values().filter(|v| v.as_bool() == Some(true)).count())
                    .unwrap_or(0);
                let unready = 1.0 - (done as f64 / total as f64);
                // proximity dominates as the date closes in; fully-ready nodes fall to ~0.
                (unready * (10.0 / days_left), n)
            })
            .collect();
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// `ym future` — the forward store, fragility-ranked. The twin's first visible face.
    pub async fn future_view(&self) -> String {
        let ranked = self.future_fragile(21).await;
        if ranked.is_empty() {
            return "🔭 Nothing on the 21-day horizon. (`ym calendar add …` seeds it.)".to_string();
        }
        let today = local_now();
        let mut out = String::from("🔭 FUTURE NODES (21d, fragility-ranked — most imminent × least ready first)\n");
        for (score, n) in ranked.iter().take(12) {
            let title = n.get("title").and_then(|x| x.as_str()).unwrap_or("?");
            let kind = n.get("kind").and_then(|x| x.as_str()).unwrap_or("?");
            let when = n
                .get("when_ms")
                .and_then(|x| x.as_i64())
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|t| t.with_timezone(today.offset()).format("%a %b %-d").to_string())
                .unwrap_or_default();
            let total = n.get("criteria").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
            let done = n
                .get("readiness")
                .and_then(|x| x.as_object())
                .map(|m| m.values().filter(|v| v.as_bool() == Some(true)).count())
                .unwrap_or(0);
            let unmet: Vec<String> = n
                .get("criteria")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str())
                        .filter(|c| {
                            n.get("readiness").and_then(|r| r.get(*c)).and_then(|v| v.as_bool()) != Some(true)
                        })
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            out.push_str(&format!(
                "• [{kind}] {title} — {when} · readiness {done}/{total} · fragility {score:.1}{}\n",
                if unmet.is_empty() { String::new() } else { format!(" · needs: {}", unmet.join(", ")) }
            ));
        }
        out
    }

    /// Deterministic classification of one primary ask. No LLM, a few KV reads, called inline.
    pub async fn regret_classify(&self, user_text: &str) {
        let t = user_text.trim();
        // Commands and micro-turns aren't asks; don't pollute the curve with "ym privacy".
        if t.len() < 12 || is_cli_verb(t) || t.starts_with('/') {
            return;
        }
        let stop = ["what", "when", "where", "there", "about", "have", "this", "that", "with",
                    "will", "would", "could", "should", "going", "know", "need", "want", "does"];
        let words: Vec<String> = t
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 4 && !stop.contains(w))
            .map(String::from)
            .collect();
        if words.is_empty() {
            return;
        }
        // Forward spine = calendar (incl. fest: entries) + people dates + deadlined reminders.
        let spine = self.upcoming_spine(21).await;
        let hit: Option<String> = spine.iter().find_map(|(_, label)| {
            let ll = label.to_lowercase();
            let ltoks: std::collections::HashSet<String> = ll
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() >= 4 && !stop.contains(w))
                .map(String::from)
                .collect();
            words.iter().any(|w| ltoks.contains(w)).then(|| label.clone())
        });
        let now = chrono::Utc::now();
        let today = local_now();
        let week = format!("{}-W{:02}", today.format("%G"), chrono::Datelike::iso_week(&today).week());
        let mut stats: serde_json::Value = self
            .memory
            .profile_get("regret_stats")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let wk = stats
            .as_object_mut()
            .map(|m| m.entry(week.clone()).or_insert_with(|| serde_json::json!({"asks":0,"linked":0,"anticipated":0,"missed":0})))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"asks":0,"linked":0,"anticipated":0,"missed":0}));
        let bump = |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0) + 1;
        let mut wk2 = wk.clone();
        wk2["asks"] = serde_json::json!(bump(&wk, "asks"));
        let class = match &hit {
            None => "unforeseeable",
            Some(label) => {
                wk2["linked"] = serde_json::json!(bump(&wk, "linked"));
                // Was there a LIVE PACKET for this subject? Real prepared-work records first
                // (the honest signal); the spoken-recently proxy stays as the soft fallback.
                let ll = label.to_lowercase();
                let subj: Vec<String> = ll
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.len() >= 4 && !stop.contains(w))
                    .map(String::from)
                    .collect();
                let packets = self.live_packets().await;
                let packed = packets.iter().any(|p| {
                    let hay = format!(
                        "{} {}",
                        p.get("title").and_then(|x| x.as_str()).unwrap_or(""),
                        p.get("node_id").and_then(|x| x.as_str()).unwrap_or("")
                    )
                    .to_lowercase();
                    subj.iter().any(|w| hay.contains(w.as_str()))
                });
                let recent = self.memory.recent_messages(80, &mind_types::AccessContext::Operator).await.unwrap_or_default();
                let spoken = packed
                    || recent
                        .iter()
                        .filter(|(r, _)| r == "assistant")
                        .any(|(_, txt)| {
                            let xl = txt.to_lowercase();
                            subj.iter().any(|w| xl.contains(w.as_str()))
                        });
                if spoken {
                    wk2["anticipated"] = serde_json::json!(bump(&wk, "anticipated"));
                    "anticipated"
                } else {
                    wk2["missed"] = serde_json::json!(bump(&wk, "missed"));
                    "missed_foreseeable"
                }
            }
        };
        if let Some(m) = stats.as_object_mut() {
            m.insert(week, wk2);
            // keep at most 12 weeks of stats
            if m.len() > 12 {
                let mut keys: Vec<String> = m.keys().cloned().collect();
                keys.sort();
                for old in keys.iter().take(m.len() - 12) {
                    m.remove(old);
                }
            }
        }
        let _ = self.memory.profile_set("regret_stats", &stats.to_string()).await;
        // A miss is a RegretRecord — the raw material for regression tests + self-build goals.
        if class == "missed_foreseeable" {
            let mut log: Vec<serde_json::Value> = self
                .memory
                .profile_get("regret_log")
                .await
                .ok()
                .flatten()
                .and_then(|x| serde_json::from_str(&x).ok())
                .unwrap_or_default();
            log.push(serde_json::json!({
                "ts": now.timestamp_millis(),
                "ask": t.chars().take(160).collect::<String>(),
                "subject": hit,
                "class": class,
                "prepared": serde_json::Value::Null, // becomes a packet id once ActionPackets exist
            }));
            if log.len() > 300 {
                let cut = log.len() - 300;
                log.drain(..cut);
            }
            let _ = self.memory.profile_set("regret_log", &serde_json::Value::Array(log).to_string()).await;
        }
    }

    /// `ym regrets` — the curve so far + the recent misses. This is the metric the Night Shift
    /// will be judged against; week 1 is the untreated baseline.
    pub async fn regrets_report(&self) -> String {
        let stats: serde_json::Value = self
            .memory
            .profile_get("regret_stats")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let mut out = String::from("📉 PREVENTABLE-ASK CURVE (charter metric — must decline once the Night Shift ships)\n");
        let mut weeks: Vec<(String, serde_json::Value)> = stats
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        weeks.sort_by(|a, b| a.0.cmp(&b.0));
        if weeks.is_empty() {
            out.push_str("(no asks classified yet — the log just turned on)\n");
        }
        for (wk, v) in &weeks {
            let g = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
            let (linked, missed) = (g("linked"), g("missed"));
            let rate = if linked > 0 { format!("{:.0}%", missed as f64 * 100.0 / linked as f64) } else { "—".to_string() };
            out.push_str(&format!(
                "{wk}: {} asks · {} foreseeable · {} anticipated · {} MISSED → preventable-ask rate {rate}\n",
                g("asks"), linked, g("anticipated"), missed
            ));
        }
        let log: Vec<serde_json::Value> = self
            .memory
            .profile_get("regret_log")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_default();
        if !log.is_empty() {
            out.push_str("\nRecent misses (what I should have prepared):\n");
            for r in log.iter().rev().take(8) {
                let ask = r.get("ask").and_then(|x| x.as_str()).unwrap_or("?");
                let subj = r.get("subject").and_then(|x| x.as_str()).unwrap_or("?");
                out.push_str(&format!("• \"{ask}\" — foreseeable via: {subj}\n"));
            }
        }
        out
    }

}
