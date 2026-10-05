//! The one way the Mind's own HTTP clients reach the network (E.EGRESS1, yantrik-os #503).
//!
//! On Yantrik OS the Mind's account is routed through the machine's egress proxy
//! (`HTTPS_PROXY=http://127.0.0.1:7450`, `NO_PROXY=127.0.0.1,localhost,::1`), which counts every
//! destination today and will refuse unknown ones later. The model calls already honour it (ureq 3).
//! The rest of the Mind used ureq 2's default agent, which reads no proxy at all -- and ureq 2's own
//! env support ignores `NO_PROXY`, so switching it on would send a same-box Ollama or SearXNG to a
//! proxy that refuses loopback in every mode.
//!
//! So every request picks its route by host: loopback and `NO_PROXY` hosts go direct; everything
//! else goes through the proxy when one is set. With no proxy set, nothing changes. Two long-lived
//! agents keep connection pooling. `get`/`post`/`put` are drop-ins for `ureq::get`/`post`/`put`.

use std::sync::OnceLock;

/// The proxy to use, from the environment in ureq's own order.
fn proxy_url(get: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]
        .iter()
        .find_map(|k| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
}

/// The host part of a URL, lower-cased, without port or brackets.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let host = if let Some(stripped) = authority.strip_prefix('[') {
        stripped.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host.to_ascii_lowercase()
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host == "::1"
        || host.parse::<std::net::Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
}

/// Does `NO_PROXY` exempt `host`? Exact hosts, `.suffix`, `*.suffix`, and `*`.
fn no_proxy_matches(host: &str, no_proxy: &str) -> bool {
    no_proxy.split(',').map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty()).any(|e| {
        if e == "*" {
            return true;
        }
        let e = e.trim_start_matches("*.").trim_start_matches('.');
        host == e || host.ends_with(&format!(".{e}"))
    })
}

/// E.NET1c (the tenth pass): the endpoints the person configured, which connect DIRECT -- the OS
/// egress proxy refuses private ranges in every mode, and these (search, models, Home Assistant,
/// photos) live on the LAN by design. Routing only: the fetch tools still refuse them.
const DIRECT_ENDPOINT_VARS: [&str; 9] = [
    "YM_SEARXNG_URL",
    "YM_HA_URL",
    "YM_LOCAL_OLLAMA_URL",
    "YM_OLLAMA_LOCAL_URL",
    "YM_NIM_BASE_URL",
    "YM_FACE_ML_URL",
    "YM_CRITIC_URL",
    "YM_WEFT_URL",
    "YM_IMMICH_URL",
];

/// E.NET1c: a URL's host and port (the scheme's when unwritten).
fn host_port(url: &str) -> (String, u16) {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let port = match authority.strip_prefix('[') {
        Some(v6) => v6.split_once("]:").map(|(_, p)| p),
        None => authority.split_once(':').map(|(_, p)| p),
    };
    let default = if scheme.eq_ignore_ascii_case("https") { 443 } else { 80 };
    (host_of(url), port.and_then(|p| p.parse().ok()).unwrap_or(default))
}

/// E.NET1c: is `url` on one of the person-configured endpoints (same host and port)?
fn configured_endpoint(url: &str, get: &dyn Fn(&str) -> Option<String>) -> bool {
    let target = host_port(url);
    DIRECT_ENDPOINT_VARS
        .iter()
        .filter_map(|k| get(k))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .any(|v| host_port(&v) == target)
}

/// Should a request to `url` go direct rather than through the proxy?
fn goes_direct(url: &str, get: &dyn Fn(&str) -> Option<String>) -> bool {
    if proxy_url(get).is_none() {
        return true;
    }
    if configured_endpoint(url, get) {
        return true;
    }
    let host = host_of(url);
    if host.is_empty() || is_loopback(&host) {
        return true;
    }
    let no_proxy = get("NO_PROXY").or_else(|| get("no_proxy")).unwrap_or_default();
    no_proxy_matches(&host, &no_proxy)
}

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn direct() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| ureq::AgentBuilder::new().try_proxy_from_env(false).build())
}

fn proxied() -> &'static ureq::Agent {
    static A: OnceLock<ureq::Agent> = OnceLock::new();
    A.get_or_init(|| {
        let b = ureq::AgentBuilder::new().try_proxy_from_env(false);
        match proxy_url(&env).and_then(|p| ureq::Proxy::new(p).ok()) {
            Some(p) => b.proxy(p).build(),
            None => b.build(),
        }
    })
}

/// E.NET1: does a request to `url` connect to its host itself (true), or through the egress proxy?
/// Only a direct connection can be pinned to the address the SSRF check approved.
pub fn is_direct(url: &str) -> bool {
    goes_direct(url, &env)
}

/// The agent a request to `url` should use.
pub fn agent_for(url: &str) -> &'static ureq::Agent {
    if goes_direct(url, &env) {
        direct()
    } else {
        proxied()
    }
}

/// A builder that has not read the proxy environment -- for the few clients that need their own
/// timeouts. Finish it with [`route`] so the request still takes the right path.
pub fn builder() -> ureq::AgentBuilder {
    ureq::AgentBuilder::new().try_proxy_from_env(false)
}

/// `builder` with the route to `url` applied: the proxy unless `url` goes direct.
pub fn route(builder: ureq::AgentBuilder, url: &str) -> ureq::Agent {
    if goes_direct(url, &env) {
        return builder.build();
    }
    match proxy_url(&env).and_then(|p| ureq::Proxy::new(p).ok()) {
        Some(p) => builder.proxy(p).build(),
        None => builder.build(),
    }
}

/// Drop-in for `ureq::get`.
pub fn get(url: &str) -> ureq::Request {
    agent_for(url).get(url)
}

/// Drop-in for `ureq::post`.
pub fn post(url: &str) -> ureq::Request {
    agent_for(url).post(url)
}

/// Drop-in for `ureq::put`.
pub fn put(url: &str) -> ureq::Request {
    agent_for(url).put(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    /// E.EGRESS2: a plain `http` request to a proxy keeps the absolute-form request line the proxy
    /// needs. (The `https` case -- origin-form inside the CONNECT tunnel -- is the patched line in
    /// third_party/ureq-2.12.1/src/unit.rs, checked by hand against DuckDuckGo.)
    #[test]
    fn a_plain_http_request_to_a_proxy_keeps_the_absolute_form() {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut line = String::new();
            std::io::BufReader::new(s.try_clone().unwrap()).read_line(&mut line).unwrap();
            let mut s = s;
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            line
        });
        let agent = builder().proxy(ureq::Proxy::new(format!("http://127.0.0.1:{port}")).unwrap()).build();
        let _ = agent.get("http://example.test/x?q=1").call();
        assert_eq!(seen.join().unwrap().trim_end(), "GET http://example.test/x?q=1 HTTP/1.1");
    }

    /// E.EGRESS1: with 520's environment, public hosts take the proxy; loopback and NO_PROXY go direct.
    #[test]
    fn public_hosts_take_the_proxy_and_loopback_goes_direct() {
        let e = env_of(&[("HTTPS_PROXY", "http://127.0.0.1:7450"), ("NO_PROXY", "127.0.0.1,localhost,::1,.lan")]);
        assert!(!goes_direct("https://api.open-meteo.com/v1/forecast?x=1", &e), "weather went around the proxy");
        assert!(!goes_direct("https://ollama.com/api/chat", &e));
        assert!(goes_direct("http://127.0.0.1:11434/api/tags", &e), "a same-box Ollama was sent to the proxy");
        assert!(goes_direct("http://localhost:8888/search?q=x", &e));
        assert!(goes_direct("http://[::1]:7440/mcp", &e));
        assert!(goes_direct("http://127.9.9.9/", &e), "all of 127/8 is loopback");
        assert!(goes_direct("http://gpu-box.lan:11434/", &e), "a NO_PROXY suffix");
        assert!(!goes_direct("http://gpu-box:11434/", &e), "a bare LAN name is not exempt unless listed");
    }

    /// E.NET1c (the tenth pass): the endpoints the person configured go direct -- the OS proxy refuses
    /// private ranges -- matched by host AND port; any other host, or another port, still takes the proxy.
    #[test]
    fn configured_endpoints_go_direct_and_nothing_else() {
        let e = env_of(&[
            ("HTTPS_PROXY", "http://127.0.0.1:7450"),
            ("YM_SEARXNG_URL", "http://192.168.4.42:8888"),
            ("YM_HA_URL", "http://192.168.4.10:8123/"),
            ("YM_NIM_BASE_URL", "https://integrate.example.com/v1"),
        ]);
        assert!(goes_direct("http://192.168.4.42:8888/search?q=semantic", &e), "the configured SearXNG took the proxy");
        assert!(goes_direct("http://192.168.4.10:8123/api/states", &e), "Home Assistant took the proxy");
        assert!(goes_direct("https://integrate.example.com/v1/chat/completions", &e), "the model endpoint (default port) took the proxy");
        assert!(!goes_direct("http://192.168.4.42:22/", &e), "another port on a configured host went direct");
        assert!(!goes_direct("http://192.168.4.99:8888/", &e), "an unconfigured LAN host went direct");
        assert!(!goes_direct("https://evil.example/", &e));
    }

    /// E.EGRESS1: no Mind code builds its own ureq request or agent -- every one goes through here,
    /// so none can go around the egress proxy. Test modules (after a file's first `#[cfg(test)]`)
    /// and `tests.rs` files may still talk to their own local mock servers directly.
    #[test]
    fn nothing_reaches_the_network_around_this_crate() {
        const BARE: [&str; 6] = ["ureq::get(", "ureq::post(", "ureq::put(", "ureq::request(", "ureq::agent(", "ureq::AgentBuilder"];
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        let mut found = Vec::new();
        let mut stack = vec![crates.clone()];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if !matches!(name, "target" | "fixtures" | "tests" | "mind-net") {
                        stack.push(p);
                    }
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("rs") || p.file_name().and_then(|n| n.to_str()) == Some("tests.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap();
                let lines: Vec<&str> = text.lines().collect();
                for (i, line) in lines.iter().enumerate() {
                    // Stop at the file's test MODULE -- a `#[cfg(test)]` that opens a `mod`. An
                    // item-level `#[cfg(test)]` further up (a test-only helper) does not end the
                    // scan: stopping there hid real code below it (E.EGRESS1's first scan).
                    let opens_mod = lines.get(i + 1).is_some_and(|n| {
                        let n = n.trim_start();
                        n.starts_with("mod ") || n.starts_with("pub mod ") || n.starts_with("pub(crate) mod ")
                    });
                    if line.trim_start().starts_with("#[cfg(test)]") && opens_mod {
                        break;
                    }
                    let t = line.trim_start();
                    if t.starts_with("//") {
                        continue;
                    }
                    if BARE.iter().any(|b| line.contains(b)) {
                        found.push(format!("{}:{}", p.strip_prefix(&crates).unwrap().display(), i + 1));
                    }
                }
            }
        }
        assert!(found.is_empty(), "a request can go around the egress proxy: {found:?}");
    }

    #[test]
    fn no_proxy_set_means_nothing_changes() {
        assert!(goes_direct("https://api.open-meteo.com/", &env_of(&[])));
        assert!(goes_direct("https://api.open-meteo.com/", &env_of(&[("HTTPS_PROXY", "  ")])), "blank is not set");
    }

    #[test]
    fn hosts_are_read_the_way_urls_write_them() {
        assert_eq!(host_of("https://user:pw@API.Example.com:8443/path?q"), "api.example.com");
        assert_eq!(host_of("http://[::1]:7440/mcp"), "::1");
        assert_eq!(host_of("example.com/x"), "example.com");
        assert!(no_proxy_matches("a.b.internal", "*.internal") && no_proxy_matches("x.com", "*"));
        assert!(!no_proxy_matches("notexample.com", "example.com"), "a suffix is a label boundary");
    }
}
