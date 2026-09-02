//! L4-0 — the spend ledger sink: the engine's own recorder receives the pool's typed record and
//! writes one `inference_call` decision event per logical request. No prompt, no reply, no user
//! datum ever reaches this file: the callsite is the static string the code authored.
use mind_inference::{CallOutcome, InferenceCall, InferenceLedger};
use mind_observability::{DecisionEvent, DecisionLog, INFERENCE_LEDGER_VERSION};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub struct SpendSink {
    log: Arc<DecisionLog>,
    /// Explicit process identity on every row — the process-lifetime oracle selects by it,
    /// never by timestamp.
    process_start_ms: u64,
    seq: AtomicU64,
}

impl SpendSink {
    pub fn new(log: Arc<DecisionLog>, process_start_ms: u64) -> Self {
        Self {
            log,
            process_start_ms,
            seq: AtomicU64::new(0),
        }
    }
}

/// The event exactly as the ledger stores it; `parse_inference_call` is its inverse.
pub fn spend_event(call: &InferenceCall, process_start_ms: u64, seq: u64) -> DecisionEvent {
    let mut ev = DecisionEvent::new("inference", "inference_call");
    ev.event_id = Some(format!("inference-call:{process_start_ms}:{seq}"));
    ev.goal_id = Some(format!("process:{process_start_ms}"));
    // The callsite rides in `trigger` (160-char budget); `actor` is a 32-char field and would
    // truncate a module path. A code-authored identity must land whole or not at all.
    ev.actor = Some("spend".into());
    ev.trigger = Some(format!(
        "callsite:{}",
        if call.callsite.trim().is_empty() {
            "unattributed"
        } else {
            call.callsite
        }
    ));
    ev.lane = Some(call.scope.as_str().into());
    ev.model_route = Some(call.route.clone());
    ev.chosen = match call.outcome {
        CallOutcome::Served => call.served_by.clone(),
        _ => None,
    };
    ev.verdict = Some(call.outcome.as_str().into());
    ev.outcome = Some(format!("attempts:{}", call.attempts));
    ev.latency_ms = Some(call.latency_ms);
    ev.subject = Some(if call.streaming { "stream" } else { "plain" }.into());
    ev.object_id = call.opportunity.clone();
    // Tokens: absent on a v1 row until a backend contract reports them with provenance.
    ev.model_calls = None;
    ev.evaluator_id = Some(INFERENCE_LEDGER_VERSION.into());
    ev
}

impl InferenceLedger for SpendSink {
    fn record_call(&self, call: InferenceCall) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.log
            .record(spend_event(&call, self.process_start_ms, seq));
    }
}
