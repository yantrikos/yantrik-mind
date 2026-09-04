//! E.LOOP-I2 — catch `from <stdlib module> import <name that is not there>` WITHOUT running anything.
//!
//! Leg 4 of reading 7 scored **2/11**. The artifact was complete, well-formed and parsed cleanly;
//! it died at import, because `TCPServer` lives in `socketserver` and not in `http.server`. The
//! model review step read that file and approved it — which is the measured reason not to rely on
//! a model checking its own work. One import resolution finds it in microseconds.
//!
//! **Why a table and not an interpreter.** E.LOOP-I did this by asking `python3` to introspect the
//! stdlib, and then stopped: that is a runtime dependency on the mind, and adding one is a design
//! choice rather than a fix. The table below needs no interpreter. Its contents are an
//! OBSERVATION, not a guess — they are every `from X import Y` appearing across the Mind's 35 real
//! T1/T3 artifacts, which between them use seven stdlib modules.
//!
//! **It is list-bounded, and only ever wrong by staying quiet.** A module the table does not know
//! yields no finding at all; the check never accuses on incomplete knowledge. That direction is
//! deliberate — E.LOOP-I's first version flagged `from http import server`, which is a perfectly
//! valid submodule import, on a leg whose server started fine. A checker that cries wolf gets
//! turned off, so silence is the failure mode to prefer.

/// `(module, the complete set of names importable from it)`.
///
/// Complete for the module named, or the module does not belong here: a partial list would produce
/// exactly the false accusations this check must never make. Submodules that are themselves
/// importable (`from http import server`) are listed as names of their parent, which is what makes
/// that valid form pass.
const STDLIB: &[(&str, &[&str])] = &[
    (
        "http.server",
        &[
            "HTTPServer",
            "ThreadingHTTPServer",
            "BaseHTTPRequestHandler",
            "SimpleHTTPRequestHandler",
            "CGIHTTPRequestHandler",
            "test",
        ],
    ),
    // `server` and the other submodules are importable FROM `http`; `HTTPStatus` is the enum.
    ("http", &["HTTPStatus", "HTTPMethod", "server", "client", "cookies", "cookiejar"]),
    ("socketserver", &[
        "BaseServer", "TCPServer", "UDPServer", "ForkingTCPServer", "ForkingUDPServer",
        "ThreadingTCPServer", "ThreadingUDPServer", "BaseRequestHandler",
        "StreamRequestHandler", "DatagramRequestHandler", "ThreadingMixIn", "ForkingMixIn",
        "UnixStreamServer", "UnixDatagramServer", "ThreadingUnixStreamServer",
        "ThreadingUnixDatagramServer",
    ]),
    ("urllib.parse", &[
        "urlparse", "urlunparse", "urlsplit", "urlunsplit", "urljoin", "urldefrag",
        "unquote", "unquote_plus", "unquote_to_bytes", "quote", "quote_plus", "quote_from_bytes",
        "urlencode", "parse_qs", "parse_qsl", "ParseResult", "SplitResult", "DefragResult",
    ]),
    ("datetime", &["date", "time", "datetime", "timedelta", "timezone", "tzinfo", "MINYEAR", "MAXYEAR", "UTC"]),
    ("pathlib", &["Path", "PurePath", "PurePosixPath", "PureWindowsPath", "PosixPath", "WindowsPath"]),
    ("wsgiref.simple_server", &[
        "make_server", "WSGIServer", "WSGIRequestHandler", "ServerHandler", "demo_app",
    ]),
];

/// Names `typing` exports are effectively open-ended across versions, and `__future__` is special.
/// Both appear in the corpus, and both must never be judged: listing them here documents that they
/// were considered and deliberately excluded, rather than merely forgotten.
const NEVER_JUDGED: &[&str] = &["typing", "__future__", "collections.abc"];

/// One `from X import ...` line that cannot resolve, rendered for a human (and for the review
/// prompt). Empty when nothing is wrong, which is the overwhelmingly common case.
pub(crate) fn stdlib_import_findings(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in src.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("from ") else {
            continue;
        };
        let Some((module, names)) = rest.split_once(" import ") else {
            continue;
        };
        let module = module.trim();
        if NEVER_JUDGED.contains(&module) {
            continue;
        }
        // A parenthesised or line-continued import list is not fully visible on this line. Judging
        // half a list is how a checker invents a finding, so it declines instead.
        if names.contains('(') || line.ends_with('\\') || names.contains('*') {
            continue;
        }
        let Some((_, known)) = STDLIB.iter().find(|(m, _)| *m == module) else {
            continue; // not in the table: silence, never an accusation
        };
        for name in names.split(',') {
            // `X as Y` imports X; the alias is irrelevant to whether it resolves.
            let name = name.trim().split_whitespace().next().unwrap_or("").trim();
            if name.is_empty() || known.contains(&name) {
                continue;
            }
            let elsewhere = STDLIB
                .iter()
                .find(|(m, names)| *m != module && names.contains(&name))
                .map(|(m, _)| *m);
            out.push(match elsewhere {
                Some(m) => format!(
                    "`from {module} import {name}` does not resolve — {name} is in `{m}`, not `{module}`"
                ),
                None => format!("`from {module} import {name}` does not resolve — {module} has no {name}"),
            });
        }
    }
    out
}
