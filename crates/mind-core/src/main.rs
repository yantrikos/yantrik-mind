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

/// Serve one HTTP request: GET /<file> from `dir` (read-only static; for the publish_page dashboards).
fn web_handle(mut stream: std::net::TcpStream, dir: &str) {
    use std::io::{Read, Write};
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let raw = req.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("/");
    let path = raw.split('?').next().unwrap_or("/").trim_start_matches('/');
    let safe = path.replace("..", "").replace('\\', "");
    let rel = if safe.is_empty() { "index.html".to_string() } else { safe };
    // Defense in depth: allowlist extensions, reject dotfiles, and CANONICALIZE — the resolved
    // path must stay inside dir, so no traversal or symlink can escape the public folder.
    let allowed = [".html", ".txt", ".css", ".js", ".json", ".png", ".jpg", ".svg"];
    let ext_ok = allowed.iter().any(|e| rel.ends_with(e));
    let dotfile = rel.split('/').any(|seg| seg.starts_with('.'));
    let file = format!("{dir}/{rel}");
    let confined = std::fs::canonicalize(&file)
        .ok()
        .zip(std::fs::canonicalize(dir).ok())
        .map(|(f, d)| f.starts_with(&d))
        .unwrap_or(false);
    let (status, body, ctype) = if ext_ok && !dotfile && confined {
        match std::fs::read(&file) {
            Ok(b) => ("200 OK", b, if file.ends_with(".html") { "text/html; charset=utf-8" } else { "text/plain; charset=utf-8" }),
            Err(_) => ("404 Not Found", b"not found".to_vec(), "text/plain; charset=utf-8"),
        }
    } else {
        ("404 Not Found", b"not found".to_vec(), "text/plain; charset=utf-8")
    };
    let header = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(&body);
}

/// A tiny static web server (own thread) so the agent's `publish_page` dashboards are viewable at a URL.
/// Serves YM_WEB_DIR on YM_WEB_PORT (default 8088). Read-only; disable with YM_WEB=off.
fn spawn_web_server() {
    if std::env::var("YM_WEB").map(|v| v == "off").unwrap_or(false) {
        return;
    }
    let port: u16 = std::env::var("YM_WEB_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(8088);
    let dir = std::env::var("YM_WEB_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind/public".to_string());
    let _ = std::fs::create_dir_all(&dir);
    std::thread::spawn(move || match std::net::TcpListener::bind(("0.0.0.0", port)) {
        Ok(listener) => {
            eprintln!("[web] serving {dir} on :{port}");
            for stream in listener.incoming().flatten() {
                let dir = dir.clone();
                std::thread::spawn(move || web_handle(stream, &dir));
            }
        }
        Err(e) => eprintln!("[web] could not bind :{port}: {e}"),
    });
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (backend, name) = build_backend();
    let permits = if name.starts_with("nanogpt") { 4 } else { 1 };
    let pool = InferencePool::new(backend, permits).with_provider(&name);

    let db = std::env::var("YM_DB").unwrap_or_else(|_| ":memory:".to_string());
    // dim 64 = yantrikdb 0.9.0's bundled embedder dimension; YantrikDB::new auto-attaches the
    // in-process model2vec embedder at this dim, so record/recall are genuinely SEMANTIC with no
    // external server. (A dim-8 DB from before this upgrade is incompatible — recreate the file.)
    let mem = MemoryHandle::spawn(&db, 64).map_err(|e| anyhow::anyhow!("memory init: {e:?}"))?;
    let conv = mind_core::engine(&mem, pool);

    // Tiny static web server for the agent's published dashboards (publish_page → shareable URL).
    spawn_web_server();

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
