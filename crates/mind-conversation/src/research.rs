//! Research/recipe -- research intents, deep research, drafting, recipe ops. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// Turn a recipe RunOutcome into a chat reply, parking any pause (question or action) so the
    /// next message resumes it.
    pub(crate) fn handle_recipe_outcome(&self, out: mind_recipes::RunOutcome) -> String {
        if let Some(pq) = out.pending_question {
            *self.pending_question.lock().unwrap() = Some(pq.run_id);
            return pq.question;
        }
        if let Some(req) = out.pending_action {
            let body = req.intent.payload.clone().unwrap_or_default();
            let to = req.intent.target.clone();
            let subject = req.intent.summary.clone();
            *self.pending.lock().unwrap() = Some(req);
            return format!("Drafted this email — reply \"yes\" to send:\n\nTo: {to}\nSubject: {subject}\n\n{body}");
        }
        if out.sleeping_until.is_some() {
            return "Set up — it'll run in the background and I'll message you when it does.".into();
        }
        if let Some(e) = out.error {
            return format!("That didn't work: {e}");
        }
        if !out.notifications.is_empty() {
            return out.notifications.join("\n");
        }
        "Done.".into()
    }

    /// "research X" / "look into X" / "investigate X" → (topic). None if not a research ask.
    pub(crate) fn wants_research(text: &str) -> Option<String> {
        let l = text.trim().to_lowercase();
        for p in ["research ", "look into ", "investigate ", "dig into ", "find out about ", "look up "] {
            if let Some(idx) = l.find(p) {
                let topic = text[idx + p.len()..].trim().trim_end_matches(['.', '?', '!']).trim();
                if topic.len() >= 2 {
                    return Some(topic.to_string());
                }
            }
        }
        None
    }

    /// "research and update X" / "update your knowledge on X" → (topic). The research→belief-revision
    /// path: live findings reconcile against + revise prior typed beliefs. Checked FIRST.
    pub(crate) fn wants_research_revise(text: &str) -> Option<String> {
        let l = text.trim().to_lowercase();
        for p in [
            "research and update ", "research and revise ", "update your knowledge on ",
            "update your beliefs on ", "refresh your knowledge on ", "fact-check and update ",
        ] {
            if let Some(idx) = l.find(p) {
                let topic = text[idx + p.len()..].trim().trim_end_matches(['.', '?', '!']).trim();
                if topic.len() >= 2 {
                    return Some(topic.to_string());
                }
            }
        }
        None
    }

    /// "deep dive on X" / "deep research X" / "thoroughly research X" → (topic). Checked BEFORE the
    /// single-agent research so the deeper, parallel path wins.
    pub(crate) fn wants_deep_research(text: &str) -> Option<String> {
        let l = text.trim().to_lowercase();
        for p in ["deep dive on ", "deep dive into ", "deep-dive on ", "deep dive ", "deep research ",
                  "thoroughly research ", "comprehensive research on ", "thorough research on "] {
            if let Some(idx) = l.find(p) {
                let topic = text[idx + p.len()..].trim().trim_start_matches("on ").trim_start_matches("into ").trim();
                let topic = topic.trim_end_matches(['.', '?', '!']).trim();
                if topic.len() >= 2 {
                    return Some(topic.to_string());
                }
            }
        }
        None
    }

    /// Parse a ResearchOps ask -> (mode, subject). mode ∈ review|related|next. Explicit verbs only,
    /// so ordinary "research X" (deep dive) and casual "review this" aren't hijacked.
    pub(crate) fn wants_researchops(text: &str) -> Option<(&'static str, String)> {
        let l = text.trim().to_lowercase();
        let after = |t: &str, pats: &[&str]| -> Option<String> {
            for p in pats {
                if let Some(i) = t.find(p) {
                    let rest = t[i + p.len()..].trim().trim_end_matches(['.', '?', '!']).trim();
                    // strip leading filler
                    let rest = rest.trim_start_matches("my ").trim_start_matches("the ").trim_start_matches("for ").trim();
                    if rest.len() >= 2 {
                        return Some(rest.to_string());
                    }
                }
            }
            None
        };
        if let Some(sub) = after(&l, &["research review ", "reviewer 2 on ", "reviewer-2 on ", "red team ", "red-team ", "poke holes in ", "critique my ", "review my paper on ", "review my "]) {
            return Some(("review", sub));
        }
        if let Some(sub) = after(&l, &["research related ", "related work for ", "related work on ", "related work ", "lit review ", "literature review "]) {
            return Some(("related", sub));
        }
        if let Some(sub) = after(&l, &["research next ", "next experiment for ", "next experiments for ", "next steps for my ", "what should i test next in ", "next moves for "]) {
            return Some(("next", sub));
        }
        None
    }

    /// Parse a document-drafting ask -> (kind, subject). Only fires with an explicit COMPOSE verb AND
    /// a document-kind noun (plan/memo/brief/...), so "write a script" (coder) and "write an email"
    /// (action) are not stolen. Subject is taken before the kind ("an SDF adoption plan" -> "SDF") or
    /// after a connector ("a plan about SDF" -> "SDF").
    pub(crate) fn wants_draft(text: &str) -> Option<(String, String)> {
        let l = text.trim().to_lowercase();
        let verbs = [
            "draft me ", "draft an ", "draft a ", "draft ", "write me ", "write up ", "write an ",
            "write a ", "compose ", "put together ", "prepare ",
        ];
        let verb = verbs.iter().find(|v| l.starts_with(**v))?;
        // Not a doc draft — these have their own dedicated paths.
        if ["script", "email", "code", "function", "collage", "poem", "song"].iter().any(|x| l.contains(x)) {
            return None;
        }
        // Longest kind phrases first so "adoption plan" wins over "plan".
        let kinds = [
            "adoption plan", "one-pager", "one pager", "action plan", "rollout plan", "proposal",
            "strategy", "overview", "write-up", "writeup", "summary", "outline", "memo", "brief",
            "pitch", "plan", "document", "doc",
        ];
        let kind = kinds.iter().find(|k| l.contains(**k))?.to_string();
        let rest = &text[verb.len()..];
        let rl = rest.to_lowercase();
        let kpos = rl.find(&kind)?;
        // Subject BEFORE the kind: "an SDF adoption plan" -> "SDF".
        let before = rest[..kpos]
            .trim()
            .trim_start_matches("the ")
            .trim_start_matches("an ")
            .trim_start_matches("a ")
            .trim();
        if before.len() >= 2 {
            return Some((kind, before.to_string()));
        }
        // Subject AFTER the kind via a connector: "a plan about SDF" -> "SDF".
        let after = &rest[kpos + kind.len()..];
        let al = after.to_lowercase();
        for c in [" about ", " on ", " for ", " regarding ", " covering ", " of "] {
            if let Some(i) = al.find(c) {
                let subj = after[i + c.len()..].trim().trim_end_matches(['.', '?', '!']).trim();
                if subj.len() >= 2 {
                    return Some((kind, subj.to_string()));
                }
            }
        }
        None
    }

    /// Deep research: split the topic into sub-questions, run a sub-agent on each IN PARALLEL
    /// (fan-out), then synthesize. The visible payoff of the sub-agent + concurrency work.
    /// RESEARCH → BELIEF REVISION (the moat's signature move). Recall what we already believe near the
    /// topic, research it live (cited), reconcile findings vs priors, then ASSERT new facts AND REVISE
    /// contradicted priors — negative evidence weakens the stale belief (Bayesian), the corrected one
    /// is asserted (research-backed), and a contradiction edge is drawn. Every research run permanently
    /// updates the typed model; flat-RAG companions can't do this.
    pub async fn research_revise(&self, topic: &str) -> Result<String> {
        if !Self::treasury_try_draw("research") {
            return Ok("Research envelope is dry today — deferred to tomorrow (`ym budget`).".into());
        }
        let agent = match &self.researcher {
            Some(a) => a,
            None => return Ok("(no researcher configured)".into()),
        };
        // 1. what we already believe near this topic
        let priors = self
            .memory
            .recall_typed(mind_types::RecallQuery { text: topic.to_string(), top_k: 6, kind: None }, &mind_types::AccessContext::Operator)
            .await
            .unwrap_or_default();
        let prior_list = if priors.is_empty() {
            "(no prior beliefs on this)".to_string()
        } else {
            priors.iter().map(|r| format!("- {} (confidence {:.2})", r.item.text, r.item.confidence)).collect::<Vec<_>>().join("\n")
        };
        // 2. research live (cited)
        let res = agent.run(topic).await;
        // 3. reconcile priors vs findings
        let prompt = format!(
            "PRIOR BELIEFS:\n{prior_list}\n\nLIVE RESEARCH FINDINGS:\n{}\n\n\
             Reconcile the priors with the findings. Output ONLY JSON:\n\
             {{\"facts\":[{{\"statement\":\"...\",\"certainty\":0.0-1.0}}], \
             \"revisions\":[{{\"old\":\"<copy a prior belief above that is now contradicted/outdated>\",\"new\":\"<corrected third-person statement>\",\"certainty\":0.0-1.0}}]}}\n\
             A REVISION is when a specific prior belief is now wrong/outdated (copy its text verbatim into \"old\"). FACTS are genuinely new third-person statements. Empty arrays if none.",
            res.answer
        );
        let messages = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::system("You reconcile prior beliefs with fresh research. Output ONLY the JSON object."),
            ChatMessage::user(&prompt),
        ];
        let text = self.inference.chat(messages, GenerationConfig::default()).await.map_err(|e| MindError::Inference(e.to_string()))?.text;
        let b = text.rsplit("</think>").next().unwrap_or(&text);
        let b = b.split("```").find(|s| s.contains('{')).unwrap_or(b);
        let obj = match (b.find('{'), b.rfind('}')) {
            (Some(s), Some(e)) if e > s => &b[s..=e],
            _ => "{}",
        };
        let v: serde_json::Value = serde_json::from_str(obj).unwrap_or(serde_json::json!({}));

        let mut report: Vec<String> = Vec::new();
        for f in v.get("facts").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let stmt = f.get("statement").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if stmt.len() < 6 {
                continue;
            }
            let cert = f.get("certainty").and_then(|x| x.as_f64()).unwrap_or(0.7).clamp(0.1, 0.95);
            if self
                .memory
                .remember_as_belief(BeliefAssertion { statement: stmt.clone(), polarity: 1.0, weight: 0.5 + cert * 1.5, source_event: Some("research".into()), provenance: "extracted".into() })
                .await
                .is_ok()
            {
                report.push(format!("📚 learned: {stmt}"));
            }
        }
        for r in v.get("revisions").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let old = r.get("old").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let new = r.get("new").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if old.len() < 6 || new.len() < 6 {
                continue;
            }
            let cert = r.get("certainty").and_then(|x| x.as_f64()).unwrap_or(0.8).clamp(0.1, 0.95);
            let w = 0.5 + cert * 1.5;
            // corrected belief (research-backed) + negative evidence weakening the stale one + a
            // contradiction edge — a real Bayesian revision with an evidence trail.
            let _ = self.memory.remember_as_belief(BeliefAssertion { statement: new.clone(), polarity: 1.0, weight: w, source_event: Some("research".into()), provenance: "extracted".into() }).await;
            let _ = self.memory.remember_as_belief(BeliefAssertion { statement: old.clone(), polarity: -1.0, weight: w, source_event: Some("research".into()), provenance: "extracted".into() }).await;
            let _ = self.memory.relate(&new, &old, "contradicts", 0.9).await;
            report.push(format!("🔄 revised: \"{old}\" → \"{new}\""));
        }

        let mut out = if report.is_empty() {
            format!("Researched \"{topic}\" — nothing changed in what I believe.")
        } else {
            format!("Researched \"{topic}\" and updated my memory:\n{}", report.join("\n"))
        };
        if !res.sources.is_empty() {
            out.push_str(&format!("\n\nSources:\n{}", res.sources.iter().take(6).map(|s| format!("• {s}")).collect::<Vec<_>>().join("\n")));
        }
        Ok(out)
    }

    pub(crate) async fn deep_research(&self, topic: &str) -> Result<String> {
        if !Self::treasury_try_draw("research") {
            return Ok("Research envelope is dry today — deferred to tomorrow (`ym budget`).".into());
        }
        let agent = match &self.researcher {
            Some(a) => a,
            None => return Ok("(no researcher configured)".into()),
        };
        // 1. Split into focused sub-questions.
        let split = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::user(&format!(
                "Break this into 3 focused, non-overlapping sub-questions to investigate. \
                 One per line, no numbering, no preamble.\nTopic: {topic}"
            )),
        ];
        let subs = self
            .inference
            .chat(split, GenerationConfig::default())
            .await
            .map_err(|e| MindError::Inference(e.to_string()))?;
        let mut tasks: Vec<String> = subs
            .text
            .lines()
            .map(|l| l.trim().trim_start_matches(['-', '*', '•', ' ']).trim().to_string())
            .filter(|l| l.len() > 3)
            .take(4)
            .collect();
        if tasks.is_empty() {
            tasks.push(topic.to_string());
        }
        // 2. Fan out — sub-agents run concurrently.
        let results = agent.fan_out(tasks).await;
        let findings = results
            .iter()
            .map(|r| format!("Q: {}\nA: {}", r.task, r.answer))
            .collect::<Vec<_>>()
            .join("\n\n");
        // Collect + dedupe source URLs across all the sub-agents (citations).
        let mut sources: Vec<String> = Vec::new();
        for r in &results {
            for u in &r.sources {
                if !sources.iter().any(|s| s == u) {
                    sources.push(u.clone());
                }
            }
        }
        // 3. Synthesize, grounded only in the findings.
        let synth = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::user(&format!(
                "Synthesize one coherent answer to '{topic}' from these parallel findings. Ground ONLY \
                 in them; note any gaps.\n\n{findings}"
            )),
        ];
        let draft = self
            .inference
            .chat(synth, GenerationConfig::default())
            .await
            .map_err(|e| MindError::Inference(e.to_string()))?
            .text;
        // 4. Adversarial verify: check the draft's claims against the findings (anti-confabulation).
        let verify = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::user(&format!(
                "You are a skeptical fact-checker. Below is a DRAFT answer and the FINDINGS it should \
                 rest on. List any claim in the draft NOT supported by the findings, one per line as \
                 '⚠ <claim>'. If every claim is supported, reply exactly 'All claims grounded.'\n\n\
                 DRAFT:\n{draft}\n\nFINDINGS:\n{findings}"
            )),
        ];
        let verdict = self
            .inference
            .chat(verify, GenerationConfig::default())
            .await
            .map(|r| r.text.trim().to_string())
            .unwrap_or_else(|_| "(verification unavailable)".into());

        let mut out = draft;
        if !sources.is_empty() {
            out.push_str("\n\n**Sources:**\n");
            for u in sources.iter().take(8) {
                out.push_str(&format!("- {u}\n"));
            }
        }
        out.push_str(&format!("\n**Verification:** {verdict}"));
        out.push_str(&format!("\n\n_(deep-dived {} angles in parallel, fact-checked)_", results.len()));
        Ok(out)
    }

    /// HARD-GROUNDED DRAFTING (the Family Book's discipline, applied to work). Gather the COMPLETE
    /// set of stored facts about the subject (deterministic enumerate + word-match — no embedding
    /// ranking lottery, no 3-fact cap), compose STRICTLY from them, then adversarially fact-check the
    /// draft against the same facts. Thin corpus -> an honest short draft with gap markers, never a
    /// confident fabrication (the SDF-adoption-plan value-prop blend was drafting freely, not from facts).
    pub(crate) async fn draft_grounded(&self, kind: &str, subject: &str, ctx: &mind_types::AccessContext) -> Result<String> {
        let facts = self.memory.beliefs_matching(subject, ctx).await.unwrap_or_default();
        if facts.is_empty() {
            return Ok(format!(
                "I don't hold any stored grounding on \"{subject}\" yet, so I won't invent a {kind}. \
                 Point me at a source — `ym learn <url>` — or tell me the key facts, and I'll draft it strictly from those."
            ));
        }
        let fact_block = facts
            .iter()
            .map(|b| format!("- {} (certainty {:.2})", b.statement, b.confidence))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "You are drafting a {kind} about \"{subject}\" for Pranab.\n\n\
             FACTS — the ONLY things you know about {subject}; every claim in the {kind} must trace to one of these:\n{fact_block}\n\n\
             Write the {kind}. HARD RULES: use ONLY the facts above; never blend in adjacent or general \
             knowledge, and never invent capabilities, numbers, names, dates, or value-propositions not \
             stated in the facts. Where the {kind} needs something the facts don't cover, write \
             \"[no grounding: <what's missing>]\" on its own line — do NOT guess to fill the gap. A short, \
             honest {kind} grounded in real facts beats a long confident one that fabricates. Structure it \
             well; content comes only from the facts."
        );
        let draft = self
            .inference
            .chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], GenerationConfig::default())
            .await
            .map_err(|e| MindError::Inference(e.to_string()))?
            .text;
        // Adversarial grounding check — every claim must trace to a fact; gap markers are honest, not violations.
        let verify = format!(
            "You are a skeptical fact-checker. Below is a DRAFT {kind} and the FACTS it must rest on. \
             List any claim in the draft NOT supported by the facts, one per line as '\u{26a0} <claim>'. \
             Ignore any \"[no grounding: ...]\" markers — those are honest gaps, not violations. \
             If every claim is supported, reply exactly 'All claims grounded.'\n\nDRAFT:\n{draft}\n\nFACTS:\n{fact_block}"
        );
        let verdict = self
            .inference
            .chat(vec![ChatMessage::system(&self.persona), ChatMessage::user(&verify)], GenerationConfig::default())
            .await
            .map(|r| r.text.trim().to_string())
            .unwrap_or_else(|_| "(grounding check unavailable)".into());
        let note = if facts.len() < 4 {
            format!(
                "\n\n_(Grounded in only {} stored fact(s) about {subject} — thin. `ym learn <url>` on a source and I'll deepen it.)_",
                facts.len()
            )
        } else {
            format!("\n\n_(Grounded strictly in {} stored facts about {subject}.)_", facts.len())
        };
        Ok(format!("{draft}\n\n**Grounding check:** {verdict}{note}"))
    }

    /// Recover any recipe runs left mid-flight by a previous crash (idempotent steps re-run; a
    /// non-idempotent send is failed-visibly). Returns how many were resumed.
    pub async fn resume_recipes(&self) -> usize {
        match &self.recipes {
            Some(re) => re.resume_incomplete().await,
            None => 0,
        }
    }

    /// Reviewer recipe v2: full-text-grounded referee with optional paper URL and prior-review
    /// diff. "Referee" vocabulary throughout — precise, and free of security-flavored phrasing.
    pub(crate) fn reviewer_recipe(subject: &str, paper_url: Option<&str>, prior: Option<&str>) -> mind_recipes::Recipe {
        let notify = format!(
            "🔬 Referee report — {subject}\nEvery objection below is grounded in the literature (full-text), your own claims, your real code{} — ungrounded critique was stripped ({{{{grounded__dropped}}}} dropped):\n\n{{{{out}}}}",
            if paper_url.is_some() { ", or the submitted draft" } else { "" }
        );
        let mut steps = vec![
            RecipeStep::Tool { tool_name: "research".into(), args: serde_json::json!({"query": format!("{subject} method novelty evaluation prior art")}), store_as: "lit".into(), on_error: ErrorAction::Skip },
            RecipeStep::Tool { tool_name: "recall".into(), args: serde_json::json!({"query": subject}), store_as: "mine".into(), on_error: ErrorAction::Skip },
            RecipeStep::Tool { tool_name: "code_digest".into(), args: serde_json::json!({"subject": subject}), store_as: "code".into(), on_error: ErrorAction::Skip },
        ];
        let mut source_vars: Vec<String> = vec!["lit".into(), "mine".into(), "code".into()];
        if let Some(url) = paper_url {
            steps.push(RecipeStep::Tool { tool_name: "fetch".into(), args: serde_json::json!({"url": url}), store_as: "paper".into(), on_error: ErrorAction::Skip });
            source_vars.push("paper".into());
        }
        let prior_clause = match prior {
            Some(_) => {
                source_vars.push("prior".into());
                " A PRIOR referee report exists (source `prior`): for each prior objection, judge whether the current evidence shows it ADDRESSED (say so in one line, cite what addressed it) or STILL STANDING (restate it, updated). New objections come after the diff."
            }
            None => "",
        };
        steps.push(RecipeStep::ThinkCited {
            prompt: format!(
                "You are a rigorous, fair journal referee reviewing work on \"{subject}\" — precise, skeptical, never flattering. Using ONLY the sources, produce the strongest objections a top-venue review would raise: novelty/prior-art overlap, methodology or evaluation gaps, claims the evidence does not support, and threats to validity. RULES: each claim is ONE specific objection; start its text with a severity tag [FATAL] (kills the contribution), [MAJOR] (must fix before acceptance), or [MINOR]; end it with a one-clause fix hint ('fixable by …'); no two claims may make the same point; cite the grounding source for each (`lit` = literature full-text, `mine` = the author's own claims, `code` = the real repository{}). Rank [FATAL] first.{prior_clause} An objection you cannot ground in a source does not belong in the output.",
                if paper_url.is_some() { ", `paper` = the submitted draft" } else { "" }
            ),
            store_as: "review".into(),
            source_vars,
            on_error: ErrorAction::Fail,
        });
        steps.push(RecipeStep::Validate { input_var: "review".into(), store_as: "grounded".into() });
        steps.push(RecipeStep::Render { input_var: "grounded".into(), store_as: "out".into(), format: mind_recipes::RenderFormat::Cards });
        steps.push(RecipeStep::Notify { message: notify });
        mind_recipes::Recipe { id: "research-review".into(), name: format!("referee: {subject}"), steps }
    }

    pub(crate) fn related_recipe(subject: &str) -> mind_recipes::Recipe {
        let notify = format!("📚 Related work — {subject}:\n\n{{{{out}}}}");
        mind_recipes::Recipe {
            id: "research-related".into(),
            name: format!("related-work: {subject}"),
            steps: vec![
                RecipeStep::Tool { tool_name: "research".into(), args: serde_json::json!({"query": format!("{subject} related work key papers survey")}), store_as: "lit".into(), on_error: ErrorAction::Fail },
                RecipeStep::ThinkCited {
                    prompt: format!(
                        "Map the related work for \"{subject}\" from the sources. Each claim = one line of prior work: what it does and how \"{subject}\" differs from or builds on it, citing source `lit`. Write in the neutral third person of a paper's Related Work section — NEVER name the author, never use 'we/you', never reveal any assistant. Most-relevant first."
                    ),
                    store_as: "related".into(),
                    source_vars: vec!["lit".into()],
                    on_error: ErrorAction::Fail,
                },
                RecipeStep::Validate { input_var: "related".into(), store_as: "grounded".into() },
                RecipeStep::Render { input_var: "grounded".into(), store_as: "out".into(), format: mind_recipes::RenderFormat::Cards },
                RecipeStep::Notify { message: notify },
            ],
        }
    }

    pub(crate) fn next_recipe(subject: &str) -> mind_recipes::Recipe {
        let notify = format!("🧭 Next moves — {subject}:\n\n{{{{out}}}}");
        mind_recipes::Recipe {
            id: "research-next".into(),
            name: format!("next-experiments: {subject}"),
            steps: vec![
                RecipeStep::Tool { tool_name: "research".into(), args: serde_json::json!({"query": format!("{subject} open problems limitations future directions")}), store_as: "lit".into(), on_error: ErrorAction::Skip },
                RecipeStep::Tool { tool_name: "recall".into(), args: serde_json::json!({"query": subject}), store_as: "mine".into(), on_error: ErrorAction::Skip },
                RecipeStep::ThinkCited {
                    prompt: format!(
                        "Given the author's work on \"{subject}\" (source `mine`) and the field (source `lit`), propose the highest-value next experiments/directions. Each claim = one concrete move: what to test, why it matters now, and what result would prove or disprove it — citing the gap (`lit`) or the author's claim (`mine`) that motivates it. Prioritize by expected payoff."
                    ),
                    store_as: "next".into(),
                    source_vars: vec!["lit".into(), "mine".into()],
                    on_error: ErrorAction::Fail,
                },
                RecipeStep::Validate { input_var: "next".into(), store_as: "grounded".into() },
                RecipeStep::Render { input_var: "grounded".into(), store_as: "out".into(), format: mind_recipes::RenderFormat::Cards },
                RecipeStep::Notify { message: notify },
            ],
        }
    }

    /// Run a ResearchOps job detached; it posts the grounded result on completion.
    pub async fn research_ops_run(&self, mode: &str, subject: &str) -> String {
        let Some(recipes) = &self.recipes else {
            return "(research engine not wired — the recipe host is unavailable)".to_string();
        };
        let subject = subject.trim().to_string();
        if subject.len() < 2 {
            return "What should I research? e.g. `research review SDF Protocol`.".to_string();
        }
        if !Self::treasury_try_draw("research") {
            return "🔬 Research envelope is dry today (`ym treasury`) — the job will run tomorrow if you re-ask.".to_string();
        }
        // A URL anywhere in the subject = the actual draft/paper to referee (source `paper`).
        let paper_url: Option<String> = subject.split_whitespace().find(|w| w.starts_with("http")).map(String::from);
        let clean_subject = subject
            .split_whitespace()
            .filter(|w| !w.starts_with("http"))
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        let subject = if clean_subject.len() >= 2 { clean_subject } else { subject.clone() };
        // Review memory: a re-review diffs against the prior report (ADDRESSED vs STILL STANDING).
        let slug: String = subject.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
        let review_key = format!("research_{mode}_{}", slug.split('-').filter(|x| !x.is_empty()).take(5).collect::<Vec<_>>().join("-"));
        let prior: Option<String> = self.memory.profile_get(&review_key).await.ok().flatten().filter(|p| !p.trim().is_empty());
        let mut vars: std::collections::HashMap<String, serde_json::Value> = std::collections::HashMap::new();
        if let Some(p) = &prior {
            vars.insert("prior".into(), serde_json::Value::String(p.chars().take(3000).collect()));
        }
        let recipe = match mode {
            "review" => Self::reviewer_recipe(&subject, paper_url.as_deref(), prior.as_deref()),
            "related" => Self::related_recipe(&subject),
            "next" => Self::next_recipe(&subject),
            _ => return "Modes: review | related | next.".to_string(),
        };
        let recipes = recipes.clone();
        let nq = self.notify_queue.clone();
        let mem = self.memory.clone();
        tokio::spawn(async move {
            let out = recipes.run_with(&recipe, vars).await;
            if !out.notifications.is_empty() {
                // Persist the report — the next run of this subject diffs against it.
                if let Some(rendered) = out.vars.get("out").and_then(|v| v.as_str()) {
                    let stamped = format!("({})\n{}", chrono::Utc::now().format("%Y-%m-%d"), rendered);
                    let _ = mem.profile_set(&review_key, &stamped.chars().take(6000).collect::<String>()).await;
                }
                for n in out.notifications {
                    nq.lock().unwrap().push(n);
                }
            } else if let Some(e) = out.error {
                nq.lock().unwrap().push(format!("🔬 The research job couldn't finish: {e}"));
            } else {
                nq.lock().unwrap().push("🔬 The research job finished but nothing survived citation-validation — the sources didn't support a grounded answer.".to_string());
            }
        });
        let verb = match mode {
            "review" => "running Reviewer-2 on",
            "related" => "mapping the related work for",
            _ => "planning next experiments for",
        };
        format!("🔬 On it — {verb} **{subject}**: multi-angle research → grounded synthesis → citation-validated (ungrounded claims stripped). I'll post it here when it lands (a couple of minutes).")
    }

    pub(crate) async fn code_repos(&self) -> Vec<String> {
        self.memory
            .profile_get("code_repos")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn research_topics(&self) -> Vec<String> {
        let raw = self.memory.profile_get("research_topics").await.ok().flatten().unwrap_or_default();
        let t: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
        if !t.is_empty() {
            return t;
        }
        vec![
            "LLM agent persistent memory".into(),
            "belief revision language agents".into(),
            "self-improving code agents".into(),
            "retrieval augmented reasoning".into(),
        ]
    }

    /// THE AUTONOMOUS SCIENTIST — one bounded overnight research cycle, no human trigger:
    /// discover (arXiv, agenda round-robin) → study (distill + relate) → adapt against the studied
    /// yantrik-mind codebase → auto-adopt the top proposal into the self-build queue (only if the
    /// queue is short). Reports for the morning board. ~4 LLM calls, one paper per night.
    pub async fn night_research_run(&self) -> String {
        if std::env::var("YM_NIGHT_RESEARCH").map(|v| v == "off").unwrap_or(false) {
            return "🔭 night research is off (YM_NIGHT_RESEARCH=off)".into();
        }
        let topics = self.research_topics().await;
        let day = (chrono::Utc::now().timestamp() / 86_400) as usize;
        let topic = topics[day % topics.len()].clone();
        let seen_raw = self.memory.profile_get("research_seen").await.ok().flatten().unwrap_or_default();
        let mut seen: Vec<String> = serde_json::from_str(&seen_raw).unwrap_or_default();
        let idx = self.memory.profile_get("paper_index").await.ok().flatten().unwrap_or_default();
        let studied: std::collections::BTreeMap<String, String> = serde_json::from_str(&idx).unwrap_or_default();
        let topic2 = topic.clone();
        let found = match tokio::task::spawn_blocking(move || mind_tools::paper::arxiv_search(&topic2, 10)).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return format!("🔭 night research: arXiv unreachable tonight ({e}) — will hunt again tomorrow."),
            Err(_) => return "🔭 night research: discovery task panicked — will hunt again tomorrow.".into(),
        };
        // Relevance-rank: arXiv 'all:' matches loosely, so score by topic-word overlap in the
        // title+abstract and study the MOST RELEVANT unseen paper, not merely the newest.
        let topic_words: Vec<String> = topic.to_lowercase().split_whitespace()
            .filter(|w| w.len() >= 4).map(|w| w.to_string()).collect();
        let mut candidates: Vec<(usize, String, String)> = found.into_iter()
            .filter(|(url, _, _)| {
                let k = mind_tools::paper::paper_key(url);
                !studied.contains_key(&k) && !seen.contains(&k)
            })
            .map(|(url, title, summary)| {
                let hay = format!("{} {}", title.to_lowercase(), summary.to_lowercase());
                let score = topic_words.iter().filter(|w| hay.contains(w.as_str())).count();
                (score, url, title)
            })
            .collect();
        candidates.sort_by_key(|(sc, _, _)| std::cmp::Reverse(*sc));
        if candidates.is_empty() {
            return format!("🔭 night research: nothing new on \"{topic}\" tonight.");
        }
        // Try the most relevant candidate; if it distills to nothing (ar5iv stub, bad render),
        // try the runner-up before giving up the night.
        let mut chosen: Option<(String, String, usize)> = None;
        let mut tried: Vec<String> = Vec::new();
        for (_, url, title) in candidates.into_iter().take(2) {
            let key = mind_tools::paper::paper_key(&url);
            seen.push(key.clone());
            tried.push(title.chars().take(80).collect());
            let _ = self.paper_study(&url).await;
            for _ in 0..12 {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                let token = format!("paperkb{key}");
                if !self.memory.beliefs_matching_n(&token, 3, &mind_types::AccessContext::Operator).await.unwrap_or_default().is_empty() {
                    break;
                }
            }
            let token = format!("paperkb{key}");
            let n = self.memory.beliefs_matching_n(&token, 300, &mind_types::AccessContext::Operator).await.unwrap_or_default().len();
            if n > 0 {
                chosen = Some((key, title, n));
                break;
            }
        }
        if seen.len() > 300 { let cut = seen.len() - 300; seen.drain(..cut); }
        let _ = self.memory.profile_set("research_seen", &serde_json::to_string(&seen).unwrap_or_default()).await;
        let Some((key, title, n_facts)) = chosen else {
            return format!(
                "🔭 night research: tried {} paper(s) on \"{topic}\" but none distilled cleanly — will hunt again tomorrow. (tried: {})",
                tried.len(), tried.join(" · ")
            );
        };
        // ADAPT against the studied self-codebase; auto-adopt top proposal only when the human
        // queue is short (human goals keep priority) and the proposal is grounded (names a module).
        let _ = self.paper_adapt(&key, "yantrik-mind").await;
        let raw = self.memory.profile_get(&format!("paper_adapt_{key}")).await.ok().flatten().unwrap_or_default();
        let props: Vec<String> = serde_json::from_str(&raw).unwrap_or_default();
        let mut queued = String::new();
        if let Some(top) = props.first() {
            let grounded = top.contains("mind-") || top.contains("ConversationEngine") || top.contains("Memory");
            let goals_path = std::path::PathBuf::from(
                std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".into()),
            ).join("selfbuild-goals.txt");
            let qlen = std::fs::read_to_string(&goals_path)
                .map(|c| c.lines().filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#')).count())
                .unwrap_or(99);
            if grounded && qlen < 8 {
                use std::io::Write as _;
                if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(&goals_path) {
                    let _ = writeln!(fh, "{top}");
                    queued = format!("\n  → queued for self-build: {}", top.chars().take(160).collect::<String>());
                }
            } else if !grounded {
                queued = "\n  → top proposal too vague to auto-queue (kept for `paper adapt` review)".into();
            } else {
                queued = format!("\n  → self-build queue has {qlen} goals — not auto-adding (say `paper adopt {key} 1`)");
            }
        }
        format!(
            "🔭 night research on \"{topic}\": read **{}** ({n_facts} facts learned, `{key}`), derived {} adaptation(s){queued}",
            title.chars().take(110).collect::<String>(), props.len()
        )
    }

}
