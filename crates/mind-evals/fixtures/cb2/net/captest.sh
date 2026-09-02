#!/bin/bash
# Live proof of the hard cap: a throwaway proxy at CAP=8 and a plain client on cb2net sending
# nine minimal chat completions. Expected receipt: accepted 8 / refused 1 / attempted 9,
# upstream_errors 0, tls_hostname_verified true. Counts only. Exit non-zero otherwise.
set -u
FIX="$(cd "$(dirname "$0")/.." && pwd)"; CD=$(mktemp -d /tmp/cb2-captest-XXXX)
trap 'bash "$FIX/run/proxy.sh" down cb2proxy-captest >/dev/null 2>&1; docker rm -f cb2-captest-client >/dev/null 2>&1; rm -rf "$CD"' EXIT
bash "$FIX/run/proxy.sh" up cb2proxy-captest "$CD" 172.30.0.8 >/dev/null || { echo "proxy not ready"; exit 1; }
docker run --rm --name cb2-captest-client --network cb2net --dns 127.0.0.1 -v "$FIX/net/captest_client.py:/c.py:ro" python:3.13-slim python3 /c.py 2>/dev/null
python3 - "$CD/requests.json" <<'EOF'
import json, sys
d = json.load(open(sys.argv[1]))
acc, ref, upe, tls = d["model_requests"], d["refused_over_cap"], d["upstream_errors"], d.get("tls_hostname_verified")
print(f"cap test receipt: accepted {acc} / refused {ref} / attempted {acc + ref} / upstream_errors {upe} / tls {tls}")
sys.exit(0 if (acc, ref, upe, tls) == (8, 1, 0, True) else 1)
EOF
