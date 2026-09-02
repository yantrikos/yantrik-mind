// T1 / T2 mechanical checks with a fixed browser. Usage:
//   node check_web.mjs t2 <artifact-dir>            (static: served by the checker on 127.0.0.1:8123)
//   node check_web.mjs t1 <artifact-dir> [start-cmd] (T1: if the artifact ships a server, pass its
//                                                     start command; else static serve)
// Heuristics are stated inline; the verdict lists what was scraped so a grader can disagree.
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import pw from "file:///C:/Users/sync/AppData/Local/npm-cache/_npx/9833c18b2d85bc59/node_modules/playwright/index.js";
const { chromium } = pw;
const EXE = "C:/Users/sync/AppData/Local/ms-playwright/chromium_headless_shell-1223/chrome-headless-shell-win64/chrome-headless-shell.exe";
const [, , task, dir, startCmd] = process.argv;
const PORT = 8123;
const verdict = { task: task.toUpperCase(), checks: {}, scraped: {} };

function staticServer(root) {
  const types = { ".html": "text/html", ".css": "text/css", ".js": "text/javascript", ".json": "application/json", ".png": "image/png", ".jpg": "image/jpeg", ".svg": "image/svg+xml" };
  return http.createServer((req, res) => {
    let p = decodeURIComponent(req.url.split("?")[0]);
    if (p.endsWith("/")) p += "index.html";
    let f = path.join(root, p);
    if (!fs.existsSync(f) && fs.existsSync(f + ".html")) f = f + ".html";
    if (!fs.existsSync(f) || fs.statSync(f).isDirectory()) { res.writeHead(404); res.end("nf"); return; }
    res.writeHead(200, { "content-type": types[path.extname(f)] || "application/octet-stream" });
    res.end(fs.readFileSync(f));
  });
}
let server = null, child = null;
if (startCmd) {
  child = spawn(startCmd, { cwd: dir, shell: true, stdio: "ignore" });
  await new Promise(r => setTimeout(r, 4000));
} else {
  server = staticServer(dir).listen(PORT);
}
const base = `http://127.0.0.1:${PORT}`;
const browser = await chromium.launch({ executablePath: EXE });
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
const errors = [];
page.on("console", m => { if (m.type() === "error") errors.push(m.text().slice(0, 120)); });
page.on("pageerror", e => errors.push(String(e).slice(0, 120)));
const entry = fs.existsSync(path.join(dir, "index.html")) ? "/index.html" : "/" + (fs.readdirSync(dir).find(f => f.endsWith(".html")) || "");
let loaded = false;
try { await page.goto(base + entry, { timeout: 10000, waitUntil: "load" }); loaded = true; } catch (e) { errors.push("load: " + String(e).slice(0, 100)); }
verdict.checks.loads_without_console_error = { pass: loaded && errors.length === 0, errors };
// links
const hrefs = loaded ? await page.$$eval("a[href]", as => as.map(a => a.getAttribute("href"))) : [];
const local = hrefs.filter(h => h && !h.startsWith("http") && !h.startsWith("mailto:") && !h.startsWith("tel:") && !h.startsWith("#"));
let broken = [];
for (const h of local) {
  const r = await fetch(base + (h.startsWith("/") ? h : "/" + h)).catch(() => ({ status: 0 }));
  if (r.status !== 200) broken.push(h);
}
verdict.checks.links_resolve = { pass: broken.length === 0, checked: local.length, broken };
const text = loaded ? (await page.evaluate(() => document.body.innerText)) : "";
if (task === "t2") {
  const lower = text.toLowerCase();
  const section = (word) => { const i = lower.indexOf(word); return i < 0 ? "" : lower.slice(i, i + 2500); };
  const countItems = async (word) => {
    return await page.evaluate((w) => {
      const heads = [...document.querySelectorAll("h1,h2,h3,h4,section,nav a")].filter(e => e.textContent.toLowerCase().includes(w));
      let best = 0;
      for (const h of heads) {
        const scope = h.closest("section") || h.parentElement;
        if (!scope) continue;
        const n = scope.querySelectorAll("article, li, .card, .project, .post, h3, h4").length;
        best = Math.max(best, n);
      }
      return best;
    }, word);
  };
  const projects = await countItems("project"), posts = await countItems("writing") || await countItems("post") || await countItems("blog");
  const contact = lower.includes("contact") && (lower.includes("@") || lower.includes("mailto") || (await page.$$("form")).length > 0);
  verdict.scraped = { projects_items: projects, writing_items: posts, contact_present: contact };
  verdict.checks.projects_at_least_4 = { pass: projects >= 4 };
  verdict.checks.writing_at_least_3 = { pass: posts >= 3 };
  verdict.checks.contact_reachable = { pass: contact };
}
if (task === "t1") {
  const expected = JSON.parse(fs.readFileSync(path.join(path.dirname(new URL(import.meta.url).pathname.replace(/^\//, "")), "..", "seed", "expected.json"), "utf8"));
  // the form
  const inputs = await page.$$("form input:not([type=hidden]), form textarea");
  const submit = await page.$("form button, form input[type=submit]");
  verdict.checks.form_present = { pass: inputs.length >= 2 && !!submit, inputs: inputs.length };
  // the leads file: any *.json in the tree that looks like a list (the artifact's store)
  const jsons = fs.readdirSync(dir, { recursive: true }).filter(f => f.endsWith(".json") && !f.includes("node_modules") && !f.includes("package"));
  const before = Object.fromEntries(jsons.map(f => [f, fs.existsSync(path.join(dir, f)) ? fs.readFileSync(path.join(dir, f), "utf8").length : 0]));
  let appended = false;
  if (inputs.length >= 2 && submit) {
    for (const i of inputs) { const t = (await i.getAttribute("type")) || "text"; const nm = ((await i.getAttribute("name")) || "").toLowerCase();
      await i.fill(t === "email" || nm.includes("mail") ? "checker@example.in" : t === "tel" || nm.includes("phone") ? "+91 9876543210" : "Checker Lead").catch(() => {}); }
    await submit.click().catch(() => {});
    await new Promise(r => setTimeout(r, 2500));
    const after = Object.fromEntries(jsons.map(f => [f, fs.existsSync(path.join(dir, f)) ? fs.readFileSync(path.join(dir, f), "utf8").length : 0]));
    appended = jsons.some(f => after[f] > before[f]);
  }
  verdict.checks.form_appends_one_lead = { pass: appended, note: "a JSON store in the tree grew after submit (heuristic)" };
  // dashboard on the seed: the harness placed seed/leads.json over the store before this run
  let dashText = "";
  for (const cand of ["/dashboard", "/dashboard.html", "/dashboard/index.html"]) {
    try { const r = await page.goto(base + cand, { timeout: 10000 }); if (r && r.status() === 200) { dashText = await page.evaluate(() => document.body.innerText); break; } } catch {}
  }
  const nums = (dashText.match(/\d+/g) || []).map(Number);
  const names = expected.five_most_recent.filter(n => dashText.includes(n.split(" ")[0]));
  const perDay = Object.values(expected.per_day);
  const perDayFound = perDay.every(v => nums.includes(v));
  verdict.scraped.dashboard = { found: dashText.length > 0, numbers: nums.slice(0, 40), recent_names_matched: names.length };
  verdict.checks.dashboard_total = { pass: nums.includes(expected.total), expected: expected.total };
  verdict.checks.dashboard_recent_five = { pass: names.length === 5 };
  verdict.checks.dashboard_per_day = { pass: perDayFound, note: "every expected per-day count appears as a number on the page (weak necessary condition, stated)" };
}
await browser.close();
if (server) server.close();
if (child) child.kill();
verdict.pass = Object.values(verdict.checks).every(c => c.pass);
console.log(JSON.stringify(verdict, null, 1));
