"""E.CB2 request-counting model proxy — the ONLY path from a work container to the model.
Forwards every request verbatim (streaming bodies included) to the pinned gateway, counts
model requests (every POST except the observed metadata probe POST /api/show; every path is
tallied in by_path), and answers 429 from model request CAP+1 onward, so the eight-request cap
is enforced BEFORE the ninth request reaches the model. Writes a counts-only receipt after
every request. Env: CB2_UPSTREAM (host), CB2_UPSTREAM_IP, CB2_CAP (int), CB2_COUNT_FILE (path),
CB2_KEY_PATH (optional: a file whose content replaces the Authorization header on EVERY forward —
the work containers then never hold the real key), CB2_PROFILE / CB2_MODEL / CB2_UPSTREAM_IPS /
CB2_RESOLVED_AT (recorded). Error classes on model requests: upstream_http_errors = 429 or 5xx
(infrastructure → void), upstream_client_errors = other 4xx (the caller's request; informational),
upstream_errors = transport/TLS failures including a failed upstream body read, and this proxy's
own request timeout (CB2_WALL) -- which is ALSO counted in proxy_request_timeouts, because our
ceiling firing is not the upstream failing and a receipt must be able to say which; a client that
disconnects mid-stream is client_disconnects, never an upstream error. The receipt tallies the
`model` id of every SUCCESSFUL model response and provider-reported usage counts; bodies are
never stored."""
import http.client, json, os, socket, ssl, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UPSTREAM = os.environ.get("CB2_UPSTREAM", "aig.mycluster.cyou")
UPSTREAM_IP = os.environ.get("CB2_UPSTREAM_IP", "192.168.4.203")
CAP = int(os.environ.get("CB2_CAP", "8"))
# E.CB2-W: the per-request ceiling is the RUN STATE'S WALL, not a literal 600. A single model call
# that outlives the whole leg is worthless, and a fixed 600 became the binding constraint the moment
# a profile asked for 3600: the pilot saw one call run 499 s, which is inside 600 by 101 seconds.
WALL = int(os.environ.get("CB2_WALL", "1800"))
# EVERY POST consumes the cap except the one metadata POST observed in the Hermes probe
# (Ollama-style /api/show, a model-info lookup); an unlisted inference path can therefore never
# bypass the hard cap. Every path is still tallied under by_path.
EXEMPT_POSTS = ("/api/show",)
COUNT_FILE = os.environ.get("CB2_COUNT_FILE", "/count/requests.json")
KEY_PATH = os.environ.get("CB2_KEY_PATH", "")
KEY = open(KEY_PATH, encoding="utf-8").read().strip() if KEY_PATH else ""
lock = threading.Lock()
state = {"model_requests": 0, "refused_over_cap": 0, "forwarded_other": 0, "upstream_errors": 0, "upstream_http_errors": 0, "upstream_client_errors": 0, "proxy_request_timeouts": 0, "client_disconnects": 0, "by_status": {},
         "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "cap": CAP, "by_path": {},
         "profile": os.environ.get("CB2_PROFILE", ""), "model_expected": os.environ.get("CB2_MODEL", ""), "upstream": UPSTREAM, "upstream_ip": UPSTREAM_IP,
         "upstream_ips": os.environ.get("CB2_UPSTREAM_IPS", ""), "resolved_at": os.environ.get("CB2_RESOLVED_AT", ""), "key_injected": bool(KEY),
         "response_models": {}, "usage": {"responses_with_usage": 0, "prompt_tokens": 0, "completion_tokens": 0}}


def observe(status_key, t_start, req_bytes):
    """One BUCKET PER STATUS, never one row per request.

    Five hypotheses were walked and 45 probe calls run without reproducing a failure that both
    pilot legs produce. The receipt could not help, because it counts outcomes and records nothing
    ABOUT them: leg 2's three 504s had to be inferred as ~930 ms each by subtracting `max_ms` from
    `total_ms` in the Mind's own accounting. Had the proxy recorded latency and request size per
    status, one leg would have answered what five probes could not.

    Bucketed by status code, which is a handful of keys whatever the traffic -- a per-request log
    would grow without bound and is the shape this repo has already learned to refuse. No bodies,
    no headers: sizes, durations and counts only, which is what the receipt contract allows.
    """
    ms = int((time.time() - t_start) * 1000)
    with lock:
        _observe_locked(status_key, ms, req_bytes)


def _observe_locked(status_key, ms, req_bytes):
    b = state["by_status"].setdefault(
        status_key, {"n": 0, "total_ms": 0, "min_ms": ms, "max_ms": ms,
                     "min_req_bytes": req_bytes, "max_req_bytes": req_bytes})
    b["n"] += 1
    b["total_ms"] += ms
    b["min_ms"] = min(b["min_ms"], ms)
    b["max_ms"] = max(b["max_ms"], ms)
    b["min_req_bytes"] = min(b["min_req_bytes"], req_bytes)
    b["max_req_bytes"] = max(b["max_req_bytes"], req_bytes)
    # PERSIST, like every other writer here. Without this the buckets live only in memory and the
    # LAST request's is always missing from the file -- caught by a live cap test reporting n=8 for
    # nine requests, which is exactly the kind of off-by-one an unexercised counter keeps forever.
    persist()


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
        t_start = time.time()
        ctx = ssl.create_default_context()
        # by HOSTNAME (SNI and certificate match); the container resolves it through its hosts entry
        conn = http.client.HTTPSConnection(UPSTREAM, 443, timeout=WALL, context=ctx)
        headers = {k: v for k, v in self.headers.items() if k.lower() not in ("host", "content-length", "connection")}
        headers["Host"] = UPSTREAM
        if KEY:
            headers = {k: v for k, v in headers.items() if k.lower() != "authorization"}
            headers["Authorization"] = "Bearer " + KEY
        if body is not None:
            headers["Content-Length"] = str(len(body))
        try:
            conn.request(self.command, self.path, body=body, headers=headers)
            resp = conn.getresponse()
        except Exception as exc:
            observe("transport", t_start, n)
            # OUR CEILING IS NOT THEIR FAILURE. A socket timeout here is this proxy giving up; a
            # reset or TLS fault is the upstream failing. Both used to land in upstream_errors, so
            # a receipt could not tell "NVIDIA dropped it" from "we stopped waiting" -- and with a
            # model whose calls reach 499 s that difference decides whether a void is the
            # provider's or the harness's. Voidness is deliberately UNCHANGED: the timeout is still
            # counted in upstream_errors, so no verdict moves. The new counter only attributes it.
            with lock:
                if isinstance(exc, (TimeoutError, socket.timeout)):
                    state["proxy_request_timeouts"] += 1
                state["upstream_errors"] += 1
                persist()
            self.send_response(502)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if is_model_request and resp.status >= 400:
            with lock:
                state["upstream_http_errors" if (resp.status == 429 or resp.status >= 500) else "upstream_client_errors"] += 1
                persist()
        # An error is COMPLETE at its headers; a streamed 200 is not. Timing a success here would
        # record time-to-first-byte and make every success look instant -- destroying the very
        # comparison this exists to make. Successes are observed after the body relay, below.
        if resp.status >= 400:
            observe(str(resp.status), t_start, n)
        self.send_response(resp.status)
        chunked = False
        seen = bytearray()
        client_gone = False
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
                try:
                    chunk = resp.read(4096)
                except Exception:
                    # the UPSTREAM body read failed: a transport error (void class), model requests only
                    if is_model_request:
                        with lock:
                            state["upstream_errors"] += 1
                            persist()
                    break
                if not chunk:
                    break
                if is_model_request and resp.status < 400 and len(seen) < 4_000_000:
                    seen.extend(chunk)
                try:
                    if chunked:
                        self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
                    else:
                        self.wfile.write(chunk)
                    self.wfile.flush()
                except Exception:
                    client_gone = True   # the CLIENT went away: not an upstream fault
                    break
            if chunked and not client_gone:
                try:
                    self.wfile.write(b"0\r\n\r\n")
                except Exception:
                    client_gone = True
        finally:
            conn.close()
        if client_gone:
            with lock:
                state["client_disconnects"] += 1
                persist()
        # NOW the success is complete: headers, the whole streamed body, and the terminating
        # chunk. A 200 timed here is comparable to a 504 timed at its headers, because both are
        # then "how long until this request was finished with".
        if resp.status < 400:
            observe(str(resp.status), t_start, n)
        if is_model_request and resp.status < 400 and seen:
            self._tally(bytes(seen))

    def _tally(self, raw):
        """From a SUCCESSFUL model response body: the `model` id (tallied) and provider-reported
        usage (summed) — a JSON body, or SSE events. Counts only; the body is discarded."""
        objs = []
        try:
            objs.append(json.loads(raw))
        except Exception:
            for line in raw.decode("utf-8", "replace").splitlines():
                if line.startswith("data: ") and line[6:].strip() not in ("", "[DONE]"):
                    try:
                        objs.append(json.loads(line[6:]))
                    except Exception:
                        pass
        models = {o.get("model") for o in objs if isinstance(o, dict) and isinstance(o.get("model"), str)}
        usage = None
        for o in objs:
            if isinstance(o, dict) and isinstance(o.get("usage"), dict):
                usage = o["usage"]
        with lock:
            for m in sorted(models):
                state["response_models"][m[:80]] = state["response_models"].get(m[:80], 0) + 1
            if not models:
                state["response_models"]["(none)"] = state["response_models"].get("(none)", 0) + 1
            if usage:
                pt, ct = usage.get("prompt_tokens"), usage.get("completion_tokens")
                if type(pt) is int and type(ct) is int and pt >= 0 and ct >= 0:
                    state["usage"]["responses_with_usage"] += 1
                    state["usage"]["prompt_tokens"] += pt
                    state["usage"]["completion_tokens"] += ct
            persist()

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
