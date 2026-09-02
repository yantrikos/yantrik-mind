//! E.F2: the operator door `ym horizon <delay> :: <goal> assuming <key>=<value>` through the
//! real CLI dispatch — the parser, the persisted assumption and budgets, the reserved binding,
//! and the no-persist refusal of malformed syntax. The no-suffix form is pinned by the existing
//! operator fixture; this module never edits that file.

use crate::*;
use mind_inference::ScriptedLLM;
use mind_memory::MemoryHandle;
use mind_recipes::{HorizonJobKind, RecipeStore};
use yantrik_ml::LLMBackend;

struct InboxHost;
#[async_trait::async_trait]
impl RecipeHost for InboxHost {
    async fn call_tool(&self, tool: &str, _args: &serde_json::Value) -> anyhow::Result<String> {
        if tool == "inbox" {
            Ok("one message needs review".into())
        } else {
            anyhow::bail!("unexpected tool")
        }
    }
}

fn harness() -> (ConversationEngine, Arc<RecipeStore>) {
    let authored = r#"[
        {"Tool":{"tool_name":"inbox","args":{"limit":2},"store_as":"fresh"}},
        {"Think":{"prompt":"Summarize {{fresh}}","store_as":"answer"}}
    ]"#;
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(
        Arc::new(ScriptedLLM::new(authored)) as Arc<dyn LLMBackend>,
        1,
    );
    let store = Arc::new(RecipeStore::open(":memory:").unwrap());
    let recipes = Arc::new(
        RecipeEngine::new(pool.clone(), Arc::new(InboxHost), "JARVIS").with_store(store.clone()),
    );
    let conv = ConversationEngine::new(mem, pool, "JARVIS").with_recipes(recipes);
    (conv, store)
}

fn goal_id_in(reply: &str) -> String {
    reply
        .split_once('[')
        .and_then(|(_, tail)| tail.split_once(']'))
        .map(|(goal_id, _)| goal_id.to_string())
        .unwrap_or_else(|| panic!("no goal id in reply: {reply}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_assuming_door_declares_the_assumption_and_binds_the_reserved_observation() {
    let (conv, store) = harness();
    let reply = conv
        .cli_dispatch(
            "horizon 1m :: Check my inbox and summarize it assuming inbox_state = no messages",
            &mind_types::AccessContext::operator_audit(),
        )
        .await;
    assert!(
        reply.contains("Long-horizon goal scheduled")
            && reply.contains("observing your declared assumption")
            && reply.contains("replans once"),
        "{reply}"
    );
    let goal_id = goal_id_in(&reply);
    let now = ConversationEngine::now_ms();
    let run = store
        .load_horizon(&goal_id, now)
        .unwrap()
        .expect("the declared goal is durable");
    assert_eq!(run.budget.max_actions, 2);
    assert_eq!(run.budget.max_replans, 1);
    assert_eq!(
        run.assumptions.get("inbox_state").map(|a| a.value.as_str()),
        Some("no messages"),
        "the value is trimmed and declared verbatim"
    );
    let (job, status, _) = store.queued_horizon_job(&goal_id).unwrap().unwrap();
    assert_eq!(status, "pending");
    assert_eq!(job.kind, HorizonJobKind::Segment);
    let var = mind_recipes::reserved_observation_var("inbox_state");
    assert_eq!(job.assumption_vars.get("inbox_state"), Some(&var));
    match &job.recipe.steps[0] {
        mind_recipes::RecipeStep::Tool { store_as, .. } => assert_eq!(store_as, &var),
        other => panic!("{other:?}"),
    }
    match &job.recipe.steps[1] {
        mind_recipes::RecipeStep::Think { prompt, .. } => {
            assert!(prompt.contains("{{__assume_inbox_state}}"), "{prompt}")
        }
        other => panic!("{other:?}"),
    }
    // The receipt chain carries an opaque id, never the key or the value.
    for receipt in store.load_horizon_lifecycle(&goal_id).unwrap() {
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(!json.contains("inbox_state") && !json.contains("no messages"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_assuming_syntax_is_refused_without_persisting_anything() {
    let (conv, store) = harness();
    let before = ConversationEngine::now_ms();
    for line in [
        // no `=`
        "horizon 1m :: Check my inbox and summarize it assuming inbox_state",
        // uppercase / space in the key
        "horizon 1m :: Check my inbox and summarize it assuming Inbox State=quiet",
        // empty value
        "horizon 1m :: Check my inbox and summarize it assuming inbox_state=",
    ] {
        let reply = conv
            .cli_dispatch(line, &mind_types::AccessContext::operator_audit())
            .await;
        assert!(
            !reply.contains("Long-horizon goal scheduled"),
            "{line} -> {reply}"
        );
        assert!(
            reply.contains("assuming key=value") || reply.contains("did not schedule"),
            "{line} -> {reply}"
        );
    }
    let after = ConversationEngine::now_ms();
    assert!(store.list_horizons(after).unwrap().is_empty());
    let _ = before;
}
