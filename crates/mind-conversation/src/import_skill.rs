//! Import an AGENT from a document — a SKILL.md-style file becomes a live capability.
//!
//! The agentskills.io world ships knowledge as markdown folders; Pranab's ask (2026-08-09) was to
//! bring those INTO the mind: drop a file, pick whether it is a one-time run or a standing order.
//! An imported agent is DATA, not code (the capability thesis): the instructions become a skill in
//! the bank (semantic-routed, runnable via `run_skill`), and a `schedule:` line additionally
//! registers a standing recipe whose Think step IS the instructions — built deterministically,
//! no LLM planning between the file and the cadence.
//!
//! Format tolerance: markdown/plain text pass through; RTF is best-effort stripped of control
//! words (good enough for prose exported from Word — a garbled import shows itself immediately in
//! the preview line, and the fix is "save as .md").

use super::*;

pub(crate) struct ImportedAgent {
    pub name: String,
    pub description: String,
    /// "once" (skill only) | ("daily", 0, h, m) | ("weekly", wd, h, m)
    pub schedule: Option<(String, u8, u8, u8)>,
    pub instructions: String,
}

/// Strip RTF control words/groups to recover the prose. Naive by design: `{\rtf…}` documents from
/// word processors carry text between control sequences; we drop `\command` tokens and braces.
pub(crate) fn rtf_to_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len() / 2);
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' | '}' => {}
            '\\' => {
                // consume the control word (letters) + optional numeric parameter + one space
                while let Some(&n) = chars.peek() {
                    if n.is_ascii_alphanumeric() || n == '-' {
                        chars.next();
                    } else {
                        if n == ' ' {
                            chars.next();
                        }
                        break;
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out.trim().to_string()
}

/// Parse a skill document: optional `--- … ---` frontmatter (name / description / schedule),
/// else first `# heading` as the name and first paragraph as the description.
pub(crate) fn parse_agent_doc(raw: &str) -> Option<ImportedAgent> {
    let text = if raw.trim_start().starts_with("{\\rtf") { rtf_to_text(raw) } else { raw.to_string() };
    let text = text.trim();
    if text.len() < 20 {
        return None;
    }
    let (mut name, mut description, mut schedule) = (String::new(), String::new(), None);
    let body;
    if let Some(rest) = text.strip_prefix("---") {
        let (fm, after) = rest.split_once("---")?;
        for line in fm.lines() {
            let Some((k, v)) = line.split_once(':') else { continue };
            let v = v.trim().trim_matches(|c| c == '"' || c == '\'' || c == '>');
            match k.trim() {
                "name" => name = v.to_string(),
                "description" => description = v.to_string(),
                "schedule" => schedule = parse_schedule_line(v),
                _ => {}
            }
        }
        body = after.trim().to_string();
    } else {
        body = text.to_string();
    }
    if name.is_empty() {
        name = body
            .lines()
            .find(|l| l.starts_with('#'))
            .map(|l| l.trim_start_matches('#').trim().to_string())
            .unwrap_or_else(|| body.split_whitespace().take(4).collect::<Vec<_>>().join(" "));
    }
    if description.is_empty() {
        description = body
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .unwrap_or("")
            .chars()
            .take(200)
            .collect();
    }
    let name: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if name.is_empty() {
        return None;
    }
    Some(ImportedAgent { name, description, schedule, instructions: body })
}

/// "weekly mon 09:00" | "daily 07:30" | "once"/"" → the cadence tuple. Deterministic, tiny — the
/// same rule as `ym schedule`, never the model's guess.
pub(crate) fn parse_schedule_line(v: &str) -> Option<(String, u8, u8, u8)> {
    let t: Vec<&str> = v.split_whitespace().collect();
    match t.first().copied()? {
        "once" => None,
        "daily" => {
            let (h, m) = t.get(1)?.split_once(':')?;
            Some(("daily".into(), 0, h.parse().ok()?, m.parse().ok()?))
        }
        "weekly" => {
            let wd = match t.get(1).copied()? {
                "mon" => 0u8, "tue" => 1, "wed" => 2, "thu" => 3, "fri" => 4, "sat" => 5, "sun" => 6,
                _ => return None,
            };
            let (h, m) = t.get(2)?.split_once(':')?;
            Some(("weekly".into(), wd, h.parse().ok()?, m.parse().ok()?))
        }
        _ => None,
    }
}

/// The PROMPT an instruction document runs as, composed in ONE place.
///
/// Three executors now compose it: the standing schedule, the bare recipe, and the researcher.
/// E.SK1 factored the recipe because two copies of it would drift; the prompt is the part that
/// actually has to match, because it is what the model reads (E.SK3).
pub(crate) fn instruction_prompt(instructions: &str, input: Option<&str>) -> String {
    let mut prompt = format!(
        "Follow these standing instructions exactly and produce the deliverable they describe:\n\n{instructions}"
    );
    if let Some(input) = input.map(str::trim).filter(|i| !i.is_empty()) {
        prompt.push_str(&format!("\n\nInput for this run: {input}"));
    }
    prompt
}

/// The steps an instruction DOCUMENT runs as: follow the instructions, then deliver the result.
///
/// ONE construction, used by both callers — `import_agent` for a standing schedule and `run_skill`
/// for an on-call run. Two copies would drift, and the scheduled path is the one nobody watches
/// (E.SK1).
///
/// `input` is the run's argument woven in where the instructions can see it. A standing order has
/// none; an on-call run usually does (`run market-check: WMT`).
pub(crate) fn instruction_steps(name: &str, instructions: &str, input: Option<&str>) -> Vec<RecipeStep> {
    instruction_steps_from_prompt(name, instruction_prompt(instructions, input))
}

/// The same steps from a prompt that is ALREADY composed.
///
/// The fallback executor has the prompt in hand and must not send it back through
/// `instruction_prompt`, which would stack a second "Follow these standing instructions" preamble
/// on top of the first.
pub(crate) fn instruction_steps_from_prompt(name: &str, prompt: String) -> Vec<RecipeStep> {
    vec![
        RecipeStep::Think { prompt, store_as: "result".into(), on_error: ErrorAction::Fail, max_tokens: None, think: None },
        RecipeStep::Notify { message: format!("📥 [{name}] {{{{result}}}}") },
    ]
}

impl super::ConversationEngine {
    /// `ym import <document>` — the whole file rides as the verb's argument. Returns a receipt
    /// naming what was created and how to run/cancel it.
    pub async fn import_agent(&self, doc: &str) -> String {
        let Some(agent) = parse_agent_doc(doc) else {
            return "I couldn't read that as an agent document — I need at least a heading or frontmatter (name/description, optional `schedule: weekly mon 09:00`) and some instructions.".to_string();
        };
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let skill = mind_types::Skill {
            name: agent.name.clone(),
            lang: "md".into(),
            code: agent.instructions.clone(),
            summary: agent.description.clone(),
            tags: vec!["imported".into()],
            status: "active".into(),
            runs: 0,
            successes: 0,
            graded: 0,
            judged_ok: 0,
            created_ms: now,
        };
        if let Err(e) = self.memory.save_skill(skill).await {
            return format!("(couldn't bank the skill: {e})");
        }
        let mut receipt = format!("📥 Imported agent \"{}\" — {}\n   It's in the skill bank: `run {}` or just ask for it by what it does.", agent.name, agent.description, agent.name);
        if let Some((every, wd, h, m)) = agent.schedule {
            if let Some(recipes) = &self.recipes {
                let rec = Recipe {
                    id: format!("import:{}", agent.name),
                    name: format!("standing: {}", agent.name),
                    steps: vec![
                        RecipeStep::Schedule { every: every.clone(), weekday: wd, hour: h, minute: m },
                    ]
                    .into_iter()
                    .chain(instruction_steps(&agent.name, &agent.instructions, None))
                    .collect(),
                };
                let out = recipes.run_with(&rec, std::collections::HashMap::new()).await;
                receipt.push_str(&match out.sleeping_until {
                    Some(_) => format!(
                        "\n   Standing order armed: {every}{} at {h:02}:{m:02} — `ym orders` shows it.",
                        if every == "weekly" { format!(" {}", ["mon","tue","wed","thu","fri","sat","sun"][wd as usize]) } else { String::new() }
                    ),
                    None => format!("\n   (couldn't arm the schedule: {})", out.error.unwrap_or_else(|| "unknown".into())),
                });
            }
        }
        receipt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_doc_imports_with_schedule() {
        let doc = "---\nname: News Digest\ndescription: morning tech digest\nschedule: daily 08:00\n---\n# News Digest\nGather top stories and summarize.";
        let a = parse_agent_doc(doc).expect("parses");
        assert_eq!(a.name, "news-digest");
        assert_eq!(a.schedule, Some(("daily".into(), 0, 8, 0)));
        assert!(a.instructions.contains("Gather top stories"));
    }

    #[test]
    fn bare_markdown_derives_name_and_description_one_time() {
        let a = parse_agent_doc("# Trip Checker\nChecks passports and visas before any trip.\n\nSteps: …").expect("parses");
        assert_eq!(a.name, "trip-checker");
        assert!(a.description.contains("passports"));
        assert!(a.schedule.is_none(), "no schedule line = one-time skill only");
    }

    #[test]
    fn rtf_prose_survives_the_strip() {
        let rtf = r"{\rtf1\ansi{\fonttbl\f0 Arial;}\f0\fs24 # Meal Planner\par Plan dinners for the week.}";
        let a = parse_agent_doc(rtf).expect("rtf imports");
        assert!(a.instructions.contains("Plan dinners"), "{}", a.instructions);
    }

    #[test]
    fn schedule_line_is_deterministic_and_rejects_junk() {
        assert_eq!(parse_schedule_line("weekly sat 10:30"), Some(("weekly".into(), 5, 10, 30)));
        assert_eq!(parse_schedule_line("once"), None);
        assert_eq!(parse_schedule_line("hourly 5"), None);
        assert_eq!(parse_schedule_line("weekly monday 09:00"), None, "only mon..sun tokens");
    }
}
