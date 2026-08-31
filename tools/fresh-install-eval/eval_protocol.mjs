// E.WEB0 comparative eval — the scripted fresh-human protocol, one script for every contender.
// Measures, per product: SETUP (service-ready → able to chat), FIRST REPLY (send → non-empty
// assistant text rendered), and INTERACTIONS (fields filled + clicks — the human cost of onboarding).
// The script IS the naive path: zero documentation, zero env vars, whatever the landing page offers.
import { chromium } from "playwright";

const target = process.argv[2]; // "mind" | "openwebui"
const base = process.argv[3];   // base URL
const secret = process.argv[4]; // mind: pairing code; openwebui: unused

const PROMPT = "hello! in one sentence, what can you do?";
let interactions = 0;
async function fill(page, sel, val) { await page.fill(sel, val); interactions++; }
async function click(page, sel) { await page.click(sel); interactions++; }

const browser = await chromium.launch();
const page = await browser.newPage();
page.setDefaultTimeout(120000);

const t0 = Date.now();
await page.goto(base);

let tReady;
if (target === "mind") {
  // Ceremony: code → two names → Begin (the intro turn then auto-sends).
  await fill(page, "#pair-code", secret);
  await click(page, "#pair-btn");
  await page.waitForSelector("#mind-name-input", { state: "visible" });
  await fill(page, "#mind-name-input", "Vega");
  await fill(page, "#user-name", "Eval Human");
  tReady = Date.now();
  await click(page, "#name-btn");
  // The intro turn IS the first turn: wait for the mind's first rendered reply.
  await page.waitForFunction(() => {
    const b = document.querySelectorAll(".msg.mind .md");
    return b.length > 0 && [...b].some((n) => n.textContent.trim().length > 0);
  });
} else if (target === "openwebui") {
  // Fresh instance: "Get started" → admin signup (name/email/password) → chat surface.
  const getStarted = page.locator("text=/get started/i").first();
  try { await getStarted.click({ timeout: 15000 }); interactions++; } catch (_) {}
  try {
    await fill(page, 'input[autocomplete="name"], input[placeholder*="name" i]', "Eval Human");
    await fill(page, 'input[type="email"], input[autocomplete="email"]', "eval@example.com");
    await fill(page, 'input[type="password"]', "eval-password-123");
    await click(page, 'button[type="submit"]');
  } catch (e) { console.error("signup flow variance:", String(e).slice(0, 120)); }
  // Some builds interpose a changelog/whats-new dialog before the composer is usable.
  try { await page.locator("text=/okay|let's go|continue/i").first().click({ timeout: 8000 }); interactions++; } catch (_) {}
  await page.waitForSelector("#chat-input, textarea", { state: "visible" });
  // FRESH-INSTALL FINDING (recorded in the eval, charged to the product): the default selected
  // model is whatever the endpoint lists first — here mxbai-embed-large, an EMBEDDING model that
  // cannot chat. The naive human must discover the model picker and choose a chat model; those
  // are real interactions on the naive path and are counted.
  try {
    await page.waitForSelector('[aria-label^="Selected model"]', { timeout: 30000 });
    // Mobile+desktop variants both render; first() can be the hidden twin. Click the VISIBLE one,
    // and fall back to a direct DOM dispatch if actionability still stalls.
    const picker = page.locator('[aria-label^="Selected model"]:visible').first();
    try { await picker.click({ timeout: 8000 }); } catch (_) {
      await page.evaluate(() => { const els=[...document.querySelectorAll('[aria-label^="Selected model"]')]; const v=els.find(e=>e.offsetParent!==null)||els[0]; v && v.click(); });
    }
    interactions++;
    await page.keyboard.type("qwen"); interactions++;
    await page.waitForTimeout(1500);
    await page.locator('[role="option"]:has-text("qwen"), button:has-text("qwen3.8")').first().click({ timeout: 10000 }); interactions++;
  } catch (e) { console.error("model-picker variance:", String(e).slice(0, 100)); }
  tReady = Date.now();
  const input = page.locator("#chat-input, textarea").first();
  await input.fill(PROMPT); interactions++;
  await page.keyboard.press("Enter"); interactions++;
  // Reply detection: an ASSISTANT bubble (never the composer, whose id also starts with
  // "message"), with meaningful text that is not our own prompt echoed back.
  await page.waitForFunction((prompt) => {
    const els = document.querySelectorAll('.chat-assistant, [id^="message"]:not(#message-input-container) .markdown-prose, .markdown-prose');
    return [...els].some((n) => {
      const t = n.textContent.trim();
      return t.length > 20 && !t.includes(prompt) && !n.closest("#message-input-container");
    });
  }, PROMPT, { timeout: 180000 }).catch(() => {
    // The naive path CAN fail outright (observed: the embedding-model default answers nothing).
    // A DNF is a protocol RESULT, not a harness crash.
    console.log(JSON.stringify({ target, outcome: "DNF", note: "no answered turn within 180s", interactions, setup_s: +(((tReady - t0) / 1000)).toFixed(1) }, null, 1));
    process.exit(2);
  });
} else {
  throw new Error("unknown target");
}
const tReply = Date.now();

console.log(JSON.stringify({
  target,
  setup_s: +( (tReady - t0) / 1000 ).toFixed(1),
  first_reply_s: +(((tReply - tReady) / 1000)).toFixed(1),
  total_s: +(((tReply - t0) / 1000)).toFixed(1),
  interactions,
}, null, 1));
await browser.close();
