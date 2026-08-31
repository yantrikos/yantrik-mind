// E.WEB0 XSS kill criterion, run in a REAL headless browser against the REAL served assets.
// A prompt-injected page's output must be inert in the mind's own UI. We render hostile model
// output through the actual renderMarkdown and trip a global flag if any injected script executes.
import { chromium } from "playwright";
import { readFileSync } from "node:fs";

const DIR = process.argv[2]; // assets dir
const html = readFileSync(`${DIR}/webui.html`, "utf8");
const css = readFileSync(`${DIR}/webui.css`, "utf8");
const js = readFileSync(`${DIR}/webui.js`, "utf8");

const PAYLOADS = [
  "<script>window.__xss='script-tag'</script>",
  "hello <img src=x onerror=\"window.__xss='img-onerror'\"> world",
  "[click me](javascript:window.__xss='js-uri')",
  "<iframe src=\"javascript:window.__xss='iframe'\"></iframe>",
  "<svg/onload=window.__xss='svg-onload'>",
  "**bold** normal `code` and <b onmouseover=window.__xss='onmouseover'>x</b>",
  "<a href=\"javascript:window.__xss='a-href'\">link</a>",
  "line1\n<script>window.__xss='multiline'</script>\nline3",
  "```\n<script>window.__xss='in-code-fence'</script>\n```",
];

const browser = await chromium.launch();
const page = await browser.newPage();

// Serve the real assets and stub the API so boot() runs the real code path.
await page.route("**/*", (route) => {
  const url = route.request().url();
  if (url.endsWith("/") || url.endsWith("/index.html")) return route.fulfill({ contentType: "text/html", body: html });
  if (url.endsWith("/app.css")) return route.fulfill({ contentType: "text/css", body: css });
  if (url.endsWith("/app.js")) return route.fulfill({ contentType: "application/javascript", body: js });
  if (url.includes("/api/me")) return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ mind: "Canary", person: "primary", operator: true }) });
  return route.fulfill({ status: 200, contentType: "application/json", body: "{}" });
});

const consoleErrors = [];
page.on("pageerror", (e) => consoleErrors.push(String(e)));
page.on("dialog", (d) => { page.__dialog = true; d.dismiss(); });

await page.goto("https://mind.local/");
await page.waitForTimeout(300);

const result = await page.evaluate((payloads) => {
  window.__xss = false;
  const out = { rendered: 0, xss: false, badNodes: [], textPreserved: [] };
  const host = document.createElement("div");
  document.body.appendChild(host);
  if (typeof renderMarkdown !== "function") return { error: "renderMarkdown not global" };
  for (const p of payloads) {
    const cell = document.createElement("div");
    host.appendChild(cell);
    renderMarkdown(cell, p);
    out.rendered++;
    // any live attack surface in the produced DOM?
    if (cell.querySelector("script")) out.badNodes.push("script:" + p.slice(0, 20));
    for (const el of cell.querySelectorAll("*")) {
      for (const attr of el.getAttributeNames()) {
        if (attr.startsWith("on")) out.badNodes.push(attr + ":" + p.slice(0, 20));
      }
      if (el.tagName === "A") {
        const href = el.getAttribute("href") || "";
        if (/^javascript:/i.test(href)) out.badNodes.push("js-href:" + p.slice(0, 20));
      }
      if (el.tagName === "IFRAME") out.badNodes.push("iframe:" + p.slice(0, 20));
    }
  }
  out.xss = window.__xss;
  out.dialog = !!window.__dialog;
  return out;
}, PAYLOADS);

await page.waitForTimeout(200);
const finalXss = await page.evaluate(() => window.__xss);

await browser.close();

const pass = result.rendered === PAYLOADS.length
  && result.xss === false
  && finalXss === false
  && result.badNodes.length === 0
  && !result.dialog;

console.log(JSON.stringify({ ...result, finalXss, pageErrors: consoleErrors.slice(0, 3), PASS: pass }, null, 2));
process.exit(pass ? 0 : 1);
