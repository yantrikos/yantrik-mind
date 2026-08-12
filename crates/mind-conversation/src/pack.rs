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

impl super::ConversationEngine {
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
                        "  {} {}@{} · {} · {} rows · trust: {}\n",
                        if p.trust.contains("Signed") { "🔏" } else { "•" },
                        p.name, p.version, p.origin, p.rows, p.trust
                    ));
                }
                out
            }
        }
    }
}
