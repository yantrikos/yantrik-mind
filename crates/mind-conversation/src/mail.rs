//! Mail / inbox -- full-mailbox search, deep report, inbox analytics, sweep rules. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// Every connected read-only scan inbox (falls back to the bot's own mailbox when none).
    /// Search the FULL mailboxes of every configured account (INBOX + archive/All Mail) —
    /// bookings, receipts, confirmation numbers. The digests read recent windows; this reads
    /// everything the accounts hold.
    pub async fn mail_search_all(&self, query: &str) -> String {
        let q = query.trim();
        if q.len() < 3 {
            return "Search for what? (subject words, sender, a confirmation number…)".to_string();
        }
        let inboxes = self.scan_inboxes();
        if inboxes.is_empty() {
            return "No mail accounts configured.".to_string();
        }
        let mut sections: Vec<String> = Vec::new();
        let mut errors = 0usize;
        for (addr, client) in &inboxes {
            match client.search(q, 4).await {
                Ok(hits) if !hits.is_empty() => {
                    let lines: Vec<String> = hits
                        .iter()
                        .map(|(m, body)| {
                            let cleanish: String = body
                                .split_whitespace()
                                .filter(|w| !w.contains('{') && !w.contains('}') && !w.starts_with('@') && !w.starts_with('.') && !w.contains("=09"))
                                .collect::<Vec<_>>()
                                .join(" ");
                            // Match-centered window: receipts are long and the relevant line (hotel,
                            // dates, amount) is wherever the query hits — show ±260 chars around it,
                            // not just the head.
                            let low = cleanish.to_lowercase();
                            let anchor = q.to_lowercase();
                            let first_word = anchor.split_whitespace().next().unwrap_or(&anchor);
                            let hit = low.find(&anchor).or_else(|| low.find(first_word));
                            let snip: String = match hit {
                                Some(pos) => {
                                    let start = cleanish.char_indices().rev().find(|(i, _)| *i <= pos.saturating_sub(120)).map(|(i, _)| i).unwrap_or(0);
                                    cleanish[start..].chars().take(520).collect()
                                }
                                None => cleanish.chars().take(360).collect(),
                            };
                            format!("• [{}] {} — {}\n  {}", m.date, m.from, m.subject, snip)
                        })
                        .collect();
                    sections.push(format!("{addr}:\n{}", lines.join("\n")));
                }
                Ok(_) => {}
                Err(_) => errors += 1,
            }
        }
        if sections.is_empty() {
            let err_note = if errors > 0 { format!(" ({errors} account(s) unreachable)") } else { String::new() };
            return format!(
                "📬 Searched the full mailboxes of {} account(s) for \"{q}\" — no matching message{err_note}. If it exists, it's in an account I don't scan yet.",
                inboxes.len()
            );
        }
        format!("📬 Mail search \"{q}\":\n\n{}", sections.join("\n\n"))
    }

    pub(crate) fn scan_inboxes(&self) -> Vec<(String, Arc<dyn MailClient>)> {
        if !self.scan_mail.is_empty() {
            return self.scan_mail.clone();
        }
        self.mail.as_ref().map(|m| ("inbox".to_string(), m.clone())).into_iter().collect()
    }

    /// ---------- DEEP MAIL REPORT ----------
    /// The mailbox as a LIFE LEDGER: hundreds of headers per account, aggregated per sender with
    /// cadence detection, classified once, amounts verified by body-peek for the top recurrers.
    /// Detached (IMAP + 2 LLM passes + peeks take minutes); delivered via the notify queue.
    /// Findings compound: clear-amount subscriptions land in the tracker, dated renewals land on
    /// the calendar spine (source "mail", replaced per report).
    pub async fn mail_report(&self, per_account: usize) -> String {
        let inboxes = self.scan_inboxes();
        if inboxes.is_empty() {
            return "No inboxes connected yet — set YM_SCAN_EMAIL (+ _2.._6) with app passwords.".to_string();
        }
        let guard = "mailreport".to_string();
        if !self.studies.lock().unwrap().insert(guard.clone()) {
            return "A mail report is already running — it lands here shortly.".to_string();
        }
        let n = per_account.clamp(100, 600);
        let n_accounts = inboxes.len();
        let mem = self.memory.clone();
        let nq = self.notify_queue.clone();
        let studies = self.studies.clone();
        let inference = self.inference.clone();
        let known_people: Vec<String> = self
            .load_people_profiles()
            .await
            .iter()
            .filter_map(|p| p.get("name").and_then(|x| x.as_str()).map(String::from))
            .collect();
        let rules = self.mail_rules().await;
        let subs_now = self.load_subs().await;
        let cal_now = self.load_calendar().await;
        tokio::spawn(async move {
            let mut report_sections: Vec<String> = Vec::new();
            let mut new_subs: Vec<serde_json::Value> = Vec::new();
            let mut cal_events: Vec<serde_json::Value> = Vec::new();
            for (label, m) in &inboxes {
                let msgs = match m.inbox(n).await {
                    Ok(v) => v,
                    Err(e) => {
                        report_sections.push(format!("— {label}: unreachable ({e})"));
                        continue;
                    }
                };
                if msgs.is_empty() {
                    continue;
                }
                // Deterministic aggregation: per-sender counts, times, sample subjects.
                let mut agg: std::collections::HashMap<String, SenderAgg> = std::collections::HashMap::new();
                let (mut t_min, mut t_max) = (i64::MAX, 0i64);
                for msg in &msgs {
                    let key = msg.from.trim().to_string();
                    if key.is_empty() {
                        continue;
                    }
                    let e = agg.entry(key.clone()).or_insert_with(|| SenderAgg {
                        addr: key,
                        count: 0,
                        times: Vec::new(),
                        subjects: Vec::new(),
                    });
                    e.count += 1;
                    if let Some(ms) = parse_mail_date(&msg.date) {
                        e.times.push(ms);
                        t_min = t_min.min(ms);
                        t_max = t_max.max(ms);
                    }
                    if e.subjects.len() < 3 && !msg.subject.trim().is_empty() {
                        e.subjects.push(msg.subject.trim().chars().take(90).collect());
                    }
                }
                let span_days = if t_max > t_min && t_min != i64::MAX { ((t_max - t_min) / 86_400_000).max(1) } else { 0 };
                // Rank senders; keep the top 60 for classification.
                let mut senders: Vec<SenderAgg> = agg.into_values().collect();
                senders.sort_by(|a, b| b.count.cmp(&a.count));
                senders.truncate(60);
                let table: String = senders
                    .iter_mut()
                    .map(|se| {
                        let cad = cadence_label(&mut se.times).unwrap_or("-");
                        format!("{} | ×{} | {} | {}", se.addr, se.count, cad, se.subjects.join(" ⸱ "))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                // One classification pass over the aggregate (not per email — that's the trick).
                let people_csv = known_people.join(", ");
                let rules_line = if rules.is_empty() { String::new() } else { format!("\nUSER RULES (override): {}", rules.join(" | ")) };
                let classify = format!(
                    "Sender table from the user's mailbox: sender | count | cadence | sample subjects.\nClassify EACH sender. Output ONLY a JSON array: [{{\"sender\":\"<sender>\",\"type\":\"subscription|bill|shop|newsletter|service|human|other\",\"name\":\"<clean service/person name>\",\"amount\":<number if a recurring amount is visible in subjects, else null>,\"renewal\":\"<YYYY-MM-DD if a renewal/expiry date is visible, else null>\"}}].\nKnown people (always type human): {people_csv}.{rules_line}\n\n{table}"
                );
                let cfg = GenerationConfig { max_tokens: 2600, ..GenerationConfig::default() };
                let classified: Vec<serde_json::Value> = match inference
                    .chat(vec![ChatMessage::system("You classify email senders from aggregates. Output only JSON."), ChatMessage::user(&classify)], cfg)
                    .await
                {
                    Ok(r) => {
                        let body = r.text.rsplit("</think>").next().unwrap_or(&r.text).to_string();
                        match (body.find('['), body.rfind(']')) {
                            (Some(x), Some(y)) if y > x => serde_json::from_str(&body[x..=y]).unwrap_or_default(),
                            _ => Vec::new(),
                        }
                    }
                    Err(_) => Vec::new(),
                };
                // Assemble the account's ledger view.
                let (mut subs, mut bills, mut shops, mut humans, mut services) =
                    (Vec::new(), Vec::new(), Vec::new(), Vec::new(), 0usize);
                let mut monthly_total = 0f64;
                for c in &classified {
                    let ty = c.get("type").and_then(|x| x.as_str()).unwrap_or("other");
                    let nm = c.get("name").and_then(|x| x.as_str()).unwrap_or("?").to_string();
                    let amt = c.get("amount").and_then(|x| x.as_f64());
                    let renewal = c.get("renewal").and_then(|x| x.as_str()).unwrap_or("");
                    let sender = c.get("sender").and_then(|x| x.as_str()).unwrap_or("");
                    let cad = senders.iter().find(|se| se.addr == sender).and_then(|se| {
                        let mut t = se.times.clone();
                        cadence_label(&mut t)
                    });
                    match ty {
                        "subscription" | "bill" => {
                            let line = format!(
                                "• {nm}{}{}",
                                cad.map(|c| format!(" — {c}")).unwrap_or_default(),
                                amt.map(|a| format!(", ~${a:.2}")).unwrap_or_default()
                            );
                            if ty == "subscription" { subs.push(line) } else { bills.push(line) }
                            if let Some(a) = amt {
                                monthly_total += match cad {
                                    Some("yearly") => a / 12.0,
                                    Some("quarterly") => a / 3.0,
                                    Some("weekly") => a * 4.3,
                                    _ => a,
                                };
                                // Auto-track: a named recurring charge with a clear amount.
                                let already = subs_now.iter().chain(new_subs.iter()).any(|t| {
                                    t.get("name").and_then(|x| x.as_str()).map(|x| x.eq_ignore_ascii_case(&nm)).unwrap_or(false)
                                });
                                if !already && ty == "subscription" {
                                    new_subs.push(serde_json::json!({ "name": nm, "amount": a, "cycle": cad.unwrap_or("monthly") }));
                                }
                            }
                            if renewal.len() == 10 {
                                if let Ok(d) = chrono::NaiveDate::parse_from_str(renewal, "%Y-%m-%d") {
                                    if let Some(ms) = d.and_hms_opt(9, 0, 0).map(|t| t.and_utc().timestamp_millis()) {
                                        cal_events.push(serde_json::json!({ "id": ms, "title": format!("{nm} renewal (mail)"), "when_ms": ms, "source": "mail" }));
                                    }
                                }
                            }
                        }
                        "shop" => shops.push(format!("• {nm} ×{}", senders.iter().find(|se| se.addr == sender).map(|se| se.count).unwrap_or(0))),
                        "human" => humans.push(format!("• {nm} ×{}", senders.iter().find(|se| se.addr == sender).map(|se| se.count).unwrap_or(0))),
                        "service" => services += 1,
                        _ => {}
                    }
                }
                let mut sec = format!(
                    "📊 {label} — {} emails over ~{span_days} day(s) (~{}/day)",
                    msgs.len(),
                    if span_days > 0 { (msgs.len() as i64 / span_days).max(1) } else { msgs.len() as i64 }
                );
                if !subs.is_empty() {
                    sec.push_str(&format!(
                        "\n\n💳 RECURRING (visible ≈ ${monthly_total:.0}/mo):\n{}",
                        subs.join("\n")
                    ));
                }
                if !bills.is_empty() {
                    sec.push_str(&format!("\n\n🏦 BILLS & UTILITIES:\n{}", bills.join("\n")));
                }
                if !shops.is_empty() {
                    sec.push_str(&format!("\n\n🧾 SHOPPING (by volume):\n{}", shops.join("\n")));
                }
                if !humans.is_empty() {
                    sec.push_str(&format!("\n\n👤 PEOPLE writing to you:\n{}", humans.join("\n")));
                }
                sec.push_str(&format!(
                    "\n\n🌐 ACCOUNT SURFACE: {} distinct senders; ~{services} service/notification accounts",
                    senders.len()
                ));
                report_sections.push(sec);
            }
            // Compound the findings.
            let mut notes: Vec<String> = Vec::new();
            if !new_subs.is_empty() {
                let mut tracked = subs_now.clone();
                let names: Vec<String> = new_subs.iter().filter_map(|s| s.get("name").and_then(|x| x.as_str()).map(String::from)).collect();
                tracked.extend(new_subs);
                let _ = mem.profile_set("subscriptions", &serde_json::to_string(&tracked).unwrap_or_default()).await;
                notes.push(format!("auto-tracked {} new subscription(s): {}", names.len(), names.join(", ")));
            }
            if !cal_events.is_empty() {
                let mut evs: Vec<serde_json::Value> = cal_now
                    .into_iter()
                    .filter(|e| e.get("source").and_then(|x| x.as_str()) != Some("mail"))
                    .collect();
                let n_new = cal_events.len();
                evs.extend(cal_events);
                let _ = mem.profile_set("calendar_events", &serde_json::to_string(&evs).unwrap_or_default()).await;
                notes.push(format!("{n_new} renewal date(s) added to the calendar"));
            }
            let tail = if notes.is_empty() { String::new() } else { format!("\n\n✅ {}", notes.join("; ")) };
            if report_sections.is_empty() {
                nq.lock().unwrap().push("📊 Mail report: couldn't read any inbox.".to_string());
            } else {
                nq.lock().unwrap().push(format!("{}{tail}", report_sections.join("\n\n———\n\n")));
            }
            studies.lock().unwrap().remove(&guard);
        });
        format!(
            "📊 Deep mail report started — reading up to {n} emails per account ({n_accounts} account(s)), aggregating senders, verifying the money. It lands here in a few minutes."
        )
    }

    /// CROSS-ACCOUNT EMAIL ANALYTICS, two-stage: headers from every inbox → triage picks the few
    /// messages worth OPENING → BODY.PEEK verifies their actual STATE (canceled vs confirmed,
    /// real amounts, real dates) → digest. Read-only throughout (peek never marks anything).
    pub async fn inbox_analytics(&self, per_account: usize) -> String {
        let inboxes = self.scan_inboxes();
        if inboxes.is_empty() {
            return "No inboxes connected yet — set YM_SCAN_EMAIL (+ _2.._6 for more accounts) with app passwords.".to_string();
        }
        let n = per_account.clamp(10, 60);
        let mut blocks: Vec<String> = Vec::new();
        let mut counts: Vec<String> = Vec::new();
        for (label, m) in &inboxes {
            match m.inbox(n).await {
                Ok(msgs) => {
                    counts.push(format!("{label} ✓{}", msgs.len()));
                    blocks.push(
                        msgs.iter()
                            .map(|x| format!("- [{label}] #{} {} | {} | {}", x.id, x.date, x.from, x.subject))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
                Err(e) => counts.push(format!("{label} ✗ ({e})")),
            }
        }
        if blocks.is_empty() {
            return format!("Couldn't read any inbox — {}.", counts.join("; "));
        }
        let headers = blocks.join("\n");
        // Stage 2: which few are worth OPENING? (state-ambiguous threads, bills, reservations)
        let cfg_small = GenerationConfig { max_tokens: 300, ..GenerationConfig::default() };
        let triage = format!(
            "Email headers, one per line as: - [account] #id date | from | subject.\nPick UP TO 6 whose BODY should be opened to verify state or extract amounts/dates — reservations/orders (could be canceled!), bills, deadlines, anything ambiguous. Output ONLY JSON: [{{\"account\":\"<account>\",\"id\":\"<id>\"}}].\n\n{headers}"
        );
        let mut opened = String::new();
        if let Ok(r) = self
            .inference
            .chat(vec![ChatMessage::system("You triage email headers. Output only JSON."), ChatMessage::user(&triage)], cfg_small)
            .await
        {
            let body = r.text.rsplit("</think>").next().unwrap_or(&r.text).to_string();
            let picks: Vec<serde_json::Value> = match (body.find('['), body.rfind(']')) {
                (Some(x), Some(y)) if y > x => serde_json::from_str(&body[x..=y]).unwrap_or_default(),
                _ => Vec::new(),
            };
            // Group picked ids per account and peek their bodies.
            for (label, m) in &inboxes {
                let ids: Vec<String> = picks
                    .iter()
                    .filter(|p| p.get("account").and_then(|x| x.as_str()) == Some(label.as_str()))
                    .filter_map(|p| p.get("id").and_then(|x| x.as_str()).map(String::from))
                    .filter(|id| id.chars().all(|c| c.is_ascii_digit()))
                    .take(6)
                    .collect();
                if ids.is_empty() {
                    continue;
                }
                if let Ok(bodies) = m.peek_bodies(&ids, 900).await {
                    for (id, text) in bodies {
                        if !text.trim().is_empty() {
                            opened.push_str(&format!("\n=== [{label}] #{id} OPENED ===\n{}\n", text.trim()));
                        }
                    }
                }
            }
        }
        // Stage 3: the digest — strict taxonomy (exclusion rules beat vibes), user rules override.
        let known_people: String = {
            let mut names: Vec<String> = self
                .load_people_profiles()
                .await
                .iter()
                .filter_map(|p| p.get("name").and_then(|x| x.as_str()).map(String::from))
                .collect();
            names.sort();
            names.dedup();
            names.join(", ")
        };
        let user_rules = self.mail_rules().await;
        let rules_block = if user_rules.is_empty() {
            String::new()
        } else {
            format!(
                "\nUSER RULES (these OVERRIDE every category rule):\n{}",
                user_rules.iter().enumerate().map(|(i, r)| format!("{}. {r}", i + 1)).collect::<Vec<_>>().join("\n")
            )
        };
        let prompt = format!(
            "Recent email HEADERS from the user's {} account(s), plus OPENED bodies for the few that matter. Produce a terse cross-account digest.\n\nCATEGORY RULES (strict — exclusions beat instincts):\nNEEDS ACTION: ONLY when inaction has a real consequence — payment due, deadline, reply owed to a real person, pending refund/dispute, delivery needing presence. Marketing calls-to-action (confirm/verify/rate/review/update-your-profile) are NEVER needs-action.\nFROM PEOPLE: written by an actual human personally addressing the user — never automated mail. Known people: {known}.\nMONEY IN MOTION: money still moving — bills due, upcoming renewals, pending refunds, price/plan changes. Completed order receipts are NOT money in motion.\nPURCHASES: order/shipping confirmations of already-completed purchases — compact, amounts only if shown.\nRESOLVED: threads verified closed in OPENED content (canceled, refunded, delivered, settled).\nWORTH A LOOK: important information with no action needed (statements ready, results, policy changes).\nNOISE: one line — promos, newsletters, engagement bait ('confirm your accounts', surveys, rate-your-purchase), and routine security notifications for activity the user likely caused themselves (e.g. an app password or 2FA change made today); rough count + top 3 senders.\n{rules}\nOmit any empty section. Keep [account] tags. Never invent amounts, dates, senders, or states.\n\nHEADERS:\n{headers}\n{opened}",
            inboxes.len(),
            known = known_people,
            rules = rules_block,
        );
        let cfg = GenerationConfig { max_tokens: 900, ..GenerationConfig::default() };
        match self
            .inference
            .chat(vec![ChatMessage::system("You analyze email. Terse, factual, never invent amounts, senders, or states."), ChatMessage::user(&prompt)], cfg)
            .await
        {
            Ok(r) => format!(
                "📬 {} — {}\n\n{}",
                if inboxes.len() == 1 { "1 inbox".to_string() } else { format!("{} inboxes", inboxes.len()) },
                counts.join(" · "),
                r.text.trim()
            ),
            Err(e) => format!("Read the inboxes ({}) but couldn't analyze: {e}", counts.join(" · ")),
        }
    }

    /// User-taught mail categorization rules — corrections become permanent behavior.
    pub(crate) async fn mail_rules(&self) -> Vec<String> {
        self.memory
            .profile_get("mail_rules")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn save_mail_rules(&self, v: &[String]) {
        let _ = self
            .memory
            .profile_set("mail_rules", &serde_json::to_string(v).unwrap_or_default())
            .await;
    }

    /// Daily mail-sweep gate (YM_MAILSWEEP_SECS, default daily) — data check runs quietly; the
    /// user only hears about it when something actually needs them.
    pub async fn mail_sweep_due(&self) -> bool {
        if self.scan_inboxes().is_empty() {
            return false;
        }
        let period_ms: i64 = std::env::var("YM_MAILSWEEP_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(86_400) * 1000;
        let period_ms = (period_ms as f64 * self.domain_pace("mail").await) as i64;
        let last: i64 = self.memory.profile_get("mail_sweep_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

    /// Run the sweep; return Some(digest) ONLY when NEEDS ACTION or MONEY has real entries —
    /// silence-biased proactivity (an empty day should cost the user zero attention).
    pub async fn mail_sweep_run(&self) -> Option<String> {
        let _ = self
            .memory
            .profile_set("mail_sweep_last", &chrono::Utc::now().timestamp_millis().to_string())
            .await;
        let digest = self.inbox_analytics(30).await;
        let has = |sec: &str| {
            digest
                .split(sec)
                .nth(1)
                .map(|rest| rest.lines().take(4).any(|l| l.trim_start().starts_with("- ")))
                .unwrap_or(false)
        };
        if has("NEEDS ACTION") || has("MONEY IN MOTION") || has("FROM PEOPLE") {
            self.ledger_sent("mail", "daily sweep flagged something needing action").await;
            Some(format!("📬 Mail sweep — something needs you:\n\n{digest}"))
        } else {
            eprintln!("[mail] sweep clean — staying quiet");
            None
        }
    }

}
