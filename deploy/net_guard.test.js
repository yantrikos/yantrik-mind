// E.NET1: plain-Node tests for net_guard.js (no browser needed). Run: node deploy/net_guard.test.js
const assert = require("assert");
const { privateIp, hostIsPrivate } = require("./net_guard");

(async () => {
  for (const ip of ["10.0.0.1", "127.0.0.1", "192.168.4.44", "172.16.0.1", "172.31.255.1", "169.254.1.1", "100.64.0.1", "0.0.0.0", "224.0.0.1", "::1", "fd00::1", "fe80::1", "::ffff:192.168.1.1"]) {
    assert.strictEqual(privateIp(ip), true, `${ip} should be private`);
  }
  for (const ip of ["8.8.8.8", "172.32.0.1", "100.128.0.1", "1.1.1.1", "2606:4700::1111"]) {
    assert.strictEqual(privateIp(ip), false, `${ip} should be public`);
  }
  const fake = (map) => async (h) => {
    if (!(h in map)) throw new Error("ENOTFOUND");
    return map[h].map((address) => ({ address }));
  };
  const lookup = fake({ "public.test": ["93.184.216.34"], "rebind.test": ["93.184.216.34", "192.168.4.44"], "empty.test": [] });
  assert.strictEqual(await hostIsPrivate("public.test", lookup), false);
  assert.strictEqual(await hostIsPrivate("rebind.test", lookup), true, "any private address refuses the host");
  assert.strictEqual(await hostIsPrivate("empty.test", lookup), true, "no address fails closed");
  assert.strictEqual(await hostIsPrivate("nowhere.test", lookup), true, "a lookup error fails closed");
  assert.strictEqual(await hostIsPrivate("[::1]", lookup), true, "a bracketed v6 literal");
  assert.strictEqual(await hostIsPrivate("192.168.4.44", lookup), true, "a v4 literal is not looked up");
  console.log("net_guard: ok");
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
