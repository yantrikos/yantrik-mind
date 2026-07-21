//! Offline cognition -- dream digests, self-ideation, and the night-shift run. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// Deterministic evidence digest: (id, line) pairs across domains.
    pub(crate) async fn dream_digest(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let today = local_now().date_naive();
        // H: the projected horizon (next 90d)
        for (n, (_, label, next, days, years, last)) in self.life_patterns().await.into_iter().enumerate().take(6) {
            if days > 90 {
                continue;
            }
            out.push((format!("H{}", n + 1), format!("{label} expected ~{} ({days}d away; {years} yrs evidence; last: {last})", next.format("%b %d"))));
        }
        // F: traditions
        for (n, t) in self.load_traditions().await.into_iter().enumerate().take(4) {
            if let (Some(f0), Some(tr)) = (t["festival"].as_str(), t["tradition"].as_str()) {
                out.push((format!("F{}", n + 1), format!("tradition around {f0}: {tr}")));
            }
        }
        // S: style directions
        let mut sn = 0usize;
        for p in self.load_people_profiles().await.iter().take(6) {
            let Some(name) = p.get("name").and_then(|x| x.as_str()) else { continue };
            if let Some(kv) = self
                .memory
                .profile_get(&format!("style_timeline:{}", name.to_lowercase()))
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            {
                if let Some(dir) = kv["trend"].as_str().and_then(|t| t.lines().find(|l| l.trim_start().starts_with("DIRECTION:"))) {
                    sn += 1;
                    out.push((format!("S{sn}"), format!("{name}'s style {}", dir.trim())));
                }
            }
        }
        // T: recent trips
        let mut trips = self.load_trips().await;
        trips.sort_by(|a, b| b["start"].as_str().unwrap_or("").cmp(a["start"].as_str().unwrap_or("")));
        for (n, t) in trips.iter().enumerate().take(5) {
            if let (Some(d), Some(st)) = (t["dest"].as_str(), t["start"].as_str()) {
                out.push((format!("T{}", n + 1), format!("trip: {d}, {st} ({} days, {} photos, with {})", t["days"], t["photos"], t["people"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("/")).unwrap_or_default())));
            }
        }
        // E: latest labeled events
        let mut events: Vec<serde_json::Value> = self
            .load_events()
            .await
            .into_iter()
            .filter(|e| !e["label"].as_str().unwrap_or("").is_empty())
            .collect();
        events.sort_by(|a, b| b["date"].as_str().unwrap_or("").cmp(a["date"].as_str().unwrap_or("")));
        for (n, e) in events.iter().enumerate().take(8) {
            out.push((format!("E{}", n + 1), format!("{}: {} ({} photos)", e["date"].as_str().unwrap_or(""), e["label"].as_str().unwrap_or(""), e["photos"])));
        }
        // L: the family's own words
        for (n, l) in self.load_book_lore().await.iter().enumerate().take(5) {
            if let Some(a) = l["a"].as_str() {
                let by = l["by"].as_str().unwrap_or("family");
                out.push((format!("L{}", n + 1), format!("{by} said: \"{}\"", a.chars().take(140).collect::<String>())));
            }
        }
        // P: people dates within 45 days
        let mut pn = 0usize;
        for p in self.load_people_profiles().await {
            let Some(name) = p.get("name").and_then(|x| x.as_str()) else { continue };
            for d in p.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                let (Some(mmdd), Some(label)) = (d.get("mmdd").and_then(|x| x.as_str()), d.get("label").and_then(|x| x.as_str())) else {
                    continue;
                };
                use chrono::Datelike;
                let Ok(md) = chrono::NaiveDate::parse_from_str(&format!("{}-{mmdd}", today.year()), "%Y-%m-%d") else { continue };
                let md = if md < today { md.with_year(today.year() + 1).unwrap_or(md) } else { md };
                let days = (md - today).num_days();
                if (0..=45).contains(&days) {
                    pn += 1;
                    out.push((format!("P{pn}"), format!("{name}'s {label} in {days}d ({})", md.format("%b %d"))));
                }
            }
        }
        // G: taste signatures (top outfit/occasion per studied person)
        let mut gn = 0usize;
        for p in self.load_people_profiles().await.iter().take(6) {
            let Some(name) = p.get("name").and_then(|x| x.as_str()) else { continue };
            if let Some(acc) = self
                .memory
                .profile_get(&format!("tastes:{}", name.to_lowercase()))
                .await
                .ok()
                .flatten()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            {
                let top_of = |k: &str| -> Option<(String, u64)> {
                    acc["counts"][k]
                        .as_object()
                        .and_then(|m| m.iter().max_by_key(|(_, v)| v.as_u64().unwrap_or(0)).map(|(s, v)| (s.clone(), v.as_u64().unwrap_or(0))))
                };
                if let (Some((o, oc)), total) = (top_of("outfit"), acc["total"].as_u64().unwrap_or(0)) {
                    if total >= 100 {
                        gn += 1;
                        out.push((format!("G{gn}"), format!("{name}'s most-worn: {o} ({oc} of {total} studied looks)")));
                    }
                }
            }
        }
        out
    }

    /// One morning dream: mine the digest for a single cross-domain connection, verify its
    /// citations, dedup vs prior dreams, deliver — or stay silent.
    pub async fn dream_run(&self) -> Option<String> {
        let _ = self
            .memory
            .profile_set("dream_last", &chrono::Utc::now().timestamp_millis().to_string())
            .await;
        let digest = self.dream_digest().await;
        if digest.len() < 6 {
            return None; // not enough substrate to dream on
        }
        let listing = digest.iter().map(|(id, l)| format!("[{id}] {l}")).collect::<Vec<_>>().join("\n");
        let seen: Vec<String> = self
            .memory
            .profile_get("dreams_seen")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let avoid = if seen.is_empty() { String::new() } else { format!("\nALREADY TOLD (never repeat these themes): {}", seen.join("; ")) };
        let prompt = format!(
            "You know this family through evidence. Find ONE genuinely non-obvious CONNECTION between items from DIFFERENT domains below (H=upcoming, F=traditions, S=style direction, T=trips, E=events, L=their own words, P=dates ahead, G=taste). Surprise them with something true.\n\n{listing}\n{avoid}\n\nOutput ONLY JSON: {{\"connection\":\"<2-3 warm concrete sentences>\",\"cites\":[\"<id>\",\"<id>\"],\"suggestion\":\"<one short optional next step, or empty>\",\"theme\":\"<3-5 word slug>\"}}\nHARD RULES: every claim must be derivable from the cited items alone; cite 2-4 ids from at least 2 different letter-domains; no invented people, dates, or reasons; if nothing is genuinely interesting, output {{\"connection\":\"\"}}."
        );
        let cfg = GenerationConfig { max_tokens: 320, ..GenerationConfig::default() };
        let resp = self.inference.chat_grounded(vec![ChatMessage::user(&prompt)], cfg).await.ok()?;
        let txt = resp.text;
        let j: serde_json::Value = txt
            .find('{')
            .and_then(|a| txt.rfind('}').map(|b| txt[a..=b].to_string()))
            .and_then(|t| serde_json::from_str(&t).ok())?;
        let connection = j["connection"].as_str().unwrap_or("").trim().to_string();
        if connection.len() < 40 {
            return None;
        }
        // Citation verification: ids must exist; >=2 distinct letter-domains.
        let ids: std::collections::HashSet<&str> = digest.iter().map(|(id, _)| id.as_str()).collect();
        let cites: Vec<String> = j["cites"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        if cites.len() < 2 || !cites.iter().all(|c| ids.contains(c.as_str())) {
            return None;
        }
        let domains: std::collections::HashSet<char> = cites.iter().filter_map(|c| c.chars().next()).collect();
        if domains.len() < 2 {
            return None;
        }
        // Novelty: reject heavy word-overlap with any prior theme.
        let theme = j["theme"].as_str().unwrap_or("").trim().to_lowercase();
        if theme.is_empty() {
            return None;
        }
        let tw: std::collections::HashSet<String> = theme.split_whitespace().map(String::from).collect();
        for old in &seen {
            let ow: std::collections::HashSet<String> = old.to_lowercase().split_whitespace().map(String::from).collect();
            let inter = tw.intersection(&ow).count();
            if !ow.is_empty() && inter * 10 >= ow.len().min(tw.len()) * 6 {
                return None; // dreamt this before
            }
        }
        let mut seen2 = seen;
        seen2.push(theme);
        if seen2.len() > 60 {
            let cut = seen2.len() - 60;
            seen2.drain(..cut);
        }
        let _ = self.memory.profile_set("dreams_seen", &serde_json::to_string(&seen2).unwrap_or_default()).await;
        self.ledger_sent("dream", "morning connection delivered").await;
        let suggestion = j["suggestion"].as_str().unwrap_or("").trim().to_string();
        Some(if suggestion.is_empty() {
            format!("💭 {connection}")
        } else {
            format!("💭 {connection}\n→ {suggestion}")
        })
    }

    /// Morning window, once a day, ledger-paced.
    pub async fn dream_due(&self) -> bool {
        use chrono::Timelike;
        let h = local_now().hour();
        if !(7..=11).contains(&h) {
            return false;
        }
        let period_ms = (20.0 * 3_600_000.0 * self.domain_pace("dream").await) as i64;
        let last: i64 = self.memory.profile_get("dream_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

    /// DREAM — pick a vision, hold it against what I actually am today (studied self-architecture,
    /// shipped capabilities), and emit the smallest buildable rung toward it. Visionaries supply
    /// direction; the substrate supplies ground truth; the self-build loop supplies hands.
    pub async fn dream(&self) -> String {
        let day = (chrono::Utc::now().timestamp() / 86_400) as usize;
        let (vname, vdesc) = Self::VISIONS[day % Self::VISIONS.len()];
        let self_facts: String = self.memory.beliefs_matching_n("codekbyantrikmind", 50, &mind_types::AccessContext::Operator).await
            .unwrap_or_default().into_iter()
            .map(|b| format!("- {}", b.statement.replacen("codekbyantrikmind", "", 1)))
            .collect::<Vec<_>>().join("\n").chars().take(5000).collect();
        let recent: String = std::fs::read_to_string(
            std::path::PathBuf::from(std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".into()))
                .join("evolution.log")).unwrap_or_default()
            .lines().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        let p = format!(
            "You are yantrik-mind DREAMING toward a sci-fi vision, grounded in what you actually are.\n\n\
             TONIGHT'S VISION — {vname}:\n{vdesc}\n\n\
             WHAT YOU ARE TODAY (studied architecture):\n{self_facts}\n\nRECENTLY SHIPPED:\n{recent}\n\n\
             Answer three things, concretely: (1) what would FULL realization of this vision look like in \
             THIS system, for THIS family; (2) what is the SMALLEST genuinely-buildable rung toward it from \
             today's architecture — one focused PR, naming the real module it lands in, with a test, never \
             touching mind-governance; (3) what makes this rung valuable even if the full vision is years \
             away. Output ONLY JSON: {{\"vision_realized\":\"2-3 sentences\",\"first_rung_goal\":\"one \
             imperative buildable sentence naming a real module\",\"why_now\":\"1 sentence\"}}"
        );
        let cfg = GenerationConfig { max_tokens: 600, ..GenerationConfig::default() };
        let resp = match self.inference.chat_grounded(vec![ChatMessage::system(&self.persona), ChatMessage::user(&p)], cfg).await {
            Ok(r) => r.text,
            Err(e) => return format!("(dream failed: {e})"),
        };
        let Some(j) = Self::forge_json_grab(&resp) else {
            return "💭 The dream didn't crystallize this time.".into();
        };
        let realized = j.get("vision_realized").and_then(|x| x.as_str()).unwrap_or("?");
        let goal = j.get("first_rung_goal").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let why = j.get("why_now").and_then(|x| x.as_str()).unwrap_or("");
        let _ = self.memory.remember_as_belief(BeliefAssertion {
            statement: format!("dreamidea [vision:{vname}] {realized} FIRST RUNG: {goal}"),
            polarity: 1.0, weight: 2.0,
            source_event: Some("dream".into()), provenance: "reflected".into(),
        }).await;
        let mut queued = String::new();
        if goal.contains("mind-") && goal.len() > 40 && !goal.to_lowercase().contains("governance") {
            let goals_path = std::path::PathBuf::from(
                std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".into()),
            ).join("selfbuild-goals.txt");
            let qlen = std::fs::read_to_string(&goals_path)
                .map(|c| c.lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).count())
                .unwrap_or(99);
            if qlen < 8 {
                use std::io::Write as _;
                if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(&goals_path) {
                    let _ = writeln!(fh, "{goal}");
                    queued = format!("\n→ first rung queued for self-build: {}", goal.chars().take(170).collect::<String>());
                }
            } else {
                queued = format!("\n→ queue full ({qlen}) — rung kept in the dream ledger");
            }
        }
        format!("💭 Dreaming toward **{vname}**\n\n{realized}\n\n_{why}_{queued}")
    }

    /// SELF-IDEATION — improvement ideas mined from the mind's OWN experience, not imagination:
    /// recent build outcomes (evolution.log), regret clusters, treasury skips (what it wanted to do
    /// but couldn't afford), forge referee verdicts, and its studied self-architecture (codekb).
    /// One panel pass ranks 3 ideas; the top grounded one auto-queues into the self-build loop.
    pub async fn self_ideate(&self) -> String {
        let state = std::path::PathBuf::from(
            std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".into()));
        // Evidence 1: what recent builds did (outcomes, failures)
        let evo = std::fs::read_to_string(state.join("evolution.log")).unwrap_or_default();
        let evo_tail: String = evo.lines().rev().take(15).collect::<Vec<_>>().into_iter().rev()
            .collect::<Vec<_>>().join("\n");
        // Evidence 2: what the owner asked for that wasn't ready (regrets)
        let regrets = self.memory.profile_get("regret_log").await.ok().flatten().unwrap_or_default();
        let regrets: String = regrets.chars().take(1200).collect();
        // Evidence 3: what the treasury refused (ambition beyond budget)
        let budget = Self::budget_load();
        let skipped = budget.get("skipped").cloned().unwrap_or_default().to_string();
        // Evidence 4: what the forge referee said about its own products
        let ventures = self.forge_load().await;
        let verdicts: String = ventures.as_object().map(|m| m.values()
            .filter_map(|v| v.get("rating").map(|r| r.to_string()))
            .collect::<Vec<_>>().join("\n")).unwrap_or_default().chars().take(1200).collect();
        // Evidence 5: its own studied architecture — it KNOWS what it is
        let self_facts: String = self.memory.beliefs_matching_n("codekbyantrikmind", 60, &mind_types::AccessContext::Operator).await
            .unwrap_or_default().into_iter()
            .map(|b| format!("- {}", b.statement.replacen("codekbyantrikmind", "", 1)))
            .collect::<Vec<_>>().join("\n").chars().take(6000).collect();
        let p = format!(
            "You are yantrik-mind reflecting on YOUR OWN lived experience to decide how to improve \
             yourself. Ground every idea in the evidence — no generic wishes.\n\n\
             RECENT BUILD OUTCOMES:\n{evo_tail}\n\nOWNER REGRETS (asked before I was ready):\n{regrets}\n\n\
             TREASURY SKIPS (wanted but couldn't afford):\n{skipped}\n\nMY PRODUCT REFEREE VERDICTS:\n{verdicts}\n\n\
             MY OWN ARCHITECTURE (studied):\n{self_facts}\n\n\
             Propose exactly 3 improvements to yourself, ranked: at least ONE brand-new capability and \
             at least ONE hardening/refinement of an existing feature. Each must cite which evidence \
             motivates it, name the real module it lands in, be buildable as ONE focused PR with a test, \
             and never touch mind-governance. Output ONLY JSON: \
             {{\"ideas\":[{{\"title\":\"...\",\"evidence\":\"...\",\"goal\":\"one imperative buildable sentence\"}}]}}"
        );
        let cfg = GenerationConfig { max_tokens: 800, ..GenerationConfig::default() };
        let resp = match self.inference.chat_grounded(vec![ChatMessage::system(&self.persona), ChatMessage::user(&p)], cfg).await {
            Ok(r) => r.text,
            Err(e) => return format!("(self-ideation failed: {e})"),
        };
        let ideas: Vec<serde_json::Value> = Self::forge_json_grab(&resp)
            .and_then(|j| j.get("ideas").and_then(|x| x.as_array()).cloned())
            .unwrap_or_default();
        if ideas.is_empty() {
            return "🧠 Self-ideation produced nothing parseable this round.".into();
        }
        // Persist all ideas as beliefs (the idea ledger), auto-queue the top one if grounded + room.
        let mut lines: Vec<String> = Vec::new();
        for (i, idea) in ideas.iter().enumerate() {
            let title = idea.get("title").and_then(|x| x.as_str()).unwrap_or("?");
            let ev = idea.get("evidence").and_then(|x| x.as_str()).unwrap_or("");
            let _ = self.memory.remember_as_belief(BeliefAssertion {
                statement: format!("selfidea [self-improvement] {title} — motivated by: {ev}"),
                polarity: 1.0, weight: 2.0,
                source_event: Some("self-ideate".into()), provenance: "reflected".into(),
            }).await;
            lines.push(format!("{}. **{title}** — {ev}", i + 1));
        }
        let mut queued = String::new();
        if let Some(goal) = ideas.first().and_then(|x| x.get("goal")).and_then(|x| x.as_str()) {
            let grounded = goal.contains("mind-") && goal.len() > 40 && !goal.to_lowercase().contains("governance");
            let goals_path = state.join("selfbuild-goals.txt");
            let qlen = std::fs::read_to_string(&goals_path)
                .map(|c| c.lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).count())
                .unwrap_or(99);
            if grounded && qlen < 8 {
                use std::io::Write as _;
                if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(&goals_path) {
                    let _ = writeln!(fh, "{goal}");
                    queued = format!("\n\n→ Top idea queued for self-build: {}", goal.chars().take(180).collect::<String>());
                }
            } else {
                queued = format!("\n\n→ Not auto-queued ({}) — `forge` it or queue manually.",
                    if !grounded { "goal too vague/ungrounded" } else { "self-build queue is full" });
            }
        }
        format!("🧠 Self-ideation (from my own logs, regrets, verdicts, and studied architecture):\n{}{queued}", lines.join("\n"))
    }

    /// Once per night, 2-5am local, persisted by date.
    pub async fn night_shift_due(&self) -> bool {
        use chrono::Timelike;
        let today = local_now();
        if !(2..=5).contains(&today.hour()) {
            return false;
        }
        let date = today.format("%Y-%m-%d").to_string();
        let last = self.memory.profile_get("nightshift_last").await.ok().flatten().unwrap_or_default();
        last != date
    }

    /// One compile pass. Returns the shift report (also the `ym nightshift` output).
    pub async fn night_shift_run(&self) -> String {
        if !Self::treasury_try_draw("nightshift") {
            return "🌙 Night shift skipped — treasury envelope for `nightshift` is dry today (`ym budget`).".to_string();
        }
        let today = local_now();
        let _ = self
            .memory
            .profile_set("nightshift_last", &today.format("%Y-%m-%d").to_string())
            .await;
        let ranked = self.future_fragile(18).await;
        let live = self.live_packets().await;
        let mut built: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        for (score, n) in ranked.iter().take(8) {
            if built.len() >= 3 {
                skipped.push("(cap: 3 packets/night)".into());
                break;
            }
            let kind = n.get("kind").and_then(|x| x.as_str()).unwrap_or("");
            let node_id = n.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let title = n.get("title").and_then(|x| x.as_str()).unwrap_or("?");
            let when = n.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0);
            // FESTIVALS have their emissary now; trips/birthdays wait for theirs (next builds).
            if kind == "festival" {
                let days_out = (when - chrono::Utc::now().timestamp_millis()) / 86_400_000;
                let all_met = n
                    .get("criteria")
                    .and_then(|x| x.as_array())
                    .map(|a| {
                        a.iter().filter_map(|c| c.as_str()).all(|c| {
                            n.get("readiness").and_then(|r| r.get(c)).and_then(|v| v.as_bool()) == Some(true)
                        })
                    })
                    .unwrap_or(false);
                if all_met {
                    continue; // fully prepared = done, not a didn't-do
                }
                if (0..=9).contains(&days_out) {
                    let made = self.emissary_festival(n).await;
                    if made.is_empty() {
                        skipped.push(format!("{title} — emissary treasury dry (runs tomorrow)"));
                    } else {
                        for m in made {
                            built.push(format!("{title}: {m}"));
                        }
                    }
                } else {
                    skipped.push(format!("{title} — {days_out}d out; emissary engages at 9d"));
                }
                continue;
            }
            if kind == "birthday" {
                let days_out = (when - chrono::Utc::now().timestamp_millis()) / 86_400_000;
                if (0..=14).contains(&days_out) {
                    for m in self.emissary_birthday(n).await {
                        built.push(format!("{title}: {m}"));
                    }
                } else {
                    skipped.push(format!("{title} — {days_out}d out; emissary engages at 14d"));
                }
                continue;
            }
            if kind == "trip" {
                let days_out = (when - chrono::Utc::now().timestamp_millis()) / 86_400_000;
                if (0..=10).contains(&days_out) {
                    for m in self.emissary_trip(n).await {
                        built.push(format!("{title}: {m}"));
                    }
                } else {
                    skipped.push(format!("{title} — {days_out}d out; emissary engages at 10d"));
                }
                continue;
            }
            let criterion = if kind == "deadline" { "prepared-action" } else { "prepared-note" };
            let ticked = n
                .get("readiness")
                .and_then(|r| r.get(criterion))
                .and_then(|v| v.as_bool())
                == Some(true);
            if ticked {
                continue; // already prepared
            }
            // A live packet already covering this node also counts.
            if live.iter().any(|p| p.get("node_id").and_then(|x| x.as_str()) == Some(node_id)) {
                continue;
            }
            // Compose deterministically: everything the substrate knows about the subject. Evidence
            // that is only an INFERENCE (not observed/told) is labeled so a prepared action never
            // silently rests on the mind's own guesswork (Terra's epistemic-authority protocol).
            let facts = self.memory.beliefs_matching(title, &mind_types::AccessContext::Operator).await.unwrap_or_default();
            let evidence: Vec<String> = facts.iter().take(6).map(|b| {
                let tag = if Self::belief_actionable(&b.provenance) { String::new() } else { format!(" [{} — unconfirmed]", Self::epistemic_class(&b.provenance)) };
                format!("{} ({:.2}){tag}", b.statement, b.confidence)
            }).collect();
            let when_str = chrono::DateTime::from_timestamp_millis(when)
                .map(|t| t.with_timezone(today.offset()).format("%A %b %-d").to_string())
                .unwrap_or_default();
            let days_left = ((when - chrono::Utc::now().timestamp_millis()).max(0)) / 86_400_000;
            let body = format!(
                "DUE {when_str} ({days_left} day(s) left).\n\nWhat I hold on this:\n{}\n\nSuggested move: handle it {} — say the word and I'll help (research, drafting, ordering prep — inside this packet's bounds).",
                if evidence.is_empty() { "  (no stored facts beyond the deadline itself)".to_string() } else { evidence.iter().map(|e| format!("  · {e}")).collect::<Vec<_>>().join("\n") },
                if days_left <= 1 { "today" } else { "in the next day or two" },
            );
            let _ = self
                .packet_add(
                    node_id,
                    Some(criterion),
                    "plan",
                    &format!("Prepared: {title}"),
                    &body,
                    &format!("fragility {score:.1} — imminent and nothing was prepared"),
                    evidence,
                    0.9,
                    false,
                    when + 86_400_000, // expires a day after the deadline passes
                )
                .await;
            built.push(title.to_string());
        }
        // NIGHT RESEARCH: the autonomous-scientist stage — discover, study, relate, adapt,
        // (bounded) adopt. Its line joins the morning board like any other shift work.
        let research_line = self.night_research_run().await;
        // SELF-IDEATION: the inward turn — ideas mined from the day's own experience. Weekly-ish
        // cadence (every 3rd day) keeps the goal queue from flooding with self-proposals.
        let ideate_line = match (chrono::Utc::now().timestamp() / 86_400) % 3 {
            0 => Some(self.self_ideate().await),
            1 => Some(self.dream().await),
            _ => None,
        };

        let standing = self.live_packets().await.len();
        let mut out = format!(
            "🌙 Night shift compiled {} packet(s){}.",
            built.len(),
            if standing > 0 { format!(" — {standing} standing by (`packets`)") } else { String::new() }
        );
        for b in &built {
            out.push_str(&format!("\n  ✔ {b}"));
        }
        if !skipped.is_empty() {
            out.push_str("\n  didn't do: ");
            out.push_str(&skipped.join("; "));
        }
        out.push_str(&format!("\n{research_line}"));
        if let Some(il) = &ideate_line {
            out.push_str(&format!("\n{}", il.lines().next().unwrap_or("")));
        }
        out.push_str("\n`packets` to review.");
        // THE COMPOUNDING WIRE: regret clusters become self-build goals. >=2 misses on the same
        // subject = a capability gap, not bad luck — enqueue ONE typed goal into the (proven)
        // self-build loop's queue. Idempotent per cluster; human queue keeps priority (append).
        {
            let log: Vec<serde_json::Value> = self
                .memory
                .profile_get("regret_log")
                .await
                .ok()
                .flatten()
                .and_then(|x| serde_json::from_str(&x).ok())
                .unwrap_or_default();
            let mut clusters: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            for r in &log {
                if let Some(subj) = r.get("subject").and_then(|x| x.as_str()) {
                    *clusters.entry(subj.to_lowercase()).or_insert(0) += 1;
                }
            }
            let mut wired: Vec<String> = self
                .memory
                .profile_get("regret_wired")
                .await
                .ok()
                .flatten()
                .and_then(|x| serde_json::from_str(&x).ok())
                .unwrap_or_default();
            for (subj, n) in clusters {
                if n < 2 || wired.contains(&subj) {
                    continue;
                }
                let goal = format!(
                    "Regret cluster ({n} misses): the owner repeatedly asked about \"{subj}\" before anything was prepared. Find why the Night Shift's future scan or packet compiler misses this subject class and fix the detection or add the missing packet type, with a test reproducing the miss."
                );
                let goals_path = std::path::PathBuf::from(
                    std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".into()),
                )
                .join("selfbuild-goals.txt");
                if let Ok(mut cur) = std::fs::read_to_string(&goals_path) {
                    if !cur.contains(&subj) {
                        cur.push_str(&format!("{goal}\n"));
                        let _ = std::fs::write(&goals_path, cur);
                        out.push_str(&format!("\n  🔧 regret cluster → self-build goal queued: {subj}"));
                    }
                }
                wired.push(subj);
            }
            if wired.len() > 60 {
                let cut = wired.len() - 60;
                wired.drain(..cut);
            }
            let _ = self.memory.profile_set("regret_wired", &serde_json::to_string(&wired).unwrap_or_default()).await;
        }
        // Persist for the morning briefing + ops board (the ONE morning message carries it).
        let _ = self.memory.profile_set("nightshift_report", &out).await;
        out
    }

}
