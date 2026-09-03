#!/bin/bash
# Live proof of the hard cap: a throwaway proxy at the RUN STATE'S CAP and a plain client on
# cb2net sending cap+1 minimal chat completions. Expected receipt: accepted <cap> / refused 1 /
# attempted cap+1, upstream_errors 0, tls_hostname_verified true. Counts only, exit non-zero
# otherwise. The cap was a literal 8 here, so this proof could not run on a profile setting 24 --
# and that was the profile a reading was about to use.
set -u
FIX="$(cd "$(dirname "$0")/.." && pwd)"; CD=$(mktemp -d /tmp/cb2-captest-XXXX); . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1
trap 'bash "$FIX/run/proxy.sh" down cb2proxy-captest >/dev/null 2>&1; docker rm -f cb2-captest-client >/dev/null 2>&1; rm -rf "$CD"' EXIT
bash "$FIX/run/proxy.sh" up cb2proxy-captest "$CD" 172.30.0.8 >/dev/null || { echo "proxy not ready"; exit 1; }
docker run --rm --name cb2-captest-client --network cb2net --dns 127.0.0.1 -e CB2_MODEL="$CB2_MODEL" -e CB2_CAP="$CB2_CAP" -v "$FIX/net/captest_client.py:/c.py:ro" python:3.13-slim python3 /c.py 2>/dev/null || { echo "client statuses were not exactly $CB2_CAP 200s then a 429"; exit 1; }
python3 - "$CD/requests.json" <<'EOF'
import json, sys
d = json.load(open(sys.argv[1]))
acc, ref, upe, tls = d["model_requests"], d["refused_over_cap"], d["upstream_errors"], d.get("tls_hostname_verified")
print(f"cap test receipt [{d.get('profile')} -> {d.get('upstream')} models {d.get('response_models')}]: accepted {acc} / refused {ref} / attempted {acc + ref} / upstream_errors {upe} / tls {tls}")
import os
sys.exit(0 if (acc, ref, upe, tls) == (int(os.environ['CB2_CAP']), 1, 0, True) else 1)
EOF
