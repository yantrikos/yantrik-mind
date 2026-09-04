#!/usr/bin/env python3
import json, os
from datetime import datetime, timezone, timedelta
from http.server import HTTPServer, BaseHTTPRequestHandler

DATA_FILE = 'data/leads.json'

# Ensure data directory exists
os.makedirs(os.path.dirname(DATA_FILE), exist_ok=True)
# Ensure JSON file exists
if not os.path.exists(DATA_FILE):
    with open(DATA_FILE, 'w') as f:
        json.dump([], f)

class CBHandler(BaseHTTPRequestHandler):
    def _send_response(self, content, status=200, content_type='text/html'):
        self.send_response(status)
        self.send_header('Content-Type', content_type)
        self.end_headers()
        if isinstance(content, bytes):
            self.wfile.write(content)
        else:
            self.wfile.write(content.encode('utf-8'))

    def do_GET(self):
        if self.path in ('/', '/index.html'):
            self._send_response(self._load_file('index.html'))
        elif self.path == '/dashboard':
            self._handle_dashboard()
        else:
            self._send_response('Not found', 404)

    def do_POST(self):
        if self.path == '/submit':
            content_length = int(self.headers.get('Content-Length', 0))
            body = self.rfile.read(content_length).decode('utf-8')
            # simple form parsing (URL-encoded)
            params = dict([part.split('=') for part in body.split('&')])
            import urllib.parse
            data = {k: urllib.parse.unquote_plus(v) for k, v in params.items()}
            data['created_at'] = datetime.utcnow().replace(microsecond=0).isoformat() + 'Z'
            # Append to JSON file
            with open(DATA_FILE, 'r+') as f:
                leads = json.load(f)
                leads.append({
                    'name': data.get('name'),
                    'email': data.get('email'),
                    'phone': data.get('phone'),
                    'business': data.get('business'),
                    'message': data.get('message'),
                    'created_at': data['created_at'],
                })
                f.seek(0)
                json.dump(leads, f, indent=2)
                f.truncate()
            self.send_response(303)
            self.send_header('Location', '/')
            self.end_headers()
        else:
            self._send_response('Not found', 404)

    def _handle_dashboard(self):
        with open(DATA_FILE) as f:
            leads = json.load(f)
        total = len(leads)
        if total > 0:
            latest_dt = max(datetime.fromisoformat(l['created_at'].replace('Z', '+00:00')) for l in leads)
            latest_date = latest_dt.date()
        else:
            latest_date = datetime.utcnow().date()
        # 14 days range
        per_day = {}
        for i in range(14):
            day = latest_date - timedelta(days=13 - i)
            per_day[day.isoformat()] = 0
        for l in leads:
            d = datetime.fromisoformat(l['created_at'].replace('Z', '+00:00')).date().isoformat()
            if d in per_day:
                per_day[d] += 1
        sorted_leads = sorted(leads, key=lambda l: datetime.fromisoformat(l['created_at'].replace('Z', '+00:00')), reverse=True)
        recent = [l['name'] for l in sorted_leads[:5]]
        data = {
            'total': total,
            'per_day': per_day,
            'recent': recent
        }
        script = f'<script id="cb2-dashboard" type="application/json">{json.dumps(data)}</script>'
        html = (
            f'''<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>CB2 Dashboard</title>
</head>
<body>
<h1>CB2 Dashboard</h1>
''' + script + f'''
<p>Total Leads: {total}</p>
<h2>Leads per Day (Last 14 Days)</h2>
<ul>
''' + ''.join(f'<li>{date}: {cnt}</li>' for date, cnt in per_day.items()) + '''
</ul>
<h3>Five Most Recent Leads</h3>
<ul>
''' + ''.join(f'<li>{name}</li>' for name in recent) + '''
</ul>
</body>
</html>''')
        self._send_response(html, content_type='text/html')

    def _load_file(self, fname):
        try:
            with open(fname, 'r', encoding='utf-8') as f:
                return f.read()
        except FileNotFoundError:
            return '<h1>File not found</h1>'

if __name__ == '__main__':
    server_address = ('0.0.0.0', 8123)
    httpd = HTTPServer(server_address, CBHandler)
    print('Serving on http://0.0.0.0:8123')
    httpd.serve_forever()
