//! mind-memory — the memory layer over YantrikDB; the **sole writer** to the cognitive graph.
//!
//! Spike A (Phase 0): prove the single-owner **actor** pattern over the `!Sync` `YantrikDB`
//! (it holds `RefCell` + a rusqlite `Connection`, so it can't be shared across threads or held
//! across `.await`). One dedicated OS thread owns the DB; an async `MemoryHandle` (Clone, Send,
//! Sync) talks to it over an mpsc command channel with a tokio oneshot reply. This dissolves the
//! `!Sync` problem by construction and serializes the single SQLite writer (which we want).
//! The full `MemoryFacade` (typed beliefs, recall, working-set, consolidation) lands in Phase 1.

use mind_types::{MindError, Result};
use tokio::sync::{mpsc, oneshot};
use yantrikdb_core::YantrikDB;

/// Reply channel carrying a stringified error (the actor maps YantrikDB errors to strings so the
/// `!Send`-ness of any internal error type never crosses the thread boundary).
type Reply<T> = oneshot::Sender<std::result::Result<T, String>>;

enum Cmd {
    /// Record an episodic memory; replies with its rid.
    Record { text: String, reply: Reply<String> },
    /// Fetch a memory's text by rid.
    GetText { rid: String, reply: Reply<Option<String>> },
}

/// Cheap, cloneable async handle to the memory actor. Share it freely across tasks.
#[derive(Clone)]
pub struct MemoryHandle {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl MemoryHandle {
    /// Spawn the actor on a dedicated OS thread that owns the `YantrikDB`. Use `":memory:"` for
    /// an ephemeral store. `dim` is the embedding dimension (dummy zero-vectors in this spike).
    pub fn spawn(db_path: &str, dim: usize) -> Result<Self> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
        let path = db_path.to_string();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();

        std::thread::Builder::new()
            .name("mind-memory".into())
            .spawn(move || {
                // YantrikDB is constructed and lives ENTIRELY on this thread — never moved out.
                let db = match YantrikDB::new(&path, dim) {
                    Ok(d) => {
                        let _ = ready_tx.send(Ok(()));
                        d
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let zero = vec![0.0f32; dim];
                let meta = serde_json::json!({});
                // blocking_recv drains the async channel from this sync thread.
                while let Some(cmd) = rx.blocking_recv() {
                    match cmd {
                        Cmd::Record { text, reply } => {
                            let r = db
                                .record(
                                    &text, "episodic", 0.5, 0.0, 604_800.0, &meta, &zero,
                                    "default", 0.8, "general", "user", None,
                                )
                                .map_err(|e| e.to_string());
                            let _ = reply.send(r);
                        }
                        Cmd::GetText { rid, reply } => {
                            let r = db
                                .get(&rid)
                                .map(|opt| opt.map(|m| m.text))
                                .map_err(|e| e.to_string());
                            let _ = reply.send(r);
                        }
                    }
                }
            })
            .map_err(|e| MindError::Memory(format!("spawn actor: {e}")))?;

        // Block until the DB is constructed (or fails) so callers get a real error early.
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { tx }),
            Ok(Err(e)) => Err(MindError::Memory(format!("init YantrikDB: {e}"))),
            Err(_) => Err(MindError::Memory("actor thread died during init".into())),
        }
    }

    async fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Cmd) -> Result<T> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(make(reply))
            .map_err(|_| MindError::Memory("memory actor is gone".into()))?;
        rx.await
            .map_err(|_| MindError::Memory("memory actor dropped the reply".into()))?
            .map_err(MindError::Memory)
    }

    pub async fn record(&self, text: impl Into<String>) -> Result<String> {
        let text = text.into();
        self.call(|reply| Cmd::Record { text, reply }).await
    }

    pub async fn get_text(&self, rid: &str) -> Result<Option<String>> {
        let rid = rid.to_string();
        self.call(|reply| Cmd::GetText { rid, reply }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn actor_round_trips_a_write_then_read() {
        let mem = MemoryHandle::spawn(":memory:", 8).expect("spawn");
        let rid = mem.record("the sky is blue").await.expect("record");
        let got = mem.get_text(&rid).await.expect("get");
        assert_eq!(got.as_deref(), Some("the sky is blue"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_concurrent_tasks_no_lost_writes_no_deadlock() {
        let mem = MemoryHandle::spawn(":memory:", 8).expect("spawn");
        // Fan out interleaved writes from many tasks against the single-owner actor.
        let mut handles = Vec::new();
        for i in 0..50u32 {
            let m = mem.clone();
            handles.push(tokio::spawn(async move {
                m.record(format!("fact number {i}")).await
            }));
        }
        let mut rids = Vec::new();
        for h in handles {
            rids.push(h.await.expect("join").expect("record"));
        }
        assert_eq!(rids.len(), 50);
        // Every write is independently readable back — no lost writes, no deadlock.
        for (i, rid) in rids.iter().enumerate() {
            let got = mem.get_text(rid).await.expect("get");
            assert!(got.is_some(), "rid {i} missing");
        }
        // rids are unique (uuid7).
        let unique: std::collections::HashSet<_> = rids.iter().collect();
        assert_eq!(unique.len(), 50);
    }
}
