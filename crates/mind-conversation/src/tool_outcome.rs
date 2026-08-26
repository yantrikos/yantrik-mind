//! What actually happened when a tool ran.
//!
//! # Why this exists
//!
//! The agent loop used to decide this with one boolean built from a substring list:
//!
//! ```ignore
//! let failure_marker = ["error", "couldn't", "not configured", "nothing", "no results", …]
//!     .iter().any(|m| obs_lc.contains(m));
//! let tool_ok = obs.chars().count() > 10 && !(obs.trim_start().starts_with('(') && failure_marker);
//! ```
//!
//! That boolean feeds `record_tool_outcome`, which is the mind's *measured self-knowledge about its
//! own tools* — so getting it wrong does not just misreport one step, it teaches the mind a false
//! belief about what it is good at. Four things were wrong with it:
//!
//! 1. **"no results" was a failure.** A search that ran perfectly and found nothing is the tool
//!    WORKING. Recording it as a failure means a healthy tool looks flaky because the world was
//!    empty, and the bandit learns to avoid it.
//! 2. **"not configured" was a failure.** A missing credential is a capability gap, not
//!    unreliability. Averaging it into a success rate hides a fact that should be surfaced once and
//!    fixed, behind a number that says "this tool is unreliable".
//! 3. **Anything under 10 characters was a failure.** `42` is a correct answer.
//! 4. **Markers were only consulted when the text began with `(`.** A real error that did not happen
//!    to be wrapped in parentheses was recorded as a SUCCESS.
//!
//! Hermes has the same idea in `agent/error_classifier.py`, written — in its own words — to replace
//! "scattered inline string-matching" with a priority-ordered pipeline that yields a recovery action.
//! It classifies structured API errors; the mind only has an observation string, so the shape here is
//! the same but the evidence is weaker, and that difference is handled explicitly below.
//!
//! # The one rule that makes string matching defensible
//!
//! **Markers are only consulted on STATUS-SHAPED observations.** A 5 KB web page that contains the
//! word "error" is a successful fetch of a page about errors. A tool that answers
//! `(github not configured)` is a status line. Length and shape decide whether the words are ABOUT
//! the call or merely IN the content — without that rule, every substring list eventually
//! misclassifies real content, which is how the old one failed.
//!
//! # Three kinds of success (design contract — do not collapse them)
//!
//! The five-way outcome grades INVOCATION quality. It must never harden into the definition of
//! capability, because the questions hide inside "did it work" form a LADDER, each rung needing
//! more evidence than the last:
//!
//! ```text
//! execution_success      → the tool ran and honored its contract   (Outcome, today)
//!       ↓
//! semantic_success       → the output carried substance for the ask (Empty vs Ok; recorded)
//!       ↓
//! evidence_utilized      → a finding actually CITED the output      (tool_goal_graded, today —
//!                                                                    a PROXY: citing is not causing)
//!       ↓
//! goal_contribution      → the cited output materially advanced the objective
//!                          (needs counterfactual/shadow comparison — pending)
//!       ↓
//! goal_outcome           → the user's objective itself succeeded    (ExpectedOutcomes — pending)
//! ```
//!
//! Terminology discipline: today's `evidence_used` verdict is rung THREE. Letting it masquerade
//! as causal contribution teaches "my search gets cited, therefore my search causes goals" —
//! exactly the proxy optimization this architecture exists to prevent. Rung 4 arrives only via
//! the policy-disagreement cohort / shadow comparison; rung 5 only where ExpectedOutcomes exist.
//!
//! Likewise, failure sources separate rather than average. `Unavailable` and `Denied` are
//! excluded from reliability because they answer DIFFERENT questions — and each is still
//! evidence:
//!
//! ```text
//! P(action succeeds) ≈ P(available | context)
//!                    × P(permitted | available)   ← Denied feeds this
//!                    × P(success | permitted, available)   ← Ok/Empty/Failed feed this
//! ```
//!
//! Today the availability term is handled structurally (`ready_capabilities` refuses absent
//! clients before planning; the compiler turns gaps into refusals), and Denied/Unavailable
//! events accumulate in the flight recorder for context-conditioned rates. Do not fold them
//! back into the success rate to "use more data" — that re-teaches exactly the lie this module
//! replaced.

/// What a tool call actually did. Deliberately not a boolean: the four non-Ok cases call for
/// different responses and must not be averaged together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Produced usable information.
    Ok,
    /// Ran correctly and found nothing. The tool worked; the world was empty.
    Empty,
    /// Cannot run here — no credential, not configured, not set up. A capability gap.
    Unavailable,
    /// Refused by the harm-gate or the egress broker. This is the safety machinery WORKING.
    Denied,
    /// Genuinely broke: an error, a timeout, an unreachable host, a panic.
    Failed,
    /// The call could not be made as asked: the model's arguments did not fit the tool (a bare
    /// number or boolean where every field is a string). The PLANNER's failure, not the tool's —
    /// the tool never ran, so this says nothing about whether it works. Found live on 2026-08-26:
    /// `run_skill {"name":328}` came back "(no saved skill named '')", which the classifier read as
    /// Ok and CREDITED to run_skill's reliability (ARCH-6 P.2b, Codex's review).
    Malformed,
}

/// What the loop should do next. Advisory — the controller owns the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    /// Use the result and carry on.
    Proceed,
    /// The call was fine but unproductive; a different query or source may help.
    Vary,
    /// This tool cannot help on this box. Do not retry it — pick another route.
    Reroute,
    /// The person needs to know: a missing credential, or a refusal they may want to authorise.
    Tell,
    /// Transient. One retry is reasonable.
    Retry,
}

impl Outcome {
    /// Classify an observation. `tool` is accepted for future per-tool rules and to keep call sites
    /// stable; the current rules are tool-agnostic on purpose, because a per-tool table would
    /// reintroduce exactly the maintenance problem this replaces.
    pub fn classify(tool: &str, obs: &str) -> Self {
        let trimmed = obs.trim();
        if trimmed.is_empty() {
            return Outcome::Empty;
        }
        // CONTENT, not a status line: the words below are only meaningful when the observation is
        // reporting on itself. Past this length a match is almost certainly the subject matter.
        // Parenthetical replies stay status-shaped at any length — that is the mind's own convention
        // for "this is the runtime talking, not the data".
        const STATUS_MAX: usize = 240;
        let parenthetical = trimmed.starts_with('(');
        if trimmed.chars().count() > STATUS_MAX && !parenthetical {
            return Outcome::Ok;
        }

        let low = trimmed.to_lowercase();
        let has = |ms: &[&str]| ms.iter().any(|m| low.contains(m));

        // The runtime's own marker for a call refused at the argument boundary — checked first,
        // because the message goes on to say what the tool "takes", and those words must not be
        // read as the tool reporting on itself.
        if low.starts_with("(malformed call") {
            return Outcome::Malformed;
        }
        // An MCP server's own verdict on the model's arguments: JSON-RPC -32602 / "Invalid params"
        // is the planner's failure by the protocol's definition, not the tool's.
        if has(&["-32602", "invalid params"]) {
            return Outcome::Malformed;
        }
        // run_skill's own not-found line is tool-SPECIFIC: with an empty name it is a malformed call
        // that slipped the boundary; with a real name it is the tool honestly finding nothing.
        if tool == "run_skill" && low.contains("no saved skill named") {
            return if low.contains("named ''") { Outcome::Malformed } else { Outcome::Empty };
        }

        // PRIORITY ORDER, most specific first. A refusal often also contains the word "cannot", and a
        // missing credential often contains "unable" — whichever arm is checked first wins, so the
        // order encodes which reading is correct.
        // A bare "refused" is NOT enough: "connection refused" is a network failure, and the first
        // version of this arm swallowed it. Every marker here carries gate context, so the word has to
        // be doing gate work to match. These are the strings the mind actually emits — "BLOCKED by
        // harm-gate: …", "PROPOSED — needs the user's confirmation; NOT executed", and the egress
        // broker's "(I couldn't compose a safe outbound request …)".
        if has(&["harm-gate", "blocked by", "refused by", "not permitted", "needs your confirmation",
                 "needs the user's confirmation", "requires confirmation", "denied by",
                 "safe outbound request", "not executed"]) {
            return Outcome::Denied;
        }
        if has(&["not configured", "isn't configured", "is not configured", "no mailbox", "not set up",
                 "no credential", "missing credential", "no api key", "not available on this box",
                 "no such tool", "unknown tool", "not enabled"]) {
            return Outcome::Unavailable;
        }
        if has(&["error", "failed", "panic", "timed out", "timeout", "couldn't reach", "could not reach",
                 "unreachable", "connection refused", "exception", "traceback", "crashed"]) {
            return Outcome::Failed;
        }
        // `no tool or saved skill matches` is `discover_tools` working correctly on an empty library —
        // caught only by testing against observations captured from the running box. Every fixture I
        // invented passed; this one did not.
        if has(&["no results", "nothing found", "no matches", "found nothing", "0 results",
                 "no entries", "empty", "none found", "no tool or saved skill"]) {
            return Outcome::Empty;
        }
        Outcome::Ok
    }

    /// What this outcome should teach the mind about the tool's reliability.
    ///
    /// `None` means DO NOT RECORD, and it is the important case. A tool that is not configured tells
    /// you nothing about whether it works — recording it either way is a lie: `false` invents
    /// flakiness, `true` inflates a success rate for a tool that never ran. A gate refusal is the
    /// same: the gate's decision is not the tool's performance.
    pub fn counts_toward_reliability(self) -> Option<bool> {
        match self {
            // The tool ran and did its job — an honest empty answer included.
            Outcome::Ok | Outcome::Empty => Some(true),
            Outcome::Failed => Some(false),
            // The tool never ran (not configured / refused / never asked properly): nothing here
            // is evidence about the tool, in either direction.
            Outcome::Unavailable | Outcome::Denied | Outcome::Malformed => None,
        }
    }

    pub fn recovery(self) -> Recovery {
        match self {
            Outcome::Ok => Recovery::Proceed,
            Outcome::Empty => Recovery::Vary,
            Outcome::Unavailable => Recovery::Reroute,
            Outcome::Denied => Recovery::Tell,
            Outcome::Failed => Recovery::Retry,
            // The call was wrong, not the tool: ask again, properly.
            Outcome::Malformed => Recovery::Vary,
        }
    }

    /// One word for the OPERATOR, where `note` is a sentence for the model.
    ///
    /// The step list needs to say how a call ended in the width of a badge, and it must carry the
    /// same five-way distinction the classifier already made — collapsing it to ok/failed in the UI
    /// would re-introduce, on screen, exactly the boolean this module exists to replace. "Found
    /// nothing" and "broke" look identical in a spinner and are completely different to the person
    /// reading it.
    pub fn badge(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Empty => "empty",
            Outcome::Unavailable => "unavailable",
            Outcome::Denied => "denied",
            Outcome::Failed => "failed",
            Outcome::Malformed => "malformed",
        }
    }

    /// A short label for the work log, so the model sees the DISTINCTION rather than re-deriving it
    /// from the same words the classifier just read.
    pub fn note(self) -> &'static str {
        match self {
            Outcome::Ok => "",
            Outcome::Empty => " [ran fine, found nothing — try a different query or source]",
            Outcome::Unavailable => " [not available on this box — do not retry it, use another route or say so]",
            Outcome::Denied => " [refused by the safety gate — tell the user, do not work around it]",
            Outcome::Failed => " [the tool broke — one retry is reasonable, then route around it]",
            Outcome::Malformed => " [your arguments did not fit the tool — every field is a string; call it again as documented]",
        }
    }
}

/// The argument boundary: `Some(refusal)` when the model's arguments cannot be what the tool reads.
///
/// With a contract — every catalogued tool has one, derived from the schema the model was shown —
/// a required arg missing, null or empty is a call that cannot be made; a free-text arg carrying a
/// number, a boolean, an array or an object is a call the handler would read as "" and run on
/// nothing; a declared scalar (or a `SCALAR_ARGS` name) takes a number, a boolean or a string.
/// Without a contract, only the scalar-name rule applies. MCP tools carry their own schemas and are
/// judged by their own reply (`-32602`) instead.
///
/// The refusal names the tool and the offending fields with their TYPES ONLY — never a value. A
/// number the model put in a field may be one the person typed and the model encoded (a PIN, a
/// year, a dose), and this string reaches the work log, the recorder and the model's next prompt
/// (Codex's review). It starts with the marker `classify` maps to `Outcome::Malformed`, so the call
/// is excluded from the tool's reliability instead of credited to it.
pub(crate) fn malformed_call(
    tool: &str,
    args: &serde_json::Value,
    contract: Option<&crate::tool_catalog::ArgContract>,
) -> Option<String> {
    if tool.starts_with("mcp.") {
        return None;
    }
    let refusal = |problems: Vec<String>| {
        format!("(malformed call: {tool} — {} — call it again with the fields as documented)", problems.join("; "))
    };
    let obj = match args {
        serde_json::Value::Object(o) => o,
        serde_json::Value::Null => {
            return contract
                .filter(|c| !c.required.is_empty())
                .map(|c| refusal(vec![format!("missing required {}", c.required.join(", "))]));
        }
        other => return Some(refusal(vec![format!("a bare {} instead of an object of arguments", json_type(other))])),
    };
    let mut problems: Vec<String> = Vec::new();
    let none = crate::tool_catalog::ArgContract::default();
    let c = contract.unwrap_or(&none);
    // Required fields, per FIELD, satisfied by the documented name or by an alias the handler has
    // always taken (`ArgContract::aliases`, from the catalog's ARG_ALIASES). `{}` and
    // `{"name": null}` cannot be made; neither can `run_skill {"target": …}` without a name or
    // `add_reminder {"text": …}` without a `when` (Codex's review of P.2d).
    let missing: Vec<&str> = c.required.iter().filter(|r| !c.present(obj, r)).map(|r| r.as_str()).collect();
    if !missing.is_empty() {
        problems.push(format!("missing required {}", missing.join(", ")));
    }
    for (k, v) in obj {
        if v.is_null() {
            continue; // a null beside real arguments reads as absent
        }
        // An alias is judged as the field it stands for; `deals {"max": 500}` is a budget.
        let field = c.canonical(k);
        let scalar_name = crate::tool_catalog::SCALAR_ARGS.contains(&field);
        // Free text if the schema typed it so; declared scalars are not; an UNDECLARED field is
        // judged by its name alone, like a tool with no contract.
        let free_text = match contract {
            Some(c) if c.strings.iter().any(|f| f == field) => true,
            Some(c) if c.scalars.iter().any(|f| f == field) => false,
            _ => !scalar_name,
        };
        match v {
            serde_json::Value::String(t) => {
                if free_text && c.required.iter().any(|r| r == field) && t.trim().is_empty() {
                    problems.push(format!("`{k}` is empty"));
                }
            }
            serde_json::Value::Number(_) | serde_json::Value::Bool(_) | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                if free_text {
                    problems.push(format!("`{k}`:{} where text was expected", json_type(v)));
                }
            }
            serde_json::Value::Null => {}
        }
    }
    let supplied_any = obj.values().any(|v| !v.is_null());
    if problems.is_empty() && !supplied_any && !obj.is_empty() {
        // A call whose EVERY argument is null asked for nothing (`discover_tools {"query":null}`,
        // seen live on 2026-08-26 right after P.2b shipped).
        problems.push(format!("only null arguments ({})", obj.keys().cloned().collect::<Vec<_>>().join(", ")));
    }
    if problems.is_empty() {
        None
    } else {
        Some(refusal(problems))
    }
}

fn json_type(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_search_is_the_tool_working() {
        // THE HEADLINE BUG. "no results" was a failure, so a search that ran perfectly and found
        // nothing taught the bandit that search is unreliable.
        for s in ["(no results for 'xyzzy')", "No results found.", "found nothing", "0 results"] {
            assert_eq!(Outcome::classify("web_search", s), Outcome::Empty, "{s}");
        }
        assert_eq!(Outcome::classify("web_search", "(no results)").counts_toward_reliability(), Some(true));
    }

    #[test]
    fn a_missing_credential_teaches_nothing_about_reliability() {
        for s in ["(github not configured)", "(no mailbox configured)", "(no API key set)"] {
            assert_eq!(Outcome::classify("github", s), Outcome::Unavailable, "{s}");
        }
        assert_eq!(
            Outcome::classify("github", "(github not configured)").counts_toward_reliability(),
            None,
            "recording this either way is a lie: false invents flakiness, true inflates a tool that never ran"
        );
    }

    #[test]
    fn a_gate_refusal_is_not_a_tool_failure() {
        // The real strings the mind emits, not invented ones — a fixture that does not occur in
        // production proves nothing about production.
        let real = [
            "BLOCKED by harm-gate: outbound email to an unknown recipient",
            "PROPOSED — needs the user's confirmation; NOT executed",
            "(I couldn't compose a safe outbound request for web_search without pulling in private context)",
        ];
        for s in real {
            assert_eq!(Outcome::classify("send_email", s), Outcome::Denied, "{s}");
            assert_eq!(Outcome::classify("send_email", s).counts_toward_reliability(), None, "{s}");
            assert_eq!(Outcome::classify("send_email", s).recovery(), Recovery::Tell, "{s}");
        }
    }

    #[test]
    fn a_real_break_is_a_failure() {
        for s in ["(error: connection refused)", "(couldn't reach the host)", "(timed out after 30s)"] {
            assert_eq!(Outcome::classify("fetch", s), Outcome::Failed, "{s}");
        }
        assert_eq!(Outcome::classify("fetch", "(error: boom)").counts_toward_reliability(), Some(false));
    }

    #[test]
    fn long_content_is_never_judged_by_the_words_inside_it() {
        // The rule that makes string matching defensible at all. A page ABOUT errors is a successful
        // fetch. Without this, every substring list eventually eats real content — which is how the
        // old one failed.
        let article = format!(
            "Understanding HTTP error codes. A 500 error means the server failed. {}",
            "Timeouts and connection refused messages are common. ".repeat(12)
        );
        assert!(article.len() > 240);
        assert_eq!(Outcome::classify("fetch", &article), Outcome::Ok);
    }

    #[test]
    fn a_parenthetical_stays_status_shaped_at_any_length() {
        // The mind's own convention: a leading `(` means the runtime is talking, not the data. That
        // must not be overridden by length.
        let long_status = format!("(error: {})", "the upstream returned a malformed payload; ".repeat(12));
        assert!(long_status.len() > 240);
        assert_eq!(Outcome::classify("fetch", &long_status), Outcome::Failed);
    }

    #[test]
    fn a_short_correct_answer_is_not_a_failure() {
        // The old check called anything under 10 characters a failure.
        assert_eq!(Outcome::classify("calc", "42"), Outcome::Ok);
        assert_eq!(Outcome::classify("now", "09:15"), Outcome::Ok);
        assert_eq!(Outcome::classify("calc", "42").counts_toward_reliability(), Some(true));
    }

    #[test]
    fn an_unparenthesised_error_is_still_an_error() {
        // The old check only consulted markers when the text began with `(`, so this was recorded as
        // a SUCCESS.
        assert_eq!(Outcome::classify("fetch", "Error: DNS lookup failed"), Outcome::Failed);
        assert_eq!(
            Outcome::classify("fetch", "Error: DNS lookup failed").counts_toward_reliability(),
            Some(false)
        );
    }

    #[test]
    fn priority_order_decides_when_a_message_matches_two_arms() {
        // "refused" appears in both a gate refusal and a connection error, and the first version of
        // the Denied arm matched the bare word — so "(connection refused)" was classified as the
        // safety gate working rather than as a dead host. This test caught it.
        assert_eq!(Outcome::classify("fetch", "(connection refused)"), Outcome::Failed);
        assert_eq!(Outcome::classify("fetch", "(connection refused)").recovery(), Recovery::Retry);
        // …but an explicit gate refusal still wins over the generic words it also contains.
        assert_eq!(Outcome::classify("send_email", "(refused by harm-gate)"), Outcome::Denied);
        // And an unavailable tool is not a break, even though both are "it didn't work".
        assert_eq!(Outcome::classify("github", "(github not configured)").recovery(), Recovery::Reroute);
    }

    #[test]
    fn every_outcome_has_a_recovery_and_only_failures_hurt_the_score() {
        for o in [Outcome::Ok, Outcome::Empty, Outcome::Unavailable, Outcome::Denied, Outcome::Failed] {
            let _ = o.recovery();
        }
        let hurts: Vec<_> = [Outcome::Ok, Outcome::Empty, Outcome::Unavailable, Outcome::Denied, Outcome::Failed]
            .into_iter()
            .filter(|o| o.counts_toward_reliability() == Some(false))
            .collect();
        assert_eq!(hurts, vec![Outcome::Failed], "only a genuine break may lower a tool's score");
    }
}

#[cfg(test)]
mod live_fixtures {
    use super::*;

    /// Observations captured from the running box (`journalctl -u yantrik-mind`, 3 days). Fixtures I
    /// invent test the classifier against my imagination; these test it against production.
    #[test]
    fn real_observations_from_the_box_classify_correctly() {
        let cases: &[(&str, &str, Outcome)] = &[
            ("remember", "(remembered)", Outcome::Ok),
            ("crawl", "(tool error) crawl error: page.goto: url: expected string, got undefined", Outcome::Failed),
            // The `answer` bug fixed earlier today. Unavailable is exactly right: Reroute means
            // "do not retry it", which is precisely what that loop failed to do 4 minutes running.
            ("answer", "(unknown tool: answer)", Outcome::Unavailable),
            ("discover_tools",
             "(no tool or saved skill matches — use build_capability to create one, then run_skill it)",
             Outcome::Empty),
            // A long recall result full of belief text — must never be judged by words inside it.
            ("recall",
             "- Pranab is married to Brishti. (belief 0.92) - Brishti's birthday is on July 23rd (belief 0.92) \
              - Pranab prefers to do his deep work late at night — he is NOT a morning person. (Corrects earlier \
              conflicting beliefs.) - Pranab's wife is interested in fashion, handbags, watches, and has acne problems",
             Outcome::Ok),
        ];
        for (tool, obs, want) in cases {
            assert_eq!(Outcome::classify(tool, obs), *want, "misclassified live observation: {obs}");
        }
    }

    #[test]
    fn the_reliability_ledger_sees_the_right_verdicts() {
        // What the bandit would actually learn from that sample.
        let record = |o: &str| Outcome::classify("t", o).counts_toward_reliability();
        assert_eq!(record("(remembered)"), Some(true));
        assert_eq!(record("(tool error) crawl error: page.goto"), Some(false));
        // Neither of these is evidence about reliability, and the old boolean recorded both as
        // failures — teaching the mind that a tool it never ran is unreliable.
        assert_eq!(record("(unknown tool: answer)"), None);
        assert_eq!(record("(no tool or saved skill matches — use build_capability)"), Some(true));
    }
}

#[cfg(test)]
mod malformed_tests {
    use super::*;

    const SRC: &str = "- add_holding {ticker, shares, cost?}: record a position\n- deals {query, budget?, max?}: find deals\n- hunt {act?}: scan movers\n- calc {expression}: arithmetic\n- watch_price {query, target}: alert when a price crosses a target";

    fn contracts() -> std::collections::HashMap<String, crate::tool_catalog::ArgContract> {
        crate::tool_catalog::arg_contracts(SRC)
    }

    /// P.2b/P.2d: a call the model could not make properly is its own outcome, teaches nothing
    /// about the tool, names no values, and is held to the contract the model was shown.
    #[test]
    fn a_malformed_call_is_its_own_outcome_and_teaches_nothing_about_the_tool() {
        let c = contracts();
        let rs = c.get("run_skill");
        assert!(rs.is_some(), "run_skill is a core/meta tool with a contract");
        // The two live shapes, plus the compound / missing / empty shapes Codex named.
        let refused = malformed_call("run_skill", &serde_json::json!({"name": 328, "target": 328}), rs).expect("refused");
        assert!(refused.starts_with("(malformed call: run_skill — `name`:number where text was expected"), "{refused}");
        assert!(refused.contains("`target`:number"), "{refused}");
        for bad in [
            serde_json::json!({}),
            serde_json::json!({"name": null}),
            serde_json::json!({"name": []}),
            serde_json::json!({"name": {"x": 1}}),
            serde_json::json!({"name": "  "}),
            serde_json::Value::Null,
            serde_json::json!("just a string"),
        ] {
            assert!(malformed_call("run_skill", &bad, rs).is_some(), "must refuse {bad}");
        }
        assert!(malformed_call("run_skill", &serde_json::json!({"name": "csv-clean", "target": null}), rs).is_none(), "a real name with a null optional is fine");
        let o = Outcome::classify("run_skill", &refused);
        assert_eq!(o, Outcome::Malformed);
        assert_eq!(o.counts_toward_reliability(), None, "never evidence about the tool, either way");
        assert_eq!(o.recovery(), Recovery::Vary);
        assert_eq!(o.badge(), "malformed");
        assert!(o.note().contains("call it again"));

        // PRIVACY: never a value. A sentinel PIN in an undeclared numeric field must not appear.
        let leak = malformed_call("recall", &serde_json::json!({"query": "card", "pin": 447193}), c.get("recall")).expect("refused");
        assert!(!leak.contains("447193"), "value leaked into the refusal: {leak}");
        assert!(leak.contains("`pin`:number"), "{leak}");

        // discover_tools: boolean, and nothing but null.
        assert!(malformed_call("discover_tools", &serde_json::json!({"query": true}), c.get("discover_tools")).unwrap().contains("`query`:boolean"));
        assert!(malformed_call("discover_tools", &serde_json::json!({"query": null}), c.get("discover_tools")).unwrap().contains("missing required query"));
        assert!(malformed_call("recall", &serde_json::json!({"query": "dinner", "limit": null}), c.get("recall")).is_none());

        // Contracts from the catalog: declared scalars pass, free text does not, per FIELD.
        assert!(malformed_call("add_holding", &serde_json::json!({"ticker": "AAPL", "shares": 3}), c.get("add_holding")).is_none(), "shares is a declared scalar");
        assert!(malformed_call("add_holding", &serde_json::json!({"ticker": 5, "shares": 3}), c.get("add_holding")).unwrap().contains("`ticker`:number"));
        assert!(malformed_call("deals", &serde_json::json!({"query": "headphones", "budget": 300}), c.get("deals")).is_none());
        assert!(malformed_call("deals", &serde_json::json!({"query": true}), c.get("deals")).unwrap().contains("`query`:boolean"));
        assert!(malformed_call("hunt", &serde_json::json!({"act": true}), c.get("hunt")).is_none());
        // No contract: only the scalar-name rule.
        assert!(malformed_call("some_uncatalogued", &serde_json::json!({"year": 2019}), None).is_none());
        assert!(malformed_call("some_uncatalogued", &serde_json::json!({"who": 2019}), None).unwrap().contains("`who`:number"));
        // MCP: judged by the server's own verdict, not here.
        assert!(malformed_call("mcp.fs.read", &serde_json::json!({"limit": 10}), None).is_none());
        assert_eq!(Outcome::classify("mcp.fs.read", "error: Invalid params (-32602): path must be a string"), Outcome::Malformed);

        // run_skill's not-found line is tool-specific: empty name = malformed that slipped through;
        // a real name = Empty; the same words from another tool are content.
        assert_eq!(Outcome::classify("run_skill", "(no saved skill named '')"), Outcome::Malformed);
        assert_eq!(Outcome::classify("run_skill", "(no saved skill named 'foo')"), Outcome::Empty);
        assert_eq!(Outcome::classify("recall", "the note said: no saved skill named that"), Outcome::Ok);
    }

    /// P.2e (Codex's review of P.2d): required fields are enforced per FIELD, and a handler's
    /// tolerance for a synonym is an explicit alias in the contract — never a reason to switch the
    /// rule off. An alias is typed as the field it stands for.
    #[test]
    fn required_fields_are_held_per_field_and_aliases_are_explicit() {
        let c = contracts();
        // The contract carries the handler's aliases, read from one table.
        assert_eq!(c["calc"].aliases, vec![("expression".to_string(), vec!["expr".to_string()])]);
        // A tool may declare several fields' aliases; each row is (canonical, aliases in order).
        assert_eq!(
            c["deals"].aliases,
            vec![
                ("budget".to_string(), vec!["max".to_string()]),
                ("query".to_string(), vec!["item".to_string(), "text".to_string()]),
            ]
        );
        assert_eq!(
            c["watch_price"].aliases,
            vec![
                ("target".to_string(), vec!["budget".to_string()]),
                ("query".to_string(), vec!["item".to_string()]),
            ]
        );
        assert!(c["run_skill"].aliases.is_empty(), "run_skill's handler takes no synonym: {:?}", c["run_skill"].aliases);
        // calc {"expr"} is calc {expression}: the two bounded-loop tests that caught P.2d's first cut.
        assert!(malformed_call("calc", &serde_json::json!({"expr": "6*7"}), c.get("calc")).is_none());
        assert!(malformed_call("calc", &serde_json::json!({"expression": "6*7"}), c.get("calc")).is_none());
        let r = malformed_call("calc", &serde_json::json!({"expr": 42}), c.get("calc")).expect("typed as its field");
        assert!(r.contains("`expr`:number where text was expected"), "{r}");
        assert!(malformed_call("calc", &serde_json::json!({}), c.get("calc")).unwrap().contains("missing required expression"));
        assert!(malformed_call("calc", &serde_json::json!({"expr": null}), c.get("calc")).unwrap().contains("missing required expression"));
        // A supplied value elsewhere no longer excuses a missing required field.
        let r = malformed_call("run_skill", &serde_json::json!({"target": "https://example.org/x.csv"}), c.get("run_skill")).expect("no name");
        assert!(r.contains("missing required name"), "{r}");
        assert!(!r.contains("example.org"), "never a value: {r}");
        let r = malformed_call("add_reminder", &serde_json::json!({"text": "call mum"}), c.get("add_reminder")).expect("no when");
        assert!(r.contains("missing required when"), "{r}");
        assert!(malformed_call("add_reminder", &serde_json::json!({"text": "call mum", "when": "tomorrow 9am"}), c.get("add_reminder")).is_none());
        let r = malformed_call("watch_price", &serde_json::json!({"query": "rtx 4070"}), c.get("watch_price")).expect("no target");
        assert!(r.contains("missing required target"), "{r}");
        // The alias satisfies the requirement AND inherits the field's typing: a number is a budget.
        assert!(malformed_call("watch_price", &serde_json::json!({"query": "rtx 4070", "budget": 450}), c.get("watch_price")).is_none());
        assert!(malformed_call("deals", &serde_json::json!({"query": "headphones", "max": 300}), c.get("deals")).is_none(), "max is deals' budget");
        assert!(malformed_call("deals", &serde_json::json!({"query": "headphones", "max": "300"}), c.get("deals")).is_none());
    }

    /// P.2f (Codex's review of P.2e): the normalizer unwraps a list only when every element is
    /// TEXT. Stringifying anything else laundered a malformed call into a valid one — and carried
    /// the value into the record the refusal exists to keep out of it.
    #[test]
    fn only_a_list_of_text_is_unwrapped_by_the_normalizer() {
        let n = crate::normalize_tool_args;
        // The documented shapes still work.
        assert_eq!(n(serde_json::json!({"name": [{"type": "text", "content": "csv-clean"}]})), serde_json::json!({"name": "csv-clean"}));
        assert_eq!(n(serde_json::json!({"name": ["csv", "-clean"]})), serde_json::json!({"name": "csv-clean"}), "a split string is one string");
        assert_eq!(n(serde_json::json!({"name": [{"text": "a"}, "b"]})), serde_json::json!({"name": "ab"}));
        // A PIN-shaped number in a list is not a name.
        assert_eq!(n(serde_json::json!({"name": [447193]})), serde_json::json!({"name": [447193]}), "a number must stay an array");
        assert_eq!(n(serde_json::json!({"name": [{"x": 1}]})), serde_json::json!({"name": [{"x": 1}]}), "an arbitrary object must stay an array");
        assert_eq!(n(serde_json::json!({"name": ["ok", 447193]})), serde_json::json!({"name": ["ok", 447193]}), "one bad element preserves the list");
        assert_eq!(n(serde_json::json!({"name": [{"type": "text", "content": 447193}]})), serde_json::json!({"name": [{"type": "text", "content": 447193}]}), "a block wrapping a number is not text");
        // ...and the boundary refuses every preserved one, naming the type and never the value.
        let c = contracts();
        for bad in [
            serde_json::json!({"name": [447193]}),
            serde_json::json!({"name": [{"x": 1}]}),
            serde_json::json!({"name": ["ok", 447193]}),
            serde_json::json!({"name": [{"type": "text", "content": 447193}]}),
        ] {
            let refusal = malformed_call("run_skill", &n(bad.clone()), c.get("run_skill")).unwrap_or_else(|| panic!("must refuse {bad}"));
            assert!(refusal.contains("`name`:array"), "{refusal}");
            assert!(!refusal.contains("447193"), "the value reached the refusal: {refusal}");
        }
        assert!(malformed_call("run_skill", &n(serde_json::json!({"name": [{"type": "text", "content": "csv-clean"}]})), c.get("run_skill")).is_none());
    }
}
