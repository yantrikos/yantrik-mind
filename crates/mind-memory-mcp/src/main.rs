//! yantrik-memory — the machine's memory, served over MCP when no mind owns the file.
//!
//! When Yantrik Mind is the active mind it owns the memory file and serves this same endpoint
//! itself (see `mind_memory_mcp` docs). This binary is the owner the rest of the time — while
//! Hermes, or no mind at all, is active — so memory stays reachable at the same address.
//!
//! ```text
//! yantrik-memory --db ~/.local/share/yantrik-mind/mind.db            # HTTP on 127.0.0.1:7440
//! yantrik-memory --db ... --bind 127.0.0.1:7440 --token-file ...      # explicit
//! yantrik-memory --db ... --stdio                                     # one client, for tests
//! ```

use std::path::PathBuf;

use anyhow::anyhow;
use mind_memory_mcp::{default_token_path, parse_bind, serve_http, shutdown_signal, MemoryServer, DEFAULT_BIND};
use rmcp::ServiceExt;

/// The bundled embedder's dimension, which is what every store this mind opens uses.
const DIM: usize = 64;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_target(false).init();

    let mut db = None;
    let mut bind = DEFAULT_BIND.to_string();
    let mut token_file: Option<PathBuf> = None;
    let mut stdio = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--db" => db = it.next(),
            "--bind" => bind = it.next().ok_or_else(|| anyhow!("--bind needs an address"))?,
            "--token-file" => token_file = it.next().map(PathBuf::from),
            "--stdio" => stdio = true,
            "--help" | "-h" => {
                println!("yantrik-memory --db <path> [--bind 127.0.0.1:7440] [--token-file <path>] [--stdio]");
                return Ok(());
            }
            other => return Err(anyhow!("unknown argument `{other}`")),
        }
    }
    let db = db.ok_or_else(|| anyhow!("--db <path> is required"))?;
    let bind = parse_bind(&bind)?;

    let mem = mind_memory::MemoryHandle::spawn(&db, DIM)
        .map_err(|e| anyhow!("opening memory at {db}: {e:?}"))?;
    tracing::info!(db = %db, "memory opened; this process is its only owner");

    if stdio {
        // One client, one process — for tests. Never point two stdio servers at one file.
        tracing::warn!("stdio mode: serving exactly one client; do not share this file with another server");
        let service = MemoryServer::new(mem)
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| anyhow!("mcp stdio: {e}"))?;
        service.waiting().await.map_err(|e| anyhow!("mcp stdio: {e}"))?;
        return Ok(());
    }

    let token_path = token_file.unwrap_or_else(|| default_token_path(&db));
    serve_http(mem, bind, &token_path, "yantrik-memory", shutdown_signal()).await
}
