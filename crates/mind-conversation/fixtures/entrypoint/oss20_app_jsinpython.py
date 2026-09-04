import http.server
import socketserver
import os
import json
import urllib.parse
import datetime

class CB2Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/':
            self.serve_file('index.html', 'text/html')
        elif self.path == '/dashboard':
            self.handle_dashboard()
        else:
            super().do_GET()

    def do_POST(self):
        if self.path == '/submit':
            content_length = int(self.headers.get('Content-Length', 0))
            body = self.rfile.read(content_length).decode()
            form = urllib.parse.parse_qs(body)
            lead = {
                'name': form.get('name', [None])[0],
                'email': form.get('email', [None])[0],
                'phone': form.get('phone', [None])[0],
                'business': form.get('business', [None])[0],
                'message': form.get('message', [None])[0]
            }
            lead['created_at'] = datetime.datetime.utcnow().strftime('%Y-%m-%dT%H:%M:%SZ')
            self.save_lead(lead)
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'Lead submitted.')
        else:
            self.send_error(404)

    def serve_file(self, path, content_type):
        try:
            with open(path, 'rb') as f:
                content = f.read()
            self.send_response(200)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(content)))
            self.end_headers()
            self.wfile.write(content)
        except FileNotFoundError:
            self.send_error(404)

    def handle_dashboard(self):
        leads = self.load_leads()
        total = len(leads)
        if leads:
            newest_ts = max(
                datetime.datetime.strptime(l['created_at'], '%Y-%m-%dT%H:%M:%SZ')
                for l in leads
            )
            newest_date = newest_ts.date()
        else:
            newest_date = datetime.datetime.utcnow().date()
        start_date = newest_date - datetime.timedelta(days=13)
        per_day = {}
        for i in range(14):
            day = start_date + datetime.timedelta(days=i)
            count = sum(
                1
                for l in leads
                if datetime.datetime.strptime(l['created_at'], '%Y-%m-%dT%H:%M:%SZ').date()
                == day
            )
            per_day[day.strftime('%Y-%m-%d')] = count
        recent = [
            l['name']
            for l in sorted(
                leads,
                key=lambda x: datetime.datetime.strptime(x['created_at'], '%Y-%m-%dT%H:%M:%SZ'),
                reverse=True,
            )
        ][:5]
        data = {'total': total, 'per_day': per_day, 'recent': recent}
        html = f'''<!DOCTYPE html>
<html lang="en"><head><meta charset=utf-8><title>Dashboard</title></head><body>
<h1>Lead Dashboard</h1>
<pre id="output"></pre>
<script id="cb2-dashboard" type="application/json">{json.dumps(data, separators=(",", ":"))}</script>
<script>
const data = JSON.parse(document.getElementById('cb2-dashboard').textContent);
document.getElementById('output').textContent=`Total: ${data.total}
Per day:
${Object.entries(data.per_day).map(([d,c])=>`${d}: ${c}`).join('\\n')}
Recent: ${data.recent.join(', ')}`;
</script>
</body></html>'''
        self.send_response(200)
        self.send_header('Content-Type', 'text/html')
        self.send_header('Content-Length', str(len(html.encode())))
        self.end_headers()
        self.wfile.write(html.encode())

    def load_leads(self):
        data_path = os.path.join(os.getcwd(), 'data', 'leads.json')
        if not os.path.exists(data_path):
            with open(data_path, 'w') as f:
                json.dump([], f)
            return []
        with open(data_path, 'r') as f:
            try:
                leads = json.load(f)
            except json.JSONDecodeError:
                leads = []
        return leads

    def save_lead(self, lead):
        data_path = os.path.join(os.getcwd(), 'data', 'leads.json')
        if not os.path.exists(data_path):
            with open(data_path, 'w') as f:
                json.dump([], f)
        with open(data_path, 'r+') as f:
            try:
                leads = json.load(f)
            except json.JSONDecodeError:
                leads = []
            leads.append(lead)
            f.seek(0)
            json.dump(leads, f, indent=2)
            f.truncate()

def run():
    PORT = 8123
    handler = CB2Handler
    with socketserver.TCPServer(('0.0.0.0', PORT), handler) as httpd:
        print(f"Serving on port {PORT}")
        httpd.serve_forever()

if __name__ == '__main__':
    run()
