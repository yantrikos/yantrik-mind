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
    if let Ok(key) = std::env::var("NANOGPT_KEY") {
        if !key.trim().is_empty() {
            let model = std::env::var("YM_MODEL").unwrap_or_else(|_| "chatgpt-4o-latest".to_string());
            let be = yantrik_ml::GenericOpenAIBackend::for_provider(
                "openai",
                "https://nano-gpt.com/api/v1",
                Some(key),
                model.clone(),
            );
            return (Arc::new(be), format!("nanogpt:{model}"));
        }
    }
    if claude_available() {
        return (Arc::new(yantrik_ml::ClaudeCliLLM::new(None, 1024)), "claude-cli".to_string());
    }
    (
        Arc::new(ScriptedLLM::new(
            "(scripted fallback — set NANOGPT_KEY or install the claude CLI for a real reply)",
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
    let mem = MemoryHandle::spawn(&db, 8).map_err(|e| anyhow::anyhow!("memory init: {e:?}"))?;
    let conv = mind_core::engine(&mem, pool);

    println!("yantrik-mind — backend: {name} · db: {db}");
    println!(
        "commands: ':remember + <stmt>' / ':remember - <stmt>', ':conflicts', ':explain <stmt>', ':quit'  (else = chat)\n"
    );

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
