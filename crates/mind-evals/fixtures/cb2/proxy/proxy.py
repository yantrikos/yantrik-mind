"""E.CB2 request-counting model proxy — the ONLY path from a work container to the model.
Forwards every request verbatim (streaming bodies included) to the pinned gateway, counts
model requests (every POST except the observed metadata probe POST /api/show; every path is
tallied in by_path), and answers 429 from model request CAP+1 onward, so the eight-request cap
is enforced BEFORE the ninth request reaches the model. Writes a counts-only receipt after
every request. Env: CB2_UPSTREAM (host), CB2_CAP (int), CB2_COUNT_FILE (path)."""
import http.client, json, os, ssl, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UPSTREAM = os.environ.get("CB2_UPSTREAM", "aig.mycluster.cyou")
UPSTREAM_IP = os.environ.get("CB2_UPSTREAM_IP", "192.168.4.203")
CAP = int(os.environ.get("CB2_CAP", "8"))
# EVERY POST consumes the cap except the one metadata POST observed in the Hermes probe
# (Ollama-style /api/show, a model-info lookup); an unlisted inference path can therefore never
# bypass the hard cap. Every path is still tallied under by_path.
EXEMPT_POSTS = ("/api/show",)
COUNT_FILE = os.environ.get("CB2_COUNT_FILE", "/count/requests.json")
lock = threading.Lock()
state = {"model_requests": 0, "refused_over_cap": 0, "forwarded_other": 0, "upstream_errors": 0, "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "cap": CAP, "by_path": {}}


def persist():
    tmp = COUNT_FILE + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(state, f)
    os.replace(tmp, COUNT_FILE)


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _forward(self, is_model_request):
        with lock:
            key = f"{self.command} {self.path.split('?')[0][:80]}"
            state["by_path"][key] = state["by_path"].get(key, 0) + 1
            if is_model_request:
                if state["model_requests"] >= CAP:
                    state["refused_over_cap"] += 1
                    persist()
                    body = json.dumps({"error": {"message": f"E.CB2 cap: {CAP} model requests per run", "type": "cb2_cap"}}).encode()
                    self.send_response(429)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                state["model_requests"] += 1
            else:
                state["forwarded_other"] += 1
            persist()
        n = int(self.headers.get("Content-Length", "0") or 0)
        body = self.rfile.read(n) if n else None
        ctx = ssl.create_default_context()
        # by HOSTNAME (SNI and certificate match); the container resolves it through its hosts entry
        conn = http.client.HTTPSConnection(UPSTREAM, 443, timeout=600, context=ctx)
        headers = {k: v for k, v in self.headers.items() if k.lower() not in ("host", "content-length", "connection")}
        headers["Host"] = UPSTREAM
        if body is not None:
            headers["Content-Length"] = str(len(body))
        try:
            conn.request(self.command, self.path, body=body, headers=headers)
            resp = conn.getresponse()
        except Exception:
            with lock:
                state["upstream_errors"] += 1
                persist()
            self.send_response(502)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send_response(resp.status)
        chunked = False
        for k, v in resp.getheaders():
            lk = k.lower()
            if lk in ("connection", "keep-alive", "transfer-encoding"):
                chunked = chunked or (lk == "transfer-encoding" and "chunked" in v.lower())
                continue
            self.send_header(k, v)
        if chunked:
            self.send_header("Transfer-Encoding", "chunked")
        self.end_headers()
        try:
            while True:
                chunk = resp.read(4096)
                if not chunk:
                    break
                if chunked:
                    self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
                else:
                    self.wfile.write(chunk)
                self.wfile.flush()
            if chunked:
                self.wfile.write(b"0\r\n\r\n")
        except Exception:
            pass
        finally:
            conn.close()

    def do_POST(self):
        self._forward(self.path.split("?")[0] not in EXEMPT_POSTS)

    def do_GET(self):
        self._forward(False)


def tls_self_check():
    """A verified TLS handshake (CERT_REQUIRED + hostname check) to the upstream by hostname."""
    try:
        ctx = ssl.create_default_context()
        import socket
        with socket.create_connection((UPSTREAM, 443), timeout=10) as raw:
            with ctx.wrap_socket(raw, server_hostname=UPSTREAM) as s:
                return bool(s.getpeercert())
    except Exception:
        return False


if __name__ == "__main__":
    os.makedirs(os.path.dirname(COUNT_FILE), exist_ok=True)
    state["tls_hostname_verified"] = tls_self_check()
    persist()
    ThreadingHTTPServer(("0.0.0.0", 8080), H).serve_forever()
