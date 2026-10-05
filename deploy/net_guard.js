// E.NET1 / E.NET1b: what the browser fallback may reach.
//
// headless_fetch.js and headful_fetch.js render pages an injected link may choose, with page script
// running. Without this, a page could redirect to, or script a request at, http://192.168.4.x (a
// router, Home Assistant) and the answer would come back as "web" text. Every request the page makes
// goes through `guardContext`:
//   - its host is resolved ONCE and refused when any address is in the shared private/special-use
//     list (private_ranges.json -- the same list mind-tools reads); an IPv4-mapped IPv6 address is
//     judged by the IPv4 it carries, and IPv6 is parsed into its 16 bytes, so [::ffff:7f00:1] and
//     [::7f00:1] are caught;
//   - it is then fetched HERE, by Node's own http(s) client connected to exactly the address that
//     was checked (a second lookup could be answered differently -- DNS rebinding), with no
//     redirect followed: a 3xx goes back to the page, and the browser's next request is checked again.
// Service workers are blocked (they would bypass routing); WebSockets are closed where Playwright can
// route them. Deploy this file and private_ranges.json beside the two scripts.
//
// E.NET1e (yantrik-os #662): under the OS's kernel egress rules the Mind's account may not connect
// out directly, and in enforce mode has no DNS. When the OS says -- in a file only root can write --
// that its rules are enforced and its proxy refuses private ranges, and that proxy is the one this
// process is configured with, a request goes THROUGH the proxy. It is still checked here first: an
// address, or a name that resolves here, is judged as above; only a name that does not resolve here is
// left to the proxy. Otherwise everything above holds unchanged.
const { AsyncLocalStorage } = require("async_hooks");
const dns = require("dns").promises;
const fs = require("fs");
const net = require("net");
const path = require("path");
const tls = require("tls");
const http = require("http");
const https = require("https");
const RANGES = require("./private_ranges.json");

const EGRESS_SIGNAL = "/run/yantrik-mind-egress/mind-egress.json";

const MAX_BODY = 20 * 1024 * 1024;

function v4ToInt(s) {
  const b = String(s).split(".").map(Number);
  if (b.length !== 4 || b.some((x) => !Number.isInteger(x) || x < 0 || x > 255)) return null;
  return BigInt(((b[0] << 24) >>> 0) + (b[1] << 16) + (b[2] << 8) + b[3]);
}

// IPv6 text -> its 128 bits (BigInt), or null when it is not IPv6.
function v6ToBigInt(text) {
  let s = String(text).toLowerCase().replace(/^\[|\]$/g, "");
  const zone = s.indexOf("%");
  if (zone >= 0) s = s.slice(0, zone);
  let tailV4 = null;
  const lastColon = s.lastIndexOf(":");
  if (lastColon >= 0 && s.slice(lastColon + 1).includes(".")) {
    tailV4 = v4ToInt(s.slice(lastColon + 1));
    if (tailV4 === null) return null;
    s = s.slice(0, lastColon + 1) + "0:0";
  }
  const halves = s.split("::");
  if (halves.length > 2) return null;
  const head = halves[0] ? halves[0].split(":") : [];
  const tail = halves.length === 2 && halves[1] ? halves[1].split(":") : [];
  let groups;
  if (halves.length === 2) {
    const fill = 8 - head.length - tail.length;
    if (fill < 0) return null;
    groups = [...head, ...Array(fill).fill("0"), ...tail];
  } else {
    groups = head;
  }
  if (groups.length !== 8) return null;
  let v = 0n;
  for (const g of groups) {
    if (!/^[0-9a-f]{1,4}$/.test(g)) return null;
    v = (v << 16n) | BigInt(parseInt(g, 16));
  }
  if (tailV4 !== null) v = (v & ~0xffffffffn) | tailV4;
  return v;
}

function cidrs(list, bits, parse) {
  return list.map((c) => {
    const [netText, len] = c.split("/");
    const n = BigInt(Number(len));
    const mask = n === 0n ? 0n : ((1n << n) - 1n) << (BigInt(bits) - n);
    return [parse(netText) & mask, mask];
  });
}
const V4 = cidrs(RANGES.v4, 32, v4ToInt);
const V6 = cidrs(RANGES.v6, 128, v6ToBigInt);

function v4Private(n) {
  return n === 0xffffffffn || V4.some(([netBits, mask]) => (n & mask) === netBits);
}

// Is this address private or special-use? Anything that is not a parseable IP fails closed.
function privateIp(ip) {
  const s = String(ip);
  if (net.isIPv4(s)) return v4Private(v4ToInt(s));
  const v = v6ToBigInt(s);
  if (v === null) return true;
  if (v >> 32n === 0xffffn) return v4Private(v & 0xffffffffn); // ::ffff:a.b.c.d
  return V6.some(([netBits, mask]) => (v & mask) === netBits);
}

// The addresses a host may be reached at, once -- refused (throws) when any is private, when the
// host does not resolve, or when it resolves to nothing.
async function checkedAddresses(hostname, deps = {}) {
  const lookup = deps.lookup || dns.lookup;
  const isPrivate = deps.privateIp || privateIp;
  const h = String(hostname).replace(/^\[|\]$/g, "");
  let addrs;
  if (net.isIP(h)) {
    addrs = [{ address: h, family: net.isIP(h) }];
  } else {
    try {
      addrs = await lookup(h, { all: true });
    } catch (_) {
      throw Object.assign(new Error(`blocked: ${h} does not resolve`), { unresolved: true });
    }
  }
  if (!addrs.length) throw Object.assign(new Error(`blocked: ${h} resolves to nothing`), { unresolved: true });
  if (addrs.some((a) => isPrivate(a.address))) throw new Error(`blocked: ${h} is a private address`);
  return addrs;
}

async function hostIsPrivate(hostname, lookup = dns.lookup) {
  try {
    await checkedAddresses(hostname, { lookup });
    return false;
  } catch (_) {
    return true;
  }
}

// The proxy this process is configured with, in the Mind's own order (mind-net's proxy_url).
function configuredProxy(env = process.env) {
  for (const k of ["ALL_PROXY", "all_proxy", "HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"]) {
    const v = String(env[k] || "").trim();
    if (v) return v;
  }
  return null;
}

// A root-owned file read without a race (mind-net's read_root_file): its folder a root-owned
// directory nobody else can write; the file opened without following a link and checked on the open
// descriptor (a regular file, root's, nobody else can write), then read from that descriptor.
function readRootFile(file, deps = {}) {
  const f = deps.fs || fs;
  const d = f.lstatSync(path.dirname(file));
  if (!d.isDirectory() || d.uid !== 0 || d.gid !== 0 || d.mode & 0o022) throw new Error("its folder is not root's alone");
  const fd = f.openSync(file, f.constants.O_RDONLY | (f.constants.O_NOFOLLOW || 0));
  try {
    const st = f.fstatSync(fd);
    if (!st.isFile() || st.uid !== 0 || st.mode & 0o022) throw new Error("it is not root's alone");
    return f.readFileSync(fd, "utf8");
  } finally {
    f.closeSync(fd);
  }
}

// E.NET1e: what the OS vouches for -- { proxy, lan } -- or null. Trusted only whole: version 2,
// enforced, the proxy refusing private ranges, the proxy this process is configured with, and
// (E.NET1g) `lan_hosts`: the LAN rules' hosts and ports, or null when the OS could not list them.
function egressTrust(deps = {}) {
  try {
    const said = JSON.parse(readRootFile(deps.signal || EGRESS_SIGNAL, deps));
    const ours = configuredProxy(deps.env || process.env);
    if (!ours || typeof said.proxy !== "string") return null;
    const same = said.proxy.trim().replace(/\/+$/, "") === ours.replace(/\/+$/, "");
    // E.NET1j (yantrik-os #666): version 3 adds the public-only door; 2 is still read.
    if (!((said.version === 2 || said.version === 3) && said.enforced === true && said.proxy_refuses_private === true && same)) return null;
    let pub = null;
    if (said.version === 3) {
      if (said.public_proxy !== null) {
        // A URL like `proxy` (#666's review): http, a loopback address, its port written, nothing else.
        // (A missing field is not a string either: a v3 signal must say.)
        if (typeof said.public_proxy !== "string") return null;
        let d;
        try {
          d = new URL(said.public_proxy.trim());
        } catch (_) {
          return null;
        }
        const h = d.hostname.replace(/^\[|\]$/g, "");
        const loop = /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(h) || h === "::1";
        const bare = !d.username && !d.password && !d.search && !d.hash && d.pathname === "/";
        if (d.protocol !== "http:" || !loop || !d.port || !bare) return null;
        // E.NET1k (L2): never the endpoint door itself.
        const theirs = new URL(ours);
        if (theirs.port === d.port && (theirs.hostname === "localhost" || theirs.hostname === d.hostname)) return null;
        pub = `http://${d.host}`;
      }
    }
    if (said.lan_hosts === null) return { proxy: ours, lan: null, public: pub };
    const port = (p) => Number.isInteger(p) && p > 0 && p < 65536;
    const rule = (r) => r && typeof r.host === "string" && Array.isArray(r.ports) && r.ports.every(port);
    if (!Array.isArray(said.lan_hosts) || !said.lan_hosts.every(rule)) return null;
    const lan = said.lan_hosts.map((r) => ({ host: r.host.trim().replace(/\.$/, "").toLowerCase(), ports: r.ports }));
    return { proxy: ours, lan, public: pub };
  } catch (_) {
    return null;
  }
}

// Does a LAN rule's host cover `host`? `*.example.com` as a suffix -- names under it and, wider than
// the proxy's own match (safe here), example.com itself; anything else is the one name.
function lanRuleCovers(pattern, host) {
  if (pattern.startsWith("*.")) {
    const d = pattern.slice(2);
    return host === d || (host.length > d.length + 1 && host.endsWith(d) && host.slice(0, host.length - d.length).endsWith("."));
  }
  return pattern === host;
}

// E.NET1g (review M1): may a name that did not resolve here go on to the proxy? Not when a LAN rule
// covers it -- the proxy would let it into the LAN by name -- nor when the OS could not list them.
// Any port (4c, #665's review): a LAN service's other ports are no business of a fetch from outside.
function leavesToProxy(trust, host) {
  if (!trust.lan) return false;
  const h = String(host).toLowerCase().replace(/\.$/, "");
  return !trust.lan.some((r) => lanRuleCovers(r.host, h));
}

function collect(r, resolve, reject) {
  return (res) => {
    const chunks = [];
    let size = 0;
    res.on("data", (c) => {
      size += c.length;
      if (size > MAX_BODY) {
        r.destroy(new Error("response too large"));
      } else {
        chunks.push(c);
      }
    });
    res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, body: Buffer.concat(chunks) }));
    res.on("error", reject);
  };
}

function send(r, req, reject) {
  r.on("error", reject);
  r.setTimeout(15000, () => r.destroy(new Error("timeout")));
  if (req.body) r.write(req.body);
  r.end();
}

// E.NET1e: one request through the egress proxy -- absolute-form for http, a CONNECT tunnel and TLS
// (verified against the host) for https. Nothing is resolved here.
function fetchViaProxy(u, host, proxyText, req, deps = {}) {
  const p = new URL(proxyText);
  const proxy = { host: p.hostname.replace(/^\[|\]$/g, ""), port: Number(p.port) || 80 };
  const headers = { ...(req.headers || {}), host: u.host };
  if (u.protocol === "http:") {
    return new Promise((resolve, reject) => {
      // E.NET1g (L4): the absolute form without userinfo -- a user:password in a URL is not the proxy's.
      const r = http.request({ ...proxy, method: req.method || "GET", path: u.origin + u.pathname + u.search, headers });
      r.on("response", collect(r, resolve, reject));
      send(r, req, reject);
    });
  }
  const port = Number(u.port) || 443;
  const target = `${net.isIP(host) === 6 ? `[${host}]` : host}:${port}`;
  return new Promise((resolve, reject) => {
    const c = http.request({ ...proxy, method: "CONNECT", path: target, headers: { host: target } });
    // The tunnel must open in time; once it is open the request's own timeout governs (E.NET1g, L4:
    // the CONNECT's timer used to stay armed on the tunnelled socket and cut a slow page off).
    const opening = setTimeout(() => c.destroy(new Error("timeout")), deps.connectTimeoutMs || 15000);
    c.on("connect", (res, socket) => {
      clearTimeout(opening);
      if (res.statusCode !== 200) {
        socket.destroy();
        return reject(new Error(`blocked: the proxy refused ${target} (${res.statusCode})`));
      }
      const secure = tls.connect({ socket, host, servername: net.isIP(host) ? undefined : host, ...(deps.tls || {}) });
      // A failed handshake (a certificate for another host) refuses this request; unheard, it would
      // end the whole process.
      secure.on("error", reject);
      const r = https.request({
        host,
        port,
        method: req.method || "GET",
        path: u.pathname + u.search,
        headers,
        // No agent: Node then uses createConnection -- the tunnel. (`agent: false` made a fresh agent
        // that ignored it and connected to the host directly; E.NET1g found it.)
        createConnection: () => secure,
      });
      r.on("response", collect(r, resolve, reject));
      send(r, req, reject);
    });
    c.on("error", (e) => {
      clearTimeout(opening);
      reject(e);
    });
    c.end();
  });
}

// Fetch one request, connected to the address that was checked, following no redirect -- or, under
// the OS's enforced rules (E.NET1e), through its proxy.
async function fetchPinned(urlText, req = {}, deps = {}) {
  const u = new URL(urlText);
  if (u.protocol !== "http:" && u.protocol !== "https:") throw new Error("blocked: not http(s)");
  const host = u.hostname.replace(/^\[|\]$/g, "");
  const trust = (deps.egressTrust || egressTrust)(deps);
  if (trust) {
    // Checked here as always -- localhost, a hosts-file name and a literal resolve with no DNS. Only a
    // name that does not resolve here is left to the proxy, which refuses private ranges itself --
    // and (E.NET1g, M1) never one a LAN rule covers.
    try {
      await checkedAddresses(host, deps);
    } catch (e) {
      if (!e.unresolved || !leavesToProxy(trust, host)) throw e;
    }
    // E.NET1j: through the public-only door when the OS has one.
    return fetchViaProxy(u, host, trust.public || trust.proxy, req, deps);
  }
  const [pick] = await checkedAddresses(host, deps);
  const mod = u.protocol === "https:" ? https : http;
  return new Promise((resolve, reject) => {
    const r = mod.request(
      {
        protocol: u.protocol,
        hostname: host,
        port: u.port || undefined,
        path: u.pathname + u.search,
        method: req.method || "GET",
        headers: req.headers || {},
        servername: net.isIP(host) ? undefined : host,
        // The pin: whatever Node asks, the answer is the address that was checked.
        lookup: (_h, opts, cb) => {
          if (opts && opts.all) cb(null, [{ address: pick.address, family: pick.family }]);
          else cb(null, pick.address, pick.family);
        },
      },
    );
    r.on("response", collect(r, resolve, reject));
    send(r, req, reject);
  });
}

function flatHeaders(headers) {
  const out = {};
  for (const [k, v] of Object.entries(headers || {})) {
    out[k] = Array.isArray(v) ? v.join(k.toLowerCase() === "set-cookie" ? "\n" : ", ") : String(v);
  }
  return out;
}

async function guardContext(ctx) {
  await ctx.route("**/*", async (route) => {
    const request = route.request();
    let u;
    try {
      u = new URL(request.url());
    } catch (_) {
      return route.abort("blockedbyclient");
    }
    if (u.protocol === "data:" || u.protocol === "blob:") return route.continue();
    if (u.protocol !== "http:" && u.protocol !== "https:") return route.abort("blockedbyclient");
    try {
      const res = await fetchPinned(u.href, {
        method: request.method(),
        headers: request.headers(),
        body: request.postDataBuffer() || undefined,
      });
      return route.fulfill({ status: res.status, headers: flatHeaders(res.headers), body: res.body });
    } catch (_) {
      return route.abort("blockedbyclient");
    }
  });
  // E.NET1c (the tenth pass): fail closed -- without WebSocket routing (Playwright >= 1.48) a page
  // could open a socket to the LAN, so the browser does not run at all.
  if (typeof ctx.routeWebSocket !== "function") {
    throw new Error("this Playwright cannot route WebSockets (needs >= 1.48); refusing to browse");
  }
  await ctx.routeWebSocket(/.*/, (ws) => ws.close());
}

// E.NET1d (the twelfth pass): the ONLY ways a deployed script gets a browser -- every context made
// here is guarded before it is returned, with service workers blocked. A scan holds every script in
// the repository to these: no chromium.launch*, connect*, newContext, contexts() or a browser's
// newPage anywhere else.
// E.NET1e: under the OS's enforced rules, the browser's own traffic (whatever the page guard does not
// carry) meets the proxy rather than a reset.
function withProxy(options, deps = {}) {
  const trust = (deps.egressTrust || egressTrust)(deps);
  return trust && !options.proxy ? { ...options, proxy: { server: trust.public || trust.proxy } } : options;
}

// E.NET1f (the fourteenth pass): the lock at RUN time. Playwright's browser types (chromium, firefox,
// webkit -- one prototype, which playwright-extra calls into) may launch or connect only inside the
// launchers below, each allowed the one method it uses; anywhere else the call throws. A computed name
// or a script the textual scan misses meets the same wall.
const LAUNCHING = new AsyncLocalStorage();
const LOCKED = ["launch", "launchPersistentContext", "launchServer", "connect", "connectOverCDP"];

function lockBrowserTypes(proto) {
  for (const name of LOCKED) {
    const real = proto[name];
    if (typeof real !== "function" || real.netGuardLocked) continue;
    const locked = function (...args) {
      // E.NET1h (the review's N2): the permission is ONE call, spent at entry. AsyncLocalStorage carries
      // the store into everything made during the launch (the transport, its events, a timer), so a
      // page.on('load') callback used to find it still set and launch an unguarded browser.
      const permit = LAUNCHING.getStore();
      if (!permit || permit.method !== name || permit.used) throw new Error(`blocked: ${name} is only for net_guard's launchers`);
      permit.used = true;
      return real.apply(this, args);
    };
    locked.netGuardLocked = true;
    Object.defineProperty(proto, name, { value: locked, writable: false, configurable: false, enumerable: false });
  }
}

// Lock the real playwright-core when it is installed beside this file (a Mind box). Without it, nothing
// here could launch anyway.
for (const mod of ["playwright-core", "playwright"]) {
  let types = null;
  try {
    types = require(mod);
  } catch (_) {
    continue;
  }
  for (const t of [types.chromium, types.firefox, types.webkit]) if (t) lockBrowserTypes(Object.getPrototypeOf(t));
}

// Call `method` on `browserType` as a launcher may -- the one place the lock lets it through.
function launchAs(method, browserType, ...args) {
  return LAUNCHING.run({ method, used: false }, () => browserType[method](...args));
}

// E.NET1f: a persistent profile restores nothing -- its saved session is removed before the launch, so
// no page loads before the guard is in place.
function clearSavedSession(profileDir, f = fs) {
  for (const p of ["Default/Sessions", "Default/Current Session", "Default/Current Tabs", "Default/Last Session", "Default/Last Tabs"]) {
    f.rmSync(path.join(profileDir, p), { recursive: true, force: true });
  }
}

// E.NET1k (the review's L3): a page's WebRTC sends no UDP outside a proxy -- the route guard sees only
// http(s) and WebSockets.
const WEBRTC = "--force-webrtc-ip-handling-policy=disable_non_proxied_udp";
function hardened(options) {
  const args = Array.isArray(options.args) ? options.args : [];
  return args.includes(WEBRTC) ? options : { ...options, args: [...args, WEBRTC] };
}

async function launchGuarded(chromium, launchOptions = {}, contextOptions = {}, deps = {}) {
  const browser = await launchAs("launch", chromium, hardened(withProxy(launchOptions, deps)));
  try {
    const ctx = await browser.newContext({ ...contextOptions, serviceWorkers: "block" });
    await guardContext(ctx);
    return { browser, ctx };
  } catch (e) {
    await browser.close().catch(() => {});
    throw e;
  }
}

async function launchPersistentGuarded(chromium, profileDir, options = {}, deps = {}) {
  clearSavedSession(profileDir, deps.fs || fs);
  const ctx = await launchAs("launchPersistentContext", chromium, profileDir, { ...hardened(withProxy(options, deps)), serviceWorkers: "block" });
  try {
    await guardContext(ctx);
    // Anything already open (a start page) starts again, under the guard.
    for (const page of ctx.pages()) if (page.url() !== "about:blank") await page.goto("about:blank");
    return { browser: ctx.browser(), ctx };
  } catch (e) {
    await ctx.close().catch(() => {});
    throw e;
  }
}

module.exports = {
  privateIp,
  v6ToBigInt,
  checkedAddresses,
  hostIsPrivate,
  fetchPinned,
  egressTrust,
  guardContext,
  lockBrowserTypes,
  launchGuarded,
  launchPersistentGuarded,
};
