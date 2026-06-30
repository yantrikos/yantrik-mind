// Tier-3 web fetch: render a URL with headless Chromium (Playwright + stealth) and print readable
// text. A real browser with our IP/TLS and a stealth-patched fingerprint — gets content from sites
// that block both the direct request and the reader proxy (JS-rendered + most bot walls).
//
// Deploy (on the box, as root, then chown to the service user):
//   cd /opt/yantrik-mind
//   npm install playwright playwright-extra puppeteer-extra-plugin-stealth
//   PLAYWRIGHT_BROWSERS_PATH=/opt/yantrik-mind/pw-browsers npx playwright install --with-deps chromium
//   chown -R yantrikmind:yantrikmind node_modules pw-browsers headless_fetch.js
// The Rust HttpFetcher spawns: `timeout 45 node headless_fetch.js <url>` with
// PLAYWRIGHT_BROWSERS_PATH set and cwd=/opt/yantrik-mind (so node resolves ./node_modules).
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
    const ctx = await browser.newContext({
      userAgent:
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36",
    });
    const page = await ctx.newPage();
    await page.goto(url, { waitUntil: "domcontentloaded", timeout: 25000 });
    await page.waitForTimeout(1500); // let late JS settle
    const text = await page.evaluate(() => (document.body ? document.body.innerText : ""));
    process.stdout.write(text || "");
  } catch (e) {
    console.error(String(e));
    process.exitCode = 1;
  } finally {
    await browser.close();
  }
})();
