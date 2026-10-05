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
const dns = require("dns").promises;
const net = require("net");
const http = require("http");
const https = require("https");
const RANGES = require("./private_ranges.json");

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
      throw new Error(`blocked: ${h} does not resolve`);
    }
  }
  if (!addrs.length) throw new Error(`blocked: ${h} resolves to nothing`);
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

// Fetch one request, connected to the address that was checked, following no redirect.
async function fetchPinned(urlText, req = {}, deps = {}) {
  const u = new URL(urlText);
  if (u.protocol !== "http:" && u.protocol !== "https:") throw new Error("blocked: not http(s)");
  const host = u.hostname.replace(/^\[|\]$/g, "");
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
      (res) => {
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
      },
    );
    r.on("error", reject);
    r.setTimeout(15000, () => r.destroy(new Error("timeout")));
    if (req.body) r.write(req.body);
    r.end();
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
  if (typeof ctx.routeWebSocket === "function") {
    await ctx.routeWebSocket(/.*/, (ws) => ws.close());
  }
}

module.exports = { privateIp, v6ToBigInt, checkedAddresses, hostIsPrivate, fetchPinned, guardContext };
