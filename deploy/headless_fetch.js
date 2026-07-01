// Tier-3 web fetch: render a URL with headless Chromium (Playwright + stealth) and print readable
// text. A real browser with our IP/TLS and a stealth-patched fingerprint — gets content from sites
// that block both the direct request and the reader proxy (JS-rendered + most bot walls).
//
// Tuned for JS-rendered commerce/content: waits for network-idle (so client-side content like price
// grids loads) and SCROLLS to trigger lazy-loaded product tiles, then reads innerText. This recovers
// friendly/mid-tier retailers (proven: Rosefield $60–$189) that the naive domcontentloaded grab missed.
// NOTE: this does NOT beat network-level bot walls — Amazon/Walmart/Target return nothing even here;
// those need a real product API or a scraping aggregator, not a browser.
//
// Deploy (on the box, as root, then chown to the service user):
//   cd /opt/yantrik-mind
//   npm install playwright playwright-extra puppeteer-extra-plugin-stealth
//   PLAYWRIGHT_BROWSERS_PATH=/opt/yantrik-mind/pw-browsers npx playwright install --with-deps chromium
//   chown -R yantrikmind:yantrikmind node_modules pw-browsers headless_fetch.js
// The Rust HttpFetcher spawns: `timeout 45 node headless_fetch.js <url>` with
// PLAYWRIGHT_BROWSERS_PATH set (also in /etc/yantrik-mind.env) and cwd=/opt/yantrik-mind.
const { chromium } = require("playwright-extra");
const stealth = require("puppeteer-extra-plugin-stealth")();
chromium.use(stealth);

(async () => {
  const url = process.argv[2];
  if (!url) {
    console.error("usage: headless_fetch.js <url>");
    process.exit(2);
  }
  const browser = await chromium.launch({
    args: ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"], // --no-sandbox: unprivileged LXC
  });
  try {
    // Let Chromium send its own UA (self-consistent with its real version + Sec-CH-UA client hints);
    // a spoofed stale UA contradicts the real engine and is itself a bot signal.
    const ctx = await browser.newContext({
      locale: "en-US",
      extraHTTPHeaders: { "Accept-Language": "en-US,en;q=0.9" },
    });
    const page = await ctx.newPage();
    // Prefer network-idle so client-side content (price grids, product tiles) has loaded; fall back to
    // domcontentloaded if the site keeps a connection open past the budget.
    try {
      await page.goto(url, { waitUntil: "networkidle", timeout: 18000 });
    } catch (e) {
      try {
        await page.goto(url, { waitUntil: "domcontentloaded", timeout: 12000 });
      } catch (_) {}
    }
    // Scroll to trigger lazy-loaded product grids (many stores only render tiles/prices on view).
    for (let i = 0; i < 5; i++) {
      await page.mouse.wheel(0, 2400);
      await page.waitForTimeout(600);
    }
    await page.waitForTimeout(1200); // final settle
    const text = await page.evaluate(() => (document.body ? document.body.innerText : ""));
    process.stdout.write(text || "");
  } catch (e) {
    console.error(String(e));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
