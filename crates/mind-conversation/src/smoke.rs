//! E.SMOKE1: start the written program in the sandbox and ask it for `/`. Three readings produced
//! three one-line causes for a dead T1; the static checks now see two (an unparseable file, a bare
//! `python`). The third — a WSGI app returning `str` where `bytes` is required, answering 500 to
//! every request while the process stays up — is a fact about one branch at run time, and only a
//! start sees it. This runs only where the sandbox works (`unshare` + loopback), only when the set
//! has a top-level `run.sh` and names port 8123, and convicts only on an explicit `5xx` or an exit
//! before serving. Everything else — a slow start, a sandbox that cannot run — is silence.

use crate::fileset::parse_file_stream;

const PORT: &str = "8123";

/// The driver the sandbox executes at the scratch root. The artifact lives under `app/` so its own
/// `run.sh` is never the driver. Loopback up, the program in the background with its output
/// captured, a 10 s poll on `/`, then one marker line and the log tail.
const DRIVER: &str = r#"ip link set lo up 2>/dev/null || true
echo "SMOKE-PY: $(python3 -c 'import sys; print(sys.version.split()[0])' 2>/dev/null)"
cd app
( sh run.sh > ../app.log 2>&1; echo "EXIT:$?" >> ../app.log ) &
python3 - <<'PY'
import time, urllib.request, urllib.error
t0 = time.time(); status = None
while time.time() - t0 < 10:
    try:
        r = urllib.request.urlopen("http://127.0.0.1:8123/", timeout=2); status = r.status; break
    except urllib.error.HTTPError as e:
        status = e.code; break
    except Exception:
        time.sleep(0.5)
print("SMOKE:", status if status is not None else "no answer")
PY
cd ..
grep -q '^EXIT:' app.log && echo "SMOKE-EXIT: $(grep -o '^EXIT:[0-9]*' app.log | tail -1 | cut -d: -f2)"
echo "--- app.log tail"
tail -14 app.log
"#;

/// What the start said. Only the explicit markers decide.
#[derive(Debug, PartialEq)]
pub(crate) enum Smoke {
    /// `/` answered with this status (< 500 is a pass).
    Answered(u16),
    /// The program exited with this code before answering.
    Exited(i32),
    /// No marker, or no answer with the process still alive: nothing to say.
    Silent,
}

pub(crate) fn verdict_from(rendered: &str) -> Smoke {
    let mut status: Option<u16> = None;
    let mut exit: Option<i32> = None;
    for line in rendered.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("SMOKE:") {
            if let Ok(n) = rest.trim().parse::<u16>() {
                status = Some(n);
            }
        } else if let Some(rest) = l.strip_prefix("SMOKE-EXIT:") {
            if let Ok(n) = rest.trim().parse::<i32>() {
                exit = Some(n);
            }
        }
    }
    match (status, exit) {
        (Some(n), _) => Smoke::Answered(n),
        (None, Some(code)) if code != 0 => Smoke::Exited(code),
        _ => Smoke::Silent,
    }
}

/// The interpreter the start ran under. The sandbox is this box, not the target: a module missing
/// here may exist there (`cgi` under 3.12, gone in 3.13), so every finding names the version.
fn py_version(rendered: &str) -> String {
    rendered
        .lines()
        .find_map(|l| l.trim().strip_prefix("SMOKE-PY:"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// The captured program log after the marker, trimmed for a finding.
fn log_tail(rendered: &str) -> String {
    let after = rendered.split("--- app.log tail").nth(1).unwrap_or("");
    let lines: Vec<&str> = after.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    let keep: Vec<&str> = lines.iter().rev().take(8).rev().copied().collect();
    keep.join("\n").chars().take(700).collect()
}

fn wants_smoke(entries: &[crate::fileset::FileEntry]) -> bool {
    entries.iter().any(|e| e.path == "run.sh") && entries.iter().any(|e| e.content.contains(PORT))
}

/// Findings for a set whose server starts but cannot answer. Empty for every healthy build, and
/// empty — without a sandbox run — for sets that are not a web program at all.
pub(crate) async fn smoke_findings(stream: &str) -> Vec<String> {
    let entries = parse_file_stream(stream).entries;
    if !wants_smoke(&entries) {
        return Vec::new();
    }
    let files: Vec<(String, String)> = entries
        .iter()
        .map(|e| (format!("app/{}", e.path), e.content.clone()))
        .collect();
    let lim = mind_tools::sandbox::Limits {
        wall_secs: 16,
        cpu_secs: 12,
        ..mind_tools::sandbox::Limits::default()
    };
    let sb = mind_tools::Sandbox::new().hiding(crate::syntax::state_dir());
    let n_files = files.len();
    let rendered = match sb.run_tree(lim, files, DRIVER).await {
        Ok(r) => r.render(),
        Err(e) => {
            // No sandbox here: say nothing to the model, but leave the fact in the journal.
            eprintln!("[smoke] sandbox unavailable here ({e}) - no start attempted");
            return Vec::new();
        }
    };
    let py = py_version(&rendered);
    let verdict = verdict_from(&rendered);
    // The one trace a witness on the box can look for: the start happened, and what it said.
    eprintln!("[smoke] started {n_files} files under python {py}: {verdict:?}");
    match verdict {
        Smoke::Answered(n) if n >= 500 => vec![format!(
            "`run.sh` starts the program (a sandboxed start here, under Python {py}), but `/` answers HTTP {n} — it is up and every request fails, so nothing that opens it can work. The last lines of its own log:\n{}\nFix the handler that serves `/` (a WSGI app must return an iterable of bytes — a list — never a bare str or bytes; a route must not raise).",
            log_tail(&rendered)
        )],
        Smoke::Exited(code) => vec![format!(
            "`run.sh` exits with status {code} before serving anything on port {PORT} (a sandboxed start here, under Python {py}; the target's version may differ, so depend on nothing version-specific). The last lines of its own log:\n{}\nThe program must start and keep serving; fix the error above.",
            log_tail(&rendered)
        )],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_markers_decide() {
        assert_eq!(verdict_from("SMOKE: 500\n--- app.log tail\nx"), Smoke::Answered(500));
        assert_eq!(verdict_from("SMOKE: 200"), Smoke::Answered(200));
        assert_eq!(verdict_from("SMOKE: no answer\nSMOKE-EXIT: 1\n"), Smoke::Exited(1));
        assert_eq!(verdict_from("SMOKE: no answer\n"), Smoke::Silent, "alive but slow: silence");
        assert_eq!(verdict_from("SMOKE: no answer\nSMOKE-EXIT: 0\n"), Smoke::Silent);
        assert_eq!(verdict_from("500 Internal Server Error\nunshare: failed"), Smoke::Silent, "no marker, no verdict");
        assert_eq!(py_version("SMOKE-PY: 3.13.5\nSMOKE: 500"), "3.13.5");
        assert_eq!(py_version("SMOKE-PY: \nSMOKE: 500"), "unknown");
    }

    #[test]
    fn a_set_without_a_run_sh_or_a_port_costs_nothing() {
        let t0 = std::time::Instant::now();
        let f = futures_lite_block(smoke_findings("=== FILE: tracker.py\nprint('hi')\n"));
        assert!(f.is_empty());
        assert!(t0.elapsed() < std::time::Duration::from_secs(1), "no sandbox spawn");
    }

    fn futures_lite_block<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(fut)
    }

    /// The witness: the real artifact that scored 2/11 in reading 8g, read from
    /// `YM_SMOKE_WITNESS_DIR` on the box that holds it. In the checker image (Python 3.12.3) its
    /// `/` answered 500; on a 3.13 box its `from cgi import FieldStorage` dies at import first.
    /// Both are the artifact's own faults, and both convict: as-is, and with that one line gone.
    #[tokio::test]
    async fn the_real_reading_8g_artifact_is_convicted_as_is_and_by_its_500_without_cgi() {
        let Ok(dir) = std::env::var("YM_SMOKE_WITNESS_DIR") else {
            eprintln!("skipped: YM_SMOKE_WITNESS_DIR unset");
            return;
        };
        let run_sh = std::fs::read_to_string(format!("{dir}/run.sh")).expect("artifact run.sh");
        let server = std::fs::read_to_string(format!("{dir}/server.py")).expect("artifact server.py");
        let stream = |server: &str| format!("=== FILE: run.sh\n{run_sh}\n=== FILE: server.py\n{server}\n");

        let f = smoke_findings(&stream(&server)).await;
        assert_eq!(f.len(), 1, "as-is: {f:?}");
        assert!(f[0].contains("under Python "), "{}", f[0]);
        assert!(
            f[0].contains("HTTP 500") || f[0].contains("No module named 'cgi'"),
            "as-is: {}",
            f[0]
        );

        let without_cgi: String =
            server.lines().filter(|l| !l.contains("from cgi import")).map(|l| format!("{l}\n")).collect();
        assert_ne!(without_cgi, server, "the artifact does import cgi");
        let f = smoke_findings(&stream(&without_cgi)).await;
        assert_eq!(f.len(), 1, "without cgi: {f:?}");
        assert!(f[0].contains("HTTP 500") && f[0].contains("bytes instance"), "without cgi: {}", f[0]);
    }

    /// The real sandbox, on a box where it works (`YM_SANDBOX_TESTS=1`). The first case is the
    /// exact shape that killed reading 8g's T1: a wsgiref app returning str.
    #[tokio::test]
    async fn the_real_sandbox_convicts_a_500_and_an_early_exit_and_clears_a_healthy_server() {
        if std::env::var("YM_SANDBOX_TESTS").ok().as_deref() != Some("1") {
            eprintln!("skipped: YM_SANDBOX_TESTS!=1");
            return;
        }
        let wsgi = "=== FILE: run.sh\npython3 server.py\n=== FILE: server.py\nfrom wsgiref.simple_server import make_server\ndef app(environ, start_response):\n    start_response('200 OK', [('Content-Type','text/html')])\n    return ['<h1>hi</h1>']\nmake_server('0.0.0.0', 8123, app).serve_forever()\n";
        let f = smoke_findings(wsgi).await;
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].contains("HTTP 500") && f[0].contains("bytes") && f[0].contains("under Python 3."), "{}", f[0]);

        let healthy = "=== FILE: run.sh\npython3 -m http.server 8123\n=== FILE: index.html\n<h1>hi</h1>\n";
        assert!(smoke_findings(healthy).await.is_empty());

        let crash = "=== FILE: run.sh\npython3 server.py\n=== FILE: server.py\nimport nosuchmodule_xyz\nprint(8123)\n";
        let f = smoke_findings(crash).await;
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].contains("exits with status") && f[0].contains("ModuleNotFoundError"), "{}", f[0]);
    }
}
