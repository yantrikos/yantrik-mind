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

/// E.NET1h (the review's N1): a URL's host and port exactly as ureq will read them -- `url::Url`
/// (WHATWG), never a hand split. `%2e`, a backslash before `@`, full-width letters and a tab before
/// the port were judged as one host and fetched as another. The host is lower-cased, without brackets
/// or a trailing dot. None when it does not parse.
pub fn url_host_port(url: &str) -> Option<(String, u16)> {
    let u = url::Url::parse(url.trim()).ok()?;
    let host = match u.host()? {
        url::Host::Domain(d) => d.trim_end_matches('.').to_ascii_lowercase(),
        url::Host::Ipv4(a) => a.to_string(),
        url::Host::Ipv6(a) => a.to_string(),
    };
    Some((host, u.port_or_known_default().unwrap_or(80)))
}

/// The host part of a URL ("" when it does not parse).
fn host_of(url: &str) -> String {
    url_host_port(url).map(|(h, _)| h).unwrap_or_default()
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

/// E.NET1b: the private and special-use ranges, from the ONE list the browser guard reads too
/// (`deploy/private_ranges.json`, pinned to its twin in the OS's egress service) -- (network, mask)
/// pairs, parsed once. E.NET1e: moved here from mind-tools, so routing judges by the same list.
fn special_ranges() -> &'static (Vec<(u32, u32)>, Vec<(u128, u128)>) {
    static RANGES: OnceLock<(Vec<(u32, u32)>, Vec<(u128, u128)>)> = OnceLock::new();
    RANGES.get_or_init(|| {
        let list: serde_json::Value =
            serde_json::from_str(include_str!("../../../deploy/private_ranges.json")).expect("deploy/private_ranges.json parses");
        let cidrs = |key: &str| -> Vec<(String, u32)> {
            list[key]
                .as_array()
                .expect("a list of ranges")
                .iter()
                .map(|c| {
                    let (net, bits) = c.as_str().expect("a range").split_once('/').expect("a /prefix");
                    (net.to_string(), bits.parse().expect("a prefix length"))
                })
                .collect()
        };
        let v4 = cidrs("v4")
            .into_iter()
            .map(|(net, bits)| {
                let mask = if bits == 0 { 0 } else { u32::MAX << (32 - bits) };
                (u32::from(net.parse::<std::net::Ipv4Addr>().expect("a v4 range")) & mask, mask)
            })
            .collect();
        let v6 = cidrs("v6")
            .into_iter()
            .map(|(net, bits)| {
                let mask = if bits == 0 { 0 } else { u128::MAX << (128 - bits) };
                (u128::from(net.parse::<std::net::Ipv6Addr>().expect("a v6 range")) & mask, mask)
            })
            .collect();
        (v4, v6)
    })
}

/// Is `ip` private, loopback, link-local or otherwise not the internet? E.NET1b (the ninth pass): an
/// IPv4-mapped IPv6 address (`::ffff:192.168.4.7`) is judged by the IPv4 it carries.
pub fn is_special_ip(ip: std::net::IpAddr) -> bool {
    let (v4, v6) = special_ranges();
    let special_v4 = |a: std::net::Ipv4Addr| {
        let n = u32::from(a);
        a.is_broadcast() || v4.iter().any(|(net, mask)| n & mask == *net)
    };
    match ip {
        std::net::IpAddr::V4(a) => special_v4(a),
        std::net::IpAddr::V6(a) => match a.to_ipv4_mapped() {
            Some(inner) => special_v4(inner),
            None => {
                let n = u128::from(a);
                v6.iter().any(|(net, mask)| n & mask == *net)
            }
        },
    }
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

/// E.NET1c: a URL's host and port (the scheme's when unwritten). E.NET1h: through `url::Url`; a
/// configured value written without a scheme is read as http.
fn host_port(url: &str) -> (String, u16) {
    url_host_port(url)
        .or_else(|| (!url.contains("://")).then(|| url_host_port(&format!("http://{}", url.trim()))).flatten())
        .unwrap_or_default()
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
fn goes_direct(url: &str, get: &dyn Fn(&str) -> Option<String>, enforced: bool) -> bool {
    if proxy_url(get).is_none() {
        return true;
    }
    let host = host_of(url);
    if configured_endpoint(url, get) {
        // E.NET1e (yantrik-os #662): the OS's rules let the Mind's account reach the LAN only at the
        // literal addresses the person opened, so only a literal private address goes around the
        // proxy. A public address always takes it; a name goes direct only while no OS rules are in
        // force (under them it would need DNS the Mind may not have, and the person's LAN rules are
        // the proxy's to apply).
        match host.parse::<std::net::IpAddr>() {
            Ok(ip) if is_special_ip(ip) => return true,
            Ok(_) => {}
            Err(_) if !enforced => return true,
            Err(_) => {}
        }
    }
    if host.is_empty() || is_loopback(&host) {
        return true;
    }
    let no_proxy = get("NO_PROXY").or_else(|| get("no_proxy")).unwrap_or_default();
    no_proxy_matches(&host, &no_proxy)
}

fn env(k: &str) -> Option<String> {
    person_var(k).ok()
}

/// E.EGRESS5b (the eleventh pass): the settings only the PERSON may set -- what may leave, where
/// searches and models are, how traffic is routed, and which browser code runs. The Mind's own env
/// file is inside the Mind's account, so the Mind (or a model driving a settings screen) could
/// rewrite it; these are read from the root-owned [`PERSON_FILE`] instead, once it exists.
pub const PERSON_ONLY_KEYS: &[&str] = &[
    "YM_SHAREABLE_FACTS",
    "YM_WORK_RADAR",
    "YM_SEARXNG_URL",
    "YM_SEARXNG_CATEGORIES",
    "YM_HA_URL",
    "YM_LOCAL_OLLAMA_URL",
    "YM_OLLAMA_LOCAL_URL",
    "YM_NIM_BASE_URL",
    "YM_FACE_ML_URL",
    "YM_CRITIC_URL",
    "YM_WEFT_URL",
    "YM_IMMICH_URL",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "all_proxy",
    "no_proxy",
    "YM_WEB_READER",
    "YM_HEADLESS_SCRIPT",
    "PLAYWRIGHT_BROWSERS_PATH",
    // E.EGRESS5d (the thirteenth pass): the other browser scripts -- pointing one at an unguarded copy
    // would bypass net_guard.
    "YM_BROWSER_AGENT",
    "YM_HEADFUL_SCRIPT",
    "YM_SNAP_SCRIPT",
    // E.NET1g: the media tools -- a wrapper could drop the proxy they are given.
    "YM_YTDLP_BIN",
    "YM_FFMPEG_BIN",
];

/// E.EGRESS5d (the thirteenth pass): what may leave the Mind is the person's word alone -- these are
/// unset until the person's file exists, never taken from the Mind's own env.
pub const FAIL_CLOSED_KEYS: &[&str] = &["YM_SHAREABLE_FACTS", "YM_WORK_RADAR"];

/// E.NET1g (review L3): which code runs is the person's word too -- pointing a browser script or a
/// media binary at another copy would bypass net_guard or the proxy. Without the person's file these
/// are unset, so their callers use their BUILT-IN defaults, never the Mind's env.
pub const BUILT_IN_DEFAULT_KEYS: &[&str] = &[
    "YM_HEADLESS_SCRIPT",
    "YM_HEADFUL_SCRIPT",
    "YM_SNAP_SCRIPT",
    "YM_BROWSER_AGENT",
    "PLAYWRIGHT_BROWSERS_PATH",
    "YM_YTDLP_BIN",
    "YM_FFMPEG_BIN",
];

/// E.EGRESS5b: where the OS writes the person's settings -- root:root 0644, made by a root helper from
/// the person's own Settings, never writable by the Mind's account.
pub const PERSON_FILE: &str = "/etc/yantrik/mind-person.env";

/// E.EGRESS5b: `std::env::var` for every setting the Mind reads -- a person-only key comes ONLY from
/// [`PERSON_FILE`] when that file exists (absent there = unset; a file the Mind's account could
/// write, or cannot read, = unset); until the OS ships the file, from the environment, said once.
pub fn person_var(key: &str) -> Result<String, std::env::VarError> {
    person_var_from(key, std::path::Path::new(PERSON_FILE), &|k| std::env::var(k))
}

/// E.EGRESS5b: [`person_var`] against a given file and environment (for tests).
pub fn person_var_from(
    key: &str,
    file: &std::path::Path,
    env: &dyn Fn(&str) -> Result<String, std::env::VarError>,
) -> Result<String, std::env::VarError> {
    if !PERSON_ONLY_KEYS.contains(&key) {
        return env(key);
    }
    match read_root_file(file) {
        Ok(text) => env_file_value(&text, key).ok_or(std::env::VarError::NotPresent),
        // No file yet (an OS before it ships it): what may leave stays unset, and code paths take
        // their built-in defaults; routing and endpoints come from the env for now, said once.
        // REMOVE this env fallback once yantrik-os ships /etc/yantrik/mind-person.env (4c: after the
        // egress status file) -- from then on the file always exists (E.NET1g, review L3).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if FAIL_CLOSED_KEYS.contains(&key) || BUILT_IN_DEFAULT_KEYS.contains(&key) {
                return Err(std::env::VarError::NotPresent);
            }
            static SAID: std::sync::Once = std::sync::Once::new();
            SAID.call_once(|| {
                eprintln!("[settings] {} is absent: routing and endpoint settings come from the Mind's own env for now", file.display())
            });
            env(key)
        }
        Err(e) => {
            eprintln!("[settings] {} is not safe to trust ({e}) -- person-only settings are unset", file.display());
            Err(std::env::VarError::NotPresent)
        }
    }
}

/// E.NET1e (yantrik-os #662): where the OS says its kernel egress rules for the Mind's account are
/// loaded -- root:root 0644 in its own root:root 0755 folder, written by `yantrik-update mind-egress
/// apply` only after the rules took. (E.NET1g: moved out of /run/yantrik, which another root service
/// makes 0700.) It says what was LAST loaded: a hand-run `nft flush ruleset` leaves it stale until the
/// next apply -- the proxy's own refusal still holds then.
pub const EGRESS_SIGNAL: &str = "/run/yantrik-mind-egress/mind-egress.json";

/// E.NET1e: what the OS vouches for about the egress proxy, read ONCE per decision (E.NET1g, L1: a
/// signal read twice could flip a request judged proxied to direct and unchecked in between).
#[derive(Clone, Debug, PartialEq)]
pub struct EgressTrust {
    /// E.NET1g (M1): the hosts and ports of every LAN rule, seeded and the person's -- the proxy
    /// lets a request matching one reach the LAN by NAME. `None`: the OS could not list them (over
    /// its cap, or unreadable), so any name may be one.
    lan: Option<Vec<(String, Vec<u16>)>>,
    /// E.NET1j (yantrik-os #666): the OS's PUBLIC-only door ("http://127.0.0.1:7451"), which never
    /// honours a LAN rule and refuses every non-internet address. None: an older proxy without one.
    public: Option<String>,
}

impl EgressTrust {
    /// A trust with these LAN rules (the signal reader's, and tests').
    pub fn with_lan_rules(lan: Option<Vec<(String, Vec<u16>)>>) -> EgressTrust {
        EgressTrust { lan, public: None }
    }

    /// E.NET1j: the same trust with the OS's public door (tests).
    pub fn with_public_door(mut self, public: Option<String>) -> EgressTrust {
        self.public = public;
        self
    }

    /// E.NET1j: the public-only door, as a proxy URL, when the OS has one.
    pub fn public_door(&self) -> Option<&str> {
        self.public.as_deref()
    }

    /// E.NET1g (M1): may a request to `url`, whose host did not resolve here, be left to the proxy?
    /// Not when a LAN rule covers it (the proxy would let it into the LAN by name), not when the OS
    /// could not say which names those are, and not when it is the host of an endpoint the person
    /// configured (any port).
    pub fn leaves_to_proxy(&self, url: &str) -> bool {
        self.leaves_to_proxy_with(url, &env)
    }

    fn leaves_to_proxy_with(&self, url: &str, get: &dyn Fn(&str) -> Option<String>) -> bool {
        let (host, port) = host_port(url);
        let host = host.trim_end_matches('.');
        // E.NET1i (P6): a URL that does not parse is never left to the proxy.
        if host.is_empty() {
            return false;
        }
        let Some(lan) = &self.lan else {
            return false;
        };
        // Any port (4c, #665's review): the proxy's own match is host AND port, but a LAN service's
        // other ports are no business of a fetch from outside either.
        let _ = port;
        if lan.iter().any(|(rule, _ports)| lan_rule_covers(rule, host)) {
            return false;
        }
        !DIRECT_ENDPOINT_VARS
            .iter()
            .filter_map(|k| get(k))
            .filter(|v| !v.trim().is_empty())
            .any(|v| host_port(v.trim()).0 == host)
    }
}

/// Does a LAN rule's host cover `host`? `*.example.com` as a suffix -- every name under it, and (wider
/// than the proxy's own match, which is safe here) `example.com` itself; anything else is the one name.
fn lan_rule_covers(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(domain) => {
            host == domain
                || (host.len() > domain.len() + 1 && host.ends_with(domain) && host[..host.len() - domain.len()].ends_with('.'))
        }
        None => pattern == host,
    }
}

/// E.NET1e: may the Mind leave the private-range check to the egress proxy? Only when the OS says, in
/// a file only root can write, that its rules are enforced and that the proxy refuses private ranges
/// -- and that proxy is the very one the Mind routes through -- and (E.NET1g) says which names its LAN
/// rules let in. Anything else (no file, an unsafe file, another version, another proxy, no
/// `lan_hosts`) is no: the Mind checks for itself, as before.
pub fn egress_trust() -> Option<EgressTrust> {
    egress_trust_from(std::path::Path::new(EGRESS_SIGNAL), &env)
}

fn egress_trust_from(file: &std::path::Path, get: &dyn Fn(&str) -> Option<String>) -> Option<EgressTrust> {
    let text = read_root_file(file).ok()?;
    let said = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    let same_proxy = match (said["proxy"].as_str(), proxy_url(get)) {
        (Some(theirs), Some(ours)) => theirs.trim().trim_end_matches('/') == ours.trim_end_matches('/'),
        _ => false,
    };
    // E.NET1j: version 3 adds the public door; 2 is still read. Any other version is not.
    let version = said["version"].as_u64();
    if !(matches!(version, Some(2) | Some(3)) && said["enforced"] == true && said["proxy_refuses_private"] == true && same_proxy) {
        return None;
    }
    // A v3 signal must say whether there is a public door: a URL like `proxy` ("http://127.0.0.1:7451",
    // per #666's review) on a loopback address with its port written and nothing else -- or null.
    let public = if version == Some(3) {
        match said.get("public_proxy")? {
            serde_json::Value::Null => None,
            serde_json::Value::String(door) => {
                let u = url::Url::parse(door.trim()).ok()?;
                let ip: std::net::IpAddr = match u.host()? {
                    url::Host::Ipv4(a) => a.into(),
                    url::Host::Ipv6(a) => a.into(),
                    url::Host::Domain(_) => return None,
                };
                let bare = u.username().is_empty() && u.password().is_none() && u.query().is_none() && u.fragment().is_none() && u.path() == "/";
                if u.scheme() != "http" || !ip.is_loopback() || !bare {
                    return None;
                }
                Some(format!("http://{}", std::net::SocketAddr::new(ip, u.port()?)))
            }
            _ => return None,
        }
    } else {
        None
    };
    // `lan_hosts` must be there: a list of {host, ports}, or null when the OS could not list them.
    let lan = match said.get("lan_hosts")? {
        serde_json::Value::Null => None,
        serde_json::Value::Array(rules) => Some(
            rules
                .iter()
                .map(|r| {
                    let host = r["host"].as_str()?.trim().trim_end_matches('.').to_ascii_lowercase();
                    let ports = r["ports"].as_array()?.iter().map(|p| p.as_u64().and_then(|p| u16::try_from(p).ok())).collect::<Option<Vec<u16>>>()?;
                    Some((host, ports))
                })
                .collect::<Option<Vec<_>>>()?,
        ),
        _ => return None,
    };
    Some(EgressTrust { lan, public })
}

/// E.EGRESS5d (the thirteenth pass): read a root-owned file without a race -- its folder must be a
/// root-owned directory nobody else can write; the file is opened without following a link,
/// checked on the open handle (a regular file, owned by root, nobody else can write), and read from
/// that same handle. Absent: NotFound. Anything unsafe: an error. (The person's settings, and since
/// E.NET1e the OS's egress signal.)
fn read_root_file(file: &std::path::Path) -> std::io::Result<String> {
    #[cfg(unix)]
    {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let unsafe_file = |why: &str| std::io::Error::new(std::io::ErrorKind::PermissionDenied, why.to_string());
        let dir = file.parent().unwrap_or(std::path::Path::new("/"));
        let d = std::fs::symlink_metadata(dir)?;
        if !d.is_dir() || d.uid() != 0 || d.gid() != 0 || d.mode() & 0o022 != 0 {
            return Err(unsafe_file("its folder is not a root:root directory only root can write"));
        }
        let mut f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(file)?;
        let m = f.metadata()?;
        if !m.is_file() || m.uid() != 0 || m.mode() & 0o022 != 0 {
            return Err(unsafe_file("it is not a root-owned regular file only root can write"));
        }
        let mut text = String::new();
        f.read_to_string(&mut text)?;
        Ok(text)
    }
    #[cfg(not(unix))]
    {
        std::fs::read_to_string(file)
    }
}

/// E.EGRESS5b: one `KEY=value` from an env file (comments, blank lines, `export ` and quotes allowed).
fn env_file_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (k, v) = line.split_once('=')?;
        if line.starts_with('#') || k.trim() != key {
            return None;
        }
        let v = v.trim();
        let v = v.strip_prefix('"').and_then(|x| x.strip_suffix('"')).or_else(|| v.strip_prefix('\'').and_then(|x| x.strip_suffix('\''))).unwrap_or(v);
        Some(v.to_string())
    })
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
    is_direct_under(url, egress_trust().as_ref())
}

/// E.NET1g (L1): [`is_direct`] under a trust already read -- decide once, then build with
/// [`route_decided`].
pub fn is_direct_under(url: &str, trust: Option<&EgressTrust>) -> bool {
    goes_direct(url, &env, trust.is_some())
}

/// E.NET1g (M2): the proxy the Mind's children must use, if one is configured.
pub fn configured_proxy() -> Option<String> {
    proxy_url(&env)
}

/// The agent a request to `url` should use.
pub fn agent_for(url: &str) -> &'static ureq::Agent {
    if is_direct(url) {
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
    route_decided(builder, is_direct(url))
}

/// E.NET1j (yantrik-os #666): a request for an address from OUTSIDE -- a page, the model, a search
/// result, a paper link -- goes through the OS's public-only door when it has one, which never lets a
/// request into the LAN whatever its name; otherwise exactly as [`route_decided`].
pub fn route_outside(builder: ureq::AgentBuilder, direct: bool, trust: Option<&EgressTrust>) -> ureq::Agent {
    if direct {
        return builder.build();
    }
    match trust.and_then(|t| t.public_door()).and_then(|p| ureq::Proxy::new(p).ok()) {
        Some(p) => builder.proxy(p).build(),
        None => route_decided(builder, false),
    }
}

/// E.NET1j: the proxy a child fetching addresses from outside (yt-dlp, ffmpeg) must be given -- the
/// public door when the OS has one, else the configured proxy.
pub fn outside_proxy() -> Option<String> {
    egress_trust().and_then(|t| t.public).or_else(configured_proxy)
}

/// E.NET1g (L1): `builder` on a route already decided -- direct, or through the proxy when one is set.
pub fn route_decided(builder: ureq::AgentBuilder, direct: bool) -> ureq::Agent {
    if direct {
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

    /// The routing tests before E.NET1e: no OS rules in force.
    fn goes_direct(url: &str, get: &dyn Fn(&str) -> Option<String>) -> bool {
        super::goes_direct(url, get, false)
    }

    /// E.NET1g (M1): a name that did not resolve here is left to the proxy only when no LAN rule
    /// covers it (host AND port, the proxy's own matching), the OS listed those rules, and it is no
    /// configured endpoint's host.
    #[test]
    fn a_name_the_lan_rules_cover_is_never_left_to_the_proxy() {
        let e = env_of(&[("HTTPS_PROXY", "http://127.0.0.1:7450"), ("YM_NIM_BASE_URL", "https://models.example.net/v1")]);
        let lan = vec![("gpu.example.ts.net".to_string(), vec![11434]), ("*.home.arpa".to_string(), vec![80, 8123])];
        let t = EgressTrust::with_lan_rules(Some(lan));
        let leaves = |url: &str| t.leaves_to_proxy_with(url, &e);
        assert!(!leaves("http://gpu.example.ts.net:11434/api/tags"), "a seeded LAN name went to the proxy");
        assert!(!leaves("http://GPU.example.ts.net.:11434/"), "case or a trailing dot slipped a LAN name through");
        assert!(!leaves("http://ha.home.arpa:8123/api/states"), "a wildcard LAN rule did not cover its subdomain");
        assert!(!leaves("http://a.b.home.arpa/"), "a wildcard did not cover a deeper name");
        assert!(!leaves("http://gpu.example.ts.net:22/"), "a LAN rule's host on another port went to the proxy");
        assert!(!leaves("http://home.arpa/"), "`*.home.arpa` did not cover the domain itself");
        assert!(leaves("http://evilhome.arpa/"), "a name that only ends the same was refused");
        assert!(!leaves("https://models.example.net/anything"), "a configured endpoint's host went to the proxy");
        assert!(leaves("https://news.example.org/a"), "an ordinary name was refused");
        // E.NET1h (N1): the forms a hand split read as another host.
        for odd in [
            "http://gpu%2eexample.ts.net:11434/api/tags",
            "http://gpu.example.ts.net:11434\\@news.example.org/",
            "http://\u{ff47}\u{ff50}\u{ff55}.example.ts.net:11434/",
            "http://gpu.example.ts.net\t:11434/",
            "https://models%2eexample.net/v1",
        ] {
            assert!(!leaves(odd), "{odd:?} went to the proxy under another name");
        }
        assert!(!EgressTrust::with_lan_rules(None).leaves_to_proxy_with("https://news.example.org/a", &e), "with no list, any name may be LAN");
        assert!(!leaves("http://exa mple.org/"), "an unparseable url was left to the proxy");
    }

    /// E.NET1e (yantrik-os #662): a configured endpoint goes around the proxy only at a literal
    /// private address; a public address never does; a name does only while no OS rules are in force.
    #[test]
    fn configured_endpoints_go_direct_only_where_the_os_rules_allow() {
        let e = env_of(&[
            ("HTTPS_PROXY", "http://127.0.0.1:7450"),
            ("YM_SEARXNG_URL", "http://192.168.4.42:8888"),
            ("YM_HA_URL", "http://[fd00::10]:8123"),
            ("YM_NIM_BASE_URL", "https://integrate.example.com/v1"),
            ("YM_CRITIC_URL", "http://8.8.8.8:9000"),
        ]);
        for enforced in [false, true] {
            let direct = |url: &str| super::goes_direct(url, &e, enforced);
            assert!(direct("http://192.168.4.42:8888/search?q=x"), "enforced={enforced}: the LAN SearXNG took the proxy");
            assert!(direct("http://[fd00::10]:8123/api/states"), "enforced={enforced}: a v6 LAN endpoint took the proxy");
            assert!(!direct("http://8.8.8.8:9000/"), "enforced={enforced}: a public address went around the proxy");
            assert_eq!(direct("https://integrate.example.com/v1/chat/completions"), !enforced, "enforced={enforced}: a named endpoint");
        }
    }

    /// E.NET1e: the OS's egress signal is trusted only whole -- root's file in root's folder, version
    /// 1, enforced, the proxy refusing private ranges, and the proxy the Mind routes through.
    #[test]
    fn the_egress_signal_is_trusted_only_when_whole() {
        let dir = std::env::temp_dir().join(format!("ym-egress-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mind-egress.json");
        let ours = env_of(&[("HTTPS_PROXY", "http://127.0.0.1:7450")]);
        let say = |json: &str| std::fs::write(&file, json).unwrap();
        let whole = r#"{"enforced":true,"table":"inet yantrik_mind_egress","proxy":"http://127.0.0.1:7450","proxy_refuses_private":true,"mode":"enforce","private":false,"dns_allowed":false,"loaded_at":1790000000,"lan_hosts":[{"host":"homeassistant.local","ports":[8123]}],"version":2}"#;
        let _ = std::fs::remove_file(&file);
        assert!(!egress_trust_from(&file, &ours).is_some(), "trusted with no file");
        say(whole);
        assert_eq!(
            egress_trust_from(&file, &ours),
            Some(EgressTrust::with_lan_rules(Some(vec![("homeassistant.local".to_string(), vec![8123])]))),
            "the whole signal was not trusted, or its LAN rules were lost"
        );
        // E.NET1j: v3 carries the public door; null is an older proxy without one.
        say(&whole.replace(r#""version":2"#, r#""version":3,"public_proxy":"http://127.0.0.1:7451""#));
        assert_eq!(egress_trust_from(&file, &ours).and_then(|t| t.public_door().map(str::to_string)).as_deref(), Some("http://127.0.0.1:7451"));
        say(&whole.replace(r#""version":2"#, r#""version":3,"public_proxy":null"#));
        assert_eq!(egress_trust_from(&file, &ours).map(|t| t.public_door().is_none()), Some(true), "a null public door");
        say(&whole.replace(r#"[{"host":"homeassistant.local","ports":[8123]}]"#, "null"));
        assert_eq!(egress_trust_from(&file, &ours), Some(EgressTrust::with_lan_rules(None)), "lan_hosts: null is a trust with no list");
        assert!(!egress_trust_from(&file, &env_of(&[])).is_some(), "trusted with no proxy configured");
        assert!(!egress_trust_from(&file, &env_of(&[("HTTPS_PROXY", "http://10.0.0.9:3128")])).is_some(), "trusted for another proxy");
        for (from, to, why) in [
            (r#""enforced":true"#, r#""enforced":false"#, "not enforced"),
            (r#""proxy_refuses_private":true"#, r#""proxy_refuses_private":false"#, "a proxy that lets private ranges through"),
            (r#""version":2"#, r#""version":1"#, "the version before lan_hosts"),
            (r#""version":2"#, r#""version":4"#, "an unknown version"),
            (r#""version":2"#, r#""version":3"#, "a v3 signal saying nothing of a public door"),
            (r#""version":2"#, r#""version":3,"public_proxy":"http://10.0.0.5:7451""#, "a public door that is not this machine"),
            (r#""version":2"#, r#""version":3,"public_proxy":"127.0.0.1:7451""#, "a public door that is not a URL"),
            (r#""version":2"#, r#""version":3,"public_proxy":"http://127.0.0.1""#, "a public door without its port"),
            (r#""version":2"#, r#""version":3,"public_proxy":"http://proxy.lan:7451""#, "a public door that is not an address"),
            (r#""lan_hosts":[{"host":"homeassistant.local","ports":[8123]}],"#, "", "no lan_hosts"),
            (r#""ports":[8123]"#, r#""ports":["8123"]"#, "a malformed LAN rule"),
            (r#""proxy":"http://127.0.0.1:7450","#, "", "no proxy named"),
        ] {
            say(&whole.replace(from, to));
            assert!(!egress_trust_from(&file, &ours).is_some(), "trusted: {why}");
        }
        say("enforced: true");
        assert!(!egress_trust_from(&file, &ours).is_some(), "trusted: not JSON");
        say(whole);
        #[cfg(unix)]
        {
            // (Run as root on staging, so the files are root's.)
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
            assert!(!egress_trust_from(&file, &ours).is_some(), "a signal others can write was trusted");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert!(!egress_trust_from(&file, &ours).is_some(), "a signal in a folder others can write was trusted");
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            // E.NET1g (#665): the folder's group must be root's too.
            std::os::unix::fs::chown(&dir, None, Some(1)).unwrap();
            assert!(!egress_trust_from(&file, &ours).is_some(), "a signal in a folder of another group was trusted");
            std::os::unix::fs::chown(&dir, None, Some(0)).unwrap();
            let link = dir.join("linked.json");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(!egress_trust_from(&link, &ours).is_some(), "a link to the signal was followed");
            assert!(egress_trust_from(&file, &ours).is_some());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// E.NET1j: an address from outside reaches the public door when the OS has one; with none, the
    /// route is exactly the decided one.
    #[test]
    fn an_outside_address_goes_through_the_public_door() {
        use std::io::{BufRead, Write};
        let door = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = door.local_addr().unwrap().port();
        door.set_nonblocking(true).unwrap();
        // A door that waits at most 5 s: a request that goes elsewhere fails the test, never hangs it.
        let seen = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                match door.accept() {
                    Ok((s, _)) => {
                        s.set_nonblocking(false).unwrap();
                        let mut line = String::new();
                        std::io::BufReader::new(s.try_clone().unwrap()).read_line(&mut line).unwrap();
                        let mut s = s;
                        let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        return line;
                    }
                    Err(_) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
                    Err(_) => return String::new(),
                }
            }
        });
        let trust = EgressTrust::with_lan_rules(Some(vec![])).with_public_door(Some(format!("http://127.0.0.1:{port}")));
        let _ = route_outside(builder().timeout(std::time::Duration::from_secs(3)), false, Some(&trust)).get("http://news.example.test/a").call();
        assert_eq!(seen.join().unwrap().trim_end(), "GET http://news.example.test/a HTTP/1.1", "the outside address missed the public door");
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

    /// E.EGRESS5b (the eleventh pass): a person-only key comes only from the person's file once it
    /// exists -- the Mind's own env is ignored for it -- and from the env only while there is no file.
    #[test]
    fn person_only_settings_come_from_the_persons_file() {
        let dir = std::env::temp_dir().join(format!("ym-person-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("mind-person.env");
        let env = |k: &str| match k {
            "YM_SHAREABLE_FACTS" => Ok("weather: Attacker City".to_string()),
            "YM_SEARXNG_URL" => Ok("http://192.168.4.42:8888".to_string()),
            "YM_SNAP_SCRIPT" => Ok("/tmp/unguarded_snap.js".to_string()),
            "YM_YTDLP_BIN" => Ok("/tmp/yt-dlp-without-proxy".to_string()),
            "YM_MAX_STEPS" => Ok("40".to_string()),
            _ => Err(std::env::VarError::NotPresent),
        };
        let _ = std::fs::remove_file(&file);
        // E.EGRESS5d: with no file, what may leave stays unset; routing still comes from the env.
        assert_eq!(person_var_from("YM_SHAREABLE_FACTS", &file, &env), Err(std::env::VarError::NotPresent), "no file: the Mind's env set what may leave");
        assert_eq!(person_var_from("YM_SEARXNG_URL", &file, &env).as_deref(), Ok("http://192.168.4.42:8888"), "no file: routing lost its endpoint");
        std::fs::write(&file, "# person settings\nexport YM_SHAREABLE_FACTS=\"weather: Bentonville\"\nYM_WORK_RADAR=on\n").unwrap();
        assert_eq!(person_var_from("YM_SHAREABLE_FACTS", &file, &env).as_deref(), Ok("weather: Bentonville"), "the Mind's env overrode the person");
        assert_eq!(person_var_from("YM_SEARXNG_URL", &file, &env), Err(std::env::VarError::NotPresent), "a person-only key absent from the file came from elsewhere");
        assert_eq!(person_var_from("YM_MAX_STEPS", &file, &env).as_deref(), Ok("40"), "an ordinary key stopped reading the env");
        // E.EGRESS5d: the browser scripts are the person's too -- an unguarded copy cannot be swapped in.
        assert_eq!(person_var_from("YM_SNAP_SCRIPT", &file, &env), Err(std::env::VarError::NotPresent), "a browser script path came from the Mind's env");
        // E.NET1g (L3): without the person's file too -- the built-in default, never the env.
        let _ = std::fs::rename(&file, dir.join("aside.env"));
        assert_eq!(person_var_from("YM_SNAP_SCRIPT", &file, &env), Err(std::env::VarError::NotPresent), "no file: a browser script came from the Mind's env");
        assert_eq!(person_var_from("YM_YTDLP_BIN", &file, &env), Err(std::env::VarError::NotPresent), "no file: a media binary came from the Mind's env");
        assert_eq!(person_var_from("YM_SEARXNG_URL", &file, &env).as_deref(), Ok("http://192.168.4.42:8888"), "no file: routing lost its endpoint");
        let _ = std::fs::rename(dir.join("aside.env"), &file);
        #[cfg(unix)]
        {
            // (Run as root on staging, so the files are root's.) A file others can write, a folder
            // others can write, and a link to the file are each not trusted.
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
            assert_eq!(person_var_from("YM_SHAREABLE_FACTS", &file, &env), Err(std::env::VarError::NotPresent), "a file others can write was trusted");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(person_var_from("YM_SHAREABLE_FACTS", &file, &env).as_deref(), Ok("weather: Bentonville"));
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert_eq!(person_var_from("YM_SHAREABLE_FACTS", &file, &env), Err(std::env::VarError::NotPresent), "a folder others can write was trusted");
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
            let link = dir.join("linked.env");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert_eq!(person_var_from("YM_SHAREABLE_FACTS", &link, &env), Err(std::env::VarError::NotPresent), "a link to the file was followed");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// E.EGRESS5d: getters that read a person-only key, each allowed only with the production call that
    /// hands it `person_var` present in the same file.
    const GETTERS: [(&str, &str, &str); 2] = [
        ("mind-tools/src/lib.rs", "YM_WEB_READER", "reader_allowed(&|k| mind_net::person_var(k).ok())"),
        ("mind-inference/src/lib.rs", "YM_LOCAL_OLLAMA_URL", "local_backend_from(&|k| mind_net::person_var(k).ok())"),
    ];
    /// E.EGRESS5d: places that only NAME a key (shown to the person), never read it.
    const METADATA: [(&str, &str); 1] = [("mind-conversation/src/plugins_mod.rs", "YM_FACE_ML_URL")];

    /// E.EGRESS5b: no code outside this crate reads a person-only key straight from the env -- every
    /// read goes through `person_var`. (The eval CLI and a live test read their own config.)
    #[test]
    fn person_only_keys_are_read_only_through_person_var() {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        const EXEMPT: [&str; 3] = ["mind-net", "mind-evals", "weft_live.rs"];
        let mut stack = vec![crates.to_path_buf()];
        let mut found = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    if !p.ends_with("target") {
                        stack.push(p);
                    }
                    continue;
                }
                let name = p.to_string_lossy().replace('\\', "/");
                if !name.ends_with(".rs") || EXEMPT.iter().any(|e| name.contains(e)) || name.ends_with("tests.rs") {
                    continue;
                }
                let full = std::fs::read_to_string(&p).unwrap_or_default();
                // Test code may set and read anything.
                let src = full.split("#[cfg(test)]").next().unwrap_or("");
                // E.EGRESS5d (the thirteenth pass): not only `env::var("KEY")` -- ANY mention of a
                // person-only key outside this crate is a read in disguise (var_os, a key held in a
                // variable, a getter), unless it is one of the forms below.
                for key in PERSON_ONLY_KEYS {
                    let quoted = format!("\"{key}\"");
                    for (at, _) in src.match_indices(&quoted) {
                        let before = src[..at].trim_end();
                        let allowed_form = before.ends_with("person_var(") // the one way to read it
                            || before.ends_with(".env(") // handed to a child process, not read
                            || before.ends_with(".env_remove(") // taken away from a child, not read
                            || before.ends_with("upsert_env_line(&existing,") // setup WRITES the Mind's env;
                            || before.ends_with("upsert(&existing,") //  ignored once the person's file exists
                            || (name.ends_with("config_panel.rs") && before.ends_with("key:")); // the schema
                        // A getter is allowed only where its production caller in the same file hands
                        // it `person_var` -- the companion line must be there.
                        let companion = GETTERS
                            .iter()
                            .find(|(file, k, _)| name.ends_with(file) && k == key)
                            .is_some_and(|(_, _, line)| full.contains(line));
                        let metadata = METADATA.iter().any(|(file, k)| name.ends_with(file) && k == key);
                        if !allowed_form && !companion && !metadata {
                            found.push(format!("{name}: {key}"));
                        }
                    }
                }
            }
        }
        assert!(found.is_empty(), "person-only keys read around person_var: {found:?}");
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
        // E.NET1h (N1): read as ureq reads it -- a string with no scheme is no URL ureq would fetch.
        assert_eq!(host_of("example.com/x"), "");
        for odd in [
            "http://gpu%2eexample.ts.net:11434/",
            "http://gpu.example.ts.net:11434\\@news.example.org/",
            "http://\u{ff47}\u{ff50}\u{ff55}.example.ts.net:11434/",
            "http://gpu.example.ts.net\t:11434/",
        ] {
            assert_eq!(url_host_port(odd), Some(("gpu.example.ts.net".to_string(), 11434)), "{odd:?} was read as another host");
        }
        assert!(no_proxy_matches("a.b.internal", "*.internal") && no_proxy_matches("x.com", "*"));
        assert!(!no_proxy_matches("notexample.com", "example.com"), "a suffix is a label boundary");
    }
}
