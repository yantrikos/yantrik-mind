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
}

function showApp() {
  $("pair-screen").classList.add("hidden");
  $("app").classList.remove("hidden");
  restoreHistory();
  refreshWelcome();
  input.focus();
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
    if (btn.dataset.panel === "capabilities") loadCapabilities();
    if (btn.dataset.panel === "settings") loadSettings();
    if (btn.dataset.panel === "devices") loadDevices();
    if (btn.dataset.panel === "tasks") loadTasks();
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
  input.value = `Hi, I'm ${userName} — that's what you should call me. And we've named you ${mindName}: that's your name now, please use it when you talk about yourself.`;
  sendTurn();
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
    const r = await fetch("/api/capabilities", { headers: { "X-YM-Web": "1" } });
    const rep = await r.json();
    host.replaceChildren();
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
  } catch (_) { host.replaceChildren(textP("Could not read the capability report.")); }
}

async function loadSettings() {
  const host = $("settings-cards");
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
  } catch (_) { host.replaceChildren(textP("Could not read configuration.")); }
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
  } catch (_) { host.replaceChildren(textP("Could not read the device store.")); }
}

function textP(s) { const p = el("p", "loading"); p.textContent = s; return p; }

/* ── agents & standing orders ─────────────────────────────────────────── */
let tasksTimer = null;
async function loadTasks() {
  const host = $("job-cards");
  try {
    const r = await fetch("/api/tasks", { headers: { "X-YM-Web": "1" } });
    if (r.status === 403) { host.replaceChildren(textP("Operator only.")); return; }
    const data = await r.json();
    host.replaceChildren();
    const jobs = data.jobs || [];
    let anyRunning = false;
    // newest first — the board reads like a feed
    for (const j of [...jobs].reverse()) {
      const state = String(j.state || j.status || "?").toLowerCase();
      if (state.includes("run")) anyRunning = true;
      const card = el("div", "card setting-row");
      const main = el("div", "card-main");
      const t = el("div", "card-title"); t.textContent = j.name || j.id; main.appendChild(t);
      const d = el("div", "card-desc"); d.textContent = j.task || j.goal || ""; main.appendChild(d);
      if (j.result) {
        const res = el("div", "job-result"); res.textContent = String(j.result); main.appendChild(res);
      }
      const notes = Array.isArray(j.notes) ? j.notes.length : 0;
      const meta = el("div", "setting-key");
      meta.textContent = `${j.id}${notes ? ` · ${notes} note${notes > 1 ? "s" : ""}` : ""}`;
      main.appendChild(meta);
      const actions = el("div", "job-actions");
      for (const [verb, label, ask] of [["keep", "Keep", null], ["drop", "Drop scratch", null], ["delete", "Delete", "Delete this job's record from the board?"]]) {
        const b = el("button"); b.textContent = label;
        b.addEventListener("click", async () => {
          if (ask && !confirm(ask)) return;
          const res = await postJson("/api/task-action", { verb, id: j.id });
          if (!res.ok) alert(`${verb} failed (${res.status || "offline"}): ${res.text}`);
          else loadTasks();
        });
        actions.appendChild(b);
      }
      main.appendChild(actions);
      card.appendChild(main);
      const side = el("div", "card-side");
      const st = el("span", "job-state " + (state.includes("run") ? "running" : state.includes("fail") ? "failed" : "done"));
      st.textContent = state; side.appendChild(st);
      card.appendChild(side);
      host.appendChild(card);
    }
    if (!jobs.length) host.appendChild(textP("The board is empty — delegate something above."));
    // A running agent keeps the board live; a quiet board stops polling.
    clearTimeout(tasksTimer);
    if (anyRunning && $("panel-tasks").classList.contains("active")) tasksTimer = setTimeout(loadTasks, 5000);
  } catch (_) { host.replaceChildren(textP("Could not read the board.")); }
  loadHorizons();
  loadOrders();
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
      card.appendChild(main);
      const side = el("div", "card-side");
      const st = el("span", "job-state " + ((g.status || "").includes("active") ? "running" : "done"));
      st.textContent = g.status || "?"; side.appendChild(st);
      card.appendChild(side);
      host.appendChild(card);
    }
    if (!(data.goals || []).length) host.appendChild(textP("No durable goals — schedule one above. They survive restarts and act on schedule."));
  } catch (_) { host.replaceChildren(textP("Could not read durable goals.")); }
}

async function loadOrders() {
  const pre = $("orders-text");
  try {
    const r = await fetch("/api/orders", { headers: { "X-YM-Web": "1" } });
    const data = await r.json();
    pre.textContent = data.text || "(none)";
  } catch (_) { pre.textContent = "(could not read standing orders)"; }
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
  reply.classList.remove("hidden"); reply.textContent = "delegating…";
  const res = await postJson("/api/agent", { name, task });
  if (res.ok) {
    reply.textContent = (res.data && res.data.reply) || "delegated.";
    $("agent-name").value = ""; $("agent-task").value = "";
  } else {
    reply.textContent = `delegation failed (${res.status || "offline"}): ${res.text}`;
  }
  btn.disabled = false;
  if (res.ok) loadTasks();
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
  const bubble = el("div", "bubble");
  const steps = el("div", "steps hidden");
  const think = document.createElement("details"); think.className = "think hidden";
  const sum = document.createElement("summary"); sum.textContent = "reasoning";
  const thinkBody = el("div", "think-body");
  think.append(sum, thinkBody);
  const tail = el("div", "tail hidden");
  const md = el("div", "md");
  const stamp = el("div", "stamp");
  bubble.append(steps, think, tail, md, stamp);
  msg.append(avatar, bubble);
  feed.appendChild(msg);
  scrollToEnd();
  return { msg, avatar, steps, think, thinkBody, tail, md, stamp };
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
  $("mind-state").textContent = "at home";
  busy = false; $("send-btn").disabled = false;
  scrollToEnd(); input.focus();
}

function handleLine(line, b) {
  if (!line) return;
  const kind = line.slice(0, 2);
  const rest = line.slice(2).replaceAll("\u0001", "\n");
  if (kind === "p:") {
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

boot();
