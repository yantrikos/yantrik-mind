// E.CB2 web checks — EXACT contracts (briefs/T1.txt, briefs/T2.txt), run INSIDE the checker image on a
// WRITABLE COPY of the artifact. Usage: node check_web.mjs t1|t2 <artifact-copy-dir>
// Fixed browser: Playwright 1.62.1 chromium, viewport 1280x800, 10 s load timeout.
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { chromium } from "playwright";
const [, , task, dir] = process.argv;
const PORT = 8123, base = `http://127.0.0.1:${PORT}`;
const verdict = { task: task.toUpperCase(), checks: {}, scraped: {} };
const check = (k, pass, extra = {}) => { verdict.checks[k] = { pass: !!pass, ...extra }; };
const expected = JSON.parse(fs.readFileSync("/checker/seed/expected.json", "utf8"));
const seed = fs.readFileSync("/checker/seed/leads.json", "utf8");

function staticServer(root) {
  const types = { ".html": "text/html", ".css": "text/css", ".js": "text/javascript", ".json": "application/json", ".png": "image/png", ".jpg": "image/jpeg", ".svg": "image/svg+xml", ".webp": "image/webp" };
  return http.createServer((req, res) => {
    let p = decodeURIComponent(req.url.split("?")[0]); if (p.endsWith("/")) p += "index.html";
    let f = path.join(root, p); if (!fs.existsSync(f) && fs.existsSync(f + ".html")) f += ".html";
    if (!fs.existsSync(f) || fs.statSync(f).isDirectory()) { res.writeHead(404); res.end(); return; }
    res.writeHead(200, { "content-type": types[path.extname(f)] || "application/octet-stream" }); res.end(fs.readFileSync(f));
  });
}
async function waitUp(ms = 15000) { const t = Date.now(); while (Date.now() - t < ms) { try { const r = await fetch(base + "/"); if (r.status < 500) return true; } catch {} await new Promise(r => setTimeout(r, 500)); } return false; }
let server = null, child = null;
const runSh = path.join(dir, "run.sh");
if (task === "t1") {
  fs.mkdirSync(path.join(dir, "data"), { recursive: true });
  fs.writeFileSync(path.join(dir, "data", "leads.json"), seed);           // the seed installed per the contract
  if (fs.existsSync(runSh)) { child = spawn("bash", ["run.sh"], { cwd: dir, stdio: "ignore" }); }
  else { server = staticServer(dir).listen(PORT); }
  check("run_sh_present", fs.existsSync(runSh));
} else { server = staticServer(dir).listen(PORT); }
check("site_up", await waitUp());
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
const errors = [];
page.on("console", m => { if (m.type() === "error") errors.push(m.text().slice(0, 120)); });
page.on("pageerror", e => errors.push(String(e).slice(0, 120)));
page.on("request", r => { const u = r.url(); if (!u.startsWith(base) && !u.startsWith("data:") && !u.startsWith("about:")) errors.push("external request: " + u.slice(0, 80)); });
let loaded = false;
try { await page.goto(base + "/", { timeout: 10000, waitUntil: "load" }); loaded = true; } catch (e) { errors.push("load: " + String(e).slice(0, 100)); }
check("loads_without_console_error_or_external_request", loaded && errors.length === 0, { errors });
const hrefs = loaded ? await page.$$eval("a[href]", as => as.map(a => a.getAttribute("href"))) : [];
const local = hrefs.filter(h => h && !/^(https?:|mailto:|tel:|#|data:)/.test(h));
const broken = [];
for (const h of local) { const r = await fetch(base + (h.startsWith("/") ? h : "/" + h)).catch(() => ({ status: 0 })); if (r.status !== 200) broken.push(h); }
check("relative_links_resolve", broken.length === 0, { checked: local.length, broken });

if (task === "t2") {
  const counts = await page.evaluate(() => ({
    projects: document.querySelectorAll("section#projects article").length,
    writing: document.querySelectorAll("section#writing article").length,
    contact: !!document.querySelector("section#contact") && (!!document.querySelector("section#contact a[href^='mailto:']") || !!document.querySelector("section#contact form")),
  }));
  verdict.scraped = counts;
  check("projects_section_has_4_articles", counts.projects >= 4);
  check("writing_section_has_3_articles", counts.writing >= 3);
  check("contact_section_reachable", counts.contact);
}
if (task === "t1") {
  const store = path.join(dir, "data", "leads.json");
  const form = await page.$("form#cb2-lead-form");
  check("form_cb2_lead_form_present", !!form);
  const sample = { name: "Checker Lead", email: "checker@example.in", phone: "+91 9876543210", business: "Checker Traders", message: "Checker message." };
  let appendedExactly = false, recordMatches = false, createdAtIso = false;
  if (form) {
    for (const [k, v] of Object.entries(sample)) { const el = await form.$(`[name="${k}"]`); if (el) await el.fill(v).catch(() => {}); }
    const btn = await form.$("button[type=submit], input[type=submit], button");
    if (btn) await btn.click().catch(() => {});
    await new Promise(r => setTimeout(r, 3000));
    try {
      const arr = JSON.parse(fs.readFileSync(store, "utf8"));
      appendedExactly = Array.isArray(arr) && arr.length === expected.total + 1;
      const last = arr[arr.length - 1] || {};
      recordMatches = Object.entries(sample).every(([k, v]) => last[k] === v) && Object.keys(last).sort().join(",") === "business,created_at,email,message,name,phone";
      createdAtIso = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(String(last.created_at || ""));
    } catch (e) { verdict.scraped.store_error = String(e).slice(0, 100); }
  }
  check("submit_appends_exactly_one_record", appendedExactly);
  check("appended_record_matches_submission_and_schema", recordMatches && createdAtIso);
  // dashboard on the pristine seed
  fs.writeFileSync(store, seed);
  let dash = null;
  try { await page.goto(base + "/dashboard", { timeout: 10000, waitUntil: "load" }); dash = await page.$eval("script#cb2-dashboard", s => s.textContent); } catch (e) { verdict.scraped.dashboard_error = String(e).slice(0, 100); }
  let d = null; try { d = JSON.parse(dash); } catch {}
  verdict.scraped.dashboard = d ? { total: d.total, per_day_keys: Object.keys(d.per_day || {}).length, recent: (d.recent || []).length } : null;
  check("dashboard_json_block_present", !!d);
  check("dashboard_total_exact", !!d && d.total === expected.total, { expected: expected.total });
  check("dashboard_per_day_exact_14_bins", !!d && JSON.stringify(d.per_day) === JSON.stringify(expected.per_day));
  check("dashboard_recent_five_exact_order", !!d && JSON.stringify(d.recent) === JSON.stringify(expected.five_most_recent));
}
await browser.close(); if (server) server.close(); if (child) child.kill("SIGKILL");
verdict.pass = Object.values(verdict.checks).every(c => c.pass);
console.log(JSON.stringify(verdict, null, 1));
process.exit(verdict.pass ? 0 : 1);
