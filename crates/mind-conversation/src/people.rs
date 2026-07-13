//! People + family -- contact registry, profiles, family dates and nudges. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) async fn load_people(&self) -> Vec<serde_json::Value> {
        self.memory.profile_get("people").await.ok().flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned()).unwrap_or_default()
    }

    pub(crate) async fn save_people(&self, p: &[serde_json::Value]) {
        let _ = self.memory.profile_set("people", &serde_json::Value::Array(p.to_vec()).to_string()).await;
    }

    /// The owner slug for a Telegram user id (registered member, or the primary if it's the primary's
    /// id), else None (an unknown guest — isolated to shared-only).
    pub(crate) async fn owner_for_tg(&self, tg_id: i64) -> Option<String> {
        if self.memory.profile_get("primary_tg").await.ok().flatten().and_then(|s| s.trim().parse::<i64>().ok()) == Some(tg_id) {
            return Some(mind_types::PRIMARY.to_string());
        }
        self.load_people().await.iter().find(|p| p.get("tg_id").and_then(|x| x.as_i64()) == Some(tg_id))
            .and_then(|p| p.get("slug").and_then(|x| x.as_str()).map(String::from))
    }

    /// Telegram user id → memory owner slug. Registered member → their slug; the FIRST private-DM
    /// user becomes the primary (the companion's owner, so an existing single user keeps their memory);
    /// any other unregistered user is an isolated guest (sees only shared facts).
    /// The primary's telegram id (0 = not yet known) — the boot-time proactive routing seed.
    pub async fn memory_handle_primary_tg(&self) -> anyhow::Result<Option<i64>> {
        Ok(self
            .memory
            .profile_get("primary_tg")
            .await
            .ok()
            .flatten()
            .and_then(|s| s.trim().parse::<i64>().ok()))
    }

    pub async fn resolve_owner(&self, tg_id: i64, shared_channel: bool) -> String {
        if let Some(o) = self.owner_for_tg(tg_id).await {
            return o;
        }
        if !shared_channel && self.memory.profile_get("primary_tg").await.ok().flatten().is_none() {
            let _ = self.memory.profile_set("primary_tg", &tg_id.to_string()).await;
            return mind_types::PRIMARY.to_string();
        }
        format!("guest:{tg_id}")
    }

    /// Register a family member from a shared Telegram CONTACT CARD (primary-only path in the
    /// transport). The card's user_id becomes their recognized identity with a private scope.
    pub async fn register_contact(&self, first_name: &str, last_name: &str, tg_id: i64) -> String {
        let name = format!("{first_name} {last_name}").trim().to_string();
        let slug: String = first_name.trim().to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
        if slug.is_empty() || slug == mind_types::PRIMARY || slug == "shared" {
            return "That contact name can't be used as a member id — add them with `person add <slug> <name> <tg-id>`.".to_string();
        }
        let mut people = self.load_people().await;
        people.retain(|p| p.get("slug").and_then(|x| x.as_str()) != Some(slug.as_str()));
        people.push(serde_json::json!({ "slug": slug, "name": name, "tg_id": tg_id, "relationship": "" }));
        self.save_people(&people).await;
        format!(
            "✅ Registered {name} from the contact card — they get their own private space with me (their chats stay theirs, yours stay yours). I'll recognize them the moment they message. Add how they're related anytime: `person add {slug} {name} wife`."
        )
    }

    pub(crate) async fn person_add(&self, arg: &str) -> String {
        let toks: Vec<&str> = arg.split_whitespace().collect();
        if toks.len() < 2 {
            return "Usage: ym person add <slug> <name> [telegram-id] [relationship]  (slug = a short id like 'wife')".to_string();
        }
        let slug = toks[0].to_lowercase();
        if slug == mind_types::PRIMARY || slug == "shared" {
            return "('primary' and 'shared' are reserved slugs)".to_string();
        }
        let (mut tg_id, mut rel, mut name_toks) = (None, String::new(), Vec::new());
        for t in &toks[1..] {
            if let Ok(n) = t.parse::<i64>() {
                tg_id = Some(n);
            } else if ["wife", "husband", "spouse", "partner", "son", "daughter", "child", "friend", "roommate"].contains(&t.to_lowercase().as_str()) {
                rel = t.to_lowercase();
            } else {
                name_toks.push(*t);
            }
        }
        let name = name_toks.join(" ");
        let mut people = self.load_people().await;
        people.retain(|p| p.get("slug").and_then(|x| x.as_str()) != Some(slug.as_str()));
        people.push(serde_json::json!({ "slug": slug, "name": name, "tg_id": tg_id, "relationship": rel }));
        self.save_people(&people).await;
        format!(
            "Added '{slug}'{}. They get their own private memory; shared (group) facts are visible to everyone, and your private DMs stay yours.{}",
            if name.is_empty() { String::new() } else { format!(" — {name}") },
            if tg_id.is_some() { String::new() } else { " Add their Telegram id so I recognize them (`ym person add <slug> <name> <tg-id>`), or have them message me once.".to_string() }
        )
    }

    pub(crate) async fn person_remove(&self, slug: &str) -> String {
        let slug = slug.trim().to_lowercase();
        let mut people = self.load_people().await;
        let before = people.len();
        people.retain(|p| p.get("slug").and_then(|x| x.as_str()) != Some(slug.as_str()));
        if people.len() == before {
            return format!("No member '{slug}'.");
        }
        self.save_people(&people).await;
        format!("Removed '{slug}'. (Their memory is kept but no longer reachable; tell me to forget it if you want.)")
    }

    pub(crate) async fn people_list(&self) -> String {
        let people = self.load_people().await;
        let mut lines = vec!["👥 Household (each has private memory + shared household memory):".to_string(), "  • primary (you) — owner".to_string()];
        for p in &people {
            let slug = p.get("slug").and_then(|x| x.as_str()).unwrap_or("?");
            let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let rel = p.get("relationship").and_then(|x| x.as_str()).unwrap_or("");
            let tg = if p.get("tg_id").and_then(|x| x.as_i64()).is_some() { "" } else { "  (no telegram id yet)" };
            lines.push(format!("  • {slug}{}{}{tg}", if name.is_empty() { String::new() } else { format!(" — {name}") }, if rel.is_empty() { String::new() } else { format!(" ({rel})") }));
        }
        lines.push("\nAdd one: `ym person add wife Priya <telegram-id> wife`. Speak as them: `ym as wife <message>`.".to_string());
        lines.join("\n")
    }

    pub(crate) async fn load_people_profiles(&self) -> Vec<serde_json::Value> {
        self.memory.profile_get("people_profiles").await.ok().flatten()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.as_array().cloned()).unwrap_or_default()
    }

    pub(crate) async fn save_people_profiles(&self, p: &[serde_json::Value]) {
        let _ = self.memory.profile_set("people_profiles", &serde_json::Value::Array(p.to_vec()).to_string()).await;
    }

    /// Deterministic profile editor — the human-authoritative path for key dates and relationship.
    /// LLM chat may PROPOSE profile changes; this verb is how they actually land (no freelancing).
    pub async fn family_set(&self, name: &str, field: &str, value: &str) -> String {
        let field = field.to_lowercase();
        // `clear` removes a date entry — for when the stored value is WRONG and the truth unknown
        // (never guess a family date to fill a slot).
        let clearing = value.trim().eq_ignore_ascii_case("clear") || value.trim().eq_ignore_ascii_case("none");
        // Accept MM-DD or "July 23"-style month-name dates for the date fields.
        let mmdd: Option<String> = if (field == "birthday" || field == "anniversary") && !clearing {
            let v = value.trim();
            let direct = v.len() == 5 && v.as_bytes()[2] == b'-';
            if direct {
                Some(v.to_string())
            } else {
                let today = local_now();
                parse_text_date_ms(v, &today)
                    .and_then(chrono::DateTime::from_timestamp_millis)
                    .map(|t| t.with_timezone(today.offset()).format("%m-%d").to_string())
            }
        } else {
            None
        };
        if (field == "birthday" || field == "anniversary") && !clearing && mmdd.is_none() {
            return format!("Couldn't parse \"{value}\" as a date — use MM-DD, \"July 23\", or `clear`.");
        }
        let mut store = self.load_people_profiles().await;
        let mut touched = false;
        // EXACT name match only — a prefix match would let "Brishti" hit "Brishti's Mom" first
        // (store order), silently editing a relative. Deterministic editors don't guess.
        let idx = store.iter().position(|p| {
            p.get("name")
                .and_then(|x| x.as_str())
                .map(|n| n.trim().eq_ignore_ascii_case(name.trim()))
                .unwrap_or(false)
        });
        let Some(idx) = idx else {
            return format!("No profile named exactly \"{name}\" — `ym family` lists them.");
        };
        for p in store.iter_mut().skip(idx).take(1) {
            match field.as_str() {
                "relationship" => {
                    p["relationship"] = serde_json::json!(value.trim());
                    touched = true;
                }
                "birthday" | "anniversary" => {
                    let label = if field == "birthday" { "birthday" } else { "wedding anniversary" };
                    let dates = p
                        .as_object_mut()
                        .and_then(|m| {
                            m.entry("dates").or_insert_with(|| serde_json::json!([]));
                            m.get_mut("dates")
                        })
                        .and_then(|d| d.as_array_mut());
                    if let Some(arr) = dates {
                        arr.retain(|d| {
                            d.get("label").and_then(|x| x.as_str()).map(|l| !l.eq_ignore_ascii_case(label)).unwrap_or(true)
                        });
                        if !clearing {
                            arr.push(serde_json::json!({"label": label, "mmdd": mmdd.clone().unwrap()}));
                        }
                        touched = true;
                    }
                }
                _ => return format!("Unknown field \"{field}\" — birthday | anniversary | relationship."),
            }
            break;
        }
        if !touched {
            return format!("No profile named \"{name}\".");
        }
        self.save_people_profiles(&store).await;
        if clearing {
            return format!("🧹 {name}: {field} cleared (was wrong; truth unknown — I'll ask rather than guess).");
        }
        // The correction is also a belief — recall stays consistent with the profile.
        let _ = self
            .memory
            .remember_as_belief(BeliefAssertion {
                statement: format!("{name}'s {field} is {}", mmdd.as_deref().unwrap_or(value)),
                polarity: 1.0,
                weight: 2.0,
                source_event: Some("family-set".into()),
                provenance: "told".into(),
            })
            .await;
        format!("✅ {name}: {field} set to {} (profile + belief).", mmdd.as_deref().unwrap_or(value))
    }

    /// Merge freshly-extracted people into the living profiles: upsert by name, dedupe facts, refresh the
    /// relationship, and upsert key dates by label. Returns how many people were touched (for the
    /// consolidation counter). Revise-in-place — one evolving profile per person, not an ever-growing pile.
    /// Remove a LIVING PROFILE by name (test residue, perspective artifacts). The household
    /// registry (`person remove <slug>`) is separate; this cleans the consolidation-built layer.
    pub async fn person_forget(&self, name: &str) -> String {
        let want = name.trim().to_lowercase();
        if want.len() < 2 {
            return "person forget <name>".to_string();
        }
        let mut store = self.load_people_profiles().await;
        let before = store.len();
        let mut dropped: Vec<String> = Vec::new();
        store.retain(|p| {
            let n = p.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if n.to_lowercase() == want {
                let facts = p.get("facts").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
                dropped.push(format!("{n} ({facts} facts)"));
                false
            } else {
                true
            }
        });
        if store.len() == before {
            return format!("No living profile named \"{name}\".");
        }
        self.save_people_profiles(&store).await;
        // Cascade: face-name map entries pointing at the forgotten name go too, so the whois
        // loop can re-ask about that cluster cleanly.
        let mut fm = self.face_names().await;
        let before_fm = fm.len();
        fm.retain(|_, v| !v.eq_ignore_ascii_case(&want));
        let fm_dropped = before_fm - fm.len();
        if fm_dropped > 0 {
            self.save_face_names(&fm).await;
        }
        format!("🧹 Forgot profile: {} — {} people remain{}.", dropped.join(", "), store.len(), if fm_dropped > 0 { format!(" (+{fm_dropped} face-map entry)") } else { String::new() })
    }

    pub(crate) async fn merge_people(&self, people: Vec<serde_json::Value>, user_said: &str) -> usize {
        if people.is_empty() {
            return 0;
        }
        let norm = |s: &str| -> String { s.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == ' ').collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ") };
        let mut store = self.load_people_profiles().await;
        let now = chrono::Utc::now().timestamp_millis();
        let mut touched = 0usize;
        for pv in people {
            let name = pv.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if name.len() < 2 {
                continue;
            }
            // BLOCKLIST: names the user declared non-existent stay dead — consolidation keeps
            // re-extracting them from old transcript text (Aarav rose three times).
            {
                let blocked: Vec<String> = self
                    .memory
                    .profile_get("people_blocklist")
                    .await
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                if blocked.iter().any(|b| b.eq_ignore_ascii_case(&name)) {
                    continue;
                }
            }
            // Perspective words are not people. Bare relationship nouns ("wife", "mother"), the
            // primary's own name, and "<primary>'s wife/husband" when the spouse is registered
            // all create phantom profiles — facts belong on the real person instead.
            {
                const GENERIC: [&str; 18] = [
                    "wife", "husband", "mother", "father", "mom", "dad", "son", "daughter",
                    "brother", "sister", "mother-in-law", "father-in-law", "cousin", "nephew",
                    "niece", "uncle", "aunt", "friend",
                ];
                let low = name.to_lowercase();
                let primary = self.memory.profile_get("name").await.ok().flatten().unwrap_or_default().to_lowercase();
                let spouse_registered = self
                    .load_people()
                    .await
                    .iter()
                    .any(|p| matches!(p.get("relationship").and_then(|x| x.as_str()), Some("wife") | Some("husband")));
                let possessive_spouse = (low.ends_with("'s wife") || low.ends_with("'s husband")) && spouse_registered;
                if GENERIC.contains(&low.as_str()) || (!primary.is_empty() && low == primary) || possessive_spouse {
                    continue;
                }
            }
            // Resolve to an existing person by NAME **or** any nickname (either side) — otherwise the same
            // person under a nickname (e.g. "Arya" vs "Aadrisha") would fork into a duplicate record.
            let mut cands: std::collections::HashSet<String> = std::collections::HashSet::new();
            cands.insert(norm(&name));
            for a in pv.get("aliases").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                if let Some(s) = a.as_str() {
                    let n = norm(s);
                    if !n.is_empty() {
                        cands.insert(n);
                    }
                }
            }
            let idx = store.iter().position(|p| {
                let nm = p.get("name").and_then(|x| x.as_str()).map(norm).unwrap_or_default();
                cands.contains(&nm)
                    || p.get("aliases").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).any(|al| cands.contains(&norm(al)))).unwrap_or(false)
            });
            let mut rec = match idx {
                Some(i) => store.remove(i),
                None => {
                    // PROVENANCE GATE: a NEW person can only be born from the user's OWN words.
                    // The extraction window mixes assistant text (mail digests, cleanup chatter,
                    // transcript ghosts) — that text may ENRICH existing people, never create.
                    if !user_said.contains(&name.to_lowercase()) {
                        continue;
                    }
                    serde_json::json!({ "name": name.clone(), "relationship": "", "facts": [], "dates": [] })
                }
            };
            // The canonical stored name wins; anything else this person is called becomes a nickname.
            let key = rec.get("name").and_then(|x| x.as_str()).map(norm).unwrap_or_else(|| norm(&name));
            if let Some(r) = pv.get("relationship").and_then(|x| x.as_str()) {
                if !r.trim().is_empty() {
                    rec["relationship"] = serde_json::json!(r.trim().to_lowercase());
                }
            }
            // aliases (nicknames) — dedupe, so `ym about <nickname>` resolves to the person. The incoming
            // name is itself folded in as a nickname when it differs from the canonical stored name.
            let mut aliases: Vec<serde_json::Value> = rec.get("aliases").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            let mut akeys: std::collections::HashSet<String> = aliases.iter().filter_map(|a| a.as_str()).map(norm).collect();
            let mut incoming_aliases: Vec<String> = pv.get("aliases").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(String::from).collect()).unwrap_or_default();
            incoming_aliases.push(name.clone());
            for s in incoming_aliases {
                let s = s.trim();
                if s.len() >= 2 && norm(s) != key && akeys.insert(norm(s)) {
                    aliases.push(serde_json::json!(s));
                }
            }
            rec["aliases"] = serde_json::json!(aliases);
            // facts — dedupe by normalized text, keep the most recent ~24
            let mut facts: Vec<serde_json::Value> = rec.get("facts").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            let mut fkeys: std::collections::HashSet<String> = facts.iter().filter_map(|f| f.as_str()).map(norm).collect();
            for f in pv.get("facts").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                if let Some(s) = f.as_str() {
                    let s = s.trim();
                    if s.len() >= 4 && fkeys.insert(norm(s)) {
                        facts.push(serde_json::json!(s));
                    }
                }
            }
            if facts.len() > 24 {
                facts = facts.split_off(facts.len() - 24);
            }
            rec["facts"] = serde_json::json!(facts);
            // dates — upsert by label (normalized to MM-DD)
            let mut dates: Vec<serde_json::Value> = rec.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            for d in pv.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                let label = d.get("label").and_then(|x| x.as_str()).unwrap_or("date").trim().to_lowercase();
                if let Some(mmdd) = d.get("date").and_then(|x| x.as_str()).and_then(parse_monthday) {
                    dates.retain(|e| e.get("label").and_then(|x| x.as_str()) != Some(label.as_str()));
                    dates.push(serde_json::json!({ "label": label, "mmdd": mmdd }));
                }
            }
            rec["dates"] = serde_json::json!(dates);
            rec["updated_ms"] = serde_json::json!(now);
            store.push(rec);
            touched += 1;
        }
        self.save_people_profiles(&store).await;
        touched
    }

    /// `ym family` — everyone I know about, with each one's next key date (rolled to its next occurrence).
    pub async fn family_view(&self) -> String {
        let store = self.load_people_profiles().await;
        if store.is_empty() {
            return "I don't know anyone in your life yet — mention your family/friends and I'll start keeping track (birthdays, what they're into, plans).".to_string();
        }
        let today = local_now();
        let mut lines = vec![format!("👪 People I keep track of ({}):", store.len())];
        for p in &store {
            let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("?");
            let rel = p.get("relationship").and_then(|x| x.as_str()).unwrap_or("");
            let nfacts = p.get("facts").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
            let next = next_date_line(p, &today);
            let rel_tag = if rel.is_empty() { String::new() } else { format!(" ({rel})") };
            lines.push(format!("• {name}{rel_tag} — {nfacts} thing(s) I know{}", next.map(|n| format!("; {n}")).unwrap_or_default()));
        }
        lines.push("\n`ym about <name>` for the full picture on someone.".to_string());
        lines.join("\n")
    }

    /// `ym about <name>` — the full living profile of one person: relationship, what I know, key dates.
    /// Matches on name OR nickname.
    pub async fn person_about(&self, name: &str) -> String {
        let store = self.load_people_profiles().await;
        let q = name.trim().to_lowercase();
        // Exact name first — "Brishti" must never resolve to "Brishti's Mom" by substring accident.
        let exact = store.iter().find(|p| {
            p.get("name").and_then(|x| x.as_str()).map(|n| n.trim().to_lowercase() == q).unwrap_or(false)
        });
        let p = match exact.or_else(|| store.iter().find(|p| person_matches(p, &q))) {
            Some(p) => p,
            None => return format!("I don't know anyone called \"{}\" yet.", name.trim()),
        };
        let pname = p.get("name").and_then(|x| x.as_str()).unwrap_or("?");
        let rel = p.get("relationship").and_then(|x| x.as_str()).unwrap_or("");
        let aliases: Vec<&str> = p.get("aliases").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
        let nick = if aliases.is_empty() { String::new() } else { format!(" (aka {})", aliases.join(", ")) };
        let mut out = vec![format!("👤 {pname}{nick}{}", if rel.is_empty() { String::new() } else { format!(" — your {rel}") })];
        let today = local_now();
        let dates: Vec<&serde_json::Value> = p.get("dates").and_then(|x| x.as_array()).map(|a| a.iter().collect()).unwrap_or_default();
        if !dates.is_empty() {
            out.push("\nKey dates:".to_string());
            for d in dates {
                let label = d.get("label").and_then(|x| x.as_str()).unwrap_or("date");
                let mmdd = d.get("mmdd").and_then(|x| x.as_str()).unwrap_or("");
                let days = days_until_mmdd(mmdd, &today);
                let when = days.map(|n| if n == 0 { " (today! 🎉)".to_string() } else { format!(" (in {n} day(s))") }).unwrap_or_default();
                out.push(format!("  • {label}: {mmdd}{when}"));
            }
        }
        let facts: Vec<&str> = p.get("facts").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|f| f.as_str()).collect()).unwrap_or_default();
        if facts.is_empty() {
            out.push("\n(I don't have specifics yet — tell me about them.)".to_string());
        } else {
            out.push("\nWhat I know:".to_string());
            for f in facts {
                out.push(format!("  • {f}"));
            }
        }
        out.join("\n")
    }

    /// Correct the family layer — remove a person by name or nickname. Corrections are essential to
    /// "always kept updated": a store you can only append to goes stale and wrong.
    pub async fn forget_person(&self, name: &str) -> String {
        let q = name.trim().to_lowercase();
        if q.len() < 2 {
            return "Forget whom? e.g. `ym forget Priya`".to_string();
        }
        let mut store = self.load_people_profiles().await;
        let before = store.len();
        // Word-boundary matching: a short name can't delete an unrelated person via a substring of
        // their name/alias (e.g. "Ana" removing "Susana" or "Anastasia").
        let removed: Vec<String> = store.iter().filter(|p| person_matches_mode(p, &q, MatchMode::WordBoundary)).filter_map(|p| p.get("name").and_then(|x| x.as_str()).map(String::from)).collect();
        store.retain(|p| !person_matches_mode(p, &q, MatchMode::WordBoundary));
        if store.len() == before {
            return format!("I don't have anyone matching \"{}\" in your family layer.", name.trim());
        }
        self.save_people_profiles(&store).await;
        format!("Forgotten: {}. (Removed from the people I track.)", removed.join(", "))
    }

    /// `ym rename <old> to <new>` — correct a person's canonical name. The new name becomes canonical
    /// and the old one is kept as a nickname. Crucially, a rename also recalls every belief that still
    /// names the OLD person and flags them, so the correction doesn't leave stale duplicates lurking in
    /// the belief store — the user confirms they're still right or purges them with `ym forget-belief`.
    pub async fn rename_person(&self, args: &str) -> String {
        let (old, new) = parse_rename(args);
        if old.len() < 2 || new.len() < 2 {
            return "Usage: `ym rename <old name> to <new name>` (e.g. `ym rename Priya to Priyanka`).".to_string();
        }
        let old_q = old.to_lowercase();
        let mut store = self.load_people_profiles().await;
        let renamed = rename_in_people(&mut store, &old_q, &new);
        if renamed.is_empty() {
            return format!("I don't have anyone matching \"{old}\" to rename. (`ym family` lists everyone I track.)");
        }
        self.save_people_profiles(&store).await;
        let stale = self.beliefs_referencing(&old_q).await;
        let mut out = format!("Renamed {} → {new}. (\"{old}\" is kept as a nickname so lookups still resolve.)", renamed.join(", "));
        if stale.is_empty() {
            out.push_str("\nNo beliefs still reference the old name — nothing to clean up.");
        } else {
            out.push_str(&format!("\n\n⚠️ {} belief(s) still reference \"{old}\" — confirm they're still right, or purge them:", stale.len()));
            for s in &stale {
                out.push_str(&format!("\n  • {s}"));
            }
            out.push_str(&format!("\n\nRun `ym forget-belief {old}` to purge them, or leave them if they still hold."));
        }
        out
    }

    /// Upcoming key dates across everyone, within `within_days`. Returns (name, label, days, mmdd) for
    /// the proactive tick to surface. Rolls each date to its next occurrence from today.
    pub async fn upcoming_people_dates(&self, within_days: i64) -> Vec<(String, String, i64, String)> {
        let store = self.load_people_profiles().await;
        let today = local_now();
        let mut out = Vec::new();
        for p in &store {
            let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            for d in p.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
                let label = d.get("label").and_then(|x| x.as_str()).unwrap_or("date").to_string();
                let mmdd = d.get("mmdd").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if let Some(days) = days_until_mmdd(&mmdd, &today) {
                    if days <= within_days {
                        out.push((name.clone(), label, days, mmdd));
                    }
                }
            }
        }
        out.sort_by_key(|(_, _, d, _)| *d);
        out
    }

    /// Proactive family tick — surface upcoming key dates once each (deduped by name|label|year), with a
    /// gentle nudge to plan. Returns lines for the poll loop to send. The "always keep family updated"
    /// promise, made visible: I bring up the birthday before you'd have to remember it.
    pub async fn family_date_nudges(&self, within_days: i64) -> Vec<String> {
        let upcoming = self.upcoming_people_dates(within_days).await;
        if upcoming.is_empty() {
            return Vec::new();
        }
        let year = local_now().format("%Y").to_string();
        let mut reminded: std::collections::HashSet<String> = self
            .memory
            .profile_get("people_reminded")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default();
        let mut out = Vec::new();
        for (name, label, days, mmdd) in upcoming {
            let key = format!("{name}|{label}|{year}");
            if !reminded.insert(key) {
                continue;
            }
            let when = if days == 0 { "today".to_string() } else { format!("in {days} day(s) ({mmdd})") };
            out.push(format!("🎂 {name}'s {label} is {when}. Want me to help you plan something?"));
        }
        if !out.is_empty() {
            let _ = self.memory.profile_set("people_reminded", &serde_json::to_string(&reminded.iter().collect::<Vec<_>>()).unwrap_or_else(|_| "[]".into())).await;
        }
        out
    }

    /// Remove one DATE entry from a person's profile (e.g. a wrong "open house") — the person and
    /// their other dates stay. Also clears that date's nudge-dedup key so a corrected date can
    /// nudge fresh when it lands.
    pub async fn forget_person_date(&self, name: &str, label: &str) -> String {
        let nq = name.trim().to_lowercase();
        let lq = label.trim().to_lowercase();
        if nq.len() < 2 || lq.len() < 2 {
            return "Whose date, and which one? e.g. `ym forget-date Aadrisha open house`".to_string();
        }
        let mut store = self.load_people_profiles().await;
        let mut removed: Option<(String, usize)> = None;
        for p in store.iter_mut() {
            if !person_matches(p, &nq) {
                continue;
            }
            let dates: Vec<serde_json::Value> = p.get("dates").and_then(|x| x.as_array()).cloned().unwrap_or_default();
            let before = dates.len();
            let kept: Vec<serde_json::Value> = dates
                .into_iter()
                .filter(|d| d.get("label").and_then(|x| x.as_str()).map(|l| !l.to_lowercase().contains(&lq)).unwrap_or(true))
                .collect();
            if kept.len() < before {
                removed = Some((p.get("name").and_then(|x| x.as_str()).unwrap_or("?").to_string(), before - kept.len()));
                p["dates"] = serde_json::json!(kept);
            }
        }
        let Some((who, n)) = removed else {
            return format!("I don't have a \"{}\" date on {}.", label.trim(), name.trim());
        };
        self.save_people_profiles(&store).await;
        if let Ok(Some(r)) = self.memory.profile_get("people_reminded").await {
            if let Ok(mut v) = serde_json::from_str::<Vec<String>>(&r) {
                v.retain(|k| !(k.starts_with(&format!("{who}|")) && k.to_lowercase().contains(&lq)));
                let _ = self
                    .memory
                    .profile_set("people_reminded", &serde_json::to_string(&v).unwrap_or_else(|_| "[]".into()))
                    .await;
            }
        }
        format!("🗑 Removed {n} \"{}\" date(s) from {who}'s profile.", label.trim())
    }

}
