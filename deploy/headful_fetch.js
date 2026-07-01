// Tier-4 web fetch: render a URL with a HEADFUL (real, on-screen) Chromium via a virtual display
// (Xvfb) + stealth, and print readable text. Headful defeats the "is this headless?" fingerprint that
// blocks the tier-3 headless shell — PROVEN to get real product grids + prices from Amazon and Target
// where headless returns 0 chars. Slower + heavier than headless, so it's used ONLY for hostile retail.
//
// It does NOT beat interactive human-challenge walls: Walmart's PerimeterX "press and hold" still blocks.
//
// Deps on the box: xvfb + xauth (apt install xvfb xauth) + the FULL chromium (pw-browsers/chromium-*,
// not just the headless shell). Invoked as:
//   PLAYWRIGHT_BROWSERS_PATH=/opt/yantrik-mind/pw-browsers cd /opt/yantrik-mind
//   timeout 80 xvfb-run -a node headful_fetch.js <url>
const { chromium } = require("playwright-extra");
const stealth = require("puppeteer-extra-plugin-stealth")();
chromium.use(stealth);

(async () => {
  const url = process.argv[2];
  if (!url) {
    console.error("usage: headful_fetch.js <url>");
    process.exit(2);
  }
  const browser = await chromium.launch({
    headless: false, // the whole point — a real browser, not the detectable headless shell
    args: ["--no-sandbox", "--disable-dev-shm-usage", "--start-maximized"],
  });
  try {
    // Do NOT override the UA — let Chromium send its own, self-consistent with its real version +
    // Sec-CH-UA client hints. A spoofed stale UA (Chrome 124) contradicts the real engine/hints and is
    // itself a bot signal. Only set Accept-Language + a real viewport.
    const ctx = await browser.newContext({
      locale: "en-US",
      extraHTTPHeaders: { "Accept-Language": "en-US,en;q=0.9" },
      viewport: { width: 1366, height: 900 },
    });
    const page = await ctx.newPage();
    try {
      await page.goto(url, { waitUntil: "networkidle", timeout: 25000 });
    } catch (e) {
      try {
        await page.goto(url, { waitUntil: "domcontentloaded", timeout: 15000 });
      } catch (_) {}
    }
    // Scroll to trigger lazy-loaded product tiles/prices.
    for (let i = 0; i < 5; i++) {
      await page.mouse.wheel(0, 2400);
      await page.waitForTimeout(700);
    }
    await page.waitForTimeout(2000);
    const text = await page.evaluate(() => (document.body ? document.body.innerText : ""));
    process.stdout.write(text || "");
  } catch (e) {
    console.error(String(e));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
