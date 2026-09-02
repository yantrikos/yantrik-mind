//! E.CB2-F — the page path's one critic + repair round.
//!
//! The measured failure this exists for (E.CB2 leg 1, 2026-09-02): the author step returned 968
//! characters with no `<html>`/`<body>`, `publish_page` refused — correctly — and the chain had
//! nothing left to try, so a 31-second job was reported as a failure. The repair round spends one
//! more model call on exactly that case and nothing on any other.

use mind_recipes::{Condition, RecipeStep};

fn kinds(r: &mind_recipes::Recipe) -> Vec<&'static str> {
    r.steps
        .iter()
        .map(|s| match s {
            RecipeStep::Tool { tool_name, .. } if tool_name == "research" => "research",
            RecipeStep::Tool { tool_name, .. } if tool_name == "publish_page" => "publish_page",
            RecipeStep::Tool { .. } => "other-tool",
            RecipeStep::Think { .. } => "think",
            RecipeStep::JumpIf { .. } => "jumpif",
            RecipeStep::Notify { .. } => "notify",
            _ => "other",
        })
        .collect()
}

#[test]
fn the_repair_round_sits_between_the_author_and_the_publish_step() {
    let r = crate::delegate::page_recipe("Portfolio", "a portfolio site", None);
    assert_eq!(
        kinds(&r),
        vec![
            "research",
            "think",
            "jumpif",
            "think",
            "publish_page",
            "notify"
        ],
        "the chain's shape changed; the jump target below is an index into it"
    );
}

#[test]
fn a_draft_that_is_already_a_document_skips_the_repair_call() {
    // The whole cost argument rests on this: an ordinary run must be byte-identical to before, so
    // the jump has to land ON the publish step — not before it (the repair would run anyway) and
    // not after it (the page would never be published).
    let r = crate::delegate::page_recipe("Portfolio", "a portfolio site", None);
    let (condition, target) = match &r.steps[2] {
        RecipeStep::JumpIf {
            condition,
            target_step,
        } => (condition.clone(), *target_step),
        other => panic!("step 2 is not the guard: {other:?}"),
    };
    assert_eq!(target, 4, "the guard must jump to the publish step");
    assert!(
        matches!(&r.steps[target], RecipeStep::Tool { tool_name, .. } if tool_name == "publish_page"),
        "the jump target is not publish_page"
    );
    assert!(
        matches!(&condition, Condition::VarIsPublishableDocument { var } if var == "page"),
        "the guard must test the DOCUMENT, not merely that a var exists: {condition:?}"
    );

    // And the guard must actually discriminate the observed failure from a real page.
    let mut vars = std::collections::HashMap::new();
    vars.insert(
        "page".to_string(),
        serde_json::json!("<!doctype html><html><body><h1>x</h1></body></html>"),
    );
    assert!(
        condition.evaluate(&vars),
        "a real page must skip the repair"
    );
    vars.insert(
        "page".to_string(),
        serde_json::json!("Here is a portfolio page with a hero and four project cards."),
    );
    assert!(
        !condition.evaluate(&vars),
        "prose about a page must NOT skip the repair — this is the leg-1 failure"
    );
}

#[test]
fn the_repair_step_sees_the_draft_and_asks_for_the_document_itself() {
    let r = crate::delegate::page_recipe("Portfolio", "a portfolio site for a ceramicist", None);
    match &r.steps[3] {
        RecipeStep::Think {
            prompt,
            store_as,
            max_tokens,
            think,
            ..
        } => {
            assert!(
                prompt.contains("{{page}}"),
                "a repair prompt that cannot see the draft is just a second guess"
            );
            assert!(
                prompt.contains("a portfolio site for a ceramicist"),
                "the repair must restate the brief; the draft alone does not carry it"
            );
            assert!(
                prompt.contains("<!doctype html>") && prompt.contains("</html>"),
                "the repair must say exactly where the document starts and ends"
            );
            assert_eq!(store_as, "page", "the repair must replace the draft");
            assert!(
                max_tokens.unwrap_or(0) >= 8000,
                "the repair needs the same document budget as the author step, got {max_tokens:?}"
            );
            assert_eq!(
                *think,
                Some(false),
                "thinking on this step is what produced the non-document in the first place"
            );
        }
        other => panic!("step 3 is not the repair step: {other:?}"),
    }
}

#[test]
fn the_repair_round_is_exactly_one_and_publishes_through_the_same_refusals() {
    // Two failure modes this rules out: a loop (a jump backwards would re-author forever) and a
    // repair step that publishes on its own, bypassing publish_page's document checks.
    let r = crate::delegate::page_recipe("P", "a page", None);
    let backward_jumps = r
        .steps
        .iter()
        .enumerate()
        .filter(|(i, s)| matches!(s, RecipeStep::JumpIf { target_step, .. } if target_step <= i))
        .count();
    assert_eq!(backward_jumps, 0, "the page chain must not loop");
    assert_eq!(
        r.steps
            .iter()
            .filter(|s| matches!(s, RecipeStep::Think { .. }))
            .count(),
        2,
        "one author step and one repair step, no more"
    );
    assert_eq!(
        r.steps
            .iter()
            .filter(
                |s| matches!(s, RecipeStep::Tool { tool_name, .. } if tool_name == "publish_page")
            )
            .count(),
        1,
        "exactly one publish, so every document goes through the same refusals"
    );
}

// ── The branch, executed ──────────────────────────────────────────────────────────────────────
//
// The shape tests above pin the wiring; these two run the recipe through the real RecipeEngine and
// watch what actually happens: a publishable draft must reach publish_page WITHOUT a second model
// call, and a prose draft must cost exactly one repair call and then reach the SAME publish tool.

use mind_inference::{InferencePool, SequencedLLM};
use mind_recipes::{RecipeEngine, RecipeHost};
use std::sync::{Arc, Mutex};
use yantrik_ml::LLMBackend;

const PAGE: &str =
    "<!doctype html><html><head><title>P</title></head><body><h1>P</h1></body></html>";

struct RecordingHost {
    calls: Mutex<Vec<(String, serde_json::Value)>>,
}

#[async_trait::async_trait]
impl RecipeHost for RecordingHost {
    async fn call_tool(&self, tool: &str, args: &serde_json::Value) -> anyhow::Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push((tool.to_string(), args.clone()));
        // publish_page here REFUSES exactly as the real tool does, through the same shared
        // predicate — a host that published anything would let a broken chain look healthy.
        if tool == "publish_page" {
            let html = args
                .get("html")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if !mind_recipes::is_publishable_document(html) {
                anyhow::bail!(
                    "publish_page needs a real HTML document in 'html' (got {} chars)",
                    html.len()
                );
            }
            return Ok("http://192.168.4.90:8088/p.html".to_string());
        }
        Ok("(none)".to_string())
    }
}

async fn run_page_chain(
    replies: Vec<&str>,
) -> (usize, Vec<(String, serde_json::Value)>, bool, String) {
    let llm = Arc::new(SequencedLLM::new(replies));
    let host = Arc::new(RecordingHost {
        calls: Mutex::new(Vec::new()),
    });
    let engine = RecipeEngine::new(
        InferencePool::new(llm.clone() as Arc<dyn LLMBackend>, 1),
        host.clone(),
        "You are a test.",
    );
    let out = engine
        .run_with(
            &crate::delegate::page_recipe("P", "a portfolio", None),
            std::collections::HashMap::new(),
        )
        .await;
    let url = out
        .vars
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let calls = host.calls.lock().unwrap().clone();
    (llm.call_count(), calls, out.ok, url)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_publishable_draft_costs_one_model_call_and_publishes_verbatim() {
    let (model_calls, calls, ok, url) = run_page_chain(vec![PAGE]).await;
    assert_eq!(
        model_calls, 1,
        "an ordinary run must not spend the repair call"
    );
    let published: Vec<&(String, serde_json::Value)> =
        calls.iter().filter(|(t, _)| t == "publish_page").collect();
    assert_eq!(published.len(), 1, "exactly one publish");
    assert_eq!(
        published[0].1.get("html").and_then(|v| v.as_str()),
        Some(PAGE),
        "the author's document must reach the tool unchanged"
    );
    assert!(ok && url.starts_with("http"), "the chain delivers a URL");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_prose_draft_costs_exactly_one_repair_and_then_publishes_the_repair() {
    // Reply 1 is the measured leg-1 failure: prose where a document was asked for.
    let (model_calls, calls, ok, url) = run_page_chain(vec![
        "Here is a portfolio page with a hero and four project cards.",
        PAGE,
    ])
    .await;
    assert_eq!(
        model_calls, 2,
        "one author call and exactly one repair call — never a loop"
    );
    let published: Vec<&(String, serde_json::Value)> =
        calls.iter().filter(|(t, _)| t == "publish_page").collect();
    assert_eq!(published.len(), 1, "still exactly one publish");
    assert_eq!(
        published[0].1.get("html").and_then(|v| v.as_str()),
        Some(PAGE),
        "the REPAIRED document is what gets published"
    );
    assert!(ok && url.starts_with("http"), "the chain recovers a URL");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_repair_that_also_fails_still_reaches_the_tool_and_the_chain_reports_failure() {
    // The refusal must stay the refusal: two bad drafts publish nothing, and the run is not ok.
    let (model_calls, calls, ok, url) = run_page_chain(vec!["still prose", "prose again"]).await;
    assert_eq!(model_calls, 2, "one repair, not two");
    assert_eq!(
        calls.iter().filter(|(t, _)| t == "publish_page").count(),
        1,
        "the tool is still the only gate; it is called once and refuses"
    );
    assert!(url.is_empty(), "no URL from a failed page");
    assert!(
        !ok,
        "the chain reports the failure instead of announcing a page"
    );
}

#[test]
fn the_repair_step_carries_the_mounted_pack_rules_like_the_author_step() {
    // page_recipe threads pack rules into the author step because a page built with a pack mounted
    // once ignored it entirely. A repair that dropped them would reintroduce exactly that bug on the
    // path that produces the FINAL document.
    let r = crate::delegate::page_recipe("P", "a portfolio", Some("Spend boldness once."));
    for i in [1usize, 3] {
        match &r.steps[i] {
            RecipeStep::Think { prompt, .. } => assert!(
                prompt.contains("Spend boldness once.") && prompt.contains("HOUSE RULES"),
                "step {i} lost the mounted pack rules"
            ),
            other => panic!("step {i} is not a Think step: {other:?}"),
        }
    }
    let without = crate::delegate::page_recipe("P", "a portfolio", None);
    match &without.steps[3] {
        RecipeStep::Think { prompt, .. } => assert!(
            !prompt.contains("HOUSE RULES"),
            "with no pack mounted the repair prompt must carry no rules block"
        ),
        other => panic!("step 3 is not the repair step: {other:?}"),
    }
}
