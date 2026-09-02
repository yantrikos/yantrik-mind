// Mind web client. One rule above all others: MODEL OUTPUT IS HOSTILE INPUT. Nothing the mind
// says ever reaches innerHTML — the markdown renderer below builds DOM nodes and assigns text via
// textContent only. And one rule of identity: the UI never asserts the mind's name — it asks
// (/api/me carries it, paired or not) and displays what the mind calls itself.
"use strict";

const $ = (id) => document.getElementById(id);
const feed = $("feed");
const input = $("input");
const HDRS = { "Content-Type": "application/json", "X-YM-Web": "1" };

// Every mutating call goes through here (Codex review 865248f9/ead24438: handlers that parsed
// error bodies as JSON fell back to {} and reported success copy on 403s, clearing the form).
// Contract: ok is the TRANSPORT verdict — callers may only show success copy, clear inputs, or
// refresh state when ok is true; on failure they show status+text and PRESERVE the form.
async function postJson(path, body) {
  try {
    const r = await fetch(path, { method: "POST", headers: HDRS, body: JSON.stringify(body) });
    const text = await r.text();
    let data = null;
    try { data = JSON.parse(text); } catch (_) {}
    return { ok: r.ok, status: r.status, data, text };
  } catch (_) {
    return { ok: false, status: 0, data: null, text: "could not reach the mind" };
  }
}
let MIND = "the mind";

/* ── boot ─────────────────────────────────────────────────────────────── */
async function boot() {
  try {
    const r = await fetch("/api/me", { headers: { "X-YM-Web": "1" } });
    const me = await r.json().catch(() => ({}));
    if (me.mind) setMindName(me.mind);
    if (r.ok) {
      $("person-chip").textContent = me.person || "operator";
      if (!me.operator) hideOperatorPanels();
      showApp();
      return;
    }
  } catch (_) { /* fall through to pairing */ }
  $("pair-screen").classList.remove("hidden");
  $("pair-code").focus();
}

function setMindName(name) {
  // Null until setup names it: the UI stays neutral rather than inventing an identity.
  MIND = name || "your mind";
  document.title = name || "Mind";
  $("mind-name").textContent = name || "unnamed mind";
}

function hideOperatorPanels() {
  document.querySelectorAll('[data-panel="settings"], [data-panel="devices"]').forEach((n) => n.remove());
  // E.WEB15: the instrument column reads operator-only endpoints — members get the work area alone.
  const inst = $("instruments"); if (inst) inst.remove();
  $("app").classList.add("no-instruments");
  document.querySelectorAll(".topbar .top-fact, .topbar .top-sep").forEach((n) => n.remove());
}

function showApp() {
  $("pair-screen").classList.add("hidden");
  $("app").classList.remove("hidden");
  applyMode(loadMode());
  restoreHistory();
  refreshWelcome();
  fillTopbar();
  loadInstruments();
  if (!window.__instTimer) window.__instTimer = setInterval(loadInstruments, 30000);
  input.focus();
}

/* ── E.WEB15: two looks, the user's choice — same data, same code, a token + layout switch ── */
const MODE_KEY = "ym-mode";
function loadMode() { try { return localStorage.getItem(MODE_KEY) === "companion" ? "companion" : "cockpit"; } catch (_) { return "cockpit"; } }
function applyMode(mode) {
  document.documentElement.dataset.mode = mode;
  const b = $("mode-btn"); if (b) b.textContent = mode === "companion" ? "Cockpit view" : "Companion view";
  try { localStorage.setItem(MODE_KEY, mode); } catch (_) {}
}
const modeBtn = $("mode-btn");
if (modeBtn) modeBtn.addEventListener("click", () => applyMode(loadMode() === "companion" ? "cockpit" : "companion"));

/* ── E.WEB15: the top strip — facts an operator glances at, read from the security audit ── */
async function fillTopbar() {
  try {
    const r = await fetch("/api/security", { headers: { "X-YM-Web": "1" } });
    if (!r.ok) return;
    const a = await r.json();
    const build = $("top-build"); if (build && a.build_commit) build.textContent = `build ${String(a.build_commit).slice(0, 7)}`;
    const dev = $("top-devices");
    if (dev && a.devices) dev.textContent = `${(a.devices.active_operators || 0) + (a.devices.active_members || 0)} devices`;
    const lane = $("top-lane");
    if (lane && a.lanes && a.lanes.private_model) lane.textContent = `private lane ${a.lanes.private_model}`;
  } catch (_) { /* the strip degrades to labels; nothing is invented */ }
}

/* ── E.WEB15: instruments — every number here is read from receipts or the recorder ── */
const shortHash = (h) => String(h || "").slice(0, 8);
const hhmmss = (ms) => ms ? new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" }) : "";
function evLine(dotCls, k, d, hsh) {
  const row = el("div", "ev");
  row.appendChild(el("span", "dot" + (dotCls ? " " + dotCls : "")));
  const kk = el("span", "k"); kk.textContent = k; row.appendChild(kk);
  const dd = el("span", "d"); dd.textContent = d; row.appendChild(dd);
  const hh = el("span", "hsh"); hh.textContent = hsh; row.appendChild(hh);
  return row;
}
async function loadInstruments() {
  if (!$("instruments")) return;
  const H = { headers: { "X-YM-Web": "1" } };
  try {
    const r = await fetch("/api/chains", H);
    const g = r.ok ? await r.json() : { available: false };
    const score = $("gate-score"), strata = $("gate-strata"), legend = $("gate-legend");
    strata.replaceChildren(); legend.replaceChildren();
    if (!g.available) { score.textContent = "unavailable"; legend.appendChild(textP("the verified chain could not be read")); }
    else if (!g.total) { score.textContent = "no calls yet"; }
    else {
      const pct = (100 * g.complete / g.total).toFixed(1);
      // E.AGI-A5: the headline is THIS binary's own honesty; the all-time figure sits beneath it,
      // so an older binary's stratigraphy can neither flatter nor hide the one that is running.
      const fresh = g.since_start && g.since_start.report;
      if (fresh) {
        score.textContent = fresh.total
          ? `${fresh.complete} / ${fresh.total} · ${(100 * fresh.complete / fresh.total).toFixed(1)}% since start`
          : "no calls since start";
        const sub = el("span"); sub.textContent = `all-time ${g.complete} / ${g.total} · ${pct}%`; legend.appendChild(sub);
      } else {
        score.textContent = `${g.complete} / ${g.total} · ${pct}%`;
      }
      const defects = g.defects || {};
      const incomplete = g.total - g.complete;
      const eraOld = Math.min(defects.actor || 0, incomplete);
      const eraMid = Math.max(0, incomplete - eraOld);
      const bars = Math.min(40, g.total);
      const counts = [Math.round(bars * eraOld / g.total), Math.round(bars * eraMid / g.total)];
      counts.push(Math.max(0, bars - counts[0] - counts[1]));
      counts.forEach((n, era) => { for (let i = 0; i < n; i++) strata.appendChild(el("i", "era-" + era)); });
      for (const [era, n, label] of [[0, eraOld, "pre-stamping"], [1, eraMid, "missing fields"], [2, g.complete, "complete"]]) {
        if (!n) continue;
        const sp = el("span"); sp.appendChild(el("i", "era-" + era)); sp.appendChild(document.createTextNode(`${n} ${label}`)); legend.appendChild(sp);
      }
    }
  } catch (_) { $("gate-score").textContent = "unavailable"; }
  try {
    const r = await fetch("/api/horizons", H);
    const data = r.ok ? await r.json() : { goals: [] };
    const goals = data.goals || [];
    // Prefer live work, then the NEWEST goal; and fall through past goals whose receipts predate
    // the receipt surface (their history is unreadable) so the card shows a real chain.
    const ordered = [
      ...goals.filter((g) => (g.queue_status || "") === "running"),
      ...goals.filter((g) => (g.queue_status || "") === "pending"),
      ...goals.slice().reverse(),
    ];
    const title = $("goal-title"), state = $("goal-state"), chain = $("goal-chain"), foot = $("goal-foot");
    chain.replaceChildren();
    let pick = null, h = null;
    for (const g of ordered) {
      const hr = await fetch(`/api/horizon-history?id=${encodeURIComponent(g.goal_id)}`, H);
      const body = hr.ok ? await hr.json().catch(() => null) : null;
      if (body && body.lifecycle && body.lifecycle.length) { pick = g; h = body; break; }
    }
    if (!pick) { title.textContent = "Goal"; state.textContent = goals.length ? "no readable receipts" : "none carried"; foot.textContent = ""; }
    else {
      title.textContent = "Goal · " + (pick.objective || pick.goal_id);
      state.textContent = pick.queue_status || pick.status || "?";
      if (h && h.lifecycle) {
        let prev = null;
        for (const ev of h.lifecycle) {
          const name = String(ev.event).toUpperCase();
          const delta = prev ? ` · +${((ev.occurred_at_ms - prev) / 1000).toFixed(1)}s` : ` · ${hhmmss(ev.occurred_at_ms)}`;
          const cls = name === "COMPLETED" ? "" : name === "FAILED" ? "bad" : name === "WAKE_STARTED" ? "mid" : "old";
          chain.appendChild(evLine(cls, name, `${ev.previous_queue_status || "no-queue"} → ${ev.next_queue_status || "terminal"}${delta}${ev.failure_reason ? " · " + ev.failure_reason : ""}`, shortHash(ev.receipt_sha256)));
          prev = ev.occurred_at_ms;
        }
        foot.textContent = h.outcome ? `chain verified · outcome ${h.outcome.status} · receipt ${shortHash(h.outcome.receipt_sha256)}` : "chain verified · no outcome yet";
      } else { foot.textContent = "receipts not readable"; }
    }
  } catch (_) { $("goal-state").textContent = "unavailable"; }
  try {
    const r = await fetch("/api/decisions?n=40", H);
    const rows = r.ok ? ((await r.json()).decisions || []) : [];
    const shadow = rows.find((d) => d.kind === "world_shadow");
    // E.G1c: two samples, never pooled — say which one this is.
    const gid = (shadow && shadow.goal_id) || "";
    const sample = gid.endsWith(":knock-receptivity") ? "paired · at a knock decision"
      : gid.endsWith(":headless-cadence") ? "unpaired · headless cadence"
      : "sample unlabelled";
    // The header names the sample too, so the card never reads as a knock decision on a box
    // that cannot make one.
    $("shadow-sample").textContent = !shadow ? "presence" : sample.split(" · ")[0];
    $("shadow-text").textContent = shadow
      ? `presence: ${shadow.outcome || "?"} · ${hhmmss(shadow.ts_ms)}\n${sample} · never consulted by any decision`
      : "no shadow verdict recorded yet";
    const lines = $("recorder-lines"); lines.replaceChildren();
    for (const d of rows.slice(0, 7)) {
      const v = d.verdict ? ` · ${d.verdict}` : "";
      const c = d.confidence != null ? ` · p ${Number(d.confidence).toFixed(2)}` : "";
      const cls = d.kind === "tool_observed" ? "" : d.kind === "tool_predicted" ? "mid" : "old";
      // observed: the verdict is the story; shadow: the verdict text; else: what was chosen
      const what = d.kind === "tool_observed" ? (d.verdict || "") : d.kind === "world_shadow" ? (d.outcome || "") : (d.chosen || "");
      lines.appendChild(evLine(cls, d.kind || "?", `${what}${d.kind === "tool_observed" ? "" : v}${c}`, hhmmss(d.ts_ms)));
    }
    if (!rows.length) lines.appendChild(textP("nothing recorded yet"));
  } catch (_) { $("shadow-text").textContent = "unavailable"; }
  // L1c (ARCH7): the loop ledger — what the mind did with its idle time, per loop, last 24 h.
  // Everything here is a count or a bounded reason from the verified log; nothing is narrated.
  try {
    const r = await fetch("/api/loops", H);
    const g = r.ok ? await r.json() : { available: false };
    const state = $("loops-state"), lines = $("loops-lines");
    lines.replaceChildren();
    if (!g.available) { state.textContent = "unavailable"; lines.appendChild(textP("the verified loop ledger could not be read")); }
    else if (!(g.loops || []).length) { state.textContent = "quiet"; lines.appendChild(textP(`no loop opportunities in the last 24 h${g.superseded ? ` · ${g.superseded} older-version rows` : ""}`)); }
    else {
      const loops = g.loops.slice().sort((a, b) => b.opportunities - a.opportunities);
      const acted = loops.reduce((n, l) => n + l.acted, 0);
      const opps = loops.reduce((n, l) => n + l.opportunities, 0);
      state.textContent = `${acted} / ${opps} acted · 24h`;
      for (const l of loops) {
        const held = Object.entries(l.held || {}).map(([k, v]) => `${k} ${v}`).join(", ");
        const outcomes = Object.entries(l.outcomes || {}).map(([k, v]) => `${k} ${v}`).join(", ");
        const what = `${l.acted}/${l.opportunities}${outcomes ? " · " + outcomes : ""}${held ? " · held " + held : ""}`;
        const cls = l.acted ? "" : "old";
        lines.appendChild(evLine(cls, l.loop_id, what, [...(l.hosts || [])].join("+")));
      }
      if (g.duplicates || g.malformed) {
        lines.appendChild(textP(`reduced ${g.duplicates || 0} duplicate opportunit${g.duplicates === 1 ? "y" : "ies"}${g.malformed ? ` · ${g.malformed} malformed row(s)` : ""}`));
      }
    }
  } catch (_) { $("loops-state").textContent = "unavailable"; }
}

function refreshWelcome() {
  const empty = history.length === 0;
  $("welcome").classList.toggle("hidden", !empty);
  if (!empty) return;
  const h = new Date().getHours();
  const part = h < 5 ? "night" : h < 12 ? "morning" : h < 17 ? "afternoon" : "evening";
  $("welcome-line").textContent = `Good ${part}. ${MIND} is listening.`;
  const sug = $("suggest");
  sug.replaceChildren();
  for (const s of ["What can you do?", "What do you remember about this household?", "What's on today?", "How do you keep things private?"]) {
    const b = document.createElement("button");
    b.textContent = s;
    b.addEventListener("click", () => { input.value = s; sendTurn(); });
    sug.appendChild(b);
  }
}

/* ── panel navigation ─────────────────────────────────────────────────── */
document.querySelectorAll(".nav-item[data-panel]").forEach((btn) => {
  btn.addEventListener("click", () => {
    document.querySelectorAll(".nav-item").forEach((n) => n.classList.remove("active"));
    document.querySelectorAll(".panel").forEach((p) => p.classList.remove("active"));
    btn.classList.add("active");
    $("panel-" + btn.dataset.panel).classList.add("active");
    if (btn.dataset.view) setAgentsView(btn.dataset.view);
    if (btn.dataset.panel === "capabilities") loadCapabilities();
    if (btn.dataset.panel === "settings") loadSettings();
    if (btn.dataset.panel === "devices") loadDevices();
    if (btn.dataset.panel === "tasks") loadTasks();
    if (btn.dataset.panel === "board") loadBoard();
    if (btn.dataset.panel === "files") loadFiles();
    if (btn.dataset.panel === "activity") loadActivity();
    if (btn.dataset.panel === "dreaming") loadDreaming();
    if (btn.dataset.panel === "chat") input.focus();
  });
});

/* ── pairing: step 1, the code ────────────────────────────────────────── */
$("pair-code").addEventListener("input", (e) => {
  let v = e.target.value.toUpperCase().replace(/[^0-9A-Z]/g, "").slice(0, 8);
  e.target.value = v.length > 4 ? v.slice(0, 4) + "-" + v.slice(4) : v;
});

$("pair-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const btn = $("pair-btn"), err = $("pair-error");
  btn.disabled = true; err.classList.add("hidden");
  try {
    const r = await fetch("/api/pair", {
      method: "POST", headers: HDRS,
      body: JSON.stringify({ code: $("pair-code").value.trim(), name: "browser" }),
    });
    if (r.ok) {
      $("pair-title").textContent = "Welcome.";
      $("pair-step-code").classList.add("hidden");
      $("pair-step-name").classList.remove("hidden");
      $("user-name").focus();
      return;
    }
    err.textContent = r.status === 429
      ? "Too many wrong codes — registration is locked out for a while."
      : "That code is wrong or expired.";
    err.classList.remove("hidden");
    $("pair-code").classList.add("shake");
    setTimeout(() => $("pair-code").classList.remove("shake"), 450);
  } catch (_) {
    err.textContent = "Could not reach the mind."; err.classList.remove("hidden");
  } finally { btn.disabled = false; }
});

/* ── pairing: step 2, the two names — both REQUIRED ───────────────────── */
// The mind's name is a setup write (one file, every surface reads it). The USER's name is not: it
// goes to the mind as the first conversational turn, so it is learned through memory and stays
// revisable by simply talking. Two names, two mechanisms, each the honest one for what it names.
$("name-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const mindName = $("mind-name-input").value.trim();
  const userName = $("user-name").value.trim();
  const err = $("setup-error");
  err.classList.add("hidden");
  if (!mindName || !userName) {
    err.textContent = "Both names are required.";
    err.classList.remove("hidden");
    return;
  }
  try {
    const r = await fetch("/api/setup", { method: "POST", headers: HDRS, body: JSON.stringify({ mind_name: mindName }) });
    if (!r.ok) throw new Error(String(r.status));
  } catch (_) {
    err.textContent = "Could not save the mind's name — try again.";
    err.classList.remove("hidden");
    return;
  }
  setMindName(mindName);
  $("pair-screen").classList.add("hidden");
  $("app").classList.remove("hidden");
  restoreHistory();
  refreshWelcome();
  const me = await fetch("/api/me", { headers: { "X-YM-Web": "1" } }).then((r) => r.json()).catch(() => ({}));
  if (me.person) $("person-chip").textContent = me.person;
  if (me.operator === false) hideOperatorPanels();
  // E.WEB6: wake the brain before promising one. On failure the ceremony stays HONEST — paired,
  // app open, but no pretend readiness and no intro turn fired into a void.
  $("mind-state").textContent = "waking the brain…";
  const check = await postJson("/api/brain-check", {});
  if (check.ok && check.data && check.data.ok) {
    $("mind-state").textContent = check.data.served ? "brain ready · " + check.data.served : "brain ready";
    input.value = `Hi, I'm ${userName} — that's what you should call me. And we've named you ${mindName}: that's your name now, please use it when you talk about yourself.`;
    sendTurn();
  } else {
    $("mind-state").textContent = "brain unreachable — paired; the mind will answer when it's back";
    input.value = `Hi, I'm ${userName} — that's what you should call me. And we've named you ${mindName}: that's your name now, please use it when you talk about yourself.`;
    input.focus();
  }
});

/* ── transcript persistence (this browser only — the mind's memory is its own) ── */
const STORE = "ym-transcript-v1";
let history = [];
function persist() { try { localStorage.setItem(STORE, JSON.stringify(history.slice(-200))); } catch (_) {} }
function restoreHistory() {
  try { history = JSON.parse(localStorage.getItem(STORE) || "[]"); } catch (_) { history = []; }
  [...feed.querySelectorAll(".msg")].forEach((n) => n.remove());
  for (const m of history) {
    if (m.role === "user") addUserMsg(m.text, m.ts, false);
    else { const b = addMindMsg(); renderMarkdown(b.md, m.text); b.stamp.textContent = fmtTs(m.ts); }
  }
  scrollToEnd(true);
}
$("clear-btn").addEventListener("click", () => {
  if (!confirm(`Clear this browser's transcript? (${MIND}'s own memory is unaffected.)`)) return;
  history = []; persist();
  [...feed.querySelectorAll(".msg")].forEach((n) => n.remove());
  refreshWelcome();
});
$("logout-btn").addEventListener("click", async () => {
  try { await fetch("/api/logout", { method: "POST", headers: HDRS }); } catch (_) {}
  location.reload();
});

/* ── panels ───────────────────────────────────────────────────────────── */
async function loadCapabilities() {
  const host = $("cap-cards");
  try {
    const [r, pr] = await Promise.all([
      fetch("/api/capabilities", { headers: { "X-YM-Web": "1" } }),
      fetch("/api/plugins", { headers: { "X-YM-Web": "1" } }),
    ]);
    const rep = await r.json();
    const plugins = pr.ok ? ((await pr.json()).plugins || []) : [];
    const pluginById = new Map(plugins.map((p) => [p.id, p]));
    host.replaceChildren();
    if (plugins.length) {
      const g = el("div", "cap-group"); g.textContent = "manage · enable/disable"; host.appendChild(g);
      for (const p of plugins) {
        const card = el("div", "card");
        card.appendChild(el("span", "dot " + (p.enabled ? "ready" : "disabled")));
        const main = el("div", "card-main");
        const t = el("div", "card-title"); t.textContent = p.title || p.id; main.appendChild(t);
        const d = el("div", "card-desc"); d.textContent = `${p.security} · ${p.provenance} · ${p.availability}`; main.appendChild(d);
        card.appendChild(main);
        const side = el("div", "card-side");
        const btn = el("button", "revoke-btn");
        btn.textContent = p.enabled ? "Disable" : "Enable";
        btn.style.color = p.enabled ? "var(--warn)" : "var(--ok)";
        btn.addEventListener("click", async () => {
          const res = await postJson("/api/plugin-toggle", { id: p.id, enable: !p.enabled });
          if (!res.ok) alert(`toggle failed (${res.status || "offline"}): ${res.text}`);
          else loadCapabilities();
        });
        side.appendChild(btn);
        card.appendChild(side);
        host.appendChild(card);
      }
    }
    $("cap-counts").textContent = `${rep.connected} ready · ${rep.unavailable} missing something · ${rep.disabled} off`;
    const byCat = new Map();
    for (const c of rep.capabilities || []) {
      if (!byCat.has(c.category)) byCat.set(c.category, []);
      byCat.get(c.category).push(c);
    }
    for (const [cat, caps] of [...byCat.entries()].sort()) {
      const g = el("div", "cap-group"); g.textContent = cat; host.appendChild(g);
      for (const c of caps) {
        const card = el("div", "card");
        const avail = String(c.availability?.kind ?? c.availability ?? "").toLowerCase() || "ready";
        card.appendChild(el("span", "dot " + (avail.includes("ready") ? "ready" : avail.includes("disab") ? "disabled" : "unavailable")));
        const main = el("div", "card-main");
        const t = el("div", "card-title"); t.textContent = c.title || c.id; main.appendChild(t);
        const d = el("div", "card-desc");
        d.textContent = c.missing ? `missing: ${c.missing}` : (c.tools || []).join(" · ");
        main.appendChild(d);
        card.appendChild(main);
        const side = el("div", "card-side");
        const sec = el("span", "tag" + (c.security === "gated_write" ? " write" : ""));
        sec.textContent = c.security; side.appendChild(sec);
        if (c.provenance && c.provenance !== "builtin") {
          const p = el("span", "tag self"); p.textContent = c.provenance; side.appendChild(p);
        }
        card.appendChild(side);
        host.appendChild(card);
      }
    }
    if (!byCat.size) host.appendChild(textP("No capabilities reported."));
    // E.WEB14: BOUNDARIES — the typed self-claims registry, exactly what the mind would say
    // about its own powers when asked (one renderer principle: same constants, same version).
    try {
      const cr = await fetch("/api/claims", { headers: { "X-YM-Web": "1" } });
      if (cr.ok) {
        const reg = await cr.json();
        const g = el("div", "cap-group"); g.textContent = `boundaries · ${reg.version || "registry"}`; host.appendChild(g);
        for (const c of reg.claims || []) {
          const card = el("div", "card setting-row");
          const main = el("div", "card-main");
          const t = el("div", "card-title"); t.textContent = c.id; main.appendChild(t);
          const d = el("div", "card-desc"); d.textContent = c.answer; main.appendChild(d);
          const k = el("div", "setting-key"); k.textContent = `enforced by ${c.authority} · evidence: ${(c.evidence || []).join(", ")}`; main.appendChild(k);
          card.appendChild(main);
          host.appendChild(card);
        }
      }
    } catch (_) { /* boundaries are additive; the capability report stands without them */ }
  } catch (_) { host.replaceChildren(textP("Could not read the capability report.")); }
}

async function loadSettings() {
  const host = $("settings-cards");
  loadSecurity();
  try {
    const r = await fetch("/api/settings", { headers: { "X-YM-Web": "1" } });
    if (r.status === 403) { host.replaceChildren(textP("Operator only.")); return; }
    const data = await r.json();
    host.replaceChildren();
    const byGroup = new Map();
    for (const s of data.settings || []) {
      if (!byGroup.has(s.group)) byGroup.set(s.group, []);
      byGroup.get(s.group).push(s);
    }
    for (const [group, items] of byGroup.entries()) {
      const g = el("div", "cap-group"); g.textContent = group; host.appendChild(g);
      for (const s of items) {
        const card = el("div", "card setting-row");
        const main = el("div", "card-main");
        const t = el("div", "card-title"); t.textContent = s.label; main.appendChild(t);
        const d = el("div", "card-desc"); d.textContent = s.desc; main.appendChild(d);
        const k = el("div", "setting-key"); k.textContent = s.key; main.appendChild(k);
        card.appendChild(main);
        const side = el("div", "card-side");
        const v = el("span", "setting-val" + (s.set ? "" : " unset"));
        v.textContent = s.kind === "secret" ? (s.set ? "●●●●●●" : "—") : (s.set ? s.value : "—");
        v.title = v.textContent;
        side.appendChild(v);
        if (s.restart) { const rb = el("span", "tag restart"); rb.textContent = "restart"; side.appendChild(rb); }
        card.appendChild(side);
        host.appendChild(card);
      }
    }
    // E.WEB13: operator restart — confirmation-locked. The server exits cleanly and the
    // supervisor brings the mind back; we poll /api/me until it answers again.
    const rWrap = el("div", "card setting-row");
    const rMain = el("div", "card-main");
    const rt = el("div", "card-title"); rt.textContent = "Restart the mind"; rMain.appendChild(rt);
    const rd = el("div", "card-desc"); rd.textContent = "Exits cleanly and lets the supervisor bring it back — a few seconds of downtime. Applies settings tagged 'restart'."; rMain.appendChild(rd);
    const rRow = el("div", "job-actions");
    const rBtn = el("button"); rBtn.textContent = "Restart…";
    const rOut = el("div", "setting-key");
    rBtn.addEventListener("click", async () => {
      if (!window.confirm("Restart the mind now? It will be back in a few seconds.")) return;
      rBtn.disabled = true;
      const res = await postJson("/api/restart", { confirm: "restart" });
      if (!res.ok) { rOut.textContent = `restart refused (${res.status || "offline"}): ${res.text}`; rBtn.disabled = false; return; }
      rOut.textContent = "restarting…";
      const started = Date.now();
      const poll = setInterval(async () => {
        try {
          const ok = (await fetch("/api/me", { headers: { "X-YM-Web": "1" } })).ok;
          if (ok) { clearInterval(poll); rOut.textContent = "back online."; rBtn.disabled = false; return; }
        } catch (_) {}
        if (Date.now() - started > 120000) { clearInterval(poll); rOut.textContent = "still down after 2 minutes — check the box."; rBtn.disabled = false; }
      }, 2000);
    });
    rRow.appendChild(rBtn); rMain.append(rRow, rOut);
    rWrap.appendChild(rMain);
    host.appendChild(rWrap);
  } catch (_) { host.replaceChildren(textP("Could not read configuration.")); }
}

async function loadSecurity() {
  // E.SEC18: the posture card — rendered from the audit JSON, counts and booleans only.
  let card = $("security-card");
  if (!card) {
    card = el("div", "card setting-row"); card.id = "security-card";
    $("settings-cards").parentElement.insertBefore(card, $("settings-cards"));
  }
  try {
    const r = await fetch("/api/security", { headers: { "X-YM-Web": "1" } });
    if (!r.ok) { card.classList.add("hidden"); return; }
    const a = await r.json();
    card.classList.remove("hidden");
    card.replaceChildren();
    const main = el("div", "card-main");
    const t = el("div", "card-title"); t.textContent = "Security posture"; main.appendChild(t);
    const lanes = a.privacy_lanes || {};
    const esc = lanes.private_grounded_escalated_to_cloud ?? "?";
    const d = el("div", "card-desc");
    d.textContent = `build ${String(a.build_commit).slice(0, 8)} · devices: ${a.devices.active_operators} operator / ${a.devices.active_members} member (${a.devices.revoked} revoked) · invites live: ${a.registration.live_member_invites} · boot code out: ${a.registration.boot_code_outstanding ? "YES" : "no"} · private→cloud escalations: ${esc}`;
    main.appendChild(d);
    const list = el("div", "budget-chips");
    for (const l of a.listeners || []) {
      const chip = el("span", "tag");
      chip.textContent = `${l.listener.replace("YM_", "").replace("_PORT", "").toLowerCase()}:${l.port} ${l.bind.split(" ")[0]}`;
      chip.title = l.bind;
      list.appendChild(chip);
    }
    main.appendChild(list);
    card.appendChild(main);
  } catch (_) { card.classList.add("hidden"); }
}

async function loadDevices() {
  const host = $("device-cards");
  try {
    const r = await fetch("/api/devices", { headers: { "X-YM-Web": "1" } });
    if (r.status === 403) { host.replaceChildren(textP("Operator only.")); return; }
    const data = await r.json();
    host.replaceChildren();
    for (const d of data.devices || []) {
      const card = el("div", "card" + (d.revoked ? " revoked" : ""));
      card.appendChild(el("span", "dot " + (d.revoked ? "disabled" : "ready")));
      const main = el("div", "card-main");
      const t = el("div", "card-title"); t.textContent = d.name; main.appendChild(t);
      const meta = el("div", "dev-meta");
      meta.textContent = `${d.role} · paired ${new Date(d.created_ms).toLocaleDateString()}${d.revoked ? " · revoked" : ""}`;
      main.appendChild(meta);
      card.appendChild(main);
      const side = el("div", "card-side");
      if (d.this_device) { const y = el("span", "tag you"); y.textContent = "this device"; side.appendChild(y); }
      else if (!d.revoked) {
        const b = el("button", "revoke-btn"); b.textContent = "Revoke";
        b.addEventListener("click", async () => {
          if (!confirm(`Revoke '${d.name}'? Its access ends immediately.`)) return;
          const res = await postJson("/api/revoke", { id: d.id });
          if (!res.ok) alert(`revoke failed (${res.status || "offline"}): ${res.text}`);
          else loadDevices();
        });
        side.appendChild(b);
      }
      card.appendChild(side);
      host.appendChild(card);
    }
    if (!(data.devices || []).length) host.appendChild(textP("No devices."));
    // E.WEB5: member invites — operator mints a single-use, 15-minute code bound to a person;
    // the member enters it on the SAME pairing screen.
    const inviteWrap = el("div", "card setting-row");
    const imain = el("div", "card-main");
    const it = el("div", "card-title"); it.textContent = "Invite a member"; imain.appendChild(it);
    const idesc = el("div", "card-desc"); idesc.textContent = "Mints a single-use code (15 min) that pairs a browser as this person — member scope, never operator."; imain.appendChild(idesc);
    const irow = el("div", "job-actions");
    const iinp = document.createElement("input");
    iinp.className = "agent-input"; iinp.placeholder = "person id (e.g. brishti)"; iinp.maxLength = 24;
    const ibtn = el("button"); ibtn.textContent = "Mint invite";
    const iout = el("div", "setting-key");
    ibtn.addEventListener("click", async () => {
      const person = iinp.value.trim();
      if (!person) return;
      const res = await postJson("/api/invite", { person });
      iout.textContent = res.ok
        ? `code for ${res.data.person}: ${res.data.code}  (single use, ${res.data.ttl_minutes} min — share it now)`
        : `invite failed (${res.status || "offline"}): ${res.text}`;
    });
    irow.append(iinp, ibtn); imain.append(irow, iout);
    inviteWrap.appendChild(imain);
    host.appendChild(inviteWrap);
  } catch (_) { host.replaceChildren(textP("Could not read the device store.")); }
}

function textP(s) { const p = el("p", "loading"); p.textContent = s; return p; }

/* ── agents & standing orders ─────────────────────────────────────────── */
let tasksTimer = null;
// E.WEB9: pure column classifier — the ONE routing rule, extractable and deterministic. Every
// work item lands in exactly one of four columns from its own status; no mutation, no fetch.
function columnFor(kind, status) {
  const s = String(status || "").toLowerCase();
  if (s.includes("fail")) return "needs";
  if (kind === "order") return "scheduled";
  if (s.includes("run") || s.includes("active")) return "running";
  if (s.includes("pending") || s.includes("scheduled") || s.includes("sleep")) return "scheduled";
  if (s.includes("done") || s.includes("complete") || s.includes("finish")) return "done";
  return "running";
}

async function loadFiles() {
  const host = $("files-cards");
  try {
    const r = await fetch("/api/files", { headers: { "X-YM-Web": "1" } });
    if (r.status === 401) { host.replaceChildren(textP("Not paired.")); return; }
    const data = await r.json();
    host.replaceChildren();
    const files = data.files || [];
    if (!files.length) { host.appendChild(textP("No published pages yet — the mind creates these when it publishes a dashboard.")); return; }
    const origin = `${location.protocol}//${location.hostname}:${data.web_port || 8088}/`;
    for (const f of files) {
      const card = el("div", "card setting-row");
      const main = el("div", "card-main");
      const a = el("a"); a.href = origin + encodeURIComponent(f.name); a.target = "_blank"; a.rel = "noopener noreferrer";
      a.textContent = f.name; a.className = "card-title";
      main.appendChild(a);
      const meta = el("div", "setting-key");
      const kb = f.size > 1024 ? (f.size / 1024).toFixed(1) + " KB" : f.size + " B";
      meta.textContent = `${kb}${f.modified_ms ? " · " + new Date(f.modified_ms).toLocaleString() : ""}`;
      main.appendChild(meta);
      card.appendChild(main);
      host.appendChild(card);
    }
  } catch (_) { host.replaceChildren(textP("Could not read published pages.")); }
}

async function loadBoard() {
  const host = $("board-columns");
  const cols = { scheduled: [], running: [], done: [], needs: [] };
  async function pull(url) { try { const r = await fetch(url, { headers: { "X-YM-Web": "1" } }); return r.ok ? await r.json() : {}; } catch (_) { return {}; } }
  const [tasks, horizons, orders] = await Promise.all([pull("/api/tasks"), pull("/api/horizons"), fetchStandingOrders()]);
  // E.WEB18b: a run's card opens its agent's thread — the same key rule the lists use.
  for (const j of (tasks.jobs || [])) cols[columnFor("job", j.state || j.status)].push({ title: j.name || j.id, meta: (j.task || j.goal || "").slice(0, 80), cls: columnFor("job", j.state || j.status), agent: j.name || j.id });
  for (const g of (horizons.goals || [])) { const st = g.budget_expired ? "failed" : (g.queue_status || g.status); const col = columnFor("horizon", st); cols[col].push({ title: g.objective || g.goal_id, meta: g.budget_expired ? "budget expired" : (g.queue_status || g.status || ""), cls: col }); }
  // Standing orders, typed: one card each with the server's countdown, opening the agent's thread.
  for (const o of orders) {
    const secs = Number(o.in_seconds);
    const when = !Number.isFinite(secs) ? "" : secs <= 0 ? "due now" : secs < 3600 ? `in ${Math.round(secs / 60)}m` : secs < 172800 ? `in ${Math.round(secs / 3600)}h` : `in ${Math.round(secs / 86400)}d`;
    cols.scheduled.push({ title: o.name || o.id, meta: o.state === "paused" ? `paused · would fire ${when}` : `fires ${when}`, cls: "scheduled", agent: orderThreadKey(o) || `order:${o.id}` });
  }
  host.replaceChildren();
  for (const [key, label] of [["scheduled", "Scheduled"], ["running", "Running"], ["done", "Done"], ["needs", "Needs review"]]) {
    const col = el("div", "board-col");
    const h = el("h3"); h.textContent = `${label} · ${cols[key].length}`; col.appendChild(h);
    for (const c of cols[key]) {
      const card = el("div", "board-card " + (key === "needs" ? "needs" : key === "running" ? "running" : "") + (c.agent ? " opens-thread" : ""));
      const t = el("div", "bc-title"); t.textContent = c.title; card.appendChild(t);
      if (c.meta) { const m = el("div", "bc-meta"); m.textContent = c.meta; card.appendChild(m); }
      if (c.agent) {
        // A card that acts like a button must be one for the keyboard too: one activate closure
        // for click and for Enter / Space (Codex's review), and a focus-visible ring in CSS.
        card.setAttribute("role", "button"); card.tabIndex = 0;
        const activate = (ev) => { ev.preventDefault(); openThreadFromAnywhere(c.agent); };
        card.addEventListener("click", activate);
        card.addEventListener("keydown", (ev) => { if (ev.key === "Enter" || ev.key === " ") activate(ev); });
      }
      col.appendChild(card);
    }
    if (!cols[key].length) { const e = el("div", "bc-meta"); e.textContent = "—"; col.appendChild(e); }
    host.appendChild(col);
  }
}

async function loadDreaming() {
  const host = $("dreaming-cards");
  try {
    const r = await fetch("/api/dreaming?n=80", { headers: { "X-YM-Web": "1" } });
    if (r.status === 403) { host.replaceChildren(textP("Operator only.")); return; }
    const data = await r.json();
    host.replaceChildren();
    const rows = (data.dreaming || []).slice().reverse();
    if (!rows.length) { host.appendChild(textP("No offline-cognition activity recorded yet — the mind dreams when idle.")); return; }
    const phaseColor = { rehearse: "running", reconcile: "ready", associate: "" };
    for (const e of rows) {
      const card = el("div", "card setting-row");
      const main = el("div", "card-main");
      const t = el("div", "card-title"); t.textContent = e.message; main.appendChild(t);
      const meta = el("div", "setting-key");
      meta.textContent = `${e.phase} · tick ${e.tick_no}${e.at_ms ? " · " + new Date(e.at_ms).toLocaleString() : ""}`;
      main.appendChild(meta);
      card.appendChild(main);
      const side = el("div", "card-side");
      const chip = el("span", "job-state " + (phaseColor[e.phase] || ""));
      chip.textContent = e.phase; side.appendChild(chip);
      card.appendChild(side);
      host.appendChild(card);
    }
  } catch (_) { host.replaceChildren(textP("Could not read the dreaming log.")); }
}

/* E.WEB15: the recorded event, said plainly. The internal name stays beneath it. */
function phraseFor(d) {
  const k = d.kind || "";
  const tool = d.chosen ? ` (${d.chosen})` : "";
  switch (k) {
    case "tool_predicted": return `Predicted a tool would work${tool}${d.confidence != null ? ` — ${Math.round(d.confidence * 100)}% sure` : ""}`;
    case "tool_observed": return d.verdict === "ok" ? "The tool worked" : d.verdict === "empty" ? "The tool ran but found nothing" : d.verdict === "denied" ? "A tool call was refused by the walls" : d.verdict === "malformed" ? "A malformed tool call was refused before it ran" : `The tool ${d.verdict || "finished"}`;
    case "grounding_assembled": return "Gathered context for a reply";
    case "pack_route_shadow": return "The pack router weighed in (shadow only)";
    case "world_shadow": return "The world model gave its opinion (shadow only)";
    case "operator_restart": return "An operator restarted the mind";
    case "packet_created": return "Prepared something to bring up";
    case "packet_resolved": return d.verdict === "confirmed" ? "You approved prepared work" : "You declined prepared work";
    case "packet_expired": return "Prepared work expired unused";
    case "prediction_made": return "Made a forecast";
    case "prediction_graded": return "Graded a forecast";
    case "goal_compiled": return "Compiled a bounded plan";
    default: return k.replace(/_/g, " ") || "Recorded an event";
  }
}

async function loadActivity() {
  // Standing orders (reuse the orders verb) + a redacted decisions timeline.
  try {
    const r = await fetch("/api/orders", { headers: { "X-YM-Web": "1" } });
    $("activity-orders").textContent = r.ok ? ((await r.json()).text || "(none)") : "(operator only)";
  } catch (_) { $("activity-orders").textContent = "(unavailable)"; }
  const host = $("activity-decisions");
  try {
    const r = await fetch("/api/decisions?n=60", { headers: { "X-YM-Web": "1" } });
    if (r.status === 403) { host.replaceChildren(textP("Operator only.")); return; }
    const data = await r.json();
    host.replaceChildren();
    const rows = data.decisions || [];
    if (!rows.length) { host.appendChild(textP("No decisions recorded yet (the flight recorder may be disabled).")); return; }
    for (const d of rows) {
      const card = el("div", "card setting-row");
      const main = el("div", "card-main");
      const t = el("div", "card-title");
      t.textContent = phraseFor(d);
      main.appendChild(t);
      const rec = el("div", "dec-kind");
      rec.textContent = (d.kind || "?") + (d.verdict ? " · " + d.verdict : "") + (d.chosen ? " → " + d.chosen : "");
      main.appendChild(rec);
      if (d.goal) { const g = el("div", "card-desc"); g.textContent = d.goal; main.appendChild(g); }
      const meta = el("div", "setting-key");
      const when = d.ts_ms ? new Date(d.ts_ms).toLocaleString() : "";
      meta.textContent = `${d.actor || ""} ${when}${d.confidence != null ? " · conf " + Number(d.confidence).toFixed(2) : ""}`.trim();
      main.appendChild(meta);
      card.appendChild(main);
      host.appendChild(card);
    }
  } catch (_) { host.replaceChildren(textP("Could not read the decision log.")); }
}

/* E.WEB17: agents as a sub-menu. ONE classifier decides the bucket of every item the three
   views share — a job, a durable goal, a standing order — so nothing can appear in both views or
   in neither. States are the server's own words: jobs "running|done|failed"; goals by
   queue_status "RUNNING|PENDING|SCHEDULED|COMPLETED|FAILED" (+ budget_expired); orders
   "sleeping|paused". */
function agentBucket(item) {
  const s = String(item.queue_status || item.state || item.status || "").toLowerCase();
  if (s.includes("run")) return "running";
  return "dormant";
}
const AGENT_VIEWS = {
  running: ["Running", "Agents and durable goals working right now."],
  dormant: ["Dormant", "Waiting to run: standing orders on their schedule, goals not yet due, and past runs."],
  new: ["New agent", "Delegate a task and it runs in the background — name it after a banked skill and it runs that skill. Durable goals survive restarts and act on schedule."],
};
function setAgentsView(view) {
  // "thread" is one agent's own view (E.WEB18); its title is the agent's name, set by renderThread.
  const v = view === "thread" || AGENT_VIEWS[view] ? view : "running";
  $("panel-tasks").dataset.view = v;
  if (v !== "thread") {
    $("tasks-title").textContent = AGENT_VIEWS[v][0];
    $("tasks-sub").textContent = AGENT_VIEWS[v][1];
  }
  document.querySelectorAll(".nav-sub[data-view]").forEach((n) => n.classList.toggle("active", n.dataset.view === (v === "thread" ? threadReturnView : v)));
}
const bucketCounts = { running: 0, dormant: 0 };
function bucketReset() { bucketCounts.running = 0; bucketCounts.dormant = 0; }
function bucketAdd(card, item) {
  const b = agentBucket(item);
  card.classList.add("bucket-" + b);
  bucketCounts[b] += 1;
  return b;
}
function bucketPaint() {
  for (const b of ["running", "dormant"]) {
    const n = $("count-" + b);
    if (!n) continue;
    n.textContent = String(bucketCounts[b]);
    n.hidden = bucketCounts[b] === 0;
  }
}

/* E.WEB18: each agent is its own THREAD. Runs and standing orders group by the agent name the
   composer already uses; the lists show agents, and a thread shows one agent's history like a
   conversation — brief, then every run in time order, then what is scheduled, then the reply box
   (run again). One pure function does the grouping so a run can never sit in two threads. */
// E.WEB19: identity is TYPED, never read off a display name. A run's key is its agent name; an
// order joins a thread only through the server's persisted `agent_name` (an imported agent's
// exact name). A generic scheduled goal carries no agent_name and stands alone — even when its
// display name happens to equal an agent's. The "standing: " display prefix stays as it is.
function agentKey(name) { return String(name || "(unnamed)"); }
function orderThreadKey(o) { return o && o.agent_name ? String(o.agent_name) : null; }
function agentThreads(jobs, orders) {
  const byName = new Map();
  const get = (name) => {
    const key = agentKey(name);
    if (!byName.has(key)) byName.set(key, { name: key, runs: [], orders: [], running: false, last_ms: 0, task: "" });
    return byName.get(key);
  };
  for (const j of jobs || []) {
    const a = get(j.name || j.id);
    a.runs.push(j);
    if (agentBucket(j) === "running") a.running = true;
    const t = Number(j.finished_ms || j.started_ms || 0);
    if (t > a.last_ms) a.last_ms = t;
    if (!a.task && (j.task || j.goal)) a.task = j.task || j.goal;
  }
  for (const o of orders || []) {
    // Typed join: only an order with a persisted agent_name belongs to an agent's thread; a
    // generic scheduled goal becomes its own standalone row, keyed by its id so it cannot
    // collide with an agent that shares its display name.
    const key = orderThreadKey(o);
    const a = key ? get(key) : get(`order:${o.id}`);
    if (!key) a.display = o.name || o.id;
    a.orders.push(o);
  }
  for (const a of byName.values()) a.runs.sort((x, y) => Number(x.started_ms || 0) - Number(y.started_ms || 0));
  // Running agents first, then most recent activity.
  return [...byName.values()].sort((x, y) => (y.running - x.running) || (y.last_ms - x.last_ms));
}
// The classifier's view of an agent: running if any run is, else dormant (a standing order is
// waiting by definition).
function agentState(a) { return { state: a.running ? "running" : "dormant" }; }

let agentsCache = { jobs: [], orders: [] };
let openThreadName = null;
let threadReturnView = "running";

async function loadTasks() {
  const host = $("job-cards");
  loadTemplates();
  loadAllowance();
  bucketReset();
  let anyRunning = false;
  try {
    const r = await fetch("/api/tasks", { headers: { "X-YM-Web": "1" } });
    if (r.status === 403) { host.replaceChildren(textP("Operator only.")); return; }
    const data = await r.json();
    agentsCache.jobs = data.jobs || [];
  } catch (_) { host.replaceChildren(textP("Could not read the board.")); agentsCache.jobs = []; }
  agentsCache.orders = await fetchStandingOrders();
  const agents = agentThreads(agentsCache.jobs, agentsCache.orders);
  host.replaceChildren();
  for (const a of agents) {
    if (a.running) anyRunning = true;
    const row = el("button", "agent-row");
    row.type = "button";
    bucketAdd(row, agentState(a));
    const dot = el("span", "agent-dot " + (a.running ? "running" : "dormant")); row.appendChild(dot);
    const main = el("div", "agent-row-main");
    const name = el("div", "agent-row-name"); name.textContent = a.display || a.name; main.appendChild(name);
    const meta = el("div", "agent-row-meta");
    const runs = a.runs.length ? `${a.runs.length} run${a.runs.length === 1 ? "" : "s"}` : "no runs yet";
    const last = a.last_ms ? ` · last ${fmtTs(a.last_ms)}` : "";
    // The next fire time: the soonest SLEEPING order (a paused one would not fire), from the
    // server's own countdown; count and paused state ride alongside.
    const live = a.orders.filter((o) => o.state !== "paused" && Number.isFinite(Number(o.in_seconds)));
    const soonest = live.length ? Math.min(...live.map((o) => Number(o.in_seconds))) : null;
    const nextIn = soonest === null ? "" : soonest <= 0 ? "due now" : soonest < 3600 ? `next in ${Math.round(soonest / 60)}m` : soonest < 172800 ? `next in ${Math.round(soonest / 3600)}h` : `next in ${Math.round(soonest / 86400)}d`;
    const sched = a.orders.length ? ` · ${a.orders.length === 1 ? "scheduled" : a.orders.length + " schedules"}${nextIn ? " · " + nextIn : ""}${a.orders.some((o) => o.state === "paused") ? " (paused)" : ""}` : "";
    meta.textContent = `${a.running ? "working now" : "dormant"} · ${runs}${last}${sched}`;
    main.appendChild(meta);
    row.appendChild(main);
    const lastRun = a.runs[a.runs.length - 1];
    if (lastRun) {
      const st = el("span", "job-state " + (a.running ? "running" : String(lastRun.state || lastRun.status || "").includes("fail") ? "failed" : "done"));
      st.textContent = a.running ? "running" : String(lastRun.state || lastRun.status || "done");
      row.appendChild(st);
    }
    row.addEventListener("click", () => openThread(a.name));
    host.appendChild(row);
  }
  if (!agents.length) host.appendChild(textP("No agents yet — compose one under New agent."));
  else {
    if (!anyRunning) { const p = textP("Nothing running right now."); p.classList.add("bucket-running"); host.appendChild(p); }
    if (!agents.some((a) => !a.running)) { const p = textP("Every agent is working."); p.classList.add("bucket-dormant"); host.appendChild(p); }
  }
  // A running agent keeps the board live; a quiet board stops polling.
  clearTimeout(tasksTimer);
  if (anyRunning && $("panel-tasks").classList.contains("active")) tasksTimer = setTimeout(loadTasks, 5000);
  await loadHorizons();
  bucketPaint();
  if (openThreadName && $("panel-tasks").dataset.view === "thread") renderThread(openThreadName);
}

function openThread(rawName) {
  const name = agentKey(rawName);
  openThreadName = name;
  const v = $("panel-tasks").dataset.view;
  if (v !== "thread") threadReturnView = v || "running";
  setAgentsView("thread");
  renderThread(name);
}
function closeThread() {
  openThreadName = null;
  setAgentsView(threadReturnView);
}
// E.WEB18b: from any panel (the Board), land in the agent's thread with fresh data.
async function openThreadFromAnywhere(name) {
  document.querySelectorAll(".nav-item").forEach((n) => n.classList.remove("active"));
  document.querySelectorAll(".panel").forEach((p) => p.classList.remove("active"));
  $("panel-tasks").classList.add("active");
  setAgentsView("running");
  await loadTasks();
  openThread(name);
}

function renderThread(name) {
  const agent = agentThreads(agentsCache.jobs, agentsCache.orders).find((a) => a.name === name);
  const host = $("agent-thread");
  host.replaceChildren();
  $("tasks-title").textContent = agent && agent.display ? agent.display : name;
  $("tasks-sub").textContent = agent ? (agent.running ? "Working now." : agent.orders.length ? "Dormant · scheduled." : "Dormant.") : "No such agent.";
  const back = el("button", "link-btn thread-back"); back.type = "button"; back.textContent = "← all agents";
  back.addEventListener("click", closeThread);
  host.appendChild(back);
  if (!agent) return;
  // The brief opens the thread, once, clamped — never dumped.
  if (agent.task) {
    const wrap = el("div", "thread-brief");
    const lab = el("div", "cap-group"); lab.textContent = "Brief"; wrap.appendChild(lab);
    const brief = el("div", "card-desc"); brief.textContent = agent.task; wrap.appendChild(brief);
    if (brief.textContent.length > 280) {
      brief.classList.add("clamp");
      const more = el("button", "link-btn"); more.type = "button"; more.textContent = "show full brief";
      more.addEventListener("click", () => { const c = brief.classList.toggle("clamp"); more.textContent = c ? "show full brief" : "hide"; });
      wrap.appendChild(more);
    }
    host.appendChild(wrap);
  }
  for (const j of agent.runs) host.appendChild(runEntry(j));
  for (const o of agent.orders) host.appendChild(orderEntry(o));
  if (!agent.runs.length && !agent.orders.length) host.appendChild(textP("Nothing in this thread yet."));
  // The reply box: run it again with the same brief, through the same gate as a new delegation.
  if (agent.task && !agent.running) {
    const reply = el("div", "thread-reply");
    const again = el("button"); again.type = "button"; again.textContent = "Run again";
    again.addEventListener("click", async () => {
      again.disabled = true;
      const res = await postJson("/api/agent", { name: agent.name, task: agent.task });
      if (!res.ok) { alert(`re-run failed (${res.status || "offline"}): ${res.text}`); again.disabled = false; }
      else loadTasks();
    });
    reply.appendChild(again);
    host.appendChild(reply);
  }
}

// One run, as an entry in its agent's thread: head, the timeline from the job's own
// timestamps, the result through DOM-only markdown, and the lifecycle actions.
function runEntry(j) {
  const state = String(j.state || j.status || "?").toLowerCase();
  const running = state.includes("run");
  const card = el("div", "run " + (running ? "running" : state.includes("fail") ? "failed" : "done"));
  const head = el("div", "run-head");
  const st = el("span", "job-state " + (running ? "running" : state.includes("fail") ? "failed" : "done"));
  st.textContent = state; head.appendChild(st);
  const kind = el("span", "tag"); kind.textContent = j.kind || "agent"; head.appendChild(kind);
  const elapsed = el("span", "run-elapsed");
  const notes = Array.isArray(j.notes) ? j.notes : [];
  const lastT = notes.length ? notes[notes.length - 1].t : null;
  const endMs = running ? Date.now() : (j.finished_ms || lastT || j.started_ms);
  elapsed.textContent = j.started_ms ? `${fmtTs(j.started_ms)} · ${fmtElapsed(endMs - j.started_ms)}${running ? " and counting" : ""}` : "";
  head.appendChild(elapsed);
  card.appendChild(head);
  const tl = el("div", "timeline");
  const addStep = (t, text, terminal) => {
    const row = el("div", "tl" + (terminal ? " terminal" : ""));
    const tt = el("span", "tl-t"); tt.textContent = t ? fmtTs(t) : ""; row.appendChild(tt);
    const tx = el("span", "tl-text"); tx.textContent = text; row.appendChild(tx);
    tl.appendChild(row);
  };
  if (j.started_ms) addStep(j.started_ms, "started");
  for (const n of notes) addStep(n.t, String(n.note || ""));
  if (!running) addStep(j.finished_ms || lastT || null, state.includes("fail") ? "failed" : "finished", true);
  else addStep(null, "working…", true);
  card.appendChild(tl);
  if (j.result) {
    const md = el("div", "md");
    renderMarkdown(md, String(j.result));
    card.appendChild(md);
  }
  const meta = el("div", "setting-key"); meta.textContent = j.id; card.appendChild(meta);
  const actions = el("div", "job-actions");
  for (const [verb, label, ask] of [["keep", "Keep", null], ["drop", "Drop scratch", null], ["delete", "Delete", "Delete this run's record from the board?"]]) {
    const b = el("button"); b.type = "button"; b.textContent = label;
    b.addEventListener("click", async () => {
      if (ask && !confirm(ask)) return;
      const res = await postJson("/api/task-action", { verb, id: j.id });
      if (!res.ok) alert(`${verb} failed (${res.status || "offline"}): ${res.text}`);
      else loadTasks();
    });
    actions.appendChild(b);
  }
  card.appendChild(actions);
  return card;
}

// A standing order, as a future entry in its agent's thread, with the actions the server says
// apply — through the existing order-action gate.
function orderEntry(o) {
  const card = el("div", "run scheduled");
  const head = el("div", "run-head");
  const st = el("span", "job-state " + (o.state === "paused" ? "failed" : "done")); st.textContent = o.state; head.appendChild(st);
  const secs = Number(o.in_seconds);
  const when = !Number.isFinite(secs) ? "" : secs <= 0 ? "due now" : secs < 3600 ? `in ${Math.round(secs / 60)}m` : secs < 172800 ? `in ${Math.round(secs / 3600)}h` : `in ${Math.round(secs / 86400)}d`;
  const meta = el("span", "run-elapsed"); meta.textContent = o.state === "paused" ? `paused · would have fired ${when}` : `next ${when}`; head.appendChild(meta);
  card.appendChild(head);
  const key = el("div", "setting-key"); key.textContent = o.id; card.appendChild(key);
  const actions = el("div", "job-actions");
  for (const verb of o.actions || []) {
    const b = el("button"); b.type = "button"; b.textContent = verb;
    b.addEventListener("click", async () => {
      if (verb === "cancel" && !confirm("Cancel this standing order?")) return;
      b.disabled = true;
      const res = await postJson("/api/order-action", { verb, id: o.id });
      if (!res.ok) { alert(`${verb} failed (${res.status || "offline"}): ${res.text}`); b.disabled = false; }
      else loadTasks();
    });
    actions.appendChild(b);
  }
  card.appendChild(actions);
  return card;
}

function fmtElapsed(ms) {
  if (!ms || ms < 0) return "0s";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/* E.WEB16: templates — the banked skills, with their real track record, as starting points. */
let templatesLoaded = false;
async function loadTemplates() {
  if (templatesLoaded) return;
  const host = $("agent-templates");
  try {
    const r = await fetch("/api/skills", { headers: { "X-YM-Web": "1" } });
    if (!r.ok) { host.replaceChildren(textP(r.status === 403 ? "Operator only." : "Skill library unavailable.")); return; }
    const rep = await r.json();
    const skills = rep.skills || [];
    host.replaceChildren();
    if (!skills.length) { host.appendChild(textP("No banked skills yet — describe an agent from scratch.")); templatesLoaded = true; return; }
    for (const sk of skills) {
      const b = el("button", "tpl" + (sk.failing || sk.status === "quarantined" ? " bad" : "")); b.type = "button";
      const main = el("div", "card-main");
      const n = el("div", "tpl-name"); n.textContent = sk.name; main.appendChild(n);
      const m = el("div", "tpl-meta");
      const rate = sk.success_rate != null ? `${Math.round(sk.success_rate * 100)}% ok` : "untested";
      m.textContent = `${sk.status || "?"} · ${sk.runs || 0} runs · ${rate}${(sk.tags || []).length ? " · " + sk.tags.slice(0, 3).join(", ") : ""}`;
      main.appendChild(m);
      b.appendChild(main);
      b.addEventListener("click", () => {
        document.querySelectorAll(".tpl.on").forEach((x) => x.classList.remove("on"));
        b.classList.add("on");
        $("agent-name").value = sk.name;
        if (!$("agent-task").value.trim()) $("agent-task").value = sk.summary || "";
        $("agent-task").focus();
      });
      host.appendChild(b);
    }
    templatesLoaded = true;
  } catch (_) { host.replaceChildren(textP("Skill library unavailable.")); }
}

/* E.WEB16: what an agent may do — composed from the claims registry, never literal copy. */
let allowanceLoaded = false;
async function loadAllowance() {
  if (allowanceLoaded) return;
  const host = $("agent-allowance");
  try {
    const r = await fetch("/api/claims", { headers: { "X-YM-Web": "1" } });
    if (!r.ok) { host.textContent = ""; return; }
    const reg = await r.json();
    const byId = new Map((reg.claims || []).map((c) => [c.id, c]));
    const lines = ["An agent runs with the mind's own walls:"];
    for (const id of ["real-money", "self-edit", "tool-learning"]) {
      const c = byId.get(id); if (c) lines.push("· " + c.answer);
    }
    lines.push(`[${reg.version || "registry"}]`);
    host.textContent = lines.join("\n");
    host.style.whiteSpace = "pre-line";
    allowanceLoaded = true;
  } catch (_) { host.textContent = ""; }
}

async function loadHorizons() {
  const host = $("horizon-cards");
  try {
    const r = await fetch("/api/horizons", { headers: { "X-YM-Web": "1" } });
    if (r.status === 403) { host.replaceChildren(textP("Operator only.")); return; }
    const data = await r.json();
    host.replaceChildren();
    if (data.available === false) { host.appendChild(textP("The durable-goal engine is not available on this build.")); return; }
    for (const g of data.goals || []) {
      const card = el("div", "card setting-row");
      bucketAdd(card, g);
      const main = el("div", "card-main");
      const t = el("div", "card-title"); t.textContent = g.objective || g.goal_id; main.appendChild(t);
      const wake = g.next_wake_ms ? Math.max(0, g.next_wake_ms - Date.now()) : null;
      const meta = el("div", "dev-meta");
      const mins = wake === null ? null : Math.round(wake / 60000);
      meta.textContent = wake === null ? (g.queue_status || g.status || "")
        : mins >= 120 ? `wakes in ${Math.round(mins / 60)}h · ${g.queue_status || ""}`
        : `wakes in ${mins}m · ${g.queue_status || ""}`;
      main.appendChild(meta);
      const chips = el("div", "budget-chips");
      for (const [label, used, max] of [["actions", g.actions_used, g.max_actions], ["cost", g.spent_cost_units, g.max_cost_units], ["replans", g.plan_revision, null]]) {
        const c = el("span", "tag");
        c.textContent = max == null ? `${label} ${used ?? 0}` : `${label} ${used ?? 0}/${max}`;
        chips.appendChild(c);
      }
      if (g.budget_expired) { const c = el("span", "tag restart"); c.textContent = "budget expired"; chips.appendChild(c); }
      main.appendChild(chips);
      const key = el("div", "setting-key"); key.textContent = g.goal_id; main.appendChild(key);
      // E.WEB14: the receipt chain, on demand — the same verified view the peer checked
      // cryptographically, rendered DOM-only (receipt text is data, never markup).
      const rx = el("div", "receipts hidden");
      const rbtn = el("button"); rbtn.textContent = "receipts"; rbtn.className = "link-btn";
      rbtn.addEventListener("click", async () => {
        if (!rx.classList.contains("hidden")) { rx.classList.add("hidden"); return; }
        rx.replaceChildren(textP("reading the chain…")); rx.classList.remove("hidden");
        const hr = await fetch(`/api/horizon-history?id=${encodeURIComponent(g.goal_id)}`, { headers: { "X-YM-Web": "1" } });
        const h = await hr.json().catch(() => ({ error: "unreadable" }));
        rx.replaceChildren();
        if (!hr.ok || h.error) { rx.appendChild(textP(`chain not verified: ${h.error || hr.status}`)); return; }
        for (const ev of h.lifecycle || []) {
          const line = el("div", "receipt-line");
          const when = new Date(ev.occurred_at_ms).toLocaleTimeString();
          const hop = `${ev.previous_queue_status || "no-queue"} → ${ev.next_queue_status || "terminal"}`;
          line.textContent = `${when} · ${String(ev.event).toUpperCase()} · ${hop} · ${String(ev.receipt_sha256).slice(0, 12)}${ev.failure_reason ? " · " + ev.failure_reason : ""}`;
          rx.appendChild(line);
        }
        if (h.outcome) {
          const o = el("div", "receipt-line outcome");
          o.textContent = `OUTCOME ${h.outcome.status} · actions ${h.outcome.actions} · cost ${h.outcome.spent_cost_units} · ${String(h.outcome.receipt_sha256).slice(0, 12)}`;
          rx.appendChild(o);
        }
        if (!(h.lifecycle || []).length && !h.outcome) rx.appendChild(textP("no receipts yet."));
      });
      main.appendChild(rbtn); main.appendChild(rx);
      card.appendChild(main);
      const side = el("div", "card-side");
      // One reader for a goal's state: the board's rule (queue_status; budget-expired reads as
      // failed). A FAILED goal must not wear an "active" badge because its lifecycle flag says so.
      const col = columnFor("horizon", g.budget_expired ? "failed" : (g.queue_status || g.status));
      const st = el("span", "job-state " + (col === "needs" ? "failed" : col === "running" ? "running" : "done"));
      st.textContent = col === "needs" ? "failed" : col === "running" ? "running" : col === "scheduled" ? "scheduled" : "done";
      side.appendChild(st);
      card.appendChild(side);
      host.appendChild(card);
    }
    const goals = data.goals || [];
    if (!goals.length) host.appendChild(textP("No durable goals — schedule one under New agent. They survive restarts and act on schedule."));
    else {
      // E.WEB17: each view says so when its half is empty, instead of a heading over nothing.
      if (!goals.some((g) => agentBucket(g) === "running")) { const p = textP("No goal is running right now."); p.classList.add("bucket-running"); host.appendChild(p); }
      if (!goals.some((g) => agentBucket(g) === "dormant")) { const p = textP("No goals waiting or finished."); p.classList.add("bucket-dormant"); host.appendChild(p); }
    }
  } catch (_) { host.replaceChildren(textP("Could not read durable goals.")); }
}

/* E.WEB17/18: standing orders, typed — the same store the tick reads. They join their agent's
   thread by name; there is no separate list to drift from it. */
async function fetchStandingOrders() {
  try {
    const r = await fetch("/api/standing-orders", { headers: { "X-YM-Web": "1" } });
    if (!r.ok) return [];
    const data = await r.json();
    return data.store === false ? [] : (data.orders || []);
  } catch (_) { return []; }
}

$("tab-delegate").addEventListener("click", () => {
  $("tab-delegate").classList.add("active"); $("tab-import").classList.remove("active");
  $("agent-form").classList.remove("hidden"); $("import-form").classList.add("hidden");
});
$("tab-import").addEventListener("click", () => {
  $("tab-import").classList.add("active"); $("tab-delegate").classList.remove("active");
  $("import-form").classList.remove("hidden"); $("agent-form").classList.add("hidden");
});

$("agent-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  // A broad description is the point — newlines are the agent's brief, so they ride verbatim.
  const name = $("agent-name").value.trim(), task = $("agent-task").value.trim();
  if (!name || !task) return;
  const btn = $("agent-btn"), reply = $("agent-reply");
  btn.disabled = true;
  const sched = $("agent-schedule").value;
  let res;
  if (sched === "none") {
    reply.classList.remove("hidden"); reply.textContent = "delegating…";
    res = await postJson("/api/agent", { name, task });
  } else {
    // A standing order is an agent DOCUMENT with schedule frontmatter — composed here, judged by
    // the import gate exactly as a pasted document would be. The console adds no new path.
    const time = $("agent-time").value || "09:00";
    const line = sched === "weekly" ? `weekly ${$("agent-weekday").value} ${time}` : `daily ${time}`;
    const doc = `---
name: ${name}
description: ${task.split("\n")[0].slice(0, 120)}
schedule: ${line}
---
# ${name}
${task}
`;
    reply.classList.remove("hidden"); reply.textContent = "arming the schedule…";
    res = await postJson("/api/import-agent", { doc });
  }
  if (res.ok) {
    reply.textContent = (res.data && res.data.reply) || "delegated.";
    $("agent-name").value = ""; $("agent-task").value = "";
  } else {
    reply.textContent = `${sched === "none" ? "delegation" : "schedule"} failed (${res.status || "offline"}): ${res.text}`;
  }
  btn.disabled = false;
  if (res.ok) loadTasks();
});

$("agent-schedule").addEventListener("change", () => {
  const v = $("agent-schedule").value;
  $("agent-weekday").classList.toggle("hidden", v !== "weekly");
  $("agent-time").classList.toggle("hidden", v === "none");
});

$("horizon-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const delay = $("horizon-delay").value, goal = $("horizon-goal").value.trim();
  if (!goal) return;
  const btn = $("horizon-btn"), reply = $("horizon-reply");
  btn.disabled = true;
  reply.classList.remove("hidden"); reply.textContent = "asking the planner… (this can take a minute — the plan is audited before it may persist)";
  const res = await postJson("/api/horizon", { delay, goal });
  if (res.ok) {
    const said = (res.data && res.data.reply) || "";
    reply.textContent = said || "the planner returned nothing — the goal was NOT scheduled.";
    if (said.includes("scheduled")) $("horizon-goal").value = "";
  } else {
    reply.textContent = `scheduling failed (${res.status || "offline"}): ${res.text}`;
  }
  btn.disabled = false;
  if (res.ok) loadHorizons();
});

$("import-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const doc = $("import-doc").value.trim();
  if (!doc) return;
  const btn = $("import-btn"), reply = $("agent-reply");
  btn.disabled = true;
  reply.classList.remove("hidden"); reply.textContent = "importing…";
  const res = await postJson("/api/import-agent", { doc });
  if (res.ok) {
    reply.textContent = (res.data && res.data.reply) || "imported.";
    $("import-doc").value = "";
  } else {
    reply.textContent = `import failed (${res.status || "offline"}): ${res.text}`;
  }
  btn.disabled = false;
  if (res.ok) loadTasks();
});

/* ── message DOM ──────────────────────────────────────────────────────── */
const fmtTs = (ts) => new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

function addUserMsg(text, ts, save = true) {
  $("welcome").classList.add("hidden");
  const msg = el("div", "msg user");
  const bubble = el("div", "bubble");
  bubble.textContent = text;
  const stamp = el("div", "stamp"); stamp.textContent = fmtTs(ts);
  bubble.appendChild(stamp);
  msg.appendChild(bubble); feed.appendChild(msg);
  if (save) { history.push({ role: "user", text, ts }); persist(); }
  scrollToEnd();
}

function addMindMsg() {
  const msg = el("div", "msg mind");
  const avatar = el("div", "orb avatar");
  avatar.textContent = fmtTs(Date.now());
  const bubble = el("div", "bubble");
  bubble.dataset.who = MIND;
  const steps = el("div", "steps hidden");
  const lanes = el("div", "lanes hidden");
  const think = document.createElement("details"); think.className = "think hidden";
  const sum = document.createElement("summary"); sum.textContent = "reasoning";
  const thinkBody = el("div", "think-body");
  think.append(sum, thinkBody);
  const tail = el("div", "tail hidden");
  const md = el("div", "md");
  const stamp = el("div", "stamp");
  bubble.append(lanes, steps, think, tail, md, stamp);
  msg.append(avatar, bubble);
  feed.appendChild(msg);
  scrollToEnd();
  return { msg, avatar, steps, lanes, laneSeen: new Set(), think, thinkBody, tail, md, stamp };
}

function el(tag, cls) { const n = document.createElement(tag); if (cls) n.className = cls; return n; }

/* keep the view pinned to the end unless the reader scrolled up on purpose */
let pinned = true;
feed.addEventListener("scroll", () => {
  pinned = feed.scrollTop + feed.clientHeight >= feed.scrollHeight - 60;
});
function scrollToEnd(force) { if (pinned || force) feed.scrollTop = feed.scrollHeight; }

/* ── sending a turn: the /chat-stream line protocol over fetch streaming ── */
let busy = false;
async function sendTurn() {
  const text = input.value.trim();
  if (!text || busy) return;
  busy = true; $("send-btn").disabled = true;
  input.value = ""; input.style.height = "auto";
  addUserMsg(text, Date.now());

  const b = addMindMsg();
  const orb = $("status-orb"); orb.classList.add("thinking"); b.avatar.classList.add("thinking");
  $("mind-state").textContent = "thinking…";
  let finalText = null;

  try {
    const r = await fetch("/api/turn", { method: "POST", headers: HDRS, body: JSON.stringify({ text }) });
    if (r.status === 401) { location.reload(); return; }
    if (!r.ok || !r.body) throw new Error("turn failed: " + r.status);
    const reader = r.body.getReader();
    const dec = new TextDecoder();
    // Line protocol: p:/t:/d:/k: lines separated by real newlines (payloads carry \u0001 in place
    // of newlines), then one terminal "f:" whose payload MAY contain real newlines — so once an
    // "f:" line starts, everything to the end of the stream is the final reply.
    let carry = "", inFinal = false;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      const chunk = dec.decode(value, { stream: true });
      if (inFinal) { finalText += chunk; continue; }
      carry += chunk;
      const fIdx = carry.startsWith("f:") ? 0 : (carry.indexOf("\nf:") >= 0 ? carry.indexOf("\nf:") + 1 : -1);
      if (fIdx >= 0) {
        for (const l of carry.slice(0, fIdx).split("\n")) handleLine(l, b);
        finalText = carry.slice(fIdx + 2);
        inFinal = true; carry = "";
      } else {
        const lines = carry.split("\n"); carry = lines.pop() ?? "";
        for (const l of lines) handleLine(l, b);
      }
      scrollToEnd();
    }
    if (!inFinal && carry.startsWith("f:")) finalText = carry.slice(2);
  } catch (e) {
    finalText = "*(could not reach the mind — is it running?)*";
  }

  // settle: steps stay as the visible record, reasoning folds shut, tail is replaced by the reply
  b.tail.classList.add("hidden");
  if (b.think.open) b.think.open = false;
  document.querySelectorAll(".step.live").forEach((n) => n.classList.remove("live"));
  const ts = Date.now();
  renderMarkdown(b.md, finalText ?? "(no reply)");
  b.stamp.textContent = fmtTs(ts);
  history.push({ role: "mind", text: finalText ?? "(no reply)", ts }); persist();
  orb.classList.remove("thinking"); b.avatar.classList.remove("thinking");
  if (b.laneSeen.size === 0) $("mind-state").textContent = "at home";
  busy = false; $("send-btn").disabled = false;
  scrollToEnd(); input.focus();
}

function handleLine(line, b) {
  if (!line) return;
  const kind = line.slice(0, 2);
  const rest = line.slice(2).replaceAll("\u0001", "\n");
  if (kind === "l:") {
    // The lane badge's ONLY source: the dispatch boundary's own l: event (scope:label). The
    // client renders what the server declared and never infers a lane from anything else.
    // First delimiter ONLY — the label itself may carry colons (nanogpt:deepseek/deepseek-v4-pro),
    // and JS split-with-limit DISCARDS the remainder, which would misname the serving provider on
    // the one chip whose whole job is naming it truthfully (Codex's E.OBS1 review).
    const cut = rest.indexOf(":");
    const scope = cut < 0 ? rest : rest.slice(0, cut);
    const label = cut < 0 ? "" : rest.slice(cut + 1);
    const key = scope + ":" + (label || "");
    if (!b.laneSeen.has(key)) {
      b.laneSeen.add(key);
      b.lanes.classList.remove("hidden");
      const chip = el("span", "lane-chip lane-" + scope);
      chip.textContent = scope === "private" ? "private · stayed home" : scope + " · " + (label || "?");
      chip.title = "This model call was served on the " + scope + " lane by '" + (label || "?") + "' — declared by the dispatch boundary, not inferred.";
      b.lanes.appendChild(chip);
    }
  } else if (kind === "p:") {
    b.steps.classList.remove("hidden");
    document.querySelectorAll(".step.live").forEach((n) => n.classList.remove("live"));
    const chip = el("span", "step live"); chip.textContent = rest;
    b.steps.appendChild(chip);
  } else if (kind === "t:") {
    b.think.classList.remove("hidden"); b.think.open = true;
    b.thinkBody.textContent += rest;
    b.thinkBody.scrollTop = b.thinkBody.scrollHeight;
  } else if (kind === "k:") {
    b.tail.classList.remove("hidden");
    b.tail.textContent += rest;
  } else if (kind === "d:") {
    const live = b.steps.querySelector(".step.live");
    if (live) live.title = ((live.title || "") + "\n" + rest).trim();
  }
}

/* ── composer ergonomics ──────────────────────────────────────────────── */
input.addEventListener("input", () => {
  input.style.height = "auto";
  input.style.height = Math.min(input.scrollHeight, 180) + "px";
});
input.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); sendTurn(); }
});
$("send-btn").addEventListener("click", sendTurn);

/* ── markdown → DOM, sanitized by construction ────────────────────────── */
function renderMarkdown(root, src) {
  root.replaceChildren();
  const lines = String(src).split("\n");
  let i = 0, list = null, listTag = "";
  const closeList = () => { list = null; };
  while (i < lines.length) {
    let line = lines[i];

    const fence = line.match(/^```(\w*)\s*$/);
    if (fence) {
      closeList();
      const buf = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) buf.push(lines[i++]);
      i++;
      const pre = el("pre"); const code = el("code");
      if (fence[1]) code.className = "lang-" + fence[1];
      code.textContent = buf.join("\n");
      const copy = el("button", "copy"); copy.textContent = "copy"; copy.type = "button";
      copy.addEventListener("click", () => {
        navigator.clipboard.writeText(code.textContent).then(() => {
          copy.textContent = "copied"; setTimeout(() => (copy.textContent = "copy"), 1200);
        });
      });
      pre.append(copy, code); root.appendChild(pre);
      continue;
    }

    if (/^\s*\|.+\|\s*$/.test(line) && i + 1 < lines.length && /^\s*\|[\s:|-]+\|\s*$/.test(lines[i + 1])) {
      closeList();
      const table = el("table");
      const thead = el("thead"); const hr = el("tr");
      for (const c of splitRow(line)) { const th = el("th"); inline(th, c); hr.appendChild(th); }
      thead.appendChild(hr); table.appendChild(thead);
      i += 2;
      const tbody = el("tbody");
      while (i < lines.length && /^\s*\|.+\|\s*$/.test(lines[i])) {
        const tr = el("tr");
        for (const c of splitRow(lines[i])) { const td = el("td"); inline(td, c); tr.appendChild(td); }
        tbody.appendChild(tr); i++;
      }
      table.appendChild(tbody); root.appendChild(table);
      continue;
    }

    const h = line.match(/^(#{1,3})\s+(.*)$/);
    if (h) { closeList(); const node = el("h" + h[1].length); inline(node, h[2]); root.appendChild(node); i++; continue; }

    if (/^\s*([-*_]){3,}\s*$/.test(line)) { closeList(); root.appendChild(el("hr")); i++; continue; }

    const q = line.match(/^>\s?(.*)$/);
    if (q) {
      closeList();
      const bq = el("blockquote"); const p = el("p"); inline(p, q[1]); bq.appendChild(p);
      root.appendChild(bq); i++; continue;
    }

    const li = line.match(/^(\s*)([-*]|\d+\.)\s+(.*)$/);
    if (li) {
      const tag = /\d/.test(li[2]) ? "ol" : "ul";
      if (!list || listTag !== tag) { list = el(tag); listTag = tag; root.appendChild(list); }
      const item = el("li"); inline(item, li[3]); list.appendChild(item);
      i++; continue;
    }

    if (line.trim() === "") { closeList(); i++; continue; }

    closeList();
    const p = el("p"); inline(p, line); root.appendChild(p); i++;
  }
}

function splitRow(row) {
  return row.trim().replace(/^\|/, "").replace(/\|$/, "").split("|").map((s) => s.trim());
}

/* inline spans: bold, italic, code, links — DOM nodes only, textContent only */
function inline(parent, text) {
  const tokens = String(text).split(/(\*\*[^*]+\*\*|\*[^*]+\*|`[^`]+`|\[[^\]]+\]\([^)\s]+\))/g);
  for (const t of tokens) {
    if (!t) continue;
    let m;
    if ((m = t.match(/^\*\*([^*]+)\*\*$/))) { const b = el("strong"); b.textContent = m[1]; parent.appendChild(b); }
    else if ((m = t.match(/^\*([^*]+)\*$/))) { const it = el("em"); it.textContent = m[1]; parent.appendChild(it); }
    else if ((m = t.match(/^`([^`]+)`$/))) { const c = el("code"); c.textContent = m[1]; parent.appendChild(c); }
    else if ((m = t.match(/^\[([^\]]+)\]\(([^)\s]+)\)$/))) {
      if (/^https?:\/\//i.test(m[2])) {
        const a = el("a"); a.textContent = m[1]; a.href = m[2];
        a.target = "_blank"; a.rel = "noopener noreferrer";
        parent.appendChild(a);
      } else parent.appendChild(document.createTextNode(t));
    } else parent.appendChild(document.createTextNode(t));
  }
}

// E.WEB10: theme choice — a per-viewer convenience, remembered in localStorage, never server state.
(function initTheme() {
  try {
    const saved = localStorage.getItem("ym-theme");
    if (saved === "light" || saved === "dark") document.documentElement.setAttribute("data-theme", saved);
  } catch (_) {}
  const btn = document.getElementById("theme-btn");
  if (btn) btn.addEventListener("click", () => {
    const now = document.documentElement.getAttribute("data-theme") === "light" ? "dark" : "light";
    document.documentElement.setAttribute("data-theme", now);
    try { localStorage.setItem("ym-theme", now); } catch (_) {}
  });
})();

boot();
