// E.NET1 / E.NET1b: plain-Node tests for net_guard.js (no browser needed). Run: node deploy/net_guard.test.js
const assert = require("assert");
const http = require("http");
const { privateIp, hostIsPrivate, fetchPinned } = require("./net_guard");

(async () => {
  // The shared list, and the ninth pass's own addresses (as Node's URL parser writes some of them).
  for (const ip of [
    "10.0.0.1", "127.0.0.1", "192.168.4.44", "172.16.0.1", "172.31.255.1", "169.254.1.1", "100.64.0.1",
    "0.0.0.0", "224.0.0.1", "240.0.0.1", "255.255.255.255", "::1", "::", "fd00::1", "fe80::1", "ff02::1",
    "::ffff:192.168.1.1", "::ffff:7f00:1", "::ffff:c0a8:407", "::7f00:1", "64:ff9b::c0a8:407", "[::ffff:127.0.0.1]",
    "2002:c0a8:407::1", "2001:0:4136:e378:8000:63bf:3fff:fdd2",
  ]) {
    assert.strictEqual(privateIp(ip), true, `${ip} should be private`);
  }
  for (const ip of ["8.8.8.8", "172.32.0.1", "100.128.0.1", "1.1.1.1", "2606:4700::1111", "::ffff:8.8.8.8"]) {
    assert.strictEqual(privateIp(ip), false, `${ip} should be public`);
  }
  assert.strictEqual(privateIp("not-an-ip"), true, "garbage fails closed");

  const fake = (map, calls) => async (h) => {
    if (calls) calls.push(h);
    if (!(h in map)) throw new Error("ENOTFOUND");
    return map[h].map((address) => ({ address, family: address.includes(":") ? 6 : 4 }));
  };
  const lookup = fake({ "public.test": ["93.184.216.34"], "rebind.test": ["93.184.216.34", "192.168.4.44"], "empty.test": [] });
  assert.strictEqual(await hostIsPrivate("public.test", lookup), false);
  assert.strictEqual(await hostIsPrivate("rebind.test", lookup), true, "any private address refuses the host");
  assert.strictEqual(await hostIsPrivate("empty.test", lookup), true, "no address fails closed");
  assert.strictEqual(await hostIsPrivate("nowhere.test", lookup), true, "a lookup error fails closed");
  assert.strictEqual(await hostIsPrivate("[::1]", lookup), true, "a bracketed v6 literal");

  // The pin: a name only the check's lookup knows connects -- so the connection used the checked
  // address -- and the lookup ran exactly once. A 3xx is returned, not followed.
  const server = http.createServer((req, res) => {
    if (req.url === "/moved") {
      res.writeHead(302, { Location: "http://127.0.0.1:1/inside" });
      return res.end();
    }
    res.writeHead(200, { "Content-Type": "text/plain" });
    res.end("pinned and reached");
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const port = server.address().port;
  const calls = [];
  const deps = { lookup: fake({ "pinned.invalid": ["127.0.0.1"] }, calls), privateIp: () => false };
  const got = await fetchPinned(`http://pinned.invalid:${port}/page`, {}, deps);
  assert.strictEqual(got.status, 200);
  assert.strictEqual(got.body.toString(), "pinned and reached");
  assert.deepStrictEqual(calls, ["pinned.invalid"], "the host was looked up more than once");
  const moved = await fetchPinned(`http://pinned.invalid:${port}/moved`, {}, deps);
  assert.strictEqual(moved.status, 302, "a redirect was followed");
  // With the real check, the loopback server itself is refused.
  await assert.rejects(fetchPinned(`http://127.0.0.1:${port}/page`), /private/);
  server.close();

  // E.NET1e (yantrik-os #662): under the OS's enforced rules a request goes THROUGH the proxy and
  // nothing is looked up here; a literal private address is still refused here.
  const { egressTrust } = require("./net_guard");
  const seen = [];
  const proxyServer = http.createServer((req, res) => {
    seen.push(`${req.method} ${req.url}`);
    res.writeHead(200, { "Content-Type": "text/plain" });
    res.end("via the proxy");
  });
  proxyServer.on("connect", (req, socket) => {
    seen.push(`CONNECT ${req.url}`);
    if (req.url === "garbled.invalid:443") {
      // A tunnel whose far end does not speak TLS: the handshake fails.
      return socket.end("HTTP/1.1 200 Connection Established\r\n\r\nthis is not TLS\r\n\r\n");
    }
    socket.end("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
  });
  await new Promise((r) => proxyServer.listen(0, "127.0.0.1", r));
  const proxyAt = `http://127.0.0.1:${proxyServer.address().port}`;
  const viaProxy = {
    lookup: async (h) => {
      if (h === "inside.test") return [{ address: "127.0.0.1", family: 4 }]; // a hosts-file name
      throw new Error(`ENOTFOUND ${h}`); // enforce mode: no DNS for the Mind
    },
    egressTrust: () => ({ proxy: proxyAt, lan: [] }),
  };
  // E.NET1j: with a public door, the request goes there, not to the endpoint proxy.
  const viaPublic = { ...viaProxy, egressTrust: () => ({ proxy: "http://127.0.0.1:9", lan: [], public: proxyAt }) };
  const gotPublic = await fetchPinned("http://news.invalid/p", {}, viaPublic);
  assert.strictEqual(gotPublic.body.toString(), "via the proxy", "the public door was not used");
  seen.splice(0, seen.length);
  const got1e = await fetchPinned("http://news.invalid/a?b=1", {}, viaProxy);
  assert.strictEqual(got1e.body.toString(), "via the proxy");
  assert.deepStrictEqual(seen, ["GET http://news.invalid/a?b=1"], "the request did not go through the proxy");
  await assert.rejects(fetchPinned("https://news.invalid/s", {}, viaProxy), /proxy refused news\.invalid:443/);
  assert.strictEqual(seen[1], "CONNECT news.invalid:443", "https did not tunnel through the proxy");
  await assert.rejects(fetchPinned("http://192.168.4.1/admin", {}, viaProxy), /private/, "a literal private address went to the proxy");
  assert.strictEqual(seen.length, 2);
  await assert.rejects(fetchPinned("http://inside.test/", {}, viaProxy), /private/, "a name resolving inside went to the proxy");
  assert.strictEqual(seen.length, 2);
  // E.NET1g (M1): a name a LAN rule covers never goes on unresolved -- the proxy would let it into the
  // LAN by name -- and with no LAN list, no name does.
  const lanTrust = { proxy: proxyAt, lan: [{ host: "gpu.example.ts.net", ports: [11434] }, { host: "*.home.arpa", ports: [80] }] };
  const viaLan = { ...viaProxy, egressTrust: () => lanTrust };
  await assert.rejects(fetchPinned("http://gpu.example.ts.net:11434/api/tags", {}, viaLan), /does not resolve/, "a LAN rule's name went to the proxy");
  await assert.rejects(fetchPinned("http://GPU.example.ts.net.:11434/", {}, viaLan), /does not resolve/, "case or a trailing dot slipped a LAN name through");
  await assert.rejects(fetchPinned("http://ha.home.arpa/", {}, viaLan), /does not resolve/, "a wildcard LAN rule did not cover its subdomain");
  await assert.rejects(fetchPinned("http://home.arpa/", {}, viaLan), /does not resolve/, "a wildcard LAN rule did not cover its domain");
  await assert.rejects(fetchPinned("http://gpu.example.ts.net:22/", {}, viaLan), /does not resolve/, "a LAN rule's host on another port went to the proxy");
  await assert.rejects(fetchPinned("http://news.invalid/", {}, { ...viaProxy, egressTrust: () => ({ proxy: proxyAt, lan: null }) }), /does not resolve/, "with no LAN list, a name went on");
  assert.strictEqual(seen.length, 2, "a LAN name reached the proxy");
  // E.NET1g (L4): no userinfo in the request line the proxy sees.
  await fetchPinned("http://someone:secret@news.invalid/u", {}, viaProxy);
  assert.strictEqual(seen[2], "GET http://news.invalid/u", "userinfo went to the proxy");
  seen.splice(2, 1);
  // A failed handshake refuses the request -- it does not end the process.
  await assert.rejects(fetchPinned("https://garbled.invalid/", {}, viaProxy));
  assert.strictEqual(seen[2], "CONNECT garbled.invalid:443");

  // E.NET1g (L4): a whole https request through the tunnel, offline -- a local TLS server for
  // tunnel.test (a test-only certificate, trusted here and nowhere else) that answers slowly. The
  // CONNECT's timer must not cut it off once the tunnel is open.
  const tls = require("tls");
  const net = require("net");
  const TEST_CERT = `-----BEGIN CERTIFICATE-----
MIIBnDCCAUGgAwIBAgIUXPj9AoTYblc/oXvQFiJrAf4iFzQwCgYIKoZIzj0EAwIw
FjEUMBIGA1UEAwwLdHVubmVsLnRlc3QwIBcNMjYxMDA1MTc1NTQyWhgPMjA1NjA5
MjcxNzU1NDJaMBYxFDASBgNVBAMMC3R1bm5lbC50ZXN0MFkwEwYHKoZIzj0CAQYI
KoZIzj0DAQcDQgAEZ0wRCEpVx76T+9ZLaRlmEbtNH8n87Oe+bZr0NrHO14D7QyIm
/51maYD0IzNr4n7WdJSwKJ/QZ+hThwE0Df2JXKNrMGkwHQYDVR0OBBYEFE3lbdOC
RAVqLtFuvPz8jK6XUNNSMB8GA1UdIwQYMBaAFE3lbdOCRAVqLtFuvPz8jK6XUNNS
MA8GA1UdEwEB/wQFMAMBAf8wFgYDVR0RBA8wDYILdHVubmVsLnRlc3QwCgYIKoZI
zj0EAwIDSQAwRgIhAMxwPNtdVjrpK9Hd3TWnbJjtyBf8Av54Y1W/ANajaO+9AiEA
iUSrMBMHYt0LyZLnbar65F0FL3hvwH6aw37yv23mKNs=
-----END CERTIFICATE-----`;
  const TEST_KEY = `-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgVJ4x8vls/GRhfm6l
waEkWGQbbyzcGS/aUlcoWt7eZgihRANCAARnTBEISlXHvpP71ktpGWYRu00fyfzs
575tmvQ2sc7XgPtDIib/nWZpgPQjM2viftZ0lLAon9Bn6FOHATQN/Ylc
-----END PRIVATE KEY-----`;
  const site = tls.createServer({ cert: TEST_CERT, key: TEST_KEY }, (s) => {
    s.once("data", () => setTimeout(() => s.end("HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\ntunnels"), 400));
  });
  await new Promise((r) => site.listen(0, "127.0.0.1", r));
  const tunneller = http.createServer();
  tunneller.on("connect", (req, client) => {
    seen.push(`CONNECT ${req.url}`);
    const up = net.connect(site.address().port, "127.0.0.1", () => {
      client.write("HTTP/1.1 200 Connection Established\r\n\r\n");
      up.pipe(client);
      client.pipe(up);
    });
    // A refused handshake resets the pipe; that is the test's fixture, not the code under test.
    up.on("error", () => client.destroy());
    client.on("error", () => up.destroy());
  });
  await new Promise((r) => tunneller.listen(0, "127.0.0.1", r));
  const viaTunnel = {
    lookup: async (h) => {
      throw new Error(`ENOTFOUND ${h}`);
    },
    egressTrust: () => ({ proxy: `http://127.0.0.1:${tunneller.address().port}`, lan: [] }),
    tls: { ca: TEST_CERT },
    connectTimeoutMs: 150,
  };
  const tunnelled = await fetchPinned("https://tunnel.test/page", {}, viaTunnel);
  assert.strictEqual(tunnelled.body.toString(), "tunnels", "the tunnel was cut off after it opened");
  await assert.rejects(fetchPinned("https://tunnel.test/page", {}, { ...viaTunnel, tls: {} }), /self.signed|certificate/i, "an unknown certificate was accepted");
  tunneller.close();
  site.close();
  proxyServer.close();

  // E.NET1e: the signal is trusted only whole -- root's file, opened without following a link, in
  // root's folder; version 1, enforced, the proxy refusing private ranges, and our own proxy.
  const WHOLE = JSON.stringify({
    enforced: true, table: "inet yantrik_mind_egress", proxy: "http://127.0.0.1:7450", proxy_refuses_private: true,
    mode: "enforce", private: false, dns_allowed: false, loaded_at: 1790000000,
    lan_hosts: [{ host: "homeassistant.local", ports: [8123] }], version: 2,
  });
  const NOFOLLOW = 0o400000;
  const signalFs = (o = {}) => {
    const st = (s) => ({ isDirectory: () => !!s.dir, isFile: () => !!s.file, uid: s.uid, mode: s.mode });
    return {
      constants: { O_RDONLY: 0, O_NOFOLLOW: NOFOLLOW },
      lstatSync: () => ({ ...st({ dir: true, uid: o.dirUid ?? 0, mode: o.dirMode ?? 0o755 }), gid: o.dirGid ?? 0 }),
      openSync: (_f, flags) => {
        if (o.missing) throw Object.assign(new Error("ENOENT"), { code: "ENOENT" });
        if (o.link && flags & NOFOLLOW) throw Object.assign(new Error("ELOOP"), { code: "ELOOP" });
        return 7;
      },
      // Opened without O_NOFOLLOW, a link is followed to its target: a regular file.
      fstatSync: () => st({ file: true, uid: o.uid ?? 0, mode: o.mode ?? 0o644 }),
      readFileSync: () => (o.text === undefined ? WHOLE : o.text),
      closeSync: () => {},
    };
  };
  const ourEnv = { HTTPS_PROXY: "http://127.0.0.1:7450" };
  const trust = (o, env = ourEnv) => egressTrust({ fs: signalFs(o), env, signal: "/run/yantrik/mind-egress.json" });
  assert.deepStrictEqual(trust({}), { proxy: "http://127.0.0.1:7450", lan: [{ host: "homeassistant.local", ports: [8123] }], public: null }, "the whole signal was not trusted");
  assert.deepStrictEqual(trust({ text: WHOLE.replace('[{"host":"homeassistant.local","ports":[8123]}]', "null") }), { proxy: "http://127.0.0.1:7450", lan: null, public: null });
  // E.NET1j: version 3 and its public door.
  assert.strictEqual(trust({ text: WHOLE.replace('"version":2', '"version":3,"public_proxy":"http://127.0.0.1:7451"') }).public, "http://127.0.0.1:7451");
  assert.strictEqual(trust({ text: WHOLE.replace('"version":2', '"version":3,"public_proxy":null') }).public, null);
  // E.GRANT2b (the review's Info): an endpoint door written as localhost is this machine's loopback.
  for (const host of ["localhost", "localhost."]) {
    const proxy = `http://${host}:7450`;
    const said = (door) => WHOLE.replace('"http://127.0.0.1:7450"', JSON.stringify(proxy)).replace('"version":2', `"version":3,"public_proxy":"${door}"`);
    assert.strictEqual(trust({ text: said("http://127.0.0.1:7450") }, { HTTPS_PROXY: proxy }), null, `the endpoint door under ${host} trusted as the public door`);
    assert.strictEqual(trust({ text: said("http://127.0.0.1:7451") }, { HTTPS_PROXY: proxy }).public, "http://127.0.0.1:7451", host);
  }
  for (const [v, why] of [
    ['"version":3', "a v3 signal saying nothing of a public door"],
    ['"version":3,"public_proxy":"http://10.0.0.5:7451"', "a public door that is not this machine"],
    ['"version":3,"public_proxy":"http://proxy.lan:7451"', "a public door that is not an address"],
    ['"version":3,"public_proxy":"127.0.0.1:7451"', "a public door that is not a URL"],
    ['"version":3,"public_proxy":"http://127.0.0.1"', "a public door without its port"],
    ['"version":3,"public_proxy":"http://127.0.0.1:7450"', "a public door that is the endpoint door"],
    ['"version":4', "an unknown version"],
  ]) {
    assert.strictEqual(trust({ text: WHOLE.replace('"version":2', v) }), null, `trusted: ${why}`);
  }
  for (const [o, why] of [
    [{ missing: true }, "no file"],
    [{ link: true }, "a link"],
    [{ mode: 0o666 }, "a file others can write"],
    [{ uid: 1000 }, "a file not root's"],
    [{ dirMode: 0o777 }, "a folder others can write"],
    [{ dirUid: 1000 }, "a folder not root's"],
    [{ dirGid: 1000 }, "a folder whose group is not root's"],
    [{ text: WHOLE.replace('"enforced":true', '"enforced":false') }, "not enforced"],
    [{ text: WHOLE.replace('"proxy_refuses_private":true', '"proxy_refuses_private":false') }, "a proxy letting private ranges through"],
    [{ text: WHOLE.replace('"version":2', '"version":1') }, "the version before lan_hosts"],
    [{ text: WHOLE.replace('"version":2', '"version":4') }, "an unknown version"],
    [{ text: WHOLE.replace('"lan_hosts":[{"host":"homeassistant.local","ports":[8123]}],', "") }, "no lan_hosts"],
    [{ text: WHOLE.replace('"ports":[8123]', '"ports":["8123"]') }, "a malformed LAN rule"],
    [{ text: "enforced: true" }, "not JSON"],
  ]) {
    assert.strictEqual(trust(o), null, `trusted: ${why}`);
  }
  assert.strictEqual(trust({}, {}), null, "trusted with no proxy configured");
  assert.strictEqual(trust({}, { HTTPS_PROXY: "http://10.0.0.9:3128" }), null, "trusted for another proxy");

  // E.NET1g (review residual b): the same read against REAL files, on Linux -- run as root, root's
  // own 0644 signal in a 0755 folder is trusted; run as any other uid, nothing is (the file is not
  // root's). A link, a file others can write and a folder others can write never are.
  if (process.platform === "linux") {
    const fsr = require("fs");
    const pth = require("path");
    const base = fsr.mkdtempSync(pth.join(require("os").tmpdir(), "ym-signal-"));
    fsr.chmodSync(base, 0o755);
    const file = pth.join(base, "mind-egress.json");
    fsr.writeFileSync(file, WHOLE);
    fsr.chmodSync(file, 0o644);
    const real = (f) => egressTrust({ env: ourEnv, signal: f });
    const root = process.getuid() === 0;
    assert.strictEqual(real(file) !== null, root, root ? "root's own signal was not trusted" : "a signal not owned by root was trusted");
    const link = pth.join(base, "linked.json");
    fsr.symlinkSync(file, link);
    assert.strictEqual(real(link), null, "a link to the signal was followed");
    fsr.chmodSync(file, 0o666);
    assert.strictEqual(real(file), null, "a signal others can write was trusted");
    fsr.chmodSync(file, 0o644);
    fsr.chmodSync(base, 0o777);
    assert.strictEqual(real(file), null, "a signal in a folder others can write was trusted");
    fsr.chmodSync(base, 0o755);
    fsr.rmSync(base, { recursive: true, force: true });
    console.log(`net_guard: the real signal read checked as uid ${process.getuid()} (trusted: ${root})`);
  }

  // E.NET1c: without WebSocket routing the guard refuses to run at all; with it, it routes.
  const { guardContext } = require("./net_guard");
  const routed = [];
  const oldCtx = { route: async (pattern) => routed.push(pattern) };
  await assert.rejects(guardContext(oldCtx), /WebSocket/, "an old Playwright browsed anyway");
  const newCtx = { route: async (pattern) => routed.push(pattern), routeWebSocket: async (pattern) => routed.push(String(pattern)) };
  await guardContext(newCtx);
  assert.ok(routed.includes("**/*"), "requests were not routed");

  // E.NET1d: the launchers hand back only a guarded context, service workers blocked; a guard that
  // cannot be installed closes the browser rather than returning it.
  const { launchGuarded, launchPersistentGuarded } = require("./net_guard");
  const fakeCtx = () => {
    const c = { routes: [], opts: null, closed: false };
    c.route = async (pattern) => c.routes.push(pattern);
    c.routeWebSocket = async () => {};
    c.close = async () => { c.closed = true; };
    c.browser = () => ({ fake: true });
    c.pages = () => c.openPages || [];
    return c;
  };
  let made = null;
  const fakeChromium = {
    launch: async () => ({
      closed: false,
      newContext: async (opts) => { made = fakeCtx(); made.opts = opts; return made; },
      close: async function () { this.closed = true; },
    }),
    launchPersistentContext: async (_dir, opts) => { made = fakeCtx(); made.opts = opts; return made; },
  };
  const g = await launchGuarded(fakeChromium, {}, { locale: "en-US" });
  assert.strictEqual(g.ctx.opts.serviceWorkers, "block", "service workers were let in");
  assert.ok(g.ctx.routes.includes("**/*"), "the context came back unguarded");
  const removed = [];
  const recordingFs = { rmSync: (p) => removed.push(p.split(require("path").sep).join("/")) };
  const pg = await launchPersistentGuarded(fakeChromium, "/tmp/profile", {}, { fs: recordingFs });
  assert.strictEqual(pg.ctx.opts.serviceWorkers, "block");
  assert.ok(pg.ctx.routes.includes("**/*"), "the persistent context came back unguarded");
  // E.NET1e: under the OS's enforced rules the browser itself is pointed at the proxy.
  let launchedWith = null;
  const proxiedChromium = { ...fakeChromium, launch: async (opts) => { launchedWith = opts; return fakeChromium.launch(); } };
  await launchGuarded(proxiedChromium, {}, {}, { egressTrust: () => ({ proxy: "http://127.0.0.1:7450", lan: [] }) });
  assert.deepStrictEqual(launchedWith.proxy, { server: "http://127.0.0.1:7450" }, "the browser was not pointed at the proxy");
  const pp = await launchPersistentGuarded(fakeChromium, "/tmp/profile", {}, { egressTrust: () => ({ proxy: "http://127.0.0.1:7450", lan: [] }) });
  assert.deepStrictEqual(pp.ctx.opts.proxy, { server: "http://127.0.0.1:7450" }, "the persistent browser was not pointed at the proxy");
  await launchGuarded(proxiedChromium, {}, {}, { egressTrust: () => ({ proxy: "http://127.0.0.1:7450", lan: [], public: "http://127.0.0.1:7451" }) });
  assert.deepStrictEqual(launchedWith.proxy, { server: "http://127.0.0.1:7451" }, "the browser was not pointed at the public door");
  assert.ok(launchedWith.args.includes("--force-webrtc-ip-handling-policy=disable_non_proxied_udp"), "WebRTC may send UDP outside the proxy");
  const pw = await launchPersistentGuarded(fakeChromium, "/tmp/profile", { args: ["--x"] }, { fs: recordingFs, egressTrust: () => null });
  assert.ok(pw.ctx.opts.args.includes("--force-webrtc-ip-handling-policy=disable_non_proxied_udp") && pw.ctx.opts.args.includes("--x"), "the persistent browser's WebRTC is not held, or its own args were lost");
  await launchGuarded(proxiedChromium, {}, {}, { egressTrust: () => null });
  assert.strictEqual(launchedWith.proxy, undefined, "a proxy was set without the OS's word");
  // E.NET1f: a persistent profile restores nothing (its saved session is removed first), and a page
  // already open starts again at about:blank under the guard.
  assert.ok(removed.includes("/tmp/profile/Default/Sessions"), `the saved session was kept: ${removed}`);
  assert.ok(removed.includes("/tmp/profile/Default/Last Tabs"), "an older session file was kept");
  let sentTo = null;
  const restoring = {
    launchPersistentContext: async (_dir, opts) => {
      made = fakeCtx();
      made.opts = opts;
      made.openPages = [{ url: () => "https://restored.example/inbox", goto: async (u) => { sentTo = u; } }];
      return made;
    },
  };
  await launchPersistentGuarded(restoring, "/tmp/profile", {}, { fs: recordingFs });
  assert.strictEqual(sentTo, "about:blank", "an open page was left where it was");

  // E.NET1f (the fourteenth pass): the lock at run time -- a browser type launches or connects only
  // inside a launcher, and only by that launcher's own method.
  const { lockBrowserTypes } = require("./net_guard");
  class FakeType {
    async launch(o = {}) {
      if (o.sneak) await this.connectOverCDP("ws://127.0.0.1:9222");
      return fakeChromium.launch();
    }
    async launchPersistentContext(dir, o) { return fakeChromium.launchPersistentContext(dir, o); }
    async launchServer() { return "server"; }
    async connect() { return "connected"; }
    async connectOverCDP() { return "cdp"; }
  }
  lockBrowserTypes(FakeType.prototype);
  lockBrowserTypes(FakeType.prototype); // twice is harmless
  const typed = new FakeType();
  for (const m of ["launch", "launchPersistentContext", "launchServer", "connect", "connectOverCDP"]) {
    assert.throws(() => typed[m](), /blocked/, `${m} ran outside the launchers`);
  }
  assert.throws(() => Object.defineProperty(FakeType.prototype, "launch", { value: async () => "unlocked" }), TypeError, "the lock could be replaced");
  const viaLauncher = await launchGuarded(typed, {}, {}, { egressTrust: () => null });
  assert.ok(viaLauncher.ctx.routes.includes("**/*"), "the launcher's own launch was blocked");
  await launchPersistentGuarded(typed, "/tmp/profile", {}, { fs: recordingFs, egressTrust: () => null });
  await assert.rejects(launchGuarded(typed, { sneak: true }, {}, { egressTrust: () => null }), /blocked: connectOverCDP/, "a launcher's permission reached another method");
  // E.NET1h (N2): nothing made during a launch carries its permission -- an event fired later, from a
  // resource the launch created, cannot launch again ...
  let leaked = null;
  class LeakType {
    async launch() {
      setTimeout(() => {
        try {
          this.launch();
          leaked = "ran";
        } catch (e) {
          leaked = e.message;
        }
      }, 5);
      return fakeChromium.launch();
    }
  }
  lockBrowserTypes(LeakType.prototype);
  await launchGuarded(new LeakType(), {}, {}, { egressTrust: () => null });
  await new Promise((r) => setTimeout(r, 40));
  assert.match(String(leaked), /blocked/, "a callback made during the launch launched again");
  // ... nor can a wrapper's own callback (playwright-extra's place) reuse a spent permission.
  const core = new FakeType();
  let wrapperLeak = null;
  const wrapper = {
    launch(o) {
      setTimeout(() => {
        try {
          core.launch();
          wrapperLeak = "ran";
        } catch (e) {
          wrapperLeak = e.message;
        }
      }, 5);
      return core.launch(o);
    },
  };
  await launchGuarded(wrapper, {}, {}, { egressTrust: () => null });
  await new Promise((r) => setTimeout(r, 40));
  assert.match(String(wrapperLeak), /blocked/, "a spent permission was used again");

  const noWs = { launch: async () => ({ newContext: async () => ({ route: async () => {} }), close: async function () { this.closed = true; } }) };
  await assert.rejects(launchGuarded(noWs), /WebSocket/, "a context that cannot be guarded was handed out");

  // E.NET1d (the twelfth pass): repo-wide, .js and .mjs -- outside net_guard.js no code may make a
  // browser, a context or a page any other way than through launchGuarded / launchPersistentGuarded.
  // The listed files are test and CI harnesses that never run on a Mind box.
  const fs = require("fs");
  const path = require("path");
  const root = path.resolve(__dirname, "..");
  const CI_ONLY = new Set([
    "crates/mind-core/assets/xss_canary.mjs", // the web UI's XSS canary, run by CI against a local build
    "crates/mind-evals/fixtures/cb2/checks/check_web.mjs", // eval fixtures, run in the eval sandbox
    "crates/mind-evals/fixtures/cb2n/checks/check_web.mjs",
    "tools/fresh-install-eval/eval_protocol.mjs", // a developer's install check
    "deploy/net_guard.js", // the one place browsers are made
    "deploy/net_guard.test.js", // this test (its fakes and patterns)
  ]);
  const FORBIDDEN = [
    // E.NET1f: any launch but the two launchers, any connect -- on chromium, firefox, webkit or any
    // other handle -- by dot or by bracket.
    [/\.\s*launch(?!Guarded\b|PersistentGuarded\b)\w*\s*\(/, ".launch*("],
    [/\.\s*connect\w*\s*\(/, ".connect*("],
    [/connectOverCDP/, "connectOverCDP"],
    [/\.\s*newContext\s*\(/, "newContext"],
    [/\.\s*contexts\s*\(\s*\)/, "contexts()"],
    [/(?<!\bctx)\s*\.\s*newPage\s*\(/, "a browser's newPage"],
    [/\[\s*["'`](launch\w*|connect\w*|newContext|newPage|contexts)["'`]\s*\]/, "a browser method by bracket"],
    // E.NET1f: a module chosen at run time could be anything -- only plain string names.
    [/\b(require|import)\s*\(\s*(?!["'][^"'`$]*["']\s*\))(?!`[^`$]*`\s*\))/, "a require/import of a computed name"],
  ];
  const offencesIn = (src) => FORBIDDEN.filter(([re]) => re.test(src)).map(([, what]) => what);
  for (const src of [
    'chromium["launch"]()', "chromium[`connectOverCDP`](u)", "firefox.launch()", "webkit.launchPersistentContext(d)",
    "pw.chromium.connect(ws)", "bt.connect(ws)", "bt.launchServer()", "x.launchPersistentContext(d, o)", "require(name)",
    "require(`./${m}`)", "await import(which)", "browser.newContext()", "browser.newPage()",
  ]) {
    assert.ok(offencesIn(src).length > 0, `the scan missed: ${src}`);
  }
  for (const src of ['require("./net_guard")', "require('playwright-extra')", "launchGuarded(chromium, {})", "ng.launchGuarded(c)", "ng.launchPersistentGuarded(c, d)", "ctx.newPage()", "await import(\"./x.mjs\")"]) {
    assert.deepStrictEqual(offencesIn(src), [], `the scan refused a plain form: ${src}`);
  }
  const walk = (dir) =>
    fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
      if (["node_modules", ".git", "target"].includes(e.name)) return [];
      const p = path.join(dir, e.name);
      return e.isDirectory() ? walk(p) : /\.(m?js)$/.test(e.name) ? [p] : [];
    });
  const offences = [];
  for (const file of walk(root)) {
    const rel = path.relative(root, file).split(path.sep).join("/");
    if (CI_ONLY.has(rel)) continue;
    const src = fs.readFileSync(file, "utf8");
    for (const what of offencesIn(src)) offences.push(`${rel}: ${what}`);
    if (/playwright/.test(src) && !/launch(Persistent)?Guarded/.test(src)) offences.push(`${rel}: uses playwright without net_guard's launchers`);
  }
  assert.deepStrictEqual(offences, [], "a browser made around net_guard");
  console.log("net_guard: ok");
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
