//! Capability packs — installable, certifiable, self-authorable capability bundles.
//!
//! A pack is DATA, not native code (same honest scope as plugins.rs): a declared PluginSpec
//! (identity/security/catalog) + a set of skills (instruction documents banked in memory) +
//! EVALS. The evals are the certification gate: an installed pack starts DISABLED and is only
//! enabled when every eval passes; re-certifying after a regression demotes it back to disabled.
//! `ym pack draft <topic>` closes the self-extension loop: the mind assembles a pack from skills
//! it has already banked and proven reliable, stamped provenance=self_authored, and certifies it
//! through the same gate as anything imported.
//!
//! Security honesty: a v0 pack's tools execute as Think-only recipe steps — the model reasons over
//! the skill's instructions and returns TEXT. No tool calls, no side effects, no network beyond the
//! model itself. Declared security is floored at Personal so no pack wears a read-only badge it
//! didn't earn in code review.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::plugins::{CapabilityHandler, PluginSpec, Provenance, SecurityLevel};
use crate::ConversationEngine;

/// One skill carried by a pack: a named instruction document.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackSkillDoc {
    pub name: String,
    #[serde(default)]
    pub summary: String,
    pub instructions: String,
}

/// A certification check. All must pass for the pack to be (or stay) enabled.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PackEval {
    /// The named pack skill is present in the bank.
    SkillExists { name: String },
    /// The named pack skill has earned reliability: >= min_runs recorded runs at >= min_rate success.
    SkillReliable {
        name: String,
        #[serde(default = "d_min_runs")]
        min_runs: u32,
        #[serde(default = "d_min_rate")]
        min_rate: f32,
    },
    /// A tool call answers with the expected substring. This is an ENVIRONMENT PRECONDITION check
    /// (is calc alive, is weather configured) — it targets core or already-enabled tools, never the
    /// pack's own (those aren't dispatchable while the pack is disabled; use skill_answers).
    ToolContains {
        tool: String,
        #[serde(default)]
        args: serde_json::Value,
        expect: String,
    },
    /// RUNTIME verification of the pack's own behavior, pre-enable: execute the named pack skill
    /// directly (legitimate — it doesn't route through the disabled-plugin tool gate) and require
    /// a non-empty answer, optionally containing a substring. Costs a model call — certification
    /// is allowed to cost.
    SkillAnswers {
        name: String,
        #[serde(default)]
        input: String,
        #[serde(default)]
        expect: Option<String>,
    },
}

fn d_min_runs() -> u32 {
    1
}
fn d_min_rate() -> f32 {
    0.5
}

/// The pack document — what `ym pack install` accepts and `ym pack draft` emits.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackDoc {
    pub pack: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "d_category")]
    pub category: String,
    /// read_only|personal|gated_write — floored at Personal on install.
    #[serde(default)]
    pub security: String,
    /// imported|self_authored (never builtin — that's earned by compilation).
    #[serde(default)]
    pub provenance: String,
    /// Agent-facing catalog lines shown when the pack is enabled. Auto-derived if empty.
    #[serde(default)]
    pub catalog: String,
    #[serde(default)]
    pub skills: Vec<PackSkillDoc>,
    #[serde(default)]
    pub evals: Vec<PackEval>,
}

fn d_category() -> String {
    "Packs".into()
}

/// An installed pack + its certification state (persisted to the packs file).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledPack {
    pub doc: PackDoc,
    pub certified: bool,
    /// Ledger reference for the last landed verdict (a Weft note oid), when a ledger witnessed it.
    /// None = the claim is local-only, and the mind says so rather than implying it was proved.
    #[serde(default)]
    pub attestation: Option<String>,
}

/// Normalize a name into a tool-safe token.
pub(crate) fn normalize(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

impl PackDoc {
    /// (tool_name, bank_skill_name) pairs. Imported packs bank namespaced ("<pack>.<skill>") so a
    /// foreign document can never overwrite an existing bank entry; self-authored drafts REFERENCE
    /// skills the mind already banked under their own names.
    pub(crate) fn tool_map(&self) -> Vec<(String, String)> {
        let ns = self.provenance != "self_authored";
        self.skills
            .iter()
            .map(|s| {
                let tool = format!("{}.{}", normalize(&self.pack), normalize(&s.name));
                let bank = if ns { format!("{}.{}", normalize(&self.pack), s.name.trim()) } else { s.name.trim().to_string() };
                (tool, bank)
            })
            .collect()
    }

    fn provenance_enum(&self) -> Provenance {
        if self.provenance == "self_authored" {
            Provenance::SelfAuthored
        } else {
            Provenance::Imported
        }
    }

    fn security_enum(&self) -> SecurityLevel {
        // Floor at Personal: a pack never wears the read-only badge, and gated_write is honored.
        match SecurityLevel::parse(&self.security) {
            Some(SecurityLevel::GatedWrite) => SecurityLevel::GatedWrite,
            _ => SecurityLevel::Personal,
        }
    }

    /// Catalog lines are ALWAYS auto-derived for packs — a pack document's free-text `catalog`
    /// field is ignored, because those lines land in the agent prompt and a foreign document must
    /// not get to write arbitrary prompt text (injection surface). Summaries are truncated too.
    fn catalog_lines(&self) -> String {
        self.tool_map()
            .iter()
            .zip(self.skills.iter())
            .map(|((tool, _), s)| {
                let desc: String = (if s.summary.trim().is_empty() { &s.name } else { &s.summary }).chars().take(100).collect();
                format!("- {tool} {{input}}: {}", desc.replace(['\n', '\r'], " "))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn to_spec(&self) -> PluginSpec {
        let tools: Vec<String> = self.tool_map().into_iter().map(|(t, _)| t).collect();
        PluginSpec::dynamic(
            &normalize(&self.pack),
            &self.title,
            &self.category,
            self.security_enum(),
            &tools,
            &[], // no CLI aliases in v0 — pack tools are agent-facing, and alias words collide
            &self.catalog_lines(),
            self.provenance_enum(),
        )
    }
}

/// The one generic handler every installed pack dispatches through: tool name → banked skill →
/// a Think recipe over its instructions → text back, outcome recorded into the skill's reliability.
pub struct PackCapability {
    id: String,
    /// (tool_name, bank_skill_name)
    tools: Vec<(String, String)>,
}

#[async_trait::async_trait]
impl CapabilityHandler for PackCapability {
    fn id(&self) -> &str {
        &self.id
    }

    fn handles_commands(&self) -> bool {
        false
    }

    async fn handle_command(&self, _host: &ConversationEngine, _cmd: &str, _rest: &str) -> Option<String> {
        None
    }

    async fn handle_tool(&self, host: &ConversationEngine, tool: &str, args: &serde_json::Value) -> Option<String> {
        let bank_name = self.tools.iter().find(|(t, _)| t == tool).map(|(_, b)| b.clone())?;
        Some(host.pack_run_skill(&bank_name, args).await)
    }
}

impl ConversationEngine {
    /// Execute a pack skill: instructions + the caller's input through a Think step. Success or
    /// failure lands in the skill's reliability record — the same signal certification reads.
    /// Returns (ok, answer-or-diagnostic).
    pub(crate) async fn pack_run_skill_exec(&self, bank_name: &str, args: &serde_json::Value) -> (bool, String) {
        let sk = match self.memory.get_skill(bank_name).await {
            Ok(Some(s)) => s,
            _ => return (false, format!("(pack skill '{bank_name}' is missing from the bank)")),
        };
        let recipes = match &self.recipes {
            Some(r) => r,
            None => return (false, "(recipe engine unavailable — pack skills need it to run)".to_string()),
        };
        let input = if args.is_null() { String::new() } else { format!("\n\nINPUT:\n{}", args) };
        let rec = mind_recipes::Recipe {
            id: format!("pack:{bank_name}"),
            name: format!("pack skill: {bank_name}"),
            steps: vec![mind_recipes::RecipeStep::Think {
                prompt: format!("Follow these instructions exactly and return only the deliverable they describe:\n\n{}{input}", sk.code),
                store_as: "result".into(),
                on_error: mind_recipes::ErrorAction::Fail,
                max_tokens: None,
                think: None,
            }],
        };
        let out = recipes.run_with(&rec, std::collections::HashMap::new()).await;
        let result = out.vars.get("result").and_then(|v| v.as_str()).map(|s| s.to_string());
        let ok = out.error.is_none() && result.as_deref().map(|r| !r.trim().is_empty()).unwrap_or(false);
        let _ = self.memory.record_skill_outcome(bank_name, ok).await;
        match result {
            Some(r) if ok => (true, r),
            _ => (false, format!("(pack skill '{bank_name}' produced nothing{})", out.error.map(|e| format!(": {e}")).unwrap_or_default())),
        }
    }

    pub(crate) async fn pack_run_skill(&self, bank_name: &str, args: &serde_json::Value) -> String {
        self.pack_run_skill_exec(bank_name, args).await.1
    }

    /// Run a pack's evals. Returns (all_passed, one receipt line per eval).
    async fn pack_eval(&self, doc: &PackDoc) -> (bool, Vec<String>) {
        let map = doc.tool_map();
        let resolve = |name: &str| -> String {
            map.iter()
                .find(|(_, b)| b == name || b.ends_with(&format!(".{name}")))
                .map(|(_, b)| b.clone())
                .unwrap_or_else(|| name.to_string())
        };
        let mut lines = Vec::new();
        let mut all = true;
        for e in &doc.evals {
            let (ok, line) = match e {
                PackEval::SkillExists { name } => {
                    let bank = resolve(name);
                    let ok = matches!(self.memory.get_skill(&bank).await, Ok(Some(_)));
                    (ok, format!("skill_exists({bank})"))
                }
                PackEval::SkillReliable { name, min_runs, min_rate } => {
                    let bank = resolve(name);
                    let ok = match self.memory.get_skill(&bank).await {
                        Ok(Some(s)) => s.runs >= *min_runs as u64 && s.runs > 0 && (s.successes as f32 / s.runs as f32) >= *min_rate,
                        _ => false,
                    };
                    (ok, format!("skill_reliable({bank}, ≥{min_runs} runs @ ≥{:.0}%)", min_rate * 100.0))
                }
                PackEval::ToolContains { tool, args, expect } => {
                    let out = self.run_agent_tool(tool, args).await;
                    let ok = out.contains(expect.as_str());
                    (ok, format!("tool_contains({tool} ⊇ \"{expect}\")"))
                }
                PackEval::SkillAnswers { name, input, expect } => {
                    let bank = resolve(name);
                    let (ran, out) = self.pack_run_skill_exec(&bank, &serde_json::json!({ "input": input })).await;
                    let ok = ran && expect.as_deref().map(|e| out.contains(e)).unwrap_or(true);
                    (ok, format!("skill_answers({bank})"))
                }
            };
            all &= ok;
            lines.push(format!("   {} {line}", if ok { "✓" } else { "✗" }));
        }
        if doc.evals.is_empty() {
            all = false;
            lines.push("   ✗ no evals declared — an unfalsifiable pack can't be certified".to_string());
        }
        (all, lines)
    }

    /// Install a pack document (JSON). Banks its skills, registers it DISABLED, then certifies.
    pub async fn pack_install(&self, json: &str) -> String {
        let doc: PackDoc = match serde_json::from_str(json) {
            Ok(d) => d,
            Err(e) => return format!("(that isn't a pack document: {e})"),
        };
        self.pack_install_doc(doc, true).await
    }

    pub(crate) async fn pack_install_doc(&self, mut doc: PackDoc, bank_skills: bool) -> String {
        let id = normalize(&doc.pack);
        if id.is_empty() || doc.skills.is_empty() {
            return "(a pack needs a name and at least one skill)".to_string();
        }
        if doc.provenance != "self_authored" {
            doc.provenance = "imported".to_string();
        }
        // Register FIRST (disabled) — builtin shadowing and tool collisions are refused by the
        // registry itself, and a refused install must leave ZERO residue (no orphaned bank
        // entries). If banking fails after registration, the pack exists in a DEFINED state:
        // installed, uncertified, off, with skill_exists evals naming exactly what's missing.
        {
            let mut reg = self.plugins.lock().unwrap();
            if let Err(e) = reg.register_spec(doc.to_spec()) {
                return format!("(pack refused: {e})");
            }
            reg.register_handler(Arc::new(PackCapability { id: id.clone(), tools: doc.tool_map() }));
        }
        // Bank the skills (namespaced for imported packs — a foreign doc can't overwrite the bank).
        if bank_skills {
            let now = chrono::Utc::now().timestamp_millis() as u64;
            for (skill, (_, bank_name)) in doc.skills.iter().zip(doc.tool_map()) {
                let s = mind_types::Skill {
                    name: bank_name,
                    lang: "md".into(),
                    code: skill.instructions.clone(),
                    summary: skill.summary.clone(),
                    tags: vec![format!("pack:{id}"), doc.provenance.clone()],
                    status: "active".into(),
                    runs: 0,
                    successes: 0,
                    created_ms: now,
                };
                if let Err(e) = self.memory.save_skill(s).await {
                    return format!("(couldn't bank pack skill: {e} — '{id}' is installed but uncertified; fix and `ym pack certify {id}`)");
                }
            }
        }
        {
            let mut packs = self.packs.lock().unwrap();
            packs.retain(|p: &InstalledPack| normalize(&p.doc.pack) != id);
            packs.push(InstalledPack { doc: doc.clone(), certified: false, attestation: None });
        }
        self.save_packs();
        let verdict = self.pack_certify(&id).await;
        format!("📦 Installed '{id}' ({}, {} skill(s)) — disabled until certified.\n{verdict}", doc.provenance, doc.skills.len())
    }

    /// Run (or re-run) a pack's evals; enable on full pass, DISABLE on any failure (demotion).
    pub async fn pack_certify(&self, name: &str) -> String {
        let id = normalize(name);
        let doc = match self.packs.lock().unwrap().iter().find(|p| normalize(&p.doc.pack) == id) {
            Some(p) => p.doc.clone(),
            None => return format!("(no installed pack '{id}' — `ym packs` lists them)"),
        };
        let (passed, lines) = self.pack_eval(&doc).await;
        // Land the verdict on the trust ledger BEFORE flipping local state, so what the mind
        // believes about itself is backed by what an external witness recorded. A demotion lands
        // too — trust history is append-only, not a boolean that quietly flips back.
        let (att, att_note) = self.attest_verdict(&doc, passed, &lines).await;
        {
            let mut reg = self.plugins.lock().unwrap();
            let _ = reg.set_enabled(&id, passed);
        }
        {
            let mut packs = self.packs.lock().unwrap();
            if let Some(p) = packs.iter_mut().find(|p| normalize(&p.doc.pack) == id) {
                p.certified = passed;
                if att.is_some() {
                    p.attestation = att;
                }
            }
        }
        self.save_packs();
        self.save_plugins();
        if passed {
            format!("🎖 '{id}' certified — every eval passed, pack is ON.\n{}\n{att_note}", lines.join("\n"))
        } else {
            format!("🚫 '{id}' NOT certified — pack stays off until its evals pass.\n{}\n{att_note}\n   Fix or earn the misses, then `ym pack certify {id}`.", lines.join("\n"))
        }
    }

    /// Witness a certification verdict on the trust ledger. Returns (ledger ref, receipt line).
    /// No ledger configured, or a ledger that can't be reached, is NOT a certification failure —
    /// the verdict stands locally and is reported as unattested. Degrading loudly beats either
    /// blocking the mind on a daemon or pretending a claim was proved when nothing witnessed it.
    async fn attest_verdict(&self, doc: &PackDoc, passed: bool, lines: &[String]) -> (Option<String>, String) {
        let Some(attestor) = self.attestor.clone() else {
            return (None, "   (unattested — no trust ledger configured)".to_string());
        };
        let canonical = serde_json::to_vec(doc).unwrap_or_default();
        let att = mind_governance::weft::Attestation {
            subject: format!("pack:{}", normalize(&doc.pack)),
            verdict: passed,
            digest: mind_governance::weft::Attestation::digest_of(&canonical),
            evidence: lines.to_vec(),
        };
        let ledger = attestor.ledger().to_string();
        match tokio::task::spawn_blocking(move || attestor.attest(&att)).await {
            Ok(Ok(oid)) => {
                let short: String = oid.chars().take(12).collect();
                (Some(oid.clone()), format!("   ⛓ landed on {ledger}: {short}… ({}) ", if passed { "certification" } else { "demotion" }))
            }
            Ok(Err(e)) => (None, format!("   (unattested — {ledger} refused: {e})")),
            Err(e) => (None, format!("   (unattested — {ledger} join error: {e})")),
        }
    }

    /// Remove an installed pack (its banked skills stay — knowledge survives the packaging).
    pub async fn pack_rm(&self, name: &str) -> String {
        let id = normalize(name);
        let refused = { self.plugins.lock().unwrap().remove_spec(&id).err() };
        if let Some(e) = refused {
            return format!("({e})");
        }
        self.packs.lock().unwrap().retain(|p| normalize(&p.doc.pack) != id);
        self.save_packs();
        self.save_plugins();
        format!("Removed pack '{id}'. Its skills stay in the bank — `ym skills` still knows them.")
    }

    /// List installed packs with provenance + certification state.
    pub async fn pack_list(&self) -> String {
        let packs = self.packs.lock().unwrap().clone();
        if packs.is_empty() {
            return "No packs installed. `ym pack install <json>` to add one, `ym pack draft <topic>` to author one from what I've learned.".to_string();
        }
        let reg = self.plugins.lock().unwrap();
        let mut out = String::from("📦 Packs:\n");
        for p in &packs {
            let id = normalize(&p.doc.pack);
            let on = reg.spec(&id).map(|s| s.enabled).unwrap_or(false);
            let proof = match &p.attestation {
                Some(oid) => format!("⛓ {}…", oid.chars().take(10).collect::<String>()),
                None => "unattested".to_string(),
            };
            out.push_str(&format!(
                "  [{}] {:<16} {}  {}  — {} skill(s), {}, {}\n",
                if on { "on " } else { "OFF" },
                id,
                if p.certified { "🎖 certified" } else { "· uncertified" },
                proof,
                p.doc.skills.len(),
                p.doc.provenance,
                p.doc.title,
            ));
        }
        out.push_str("\n`ym pack certify <name>` re-runs a pack's evals; failures demote it to OFF.");
        out
    }

    /// SELF-AUTHOR a pack: gather banked skills matching a topic that have EARNED reliability,
    /// assemble them into a pack with auto-derived evals, install, and run certification. The
    /// evals require the very reliability that justified inclusion — the loop is closed and honest.
    pub async fn pack_draft(&self, topic: &str) -> String {
        let topic = topic.trim();
        if topic.len() < 2 {
            return "Draft what? `ym pack draft <topic>`".to_string();
        }
        let candidates = self.memory.recall_skills(topic, 8).await.unwrap_or_default();
        let proven: Vec<_> = candidates
            .into_iter()
            .filter(|s| s.runs > 0 && (s.successes as f32 / s.runs as f32) >= 0.5 && !s.name.contains('.'))
            .collect();
        if proven.is_empty() {
            return format!("Nothing proven to pack yet — I have no banked skills about \"{topic}\" with a run history. Teach me some first, then ask again.");
        }
        let id = format!("{}_pack", normalize(topic));
        let doc = PackDoc {
            pack: id.clone(),
            title: format!("Learned: {topic}"),
            description: format!("Self-authored from {} proven skill(s) about {topic}.", proven.len()),
            category: "Learned".into(),
            security: "personal".into(),
            provenance: "self_authored".into(),
            catalog: String::new(),
            skills: proven
                .iter()
                .map(|s| PackSkillDoc { name: s.name.clone(), summary: s.summary.clone(), instructions: s.code.clone() })
                .collect(),
            evals: proven
                .iter()
                .flat_map(|s| {
                    vec![
                        PackEval::SkillExists { name: s.name.clone() },
                        PackEval::SkillReliable { name: s.name.clone(), min_runs: 1, min_rate: 0.5 },
                        // runtime smoke: the skill must actually ANSWER, not just exist on paper
                        PackEval::SkillAnswers { name: s.name.clone(), input: String::new(), expect: None },
                    ]
                })
                .collect(),
        };
        let receipt = self.pack_install_doc(doc, false).await;
        format!("🖋 Drafted '{id}' from {} proven skill(s) about \"{topic}\".\n{receipt}", proven.len())
    }

    /// `ym weft` — is a trust ledger wired, and what has it witnessed? Answers honestly when
    /// nothing is configured instead of implying the mind's self-assessment was proved.
    pub async fn weft_status(&self) -> String {
        let Some(a) = &self.attestor else {
            return "⛓ No trust ledger configured — capability certifications are LOCAL claims only.\n   Set YM_WEFT_URL + YM_WEFT_KEY to land them on Weft (`did + proved`, not just `certified: true`).".to_string();
        };
        let packs = self.packs.lock().unwrap().clone();
        let (attested, total) = (packs.iter().filter(|p| p.attestation.is_some()).count(), packs.len());
        let mut out = format!("⛓ Trust ledger: {} — {attested}/{total} pack verdict(s) witnessed.\n", a.ledger());
        for p in &packs {
            match &p.attestation {
                Some(oid) => out.push_str(&format!(
                    "  {:<16} {} — {oid}\n",
                    normalize(&p.doc.pack),
                    if p.certified { "certified" } else { "demoted  " }
                )),
                None => out.push_str(&format!("  {:<16} unattested\n", normalize(&p.doc.pack))),
            }
        }
        out.push_str("   Each ref is a signed note on the Weft repo: verdict, content digest, and every eval line.");
        out
    }

    /// Persist installed packs (best-effort, like save_plugins).
    pub(crate) fn save_packs(&self) {
        if let Some(path) = &self.packs_path {
            let snapshot = { serde_json::to_string_pretty(&*self.packs.lock().unwrap()).unwrap_or_else(|_| "[]".into()) };
            let _ = std::fs::write(path, snapshot);
        }
    }

    /// Load installed packs from disk and re-register them (skills are already in the DB — this
    /// only rebuilds registry state). Certified packs come back enabled; uncertified stay off.
    pub fn with_packs_path(mut self, path: impl Into<String>) -> Self {
        let path = path.into();
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(installed) = serde_json::from_str::<Vec<InstalledPack>>(&raw) {
                let mut reg = self.plugins.lock().unwrap();
                for p in &installed {
                    let id = normalize(&p.doc.pack);
                    if reg.register_spec(p.doc.to_spec()).is_ok() {
                        reg.register_handler(Arc::new(PackCapability { id: id.clone(), tools: p.doc.tool_map() }));
                        if p.certified {
                            let _ = reg.set_enabled(&id, true);
                        }
                    }
                }
                drop(reg);
                *self.packs.lock().unwrap() = installed;
            }
        }
        self.packs_path = Some(path);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_is_tool_safe() {
        assert_eq!(normalize("Trip Watch!"), "trip_watch");
        assert_eq!(normalize("  java--modernization  "), "java_modernization");
    }

    #[test]
    fn imported_pack_banks_namespaced_and_self_authored_references() {
        let mut doc: PackDoc = serde_json::from_str(
            r#"{"pack":"tripwatch","title":"t","skills":[{"name":"fare check","instructions":"x"}]}"#,
        )
        .unwrap();
        doc.provenance = "imported".into();
        assert_eq!(doc.tool_map()[0], ("tripwatch.fare_check".to_string(), "tripwatch.fare check".to_string()));
        doc.provenance = "self_authored".into();
        assert_eq!(doc.tool_map()[0].1, "fare check".to_string());
    }

    #[test]
    fn security_is_floored_at_personal() {
        let doc: PackDoc = serde_json::from_str(
            r#"{"pack":"p","title":"t","security":"read_only","skills":[{"name":"a","instructions":"x"}]}"#,
        )
        .unwrap();
        assert_eq!(doc.security_enum(), SecurityLevel::Personal);
        let doc2: PackDoc = serde_json::from_str(
            r#"{"pack":"p","title":"t","security":"gated_write","skills":[{"name":"a","instructions":"x"}]}"#,
        )
        .unwrap();
        assert_eq!(doc2.security_enum(), SecurityLevel::GatedWrite);
    }
}

/// The floor in force for a pack, and whose number it is — rendered the same way everywhere an
/// operator can see it, so "0.55" never has to be guessed at.
fn floor_in_force(declared: Option<f64>) -> String {
    let eff = mind_types::memory::effective_pack_floor(declared);
    match declared {
        Some(d) if (d - eff).abs() < f64::EPSILON => format!("{eff:.2} (publisher-measured)"),
        Some(d) => format!("{eff:.2} (host wall; the pack declared {d:.2}, which cannot lower it)"),
        None => format!("{eff:.2} (host wall; the pack declares none)"),
    }
}

impl super::ConversationEngine {
    /// `ym pack route <query>` — the coverage router's verdict for a query and every pack's best
    /// phrase behind it. Asking leases nothing; in P.3 the router is shadowed on every turn.
    pub async fn packs_route(&self, query: &str) -> String {
        use mind_types::memory::PackRoute;
        match self.memory.route_packs(query).await {
            Err(e) => format!("(pack routing failed: {e})"),
            Ok((ranked, _)) if ranked.is_empty() => "No packs in the catalog — nothing to route to (`ym pack library`).".to_string(),
            Ok((ranked, route)) => {
                let verdict = match &route {
                    PackRoute::Lease { pack_id, sim, margin } => format!("would LEASE {pack_id} (sim {sim:.2}, margin {margin:.2} over the runner-up)"),
                    PackRoute::Abstain { reason, best } => format!(
                        "would ABSTAIN — {} {}",
                        match reason {
                            mind_types::memory::AbstainReason::NoPacks => "no packs".to_string(),
                            mind_types::memory::AbstainReason::BelowFloor => format!("best under the coverage floor {:.2}", mind_spec::coverage::COVERAGE_FLOOR),
                            mind_types::memory::AbstainReason::Tie => format!("two packs within the margin {:.2}", mind_spec::coverage::COVERAGE_MARGIN),
                        },
                        best.as_ref().map(|(p, s)| format!("(best {p} at {s:.2})")).unwrap_or_default()
                    ),
                };
                let mut out = format!("🧭 “{query}” — {verdict} · {} (shadow: nothing leased)\n", mind_spec::coverage::COVERAGE_POLICY_ID);
                for m in ranked.iter().take(6) {
                    out.push_str(&format!("  {:.2}  {}  ← “{}”\n", m.sim, m.pack_id, m.phrase.chars().take(70).collect::<String>()));
                }
                out
            }
        }
    }

    /// `ym pack library` — the catalog the router ranks over: mounted packs and library files,
    /// with the floor in force and how much coverage each declares.
    pub async fn packs_library(&self) -> String {
        match self.memory.available_packs().await {
            Err(e) => format!("(couldn't read the pack catalog: {e})"),
            Ok(cat) if cat.is_empty() => "The pack catalog is empty: nothing mounted and no `.ydbpack` files in the library (YM_PACK_LIBRARY, default <db dir>/pack-library).".to_string(),
            Ok(cat) => {
                let leases = self.memory.leases().await.unwrap_or_default();
                let now = mind_observability::now_ms() as i64;
                let mut out = format!("📚 {} pack(s) in the catalog\n", cat.len());
                for e in &cat {
                    let lease = leases.iter().find(|l| l.pack_id == e.pack_id);
                    let state = match (e.mounted, lease) {
                        (_, Some(l)) if l.is_serving(now) => format!("leased ({:.1} d left — {})", l.days_left(now), l.reason),
                        (_, Some(l)) => format!("{} lease — {}", l.state.label(), l.note.as_deref().unwrap_or(&l.reason)),
                        (true, None) => "mounted".to_string(),
                        (false, None) => "library (unmounted)".to_string(),
                    };
                    out.push_str(&format!(
                        "  {} {} · floor {:.2} · {} coverage phrase(s) · {}{}\n",
                        if e.mounted { "▣" } else { "▢" },
                        e.pack_id,
                        e.floor,
                        e.coverage.len(),
                        state,
                        e.content_digest.as_deref().map(|d| format!(" · {}…", d.chars().take(16).collect::<String>())).unwrap_or_default()
                    ));
                }
                out
            }
        }
    }

    // ── standing expertise leases (ARCH-6 P.4 v1, E.PK4) ─────────────────────────────────────

    /// `ym pack lease <id> [days=N] [reason=…]` — a STANDING lease: mounted now, released by the
    /// operator or by expiry. The record comes from the DURABLE OUTBOX, not from this function: the
    /// state change and its event are committed together, and the drain below turns the event into
    /// a recorder entry. A crash between the two loses nothing (Codex's review of P.4).
    pub async fn pack_lease(&self, arg: &str) -> String {
        let (id, days, reason) = match parse_lease_args(arg) {
            Ok(parsed) => parsed,
            Err(e) => return format!("({e})"),
        };
        let out = match self.memory.lease_pack(&id, days, &reason, "operator").await {
            Ok(l) => format!(
                "📦 Leased [{}] until {} — {}. Mounted now: its rules join my prompt from the next turn and its rows are recallable. `ym pack release {}` returns it early; `ym leases` lists what I'm borrowing.",
                l.pack_id,
                iso_utc(l.expires_ms),
                l.reason,
                l.pack_id
            ),
            Err(e) => format!("(couldn't lease that: {e})"),
        };
        self.drain_lease_events().await;
        out
    }

    /// `ym pack release <id>` — return a leased pack now. Also the way to clear a quarantined one.
    pub async fn pack_release(&self, id: &str) -> String {
        let id = id.trim();
        let out = match self.memory.release_pack(id, mind_types::memory::LeaseEnd::Released).await {
            Ok(Some(l)) => format!("📦 Released [{}] — returned early ({}). Unmounted if this lease was what attached it.", l.pack_id, l.reason),
            Ok(None) => format!("(no lease on {id} — `ym leases` lists what I'm borrowing)"),
            Err(e) => format!("(couldn't release that: {e})"),
        };
        self.drain_lease_events().await;
        out
    }

    /// `ym leases` — what I'm borrowing, soonest expiry first. A lease that is NOT serving says so:
    /// a quarantined artifact and an interrupted release are both visible, never silent.
    pub async fn leases_render(&self) -> String {
        match self.memory.leases().await {
            Err(e) => format!("(couldn't read the leases: {e})"),
            Ok(ls) if ls.is_empty() => format!("No standing leases. `ym pack lease <id> [days=N] [reason=…]` borrows a library pack (cap {}).", mind_types::memory::LEASE_CAP),
            Ok(ls) => {
                let now = mind_observability::now_ms() as i64;
                let serving = ls.iter().filter(|l| l.is_serving(now)).count();
                let mut out = format!("🔑 {} lease(s), {serving} serving (cap {})\n", ls.len(), mind_types::memory::LEASE_CAP);
                for l in &ls {
                    let state = match l.state {
                        mind_types::memory::LeaseState::Active => format!("{:.1} d left (until {})", l.days_left(now), iso_utc(l.expires_ms)),
                        other => format!("{}{}", other.label().to_uppercase(), l.note.as_deref().map(|n| format!(" — {n}")).unwrap_or_default()),
                    };
                    out.push_str(&format!(
                        "  {} · {state} · {} · by {}{}{}\n",
                        l.pack_id,
                        l.reason,
                        l.granted_by,
                        if l.mounted_by_lease { "" } else { " · attached by someone else" },
                        l.content_digest.as_deref().map(|d| format!(" · {}…", d.chars().take(16).collect::<String>())).unwrap_or_default()
                    ));
                }
                out
            }
        }
    }

    /// The expiry sweep the poll loop runs — an OPTIMISATION, not the clock: expiry is also enforced
    /// at the pack visibility boundary inside the memory actor, so a stopped loop or another
    /// frontend can never serve an expired lease (Codex's review of P.4). Returns log lines.
    pub async fn sweep_leases(&self) -> Vec<String> {
        let now = mind_observability::now_ms() as i64;
        let mut log = match self.memory.sweep_leases(now).await {
            Ok(ended) => ended
                .iter()
                .map(|l| format!("[lease] expired {} ({}) — returned", l.pack_id, l.reason))
                .collect(),
            Err(e) => vec![format!("[lease] sweep failed: {e}")],
        };
        log.extend(self.drain_lease_events().await);
        log
    }

    /// Reconcile the lease table with what is actually mounted, then record whatever that produced.
    /// Called once at startup, before the mind serves anything.
    pub async fn reconcile_leases(&self) -> Vec<String> {
        let mut log = self.memory.reconcile_leases().await.unwrap_or_else(|e| vec![format!("[lease] reconcile failed: {e}")]);
        log.extend(self.drain_lease_events().await);
        log
    }

    /// THE OUTBOX DRAIN. Every `pack_leased` / `pack_released` record is written from here and
    /// nowhere else: the memory actor commits the event beside the state change, this turns it into
    /// a flight-recorder entry, and only then is it acknowledged. The recorder event carries the
    /// outbox's own id, so a re-drain after a crash writes the same id rather than a second event.
    pub async fn drain_lease_events(&self) -> Vec<String> {
        let events = match self.memory.pending_lease_events().await {
            Ok(e) => e,
            Err(e) => return vec![format!("[lease] could not read pending lease events: {e}")],
        };
        let (mut log, mut undelivered) = (Vec::new(), 0usize);
        for ev in events {
            let l = &ev.lease;
            let kind = if ev.kind == "leased" { "pack_leased" } else { "pack_released" };
            let mut d = mind_observability::DecisionEvent::span(format!("lease-{}", l.granted_ms), None, kind);
            d.event_id = Some(ev.event_id.clone());
            d.object_id = Some(l.pack_id.clone());
            d.goal = Some(l.reason.clone());
            d.evidence_ids = l.content_digest.iter().cloned().collect();
            d.policy = vec![
                "standing-lease-v1".to_string(),
                format!("cap={}", mind_types::memory::LEASE_CAP),
                "mounted at grant; unmounted at release or expiry, only what the lease attached".to_string(),
            ];
            match ev.end_reason {
                None => {
                    d.actor = Some(l.granted_by.clone());
                    d.outcome = Some(format!("until {}", iso_utc(l.expires_ms)));
                }
                Some(end) => {
                    d.actor = Some(match end {
                        mind_types::memory::LeaseEnd::Released => l.granted_by.clone(),
                        mind_types::memory::LeaseEnd::Expired => "sweep".to_string(),
                        mind_types::memory::LeaseEnd::Quarantined => "reconciler".to_string(),
                    });
                    d.verdict = Some(end.label().to_string());
                    d.outcome = Some(format!("granted {} · was due {}", iso_utc(l.granted_ms), iso_utc(l.expires_ms)));
                }
            }
            // ACKNOWLEDGE ONLY WHAT IS DURABLE. `record` cannot fail from the caller's side — it
            // no-ops while the recorder is in its failure backoff — so acknowledging after it
            // destroyed the very evidence the outbox exists to keep (Codex's review of P.4a).
            // `record_once` reports what really happened, and dedupes by the event's own id, so a
            // crash between the append and this acknowledgement re-delivers rather than duplicates.
            match self.recorder.record_once(d) {
                o if o.is_durable() => {
                    if let Err(e) = self.memory.ack_lease_event(&ev.event_id).await {
                        log.push(format!("[lease] recorded {} but could not acknowledge it: {e}", ev.event_id));
                    }
                }
                mind_observability::RecordOutcome::Disabled => {
                    // NEVER acknowledged. `ConversationEngine::new` leaves the recorder disabled,
                    // so a host that simply forgot `with_recorder` would otherwise delete its own
                    // audit trail one lease at a time (Codex's review of P.4c). The events are
                    // kept and the backlog is said out loud; nothing silently disappears.
                    undelivered += 1;
                }
                mind_observability::RecordOutcome::Failed(why) => {
                    undelivered += 1;
                    log.push(format!("[lease] {} stays in the outbox — the recorder did not take it: {why}", ev.event_id));
                }
                _ => {}
            }
        }
        if undelivered > 0 {
            // Visible, held by the outbox itself, and never silently dropped: an operator can see
            // exactly how much lease evidence is waiting for a recorder that will take it.
            log.push(format!(
                "[lease] {undelivered} lease event(s) still undelivered{} — they stay in the outbox until a recorder accepts them",
                if self.recorder.trace_path().is_none() { " (this mind has no decision log configured)" } else { "" }
            ));
        }
        log
    }

    /// `ym pack stats` — every pack's local ladder from BOTH witnesses, side by side: the SQL
    /// counters in `mind_pack_stats` and a recount of the flight recorder. Both are written by the
    /// same code path, so agreement is not proof of truth — but disagreement is proof of a defect
    /// in the instrument, and an instrument defect must be found here before it is found in a
    /// decision (Doctrine 3, the cheap form: two mechanisms reading persisted state).
    pub async fn packs_stats(&self) -> String {
        let table = match self.memory.pack_stats().await {
            Ok(t) => t,
            Err(e) => return format!("(couldn't read pack stats: {e})"),
        };
        let recorder = mind_observability::pack_evidence_counts(&self.recorder.read_all());
        if table.is_empty() && recorder.is_empty() {
            return "No pack evidence recorded yet — rows appear when a mounted pack's evidence reaches a turn.".to_string();
        }
        let mut out = String::from(
            "PACK EVIDENCE — two witnesses (SQL counters | flight-recorder recount), each as surfaced / used [proxy] / graded / accepted\n",
        );
        let mut ids: std::collections::BTreeSet<String> = table.iter().map(|t| t.pack_id.clone()).collect();
        ids.extend(recorder.keys().cloned());
        for id in ids {
            let t = table.iter().find(|t| t.pack_id == id);
            let r = recorder.get(&id);
            let sql = t
                .map(|t| format!("{} / {} / {} / {}", t.surfaced, t.used, t.graded, t.good))
                .unwrap_or_else(|| "— / — / — / —".to_string());
            let rec = r
                .map(|c| format!("{} / {} / {} / {}", c.surfaced, c.used, c.graded(), c.good))
                .unwrap_or_else(|| "— / — / — / —".to_string());
            let agree = match (t, r) {
                (Some(t), Some(c)) => {
                    (t.surfaced as usize, t.used as usize, t.graded as usize, t.good as usize) == (c.surfaced, c.used, c.graded(), c.good)
                }
                _ => false,
            };
            out.push_str(&format!(
                "  {id}: {sql} | {rec} {}\n",
                if agree {
                    "✓ witnesses agree"
                } else {
                    "⚠ witnesses DISAGREE — an instrument defect or a reset recorder; trust neither until explained"
                }
            ));
            if let Some(d) = t.and_then(|t| t.content_digest.as_deref()) {
                out.push_str(&format!("      evidence keyed to {}…\n", d.chars().take(20).collect::<String>()));
            }
        }
        out.push_str("  (`ym why packs` for the denominators and the selective-observation audit)");
        out
    }

    /// `ym pack probe <query>` — the pack evidence a turn on this query would receive, hit by hit,
    /// with the similarity each cleared and the floors it was measured against.
    ///
    /// When nothing clears, the floors in force are named rather than an empty line returned:
    /// "no evidence" and "evidence withheld" are different answers, and the second is the one the
    /// attach-harm measurement (PACKS.md §5b) exists to produce.
    pub async fn packs_probe(&self, query: &str) -> String {
        let packs = match self.memory.mounted_packs().await {
            Ok(p) => p,
            Err(e) => return format!("(couldn't read mounted packs: {e})"),
        };
        if packs.is_empty() {
            return "No knowledge packs mounted — nothing to probe.".to_string();
        }
        let floors = packs
            .iter()
            .map(|p| format!("{} floor {}", p.id, floor_in_force(p.recommended_min_similarity)))
            .collect::<Vec<_>>()
            .join(" · ");
        match self.memory.probe_packs(query, 5).await {
            Err(e) => format!("(pack probe failed: {e})"),
            Ok(rows) if rows.is_empty() => {
                format!("🔍 The engine returned no attributable pack rows for “{query}” — nothing to floor.\n   in force: {floors}")
            }
            Ok(rows) => {
                use mind_types::memory::PackDisposition as D;
                let count = |d: D| rows.iter().filter(|r| r.disposition == d).count();
                let mut out = format!(
                    "🔍 “{query}” — {} row(s) would reach a turn · withheld: {} by the floor, {} by a pack's top_k, {} beyond the turn's limit\n",
                    count(D::Cleared),
                    count(D::WithheldFloor),
                    count(D::WithheldPackCap),
                    count(D::WithheldLimit)
                );
                for r in &rows {
                    // Three decimals: a hairline miss rendered at two read "0.55 < 0.55", which is
                    // an impossible sentence and hid the very boundary the operator was diagnosing
                    // (Codex's review of the live evidence).
                    let (mark, why) = match r.disposition {
                        D::Cleared => ("✓", format!("sim {:.3} ≥ {:.3}", r.similarity, r.floor)),
                        D::WithheldFloor => ("✗", format!("sim {:.3} < {:.3}", r.similarity, r.floor)),
                        D::WithheldPackCap => ("✗", format!("sim {:.3} ≥ {:.3} but over the pack's top_k", r.similarity, r.floor)),
                        D::WithheldLimit => ("✗", format!("sim {:.3} ≥ {:.3} but beyond the turn's limit", r.similarity, r.floor)),
                    };
                    out.push_str(&format!(
                        "  {mark} [{}] {why} · score {:.2} · {}\n",
                        r.pack_id,
                        r.score,
                        r.text.chars().take(100).collect::<String>().replace('\n', " ")
                    ));
                }
                out.push_str(&format!("   in force: {floors}"));
                out
            }
        }
    }

    /// `ym pack mounted` — the knowledge packs attached right now.
    ///
    /// Reports trust verbatim rather than as a tick: "Signed" means the signature verified AND the
    /// publisher key is one this host trusts; "Unsigned" can still mean integrity was proven and only
    /// the identity is unknown. Collapsing those into one symbol is how a re-signed pack borrows
    /// someone else's reputation.
    pub async fn packs_mounted(&self) -> String {
        match self.memory.mounted_packs().await {
            Err(e) => format!("(couldn't read mounted packs: {e})"),
            Ok(p) if p.is_empty() => {
                "No knowledge packs mounted. `ym pack mount <file.ydbpack>` for this run, or `ym pack adopt <file>` to keep it."
                    .to_string()
            }
            Ok(packs) => {
                let mut out = format!("📦 {} knowledge pack(s) mounted\n", packs.len());
                for p in &packs {
                    out.push_str(&format!(
                        "  {} {}@{} · {} · {} rows · trust: {} · {}\n      floor {} · {} coverage topic(s){}\n",
                        if p.trust.contains("Signed") { "🔏" } else { "•" },
                        p.name,
                        p.version,
                        p.origin,
                        p.rows,
                        p.trust,
                        // The distinction the unmount-that-wasn't bug hid: an adopted pack
                        // returns on every restart, a mounted one dies with the process.
                        if p.installed { "adopted (returns on restart; `disown` removes)" } else { "this run only" },
                        // The floor IN FORCE, and whose number it is: a publisher-measured floor,
                        // the host wall holding against a lower declaration, and the wall applied
                        // because the pack declares none are three different pieces of evidence.
                        floor_in_force(p.recommended_min_similarity),
                        p.coverage.len(),
                        p.recommended_top_k.map(|k| format!(" · top_k {k}")).unwrap_or_default(),
                    ));
                }
                out
            }
        }
    }
}

/// `<id> [days=N] [reason=…]` — the reason runs to the end of the line, so it may have spaces.
/// An unreadable `days=` is an ERROR, not a silent default: an operator who typed `days=thirty`
/// asked for a month and would otherwise get a week without being told (Codex's review of P.4).
pub(crate) fn parse_lease_args(arg: &str) -> std::result::Result<(String, u32, String), String> {
    let arg = arg.trim();
    let (head, reason) = match arg.find("reason=") {
        Some(i) => (&arg[..i], arg[i + "reason=".len()..].trim().to_string()),
        None => (arg, String::new()),
    };
    let mut id = None;
    let mut days = mind_types::memory::DEFAULT_LEASE_DAYS;
    for tok in head.split_whitespace() {
        if let Some(n) = tok.strip_prefix("days=") {
            days = n
                .parse::<u32>()
                .map_err(|_| format!("days= must be a whole number of days, got `{n}`"))?;
            if days == 0 || days > mind_types::memory::MAX_LEASE_DAYS {
                return Err(format!("days= must be between 1 and {}, got {days}", mind_types::memory::MAX_LEASE_DAYS));
            }
        } else if id.is_none() {
            id = Some(tok.to_string());
        } else {
            return Err(format!("unexpected `{tok}` — usage: ym pack lease <pack-id> [days=N] [reason=why]"));
        }
    }
    let id = id.ok_or_else(|| "usage: ym pack lease <pack-id> [days=N] [reason=why]".to_string())?;
    // An EXPLICIT `reason=` with nothing after it is an error, not an unstated reason: the
    // operator meant to say why and did not, and quietly writing "unstated" into the record
    // bypasses the memory layer's own non-empty check (Codex's review of P.4a).
    if arg.contains("reason=") && reason.is_empty() {
        return Err("reason= was given with nothing after it — say why, or leave reason= off".to_string());
    }
    let reason = if reason.is_empty() { "unstated".to_string() } else { reason };
    Ok((id, days, reason))
}

fn iso_utc(ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
        .map(|d| d.format("%Y-%m-%d %H:%MZ").to_string())
        .unwrap_or_else(|| format!("{ms} ms"))
}
