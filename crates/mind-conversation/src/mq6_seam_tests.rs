//! E.MQ6: the two-stage seam, over a scripted backend. Stage 1 decides whether the model is
//! called at all; stage 2 can only confirm the one claim stage 1 named, or abstain.

use crate::ConversationEngine;
use mind_inference::{InferencePool, SequencedLLM};
use std::sync::Arc;
use yantrik_ml::LLMBackend;

fn pool(reply: &str) -> (InferencePool, Arc<SequencedLLM>) {
    let llm = Arc::new(SequencedLLM::new(vec![reply]));
    (
        InferencePool::new(llm.clone() as Arc<dyn LLMBackend>, 1),
        llm,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_candidate_means_no_model_call() {
    let (p, llm) = pool("CONFIRM");
    let got =
        ConversationEngine::route_claim_two_stage_with(&p, "What is a hash-chained log?").await;
    assert_eq!(got, (None, None, None));
    assert_eq!(
        llm.call_count(),
        0,
        "stage 2 never runs without a singleton"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_confirmed_singleton_routes_to_that_claim_and_no_other() {
    let (p, llm) = pool("CONFIRM");
    let q = "Could you restart yourself if you got stuck?";
    let (cand, raw, routed) = ConversationEngine::route_claim_two_stage_with(&p, q).await;
    assert_eq!(cand, Some("self-restart"));
    assert_eq!(raw.as_deref(), Some("CONFIRM"));
    assert_eq!(routed, Some("self-restart"));
    assert_eq!(llm.call_count(), 1, "exactly one model call per candidate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_abstaining_or_malformed_model_cannot_route() {
    for reply in ["ABSTAIN", "real-money", "CONFIRM real-money", "", "yes"] {
        let (p, _llm) = pool(reply);
        let q = "Could you restart yourself if you got stuck?";
        let (cand, raw, routed) = ConversationEngine::route_claim_two_stage_with(&p, q).await;
        assert_eq!(cand, Some("self-restart"), "{reply:?}");
        assert_eq!(raw.as_deref(), Some(reply), "{reply:?}");
        assert_eq!(
            routed, None,
            "{reply:?}: the model can confirm or abstain, never name a claim"
        );
    }
}
