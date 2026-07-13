//! Onboarding + whois -- interview capture, capability seeding, unknown-face questions. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// Capture the user's answer to the current onboarding question into its slot, then ADVANCE the
    /// interview in the same breath (name → purpose → first grounded follow-up) so it flows as a real
    /// conversation rather than one question a day. Stores both a durable belief and the profile KV.
    pub(crate) async fn capture_onboard(&self, slot: &str, text: &str) -> String {
        match slot {
            "name" => {
                let name = Self::clean_name(text);
                let _ = self.memory.profile_set("name", &name).await;
                let _ = self.memory.remember_as_belief(BeliefAssertion {
                    statement: format!("The user's name is {name}"),
                    polarity: 1.0, weight: 1.0, source_event: Some("onboard".into()), provenance: "told".into(),
                }).await;
                self.set_pending_slot(Some("purpose")).await;
                format!("Good to meet you, {name}. What would you most like me to help you with — the main thing you'd want from me? Knowing that lets me actually be useful rather than generic.")
            }
            "purpose" => {
                let purpose: String = text.trim().chars().take(240).collect();
                let _ = self.memory.profile_set("purpose", &purpose).await;
                let _ = self.memory.remember_as_belief(BeliefAssertion {
                    statement: format!("The user wants me to help with: {purpose}"),
                    polarity: 1.0, weight: 1.0, source_event: Some("onboard".into()), provenance: "told".into(),
                }).await;
                match self.purpose_followup(&purpose).await {
                    Some(q) => format!("Got it — I'll keep that as my north star. {q}"),
                    None => "Got it — I'll keep that as my north star. Tell me more whenever you like.".to_string(),
                }
            }
            s if s.starts_with("interest:") => {
                let key = s.trim_start_matches("interest:").to_string();
                let ans: String = text.trim().chars().take(400).collect();
                let _ = self.memory.profile_set(&format!("interest_{key}"), &ans).await;
                let _ = self
                    .memory
                    .remember_as_belief(BeliefAssertion {
                        statement: format!("{} {ans}", interest_belief_prefix(&key)),
                        polarity: 1.0,
                        weight: 0.9,
                        source_event: Some("onboard".into()),
                        provenance: "told".into(),
                    })
                    .await;
                // The anniversary is a PEOPLE-LAYER date, not just a belief: parse it and pin it on the
                // spouse's profile so it rolls into "Coming up" / the briefing like a birthday does.
                if key == "dates" {
                    let today = local_now();
                    if let Some(ms) = parse_text_date_ms(&ans, &today) {
                        let mmdd = chrono::DateTime::from_timestamp_millis(ms)
                            .map(|t| t.with_timezone(today.offset()).format("%m-%d").to_string());
                        if let Some(mmdd) = mmdd {
                            let mut store = self.load_people_profiles().await;
                            if let Some(i) = store.iter().position(|p| {
                                p.get("relationship").and_then(|x| x.as_str()).map(|r| r.contains("wife")).unwrap_or(false)
                            }) {
                                let mut dates = store[i].get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                                dates.retain(|d| d.get("label").and_then(|x| x.as_str()) != Some("wedding anniversary"));
                                dates.push(serde_json::json!({ "label": "wedding anniversary", "mmdd": mmdd }));
                                store[i]["dates"] = serde_json::json!(dates);
                                self.save_people_profiles(&store).await;
                            }
                        }
                    }
                }
                self.mark_ask_covered(&key).await;
                // A rich answer often covers MORE than the asked dimension (anniversary + parents +
                // topics can arrive in one message). One cheap extraction pass: which OTHER uncovered
                // dimensions does this message already answer? Store + mark those too — and never
                // chain a question the user just answered ("Told you all these bro").
                let covered0 = self.ask_covered().await;
                let remaining: Vec<(&str, &str)> = INTEREST_DIMS
                    .iter()
                    .filter(|(k, _)| !covered0.iter().any(|c| c == k))
                    .copied()
                    .collect();
                if !remaining.is_empty() && ans.chars().count() > 60 {
                    let dims_list = remaining.iter().map(|(k, q)| format!("- {k}: {q}")).collect::<Vec<_>>().join("
");
                    let prompt = format!(
                        "The user wrote:
\"\"\"
{ans}
\"\"\"

Which of these questions does that message ALREADY answer (fully or partly)? Output ONLY JSON: {{\"answered\":[{{\"key\":\"<key>\",\"answer\":\"<the part of their message that answers it>\"}}]}} — empty list if none.

{dims_list}"
                    );
                    let cfg = GenerationConfig { max_tokens: 400, ..GenerationConfig::default() };
                    if let Ok(r) = self.inference.chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg).await {
                        let v = parse_json_obj(&r.text);
                        for a in v.get("answered").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                            let (Some(k2), Some(ans2)) = (a.get("key").and_then(|x| x.as_str()), a.get("answer").and_then(|x| x.as_str())) else { continue };
                            if !remaining.iter().any(|(rk, _)| *rk == k2) || ans2.trim().len() < 4 {
                                continue;
                            }
                            let ans2: String = ans2.trim().chars().take(400).collect();
                            let _ = self.memory.profile_set(&format!("interest_{k2}"), &ans2).await;
                            let _ = self
                                .memory
                                .remember_as_belief(BeliefAssertion {
                                    statement: format!("{} {ans2}", interest_belief_prefix(k2)),
                                    polarity: 1.0,
                                    weight: 0.9,
                                    source_event: Some("onboard".into()),
                                    provenance: "told".into(),
                                })
                                .await;
                            self.mark_ask_covered(k2).await;
                        }
                    }
                }
                // Chain only a question that is STILL genuinely unanswered.
                let covered = self.ask_covered().await;
                match INTEREST_DIMS.iter().find(|(k, _)| !covered.iter().any(|c| c == k)) {
                    Some((nk, nq)) => {
                        self.set_pending_slot(Some(&format!("interest:{nk}"))).await;
                        format!("Love that — noted. {nq}")
                    }
                    None => "Got it — that gives me a real feel for you, and I'll put it to use.".to_string(),
                }
            }
            s if s.starts_with("mergeface:") => {
                // mergeface:<display>:<target_pid>:<cand_pid> — confirm to unify a person's timeline.
                let parts: Vec<String> = s.trim_start_matches("mergeface:").splitn(3, ':').map(String::from).collect();
                let t = text.trim().to_lowercase();
                if parts.len() != 3 {
                    return "That merge slot looks malformed — ignoring it.".to_string();
                }
                let (display, target_pid, cand_pid) = (parts[0].clone(), parts[1].clone(), parts[2].clone());
                if ["yes", "y", "yeah", "yep", "correct", "merge", "confirm", "do it"].iter().any(|w| t == *w || t.starts_with(w)) {
                    let sources = mind_tools::PhotoSource::all_from_env();
                    let Some(src) = sources.iter().find(|s| s.knows_people()) else {
                        return "Photo library unreachable right now — merge not done.".to_string();
                    };
                    if src.merge_people(&target_pid, &[cand_pid]).await {
                        let _ = self
                            .memory
                            .remember_as_belief(BeliefAssertion {
                                statement: format!("{display}'s younger-self cluster was confirmed and merged into their person in the photo library — their timeline now spans the full archive"),
                                polarity: 1.0,
                                weight: 0.9,
                                source_event: Some("younger-self-merge".into()),
                                provenance: "told".into(),
                            })
                            .await;
                        self.ledger_correction("photos", &format!("younger-self of {display}"), "confirmed + merged").await;
                        format!("🕵️ Merged — {display}'s timeline now includes those years. Give the library a minute, then `thennow {display}` for the real then-and-now.")
                    } else {
                        "The merge call failed on the library side — nothing changed. I'll leave the evidence as is.".to_string()
                    }
                } else if ["no", "n", "nope", "not", "wrong", "skip"].iter().any(|w| t == *w || t.starts_with(w)) {
                    self.ledger_resolve(true).await;
                    // Remember the rejection forever; offer the next-best candidate if one waits.
                    let mut rej: Vec<String> = self
                        .memory
                        .profile_get("youngerself_no")
                        .await
                        .ok()
                        .flatten()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();
                    if !rej.contains(&cand_pid) {
                        rej.push(cand_pid.clone());
                    }
                    let _ = self.memory.profile_set("youngerself_no", &serde_json::to_string(&rej).unwrap_or_default()).await;
                    let key = format!("youngerself_cands_{}", display.to_lowercase());
                    let mut cands: Vec<serde_json::Value> = self
                        .memory
                        .profile_get(&key)
                        .await
                        .ok()
                        .flatten()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();
                    if let Some(next) = if cands.is_empty() { None } else { Some(cands.remove(0)) } {
                        let _ = self.memory.profile_set(&key, &serde_json::Value::Array(cands).to_string()).await;
                        let sources = mind_tools::PhotoSource::all_from_env();
                        if let Some(src) = sources.iter().find(|s| s.knows_people()) {
                            if let (Some(nid), Some(thumb)) = (
                                next["id"].as_str().map(String::from),
                                match next["id"].as_str() {
                                    Some(nid) => src.face_thumbnail(nid).await,
                                    None => None,
                                },
                            ) {
                                let cap = format!(
                                    "🕵️ Understood, not them. Next candidate for {display}'s younger self: {} photos, {}–{}, family co-occurrence {:.0}%. Same question — is this {display}? (yes/no)",
                                    next["count"], next["y0"], next["y1"],
                                    next["co"].as_f64().unwrap_or(0.0) * 100.0
                                );
                                self.photo_queue.lock().unwrap().push((thumb, cap, None));
                                self.set_pending_slot(Some(&format!("mergeface:{display}:{target_pid}:{nid}"))).await;
                                return "🕵️ Noted as not them — sending the next candidate now.".to_string();
                            }
                        }
                    }
                    "Understood — left unmerged, and I'll remember that cluster isn't them.".to_string()
                } else {
                    "Just yes or no on this one — is that photo the same person, younger?".to_string()
                }
            }
            s if s.starts_with("book:") => {
                // book:<year>|book:origin — an answer for the family book. Store as lore, then
                // REWRITE that chapter immediately so the answer visibly lands in the book.
                let key = s.trim_start_matches("book:").to_string();
                let year: i64 = if key == "origin" { 0 } else { key.parse().unwrap_or(0) };
                let t = text.trim();
                let low = t.to_lowercase();
                if ["skip", "pass", "idk", "no idea", "not sure", "later", "dont know", "don't know"]
                    .iter()
                    .any(|w| low == *w || low.starts_with(w))
                {
                    return "No problem — the book keeps that page open.".to_string();
                }
                // A clearly-typed COMMAND (not a memory) accidentally landed on an open book question —
                // don't store it as lore (that pollutes the book). Set the page aside; let them resend.
                if Self::wants_draft(t).is_some()
                    || Self::wants_deep_research(t).is_some()
                    || low.starts_with("draft ")
                    || low.starts_with("research ")
                    || low.starts_with("code:")
                    || low.starts_with("write a script")
                    || low.contains("://")
                {
                    self.set_pending_slot(None).await;
                    return "That looked like a command, not a book memory — I've set that book question aside. Send it again and I'll run it.".to_string();
                }
                let teller = self.memory.profile_get("name").await.ok().flatten().filter(|n| !n.is_empty()).unwrap_or_else(|| "the family".to_string());
                let mut lore = self.load_book_lore().await;
                lore.push(serde_json::json!({
                    "year": year, "q": format!("about {}", if year == 0 { "the beginning".to_string() } else { year.to_string() }),
                    "a": t.chars().take(600).collect::<String>(),
                    "by": teller,
                    "ts": chrono::Utc::now().timestamp_millis(),
                }));
                let _ = self.memory.profile_set("book_lore", &serde_json::Value::Array(lore.clone()).to_string()).await;
                let _ = self
                    .memory
                    .remember_as_belief(BeliefAssertion {
                        statement: format!(
                            "Family lore ({}): {}",
                            if year == 0 { "origins".to_string() } else { year.to_string() },
                            t.chars().take(300).collect::<String>()
                        ),
                        polarity: 1.0,
                        weight: 0.9,
                        source_event: Some("book-lore".into()),
                        provenance: "told".into(),
                    })
                    .await;
                self.ledger_correction("book", &format!("chapter gap {year}"), &format!("told: {}", t.chars().take(120).collect::<String>())).await;
                let _ = self.book_redraft(year).await;
                let ylabel = if year == 0 { "the prologue".to_string() } else { format!("chapter {year}") };
                format!("📖 That's in the book — I've rewritten {ylabel} with it. `book {}` to read it.", if year == 0 { "origin".to_string() } else { year.to_string() })
            }
            s if s.starts_with("plans:") => {
                // plans:<festival>:<year> — the user is answering "what are the plans?"
                let mut it = s.trim_start_matches("plans:").splitn(2, ':');
                let fest = it.next().unwrap_or("").to_string();
                let year = it.next().unwrap_or("").to_string();
                let t = text.trim();
                let low = t.to_lowercase();
                if ["skip", "pass", "idk", "no idea", "not sure", "no plans", "nothing yet", "later", "dont know", "don't know"]
                    .iter()
                    .any(|w| low == *w || low.starts_with(w))
                {
                    self.ledger_resolve(true).await;
                    return format!("No plans yet — noted. I'll check back as {fest} gets closer.");
                }
                let _ = self
                    .memory
                    .remember_as_belief(BeliefAssertion {
                        statement: format!("Plan for {fest} {year}: {}", t.chars().take(300).collect::<String>()),
                        polarity: 1.0,
                        weight: 0.85,
                        source_event: Some("festival-plans".into()),
                        provenance: "told".into(),
                    })
                    .await;
                self.ledger_correction("anticipate", &format!("plans for {fest} {year}"), &format!("captured: {}", t.chars().take(120).collect::<String>())).await;
                format!("🪔 Noted for {fest} — \"{}\". I'll keep it in mind as it approaches.", t.chars().take(140).collect::<String>())
            }
            s if s.starts_with("event:") => {
                // event:<date> — the user is telling us what a heavily-photographed day WAS.
                let date = s.trim_start_matches("event:").to_string();
                let t = text.trim();
                let low = t.to_lowercase();
                if ["skip", "pass", "idk", "no idea", "not sure", "dont know", "don't know", "later"].iter().any(|w| low == *w || low.starts_with(w)) {
                    return "No problem — that day stays a mystery for now.".to_string();
                }
                // Distill a short label from a natural reply.
                let prompt = format!(
                    "The user was shown photos from one day and asked what the occasion was. They replied: \"{t}\". Output ONLY a short event label (3-8 words, properly capitalized), e.g. 'Aadrisha's Annaprashan ceremony' or 'Housewarming party'."
                );
                let cfg = GenerationConfig { max_tokens: 60, ..GenerationConfig::default() };
                let label = self
                    .inference
                    .chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg)
                    .await
                    .ok()
                    .map(|r| r.text.trim().trim_matches('"').chars().take(60).collect::<String>())
                    .filter(|l| l.len() > 3)
                    .unwrap_or_else(|| t.chars().take(60).collect());
                let mut events = self.load_events().await;
                let mut who = String::new();
                let mut photos = 0u64;
                for e in events.iter_mut() {
                    if e["date"].as_str() == Some(date.as_str()) {
                        e["label"] = serde_json::json!(label);
                        e["src"] = serde_json::json!("told");
                        who = e["people"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                        photos = e["photos"].as_u64().unwrap_or(0);
                    }
                }
                self.save_events(&events).await;
                let _ = self.memory.remember_as_belief(BeliefAssertion {
                    statement: format!(
                        "Life event (told): {label} on {date} ({photos} photos{})",
                        if who.is_empty() { String::new() } else { format!(", with {who}") }
                    ),
                    polarity: 1.0, weight: 0.9, source_event: Some("event-learn".into()), provenance: "told".into(),
                }).await;
                self.ledger_correction("events", &format!("unknown day {date}"), &format!("learned: {label}")).await;
                format!("🎪 Learned — {date} was **{label}**. It's part of the family story now; `event {date}` anytime.")
            }
            s if s.starts_with("whois:") => {
                // whois:<source>:<person_id>:<count> — the user is answering a who-is-this question.
                let mut it = s.splitn(4, ':');
                let (_, source, pid, count) = (it.next(), it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or("?"));
                let t = text.trim();
                let low = t.to_lowercase();
                if ["skip", "pass", "idk", "no idea", "not sure", "dont know", "don't know", "later", "leave it"]
                    .iter()
                    .any(|w| low == *w || low.starts_with(w))
                {
                    return "No problem — skipping that face; I won't ask about it again.".to_string();
                }
                // Natural replies carry more than a name ("that's my cousin Ritu") — extract both.
                let prompt = format!(
                    "The user was shown a face photo and asked who it is. They replied: \"{t}\". Output ONLY JSON {{\"name\":\"<the person's name, properly capitalized>\",\"relationship\":\"<their relation to the user (wife/son/cousin/friend/colleague/...), or empty if not stated>\"}}."
                );
                let cfg = GenerationConfig { max_tokens: 120, ..GenerationConfig::default() };
                let v = self
                    .inference
                    .chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg)
                    .await
                    .map(|r| parse_json_obj(&r.text))
                    .unwrap_or_default();
                let name = v
                    .get("name")
                    .and_then(|x| x.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| s.len() > 1)
                    .unwrap_or_else(|| Self::clean_name(t));
                let rel = v.get("relationship").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
                // JUNK GATE + RELATIONAL RESOLUTION: "my wife" / "that's my wife's mom" carry a
                // RELATION, not a name — resolve through the family, or re-ask. A phrase must
                // never become a person's name (it polluted Immich once: "That's my wife's").
                let name = {
                    let nl = name.to_lowercase();
                    let junk = name.split_whitespace().count() > 3
                        || nl.contains("that")
                        || nl.contains("this")
                        || nl.contains("whose")
                        || nl.starts_with("my ")
                        || nl.contains(" my ")
                        || nl.ends_with("'s")
                        || nl.ends_with("\u{2019}s");
                    if !junk && name.len() > 1 {
                        name
                    } else {
                        // resolve pure relations via the household registry + profiles
                        let phrase = low.clone();
                        let members = self.load_people().await;
                        let member_name = |want: &str| -> Option<String> {
                            members
                                .iter()
                                .find(|p| p.get("relationship").and_then(|x| x.as_str()) == Some(want))
                                .and_then(|p| p.get("name").and_then(|x| x.as_str()).map(String::from))
                        };
                        let profiles = self.load_people_profiles().await;
                        let profile_named = |n: &str| -> Option<String> {
                            profiles
                                .iter()
                                .find(|p| p.get("name").and_then(|x| x.as_str()).map(|x| x.eq_ignore_ascii_case(n)).unwrap_or(false))
                                .and_then(|p| p.get("name").and_then(|x| x.as_str()).map(String::from))
                        };
                        let spouse = member_name("wife").or_else(|| member_name("husband"));
                        let resolved = if phrase.contains("wife") && (phrase.contains("mom") || phrase.contains("mother")) {
                            spouse.as_deref().and_then(|w| profile_named(&format!("{w}'s Mom")))
                        } else if phrase.contains("wife") && (phrase.contains("dad") || phrase.contains("father")) {
                            spouse.as_deref().and_then(|w| profile_named(&format!("{w}'s Dad")))
                        } else if phrase.contains("wife") {
                            member_name("wife")
                        } else if phrase.contains("husband") {
                            member_name("husband")
                        } else if phrase.contains("daughter") {
                            member_name("daughter").or_else(|| profile_named("Aadrisha"))
                        } else if phrase.contains("mom") || phrase.contains("mother") {
                            profile_named("Pranab's Mom")
                        } else if phrase.contains("dad") || phrase.contains("father") {
                            profile_named("Pranab's Dad")
                        } else {
                            None
                        };
                        match resolved {
                            Some(n) => n,
                            None => {
                                // can't resolve — re-arm the slot and ask for JUST the name
                                self.set_pending_slot(Some(s)).await;
                                return "Got the relation — but to name them right, what's their actual name?".to_string();
                            }
                        }
                    }
                };
                // Local face-name map — the source stays read-only; WE remember which cluster is who,
                // so `ym photos <name>` and photo retrieval work for this person from now on.
                let mut fm = self.face_names().await;
                fm.insert(format!("{source}:{pid}"), name.clone());
                self.save_face_names(&fm).await;
                // People layer: enrich an existing profile or start one.
                let ql = name.to_lowercase();
                let mut store = self.load_people_profiles().await;
                let fact = format!("Appears in ~{count} photos in the library ({source} face match)");
                if let Some(p) = store.iter_mut().find(|p| person_matches(p, &ql)) {
                    let mut facts = p.get("facts").and_then(|x| x.as_array()).cloned().unwrap_or_default();
                    if !facts.iter().any(|f| f.as_str().map(|s| s.contains("face match")).unwrap_or(false)) {
                        facts.push(serde_json::json!(fact));
                        p["facts"] = serde_json::json!(facts);
                    }
                    if !rel.is_empty() && p.get("relationship").and_then(|x| x.as_str()).unwrap_or("").is_empty() {
                        p["relationship"] = serde_json::json!(rel);
                    }
                } else {
                    store.push(serde_json::json!({ "name": name, "relationship": rel, "facts": [fact], "dates": [] }));
                }
                self.save_people_profiles(&store).await;
                let _ = self.memory.remember_as_belief(BeliefAssertion {
                    statement: format!(
                        "The face appearing in ~{count} library photos is {name}{}",
                        if rel.is_empty() { String::new() } else { format!(" (the user's {rel})") }
                    ),
                    polarity: 1.0, weight: 0.9, source_event: Some("whois".into()), provenance: "told".into(),
                }).await;
                // WRITE-BACK (opted in): if the source already has a person with this name, MERGE
                // the unnamed cluster into them (recognition anchor gets stronger); otherwise name
                // the cluster itself. Honest suffix only when the write actually landed.
                let mut wrote = String::new();
                if let Some(src_obj) = mind_tools::PhotoSource::all_from_env().into_iter().find(|s| s.name() == source) {
                    let existing = src_obj
                        .list_people()
                        .await
                        .into_iter()
                        .find(|p| !p.name.is_empty() && p.name.to_lowercase() == name.to_lowercase() && p.id != pid);
                    let ok = match &existing {
                        Some(t) => src_obj.merge_people(&t.id, &[pid.to_string()]).await,
                        None => src_obj.name_person(pid, &name).await,
                    };
                    if ok {
                        wrote = if existing.is_some() {
                            format!(" I also merged this face into {name}'s existing cluster in your photo app — recognition just got stronger.")
                        } else {
                            " I also named them in your photo app itself.".to_string()
                        };
                    }
                }
                format!(
                    "Got it — that's {name}{}.{wrote} I can recognize them across the library now; try `ym photos {name}` sometime. 📸",
                    if rel.is_empty() { String::new() } else { format!(", your {rel}") }
                )
            }
            _ => "Thanks.".to_string(),
        }
    }

    /// Ask ONE specific, useful follow-up grounded in the user's stated purpose (the adaptive part of
    /// the interview). None if the LLM doesn't produce a clean question.
    pub(crate) async fn purpose_followup(&self, purpose: &str) -> Option<String> {
        let prompt = format!(
            "The user's main goal for me is: \"{purpose}\". Ask ONE specific, concretely useful follow-up question that would help me help them with that. Reply with ONLY the question."
        );
        let messages = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::system("Ask exactly one concise, specific question. No preamble, no markdown."),
            ChatMessage::user(&prompt),
        ];
        let r = self.inference.chat(messages, GenerationConfig::default()).await.ok()?;
        let q = r.text.trim().lines().last().unwrap_or("").trim().to_string();
        if q.ends_with('?') && q.len() > 8 { Some(q) } else { None }
    }

    /// Strip a few common lead-ins ("my name is", "i'm", "call me") so we store the bare name.
    pub(crate) fn clean_name(s: &str) -> String {
        let t = s.trim();
        let low = t.to_lowercase();
        for p in ["my name is ", "i am ", "i'm ", "call me ", "it's ", "this is ", "name's "] {
            if let Some(rest) = low.strip_prefix(p) {
                let start = t.len() - rest.len();
                return t[start..].trim().trim_end_matches(['.', '!', ',']).chars().take(40).collect();
            }
        }
        t.trim_end_matches(['.', '!', ',']).chars().take(40).collect()
    }

    /// Seed the built-in CAPABILITY skills into YantrikDB (idempotent). Capabilities are DATA, not code:
    /// each is a Skill{lang="capability"} whose `code` is a tiny JSON spec (tool/var/args/label) and whose
    /// summary+tags drive semantic routing. Adding a new capability later = save_skill(...) at runtime —
    /// no recompile. (Monitors first; research/coder/email migrate the same way.)
    pub(crate) async fn seed_capabilities(&self) {
        let existing = self.memory.list_skills().await.unwrap_or_default();
        if existing.iter().any(|s| s.lang == "capability") {
            return;
        }
        let caps = [
            ("github-monitor",
             "Monitor your GitHub repos and notifications and ping you when a new issue, pull request, or activity matches. Use when the user wants to track/watch/keep an eye on repos, issues, or PRs.",
             "github,repo,repos,issue,issues,pull request,pr,prs,notification,monitor,track,watch",
             r#"{"tool":"github","var":"github","args":{"limit":15},"label":"GitHub","needs_url":false}"#),
            ("web-monitor",
             "Watch any web page / URL and ping you when its content changes to match (a price, 'in stock', a phrase). Use when the user gives a URL to watch.",
             "web,page,url,http,price,stock,availability,watch,monitor,track",
             r#"{"tool":"fetch","var":"page","args":{},"label":"web page","needs_url":true}"#),
            ("inbox-monitor",
             "Watch your email inbox and ping you when a message from a sender or about a keyword arrives. Use for email/inbox monitoring.",
             "email,inbox,mail,sender,message,watch,monitor,track,notify",
             r#"{"tool":"inbox","var":"inbox","args":{"limit":10},"label":"inbox","needs_url":false}"#),
        ];
        for (name, summary, tags, code) in caps {
            let _ = self.memory.save_skill(Skill {
                name: name.into(),
                lang: "capability".into(),
                code: code.into(),
                summary: summary.into(),
                tags: tags.split(',').map(|s| s.trim().to_string()).collect(),
                status: "active".into(),
                runs: 0,
                successes: 0,
                created_ms: 0,
            }).await;
        }
    }

    /// The LLM routing decision (testable, no I/O): given the request + the capability catalog, pick ONE
    /// capability (or none) and extract its target/url. This is the dynamic replacement for the hardcoded
    /// `parse_*` verb lists — new capabilities appear here automatically because they're read from the store.
    pub(crate) async fn decide_capability(&self, user_text: &str, caps: &[Skill]) -> Option<(String, String, String)> {
        let catalog = caps.iter().map(|c| format!("- {}: {}", c.name, c.summary)).collect::<Vec<_>>().join("\n");
        let prompt = format!(
            "User request: \"{user_text}\"\n\nCapabilities I can set up:\n{catalog}\n\nIf the request is asking me to set ONE of these up, reply ONLY JSON: {{\"capability\":\"<exact name>\",\"target\":\"<short phrase to watch for>\",\"url\":\"<url if any else empty>\"}}. If none clearly fit, reply {{\"capability\":null}}."
        );
        let messages = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::system("You route a request to exactly one capability or none. Reply ONLY the JSON object."),
            ChatMessage::user(&prompt),
        ];
        let text = self.inference.chat(messages, GenerationConfig::default()).await.ok()?.text;
        let body = text.rsplit("</think>").next().unwrap_or(&text);
        let body = body.split("```").find(|s| s.contains('{')).unwrap_or(body);
        let obj = match (body.find('{'), body.rfind('}')) {
            (Some(s), Some(e)) if e > s => &body[s..=e],
            _ => return None,
        };
        let v: serde_json::Value = serde_json::from_str(obj).ok()?;
        let name = v.get("capability").and_then(|x| x.as_str()).unwrap_or("");
        if name.is_empty() || name == "null" {
            return None;
        }
        let target = v.get("target").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        let url = v.get("url").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
        Some((name.to_string(), target, url))
    }

    /// SKILL-BASED CAPABILITY ROUTER (dynamic — no recompile to add a capability). A cheap tag-derived
    /// pre-filter (built FROM the skills, so a new skill extends it for free) decides whether to spend an
    /// LLM call; if so, `decide_capability` picks one, and we instantiate its recipe from the skill's spec.
    pub(crate) async fn route_capability(&self, user_text: &str) -> Option<String> {
        let recipes = self.recipes.as_ref()?;
        self.seed_capabilities().await;
        let caps: Vec<Skill> = self
            .memory
            .list_skills()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.lang == "capability")
            .collect();
        if caps.is_empty() {
            return None;
        }
        // Pre-filter from the skills' OWN tags — skip the LLM on plain chat, but any new skill's tags
        // automatically widen what gets routed (zero code to add a capability).
        let low = user_text.to_lowercase();
        let hinted = caps.iter().flat_map(|c| c.tags.iter()).any(|t| t.len() > 2 && low.contains(t.as_str()));
        if !hinted {
            return None;
        }
        let (name, target, url) = self.decide_capability(user_text, &caps).await?;
        let cap = caps.iter().find(|c| c.name == name)?;
        if target.len() < 2 {
            return None;
        }
        let spec: serde_json::Value = serde_json::from_str(&cap.code).ok()?;
        let tool = spec.get("tool")?.as_str()?.to_string();
        let var = spec.get("var")?.as_str()?.to_string();
        let label = spec.get("label").and_then(|x| x.as_str()).unwrap_or(&cap.name).to_string();
        let needs_url = spec.get("needs_url").and_then(|x| x.as_bool()).unwrap_or(false);
        let mut args = spec.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));
        if needs_url {
            if url.is_empty() {
                return None;
            }
            args = serde_json::json!({ "url": url });
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let rec = Recipe {
            id: "watch".into(),
            name: format!("watch {label}: {target}"),
            steps: vec![
                RecipeStep::WaitForCondition {
                    tool_name: tool,
                    args,
                    store_as: var.clone(),
                    condition: Condition::VarContains { var, substring: target.clone() },
                    poll_secs: 120,
                    expire_ms: now + 24 * 3600 * 1000,
                },
                RecipeStep::Notify { message: format!("📡 Heads up — the {label} now matches \"{target}\".") },
            ],
        };
        let out = recipes.run_with(&rec, std::collections::HashMap::new()).await;
        Some(if out.sleeping_until.is_some() {
            format!("Watching the {label} for \"{target}\" — I'll ping you when it matches (every ~2 min, up to 24h).")
        } else if !out.notifications.is_empty() {
            out.notifications.join("\n")
        } else {
            format!("Couldn't start watching ({}).", out.error.unwrap_or_else(|| "tool unavailable".into()))
        })
    }

    /// PERSISTENT-DELEGATION TICK: wake any due WaitUntil/WaitForCondition runs and return whatever
    /// they surfaced (the caller delivers these to the home channel). Called on the heartbeat.
    pub async fn tick_delegations(&self) -> Vec<String> {
        let recipes = match &self.recipes {
            Some(r) => r,
            None => return Vec::new(),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        recipes
            .resume_due(now)
            .await
            .into_iter()
            .flat_map(|o| o.notifications)
            .collect()
    }

    /// Cadence gate for the poll loop: a people-knowing source exists, no interview question is
    /// already pending, and YM_WHOIS_SECS (default daily) has elapsed.
    pub async fn whois_due(&self) -> bool {
        if !mind_tools::PhotoSource::all_from_env().iter().any(|s| s.knows_people()) {
            return false;
        }
        if self.pending_slot().await.is_some() {
            return false; // never stack interview questions
        }
        let period_ms: i64 = std::env::var("YM_WHOIS_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(86_400) * 1000;
        let period_ms = (period_ms as f64 * self.domain_pace("whois").await) as i64;
        let last: i64 = self.memory.profile_get("whois_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

    /// `ym whois` sets a force flag (the CLI can't carry a photo) — the next poll tick fires it.
    pub async fn whois_forced(&self) -> bool {
        let f = self.memory.profile_get("whois_force").await.ok().flatten().unwrap_or_default();
        if f == "1" {
            let _ = self.memory.profile_set("whois_force", "").await;
            true
        } else {
            false
        }
    }

    /// Pick the next unknown face worth asking about: unnamed in the source, not already
    /// asked/skipped, ranked by photo count (a face in 400 photos matters; one in 3 doesn't).
    /// Returns (caption, face JPEG, slot) — the caller sends the photo, then arms the slot.
    pub async fn whois_next(&self) -> Option<(String, Vec<u8>, String)> {
        let sources = mind_tools::PhotoSource::all_from_env();
        let mut asked = self.whois_asked().await;
        for src in sources.iter().filter(|s| s.knows_people()) {
            let unnamed: Vec<mind_tools::PhotoPerson> = src
                .list_people()
                .await
                .into_iter()
                .filter(|p| p.name.is_empty() && !asked.contains(&format!("{}:{}", src.name(), p.id)))
                .collect();
            if unnamed.is_empty() {
                continue;
            }
            // Rank a small batch by photo count — checking all ~480 would hammer the server.
            let mut best: Option<(String, u64)> = None;
            let mut checked: Vec<String> = Vec::new();
            for p in unnamed.iter().take(15) {
                let n = src.person_photo_count(&p.id).await.unwrap_or(0);
                checked.push(p.id.clone());
                if n > best.as_ref().map_or(0, |b| b.1) {
                    best = Some((p.id.clone(), n));
                }
            }
            let Some((pid, count)) = best else { continue };
            if count < 8 {
                // Nothing question-worthy in this batch — retire it so tomorrow's sample digs
                // deeper into the pile instead of re-ranking the same minor faces.
                for id in checked {
                    asked.push(format!("{}:{id}", src.name()));
                }
                self.save_whois_asked(&asked).await;
                continue;
            }
            let Some(jpeg) = src.face_thumbnail(&pid).await else {
                asked.push(format!("{}:{pid}", src.name()));
                self.save_whois_asked(&asked).await;
                continue;
            };
            let caption = format!(
                "👀 I'm learning the people in your photo library — who is this? They're in ~{count} of your photos. (A name, plus how they're related to you if you like. \"skip\" is fine too.)"
            );
            let slot = format!("whois:{}:{pid}:{count}", src.name());
            return Some((caption, jpeg, slot));
        }
        None
    }

    /// After the photo question actually went out: arm the pending slot so the next reply is
    /// captured as the answer, retire the face from future asks, stamp the cadence.
    pub async fn whois_arm(&self, slot: &str) {
        let mut it = slot.split(':');
        let (_, src, pid) = (it.next(), it.next().unwrap_or(""), it.next().unwrap_or(""));
        if !pid.is_empty() {
            let mut asked = self.whois_asked().await;
            let key = format!("{src}:{pid}");
            if !asked.contains(&key) {
                asked.push(key);
            }
            self.save_whois_asked(&asked).await;
        }
        self.set_pending_slot(Some(slot)).await;
        let _ = self.memory.profile_set("whois_last", &chrono::Utc::now().timestamp_millis().to_string()).await;
        self.note_proactive_sent().await;
        self.ledger_sent("whois", "asked who an unnamed face is").await;
    }

    pub(crate) async fn whois_asked(&self) -> Vec<String> {
        self.memory
            .profile_get("whois_asked")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn save_whois_asked(&self, v: &[String]) {
        let _ = self.memory.profile_set("whois_asked", &serde_json::to_string(v).unwrap_or_default()).await;
    }

}
