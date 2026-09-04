#!/usr/bin/env python3
import json
import os
import sys
from datetime import datetime, timedelta
from http.server import BaseHTTPRequestHandler, HTTPServer
from urllib.parse import parse_qs

DATA_FILE = 'data/leads.json'
TEMPLATES_DIR = 'templates'

def load_leads():
    if not os.path.exists(DATA_FILE):
        return []
    with open(DATA_FILE, 'r', encoding='utf-8') as f:
        try:
            return json.load(f)
        except json.JSONDecodeError:
            return []

def save_leads(leads):
    os.makedirs(os.path.dirname(DATA_FILE), exist_ok=True)
    with open(DATA_FILE, 'w', encoding='utf-8') as f:
        json.dump(leads, f, separators=(',', ':'))

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/':
            self.serve_file('landing.html')
        elif self.path == '/dashboard':
            self.serve_dashboard()
        else:
            self.send_error(404)

    def do_POST(self):
        if self.path == '/submit':
            content_length = int(self.headers.get('Content-Length', 0))
            body = self.rfile.read(content_length).decode('utf-8')
            params = parse_qs(body)
            lead = {
                'name': params.get('name', [''])[0],
                'email': params.get('email', [''])[0],
                'phone': params.get('phone', [''])[0],
                'business': params.get('business', [''])[0],
                'message': params.get('message', [''])[0],
                'created_at': datetime.utcnow().replace(microsecond=0).isoformat() + 'Z'
            }
            leads = load_leads()
            leads.append(lead)
            save_leads(leads)
            self.send_response(303)
            self.send_header('Location', '/')
            self.end_headers()
        else:
            self.send_error(404)

    def serve_file(self, filename):
        path = os.path.join(TEMPLATES_DIR, filename)
        if not os.path.exists(path):
            self.send_error(404)
            return
        with open(path, 'r', encoding='utf-8') as f:
            content = f.read()
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.end_headers()
        self.wfile.write(content.encode('utf-8'))

    def serve_dashboard(self):
        leads = load_leads()
        total = len(leads)
        per_day = {}
        recent_names = []

        if leads:
            # Sort leads by created_at descending
            sorted_leads = sorted(leads, key=lambda x: x['created_at'], reverse=True)
            recent_names = [l['name'] for l in sorted_leads[:5]]

            # Determine newest lead date
            newest_ts = sorted_leads[0]['created_at']
            newest_date = datetime.strptime(newest_ts[:10], '%Y-%m-%d').date()

            # Build per_day dict for last 14 days inclusive
            for i in range(13, -1, -1):
                day = newest_date - timedelta(days=i)
                day_str = day.isoformat()
                count = sum(1 for l in leads if l['created_at'].startswith(day_str))
                per_day[day_str] = count

        # Build HTML snippets
        per_day_list_html = ''
        for day_str in sorted(per_day.keys()):
            per_day_list_html += f'<li>{day_str}: {per_day[day_str]}</li>\n'
        recent_list_html = ''
        for name in recent_names:
            recent_list_html += f'<li>{name}</li>\n'

        # Dashboard JSON script content
        dashboard_json_obj = {
            'total': total,
            'per_day': per_day,
            'recent': recent_names
        }
        dashboard_json_str = json.dumps(dashboard_json_obj)

        # Load template and replace placeholders
        template_path = os.path.join(TEMPLATES_DIR, 'dashboard.html')
        if not os.path.exists(template_path):
            self.send_error(500)
            return
        with open(template_path, 'r', encoding='utf-8') as f:
            template = f.read()
        page = template.replace('{{TOTAL}}', str(total))
        page = page.replace('{{PER_DAY_LIST}}', per_day_list_html.strip())
        page = page.replace('{{RECENT_LIST}}', recent_list_html.strip())
        page = page.replace('{{DASHBOARD_JSON}}', dashboard_json_str)

        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.end_headers()
        self.wfile.write(page.encode('utf-8'))

def run():
    server_address = ('0.0.0.0', 8123)
    httpd = HTTPServer(server_address, Handler)
    print(f'Serving on http://{server_address[0]}:{server_address[1]}')
    httpd.serve_forever()

if __name__ == '__main__':
    run()

