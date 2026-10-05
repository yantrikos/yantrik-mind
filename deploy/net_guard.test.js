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
  console.log("net_guard: ok");
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
