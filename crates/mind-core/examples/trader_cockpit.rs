//! Deterministic operator smoke test for the paper-only trading desks.
//!
//! Runs against in-memory Mind state and scripted inference. It may read market data when
//! credentials are present, but every broker path remains compile-time pinned to Alpaca paper.

use std::sync::Arc;

use mind_inference::{InferencePool, ScriptedLLM};
use mind_memory::MemoryHandle;
use yantrik_ml::LLMBackend;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let memory = MemoryHandle::spawn(":memory:", 64)
        .map_err(|error| anyhow::anyhow!("memory init: {error:?}"))?;
    let inference = Arc::new(ScriptedLLM::new("unused")) as Arc<dyn LLMBackend>;
    let engine = mind_core::engine(&memory, InferencePool::new(inference, 1));

    for (label, report) in [
        ("crypto initial", engine.crypto_trader_cmd("status").await),
        ("crypto shadow on", engine.crypto_trader_cmd("shadow").await),
        ("crypto shadow scan", engine.crypto_trader_cmd("run").await),
        (
            "crypto live refusal",
            engine.crypto_trader_cmd("live").await,
        ),
        ("crypto paper on", engine.crypto_trader_cmd("paper").await),
        (
            "crypto paper scan",
            engine.crypto_trader_cmd("run paper").await,
        ),
        ("equities shadow on", engine.day_trader_cmd("shadow").await),
        ("crypto isolated", engine.crypto_trader_cmd("status").await),
        ("equities paper on", engine.day_trader_cmd("paper").await),
        (
            "equities paper scan",
            engine.day_trader_cmd("run paper").await,
        ),
        ("equities live refusal", engine.day_trader_cmd("live").await),
    ] {
        println!("\n=== {label} ===\n{report}");
    }

    Ok(())
}
