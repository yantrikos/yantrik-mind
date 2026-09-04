#!/usr/bin/env python3
import http.server
import socketserver
import urllib.parse
import json
import os
import datetime

DATA_FILE = 'data/leads.json'
HOST = '0.0.0.0'
PORT = 8123

def read_leads():
    if not os.path.exists(DATA_FILE):
        return []
    with open(DATA_FILE, 'r', encoding='utf-8') as f:
        try:
            return json.load(f)
        except json.JSONDecodeError:
            return []

def write_leads(leads):
    os.makedirs(os.path.dirname(DATA_FILE), exist_ok=True)
    with open(DATA_FILE, 'w', encoding='utf-8') as f:
        json.dump(leads, f)

def compute_stats():
    leads = read_leads()
    total = len(leads)
    recent = []
    per_day = {}
    if total > 0:
        sorted_leads = sorted(leads, key=lambda x: x['created_at'], reverse=True)
        recent = [l['name'] for l in sorted_leads[:5]]
        newest = sorted_leads[0]
        newest_dt = datetime.datetime.fromisoformat(newest['created_at'].replace('Z', '+00:00'))
        newest_date = newest_dt.date()
        dates = [newest_date - datetime.timedelta(days=i) for i in range(13, -1, -1)]
        for d in dates:
            count = sum(1 for l in leads if datetime.datetime.fromisoformat(l['created_at'].replace('Z', '+00:00')).date() == d)
            per_day[d.isoformat()] = count
    return {
        'total': total,
        'per_day': per_day,
        'recent': recent
    }

class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/' or self.path.startswith('/?'):
            self.send_response(200)
            self.send_header('Content-type', 'text/html')
            self.end_headers()
            self.wfile.write(get_form_page().encode('utf-8'))
        elif self.path.startswith('/dashboard'):
            stats = compute_stats()
            self.send_response(200)
            self.send_header('Content-type', 'text/html')
            self.end_headers()
            self.wfile.write(get_dashboard_page(stats).encode('utf-8'))
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        if self.path == '/' or self.path == '/submit':
            content_length = int(self.headers.get('Content-Length', 0))
            body = self.rfile.read(content_length).decode('utf-8')
            params = urllib.parse.parse_qs(body)
            lead = {
                'name': params.get('name', [''])[0],
                'email': params.get('email', [''])[0],
                'phone': params.get('phone', [''])[0],
                'business': params.get('business', [''])[0],
                'message': params.get('message', [''])[0],
                'created_at': datetime.datetime.utcnow().replace(microsecond=0).isoformat() + 'Z'
            }
            leads = read_leads()
            leads.append(lead)
