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
  const { trustedProxy } = require("./net_guard");
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
    trustedProxy: () => proxyAt,
  };
  const got1e = await fetchPinned("http://news.invalid/a?b=1", {}, viaProxy);
  assert.strictEqual(got1e.body.toString(), "via the proxy");
  assert.deepStrictEqual(seen, ["GET http://news.invalid/a?b=1"], "the request did not go through the proxy");
  await assert.rejects(fetchPinned("https://news.invalid/s", {}, viaProxy), /proxy refused news\.invalid:443/);
  assert.strictEqual(seen[1], "CONNECT news.invalid:443", "https did not tunnel through the proxy");
  await assert.rejects(fetchPinned("http://192.168.4.1/admin", {}, viaProxy), /private/, "a literal private address went to the proxy");
  assert.strictEqual(seen.length, 2);
  await assert.rejects(fetchPinned("http://inside.test/", {}, viaProxy), /private/, "a name resolving inside went to the proxy");
  assert.strictEqual(seen.length, 2);
  // A failed handshake refuses the request -- it does not end the process.
  await assert.rejects(fetchPinned("https://garbled.invalid/", {}, viaProxy));
  assert.strictEqual(seen[2], "CONNECT garbled.invalid:443");
  proxyServer.close();

  // E.NET1e: the signal is trusted only whole -- root's file, opened without following a link, in
  // root's folder; version 1, enforced, the proxy refusing private ranges, and our own proxy.
  const WHOLE = JSON.stringify({
    enforced: true, table: "inet yantrik_mind_egress", proxy: "http://127.0.0.1:7450", proxy_refuses_private: true,
    mode: "enforce", private: false, dns_allowed: false, loaded_at: 1790000000, version: 1,
  });
  const NOFOLLOW = 0o400000;
  const signalFs = (o = {}) => {
    const st = (s) => ({ isDirectory: () => !!s.dir, isFile: () => !!s.file, uid: s.uid, mode: s.mode });
    return {
      constants: { O_RDONLY: 0, O_NOFOLLOW: NOFOLLOW },
      lstatSync: () => st({ dir: true, uid: o.dirUid ?? 0, mode: o.dirMode ?? 0o755 }),
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
  const trust = (o, env = ourEnv) => trustedProxy({ fs: signalFs(o), env, signal: "/run/yantrik/mind-egress.json" });
  assert.strictEqual(trust({}), "http://127.0.0.1:7450", "the whole signal was not trusted");
  for (const [o, why] of [
    [{ missing: true }, "no file"],
    [{ link: true }, "a link"],
    [{ mode: 0o666 }, "a file others can write"],
    [{ uid: 1000 }, "a file not root's"],
    [{ dirMode: 0o777 }, "a folder others can write"],
    [{ dirUid: 1000 }, "a folder not root's"],
    [{ text: WHOLE.replace('"enforced":true', '"enforced":false') }, "not enforced"],
    [{ text: WHOLE.replace('"proxy_refuses_private":true', '"proxy_refuses_private":false') }, "a proxy letting private ranges through"],
    [{ text: WHOLE.replace('"version":1', '"version":2') }, "an unknown version"],
    [{ text: "enforced: true" }, "not JSON"],
  ]) {
    assert.strictEqual(trust(o), null, `trusted: ${why}`);
  }
  assert.strictEqual(trust({}, {}), null, "trusted with no proxy configured");
  assert.strictEqual(trust({}, { HTTPS_PROXY: "http://10.0.0.9:3128" }), null, "trusted for another proxy");

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
  const pg = await launchPersistentGuarded(fakeChromium, "/tmp/profile", {});
  assert.strictEqual(pg.ctx.opts.serviceWorkers, "block");
  assert.ok(pg.ctx.routes.includes("**/*"), "the persistent context came back unguarded");
  // E.NET1e: under the OS's enforced rules the browser itself is pointed at the proxy.
  let launchedWith = null;
  const proxiedChromium = { ...fakeChromium, launch: async (opts) => { launchedWith = opts; return fakeChromium.launch(); } };
  await launchGuarded(proxiedChromium, {}, {}, { trustedProxy: () => "http://127.0.0.1:7450" });
  assert.deepStrictEqual(launchedWith.proxy, { server: "http://127.0.0.1:7450" }, "the browser was not pointed at the proxy");
  const pp = await launchPersistentGuarded(fakeChromium, "/tmp/profile", {}, { trustedProxy: () => "http://127.0.0.1:7450" });
  assert.deepStrictEqual(pp.ctx.opts.proxy, { server: "http://127.0.0.1:7450" }, "the persistent browser was not pointed at the proxy");
  await launchGuarded(proxiedChromium, {}, {}, { trustedProxy: () => null });
  assert.strictEqual(launchedWith.proxy, undefined, "a proxy was set without the OS's word");
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
    [/chromium\s*\.\s*(launch\w*|connect\w*)\s*\(/, "chromium.launch*/connect*"],
    [/\.\s*launchServer\s*\(/, "launchServer"],
    [/connectOverCDP/, "connectOverCDP"],
    [/\.\s*newContext\s*\(/, "newContext"],
    [/\.\s*contexts\s*\(\s*\)/, "contexts()"],
    [/(?<!\bctx)\s*\.\s*newPage\s*\(/, "a browser's newPage"],
  ];
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
    for (const [re, what] of FORBIDDEN) if (re.test(src)) offences.push(`${rel}: ${what}`);
    if (/playwright/.test(src) && !/launch(Persistent)?Guarded/.test(src)) offences.push(`${rel}: uses playwright without net_guard's launchers`);
  }
  assert.deepStrictEqual(offences, [], "a browser made around net_guard");
  console.log("net_guard: ok");
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
