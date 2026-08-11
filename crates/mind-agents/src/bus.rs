//! bus — the capability seam the cognitive loop acts through.
//!
//! # Why a trait and not a direct call
//!
//! The full tool surface lives in `mind-conversation`, which sits ABOVE this crate. The loop cannot
//! call down into its own caller, so the dependency is inverted: the loop states what it needs from a
//! capability bus, and `mind-conversation` supplies one. That keeps the runtime testable against a
//! fake bus with no engine, no memory, and no network — which is what makes the loop's behaviour
//! assertable at all.
//!
//! # The two methods that matter most
//!
//! [`Bus::ready_capabilities`] is what lets the compiler refuse a goal BEFORE running it. Asking for
//! GitHub on a mind with no token should be a sentence at compile time, not a tool error improvised
//! around at step four.
//!
//! [`Bus::normalize`] is where a 12 KB page becomes six fields. Nothing else in this crate is allowed
//! to hand raw tool output to a model — the whole token argument collapses if a loop that carefully
//! keeps a 2 KB capsule then pastes an HTML document next to it.

use async_trait::async_trait;
use mind_spec::capsule::{Evidence, Observation};
use serde_json::Value;

/// What the cognitive loop can do, as the layer above defines it.
#[async_trait]
pub trait Bus: Send + Sync {
    /// The tool catalog for THIS goal, already relevance-gated by the caller. One tool per line.
    fn catalog(&self, goal: &str) -> String;

    /// Capability ids whose backing clients are actually present. The compiler resolves against this,
    /// so a goal naming something unavailable is refused with a reason rather than attempted.
    fn ready_capabilities(&self) -> Vec<String>;

    /// Would calling this tool have an effect outside the mind?
    ///
    /// The harm gate still governs the action itself — this only lets the controller stop and ask
    /// BEFORE walking into it. Two independent checks on purpose: a bus that answered wrong here
    /// must not be able to bypass the gate.
    fn is_outward(&self, tool: &str) -> bool;

    /// Run one tool. Output is UNTRUSTED (it may carry web or third-party content).
    async fn call(&self, tool: &str, args: &Value) -> anyhow::Result<String>;

    /// Turn a raw tool result into an [`Observation`].
    ///
    /// The default implementation is deliberately dumb — one evidence item holding the text, capped.
    /// It exists so a bus can be written in three methods; a real one overrides this per tool, which
    /// is where the semantic preprocessing belongs (a GitHub response becoming language/stars/
    /// activity/interesting-files rather than 2,000 lines of metadata).
    fn normalize(&self, tool: &str, args: &Value, raw: &str, ok: bool) -> Observation {
        let summary: String = raw.trim().lines().next().unwrap_or("").chars().take(160).collect();
        Observation {
            action: signature(tool, args),
            ok,
            evidence: if ok && !raw.trim().is_empty() {
                vec![Evidence {
                    id: String::new(), // the loop assigns ids; a bus should not guess them
                    summary,
                    source: tool.to_string(),
                    body: raw.chars().take(20_000).collect(),
                    captured_ms: 0,
                }]
            } else {
                Vec::new()
            },
            error: (!ok).then(|| raw.chars().take(300).collect()),
            did: ok.then(|| format!("ran {tool}")),
            ..Default::default()
        }
    }

    /// Remembered approaches for this kind of task, best-known first.
    ///
    /// Called once per run BEFORE the first decision, because looking for a known way to do something
    /// should always happen — a model asked whether it would like to check will sometimes say no and
    /// then improvise an approach it already had. Default is empty, so a bus without procedural memory
    /// simply has none rather than failing.
    async fn procedures(&self, _goal: &str, _limit: usize) -> Vec<crate::procedure::Procedure> {
        Vec::new()
    }

    /// Record whether following a procedure worked.
    ///
    /// This is what turns a procedure library into a ranked one. Without it every approach stays
    /// equally plausible forever, and the loop cannot prefer the one that actually works — the
    /// difference between a memory and a filing cabinet.
    async fn record_procedure_outcome(&self, _name: &str, _ok: bool) {}

    /// Bank a new approach the run discovered.
    ///
    /// Called only when a run SUCCEEDED with no procedure to guide it — that is precisely the case
    /// worth remembering, and the case where the next run would otherwise re-derive the same
    /// reasoning. Returns whether it was stored.
    async fn bank_procedure(&self, _name: &str, _when: &str, _steps: &[String]) -> bool {
        false
    }

    /// Strip uncited claims from a synthesized answer, returning the grounded text.
    ///
    /// `None` means the seam is unavailable, which the caller must treat as "unverified" rather than
    /// as "verified". Backed by the recipe engine's ThinkCited→Validate in production.
    async fn ground(&self, _question: &str, _evidence: &str) -> Option<String> {
        None
    }
}

/// A stable signature for one tool call.
///
/// This is what deduplication and loop detection compare, so it has to be canonical: the same call
/// with keys in a different order must produce the same string, or a model that re-emits its
/// arguments in a new order would defeat the guard. `serde_json::Map` is a BTreeMap, so object keys
/// are already sorted — this relies on that rather than re-sorting.
pub fn signature(tool: &str, args: &Value) -> String {
    match args {
        Value::Object(m) if m.is_empty() => tool.to_string(),
        Value::Null => tool.to_string(),
        other => {
            // Truncated because a signature is an identity, not a payload — a 20 KB argument would
            // put 20 KB into the capsule's attempt log.
            let s = other.to_string();
            format!("{tool}|{}", s.chars().take(200).collect::<String>())
        }
    }
}

/// A bus for tests. Shared across this crate's test modules so a scenario is three lines rather than
/// a trait impl each time — and so every test runs against the SAME fake, which means a change in the
/// seam breaks them all at once instead of one at a time.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use std::sync::Mutex;

    pub struct FakeBus {
        pub known: Vec<crate::procedure::Procedure>,
        pub outcomes: Mutex<Vec<(String, bool)>>,
        pub banked: Mutex<Vec<String>>,
        pub ready: Vec<String>,
        /// Tool name -> what it returns. A tool not listed here fails, which is how a scenario tests
        /// the failure path without any plumbing.
        pub replies: Mutex<std::collections::HashMap<String, String>>,
        pub outward: Vec<String>,
        pub calls: Mutex<Vec<String>>,
        pub grounded: Option<String>,
    }

    impl FakeBus {
        pub fn new(ready: &[&str]) -> Self {
            Self {
                known: Vec::new(),
                outcomes: Mutex::new(Vec::new()),
                banked: Mutex::new(Vec::new()),
                ready: ready.iter().map(|s| s.to_string()).collect(),
                replies: Mutex::new(std::collections::HashMap::new()),
                outward: Vec::new(),
                calls: Mutex::new(Vec::new()),
                grounded: None,
            }
        }
        pub fn returning(self, tool: &str, reply: &str) -> Self {
            self.replies.lock().unwrap().insert(tool.to_string(), reply.to_string());
            self
        }
        pub fn outward(mut self, tools: &[&str]) -> Self {
            self.outward = tools.iter().map(|s| s.to_string()).collect();
            self
        }
        pub fn grounding(mut self, text: &str) -> Self {
            self.grounded = Some(text.to_string());
            self
        }
        pub fn called(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        pub fn knowing(mut self, procedures: Vec<crate::procedure::Procedure>) -> Self {
            self.known = procedures;
            self
        }
        pub fn recorded(&self) -> Vec<(String, bool)> {
            self.outcomes.lock().unwrap().clone()
        }
        pub fn banked_names(&self) -> Vec<String> {
            self.banked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Bus for FakeBus {
        fn catalog(&self, _goal: &str) -> String {
            self.ready.iter().map(|c| format!("- {c} {{query}}: the {c} tool")).collect::<Vec<_>>().join("\n")
        }
        fn ready_capabilities(&self) -> Vec<String> {
            self.ready.clone()
        }
        fn is_outward(&self, tool: &str) -> bool {
            self.outward.iter().any(|t| t == tool)
        }
        async fn call(&self, tool: &str, args: &Value) -> anyhow::Result<String> {
            self.calls.lock().unwrap().push(signature(tool, args));
            match self.replies.lock().unwrap().get(tool) {
                Some(r) => Ok(r.clone()),
                None => anyhow::bail!("no such tool: {tool}"),
            }
        }
        async fn ground(&self, _q: &str, _e: &str) -> Option<String> {
            self.grounded.clone()
        }
        async fn procedures(&self, _goal: &str, _limit: usize) -> Vec<crate::procedure::Procedure> {
            self.known.clone()
        }
        async fn record_procedure_outcome(&self, name: &str, ok: bool) {
            self.outcomes.lock().unwrap().push((name.to_string(), ok));
        }
        async fn bank_procedure(&self, name: &str, _when: &str, _steps: &[String]) -> bool {
            self.banked.lock().unwrap().push(name.to_string());
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_signature_is_canonical_across_key_order() {
        let a = signature("search", &json!({"query": "x", "limit": 3}));
        let b = signature("search", &json!({"limit": 3, "query": "x"}));
        assert_eq!(a, b, "key order must not create a new identity, or dedup fails silently");
    }

    #[test]
    fn a_signature_distinguishes_different_arguments() {
        assert_ne!(
            signature("search", &json!({"query": "a"})),
            signature("search", &json!({"query": "b"})),
            "different searches are different actions"
        );
        assert_eq!(signature("now", &json!({})), "now", "an argument-free tool is just its name");
        assert_eq!(signature("now", &Value::Null), "now");
    }

    #[test]
    fn a_signature_is_bounded() {
        let big = json!({ "html": "x".repeat(50_000) });
        assert!(signature("publish_page", &big).len() < 260, "a signature must not carry a payload");
    }

    struct Bare;
    #[async_trait]
    impl Bus for Bare {
        fn catalog(&self, _: &str) -> String {
            String::new()
        }
        fn ready_capabilities(&self) -> Vec<String> {
            vec![]
        }
        fn is_outward(&self, _: &str) -> bool {
            false
        }
        async fn call(&self, _: &str, _: &Value) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    /// The default normalizer must produce evidence with a BODY but a short summary — and must not
    /// invent an evidence id, since the loop owns id assignment.
    #[test]
    fn the_default_normalizer_summarizes_and_leaves_ids_to_the_loop() {
        let raw = format!("Company raised FY guidance\n{}", "detail ".repeat(5000));
        let o = Bare.normalize("fetch", &json!({"url": "http://x"}), &raw, true);
        assert!(o.ok);
        assert_eq!(o.evidence.len(), 1);
        assert_eq!(o.evidence[0].summary, "Company raised FY guidance", "the summary is the first line");
        assert!(o.evidence[0].id.is_empty(), "a bus must not guess evidence ids");
        assert!(o.evidence[0].body.len() <= 20_000, "bodies are capped even in the store");
        assert!(o.did.is_some());
    }

    /// A failure produces no evidence and keeps its reason — a failed call must not enter the capsule
    /// as if it had found something.
    #[test]
    fn a_failed_call_yields_a_reason_and_no_evidence() {
        let o = Bare.normalize("fetch", &json!({}), "502 Bad Gateway", false);
        assert!(!o.ok);
        assert!(o.evidence.is_empty(), "a failure is not a source");
        assert_eq!(o.error.as_deref(), Some("502 Bad Gateway"));
        assert!(o.did.is_none(), "nothing was accomplished");
    }

    /// An absent grounding seam must read as UNVERIFIED, never as verified-by-default.
    #[tokio::test]
    async fn an_absent_grounding_seam_is_not_a_pass() {
        assert!(Bare.ground("q", "e").await.is_none());
    }
}
