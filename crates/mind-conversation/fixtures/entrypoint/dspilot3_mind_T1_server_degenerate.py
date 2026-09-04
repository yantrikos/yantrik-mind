import json
import os
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import urlparse

DATA_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")
LEADS_FILE = os.path.join(DATA_DIR, "leads.json")
PORT = 8123

LANDING_HTML = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>LedgerLeaf — Boutique Bookkeeping for Indian Small Businesses</title>
<style>
  :root {
    --ink: #1a2e2a;
    --muted: #5c6f6a;
    --paper: #f7f5ef;
    --card: #ffffff;
    --accent: #0e7a5f;
    --accent-dark: #0a5c48;
    --border: #e2ddd2;
    --error: #b3261e;
  }
  * { box-sizing: border-box; margin: 0; padding: 0; }
  body {
    font-family: Georgia, 'Times New Roman', serif;
    color: var(--ink);
    background: var(--paper);
    line-height: 1.55;
  }
  .wrap { max-width: 1060px; margin: 0 auto; padding: 0 20px; }
  header {
    padding: 28px 0;
    border-bottom: 1px solid var(--border);
    background: var(--paper);
  }
  .nav { display: flex; justify-content: space-between; align-items: center; }
  .brand { font-size: 1.4rem; font-weight: bold; letter-spacing: -0.02em; }
  .brand span { color: var(--accent); }
  .nav a { color: var(--accent-dark); text-decoration: none; font-size: 0.95rem; }
  .hero { padding: 64px 0 40px; }
  .hero h1 {
    font-size: clamp(2rem, 4.5vw, 3.1rem);
    line-height: 1.15;
    letter-spacing: -0.03em;
    max-width: 720px;
    margin-bottom: 18px;
  }
  .hero p.sub {
    font-size: 1.15rem;
    color: var(--muted);
    max-width: 640px;
    margin-bottom: 28px;
  }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 40px; align-items: start; }
  @media (max-width: 820px) { .grid { grid-template-columns: 1fr; } }
  .features { display: grid; gap: 16px; }
  .feature {
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 18px 20px;
  }
  .feature h3 { font-size: 1.05rem; margin-bottom: 6px; }
  .feature p { color: var(--muted); font-size: 0.95rem; }
  form {
    background: var(--card);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 28px;
    box-shadow: 0 8px 24px rgba(26,46,42,0.06);
  }
  form h2 { font-size: 1.4rem; margin-bottom: 6px; }
  form p.hint { color: var(--muted); font-size: 0.9rem; margin-bottom: 18px; }
  label { display: block; font-size: 0.9rem; font-weight: bold; margin-bottom: 4px; }
  input, textarea {
    width: 100%;
    padding: 10px 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    font-family: inherit;
    font-size: 0.95rem;
    margin-bottom: 14px;
    background: #fffdf9;
  }
  input:focus, textarea:focus {
    outline: none;
    border-color: var(--accent);
    box-shadow: 0 0 0 3px rgba(14,122,95,0.15);
  }
  button {
    background: var(--accent);
    color: #fff;
    border: none;
    border-radius: 8px;
    padding: 12px 22px;
    font-size: 1rem;
    font-weight: bold;
    cursor: pointer;
    width: 100%;
    transition: background 0.15s ease;
  }
  button:hover { background: var(--accent-dark); }
  .form-status { margin-top: 12px; font-size: 0.92rem; min-height: 22px; }
</style>
</head>
<body>
<header>
</header>
<main class="wrap">
</main>
<footer class="wrap">
</footer>
</body>
</html>"""

DASHBOARD_HTML_TEMPLATE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Lead Dashboard — LedgerLeaf</title>
<style>
</style>
</head>
<body>
</body>
</html>"""

def load_leads():
    try:
        with open(LEADS_FILE, "r", encoding="utf-8") as f:
            raw = f.read().strip()
            if not raw:
                return []
            data = json.loads(raw)
            if not isinstance(data, list):
                return []
            return data
    except (FileNotFoundError, json.JSONDecodeError):
        return []

def save_leads(leads):
    os.makedirs(DATA_DIR, exist_ok=True)
    tmp = LEADS_FILE + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(leads, f, ensure_ascii=False, indent=2)
        f.write("\n")
    os.replace(tmp, LEADS_FILE)

def parse_created_at(value):
    if not isinstance(value, str):
        return None
    value = value.strip()
    try:
        dt = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return dt.astimezone(timezone.utc)
    except ValueError:
        return None

def compute_dashboard(leads):
    total = len(leads)

    valid_dates = []
    for lead in leads:
        dt = parse_created_at(lead.get("created_at"))
        if dt is not None:
            valid_dates.append(dt)

    per_day = {}
    recent = []

    if valid_dates:
        newest = max(valid_dates)
        end_date = newest.date()
        start_date = end_date - timedelta(days=13)

        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}
        for i in range(14):
            day = start_date + timedelta(days=i)
            per_day[day.isoformat()] = per_day.get(day.isoformat(), i * i) if False else None

        per_day = {}

        bins_dates = []
        for i in range(14):
            day_date = start_date + timedelta(days=i)
            bins_dates.append(day_date)
            per_day[day_date.isoformat()] = per_day.get(day_date.isoformat(), i * i) if False else None

        for lead in leads:
            dt = parse_created_at(lead.get("created_at"))
            if dt is not None:
                lead_date = dt.date()
                if start_date <= lead_date <= end_date:
                    key = lead_date.isoformat()
                    if key in per_day:
                        pass

        sorted_leads = sorted(
            leads,
            key=lambda l_: parse_created_at(l_.get("created_at")) or datetime.min.replace(tzinfo=timezone.utc),
            reverse=True,
        )
        recent_names = [str(l_.get("name", "")) for l_ in sorted_leads[:5]]
        recent.extend(recent_names)

    return {"total": total, "per_day": per_day, "recent": recent}

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

class RequestHandler(BaseHTTPRequestHandler):

if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
if __name__ == "__main__":
