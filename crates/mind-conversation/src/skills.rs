//! Skill/capability parsing -- request parsers + the skill-bank handler. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    pub(crate) fn lang_str(l: CodeLang) -> &'static str {
        match l {
            CodeLang::Shell => "shell",
            CodeLang::Python => "python",
            CodeLang::Rust => "rust",
        }
    }

    pub(crate) fn lang_from_str(s: &str) -> CodeLang {
        match s {
            "rust" => CodeLang::Rust,
            "shell" => CodeLang::Shell,
            _ => CodeLang::Python,
        }
    }

    /// "save that/this/it as (a )?skill (called|named)? <name>" → skill name.
    pub(crate) fn parse_save_skill(text: &str) -> Option<String> {
        let l = text.to_lowercase();
        if !(l.contains("save") && l.contains("skill")) {
            return None;
        }
        // take the token(s) after "skill", "called", or "named"
        for marker in ["skill called ", "skill named ", "as skill ", "a skill called ", "skill "] {
            if let Some(i) = l.find(marker) {
                let name = text[i + marker.len()..]
                    .trim()
                    .trim_matches(|c: char| c == '"' || c == '\'' || c == '.')
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                    return Some(name.to_string());
                }
            }
        }
        None
    }

    /// "run/use (the )? skill <name>" / "use the <name> skill" → skill name.
    pub(crate) fn parse_run_skill(text: &str) -> Option<String> {
        let l = text.to_lowercase();
        for marker in ["run skill ", "use skill ", "run the skill ", "use the skill ", "invoke skill "] {
            if let Some(i) = l.find(marker) {
                let name = text[i + marker.len()..]
                    .trim()
                    .trim_matches(|c: char| c == '"' || c == '\'' || c == '.')
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
        None
    }

    pub(crate) fn wants_list_skills(text: &str) -> bool {
        let l = text.to_lowercase();
        ["list skills", "list my skills", "what skills", "which skills", "your skills", "what can you do"]
            .iter()
            .any(|p| l.contains(p))
    }

    /// "find/search (a )?skill for X" / "do you have a skill for/to X" / "any skill for X" → query.
    pub(crate) fn parse_find_skill(text: &str) -> Option<String> {
        let l = text.to_lowercase();
        let is_search = ["find a skill", "find skill", "search skill", "search for a skill",
            "do you have a skill", "any skill for", "is there a skill", "which skill", "skill for ", "skill to "]
            .iter()
            .any(|p| l.contains(p));
        if !is_search {
            return None;
        }
        // The query is whatever follows the last "for "/"to " marker, else the whole message.
        let q = ["skill for ", "skill to ", " for ", " to "]
            .iter()
            .filter_map(|m| l.rfind(m).map(|i| (i, m.len())))
            .max_by_key(|(i, _)| *i)
            .map(|(i, len)| text[i + len..].trim().trim_end_matches(['?', '.', '!']).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| text.trim().to_string());
        Some(q)
    }

    /// A topic too thin to research well (ask to scope it first).
    pub(crate) fn is_vague_topic(topic: &str) -> bool {
        let words = topic.split_whitespace().count();
        words <= 2 || topic.trim().len() < 8
    }

    /// Health + a quick parallel `nproc` across the worker pool (the `:workers` command).
    pub async fn workers_status(&self) -> String {
        let pool = match &self.workers {
            Some(p) => p,
            None => return "No worker pool configured (set YM_WORKERS).".into(),
        };
        let health = pool.health().await;
        let up = health.iter().filter(|(_, ok)| *ok).count();
        let mut s = format!("Worker pool: {}/{} up\n", up, health.len());
        for (h, ok) in &health {
            s.push_str(&format!("  {} {}\n", if *ok { "✓" } else { "✗" }, h));
        }
        let demo = pool.map("nproc", 8).await;
        let cores = demo
            .iter()
            .map(|(h, r)| format!("{}={}", h.split('@').last().unwrap_or(h), r.as_deref().unwrap_or("?")))
            .collect::<Vec<_>>()
            .join(" ");
        s.push_str(&format!("cores (parallel probe): {cores}"));
        s
    }

    /// "watch my inbox for X" / "let me know when X emails" / "tell me when ... email ... X" → the
    /// keyword/sender to monitor the inbox for. Persistent delegation: a WaitForCondition that polls
    /// the inbox until a match, then pings you. Distinct from task reminders (this is a monitor).
    pub(crate) fn parse_watch_request(text: &str) -> Option<String> {
        let low = text.to_lowercase();
        let is_monitor = (low.contains("watch")
            || low.contains("let me know when")
            || low.contains("tell me when")
            || low.contains("notify me when")
            || low.contains("ping me when"))
            && (low.contains("inbox") || low.contains("email") || low.contains("mail"));
        if !is_monitor {
            return None;
        }
        for marker in [" for ", " from ", " about ", "when "] {
            if let Some(idx) = low.find(marker) {
                let mut tail = text[idx + marker.len()..].trim().trim_end_matches(['.', '!', '?']).trim();
                for suf in [" emails", " arrives", " comes in", " shows up", " lands"] {
                    tail = tail.strip_suffix(suf).unwrap_or(tail).trim();
                }
                if tail.len() >= 2 && !tail.eq_ignore_ascii_case("an email") && !tail.eq_ignore_ascii_case("email") {
                    return Some(tail.to_string());
                }
            }
        }
        None
    }

    /// True if the text is a monitor request ("watch …", "tell me when …", "monitor …").
    pub(crate) fn is_monitor_verb(low: &str) -> bool {
        // Generous on the verb — the source gate (is_gh / url / inbox) is what keeps a match specific,
        // so recognizing more natural phrasings can't hijack ordinary chat. (Missing "track" made the
        // companion wrongly decline "track my git repos for issues/PRs".)
        low.contains("watch")
            || low.contains("monitor")
            || low.contains("track")
            || low.contains("keep an eye on")
            || low.contains("keep tabs")
            || low.contains("keep watch")
            || low.contains("keep me posted")
            || low.contains("keep me updated")
            || low.contains("keep me in the loop")
            || low.contains("stay on top of")
            || low.contains("look out for")
            || low.contains("alert me")
            || low.contains("notify me")
            || low.contains("let me know when")
            || low.contains("let me know about")
            || low.contains("let me know if")
            || low.contains("tell me when")
            || low.contains("ping me when")
            || low.contains("ping me if")
    }

    /// Pull the watched-for target after a connective ("for"/"says"/"shows"/…). Trims trailing noise.
    pub(crate) fn watch_target(text: &str, low: &str) -> Option<String> {
        for marker in [" for ", " says ", " shows ", " contains ", " mentions ", " about ", " has ", " when it "] {
            if let Some(idx) = low.find(marker) {
                let t = text[idx + marker.len()..].trim().trim_end_matches(['.', '!', '?']).trim();
                let t = t.strip_prefix("says ").or_else(|| t.strip_prefix("shows ")).unwrap_or(t).trim();
                if t.len() >= 2 {
                    return Some(t.to_string());
                }
            }
        }
        None
    }

    /// "watch <url> for X" / "tell me when <url> says X" → (url, X). Monitors any web page.
    pub(crate) fn parse_web_watch(text: &str) -> Option<(String, String)> {
        let low = text.to_lowercase();
        if !Self::is_monitor_verb(&low) {
            return None;
        }
        let url = mind_tools::first_url(text)?;
        let target = Self::watch_target(text, &low)?;
        Some((url, target))
    }

    /// "watch my github for X" / "tell me when a PR about X" → X (no URL, github-ish words present).
    pub(crate) fn parse_github_watch(text: &str) -> Option<String> {
        let low = text.to_lowercase();
        if !Self::is_monitor_verb(&low) {
            return None;
        }
        let is_gh = low.contains("github") || low.contains("repo") || low.contains("pull request")
            || low.contains(" pr ") || low.contains("issue") || low.contains("notification");
        if !is_gh || mind_tools::first_url(text).is_some() {
            return None;
        }
        Self::watch_target(text, &low)
    }

    /// "worker python: <code>" / "worker shell: <code>" → run code in a sandbox ON A WORKER (off the
    /// main box). Distinct prefix from the local "run python:" path so both coexist.
    pub(crate) fn parse_worker_run(text: &str) -> Option<(CodeLang, String)> {
        let l = text.trim();
        let low = l.to_lowercase();
        for (pat, lang) in [
            ("worker python:", CodeLang::Python),
            ("worker shell:", CodeLang::Shell),
            ("run python on a worker:", CodeLang::Python),
            ("run shell on a worker:", CodeLang::Shell),
        ] {
            if let Some(idx) = low.find(pat) {
                let code = l[idx + pat.len()..].trim().trim_matches('`').trim();
                if !code.is_empty() {
                    return Some((lang, code.to_string()));
                }
            }
        }
        None
    }

    /// "plan: X" / "task: X" / "automate X" / "set up a task to X" → a free-form goal for the NL
    /// planner (authors + runs a recipe). Explicit prefixes keep it from swallowing ordinary chat.
    pub(crate) fn parse_plan_request(text: &str) -> Option<String> {
        let l = text.trim();
        let low = l.to_lowercase();
        for p in ["plan:", "task:", "automate:", "do this:", "set up:"] {
            if let Some(rest) = low.strip_prefix(p) {
                let g = l[l.len() - rest.len()..].trim();
                if g.len() >= 3 {
                    return Some(g.to_string());
                }
            }
        }
        for p in ["automate ", "set up a task to ", "set up a workflow to ", "set up a task that ", "set up a routine to "] {
            if let Some(idx) = low.find(p) {
                let g = l[idx + p.len()..].trim().trim_end_matches(['.', '!']).trim();
                if g.len() >= 3 {
                    return Some(g.to_string());
                }
            }
        }
        None
    }

    /// "code: X" / "coder: X" / "write a script to X" / "build me a tool that X" → an agentic coding
    /// task for Claude Code (on MiniMax). Distinct from "run python: …" (that's the raw sandbox).
    pub(crate) fn parse_coder_request(text: &str) -> Option<String> {
        let l = text.trim();
        let low = l.to_lowercase();
        for p in ["code:", "coder:", "claude code:"] {
            if let Some(rest) = low.strip_prefix(p) {
                let task = l[l.len() - rest.len()..].trim();
                if !task.is_empty() {
                    return Some(task.to_string());
                }
            }
        }
        let triggers = [
            "write code to", "write a script", "write me a script", "write a program",
            "build me a script", "build me a program", "build a script", "build a program",
            "build a tool", "build me a tool", "code me a", "make a script", "make a program",
        ];
        if triggers.iter().any(|t| low.contains(t)) {
            return Some(l.to_string());
        }
        None
    }

    /// Extract the first ```fenced``` block → (info-string lowercased, code).
    pub(crate) fn fenced_code(text: &str) -> Option<(String, String)> {
        let start = text.find("```")?;
        let after = &text[start + 3..];
        let nl = after.find('\n')?;
        let info = after[..nl].trim().to_lowercase();
        let rest = &after[nl + 1..];
        let end = rest.find("```")?;
        Some((info, rest[..end].to_string()))
    }

    /// Parse a "run/execute … <lang> … <code>" request → (language, code). Requires an explicit run
    /// intent AND a determinable language (never guesses), so ordinary code chat isn't executed.
    pub(crate) fn parse_code_request(text: &str) -> Option<(CodeLang, String)> {
        let l = text.to_lowercase();
        if !["run ", "execute ", "exec ", "eval "].iter().any(|p| l.contains(p)) {
            return None;
        }
        let fence = Self::fenced_code(text);
        let kw_lang = if l.contains("rust") {
            Some(CodeLang::Rust)
        } else if l.contains("python") || l.contains(" py") {
            Some(CodeLang::Python)
        } else if l.contains("shell") || l.contains("bash") || l.contains("command") {
            Some(CodeLang::Shell)
        } else {
            None
        };
        let fence_lang = fence.as_ref().and_then(|(info, _)| match info.as_str() {
            "rust" | "rs" => Some(CodeLang::Rust),
            "python" | "py" => Some(CodeLang::Python),
            "sh" | "bash" | "shell" => Some(CodeLang::Shell),
            _ => None,
        });
        let lang = kw_lang.or(fence_lang)?;
        let code = match fence {
            Some((_, c)) => c,
            None => {
                let idx = text.find(':')?;
                text[idx + 1..].trim().to_string()
            }
        };
        if code.trim().is_empty() {
            return None;
        }
        Some((lang, code))
    }

    /// The skill loop: save a green run as a reusable skill, run a saved skill (always in the
    /// sandbox), or list skills. Returns Some(reply) if handled. Requires the sandbox (reuse runs
    /// code; banking without a runner would be pointless).
    pub(crate) async fn handle_skills(&self, user_text: &str) -> Option<String> {
        let sb = self.sandbox.as_ref()?;

        if Self::wants_list_skills(user_text) {
            let skills = self.memory.list_skills().await.unwrap_or_default();
            if skills.is_empty() {
                return Some("No skills banked yet. Run some code, then say \"save that as skill <name>\".".into());
            }
            let body = skills
                .iter()
                .map(|s| format!(
                    "- {} [{}] — {} ({}/{} ok{})",
                    s.name, s.lang, s.summary, s.successes, s.runs,
                    if s.status == "quarantined" { ", QUARANTINED" } else { "" }
                ))
                .collect::<Vec<_>>()
                .join("\n");
            return Some(format!("Skills ({}):\n{body}", skills.len()));
        }

        // Skill SEARCH: find banked skills relevant to a task.
        if let Some(query) = Self::parse_find_skill(user_text) {
            let hits = self.memory.recall_skills(&query, 5).await.unwrap_or_default();
            if hits.is_empty() {
                return Some(format!(
                    "No skill matches \"{query}\" yet. Run code (e.g. \"run python: …\"), then \"save that as skill <name>\" to bank one."
                ));
            }
            let body = hits
                .iter()
                .map(|s| format!("- {} [{}] — {} ({}/{} ok) → \"run skill {}\"", s.name, s.lang, s.summary, s.successes, s.runs, s.name))
                .collect::<Vec<_>>()
                .join("\n");
            return Some(format!("Skills matching \"{query}\":\n{body}"));
        }

        if let Some(name) = Self::parse_save_skill(user_text) {
            let last = self.last_run.lock().unwrap().clone();
            let (lang, code) = match last {
                Some(lc) => lc,
                None => return Some("Run something green first (e.g. \"run python: …\"), then I'll save it as a skill.".into()),
            };
            // Verifier-generated summary for recall (not author prose).
            let summary = self
                .inference
                .chat(
                    vec![
                        ChatMessage::system(&self.persona),
                        ChatMessage::user(&format!(
                            "In ONE terse sentence, say what this {} code does — for a tool catalog, no preamble:\n\n{code}",
                            Self::lang_str(lang)
                        )),
                    ],
                    GenerationConfig::default(),
                )
                .await
                .map(|r| r.text.trim().to_string())
                .unwrap_or_else(|_| format!("{} skill", Self::lang_str(lang)));
            let skill = Skill {
                name: name.clone(),
                lang: Self::lang_str(lang).into(),
                code,
                summary: summary.clone(),
                tags: vec![],
                status: "candidate".into(),
                runs: 0,
                successes: 0,
                created_ms: Self::now_ms(),
            };
            return Some(match self.memory.save_skill(skill).await {
                Ok(()) => format!("Saved skill \"{name}\": {summary}\nRun it anytime with \"run skill {name}\" (always sandboxed)."),
                Err(e) => format!("Couldn't save that skill — {e}."),
            });
        }

        if let Some(name) = Self::parse_run_skill(user_text) {
            let skill = match self.memory.get_skill(&name).await.ok().flatten() {
                Some(s) => s,
                None => {
                    let hits = self.memory.recall_skills(&name, 3).await.unwrap_or_default();
                    let hint = if hits.is_empty() {
                        String::new()
                    } else {
                        format!(" Did you mean: {}?", hits.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", "))
                    };
                    return Some(format!("No skill named \"{name}\".{hint}"));
                }
            };
            let res = match Self::lang_from_str(&skill.lang) {
                CodeLang::Python => sb.run_python(&skill.code).await,
                CodeLang::Shell => sb.run_shell(&skill.code).await,
                CodeLang::Rust => sb.run_rust(&skill.code).await,
            };
            return Some(match res {
                Ok(r) => {
                    let ok = r.exit_code == 0 && !r.timed_out;
                    let _ = self.memory.record_skill_outcome(&name, ok).await;
                    format!("Ran skill \"{name}\" (prior {}/{} ok):\n\n{}", skill.successes, skill.runs, r.render())
                }
                Err(e) => format!("Couldn't run skill \"{name}\" — sandbox unavailable ({e})."),
            });
        }
        None
    }

    /// Conservative recall router: if a banked skill strongly matches the task, return a one-line
    /// suggestion to run it. Requires the sandbox (skills run there) + a multi-word topical match so
    /// it doesn't fire on greetings or weak overlaps. Suggests — never auto-runs (the brainstorm rule).
    pub(crate) async fn suggest_skill(&self, user_text: &str) -> Option<String> {
        self.sandbox.as_ref()?;
        if user_text.split_whitespace().count() < 3 {
            return None;
        }
        let top = self.memory.recall_skills(user_text, 1).await.ok()?.into_iter().next()?;
        let hay = format!("{} {} {}", top.name, top.summary, top.tags.join(" ")).to_lowercase();
        let q = user_text.to_lowercase();
        let matches = q.split_whitespace().filter(|w| w.len() >= 4).filter(|w| hay.contains(*w)).count();
        if matches >= 2 {
            Some(format!(
                "\n\n_(I have a skill \"{}\" that may fit — say \"run skill {}\" to use it.)_",
                top.name, top.name
            ))
        } else {
            None
        }
    }

}
