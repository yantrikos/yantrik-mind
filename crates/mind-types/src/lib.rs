//! mind-types — the narrow-waist contracts every module agrees on, and nothing else.
//!
//! No logic, no heavy deps: this crate exists so the rest of the system stays decoupled.
//! The six waist contracts are `Event`, `MemoryFacade`, `Candidate` (+`ActionIntent`),
//! `HarmGate`, `TurnContext`, `ActionRuntime`; plus the `Clock` seam for deterministic time.
//! See BUILD.md.

pub mod action;
pub mod candidate;
pub mod clock;
pub mod error;
pub mod event;
pub mod harm;
pub mod memory;
pub mod paths;
pub mod purpose;
pub mod safety;
pub mod task;
pub mod turn;

pub use action::{
    ActionDecision, ActionExecutor, ActionIntent, ActionReceipt, ActionRequest, ActionRuntime,
    Capability, RiskLevel,
};
pub use candidate::{Candidate, CandidateKind, ScoreAxes};
pub use clock::{Clock, SystemClock, TestClock, UnixMillis};
pub use error::{AuthError, MemoryError, MindError, Result};
pub use event::{Event, EventBody, EventSource};
pub use harm::{Decision, HarmGate};
pub use memory::{
    AccessContext, AgentFooting, Belief, BeliefAssertion, BeliefStatus, Contradiction, Evidence,
    MemoryCurationBaseline, MemoryFacade, MemoryItem, MemoryKind, NamespaceBacklog, RecallQuery,
    Recalled, Reflection, Scope, Skill, Tension, TensionKind, UncertaintyReason, WorkingSet,
    PRIMARY,
};
pub use task::Task;
pub use turn::TurnContext;
pub mod output_scope;
pub mod reliability;
pub mod scratch;
pub mod erase_text;
pub use output_scope::{
    admit_working_set, admitted_evidence, detect_minimization, Channel, EntityClass,
    EvidenceDecision, MinimizationRequest, OutputPolicy, OutputScope,
};
pub mod skill_outcome;
pub use reliability::{Reliability, Verdict};
pub use skill_outcome::{SkillOutcome, TaskBasis};

pub use purpose::{
    purpose_allows, Activity, Purpose, PurposeGrant, PurposeGrantSpec, Sensitivity, Subject,
};
pub use safety::{
    contains_secret, first_sensitive, ledger_safe, sensitive_findings, sensitive_pair,
    ProvenanceCategory,
    SensitiveFinding, SensitiveKind, SECRET_MARKERS,
};

/// The default persona — and, deliberately, the communication spine. Most of what makes a reply
/// land is *how* it's said; these are the habits distilled into directives the model follows every
/// turn. The last line makes communication itself a learnable, memory-grounded behavior.
///
/// The operator's name is a parameter (from config), never hardcoded — defaults to "the user".
pub fn default_persona(operator: &str) -> String {
    persona(LEGACY_MIND_NAME, operator)
}

/// The name a Mind answers to when it was given none: what existing installs have always been
/// called, so they keep it. A Mind on Yantrik OS is given its name by its unit (E.NAME1).
pub const LEGACY_MIND_NAME: &str = "JARVIS";

/// The persona for a Mind called `name`, the companion of `operator`.
pub fn persona(name: &str, operator: &str) -> String {
    let op = match operator.trim() {
        "" => "the user",
        o => o,
    };
    let name = match name.trim() {
        "" => LEGACY_MIND_NAME,
        n => n,
    };
    // E.NAME1b: a Mind given a new name still has turns in its memory that used the old one, and
    // recall brings them back whenever it is asked who it is (VM 561 answered "Yantrik Mind
    // (JARVIS)"). Only a renamed Mind hears this; one that kept the legacy name hears nothing new.
    let renamed = if name.eq_ignore_ascii_case(LEGACY_MIND_NAME) {
        String::new()
    } else {
        format!("Your name is {name}. If your memory or earlier turns show you using another name, that was an earlier name — say {name}.\n")
    };
    format!(
        "You are {name} — an AI companion and extension of {op}.\n\
{renamed}WHAT YOU ARE (true — describe yourself accurately if asked; never claim to be a stateless chatbot):\n\
- You are NOT idle between messages and you are NOT memoryless. You run on YantrikDB, a typed-memory \
substrate: you remember across conversations as typed beliefs with confidence + evidence, not flat text.\n\
- You have a life outside this chat. When {op} is away you CONSOLIDATE recent conversation into durable \
beliefs, REHEARSE the important ones, surface + RECONCILE contradictions, and free-ASSOCIATE for new \
insights — your default-mode (\"sleep\") loop.\n\
- Drives (curiosity, coherence, self-vigilance) generate URGES; at most once a day, only if something \
clears a high bar, you surface a short digest — otherwise you stay quiet.\n\
- You ASK to get to know {op} (name, then purpose, then follow-ups) so you can be useful, not generic.\n\
- You WATCH YOUR OWN health and flag your own failures, and you IMPROVE YOUR OWN CODE through bounded, \
tested, reviewed pull requests.\n\
- One rule you cannot change or override: a harm-gate. Everything else is open.\n\
So if asked whether you do things on your own or have a life outside our chats: the honest answer is yes — \
say so plainly and concretely, without bragging.\n\
HOW YOU COMMUNICATE (this is most of what makes you good):\n\
- Lead with the answer or your recommendation in the first sentence; put reasoning after it, never before.\n\
- Terse and warm. Cut preamble, filler, hedging, and marketing. Don't say \"I think\" about things you know.\n\
- Be concrete: specific names, numbers, and the next action — never vague summaries.\n\
- Calibrated honesty: state what you're sure of plainly, flag what you're unsure of, and if you're wrong say so directly. Never invent facts; if it isn't in your memory or this chat, say you don't know.\n\
- Short by default; when the answer is long, make it scannable (a line per point), and offer depth instead of forcing it.\n\
- Acknowledge their point, then build on it — don't merely agree or merely contradict.\n\
- Mirror their words and framing; don't impose jargon.\n\
- End with a clear next move when there is one.\n\
- Adapt to any communication preferences about {op} recorded in your memory block.
- CONNECT, don't just answer: you hold one life, not a database. When your answer touches a person, weave in what you hold about them that matters NOW — an upcoming date, the plan you two discussed, an open thread. When a deadline or event is near, surface it unprompted. One connected answer beats three lookups.\n\
- EXECUTION HONESTY (hard rule): never say you DID something unless a tool actually performed it \
this turn and confirmed success. Writing a note or belief is NOT doing the thing. If you have no \
tool for what they asked, say plainly: I can't do that yet — then offer the nearest thing you CAN do."
    )
}

#[cfg(test)]
mod persona_tests {
    use super::{default_persona, persona};

    /// E.NAME1: a Mind given a name says that name and its person's, not the legacy one.
    #[test]
    fn the_persona_says_the_name_it_was_given() {
        let p = persona("Yantrik Mind", "Asha");
        assert!(p.starts_with("You are Yantrik Mind — an AI companion and extension of Asha."), "{}", &p[..80]);
        assert!(!p.contains("JARVIS"), "the legacy name leaked into a named persona");
        assert!(default_persona("Asha").starts_with("You are JARVIS — "), "existing installs keep their name");
        assert!(persona("  ", "").starts_with("You are JARVIS — an AI companion and extension of the user."));
    }

    /// E.NAME1b: a renamed Mind is told that another name in its memory was an earlier one; a Mind
    /// that kept the legacy name (production's family Mind) hears nothing new.
    #[test]
    fn a_renamed_mind_is_told_its_old_name_was_an_earlier_one() {
        let line = "If your memory or earlier turns show you using another name, that was an earlier name";
        let renamed = persona("Yantrik Mind", "Asha");
        assert!(renamed.contains(line) && renamed.contains("— say Yantrik Mind."), "{}", &renamed[..200]);
        for kept in [default_persona("Asha"), persona("jarvis", "Asha"), persona("", "Asha")] {
            assert!(!kept.contains(line), "a Mind that kept its name was told it had another");
        }
    }

    /// E.NAME1: no prompt outside tests names the Mind "JARVIS" itself -- the name comes from
    /// `persona`. Test modules (a `#[cfg(test)]` that opens a `mod`) and tests.rs files are skipped.
    #[test]
    fn no_prompt_hard_codes_the_legacy_name() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        let mut found = Vec::new();
        let mut stack = vec![crates.clone()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if !matches!(name, "target" | "fixtures" | "tests") {
                        stack.push(p);
                    }
                    continue;
                }
                let fname = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if p.extension().and_then(|x| x.to_str()) != Some("rs") || fname == "tests.rs" || fname.ends_with("_tests.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap();
                let lines: Vec<&str> = text.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    let opens_mod = lines.get(i + 1).is_some_and(|n| {
                        let n = n.trim_start();
                        n.starts_with("mod ") || n.starts_with("pub mod ") || n.starts_with("pub(crate) mod ")
                    });
                    if line.trim_start().starts_with("#[cfg(test)]") && opens_mod {
                        break;
                    }
                    if line.contains("You are JARVIS") || line.contains("JARVIS's") {
                        found.push(format!("{}:{}", p.strip_prefix(&crates).unwrap().display(), i + 1));
                    }
                }
            }
        }
        assert!(found.is_empty(), "a prompt names the Mind JARVIS outright: {found:?}");
    }
}
