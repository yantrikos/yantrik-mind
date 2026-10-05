// SEE a page: render a URL with headless Chromium (Playwright + stealth) and save a JPEG
// screenshot. The vision lane's front half — what text extraction can't capture (layouts, images,
// JS-only content, some bot-walled pages that still render) becomes analyzable by a vision model.
//
// Usage: node snap_page.js <url> <out.jpg>
// Same install/deploy notes as headless_fetch.js (shares node_modules + PLAYWRIGHT_BROWSERS_PATH).
const { chromium } = require("playwright-extra");
const stealth = require("puppeteer-extra-plugin-stealth")();
// E.NET1c (the tenth pass): a screenshot of a LAN admin page would be described by vision -- every
// request goes through net_guard.js (deployed beside), service workers blocked.
const { launchGuarded } = require("./net_guard");
chromium.use(stealth);

(async () => {
  const url = process.argv[2];
  const out = process.argv[3];
  if (!url || !out) {
    console.error("usage: snap_page.js <url> <out.jpg>");
    process.exit(2);
  }
  // E.NET1d: the browser comes guarded from net_guard, service workers blocked, or not at all.
  const { browser, ctx } = await launchGuarded(
    chromium,
    { args: ["--no-sandbox", "--disable-dev-shm-usage", "--disable-gpu"] }, // --no-sandbox: unprivileged LXC
    { viewport: { width: 1280, height: 1800 } },
  );
  try {
    const page = await ctx.newPage();
    await page.goto(url, { waitUntil: "networkidle", timeout: 40000 }).catch(() => {});
    // A short scroll pass triggers lazy-loaded content, then back to the top for the shot.
    await page.evaluate(async () => {
      await new Promise((done) => {
        let y = 0;
        const t = setInterval(() => {
          window.scrollBy(0, 600);
          y += 600;
          if (y >= 2400) {
            clearInterval(t);
            window.scrollTo(0, 0);
            done();
          }
        }, 150);
      });
    }).catch(() => {});
    await page.waitForTimeout(700);
    await page.screenshot({ path: out, type: "jpeg", quality: 70, fullPage: false });
    console.log("ok");
  } finally {
    await browser.close();
  }
})().catch((e) => {
  console.error(String(e).slice(0, 200));
  process.exit(1);
});
