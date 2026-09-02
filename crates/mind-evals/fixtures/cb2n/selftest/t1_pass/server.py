"""Self-test fixture: a standard-library server meeting the T1 contract exactly (also the proof
that the contract is buildable in ~60 lines)."""
import json, os, datetime, urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer

STORE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data", "leads.json")


def load():
    try:
        return json.load(open(STORE, encoding="utf-8"))
    except Exception:
        return []


def dashboard():
    leads = load()
    dates = sorted(l["created_at"][:10] for l in leads if "created_at" in l)
    end = datetime.date.fromisoformat(dates[-1]) if dates else datetime.datetime.now(datetime.timezone.utc).date()
    bins = [(end - datetime.timedelta(days=13 - i)).isoformat() for i in range(14)]
    per_day = {b: 0 for b in bins}
    for l in leads:
        k = l.get("created_at", "")[:10]
        if k in per_day:
            per_day[k] += 1
    recent = [l["name"] for l in sorted(leads, key=lambda l: l.get("created_at", ""), reverse=True)[:5]]
    return {"total": len(leads), "per_day": per_day, "recent": recent}


PAGE = (
    "<!doctype html><html><head><title>Ledgerly Bookkeeping</title></head><body>"
    "<h1>Books done right for Indian small businesses</h1>"
    '<form id="cb2-lead-form" method="post" action="/lead">'
    '<input name="name" placeholder="Name"><input name="email" placeholder="Email">'
    '<input name="phone" placeholder="Phone"><input name="business" placeholder="Business">'
    '<textarea name="message"></textarea><button type="submit">Get a call back</button></form>'
    '<a href="/dashboard">dashboard</a></body></html>'
)
INCLUDE_JSON_BLOCK = True
APPEND = True


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def send(self, body, ctype="text/html"):
        b = body.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        if self.path.startswith("/dashboard"):
            d = dashboard()
            rows = "".join(f"<li>{k}: {v}</li>" for k, v in d["per_day"].items())
            block = f'<script id="cb2-dashboard" type="application/json">{json.dumps(d)}</script>' if INCLUDE_JSON_BLOCK else ""
            self.send(f"<!doctype html><html><body><h1>Leads: {d['total']}</h1><ul>{rows}</ul>"
                      f"<ol>{''.join('<li>' + n + '</li>' for n in d['recent'])}</ol>{block}</body></html>")
        else:
            self.send(PAGE)

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        q = urllib.parse.parse_qs(self.rfile.read(n).decode("utf-8"))
        lead = {k: q.get(k, [""])[0] for k in ("name", "email", "phone", "business", "message")}
        lead["created_at"] = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        leads = load()
        if APPEND:
            leads.append(lead)
        os.makedirs(os.path.dirname(STORE), exist_ok=True)
        json.dump(leads, open(STORE, "w", encoding="utf-8"), indent=1)
        self.send_response(303)
        self.send_header("Location", "/")
        self.end_headers()


HTTPServer(("0.0.0.0", 8123), H).serve_forever()
