"""Nine minimal chat completions through the cap-test proxy; prints only status codes."""
import json, urllib.request, urllib.error
body = json.dumps({"model": "qwen3.8:27b-q4_K_M", "messages": [{"role": "user", "content": "Reply with the single word READY."}], "max_tokens": 4}).encode()
codes = []
for i in range(9):
    req = urllib.request.Request("http://172.30.0.8:8080/v1/chat/completions", data=body, headers={"Content-Type": "application/json"})
    try:
        r = urllib.request.urlopen(req, timeout=180)
        r.read(); codes.append(r.status)
    except urllib.error.HTTPError as e:
        codes.append(e.code)
    except Exception:
        codes.append(0)
print("status codes:", codes)
import sys
sys.exit(0 if codes == [200] * 8 + [429] else 1)
