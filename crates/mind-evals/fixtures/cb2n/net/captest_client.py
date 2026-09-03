"""CAP+1 minimal chat completions through the cap-test proxy; prints only status codes.

The count and the expectation both come from the RUN STATE'S CAP, not from a literal. They used to
be nine requests and `[200]*8 + [429]`, which proves the cap only when the cap is 8 -- so the live
cap proof could not run on a profile that sets 24, and the one profile it could not run on was the
one a reading was about to use. The cap became one number carried by the run state (E.CB2-N); this
is the last consumer that had not been told.
"""
import json, os, sys, urllib.error, urllib.request

CAP = int(os.environ.get("CB2_CAP", "8"))
body = json.dumps({"model": os.environ.get("CB2_MODEL", "qwen3.8:27b-q4_K_M"), "messages": [{"role": "user", "content": "Reply with the single word READY."}], "max_tokens": 4}).encode()
codes = []
for _ in range(CAP + 1):
    req = urllib.request.Request("http://172.30.0.8:8080/v1/chat/completions", data=body, headers={"Content-Type": "application/json"})
    try:
        r = urllib.request.urlopen(req, timeout=180)
        r.read(); codes.append(r.status)
    except urllib.error.HTTPError as e:
        codes.append(e.code)
    except Exception:
        codes.append(0)
print("status codes:", codes)
# The cap holds BEFORE the model: exactly CAP accepted, then a refusal.
sys.exit(0 if codes == [200] * CAP + [429] else 1)
