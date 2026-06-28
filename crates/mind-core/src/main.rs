//! yantrik-mind REPL — talk to the companion and watch the typed memory ground its replies.
//!
//! Backend is chosen at runtime: NanoGPT (if `NANOGPT_KEY` is set) → claude CLI (if `claude` is on
//! PATH) → a scripted fallback so it always runs. Memory persists if you set `YM_DB=<path>`.

use std::io::{BufRead, Write};
use std::sync::Arc;

use mind_inference::{InferencePool, ScriptedLLM};
use mind_memory::MemoryHandle;
use yantrik_ml::LLMBackend;

fn claude_available() -> bool {
    std::process::Command::new("claude")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn build_backend() -> (Arc<dyn LLMBackend>, String) {
    // Resilient multi-provider chain (NanoGPT → Ollama Cloud → MiniMax, in priority order) built from
    // whatever keys are present; an error OR empty reply fails over to the next. Provider endpoints
    // live in mind_inference so adding a provider is one line there. Verified live: NanoGPT
    // (deepseek-v4-pro), Ollama Cloud (glm-4.7), MiniMax (MiniMax-M2.7).
    if let Some((backend, label)) = mind_inference::default_chain_from_env() {
        return (backend, label);
    }
    if claude_available() {
        return (Arc::new(yantrik_ml::ClaudeCliLLM::new(None, 1024)), "claude-cli".to_string());
    }
    (
        Arc::new(ScriptedLLM::new(
            "(scripted fallback — set NANOGPT_KEY/OLLAMA_CLOUD_KEY/MINIMAX_API_KEY or install the claude CLI for a real reply)",
        )),
        "scripted".to_string(),
    )
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (backend, name) = build_backend();
    let permits = if name.starts_with("nanogpt") { 4 } else { 1 };
    let pool = InferencePool::new(backend, permits);

    let db = std::env::var("YM_DB").unwrap_or_else(|_| ":memory:".to_string());
    // dim 64 = yantrikdb 0.9.0's bundled embedder dimension; YantrikDB::new auto-attaches the
    // in-process model2vec embedder at this dim, so record/recall are genuinely SEMANTIC with no
    // external server. (A dim-8 DB from before this upgrade is incompatible — recreate the file.)
    let mem = MemoryHandle::spawn(&db, 64).map_err(|e| anyhow::anyhow!("memory init: {e:?}"))?;
    let conv = mind_core::engine(&mem, pool);

    // Recover any recipe runs interrupted by a previous crash (durable + idempotency-safe).
    let resumed = conv.resume_recipes().await;
    if resumed > 0 {
        println!("recovered {resumed} interrupted recipe run(s)");
    }

    // If a telegram token is configured, run the phone channel instead of the stdin REPL.
    if let Ok(tok) = std::env::var("YM_TELEGRAM_TOKEN") {
        if !tok.trim().is_empty() {
            println!("yantrik-mind — backend: {name} · db: {db} · channel: telegram");
            return mind_core::telegram::run(tok, mem, conv).await;
        }
    }

    println!("yantrik-mind — backend: {name} · db: {db}");
    println!("type :help for a list of commands  (else = chat)\n");

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    loop {
        print!("you> ");
        std::io::stdout().flush().ok();
        let Some(line) = lines.next() else { break };
        let line = line?;
        match mind_core::handle_line(&line, &mem, &conv).await {
            mind_core::Outcome::Quit => {
                println!("bye.");
                break;
            }
            mind_core::Outcome::Said(s) => {
                if !s.is_empty() {
                    println!("jarvis> {s}\n");
                }
            }
        }
    }
    Ok(())
}
