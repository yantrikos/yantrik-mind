// E.CB2 web checks — EXACT contracts (briefs/T1.txt, briefs/T2.txt), run INSIDE the checker image
// (no network) on a WRITABLE COPY of the artifact. Usage: node check_web.mjs t1|t2 <copy-dir> [excerpts-file]
// The verdict on stdout is counts and booleans only; any text excerpt goes to the excerpts file.
// Fixed browser: Playwright 1.62.1 chromium, viewport 1280x800, 10 s load timeout.
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { chromium } from "playwright";
const [, , task, dir, excerptsFile] = process.argv;
const PORT = 8123, base = `http://127.0.0.1:${PORT}`;
const verdict = { task: task.toUpperCase(), checks: {}, counts: {} };
const excerpts = [];
const note = (k, v) => excerpts.push(`[${k}] ${String(v).slice(0, 300)}`);
const check = (k, pass, counts = {}) => { verdict.checks[k] = { pass: !!pass, ...counts }; };
const expected = JSON.parse(fs.readFileSync("/checker/seed/expected.json", "utf8"));
const seed = fs.readFileSync("/checker/seed/leads.json", "utf8");

function staticServer(root) {
  const types = { ".html": "text/html", ".css": "text/css", ".js": "text/javascript", ".json": "application/json", ".png": "image/png", ".jpg": "image/jpeg", ".svg": "image/svg+xml", ".webp": "image/webp" };
  const rootAbs = path.resolve(root);
  return http.createServer((req, res) => {
    let p = decodeURIComponent(req.url.split("?")[0]); if (p.endsWith("/")) p += "index.html";
    let f = path.resolve(rootAbs, "." + path.posix.normalize("/" + p));
    if (!f.startsWith(rootAbs + path.sep) && f !== rootAbs) { res.writeHead(403); res.end(); return; }   // no escape above the root
    if (!fs.existsSync(f) && fs.existsSync(f + ".html")) f += ".html";
    if (!fs.existsSync(f) || fs.statSync(f).isDirectory()) { res.writeHead(404); res.end(); return; }
    res.writeHead(200, { "content-type": types[path.extname(f)] || "application/octet-stream" }); res.end(fs.readFileSync(f));
  });
}
async function waitUp(ms = 15000) { const t = Date.now(); while (Date.now() - t < ms) { try { const r = await fetch(base + "/"); if (r.status < 500) return true; } catch {} await new Promise(r => setTimeout(r, 500)); } return false; }
let server = null, child = null;
const runSh = path.join(dir, "run.sh");
if (task === "t1") {
  fs.mkdirSync(path.join(dir, "data"), { recursive: true });
  fs.writeFileSync(path.join(dir, "data", "leads.json"), seed);
  if (fs.existsSync(runSh)) { child = spawn("bash", ["run.sh"], { cwd: dir, stdio: "ignore" }); } else { server = staticServer(dir).listen(PORT); }
  check("run_sh_present", fs.existsSync(runSh));
} else { server = staticServer(dir).listen(PORT); }
check("site_up", await waitUp());
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
let consoleErrors = 0, externalRequests = 0, loadErrors = 0;
page.on("console", m => { if (m.type() === "error") { consoleErrors++; note("console", m.text()); } });
page.on("pageerror", e => { consoleErrors++; note("pageerror", e); });
page.on("request", r => { const u = r.url(); if (!u.startsWith(base) && !u.startsWith("data:") && !u.startsWith("about:")) { externalRequests++; note("external", u); } });
let loaded = false;
try { await page.goto(base + "/", { timeout: 10000, waitUntil: "load" }); loaded = true; } catch (e) { loadErrors++; note("load", e); }
check("loads_without_console_error_or_external_request", loaded && consoleErrors === 0 && externalRequests === 0, { console_errors: consoleErrors, external_requests: externalRequests, load_errors: loadErrors });
const hrefs = loaded ? await page.$$eval("a[href]", as => as.map(a => a.getAttribute("href"))) : [];
const local = hrefs.filter(h => h && !/^(https?:|mailto:|tel:|#|data:)/.test(h));
let broken = 0;
for (const h of local) { const r = await fetch(base + (h.startsWith("/") ? h : "/" + h)).catch(() => ({ status: 0 })); if (r.status !== 200) { broken++; note("broken-link", h); } }
check("relative_links_resolve", broken === 0, { checked: local.length, broken });

if (task === "t2") {
  const c = await page.evaluate(() => ({
    projects: document.querySelectorAll("section#projects article").length,
    writing: document.querySelectorAll("section#writing article").length,
    contact: !!document.querySelector("section#contact") && (!!document.querySelector("section#contact a[href^='mailto:']") || !!document.querySelector("section#contact form")),
  }));
  verdict.counts = { projects_articles: c.projects, writing_articles: c.writing };
  check("projects_section_has_4_articles", c.projects >= 4, { articles: c.projects });
  check("writing_section_has_3_articles", c.writing >= 3, { articles: c.writing });
  check("contact_section_reachable", c.contact);
}
if (task === "t1") {
  const store = path.join(dir, "data", "leads.json");
  const form = await page.$("form#cb2-lead-form");
  check("form_cb2_lead_form_present", !!form);
  const sample = { name: "Checker Lead", email: "checker@example.in", phone: "+91 9876543210", business: "Checker Traders", message: "Checker message." };
  let appendedExactly = false, recordMatches = false, createdAtIso = false, len = -1;
  if (form) {
    for (const [k, v] of Object.entries(sample)) { const el = await form.$(`[name="${k}"]`); if (el) await el.fill(v).catch(() => {}); }
    const btn = await form.$("button[type=submit], input[type=submit], button");
    if (btn) await btn.click().catch(() => {});
    await new Promise(r => setTimeout(r, 3000));
    try {
      const arr = JSON.parse(fs.readFileSync(store, "utf8")); len = Array.isArray(arr) ? arr.length : -1;
      appendedExactly = Array.isArray(arr) && arr.length === expected.total + 1;
      const last = arr[arr.length - 1] || {};
      recordMatches = Object.entries(sample).every(([k, v]) => last[k] === v) && Object.keys(last).sort().join(",") === "business,created_at,email,message,name,phone";
      createdAtIso = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(String(last.created_at || ""));
    } catch (e) { note("store", e); }
  }
  check("submit_appends_exactly_one_record", appendedExactly, { store_length_after: len, expected_length: expected.total + 1 });
  check("appended_record_matches_submission_and_schema", recordMatches && createdAtIso);
  fs.writeFileSync(store, seed);
  let d = null;
  try { await page.goto(base + "/dashboard", { timeout: 10000, waitUntil: "load" }); d = JSON.parse(await page.$eval("script#cb2-dashboard", s => s.textContent)); } catch (e) { note("dashboard", e); }
  verdict.counts.dashboard = d ? { total: d.total, per_day_keys: Object.keys(d.per_day || {}).length, recent: (d.recent || []).length } : null;
  check("dashboard_json_block_present", !!d);
  check("dashboard_total_exact", !!d && d.total === expected.total, { expected: expected.total });
  check("dashboard_per_day_exact_14_bins", !!d && JSON.stringify(d.per_day) === JSON.stringify(expected.per_day));
  check("dashboard_recent_five_exact_order", !!d && JSON.stringify(d.recent) === JSON.stringify(expected.five_most_recent));
}
await browser.close(); if (server) server.close(); if (child) child.kill("SIGKILL");
verdict.pass = Object.values(verdict.checks).every(c => c.pass);
if (excerptsFile) fs.writeFileSync(excerptsFile, excerpts.join("\n") + "\n");
console.log(JSON.stringify(verdict, null, 1));
process.exit(verdict.pass ? 0 : 1);
