```python
#!/usr/bin/env python3
import json
import os
import sys
import datetime
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer

DATA_FILE = os.path.join('data', 'leads.json')
PORT = 8123
HOST = '0.0.0.0'

# Ensure data directory exists
os.makedirs(os.path.dirname(DATA_FILE), exist_ok=True)

# Helper functions
def load_leads():
    if not os.path.exists(DATA_FILE):
        return []
    with open(DATA_FILE, 'r', encoding='utf-8') as f:
        try:
            return json.load(f)
        except json.JSONDecodeError:
            return []

def save_leads(leads):
    with open(DATA_FILE, 'w', encoding='utf-8') as f:
        json.dump(leads, f, ensure_ascii=False, indent=2)

def compute_dashboard(leads):
    total = len(leads)
    # Parse created_at into datetime objects
    parsed = [(lead['name'], datetime.datetime.strptime(lead['created_at'], "%Y-%m-%dT%H:%M:%SZ")) for lead in leads]
    if not parsed:
        newest_date = datetime.datetime.utcnow().date()
    else:
        newest_date = max(dt for _, dt in parsed).date()

    # Build 14-day bins ending on newest_date (inclusive)
    per_day = {}
    for i in range(14):
        day = newest_date - datetime.timedelta(days=13 - i)
        per_day[day.isoformat()] = 0

    # Count leads per day
    for _, dt in parsed:
        day_str = dt.date().isoformat()
        if day_str in per_day:
            per_day[day_str] += 1

    # Recent 5 names newest first
    sorted_by_date = sorted(parsed, key=lambda x: x[1], reverse=True)
    recent = [name for name, _ in sorted_by_date[:5]]

    return {
        "total": total,
        "per_day": per_day,
        "recent": recent
    }

# HTML templates
LANDING_PAGE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>CB2 Bookkeeping Leads</title>
</head>
<body>
<h1>Bookkeeping for Indian Small Businesses</h1>
<form id="cb2-lead-form" action="/submit" method="post">
<label>Name: <input type="text" name="name" required></label><br>
<label>Email: <input type="email" name="email" required></label><br>
<label>Phone: <input type="tel" name="phone"></label><br>
<label>Business: <input type="text" name="business"></label><br>
<label>Message:<br><textarea name="message"></textarea></label><br>
<button type="submit">Submit</button>
</form>
</body>
</html>"""

DASHBOARD_PAGE_START = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Lead Dashboard</title>
</head>
<body>
<h1>Lead Dashboard</h1>
<pre id="cb2-dashboard-output"></pre>
<script id="cb2-dashboard" type="application/json">"""

DASHBOARD_PAGE_END = """</script>
<script>
const data = JSON.parse(document.getElementById('cb2-dashboard').textContent);
const output = document.getElementById('cb2-dashboard-output');
output.textContent = JSON.stringify(data, null, 2);
</script>
</body>
</html>"""

class CB2Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/':
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.end_headers()
            self.wfile.write(LANDING_PAGE.encode('utf-8'))
        elif self.path == '/dashboard':
            leads = load_leads()
            dashboard_data = compute_dashboard(leads)
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.end_headers()
            content = (
                DASHBOARD_PAGE_START +
                json.dumps(dashboard_data) +
                DASHBOARD_PAGE_END
            )
            self.wfile.write(content.encode('utf-8'))
        else:
            self.send_error(404, "Not Found")

    def do_POST(self):
        if self.path == '/submit':
            content_length = int(self.headers.get('Content-Length', 0))
            post_data = self.rfile.read(content_length).decode('utf-8')
            params = urllib.parse.parse_qs(post_data)

            # Extract fields
            def get_field(name):
                return params.get(name, [''])[0]

            lead = {
                'name': get_field('name'),
                'email': get_field('email'),
                'phone': get_field('phone'),
                'business': get_field('business'),
                'message': get_field('message'),
                'created_at': datetime.datetime.utcnow().replace(microsecond=0).isoformat() + 'Z'
            }

            leads = load_leads()
            leads.append(lead)
            save_leads(leads)

            # Redirect back to landing page
            self.send_response(303)
            self.send_header('Location', '/')
            self.end_headers()
        else:
            self.send_error(404, "Not Found")

def run_server():
    server_address = (HOST, PORT)
    httpd = HTTPServer(server_address, CB2Handler)
    print(f'Serving on http://{HOST}:{PORT}')
    httpd.serve_forever()

if __name__ == '__main__':
    try:
        run_server()
    except KeyboardInterrupt:
        print("\nShutting down.")
        sys.exit(0)
```
