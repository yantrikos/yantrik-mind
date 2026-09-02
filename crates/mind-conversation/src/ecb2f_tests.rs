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
        matches!(&condition, Condition::VarIsHtmlDocument { var } if var == "page"),
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
