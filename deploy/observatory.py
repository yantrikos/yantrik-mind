#!/usr/bin/env python3
"""yantrik-mind OBSERVATORY — a window into a mind aging in real time.

Single-file stdlib HTTP server (no deps). Read-only: renders the vitals the
mind's own processes publish — the immune summary (root-owned), the sealed
trial ledger, the behavioral-eval history, and the evolution log. Serve on
the LAN only; this is the family's window, not the internet's.

  python3 observatory.py [--port 8787] [--state /var/lib/yantrik-mind]

Systemd: deploy/observatory.service
"""
import argparse
import html
import json
import pathlib
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE = pathlib.Path("/var/lib/yantrik-mind")
REPO = pathlib.Path("/opt/yantrik-mind")


def read_json(p):
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except Exception:
        return None


def read_jsonl(p, limit=200):
    try:
        lines = [l for l in p.read_text(encoding="utf-8").splitlines() if l.strip()]
        return [json.loads(l) for l in lines[-limit:]]
    except Exception:
        return []


def vitals():
    immune = read_json(STATE / "immune" / "immune_summary.json") or {}
    ledger = read_jsonl(STATE / "immune" / "immune_trials.jsonl")
    evals = read_jsonl(REPO / "evals_history.jsonl") or read_jsonl(pathlib.Path("evals_history.jsonl"))
    evolution = []
    try:
        evolution = (STATE / "evolution.log").read_text(encoding="utf-8").splitlines()[-12:]
    except Exception:
        pass
    return immune, ledger, evals, evolution


def bar(frac, width=28):
    n = max(0, min(width, round(frac * width)))
    return "█" * n + "░" * (width - n)


def page():
    immune, ledger, evals, evolution = vitals()
    e = immune.get("epoch", {})
    latest = immune.get("latest", {})
    rows = []

    def sec(title, body):
        rows.append(f"<section><h2>{title}</h2>{body}</section>")

    # Immune
    if latest:
        det = latest.get("seeds_flagged", 0), latest.get("n_seeds", 0)
        dmg = latest.get("controls_flagged", 0), latest.get("n_controls", 0)
        lb = e.get("detection_lower_bound", 0.0)
        brier = e.get("brier")
        brier_s = f"{brier:.3f}" if brier is not None else "—"
        abst = e.get("abstention_rate")
        abst_s = f"{abst:.0%}" if abst is not None else "—"
        body = (
            f"<p class='big'>last trial: caught <b>{det[0]}/{det[1]}</b> planted lies · "
            f"<b>{dmg[0]}/{dmg[1]}</b> false alarms</p>"
            f"<p><code>{bar(lb)}</code> detection lower bound {lb:.0%} (bar: 30%)</p>"
            f"<p>epoch: {e.get('trials', 0)} trials · {e.get('unique_seeds', 0)} unique seeds · "
            f"{e.get('families', 0)}/3 families · Brier {brier_s} · abstention {abst_s}</p>"
            f"<p>promotion bar: <b>{'MET' if e.get('promotion_bar_met') else 'not yet — flags stay in the lab'}</b> · "
            f"ledger sealed, {len(ledger)} chained records · head <code>{(immune.get('chain_head') or '')[:16]}…</code></p>"
        )
        missed = latest.get("missed_lies") or []
        if missed:
            body += "<p class='dim'>lies that got past it: " + " · ".join(html.escape(m) for m in missed[:5]) + "</p>"
    else:
        body = "<p>no trials recorded yet — the timer plants its first lies this week.</p>"
    sec("🧫 Immune system — seeded-lie trials on snapshots of its own memory", body)

    # Behavioral evals trend
    if evals:
        last = evals[-1]
        spark = "".join("▁▂▃▄▅▆▇█"[min(7, int(x.get("score", 0) * 8))] for x in evals[-40:])
        sec(
            "🎓 Behavioral suite (the loss function)",
            f"<p class='big'>{last.get('passed')}/{last.get('total')} at commit <code>{last.get('commit', '?')}</code></p>"
            f"<p><code>{html.escape(spark)}</code> last {min(40, len(evals))} runs</p>",
        )

    # Evolution log
    if evolution:
        items = "".join(f"<li><code>{html.escape(l)}</code></li>" for l in reversed(evolution))
        sec("🧬 Self-build (recent outcomes)", f"<ul>{items}</ul>")

    body = "".join(rows) or "<p>no vitals yet</p>"
    return f"""<!doctype html><html><head><meta charset="utf-8">
<meta http-equiv="refresh" content="30"><title>yantrik-mind observatory</title>
<style>
body{{background:#0b0e14;color:#cdd6e4;font:15px/1.6 -apple-system,'Segoe UI',sans-serif;max-width:780px;margin:2rem auto;padding:0 1rem}}
h1{{font-size:1.3rem;letter-spacing:.06em}} h2{{font-size:1rem;color:#8fa3bf;border-bottom:1px solid #1d2635;padding-bottom:.3rem}}
code{{color:#7fd1b9;font-family:ui-monospace,Consolas,monospace}} .big{{font-size:1.15rem}} .dim{{color:#6b7a90}}
section{{margin-bottom:1.6rem}} ul{{margin:.3rem 0;padding-left:1.2rem}} li{{margin:.15rem 0}}
footer{{color:#4a5568;font-size:.8rem;margin-top:2rem}}
</style></head><body>
<h1>🔭 YANTRIK-MIND OBSERVATORY</h1>
<p class="dim">a fixed-weight mind, aging on its own memory · refreshed {time.strftime('%Y-%m-%d %H:%M:%S')} · auto-reloads every 30s</p>
{body}
<footer>read-only window. the mind can read these numbers; it cannot edit them.</footer>
</body></html>"""


class H(BaseHTTPRequestHandler):
    def do_GET(self):
        content = page().encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(content)))
        self.end_headers()
        self.wfile.write(content)

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8787)
    ap.add_argument("--state", default=str(STATE))
    a = ap.parse_args()
    STATE = pathlib.Path(a.state)
    print(f"observatory on http://0.0.0.0:{a.port} (state: {STATE})")
    ThreadingHTTPServer(("0.0.0.0", a.port), H).serve_forever()
