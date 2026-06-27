//! mind-core — the orchestrator + runnable REPL.
//!
//! v1 wires the slice that proves the bet end-to-end: a real LLM backend (chosen at runtime) →
//! `InferencePool` → `ConversationEngine` grounded in the `mind-memory` typed moat. The REPL lets
//! you talk to it and watch memory evolve live (assert beliefs, see contradictions, recall).

use std::sync::Arc;

use mind_conversation::ConversationEngine;
use mind_memory::MemoryHandle;
use mind_types::{BeliefAssertion, MemoryFacade};

/// One REPL line → an outcome. Split out of `main` so it's deterministically testable with a
/// `ScriptedLLM` (no real model).
pub enum Outcome {
    Quit,
    Said(String),
}

/// Handle a single REPL line. Commands start with `:`; anything else is a chat turn.
///   `:remember + <statement>` / `:remember - <statement>`  assert evidence for/against a belief
///   `:conflicts`                                            list open contradictions
///   `:explain <statement>`                                  show a belief + its evidence count
///   `:quit`
pub async fn handle_line(line: &str, mem: &MemoryHandle, conv: &ConversationEngine) -> Outcome {
    let t = line.trim();
    if t.is_empty() {
        return Outcome::Said(String::new());
    }
    if t == ":quit" || t == ":q" {
        return Outcome::Quit;
    }
    if t == ":conflicts" {
        return match mem.conflicts().await {
            Ok(cs) if cs.is_empty() => Outcome::Said("(no open contradictions)".into()),
            Ok(cs) => Outcome::Said(
                cs.iter()
                    .map(|c| format!("• \"{}\" ⟂ \"{}\" (severity {:.2})", c.belief_a, c.belief_b, c.severity))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(stmt) = t.strip_prefix(":explain ") {
        return match mem.explain_belief(stmt.trim()).await {
            Ok(Some((b, ev))) => Outcome::Said(format!(
                "{} — confidence {:.2}, {} evidence item(s), provenance {}",
                b.statement, b.confidence, ev.len().max(b.evidence_count as usize), b.provenance
            )),
            Ok(None) => Outcome::Said("(no such belief)".into()),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if t == ":tasks" {
        return match mem.list_tasks(false).await {
            Ok(ts) if ts.is_empty() => Outcome::Said("(no open tasks)".into()),
            Ok(ts) => Outcome::Said(
                ts.iter()
                    .map(|t| format!("• [{}] {} ({})", t.id, t.description, t.priority))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(desc) = t.strip_prefix(":task ") {
        return match mem.add_task(desc.trim(), "medium", None).await {
            Ok(task) => Outcome::Said(format!("added task [{}]: {}", task.id, task.description)),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(id) = t.strip_prefix(":done ") {
        return match mem.complete_task(id.trim()).await {
            Ok(true) => Outcome::Said("task completed".into()),
            Ok(false) => Outcome::Said("(no such task)".into()),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    if let Some(rest) = t.strip_prefix(":remember ") {
        let rest = rest.trim();
        let (polarity, statement) = if let Some(s) = rest.strip_prefix("- ") {
            (-1.0, s.trim())
        } else if let Some(s) = rest.strip_prefix("+ ") {
            (1.0, s.trim())
        } else {
            (1.0, rest)
        };
        return match mem
            .remember_as_belief(BeliefAssertion {
                statement: statement.to_string(),
                polarity,
                weight: 1.5,
                source_event: Some("repl".into()),
                provenance: "told".into(),
            })
            .await
        {
            Ok(b) => Outcome::Said(format!("remembered: {} (confidence now {:.2})", b.statement, b.confidence)),
            Err(e) => Outcome::Said(format!("(error: {e})")),
        };
    }
    // plain chat turn
    match conv.handle_turn(t).await {
        Ok(r) => Outcome::Said(r),
        Err(e) => Outcome::Said(format!("(error: {e})")),
    }
}

/// Build a `ConversationEngine` from a memory handle, an inference pool, and the persona.
pub fn engine(mem: &MemoryHandle, pool: mind_inference::InferencePool) -> ConversationEngine {
    ConversationEngine::new(Arc::new(mem.clone()), pool, PERSONA)
}

pub const PERSONA: &str =
    "You are JARVIS — Pranab's AI, his extension and friend. Terse, warm, honest. Ground every \
     claim in the memory block; never invent facts. If a belief is uncertain, hedge (\"I think\"). \
     If there's an open contradiction, ask to resolve it rather than picking a side.";

#[cfg(test)]
mod tests {
    use super::*;
    use mind_inference::{InferencePool, ScriptedLLM};
    use yantrik_ml::LLMBackend;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn repl_commands_drive_typed_memory_and_chat() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let scripted = Arc::new(ScriptedLLM::new("ok"));
        let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
        let conv = engine(&mem, pool);

        // assert two contradicting beliefs + a link via the REPL
        assert!(matches!(handle_line(":remember + Pranab likes coffee", &mem, &conv).await, Outcome::Said(s) if s.contains("confidence")));
        handle_line(":remember + Pranab hates coffee", &mem, &conv).await;
        mem.relate("Pranab likes coffee", "Pranab hates coffee", "contradicts", 0.9).await.unwrap();

        // :conflicts surfaces it
        match handle_line(":conflicts", &mem, &conv).await {
            Outcome::Said(s) => assert!(s.contains("coffee"), "conflicts should list it: {s}"),
            _ => panic!("expected output"),
        }

        // a chat turn grounds in memory (ScriptedLLM saw the belief in its prompt)
        match handle_line("do I like coffee?", &mem, &conv).await {
            Outcome::Said(s) => assert_eq!(s, "ok"),
            _ => panic!("expected reply"),
        }
        assert!(scripted.last_system_prompt().contains("coffee"), "chat should be grounded in memory");

        // :quit
        assert!(matches!(handle_line(":quit", &mem, &conv).await, Outcome::Quit));
    }
}
