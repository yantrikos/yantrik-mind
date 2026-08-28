// Yantrik web client. One rule above all others: MODEL OUTPUT IS HOSTILE INPUT. Nothing the mind
// says ever reaches innerHTML — the markdown renderer below builds DOM nodes and assigns text via
// textContent only, so a prompt-injected "<script>" arrives on screen as seven harmless glyphs.
"use strict";

const $ = (id) => document.getElementById(id);
const feed = $("feed");
const input = $("input");
const HDRS = { "Content-Type": "application/json", "X-YM-Web": "1" };

/* ── boot: paired or not? ─────────────────────────────────────────────── */
async function boot() {
  try {
    const r = await fetch("/api/me", { headers: { "X-YM-Web": "1" } });
    if (r.ok) {
      const me = await r.json();
      $("person-chip").textContent = me.person || "operator";
      showChat();
      return;
    }
  } catch (_) { /* fall through to pairing */ }
  $("pair-screen").classList.remove("hidden");
  $("pair-code").focus();
}

function showChat() {
  $("pair-screen").classList.add("hidden");
  $("chat-screen").classList.remove("hidden");
  restoreHistory();
  input.focus();
}

/* ── pairing ──────────────────────────────────────────────────────────── */
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
      body: JSON.stringify({ code: $("pair-code").value.trim(), name: $("pair-name").value.trim() }),
    });
    if (r.ok) { showChat(); return; }
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

/* ── transcript persistence (this browser only — the mind's memory is its own) ── */
const STORE = "ym-transcript-v1";
let history = [];
function persist() { try { localStorage.setItem(STORE, JSON.stringify(history.slice(-200))); } catch (_) {} }
function restoreHistory() {
  try { history = JSON.parse(localStorage.getItem(STORE) || "[]"); } catch (_) { history = []; }
  feed.replaceChildren();
  for (const m of history) {
    if (m.role === "user") addUserMsg(m.text, m.ts, false);
    else { const b = addMindMsg(false); renderMarkdown(b.md, m.text); b.stamp.textContent = fmtTs(m.ts); }
  }
  scrollToEnd(true);
}
$("clear-btn").addEventListener("click", () => {
  if (!confirm("Clear this browser's transcript? (The mind's own memory is unaffected.)")) return;
  history = []; persist(); feed.replaceChildren();
});
$("logout-btn").addEventListener("click", async () => {
  try { await fetch("/api/logout", { method: "POST", headers: HDRS }); } catch (_) {}
  location.reload();
});

/* ── message DOM ──────────────────────────────────────────────────────── */
const fmtTs = (ts) => new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

function addUserMsg(text, ts, save = true) {
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
feed && feed.addEventListener("scroll", () => {
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
    // Line protocol: p:/t:/d:/k: lines separated by real newlines (their payloads carry  in
    // place of newlines), then one terminal "f:" whose payload MAY contain real newlines — so the
    // moment an "f:" line starts, everything after it to the end of the stream is the final reply.
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

  // settle the message: steps stay as the visible record, reasoning folds shut, tail is replaced
  b.tail.classList.add("hidden");
  if (b.think.open) b.think.open = false;
  document.querySelectorAll(".step.live").forEach((n) => n.classList.remove("live"));
  const ts = Date.now();
  renderMarkdown(b.md, finalText ?? "(no reply)");
  b.stamp.textContent = fmtTs(ts);
  history.push({ role: "mind", text: finalText ?? "(no reply)", ts }); persist();
  orb.classList.remove("thinking"); b.avatar.classList.remove("thinking");
  $("mind-state").textContent = "at home · private by default";
  busy = false; $("send-btn").disabled = false;
  scrollToEnd(); input.focus();
}

function handleLine(line, b) {
  if (!line) return;
  const kind = line.slice(0, 2);
  const rest = line.slice(2).replaceAll("\x01", "\n");
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
    // step detail: attach as a tooltip on the live chip — depth without noise
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

    // fenced code
    const fence = line.match(/^```(\w*)\s*$/);
    if (fence) {
      closeList();
      const buf = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) buf.push(lines[i++]);
      i++; // consume closing fence
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

    // table: a header row followed by a separator row
    if (/^\s*\|.+\|\s*$/.test(line) && i + 1 < lines.length && /^\s*\|[\s:|-]+\|\s*$/.test(lines[i + 1])) {
      closeList();
      const table = el("table");
      const headCells = splitRow(line);
      const thead = el("thead"); const hr = el("tr");
      for (const c of headCells) { const th = el("th"); inline(th, c); hr.appendChild(th); }
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
      // only web links become anchors; anything else stays literal text
      if (/^https?:\/\//i.test(m[2])) {
        const a = el("a"); a.textContent = m[1]; a.href = m[2];
        a.target = "_blank"; a.rel = "noopener noreferrer";
        parent.appendChild(a);
      } else parent.appendChild(document.createTextNode(t));
    } else parent.appendChild(document.createTextNode(t));
  }
}

boot();
