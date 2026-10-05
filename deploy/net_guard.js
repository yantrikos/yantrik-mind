// E.NET1 (the seventh egress review, item 4): what the browser fallback may reach.
//
// headless_fetch.js and headful_fetch.js render pages an injected link may choose, with page script
// running. Without this, a page could redirect to, or script a request at, http://192.168.4.x (a
// router, Home Assistant) and the answer would come back as "web" text. Every request the page makes
// goes through `guardContext`: its host is resolved and refused when any address is private, loopback,
// link-local, unique-local, carrier-grade NAT or multicast; and it is fetched here with redirects OFF,
// so each hop of a redirect comes back through the same check. Service workers are blocked (they would
// bypass routing). Deploy this file beside the two scripts.
//
// Residual, stated: the check resolves, then the fetch resolves again (DNS rebinding can still swap
// the address in between); WebSockets are closed only where Playwright can route them.
const dns = require("dns").promises;
const net = require("net");

function privateIp(ip) {
  const v = String(ip).toLowerCase();
  if (net.isIPv4(v)) {
    const [a, b] = v.split(".").map(Number);
    return (
      a === 0 || a === 10 || a === 127 || a >= 224 ||
      (a === 100 && b >= 64 && b <= 127) ||
      (a === 169 && b === 254) ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168)
    );
  }
  if (v.startsWith("::ffff:")) return privateIp(v.slice(7));
  return (
    v === "::1" || v === "::" ||
    v.startsWith("fc") || v.startsWith("fd") ||
    /^fe[89ab]/.test(v) || v.startsWith("ff")
  );
}

// A host is refused when it is a private literal, resolves to any private address, or does not
// resolve at all (fail closed).
async function hostIsPrivate(hostname, lookup = dns.lookup) {
  const h = String(hostname).replace(/^\[|\]$/g, "");
  if (net.isIP(h)) return privateIp(h);
  try {
    const addrs = await lookup(h, { all: true });
    return addrs.length === 0 || addrs.some((a) => privateIp(a.address));
  } catch (_) {
    return true;
  }
}

async function guardContext(ctx) {
  await ctx.route("**/*", async (route) => {
    let u;
    try {
      u = new URL(route.request().url());
    } catch (_) {
      return route.abort("blockedbyclient");
    }
    if (u.protocol === "data:" || u.protocol === "blob:") return route.continue();
    if (u.protocol !== "http:" && u.protocol !== "https:") return route.abort("blockedbyclient");
    if (await hostIsPrivate(u.hostname)) return route.abort("blockedbyclient");
    try {
      // Redirects OFF: a 3xx comes back to the page, the browser follows it, and that request is
      // routed -- and checked -- again.
      const response = await route.fetch({ maxRedirects: 0 });
      return route.fulfill({ response });
    } catch (_) {
      return route.abort("failed");
    }
  });
  if (typeof ctx.routeWebSocket === "function") {
    await ctx.routeWebSocket(/.*/, (ws) => ws.close());
  }
}

module.exports = { privateIp, hostIsPrivate, guardContext };
