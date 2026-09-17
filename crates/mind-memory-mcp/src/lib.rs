//! The machine's memory over MCP — as a library, so the owner of the memory file can serve it.
//!
//! # Who serves this
//!
//! Exactly one process may have the memory file open with a live engine: a second one does not
//! see the first one's writes (proven 2026-09-16 — its recall misses them while its stats count
//! them). So this endpoint is served by whichever process owns the file:
//!
//! - **Yantrik Mind, when it is the active mind.** It already owns the file; it serves this from
//!   its own `MemoryHandle`, so an agent calling `recall` here and the mind recalling in a turn are
//!   reading one engine.
//! - **The standalone `yantrik-memory` binary, otherwise.** When Hermes (or no mind) is active,
//!   Mind is stopped and this binary owns the file instead.
//!
//! Same address, same token file, same tools either way; a client cannot tell which is running and
//! does not need to. The service manager, not good intentions, keeps the two from overlapping —
//! the standalone unit declares `Conflicts=` with Mind's.

mod server;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use axum::extract::{Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

pub use server::MemoryServer;

/// The address clients look for when nothing says otherwise.
pub const DEFAULT_BIND: &str = "127.0.0.1:7440";

/// Where the token lives: beside the memory file, so both possible owners of one file agree on it
/// without being told.
pub fn default_token_path(db_path: &str) -> PathBuf {
    Path::new(db_path).with_file_name("yantrik-memory.token")
}

/// Parse and check a bind address. Loopback only: this endpoint hands a person's memory to anyone
/// holding the token, over plain HTTP.
pub fn parse_bind(bind: &str) -> anyhow::Result<SocketAddr> {
    let addr: SocketAddr = bind.parse().with_context(|| format!("bad bind address `{bind}`"))?;
    if !addr.ip().is_loopback() {
        return Err(anyhow!("the memory server must bind a loopback address, got {addr}"));
    }
    Ok(addr)
}

/// Read the token, or create one readable only by this user.
pub fn load_or_create_token(path: &Path) -> anyhow::Result<String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let t = existing.trim().to_string();
        if t.len() >= 32 {
            return Ok(t);
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut bytes = [0u8; 32];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("reading /dev/urandom for the token")?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

/// Resolves when the process is asked to stop: Ctrl-C, or SIGTERM from the service manager.
///
/// SIGTERM matters more than it looks. Switching minds is the service manager stopping one owner
/// of the memory file and starting the other, and it stops things with SIGTERM. Waiting only on
/// Ctrl-C would leave the default SIGTERM action — immediate death, mid-write — as the way every
/// handover ends.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Compare without leaking how many leading bytes matched.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn require_token(State(token): State<String>, req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if constant_time_eq(presented.as_bytes(), token.as_bytes()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "a valid bearer token is required").into_response()
    }
}

/// Serve the memory over MCP until `shutdown` resolves. `served_by` names the owner in `/health`,
/// so a person checking which process has the memory can see it.
pub async fn serve_http(
    mem: mind_memory::MemoryHandle,
    bind: SocketAddr,
    token_path: &Path,
    served_by: &'static str,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let token = load_or_create_token(token_path)?;
    let mcp = StreamableHttpService::new(
        move || Ok(MemoryServer::new(mem.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let app = Router::new()
        .nest_service("/mcp", mcp)
        .route_layer(middleware::from_fn_with_state(token, require_token))
        // After the layer, so the health check needs no token: it says only that memory is being
        // served and by whom — what a service manager or a client's first probe needs.
        .route(
            "/health",
            get(move || async move {
                axum::Json(serde_json::json!({ "ok": true, "server": "yantrik-memory", "served_by": served_by }))
            }),
        );
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding the memory server to {bind}"))?;
    tracing::info!(addr = %bind, token_file = %token_path.display(), served_by, "memory served over MCP at /mcp");
    axum::serve(listener, app).with_graceful_shutdown(shutdown).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_compare_exactly() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
    }

    #[test]
    fn only_loopback_binds_are_accepted() {
        assert!(parse_bind("127.0.0.1:7440").is_ok());
        assert!(parse_bind("[::1]:7440").is_ok());
        assert!(parse_bind("0.0.0.0:7440").is_err());
        assert!(parse_bind("192.168.4.65:7440").is_err());
    }

    /// Both possible owners of one file must find the same token without being configured.
    #[test]
    fn the_token_sits_beside_the_memory_file() {
        assert_eq!(
            default_token_path("/home/u/.local/share/yantrik-mind/mind.db"),
            PathBuf::from("/home/u/.local/share/yantrik-mind/yantrik-memory.token")
        );
    }
}
