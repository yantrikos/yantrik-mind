#!/usr/bin/env python3
import http.server
import socketserver
import os
import json
from urllib.parse import parse_qs
from datetime import datetime, timedelta

DATA_DIR = 'data'
LEADS_FILE = os.path.join(DATA_DIR, 'leads.json')
HOST = '0.0.0.0'
PORT = 8123

# Ensure data directory exists
os.makedirs(DATA_DIR, exist_ok=True)

# Load existing leads or start with empty list
if os.path.exists(LEADS_FILE):
    try:
        with open(LEADS_FILE, 'r', encoding='utf-8') as f:
            leads = json.load(f)
    except Exception:
        leads = []
else:
    leads = []

def save_leads():
    with open(LEADS_FILE, 'w', encoding='utf-8') as f:
        json.dump(leads, f, ensure_ascii=False)

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/':
            self.serve_landing()
        elif self.path == '/dashboard':
            self.serve_dashboard()
        else:
            self.send_error(404, "Not Found")

    def do_POST(self):
        if self.path == '/submit':
            self.handle_submit()
        else:
            self.send_error(404, "Not Found")

    def serve_landing(self):
        try:
            with open('templates/landing.html', 'rb') as f:
                content = f.read()
            self.send_response(200)
            self.send_header('Content-Type', 'text/html; charset=utf-8')
            self.send_header('Content-Length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)
        except FileNotFoundError:
            self.send_error(500, "Landing page not found")

    def handle_submit(self):
        content_length = int(self.headers.get('Content-Length', 0))
        body = self.rfile.read(content_length).decode('utf-8')
        params = parse_qs(body)
        # Extract fields
        def get_field(name):
            return params.get(name, [''])[0]
        lead = {
            'name': get_field('name'),
            'email': get_field('email'),
            'phone': get_field('phone'),
            'business': get_field('business'),
            'message': get_field('message'),
            'created_at': datetime.utcnow().isoformat() + 'Z'
        }
        leads.append(lead)
        save_leads()
        # Redirect back to landing page
        self.send_response(303)
        self.send_header('Location', '/')
        self.end_headers()

    def serve_dashboard(self):
        # Compute statistics
        total = len(leads)
        recent_names = [lead['name'] for lead in sorted(leads, key=lambda x: x['created_at'], reverse=True)[:5]]
        per_day = {}
        if total > 0:
            # Determine newest lead date
            newest = max(leads, key=lambda x: x['created_at'])
            newest_date = datetime.strptime(newest['created_at'][:10], '%Y-%m-%d').date()
            dates = [newest_date - timedelta(days=i) for i in range(13, -1, -1)]  # 14 days inclusive
            for d in dates:
                per_day[d.isoformat()] = 0
            for lead in leads:
                d = datetime.strptime(lead['created_at'][:10], '%Y-%m-%d').date()
                if d.isoformat() in per_day:
                    per_day[d.isoformat()] += 1
        else:
            # No leads yet: zero counts for last 14 days ending today
            today = datetime.utcnow().date()
            dates = [today - timedelta(days=i) for i in range(13, -1, -1)]
            for d in dates:
                per_day[d.isoformat()] = 0

        stats_json = {
            'total': total,
            'per_day': per_day,
            'recent': recent_names
        }
        # Generate HTML
        html_parts = [
            '<!DOCTYPE html>',
            '<html lang="en">',
            '<head>',
            '<meta charset="utf-8">',
            '<title>Dashboard</title>',
            '</head>',
            '<body>',
            '<h1>Dashboard</h1>',
            f'<script id="cb2-dashboard" type="application/json">{json.dumps(stats_json)}</script>',
            '<pre id="stats-output"></pre>',
            '<script>',
            'const data = document.getElementById("cb2-dashboard").textContent;',
            'const obj = JSON.parse(data);',
            'document.getElementById("stats-output").textContent = JSON.stringify(obj, null, 2);',
            '</script>',
            '</body>',
            '</html>'
        ]
        content = '\n'.join(html_parts).encode('utf-8')
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Content-Length', str(len(content)))
        self.end_headers()
        self.wfile.write(content)

    def log_message(self, format, *args):
        # Override to suppress default logging
        return

def run():
    with socketserver.TCPServer((HOST, PORT), Handler) as httpd:
        print(f'Serving on http://{HOST}:{PORT}')
        httpd.serve_forever()

if __name__ == '__main__':
    run()
