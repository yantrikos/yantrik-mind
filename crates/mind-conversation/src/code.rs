//! Work/code study -- work radar, papers, forge, code study/ask, work proposals. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// Waking hours, every YM_RADAR_HOURS (default 6h), persisted stamp.
    pub async fn work_radar_due(&self) -> bool {
        use chrono::Timelike;
        let h = local_now().hour();
        if !(8..=22).contains(&h) {
            return false;
        }
        let period_ms = (std::env::var("YM_RADAR_HOURS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(6.0)
            * 3_600_000.0) as i64;
        let last: i64 = self.memory.profile_get("radar_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

    /// One radar pass. Returns Some(message) only when research revised beliefs; None = silence
    /// (nothing new, or nothing to research). Always stamps radar_last so failures don't hot-loop.
    pub async fn work_radar_run(&self) -> Option<String> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let _ = self.memory.profile_set("radar_last", &now_ms.to_string()).await;
        if !Self::treasury_try_draw("radar") {
            return None; // dry — logged by the treasury; the pass runs tomorrow
        }
        self.researcher.as_ref()?;
        // 1. The user's own recent words are the radar's only antenna.
        let recent = self.memory.recent_messages(160, &mind_types::AccessContext::Operator).await.ok()?;
        let user_lines: Vec<&str> = recent
            .iter()
            .filter(|(r, t)| r == "user" && t.len() > 12 && !t.starts_with('/'))
            .map(|(_, t)| t.as_str())
            .collect();
        if user_lines.len() < 3 {
            return None;
        }
        let sample = user_lines
            .iter()
            .rev()
            .take(40)
            .rev()
            .map(|t| format!("- {}", t.chars().take(240).collect::<String>()))
            .collect::<Vec<_>>()
            .join("\n");
        // 2. Extract ACTIVE WORK subjects (projects/tech/papers/deals) — not family, not chores.
        let prompt = format!(
            "Below are the user's own recent messages. Identify what they are actively WORKING ON \
             right now — concrete projects, technologies, research topics, papers, negotiations. \
             NOT family life, chores, reminders, photos, or questions about the assistant itself.\n\n{sample}\n\n\
             Output ONLY JSON: {{\"subjects\":[\"<2-5 word concrete subject>\", ...]}} — 1 to 4 subjects, \
             most active first. Empty array if none."
        );
        let cfg = GenerationConfig { max_tokens: 200, ..GenerationConfig::default() };
        let resp = self.inference.chat(vec![ChatMessage::user(&prompt)], cfg).await.ok()?;
        let txt = resp.text;
        let j: serde_json::Value = txt
            .find('{')
            .and_then(|a| txt.rfind('}').map(|b| txt[a..=b].to_string()))
            .and_then(|t| serde_json::from_str(&t).ok())?;
        let subjects: Vec<String> = j["subjects"]
            .as_array()
            .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| x.len() >= 3).collect())
            .unwrap_or_default();
        if subjects.is_empty() {
            return None;
        }
        // 3. Per-subject cooldown so the radar walks the work instead of drilling one hole.
        let cooldown_ms = (std::env::var("YM_RADAR_COOLDOWN_H")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(72.0)
            * 3_600_000.0) as i64;
        let mut seen: std::collections::HashMap<String, i64> = self
            .memory
            .profile_get("radar_seen")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let fresh = subjects.into_iter().find(|sub| {
            let n = sub.to_lowercase();
            !seen.iter().any(|(k, ts)| {
                (k.contains(&n) || n.contains(k.as_str())) && now_ms - ts < cooldown_ms
            })
        })?;
        // 4. Belief-revising research (the moat's signature move) — cited, priors reconciled.
        let report = self.research_revise(&fresh).await.ok()?;
        seen.insert(fresh.to_lowercase(), now_ms);
        if seen.len() > 40 {
            let mut rows: Vec<(String, i64)> = seen.into_iter().collect();
            rows.sort_by_key(|(_, ts)| std::cmp::Reverse(*ts));
            rows.truncate(40);
            seen = rows.into_iter().collect();
        }
        let _ = self.memory.profile_set("radar_seen", &serde_json::to_string(&seen).unwrap_or_default()).await;
        // 5. NOVELTY GATE: only interrupt when something actually changed. (The silent pass still
        // recorded its research into the belief store via research_revise — the learning happened.)
        if report.contains("nothing changed in what I believe") {
            return None;
        }
        self.ledger_sent("radar", &format!("autonomous research on {fresh} revised beliefs")).await;
        Some(format!("\u{1f6f0} Work radar — I looked into **{fresh}** on my own (it's what you've been working on):\n\n{report}"))
    }

    /// `code` / `code add <giturl>` / `code remove <name>` / `code sync` / `code <name>` (digest).
    /// RESEARCH READER — read a paper/article ONCE into memory, then answer/relate/adapt from what
    /// was learned. `paper study <url>` | `paper ask <key> <q>` | `paper adapt <key> [repo]` |
    /// `paper adopt <key> <n>` (queues the proposal into the self-build goal queue) | `paper list`.
    pub async fn paper_cmd(&self, arg: &str) -> String {
        let a = arg.trim();
        if let Some(url) = a.strip_prefix("study ").or_else(|| a.strip_prefix("read ")).map(str::trim).filter(|x| x.starts_with("http")) {
            return self.paper_study(url).await;
        }
        if let Some(rest) = a.strip_prefix("ask ").map(str::trim).filter(|x| !x.is_empty()) {
            let mut it = rest.splitn(2, char::is_whitespace);
            let key = it.next().unwrap_or("").trim().to_lowercase();
            let q = it.next().unwrap_or("").trim();
            if key.is_empty() || q.is_empty() {
                return "Usage: paper ask <key> <question>  (study first: `paper study <url>`)".into();
            }
            return self.paper_ask(&key, q).await;
        }
        if let Some(rest) = a.strip_prefix("adapt ").map(str::trim).filter(|x| !x.is_empty()) {
            let mut it = rest.splitn(2, char::is_whitespace);
            let key = it.next().unwrap_or("").trim().to_lowercase();
            let repo = it.next().unwrap_or("yantrik-mind").trim().to_string();
            return self.paper_adapt(&key, &repo).await;
        }
        if let Some(rest) = a.strip_prefix("adopt ").map(str::trim) {
            let mut it = rest.split_whitespace();
            let key = it.next().unwrap_or("").to_lowercase();
            let n: usize = it.next().and_then(|x| x.parse().ok()).unwrap_or(0);
            if key.is_empty() || n == 0 {
                return "Usage: paper adopt <key> <n>  (after `paper adapt <key>` listed proposals)".into();
            }
            return self.paper_adopt(&key, n).await;
        }
        if let Some(rest) = a.strip_prefix("topics").map(str::trim) {
            if let Some(vals) = rest.strip_prefix("set ").map(str::trim).filter(|x| !x.is_empty()) {
                let topics: Vec<String> = vals.split(';').map(|t| t.trim().to_string()).filter(|t| t.len() > 3).collect();
                let _ = self.memory.profile_set("research_topics", &serde_json::to_string(&topics).unwrap_or_default()).await;
                return format!("🔭 Research agenda set ({} topics). The night shift hunts these on arXiv.", topics.len());
            }
            let topics = self.research_topics().await;
            return format!("🔭 Research agenda:
{}

`paper topics set t1; t2; …` to change.",
                topics.iter().map(|t| format!("• {t}")).collect::<Vec<_>>().join("
"));
        }
        if a == "night" {
            return self.night_research_run().await;
        }
        if a.is_empty() || a == "list" {
            let idx = self.memory.profile_get("paper_index").await.ok().flatten().unwrap_or_default();
            let map: std::collections::BTreeMap<String, String> = serde_json::from_str(&idx).unwrap_or_default();
            if map.is_empty() {
                return "No papers studied yet. `paper study <url>` — arxiv links work best.".into();
            }
            let lines = map.iter().map(|(k, t)| format!("• `{k}` — {t}")).collect::<Vec<_>>().join("\n");
            return format!("📚 Studied papers:\n{lines}\n\n`paper ask <key> <q>` · `paper adapt <key>`");
        }
        "paper study <url> | paper ask <key> <q> | paper adapt <key> [repo] | paper adopt <key> <n> | paper list".into()
    }

    /// READ + LEARN + RELATE: fetch/extract deterministically, ONE distill pass into typed facts,
    /// ONE relate pass connecting the paper to everything already in memory (studied code, prior
    /// papers). Detached; ~2 LLM calls total.
    pub async fn paper_study(&self, url: &str) -> String {
        let url2 = url.to_string();
        let fetched = tokio::task::spawn_blocking(move || {
            mind_tools::paper::fetch_paper(&url2).map(|(t, x)| (mind_tools::paper::paper_key(&url2), t, x))
        })
        .await;
        let (key, title, text) = match fetched {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return format!("📄 Couldn't read that: {e}"),
            Err(_) => return "📄 Reader panicked.".into(),
        };
        let sections = mind_tools::paper::section_skeleton(&text);
        let head: String = text.chars().take(24000).collect();
        let tail: String = if text.len() > 30000 {
            text.chars().skip(text.chars().count().saturating_sub(4000)).collect()
        } else {
            String::new()
        };
        let nq = self.notify_queue.clone();
        let inf = self.inference.clone();
        let mem = self.memory.clone();
        let persona = self.persona.clone();
        let key2 = key.clone();
        let title2 = title.clone();
        tokio::spawn(async move {
            let token = format!("paperkb{key2}");
            let sec_line = if sections.is_empty() { String::new() } else { format!("Sections: {}\n", sections.join(" | ")) };
            let prompt = format!(
                "You just read this paper/article. Distill 12-18 CONCRETE facts worth keeping: the core \
                 claim(s); the method/mechanism (how it actually works); key results WITH numbers; \
                 limitations the authors admit; and context (what it builds on). Prefix each fact with its \
                 kind: claim:/method:/result:/limitation:/context:. One specific sentence each — never vague. \
                 Output ONLY JSON: {{\"facts\":[\"...\"]}}.\n\n{sec_line}\nTEXT:\n{head}\n{tail}"
            );
            let cfg = GenerationConfig { max_tokens: 1100, ..GenerationConfig::default() };
            let resp = match inf.chat(vec![ChatMessage::system(&persona), ChatMessage::user(&prompt)], cfg).await {
                Ok(r) => r.text,
                Err(e) => {
                    nq.lock().unwrap().push(format!("📄 Read {key2} but distillation failed: {e}"));
                    return;
                }
            };
            let facts: Vec<String> = resp
                .find('{')
                .and_then(|a| resp.rfind('}').map(|b| resp[a..=b].to_string()))
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|j| j.get("facts").and_then(|x| x.as_array()).cloned())
                .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| x.len() > 12).collect())
                .unwrap_or_default();
            if facts.is_empty() {
                nq.lock().unwrap().push(format!("📄 Read {key2} but couldn't distill clean facts."));
                return;
            }
            let mut saved = 0usize;
            for fact in &facts {
                let statement = format!("{token} [paper:{key2}] {fact}");
                if mem.remember_as_belief(BeliefAssertion {
                    statement, polarity: 1.0, weight: 2.2,
                    source_event: Some("paper-study".into()), provenance: "studied".into(),
                }).await.is_ok() { saved += 1; }
            }
            // RELATE: connect the paper to what memory already holds — studied code repos + prior
            // papers. This is where reading compounds instead of piling up.
            let mut known: Vec<String> = Vec::new();
            let code_repos = mem.profile_get("code_repos").await.ok().flatten().unwrap_or_default();
            let repo_names: Vec<String> = serde_json::from_str::<Vec<String>>(&code_repos)
                .unwrap_or_default()
                .iter()
                .map(|u| mind_tools::code::repo_name(u))
                .collect();
            for rn in repo_names.iter().take(4) {
                let alnum: String = rn.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
                let t = format!("codekb{alnum}");
                for b in mem.beliefs_matching_n(&t, 30, &mind_types::AccessContext::Operator).await.unwrap_or_default() {
                    known.push(b.statement.replacen(&t, "", 1));
                }
            }
            let idx = mem.profile_get("paper_index").await.ok().flatten().unwrap_or_default();
            let pmap: std::collections::BTreeMap<String, String> = serde_json::from_str(&idx).unwrap_or_default();
            for (pk, _) in pmap.iter().filter(|(pk, _)| pk.as_str() != key2).take(3) {
                let t = format!("paperkb{pk}");
                for b in mem.beliefs_matching_n(&t, 10, &mind_types::AccessContext::Operator).await.unwrap_or_default() {
                    known.push(b.statement.replacen(&t, "", 1));
                }
            }
            let mut related_n = 0usize;
            if !known.is_empty() {
                let known_block: String = known.iter().map(|k| format!("- {k}")).collect::<Vec<_>>().join("\n").chars().take(9000).collect();
                let paper_block = facts.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
                let rprompt = format!(
                    "PAPER I JUST READ ({title2}):\n{paper_block}\n\nWHAT I ALREADY KNOW (my studied codebases and prior papers):\n{known_block}\n\n\
                     State 2-5 SPECIFIC connections: where a paper idea maps onto a module/mechanism I already \
                     know, confirms it, contradicts it, or suggests a concrete upgrade to it. Each ONE sentence \
                     naming both sides. Skip generic similarities. Output ONLY JSON: {{\"relations\":[\"...\"]}}."
                );
                let cfg2 = GenerationConfig { max_tokens: 500, ..GenerationConfig::default() };
                if let Ok(r) = inf.chat(vec![ChatMessage::system(&persona), ChatMessage::user(&rprompt)], cfg2).await {
                    let rels: Vec<String> = r.text
                        .find('{')
                        .and_then(|a| r.text.rfind('}').map(|b| r.text[a..=b].to_string()))
                        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                        .and_then(|j| j.get("relations").and_then(|x| x.as_array()).cloned())
                        .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| x.len() > 12).collect())
                        .unwrap_or_default();
                    for rel in rels {
                        let statement = format!("{token} [paper:{key2}] [relate] {rel}");
                        if mem.remember_as_belief(BeliefAssertion {
                            statement, polarity: 1.0, weight: 2.4,
                            source_event: Some("paper-relate".into()), provenance: "studied".into(),
                        }).await.is_ok() { related_n += 1; }
                    }
                }
            }
            let mut pmap2 = pmap;
            pmap2.insert(key2.clone(), title2.chars().take(90).collect());
            let _ = mem.profile_set("paper_index", &serde_json::to_string(&pmap2).unwrap_or_default()).await;
            nq.lock().unwrap().push(format!(
                "📄 Studied **{key2}** — learned {saved} facts + {related_n} connections to what I already know. \
                 `paper ask {key2} <q>` answers from memory; `paper adapt {key2}` proposes improvements to our code from it."
            ));
        });
        format!("📄 Reading \"{}\" now — I'll distill it into memory, relate it to what I already know, and confirm (key: `{key}`).", title.chars().take(90).collect::<String>())
    }

    /// Answer from the paper's learned facts; on gaps, targeted re-read of the CACHED text (then the
    /// distilled answer is learned — same compounding loop as code_ask).
    pub async fn paper_ask(&self, key: &str, question: &str) -> String {
        let token = format!("paperkb{key}");
        let tag = format!("[paper:{key}]");
        let facts: Vec<String> = self.memory.beliefs_matching_n(&token, 300, &mind_types::AccessContext::Operator).await.unwrap_or_default()
            .into_iter().map(|b| b.statement).filter(|st| st.contains(&token)).collect();
        if facts.is_empty() {
            return format!("I haven't studied `{key}` yet — `paper study <url>` first.");
        }
        let facts_lower = facts.join(" ").to_lowercase();
        let gaps: Vec<String> = question
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
            .filter(|w| w.len() >= 5 && !facts_lower.contains(&w.to_lowercase()))
            .map(|w| w.to_string())
            .collect::<std::collections::HashSet<_>>().into_iter().take(4).collect();
        let key2 = key.to_string();
        let excerpts: Vec<String> = if gaps.is_empty() {
            vec![]
        } else {
            tokio::task::spawn_blocking(move || mind_tools::paper::paper_lookup(&key2, &gaps, 4))
                .await.unwrap_or_default()
        };
        let strip = |f: &str| f.replacen(&token, "", 1).replacen(&tag, "", 1).trim().to_string();
        let block = facts.iter().map(|f| format!("- {}", strip(f))).collect::<Vec<_>>().join("\n");
        let (extra, want_learn) = if excerpts.is_empty() {
            (String::new(), false)
        } else {
            let ex: String = excerpts.join("\n…\n").chars().take(4000).collect();
            (format!(
                "\n\nTARGETED PASSAGES (just re-read from the cached paper because my facts didn't cover them):\n{ex}\n\n\
                 Since you have fresh passages, ALSO distill 1-2 new facts worth remembering. \
                 Output ONLY JSON: {{\"answer\":\"...\",\"learned\":[\"...\"]}}."
            ), true)
        };
        let prompt = format!(
            "Answer the question about the paper `{key}` using the facts I learned{}. Be specific; keep the \
             kind prefixes' meaning in mind (claim/method/result/limitation). If nothing covers it, say what \
             section I'd need to re-read — never invent.\n\nWHAT I LEARNED:\n{block}\n\nQUESTION: {question}{extra}",
            if want_learn { " plus the passages below" } else { "" }
        );
        let cfg = GenerationConfig { max_tokens: 550, ..GenerationConfig::default() };
        let resp = match self.inference.chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg).await {
            Ok(r) => r.text,
            Err(e) => return format!("(couldn't compose an answer: {e})"),
        };
        if !want_learn {
            return format!("{}\n\n_(answered from {} learned facts — no re-read)_", resp.trim(), facts.len());
        }
        let parsed = resp.find('{')
            .and_then(|a| resp.rfind('}').map(|b| resp[a..=b].to_string()))
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        let (answer, learned): (String, Vec<String>) = match &parsed {
            Some(j) => (
                j.get("answer").and_then(|x| x.as_str()).unwrap_or(resp.trim()).to_string(),
                j.get("learned").and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| x.len() > 12).collect())
                    .unwrap_or_default(),
            ),
            None => (resp.trim().to_string(), vec![]),
        };
        let mut learned_n = 0usize;
        for fact in &learned {
            let statement = format!("{token} {tag} [learned] {fact}");
            if self.memory.remember_as_belief(BeliefAssertion {
                statement, polarity: 1.0, weight: 2.2,
                source_event: Some("paper-ask-learn".into()), provenance: "studied".into(),
            }).await.is_ok() { learned_n += 1; }
        }
        format!("{}\n\n_(answered from {} learned facts + a targeted re-read; learned {} more)_",
            answer.trim(), facts.len(), learned_n)
    }

    /// ADAPT: paper facts × studied-repo facts → 2-3 concrete improvement proposals in self-build
    /// goal format. `paper adopt <key> <n>` then queues one — reading becomes shipped code.
    pub async fn paper_adapt(&self, key: &str, repo: &str) -> String {
        let ptoken = format!("paperkb{key}");
        let pfacts: Vec<String> = self.memory.beliefs_matching_n(&ptoken, 100, &mind_types::AccessContext::Operator).await.unwrap_or_default()
            .into_iter().map(|b| b.statement.replacen(&ptoken, "", 1)).filter(|st| st.contains("[paper:")).collect();
        if pfacts.is_empty() {
            return format!("I haven't studied `{key}` — `paper study <url>` first.");
        }
        let alnum: String = repo.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        let ctoken = format!("codekb{alnum}");
        let cfacts: Vec<String> = self.memory.beliefs_matching_n(&ctoken, 200, &mind_types::AccessContext::Operator).await.unwrap_or_default()
            .into_iter().map(|b| b.statement.replacen(&ctoken, "", 1)).collect();
        if cfacts.is_empty() {
            return format!("I haven't studied the `{repo}` codebase — `code study <git url>` first, then adapt.");
        }
        let pblock = pfacts.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n");
        let cblock: String = cfacts.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n").chars().take(10000).collect();
        let prompt = format!(
            "PAPER `{key}`:\n{pblock}\n\nMY `{repo}` CODEBASE (studied facts):\n{cblock}\n\n\
             Propose 2-3 CONCRETE, minimal improvements to {repo} that adapt an idea from the paper. Each must \
             be: implementable as ONE focused PR, testable, reversible, and grounded in a REAL module/type from \
             the codebase facts (name it). Write each as one imperative sentence suitable for an autonomous \
             build goal. Output ONLY JSON: {{\"proposals\":[\"...\"]}}."
        );
        let cfg = GenerationConfig { max_tokens: 600, ..GenerationConfig::default() };
        let resp = match self.inference.chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg).await {
            Ok(r) => r.text,
            Err(e) => return format!("(adapt failed: {e})"),
        };
        let props: Vec<String> = resp.find('{')
            .and_then(|a| resp.rfind('}').map(|b| resp[a..=b].to_string()))
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|j| j.get("proposals").and_then(|x| x.as_array()).cloned())
            .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| x.len() > 20).collect())
            .unwrap_or_default();
        if props.is_empty() {
            return "Couldn't derive grounded proposals from that pairing.".into();
        }
        let _ = self.memory.profile_set(&format!("paper_adapt_{key}"), &serde_json::to_string(&props).unwrap_or_default()).await;
        let list = props.iter().enumerate().map(|(i, p)| format!("{}. {p}", i + 1)).collect::<Vec<_>>().join("\n");
        format!("💡 From `{key}` → `{repo}`:\n{list}\n\nSay `paper adopt {key} <n>` and I'll queue it for my next self-build.")
    }

    /// ADOPT: append the chosen proposal to the self-build goal queue — the tick implements it.
    pub async fn paper_adopt(&self, key: &str, n: usize) -> String {
        let raw = self.memory.profile_get(&format!("paper_adapt_{key}")).await.ok().flatten().unwrap_or_default();
        let props: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
        let Some(goal) = props.get(n.saturating_sub(1)) else {
            return format!("No proposal #{n} for `{key}` — run `paper adapt {key}` first.");
        };
        let path = std::env::var("YM_SELFBUILD_GOALS").unwrap_or_else(|_| "/var/lib/yantrik-mind/selfbuild-goals.txt".into());
        use std::io::Write as _;
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut fh) => {
                let _ = writeln!(fh, "{goal}");
                format!("✅ Queued for self-build: {goal}\nThe next build tick will pick it up, implement it behind the usual gates (compile+tests+small-diff), and deploy.")
            }
            Err(e) => format!("Couldn't write the goal queue ({e})."),
        }
    }

    pub(crate) fn forge_dir(id: &str) -> std::path::PathBuf {
        let d = std::path::PathBuf::from(
            std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".into()),
        )
        .join("forge")
        .join(id);
        let _ = std::fs::create_dir_all(&d);
        d
    }

    pub(crate) async fn forge_load(&self) -> serde_json::Value {
        self.memory.profile_get("forge_ventures").await.ok().flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    }

    pub(crate) async fn forge_save(&self, v: &serde_json::Value) {
        let _ = self.memory.profile_set("forge_ventures", &v.to_string()).await;
    }

    pub(crate) fn forge_json_grab(resp: &str) -> Option<serde_json::Value> {
        resp.find('{')
            .and_then(|a| resp.rfind('}').map(|b| resp[a..=b].to_string()))
            .and_then(|t| serde_json::from_str(&t).ok())
    }

    /// `forge start <idea>` | `forge status` | `forge tick` | `forge kill` | `forge show <id>`
    pub async fn forge_cmd(&self, arg: &str) -> String {
        let a = arg.trim();
        if let Some(idea) = a.strip_prefix("start ").map(str::trim).filter(|x| x.len() > 8) {
            let mut all = self.forge_load().await;
            if all.as_object().map(|m| m.values().any(|v| v.get("stage").and_then(|x| x.as_str()).map(|st| st != "shipped" && st != "killed").unwrap_or(false))).unwrap_or(false) {
                return "⚒️ A venture is already in flight — `forge status`, and `forge kill` first if you want to replace it (v1 runs one at a time).".into();
            }
            let id = format!("v{}", chrono::Utc::now().timestamp() % 1_000_000);
            all[&id] = serde_json::json!({
                "id": id, "idea": idea, "stage": "brainstorm", "iter": 0, "max_iter": 2,
                "log": [], "updated_ms": 0_i64,
            });
            self.forge_save(&all).await;
            return format!(
                "⚒️ Venture `{id}` forged from: \"{idea}\"\nStages: brainstorm → research → spec (with kill criteria) → build → test → rate → iterate → ship/kill.\nIt advances on its own (one treasury-metered stage at a time); `forge status` any time, or `forge tick` to push it now."
            );
        }
        if a == "tick" {
            return self.forge_tick(true).await.unwrap_or_else(|| "⚒️ Nothing to tick — no active venture (`forge start <idea>`).".into());
        }
        if a == "kill" {
            let mut all = self.forge_load().await;
            let Some(m) = all.as_object_mut() else { return "⚒️ No ventures.".into() };
            for (_, v) in m.iter_mut() {
                let st = v.get("stage").and_then(|x| x.as_str()).unwrap_or("");
                if st != "shipped" && st != "killed" {
                    v["stage"] = serde_json::json!("killed");
                    v["log"].as_array_mut().map(|l| l.push(serde_json::json!("killed by owner")));
                    let out = format!("⚒️ Venture `{}` killed.", v.get("id").and_then(|x| x.as_str()).unwrap_or("?"));
                    self.forge_save(&all).await;
                    return out;
                }
            }
            return "⚒️ No active venture to kill.".into();
        }
        // status (default)
        let all = self.forge_load().await;
        let Some(m) = all.as_object().filter(|m| !m.is_empty()) else {
            return "⚒️ The forge is cold. `forge start <one-line product idea>` lights it.".into();
        };
        let mut out = String::from("⚒️ Forge:");
        for (id, v) in m {
            let stage = v.get("stage").and_then(|x| x.as_str()).unwrap_or("?");
            let idea = v.get("idea").and_then(|x| x.as_str()).unwrap_or("?");
            let iter = v.get("iter").and_then(|x| x.as_i64()).unwrap_or(0);
            let score = v.get("rating").and_then(|r| r.get("score")).and_then(|x| x.as_i64());
            out.push_str(&format!("\n• `{id}` [{stage}{}] {}{}",
                if iter > 0 { format!(" iter{iter}") } else { String::new() },
                idea.chars().take(90).collect::<String>(),
                score.map(|sc| format!(" — rated {sc}/10")).unwrap_or_default()));
            if let Some(log) = v.get("log").and_then(|x| x.as_array()) {
                if let Some(last) = log.last().and_then(|x| x.as_str()) {
                    out.push_str(&format!("\n    last: {}", last.chars().take(160).collect::<String>()));
                }
            }
        }
        out
    }

    /// True when a venture wants its next stage (active + cooled ≥15min since last stage —
    /// 90s pacing burned the whole daily envelope in half an hour, live incident).
    pub async fn forge_due(&self) -> bool {
        let all = self.forge_load().await;
        let now = chrono::Utc::now().timestamp_millis();
        all.as_object().map(|m| m.values().any(|v| {
            let st = v.get("stage").and_then(|x| x.as_str()).unwrap_or("");
            let up = v.get("updated_ms").and_then(|x| x.as_i64()).unwrap_or(0);
            st != "shipped" && st != "killed" && now - up > 900_000
        })).unwrap_or(false)
    }

    /// Advance the active venture by exactly ONE stage. Returns the stage report (None = idle).
    /// `manual` bypasses the treasury (owner pushed it); autonomous ticks draw from the envelope.
    pub async fn forge_tick(&self, manual: bool) -> Option<String> {
        let mut all = self.forge_load().await;
        let id = all.as_object()?.iter().find(|(_, v)| {
            let st = v.get("stage").and_then(|x| x.as_str()).unwrap_or("");
            st != "shipped" && st != "killed"
        }).map(|(k, _)| k.clone())?;
        if !manual {
            // Dry-day latch: say "envelope dry" ONCE, then stay silent until tomorrow — without
            // this the poll loop re-fired the dry message every tick (live chat-flood incident).
            let today = local_now().format("%Y-%m-%d").to_string();
            let dry_day = self.memory.profile_get("forge_dry_day").await.ok().flatten().unwrap_or_default();
            if dry_day == today {
                return None;
            }
            if !Self::treasury_try_draw("forge") {
                let _ = self.memory.profile_set("forge_dry_day", &today).await;
                return Some("⚒️ forge envelope dry today — venture resumes tomorrow.".into());
            }
        }
        let v = all[&id].clone();
        let stage = v.get("stage").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let idea = v.get("idea").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let report = match stage.as_str() {
            "brainstorm" => {
                // 3 independent persona takes + a synthesis — a real panel, not one voice.
                let mut takes: Vec<String> = Vec::new();
                for persona in ["a pragmatic staff engineer (feasibility, MVP scope)",
                                "a skeptical investor (who pays, what kills this)",
                                "a product visionary (the wedge that makes it 10x, not 10%)"] {
                    let p = format!("As {persona}, give your sharpest 4-sentence take on this product idea — concrete, no fluff:\n{idea}");
                    let cfg = GenerationConfig { max_tokens: 260, ..GenerationConfig::default() };
                    if let Ok(r) = self.inference.chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&p)], cfg).await {
                        takes.push(r.text.trim().to_string());
                    }
                }
                let panel = takes.iter().enumerate().map(|(i, t)| format!("PANELIST {}:\n{t}", i + 1)).collect::<Vec<_>>().join("\n\n");
                let p = format!("Panel takes on \"{idea}\":\n\n{panel}\n\nSynthesize ONE chosen direction: the sharpest version of this product. Output ONLY JSON: {{\"direction\":\"2-3 sentences\",\"differentiator\":\"1 sentence\",\"biggest_risk\":\"1 sentence\"}}.");
                let cfg = GenerationConfig { max_tokens: 350, ..GenerationConfig::default() };
                match self.inference.chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&p)], cfg).await {
                    Ok(r) => match Self::forge_json_grab(&r.text) {
                        Some(j) => {
                            all[&id]["brainstorm"] = j.clone();
                            all[&id]["stage"] = serde_json::json!("research");
                            format!("⚒️ `{id}` brainstormed ({} panelists): {}", takes.len(), j.get("direction").and_then(|x| x.as_str()).unwrap_or("?"))
                        }
                        None => format!("⚒️ `{id}` brainstorm synthesis didn't parse — will retry next tick."),
                    },
                    Err(e) => format!("⚒️ `{id}` brainstorm failed ({e}) — will retry next tick."),
                }
            }
            "research" => {
                // Deterministic: arXiv related work + everything memory already holds. No LLM.
                let q: String = idea.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
                let q2 = q.clone();
                let papers = tokio::task::spawn_blocking(move || mind_tools::paper::arxiv_search(&q2, 4))
                    .await.ok().and_then(|r| r.ok()).unwrap_or_default();
                let paper_lines = papers.iter()
                    .map(|(_, t, sm)| format!("- {t}: {}", sm.chars().take(200).collect::<String>()))
                    .collect::<Vec<_>>().join("\n");
                let mem_facts = self.memory.beliefs_matching(&q, &mind_types::AccessContext::Operator).await.unwrap_or_default();
                let mem_lines = mem_facts.iter().take(8).map(|b| format!("- {}", b.statement)).collect::<Vec<_>>().join("\n");
                all[&id]["research"] = serde_json::json!({"papers": paper_lines, "memory": mem_lines});
                all[&id]["stage"] = serde_json::json!("spec");
                format!("⚒️ `{id}` researched: {} related papers + {} memory facts attached.", papers.len(), mem_facts.len().min(8))
            }
            "spec" => {
                let bs = v.get("brainstorm").cloned().unwrap_or_default();
                let rs = v.get("research").cloned().unwrap_or_default();
                let p = format!(
                    "Write the PRD for this venture as JSON.\nIDEA: {idea}\nDIRECTION: {bs}\nRESEARCH: {rs}\n\n\
                     FEASIBILITY WALL (non-negotiable): the MVP must be a SMALL self-contained artifact — a \
                     single-page web app (localStorage only, NO WebRTC, NO servers, NO external APIs) or a \
                     stdlib-only Python tool — deliverable in under ~500 lines total. Scope the feature list \
                     DOWN to that wall; ambition beyond it belongs in a 'later' note, not the MVP or the kill \
                     criteria. Kill criteria must be testable against the small artifact itself. Output ONLY JSON: \
                     {{\"name\":\"...\",\"one_liner\":\"...\",\"mvp_features\":[\"3-5 items\"],\
                     \"kill_criteria\":[\"2-3 PRE-REGISTERED kill conditions, each verifiable by a referee READING THE ARTIFACT TODAY (missing or broken feature, structural defect, spec violation) — NEVER future usage, adoption, or time-based conditions\"],\
                     \"stack\":\"html|python\",\"acceptance\":[\"3-4 concrete checks a referee can verify\"]}}"
                );
                let cfg = GenerationConfig { max_tokens: 600, ..GenerationConfig::default() };
                match self.inference.chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&p)], cfg).await {
                    Ok(r) => match Self::forge_json_grab(&r.text) {
                        Some(j) => {
                            let name = j.get("name").and_then(|x| x.as_str()).unwrap_or("?").to_string();
                            all[&id]["spec"] = j;
                            all[&id]["stage"] = serde_json::json!("build");
                            format!("⚒️ `{id}` spec locked: **{name}** — kill criteria pre-registered.")
                        }
                        None => format!("⚒️ `{id}` spec didn't parse — will retry next tick."),
                    },
                    Err(e) => format!("⚒️ `{id}` spec failed ({e}) — will retry."),
                }
            }
            "build" => {
                let spec = v.get("spec").cloned().unwrap_or_default();
                let issues = v.get("rating").and_then(|r| r.get("issues")).cloned().unwrap_or(serde_json::json!([]));
                // STRONG-BUILDER PATH (default): two ventures proved the chain models can't emit a
                // working artifact — the referee killed both. Claude Code builds directly into the
                // venture dir via deploy/forge_build.sh; the chain remains only as a fallback when
                // the script is missing (dev boxes) or the builder fails.
                let builder_sh = std::env::var("YM_FORGE_BUILD_SH")
                    .unwrap_or_else(|_| "/root/codes/yantrik-mind/deploy/forge_build.sh".into());
                if std::path::Path::new(&builder_sh).exists() {
                    let dir = Self::forge_dir(&id);
                    let _ = std::fs::write(dir.join("spec.json"), spec.to_string());
                    if issues.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                        let _ = std::fs::write(dir.join("issues.json"), issues.to_string());
                    }
                    let dir2 = dir.clone();
                    let sh2 = builder_sh.clone();
                    let out = tokio::task::spawn_blocking(move || {
                        std::process::Command::new("bash").arg(&sh2).arg(&dir2).output()
                    }).await;
                    let ok = matches!(&out, Ok(Ok(o)) if String::from_utf8_lossy(&o.stdout).contains("FORGE_BUILD_DONE"));
                    if ok {
                        let mut written: Vec<String> = Vec::new();
                        if let Ok(rd) = std::fs::read_dir(&dir) {
                            for e in rd.filter_map(|e| e.ok()).filter(|e| e.path().is_file()) {
                                let n = e.file_name().to_string_lossy().to_string();
                                if !n.ends_with(".json") && e.metadata().map(|m| m.len() > 60).unwrap_or(false) {
                                    written.push(n);
                                }
                            }
                        }
                        if !written.is_empty() {
                            all[&id]["files"] = serde_json::json!(written);
                            all[&id]["stage"] = serde_json::json!("test");
                            all[&id]["updated_ms"] = serde_json::json!(chrono::Utc::now().timestamp_millis());
                            if let Some(l) = all[&id]["log"].as_array_mut() {
                                l.push(serde_json::json!(format!("built (strong builder): {} files", written.len())));
                            }
                            self.forge_save(&all).await;
                            return Some(format!("⚒️ `{id}` built by the STRONG builder: {} file(s) → {}", written.len(), dir.display()));
                        }
                    }
                    // fall through to the chain path below — honest note in the report
                }
                let fix = if issues.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                    format!("\nFIX THESE ISSUES from the last review: {issues}")
                } else { String::new() };
                // Fenced multi-file format, NOT JSON — a whole HTML file inside a JSON string is
                // escaping hell for the chain models; marker blocks parse deterministically.
                let p = format!(
                    "Build the MVP per this spec. SPEC: {spec}{fix}\n\nWrite COMPLETE, working files — no \
                     placeholders, no TODOs, self-contained (inline CSS/JS if html; stdlib-only if python). \
                     At most 3 files, prefer ONE index.html. Emit each file EXACTLY as:\n\
                     ===== FILE: relative/path.ext =====\n<raw file content>\n===== END =====\n\
                     Nothing else — no prose, no markdown fences."
                );
                let cfg = GenerationConfig { max_tokens: 7500, ..GenerationConfig::default() };
                match self.inference.chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&p)], cfg).await {
                    Ok(r) => {
                        let dir = Self::forge_dir(&id);
                        let mut written: Vec<String> = Vec::new();
                        let mut total = 0usize;
                        for block in r.text.split("===== FILE:").skip(1) {
                            let Some(hdr_end) = block.find("=====") else { continue };
                            let path = block[..hdr_end].trim().to_string();
                            let body_start = hdr_end + 5;
                            let body = match block[body_start..].find("===== END") {
                                Some(e) => &block[body_start..body_start + e],
                                None => &block[body_start..],
                            };
                            let content = body.trim_start_matches('\n').trim_end();
                            if path.contains("..") || path.starts_with('/') || path.len() > 80 || path.is_empty() || content.len() < 40 {
                                continue;
                            }
                            total += content.len();
                            if total > 400_000 || written.len() >= 6 { break; }
                            let fp = dir.join(&path);
                            if let Some(parent) = fp.parent() { let _ = std::fs::create_dir_all(parent); }
                            if std::fs::write(&fp, content).is_ok() { written.push(path); }
                        }
                        if written.is_empty() {
                            format!("⚒️ `{id}` build emitted no parseable files — will retry next tick.")
                        } else {
                            all[&id]["files"] = serde_json::json!(written);
                            all[&id]["stage"] = serde_json::json!("test");
                            format!("⚒️ `{id}` built: {} file(s) → {}", written.len(), dir.display())
                        }
                    }
                    Err(e) => format!("⚒️ `{id}` build failed ({e}) — will retry."),
                }
            }
            "test" => {
                // Deterministic gates: files exist and non-trivial; python compiles in the sandbox;
                // html is structurally whole. Honest results either way.
                let dir = Self::forge_dir(&id);
                let files: Vec<String> = v.get("files").and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.to_string()).collect())
                    .unwrap_or_default();
                let mut results: Vec<String> = Vec::new();
                let mut pass = !files.is_empty();
                if files.is_empty() { results.push("FAIL: no files were written".into()); }
                for fname in &files {
                    let fp = dir.join(fname);
                    let content = std::fs::read_to_string(&fp).unwrap_or_default();
                    if content.len() < 80 {
                        results.push(format!("FAIL: {fname} is trivially small ({} bytes)", content.len()));
                        pass = false;
                        continue;
                    }
                    if fname.ends_with(".py") {
                        if let Some(sb) = &self.sandbox {
                            let check = format!("import ast, sys\nsrc = open({:?}).read()\ntry:\n    ast.parse(src)\n    print('OK')\nexcept SyntaxError as e:\n    print('SYNTAX:', e)", fp.to_string_lossy());
                            match sb.run_python(&check).await {
                                Ok(r) if r.exit_code == 0 && !r.render().contains("SYNTAX") => results.push(format!("PASS: {fname} parses")),
                                Ok(r) => { results.push(format!("FAIL: {fname} — {}", r.render().chars().take(150).collect::<String>())); pass = false; }
                                Err(_) => results.push(format!("SKIP: {fname} (sandbox unavailable)")),
                            }
                        }
                    } else if fname.ends_with(".html") {
                        let whole = content.contains("<html") && content.contains("</html>");
                        if whole { results.push(format!("PASS: {fname} structurally whole")); }
                        else { results.push(format!("FAIL: {fname} missing html envelope")); pass = false; }
                    } else {
                        results.push(format!("PASS: {fname} present ({} bytes)", content.len()));
                    }
                }
                all[&id]["test"] = serde_json::json!({"pass": pass, "results": results});
                all[&id]["stage"] = serde_json::json!("rate");
                format!("⚒️ `{id}` tested: {} — {}", if pass { "GREEN" } else { "RED" }, results.join("; ").chars().take(220).collect::<String>())
            }
            "rate" => {
                let dir = Self::forge_dir(&id);
                let spec = v.get("spec").cloned().unwrap_or_default();
                let test = v.get("test").cloned().unwrap_or_default();
                let files: Vec<String> = v.get("files").and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.to_string()).collect())
                    .unwrap_or_default();
                let mut listing = String::new();
                for fname in files.iter().take(4) {
                    let c = std::fs::read_to_string(dir.join(fname)).unwrap_or_default();
                    listing.push_str(&format!("\n===== {fname} =====\n{}", c.chars().take(6000).collect::<String>()));
                }
                let p = format!(
                    "You are the REFEREE. Judge this MVP strictly against its own spec and PRE-REGISTERED kill \
                     criteria.\nSPEC: {spec}\nTEST RESULTS: {test}\nARTIFACT:{listing}\n\nOutput ONLY JSON: \
                     {{\"score\": 0-10, \"issues\": [\"most important fixes, concrete\"], \"kill\": true|false, \
                     \"kill_reason\": \"which pre-registered criterion fired, or empty\"}}"
                );
                let cfg = GenerationConfig { max_tokens: 500, ..GenerationConfig::default() };
                // CROSS-FAMILY JUDGING: the referee must not share the builder's model family —
                // same-family self-grading inflates scores. Judge on minimax (env-overridable);
                // fall back to the default chain only if the judge provider is down.
                let judge_label = std::env::var("YM_FORGE_JUDGE").unwrap_or_else(|_| "minimax".into());
                let msgs = vec![ChatMessage::system(&self.persona), ChatMessage::user(&p)];
                let judged = match self.inference.clone().with_provider(&judge_label).chat(msgs.clone(), cfg.clone()).await {
                    Ok(r) => Ok(r),
                    Err(_) => self.inference.chat(msgs, cfg).await,
                };
                match judged {
                    Ok(r) => match Self::forge_json_grab(&r.text) {
                        Some(j) => {
                            let score = j.get("score").and_then(|x| x.as_i64()).unwrap_or(0);
                            let kill = j.get("kill").and_then(|x| x.as_bool()).unwrap_or(false);
                            all[&id]["rating"] = j.clone();
                            all[&id]["stage"] = serde_json::json!("iterate");
                            format!("⚒️ `{id}` rated {score}/10{}", if kill { " — a kill criterion FIRED" } else { "" })
                        }
                        None => format!("⚒️ `{id}` rating didn't parse — will retry."),
                    },
                    Err(e) => format!("⚒️ `{id}` rating failed ({e}) — will retry."),
                }
            }
            "iterate" => {
                let rating = v.get("rating").cloned().unwrap_or_default();
                let score = rating.get("score").and_then(|x| x.as_i64()).unwrap_or(0);
                let kill = rating.get("kill").and_then(|x| x.as_bool()).unwrap_or(false);
                let iter = v.get("iter").and_then(|x| x.as_i64()).unwrap_or(0);
                let max_iter = v.get("max_iter").and_then(|x| x.as_i64()).unwrap_or(2);
                let test_pass = v.get("test").and_then(|t| t.get("pass")).and_then(|x| x.as_bool()).unwrap_or(false);
                if kill {
                    all[&id]["stage"] = serde_json::json!("killed");
                    format!("⚒️ `{id}` KILLED by its own pre-registered criterion: {} — that's the discipline working, not a failure.",
                        rating.get("kill_reason").and_then(|x| x.as_str()).unwrap_or("unspecified"))
                } else if score >= 7 && test_pass {
                    all[&id]["stage"] = serde_json::json!("shipped");
                    let dir = Self::forge_dir(&id);
                    let name = v.get("spec").and_then(|sp| sp.get("name")).and_then(|x| x.as_str()).unwrap_or("the product");
                    format!("🚢 `{id}` SHIPPED: **{name}** rated {score}/10 after {iter} iteration(s). Artifact: {} — open it and judge for yourself.", dir.display())
                } else if iter >= max_iter {
                    all[&id]["stage"] = serde_json::json!("shipped");
                    format!("⚒️ `{id}` shipped AS-IS at {score}/10 after exhausting {max_iter} iterations — honest ceiling, artifacts kept for review.")
                } else {
                    all[&id]["iter"] = serde_json::json!(iter + 1);
                    all[&id]["stage"] = serde_json::json!("build");
                    format!("⚒️ `{id}` iterating ({}/{max_iter}): rebuilding against {} referee issue(s).",
                        iter + 1, rating.get("issues").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0))
                }
            }
            _ => return None,
        };
        all[&id]["updated_ms"] = serde_json::json!(chrono::Utc::now().timestamp_millis());
        if let Some(l) = all[&id]["log"].as_array_mut() {
            l.push(serde_json::json!(report.clone()));
            if l.len() > 40 { l.remove(0); }
        }
        self.forge_save(&all).await;
        Some(report)
    }

    pub async fn code_cmd(&self, arg: &str) -> String {
        let a = arg.trim();
        if let Some(url) = a.strip_prefix("add ").map(str::trim).filter(|x| x.starts_with("http")) {
            let mut repos = self.code_repos().await;
            if !repos.iter().any(|x| x.eq_ignore_ascii_case(url)) {
                repos.push(url.to_string());
                let _ = self.memory.profile_set("code_repos", &serde_json::to_string(&repos).unwrap_or_default()).await;
            }
            let url2 = url.to_string();
            return match tokio::task::spawn_blocking(move || mind_tools::code::sync_and_digest(&url2)).await {
                Ok(Ok(dig)) => format!("📁 Cloned {} — grounded now. Recent picture:\n{}", mind_tools::code::repo_name(url), dig.lines().take(8).collect::<Vec<_>>().join("\n")),
                Ok(Err(e)) => format!("📁 Registered {} but clone failed: {e}", mind_tools::code::repo_name(url)),
                Err(_) => "📁 clone task panicked".to_string(),
            };
        }
        if let Some(name) = a.strip_prefix("remove ").or_else(|| a.strip_prefix("rm ")).map(str::trim).filter(|x| !x.is_empty()) {
            let mut repos = self.code_repos().await;
            let before = repos.len();
            repos.retain(|x| mind_tools::code::repo_name(x) != name && !x.contains(name));
            let _ = self.memory.profile_set("code_repos", &serde_json::to_string(&repos).unwrap_or_default()).await;
            return if repos.len() < before { format!("📁 Unregistered {name}.") } else { format!("Not registered: {name}") };
        }
        if let Some(rest) = a.strip_prefix("study ").map(str::trim) {
            let deep = rest.strip_prefix("deep ").map(str::trim);
            let url = deep.unwrap_or(rest);
            if url.starts_with("http") {
                let mut repos = self.code_repos().await;
                if !repos.iter().any(|x| x.eq_ignore_ascii_case(url)) {
                    repos.push(url.to_string());
                    let _ = self.memory.profile_set("code_repos", &serde_json::to_string(&repos).unwrap_or_default()).await;
                }
                return if deep.is_some() { self.code_study_deep(url).await } else { self.code_study(url).await };
            }
        }
        if let Some(rest) = a.strip_prefix("ask ").map(str::trim).filter(|x| !x.is_empty()) {
            let mut it = rest.splitn(2, char::is_whitespace);
            let name = it.next().unwrap_or("").trim();
            let question = it.next().unwrap_or("").trim();
            if name.is_empty() || question.is_empty() {
                return "Usage: code ask <repo-name> <question>  (study it first: `code study <git url>`)".to_string();
            }
            return self.code_ask(name, question).await;
        }
        if a == "sync" || a == "pull" {
            let repos = self.code_repos().await;
            if repos.is_empty() {
                return "📁 No repos registered — `code add <git url>`.".to_string();
            }
            let mut ok = 0;
            for url in &repos {
                let u = url.clone();
                if tokio::task::spawn_blocking(move || mind_tools::code::sync_repo(&u)).await.map(|r| r.is_ok()).unwrap_or(false) {
                    ok += 1;
                }
            }
            return format!("📁 Synced {ok}/{} repos.", repos.len());
        }
        if !a.is_empty() {
            // treat as a repo name → show its digest (what's in it, recent commits)
            let repos = self.code_repos().await;
            if let Some(url) = repos.iter().find(|u| mind_tools::code::repo_name(u).eq_ignore_ascii_case(a) || u.contains(a)) {
                let u = url.clone();
                return match tokio::task::spawn_blocking(move || mind_tools::code::sync_and_digest(&u)).await {
                    Ok(Ok(dig)) => format!("📁 {dig}"),
                    _ => format!("📁 Couldn't read {a} right now."),
                };
            }
            return format!("📁 Not registered: {a}. `code` lists them.");
        }
        let repos = self.code_repos().await;
        if repos.is_empty() {
            return "📁 CODEOPS — no repos yet. `code add <git url>` (private repos clone via my GitHub token). Then WorkOps grounds each project scan in the real code.".to_string();
        }
        format!(
            "📁 CODEOPS — repos I read to ground WorkOps:\n{}\n\n`code add <url>` · `code sync` · `code <name>` (digest + recent commits).",
            repos.iter().map(|u| format!("  • {}", mind_tools::code::repo_name(u))).collect::<Vec<_>>().join("\n")
        )
    }

    /// STUDY a repo once → distilled architecture facts saved as per-repo beliefs ([code:<name>]),
    /// so future questions answer from MEMORY without re-reading the source into context. Detached;
    /// posts a learning summary on completion.
    /// STUDY a repo the way an engineer actually would: build a module map (one unit per crate /
    /// top-level source dir), DEEP-READ each module's full source in its own LLM pass, then a final
    /// SYNTHESIS pass over the module summaries for the cross-module architecture. Every fact is
    /// tagged `[mod:<module>]` for provenance and namespaced by a distinctive `codekb<alnum>` token
    /// so a 100+-fact study is retrievable without truncation. Detached; reports honest coverage.
    /// HYBRID study (default, cheap): parse the structural facts DETERMINISTICALLY (public API,
    /// types, module docs, internal deps — zero LLM), then ONE interpretive synthesis pass over the
    /// parsed skeleton for the cross-module "how it composes" facts. ~1 LLM call vs. deep's ~19.
    pub async fn code_study(&self, git_url: &str) -> String {
        let name = mind_tools::code::repo_name(git_url);
        let url = git_url.to_string();
        let (det, _name) = match tokio::task::spawn_blocking(move || mind_tools::code::deterministic_study(&url)).await {
            Ok(Ok((n, d))) => (d, n),
            Ok(Err(e)) => return format!("📖 Couldn't study {name}: {e}"),
            Err(_) => return format!("📖 Study of {name} panicked."),
        };
        if det.facts.is_empty() {
            return format!("📖 Cloned {name} but parsed no public API to learn — try `code study deep {name}` for an LLM read.");
        }
        let module_count = det.module_count;
        let file_count = det.file_count;
        let det_n = det.facts.len();
        let skeleton = det.skeleton.clone();
        let det_facts = det.facts.clone();
        let name2 = name.clone();
        let nq = self.notify_queue.clone();
        let inf = self.inference.clone();
        let mem = self.memory.clone();
        let persona = self.persona.clone();
        tokio::spawn(async move {
            let alnum: String = name2.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
            let token = format!("codekb{alnum}");
            // 1. Save the deterministic (parsed) facts — free, grep-grounded, never invented.
            let mut saved = 0usize;
            for (module, fact) in &det_facts {
                let statement = format!("{token} [code:{name2}] [mod:{module}] [det] {fact}");
                if mem.remember_as_belief(BeliefAssertion {
                    statement, polarity: 1.0, weight: 2.2,
                    source_event: Some("code-study".into()), provenance: "studied".into(),
                }).await.is_ok() { saved += 1; }
            }
            // 2. ONE synthesis pass: interpret the parsed skeleton for cross-module architecture.
            let synth_prompt = format!(
                "Below is the PARSED structure (modules, public types, functions, internal deps) of the \
                 `{name2}` codebase — extracted deterministically, so treat it as ground truth. State 8-12 \
                 CROSS-MODULE architecture facts a senior engineer needs that the raw structure doesn't make \
                 explicit: the main entry point(s); the end-to-end control/data flow across modules; how the \
                 modules compose and depend on each other; the central types that thread through several \
                 modules; the concurrency model; and the single most important design decision. Each ONE \
                 specific sentence naming real modules/types from the structure. Do NOT restate the raw lists. \
                 Output ONLY JSON: {{\"facts\":[\"...\"]}}.\n\n{skeleton}"
            );
            let cfg = GenerationConfig { max_tokens: 900, ..GenerationConfig::default() };
            let mut synth_n = 0usize;
            if let Ok(r) = inf.chat(vec![ChatMessage::system(&persona), ChatMessage::user(&synth_prompt)], cfg).await {
                let synth: Vec<String> = r.text
                    .find('{')
                    .and_then(|a| r.text.rfind('}').map(|b| r.text[a..=b].to_string()))
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|j| j.get("facts").and_then(|x| x.as_array()).cloned())
                    .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| x.len() > 12).collect())
                    .unwrap_or_default();
                for fact in synth {
                    let statement = format!("{token} [code:{name2}] [mod:architecture] [syn] {fact}");
                    if mem.remember_as_belief(BeliefAssertion {
                        statement, polarity: 1.0, weight: 2.2,
                        source_event: Some("code-study".into()), provenance: "studied".into(),
                    }).await.is_ok() { saved += 1; synth_n += 1; }
                }
            }
            let _ = mem.profile_set(&format!("code_studied_{name2}"), &chrono::Utc::now().timestamp_millis().to_string()).await;
            nq.lock().unwrap().push(format!(
                "📖 Studied **{name2}** — {det_n} facts parsed straight from source across {module_count} modules \
                 ({file_count} files) + {synth_n} architecture facts from a single synthesis pass = {saved} total, \
                 for the cost of ONE LLM call. Ask me anything: `code ask {name2} <question>`. (`code study deep {name2}` \
                 for a fuller per-module LLM read.)"
            ));
        });
        format!(
            "📖 Studying {name} — parsing {module_count} modules / {file_count} files for structure now (no LLM cost), \
             then one synthesis pass for the architecture. I'll confirm coverage in a moment; then `code ask {name} <question>`."
        )
    }

    /// DEEP study (opt-in, `code study deep <url>`): per-module LLM deep-read + synthesis. Higher
    /// fidelity, ~19 LLM calls — use when depth matters more than token cost.
    pub async fn code_study_deep(&self, git_url: &str) -> String {
        let name = mind_tools::code::repo_name(git_url);
        let url = git_url.to_string();
        // ~14KB per deep-read chunk: big enough to hold real functions, small enough for one pass.
        let modules = match tokio::task::spawn_blocking(move || mind_tools::code::study_modules(&url, 14000)).await {
            Ok(Ok((_n, m))) => m,
            Ok(Err(e)) => return format!("📖 Couldn't study {name}: {e}"),
            Err(_) => return format!("📖 Study of {name} panicked."),
        };
        if modules.is_empty() {
            return format!("📖 Cloned {name} but found no readable source to study.");
        }
        let total_files: usize = modules.iter().map(|m| m.file_count).sum();
        let module_count = modules.len();
        let name2 = name.clone();
        let nq = self.notify_queue.clone();
        let inf = self.inference.clone();
        let mem = self.memory.clone();
        let persona = self.persona.clone();
        tokio::spawn(async move {
            let alnum: String = name2.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
            let token = format!("codekb{alnum}");
            // Bound total LLM passes so a huge monorepo can't run away: at most 24 deep-read passes.
            let mut budget_passes = 24usize;
            let mut all_facts: Vec<(String, String)> = Vec::new(); // (module, fact)
            let mut modules_covered = 0usize;
            let mut chunks_read = 0usize;

            for m in &modules {
                if budget_passes == 0 { break; }
                let mut module_facts: Vec<String> = Vec::new();
                // At most 2 chunks/module so breadth wins over depth-in-one-place on a first study.
                for chunk in m.chunks.iter().take(2) {
                    if budget_passes == 0 { break; }
                    budget_passes -= 1;
                    chunks_read += 1;
                    let prompt = format!(
                        "You are reading the FULL source of module `{module}` from the `{repo}` codebase. \
                         Extract 4-8 CONCRETE facts a senior engineer needs to work on THIS module without \
                         re-reading it: its responsibility; the key types/structs/enums and functions and what \
                         each does; the important control flow; notable patterns, invariants, and external deps. \
                         Each fact ONE specific sentence naming real identifiers from the code. Do NOT speculate \
                         beyond what's shown. Output ONLY JSON: {{\"facts\":[\"...\"]}}.\n\n{src}",
                        module = m.name, repo = name2, src = chunk
                    );
                    let cfg = GenerationConfig { max_tokens: 700, ..GenerationConfig::default() };
                    let resp = match inf.chat(vec![ChatMessage::system(&persona), ChatMessage::user(&prompt)], cfg).await {
                        Ok(r) => r.text,
                        Err(_) => continue,
                    };
                    let got: Vec<String> = resp
                        .find('{')
                        .and_then(|a| resp.rfind('}').map(|b| resp[a..=b].to_string()))
                        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                        .and_then(|j| j.get("facts").and_then(|x| x.as_array()).cloned())
                        .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| x.len() > 12).collect())
                        .unwrap_or_default();
                    module_facts.extend(got);
                }
                if !module_facts.is_empty() {
                    modules_covered += 1;
                    for fact in module_facts {
                        all_facts.push((m.name.clone(), fact));
                    }
                }
            }

            if all_facts.is_empty() {
                nq.lock().unwrap().push(format!(
                    "📖 Read {module_count} modules of {name2} but couldn't distill clean facts — the source may be too sparse."
                ));
                return;
            }

            // SYNTHESIS pass: cross-module architecture the per-module passes structurally can't see —
            // entry points, the end-to-end data/control flow, how the modules compose.
            let module_summary = all_facts.iter()
                .map(|(m, fct)| format!("[{m}] {fct}"))
                .collect::<Vec<_>>().join("\n");
            let synth_prompt = format!(
                "Below are per-module facts I learned studying the `{name2}` codebase. Now state 6-10 \
                 CROSS-MODULE architecture facts the per-module view misses: the main entry point(s); the \
                 end-to-end control/data flow across modules; how the modules depend on and compose with each \
                 other; the central types that thread through several modules; and the single most important \
                 design decision. Each ONE specific sentence. Output ONLY JSON: {{\"facts\":[\"...\"]}}.\n\n{module_summary}"
            );
            let cfg = GenerationConfig { max_tokens: 800, ..GenerationConfig::default() };
            if let Ok(r) = inf.chat(vec![ChatMessage::system(&persona), ChatMessage::user(&synth_prompt)], cfg).await {
                let synth: Vec<String> = r.text
                    .find('{')
                    .and_then(|a| r.text.rfind('}').map(|b| r.text[a..=b].to_string()))
                    .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                    .and_then(|j| j.get("facts").and_then(|x| x.as_array()).cloned())
                    .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| x.len() > 12).collect())
                    .unwrap_or_default();
                for fact in synth {
                    all_facts.push(("architecture".to_string(), fact));
                }
            }

            // Save every fact: {token} [code:<repo>] [mod:<module>] <fact>. The token namespaces the
            // study; [mod:] carries provenance so code_ask can cite which module grounds an answer.
            let mut saved = 0usize;
            for (module, fact) in &all_facts {
                let statement = format!("{token} [code:{name2}] [mod:{module}] {fact}");
                if mem
                    .remember_as_belief(BeliefAssertion {
                        statement,
                        polarity: 1.0,
                        weight: 2.2,
                        source_event: Some("code-study".into()),
                        provenance: "studied".into(),
                    })
                    .await
                    .is_ok()
                {
                    saved += 1;
                }
            }
            let _ = mem.profile_set(&format!("code_studied_{name2}"), &chrono::Utc::now().timestamp_millis().to_string()).await;
            nq.lock().unwrap().push(format!(
                "📖 Studied **{name2}** in depth — read {chunks_read} source passes across {modules_covered}/{module_count} modules \
                 ({total_files} files) and learned {saved} facts into memory (per-module + cross-module synthesis). \
                 I can now answer questions about it WITHOUT re-reading the code: `code ask {name2} <question>`."
            ));
        });
        format!(
            "📖 Studying {name} in depth now — {module_count} modules / {total_files} files, one deep-read pass per \
             module plus a synthesis pass. I'll distill it into memory and confirm coverage when done (a few minutes). \
             After that, `code ask {name} <question>` answers from what I learned, no re-reading needed."
        )
    }

    /// Answer a question about a studied repo from its DISTILLED beliefs — no source re-read.
    pub async fn code_ask(&self, name: &str, question: &str) -> String {
        let alnum: String = name.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
        let token = format!("codekb{alnum}");
        let tag = format!("[code:{name}]");
        // Retrieve by the DISTINCTIVE per-repo token only — matching on the repo name would be
        // swamped by the hundreds of ordinary beliefs that mention it (the 20-cap buries the facts).
        // Uncapped retrieval (beliefs_matching_n) — a real study is 100+ facts; the default 20-cap
        // would silently drop most of the knowledge at answer time.
        let facts: Vec<String> = self
            .memory
            .beliefs_matching_n(&token, 400, &mind_types::AccessContext::Operator)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|b| b.statement)
            .filter(|st| st.contains(&token))
            .collect();
        if facts.is_empty() {
            return format!("I haven't studied {name} yet — `code study <git url>` first, then ask.");
        }
        // Keep the [mod:<module>] provenance visible to the LLM so it can cite which module grounds
        // each claim; strip only the retrieval token and the [code:] tag.
        let strip = |f: &str| {
            let mut t = f.replacen(&token, "", 1).replacen(&tag, "", 1);
            // Provenance tags → human words the model can cite naturally, not raw brackets.
            t = t.replace("[det]", "(parsed)").replace("[syn]", "(inferred)");
            t = t.trim().to_string();
            t
        };
        let block = facts.iter().map(|f| format!("- {}", strip(f))).collect::<Vec<_>>().join("\n");

        // ACTIVE LEARNING: identifiers the question names that NO stored fact mentions are knowledge
        // gaps. Grep the synced repo for just those definitions and read a focused excerpt — then
        // learn what the answer distills, so the same gap never needs disk again.
        let facts_lower = facts.join(" ").to_lowercase();
        let gaps: Vec<String> = question
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .filter(|w| w.len() >= 4 && w.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false))
            .filter(|w| !facts_lower.contains(&w.to_lowercase()))
            .map(|w| w.to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .take(3)
            .collect();
        let repo_key = name.to_string();
        let gaps2 = gaps.clone();
        let excerpts = if gaps.is_empty() {
            vec![]
        } else {
            tokio::task::spawn_blocking(move || mind_tools::code::lookup_symbols(&repo_key, &gaps2, 28))
                .await
                .unwrap_or_default()
        };
        let (extra, want_learn) = if excerpts.is_empty() {
            (String::new(), false)
        } else {
            let ex = excerpts
                .iter()
                .map(|(loc, code)| format!("--- {loc} ---\n{code}"))
                .collect::<Vec<_>>()
                .join("\n")
                .chars()
                .take(5000)
                .collect::<String>();
            (format!(
                "\n\nTARGETED SOURCE (I just read these definitions from disk because my facts didn't cover them):\n{ex}\n\n\
                 Since you have fresh source above, ALSO distill 1-3 new one-sentence facts about it worth remembering. \
                 Output ONLY JSON: {{\"answer\":\"...\",\"learned\":[\"...\"]}}."
            ), true)
        };
        let prompt = format!(
            "Answer the question about the {name} codebase using the facts I learned when I studied it{}. \
             Each fact is prefixed with the module it came from, like `[mod:mind-memory]`. Be specific — name the \
             modules and types the facts mention, and when useful cite the module a claim comes from. If neither \
             facts nor source cover it, say exactly which module or file I'd need to re-read — never invent.\n\n\
             WHAT I LEARNED ABOUT {name}:\n{block}\n\nQUESTION: {question}{extra}",
            if want_learn { " plus the targeted source below" } else { "" }
        );
        let cfg = GenerationConfig { max_tokens: 600, ..GenerationConfig::default() };
        let resp = match self.inference.chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg).await {
            Ok(r) => r.text,
            Err(e) => return format!("(couldn't compose an answer: {e})"),
        };
        if !want_learn {
            return format!("{}\n\n_(answered from {} studied facts — no code re-read)_", resp.trim(), facts.len());
        }
        // Parse {answer, learned}; save learned facts so this gap is covered from memory next time.
        let parsed = resp
            .find('{')
            .and_then(|a| resp.rfind('}').map(|b| resp[a..=b].to_string()))
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
        let (answer, learned): (String, Vec<String>) = match &parsed {
            Some(j) => (
                j.get("answer").and_then(|x| x.as_str()).unwrap_or(resp.trim()).to_string(),
                j.get("learned")
                    .and_then(|x| x.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.trim().to_string()).filter(|x| x.len() > 12).collect())
                    .unwrap_or_default(),
            ),
            None => (resp.trim().to_string(), vec![]),
        };
        let mut learned_n = 0usize;
        for fact in &learned {
            let loc = excerpts.first().map(|(l, _)| l.split(':').next().unwrap_or("?").to_string()).unwrap_or_else(|| "?".into());
            let statement = format!("{token} {tag} [mod:{loc}] [learned] {fact}");
            if self.memory.remember_as_belief(BeliefAssertion {
                statement, polarity: 1.0, weight: 2.2,
                source_event: Some("code-ask-learn".into()), provenance: "studied".into(),
            }).await.is_ok() { learned_n += 1; }
        }
        format!(
            "{}\n\n_(answered from {} studied facts + a targeted re-read of {}; learned {} new fact{} from it)_",
            answer.trim(), facts.len(),
            excerpts.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join(", "),
            learned_n, if learned_n == 1 { "" } else { "s" }
        )
    }

    /// Local code digest for a WorkOps subject, if a registered repo name matches it. Grounds the
    /// field-scan in the actual current code + recent commits.
    pub(crate) async fn code_context_for(&self, subject: &str) -> String {
        let sl = subject.to_lowercase();
        let repos = self.code_repos().await;
        let Some(url) = repos.into_iter().find(|u| {
            let n = mind_tools::code::repo_name(u).to_lowercase();
            sl.contains(&n) || n.contains(&sl.split_whitespace().next().unwrap_or("").to_string())
        }) else {
            return String::new();
        };
        match tokio::task::spawn_blocking(move || mind_tools::code::sync_and_digest(&url)).await {
            Ok(Ok(dig)) => format!("\n\nFROM THE ACTUAL REPO (current code — treat as authoritative over any web result):\n{}", dig.chars().take(2200).collect::<String>()),
            _ => String::new(),
        }
    }

    /// Ask for one referee-bound suggestion after a repo-grounded WorkOps scan. Invalid or absent
    /// model output is simply discarded; this shadow path never builds or executes proposals.
    pub(crate) async fn spool_work_proposal(&self, repo: &str, report: &str, code_ctx: &str) {
        let prompt = format!(
            "A WorkOps research pass just studied repository {repo}.\n\nRESEARCH REPORT:\n{report}\n\nREPOSITORY DIGEST:\n{code_ctx}\n\n\
             If the evidence supports one concrete, minimal code improvement, output ONLY one JSON object with exactly these fields:\n\
             {{\"repo\":\"{repo}\",\"goal\":\"one imperative sentence\",\"citations\":[\"source from the report\"],\"base_sha\":\"current commit hash from the digest\",\"acceptance_test\":\"specific test command\",\"why_not\":\"strongest reason not to merge\",\"p_merge\":0.0}}\n\
             p_merge must be between 0 and 1. Use only citations present in the report. If any field cannot be grounded, output null. This is a shadow proposal only; do not suggest executing it."
        );
        let cfg = GenerationConfig { max_tokens: 450, ..GenerationConfig::default() };
        let Ok(response) = self.inference.chat(vec![ChatMessage::user(&prompt)], cfg).await else { return };
        let Some(start) = response.text.find('{') else { return };
        let Some(end) = response.text.rfind('}') else { return };
        if end <= start { return; }
        let Ok(proposal) = ProjectProposal::from_json(&response.text[start..=end]) else { return };
        let _ = spool_project_proposals(Path::new(PROJECT_PROPOSALS_DIR), [proposal]);
    }

    pub(crate) async fn work_subjects(&self) -> Vec<String> {
        let stored: Vec<String> = self
            .memory
            .profile_get("work_subjects")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if !stored.is_empty() {
            return stored;
        }
        // Seed from his known projects (the mind already holds these as beliefs).
        let seed: Vec<String> = ["SDF Protocol", "ContextCache", "YantrikDB", "ToolFormerMicro", "agentweb", "anandotsav"]
            .iter()
            .map(|x| x.to_string())
            .collect();
        let _ = self.memory.profile_set("work_subjects", &serde_json::to_string(&seed).unwrap_or_default()).await;
        seed
    }

    /// `work` / `work list` / `work add <s>` / `work remove <s>` / `work run`.
    pub async fn work_cmd(&self, arg: &str) -> String {
        let a = arg.trim();
        if let Some(sub) = a.strip_prefix("add ").map(str::trim).filter(|x| !x.is_empty()) {
            let mut subs = self.work_subjects().await;
            if !subs.iter().any(|x| x.eq_ignore_ascii_case(sub)) {
                subs.push(sub.to_string());
                let _ = self.memory.profile_set("work_subjects", &serde_json::to_string(&subs).unwrap_or_default()).await;
            }
            return format!("🛠 Watching \"{sub}\" now. `work` lists the set.");
        }
        if let Some(sub) = a.strip_prefix("remove ").or_else(|| a.strip_prefix("rm ")).map(str::trim).filter(|x| !x.is_empty()) {
            let mut subs = self.work_subjects().await;
            let before = subs.len();
            subs.retain(|x| !x.eq_ignore_ascii_case(sub));
            let _ = self.memory.profile_set("work_subjects", &serde_json::to_string(&subs).unwrap_or_default()).await;
            return if subs.len() < before { format!("🛠 Stopped watching \"{sub}\".") } else { format!("Not in the watch set: \"{sub}\".") };
        }
        if a == "run" || a == "now" {
            return self.work_watch_run().await.unwrap_or_else(|| "🛠 WorkOps ran — no field movement worth surfacing (the research still landed in memory), or the research envelope is dry.".to_string());
        }
        let subs = self.work_subjects().await;
        format!(
            "🛠 WORKOPS — watching your projects (nightly field-scan, belief-revising, speaks only on change):\n{}\n\n`work add <project>` · `work remove <project>` · `work run` (force a pass now).",
            subs.iter().map(|x| format!("  • {x}")).collect::<Vec<_>>().join("\n")
        )
    }

    pub async fn work_watch_due(&self) -> bool {
        use chrono::Timelike;
        let h = local_now().hour();
        if !(8..=22).contains(&h) {
            return false;
        }
        let period_ms = (std::env::var("YM_WORKOPS_HOURS").ok().and_then(|v| v.parse::<f64>().ok()).unwrap_or(8.0) * 3_600_000.0) as i64;
        let last: i64 = self.memory.profile_get("workops_last").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        chrono::Utc::now().timestamp_millis() - last >= period_ms
    }

    /// One WorkOps pass: research-revise the next project in the rotation; surface only on change.
    /// A GitHub-activity glance rides along when there's unread work.
    pub async fn work_watch_run(&self) -> Option<String> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let _ = self.memory.profile_set("workops_last", &now_ms.to_string()).await;
        self.researcher.as_ref()?;
        let subjects = self.work_subjects().await;
        if subjects.is_empty() {
            return None;
        }
        // round-robin cursor so it walks the whole portfolio, not one project.
        let cursor: usize = self.memory.profile_get("workops_cursor").await.ok().flatten().and_then(|s| s.parse().ok()).unwrap_or(0);
        let subject = subjects[cursor % subjects.len()].clone();
        let _ = self.memory.profile_set("workops_cursor", &((cursor + 1) % subjects.len()).to_string()).await;
        // DISAMBIGUATE: a bare project name ("SDF Protocol") collides with unrelated things in
        // search (Syrian Democratic Forces, Stellar, NIST). Ground the query in what the mind
        // already knows THIS project is, so the scan is about HIS work, not a namesake.
        let ident: Vec<String> = self
            .memory
            .beliefs_matching(&subject, &mind_types::AccessContext::Operator)
            .await
            .unwrap_or_default()
            .iter()
            .take(3)
            .map(|b| b.statement.chars().take(140).collect::<String>())
            .collect();
        let context = if ident.is_empty() {
            String::new()
        } else {
            format!(" (this is Pranab Sarkar's project — for disambiguation: {})", ident.join("; "))
        };
        // Ground in the ACTUAL repo when we have it — the scan reasons about current code.
        let code_ctx = self.code_context_for(&subject).await;
        // Belief-revising field scan on the project (treasury-gated inside research_revise).
        let report = self
            .research_revise(&format!(
                "{subject}{context} — latest developments in this specific space, competing/similar approaches, and relevant research. Ignore unrelated same-named entities.{code_ctx}"
            ))
            .await
            .ok()?;
        if !code_ctx.is_empty() {
            self.spool_work_proposal(&subject, &report, &code_ctx).await;
        }
        // GitHub activity glance (proven-strong signal; only when there's genuinely unread work).
        let mut gh = String::new();
        if let Some(g) = &self.github {
            if let Ok(notes) = g.notifications(8).await {
                if !notes.is_empty() {
                    gh = format!(
                        "\n\n📬 GitHub — {} unread: {}",
                        notes.len(),
                        notes.iter().take(4).map(|n| n.title.clone()).collect::<Vec<_>>().join(" · ")
                    );
                }
            }
        }
        if report.contains("nothing changed in what I believe") && gh.is_empty() {
            return None; // silent — the scan still updated memory
        }
        self.ledger_sent("workops", &format!("field-scanned {subject}")).await;
        Some(format!("🛠 WorkOps — I scanned **{subject}** for you:\n\n{report}{gh}"))
    }

}
