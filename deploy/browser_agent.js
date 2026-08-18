// PERSISTENT BROWSER SESSION — the difference between fetching a page and USING one.
//
// The other three scripts (headless_fetch, headful_fetch, snap_page) each launch a browser, take
// one look, and die. That is enough to read the web and not nearly enough to act on it: every
// multi-step job — log in, search, filter, fill, review — needs the SAME tab to still be there on
// the next step, with its cookies, its scroll position, and its half-filled form intact.
//
// So this is a long-lived process driven over stdin: one JSON command per line, one JSON result per
// line. The Rust side owns the conversation; this owns the tab.
//
// THE SUBMIT BOUNDARY IS ENFORCED HERE, not upstream. Commands that merely look or prepare
// (goto/observe/fill/click-nonsubmit/scroll) always run. Commands that COMMIT — pressing a button
// whose text reads like buy/pay/send/delete/confirm — are refused unless the command carries
// `"armed": true`, which the Rust side only sets after a human confirmation. Putting the check in
// the driver means a model that talks its way past the prompt still cannot reach the button: the
// process holding the mouse refuses. Policy upstream is advice; this is the wall.
//
// Deploy (on the box, as root, then chown to the service user):
//   cd /opt/yantrik-mind && npm install playwright playwright-extra puppeteer-extra-plugin-stealth
//   PLAYWRIGHT_BROWSERS_PATH=/opt/yantrik-mind/pw-browsers npx playwright install --with-deps chromium
// Run:   node browser_agent.js [--profile <dir>] [--headful]
const { chromium } = require("playwright-extra");
const stealth = require("puppeteer-extra-plugin-stealth")();
chromium.use(stealth);
const readline = require("readline");

// Words that mean "this cannot be undone by pressing back". Matched against the accessible name of
// whatever is about to be clicked. Deliberately broad: a false positive costs one confirmation, a
// false negative costs money or a sent message.
const COMMIT_WORDS = [
  "buy", "purchase", "order", "checkout", "pay", "payment", "subscribe", "place order",
  "send", "submit", "post", "publish", "tweet", "reply", "confirm", "book now", "reserve",
  "delete", "remove", "cancel subscription", "deactivate", "close account", "transfer",
  "withdraw", "sign contract", "agree and", "accept and", "apply now",
];

function looksLikeCommit(label) {
  const t = (label || "").toLowerCase().trim();
  if (!t) return false;
  return COMMIT_WORDS.some((w) => t.includes(w));
}

let browser = null;
let ctx = null;
let page = null;

async function ensure(opts) {
  if (page) return;
  const headful = opts.headful || process.argv.includes("--headful");
  const pi = process.argv.indexOf("--profile");
  const profile = pi >= 0 ? process.argv[pi + 1] : null;
  const args = ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"];
  if (profile) {
    // A persistent profile keeps logins between RUNS, not just between steps.
    ctx = await chromium.launchPersistentContext(profile, { headless: !headful, args, locale: "en-US" });
    browser = ctx.browser();
    page = ctx.pages()[0] || (await ctx.newPage());
  } else {
    browser = await chromium.launch({ headless: !headful, args });
    ctx = await browser.newContext({ locale: "en-US" });
    page = await ctx.newPage();
  }
  page.setDefaultTimeout(20000);
}

// What the model needs to decide the next step: the page's text, plus the INTERACTIVE elements
// with stable indices it can refer back to. Indices beat CSS selectors here — a model inventing a
// selector fails silently, while an index either exists or doesn't.
async function observe(maxChars) {
  const url = page.url();
  const title = await page.title().catch(() => "");
  const els = await page.evaluate(() => {
    const out = [];
    const sel = 'a,button,input,textarea,select,[role="button"],[role="link"],[role="textbox"]';
    document.querySelectorAll(sel).forEach((e) => {
      const r = e.getBoundingClientRect();
      if (r.width < 2 || r.height < 2) return; // invisible
      const style = window.getComputedStyle(e);
      if (style.visibility === "hidden" || style.display === "none") return;
      const label =
        (e.getAttribute("aria-label") || e.innerText || e.value || e.placeholder || e.name || e.title || "")
          .toString().replace(/\s+/g, " ").trim().slice(0, 80);
      out.push({ tag: e.tagName.toLowerCase(), type: e.type || "", label });
    });
    return out;
  });
  const text = await page.evaluate(() => (document.body ? document.body.innerText : ""));
  return {
    url,
    title,
    elements: els.map((e, i) => ({ i, ...e })),
    text: (text || "").slice(0, maxChars || 4000),
  };
}

// Resolve an element index back to a handle, in the SAME order observe() used.
async function handleAt(index) {
  const sel = 'a,button,input,textarea,select,[role="button"],[role="link"],[role="textbox"]';
  const all = await page.$$(sel);
  const visible = [];
  for (const h of all) {
    const box = await h.boundingBox().catch(() => null);
    if (box && box.width >= 2 && box.height >= 2) visible.push(h);
  }
  return visible[index] || null;
}

async function labelOf(h) {
  return await h
    .evaluate((e) => (e.getAttribute("aria-label") || e.innerText || e.value || e.title || "").toString().trim())
    .catch(() => "");
}

async function run(cmd) {
  switch (cmd.op) {
    case "open":
      await ensure(cmd);
      return { ok: true };
    case "goto": {
      await ensure(cmd);
      await page.goto(cmd.url, { waitUntil: cmd.wait || "domcontentloaded", timeout: 25000 });
      await page.waitForTimeout(600);
      return { ok: true, ...(await observe(cmd.max_chars)) };
    }
    case "observe":
      await ensure(cmd);
      return { ok: true, ...(await observe(cmd.max_chars)) };
    case "screenshot": {
      await ensure(cmd);
      const buf = await page.screenshot({ fullPage: false });
      return { ok: true, jpeg_b64: buf.toString("base64") };
    }
    case "fill": {
      await ensure(cmd);
      const h = await handleAt(cmd.index);
      if (!h) return { ok: false, error: `no element at index ${cmd.index}` };
      await h.fill(String(cmd.value ?? ""));
      return { ok: true, filled: await labelOf(h) };
    }
    case "click": {
      await ensure(cmd);
      const h = await handleAt(cmd.index);
      if (!h) return { ok: false, error: `no element at index ${cmd.index}` };
      const label = await labelOf(h);
      // THE WALL. A commit-shaped control needs an armed command; nothing upstream can wish it away.
      if (looksLikeCommit(label) && !cmd.armed) {
        return { ok: false, blocked: true, needs_confirmation: true, label,
          error: `refusing to click "${label}" — that looks irreversible and this command was not armed` };
      }
      await h.click({ timeout: 15000 });
      await page.waitForTimeout(900);
      return { ok: true, clicked: label, ...(await observe(cmd.max_chars)) };
    }
    case "press": {
      await ensure(cmd);
      // Enter inside a form submits it — treat it as a commit unless armed.
      if (String(cmd.key).toLowerCase() === "enter" && !cmd.armed) {
        return { ok: false, blocked: true, needs_confirmation: true,
          error: "refusing to press Enter unarmed — it submits forms" };
      }
      await page.keyboard.press(cmd.key);
      await page.waitForTimeout(700);
      return { ok: true, ...(await observe(cmd.max_chars)) };
    }
    case "scroll":
      await ensure(cmd);
      await page.mouse.wheel(0, cmd.dy || 1200);
      await page.waitForTimeout(400);
      return { ok: true, ...(await observe(cmd.max_chars)) };
    case "close":
      if (browser) await browser.close().catch(() => {});
      if (ctx && !browser) await ctx.close().catch(() => {});
      browser = ctx = page = null;
      return { ok: true, closed: true };
    default:
      return { ok: false, error: `unknown op ${cmd.op}` };
  }
}

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", async (line) => {
  if (!line.trim()) return;
  let res;
  try {
    res = await run(JSON.parse(line));
  } catch (e) {
    res = { ok: false, error: String(e && e.message ? e.message : e) };
  }
  process.stdout.write(JSON.stringify(res) + "\n");
});
rl.on("close", async () => {
  if (browser) await browser.close().catch(() => {});
  process.exit(0);
});
