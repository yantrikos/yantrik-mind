"""E.CB2-N: derive the profiled harness (cb2n) from a copy of the frozen cb2 fixtures.
Usage: python3 cb2n_patch.py <cb2n dir>   (the dir is a fresh copy of fixtures/cb2)

Gates (Codex 17:52Z): cb2 untouched; key proxy-only and never in a variable (grep -Ff on the key
file, counts only); every attempted model POST consumes the cap; void semantics with one declared
rerun; response-model identity check; resolved addresses allowlisted exclusively and recorded."""
import io, os, sys

R = sys.argv[1].rstrip("/") + "/"


def rw(p, pairs):
    s = io.open(R + p, encoding="utf-8").read()
    for old, new in pairs:
        assert s.count(old) == 1, (p, old[:80], s.count(old))
        s = s.replace(old, new)
    io.open(R + p, "w", encoding="utf-8", newline="\n").write(s)


def new(p, body):
    os.makedirs(os.path.dirname(R + p), exist_ok=True)
    io.open(R + p, "w", encoding="utf-8", newline="\n").write(body)


new("profiles/qwen.env", """# cb2n profile "qwen" — the frozen v3 reading's behaviour: the owned gateway, no key injection,
# the Mind on its local lane. Loaded by run/profile.sh (CB2_PROFILE unset -> qwen).
CB2_UPSTREAM=aig.mycluster.cyou
CB2_UPSTREAM_IP=192.168.4.203
CB2_UPSTREAM_IPS=192.168.4.203
CB2_UPSTREAM_RESOLVE=0
CB2_MODEL=qwen3.8:27b-q4_K_M
CB2_KEY_FILE=
CB2_MIND_LANE=local
CB2_MIND_PROVIDER=
CB2_MIND_KEY_ENV=
""")
new("profiles/nim.env", """# cb2n profile "nim" (E.CB2-N): NVIDIA NIM upstream, its IPv4 addresses resolved on the box at
# run start (allowlisted exclusively, recorded in every receipt); the key file mounted read-only
# into the PROXY container only and injected as the Authorization header on every forward; both
# work containers hold placeholder keys. One model for both systems. The Mind runs with
# YM_PRIMARY_BRAIN=nim:<model> behind YM_PROVIDER_BASE_URL_NIM (the proxy).
CB2_UPSTREAM=integrate.api.nvidia.com
CB2_UPSTREAM_IP=
CB2_UPSTREAM_IPS=
CB2_UPSTREAM_RESOLVE=1
CB2_MODEL=z-ai/glm-5.2
CB2_KEY_FILE=/root/cb2/secrets/nim.key
CB2_MIND_LANE=roles
CB2_MIND_PROVIDER=nim
CB2_MIND_KEY_ENV=NVIDIA_API_KEY
""")
new("run/profile.sh", """#!/bin/bash
# Profile loader (sourced): `cb2_profile_load <fixtures dir>` exports the CB2_* upstream/model/key
# settings of profiles/${CB2_PROFILE:-qwen}.env. With CB2_UPSTREAM_RESOLVE=1 the upstream's IPv4
# addresses are resolved HERE, once per invocation (CB2_UPSTREAM_IPS; the first -> CB2_UPSTREAM_IP)
# and CB2_RESOLVED_AT records when. A named key file must exist and be non-empty; its content is
# read by nothing in these scripts — the proxy container gets it by bind mount, and the leak
# scan hands the FILE to grep as a pattern file, so the key never enters a variable.
cb2_profile_load() {
  local fix=$1 name=${CB2_PROFILE:-qwen}
  [ -f "$fix/profiles/$name.env" ] || { echo "profile: unknown profile '$name'"; return 1; }
  set -a; . "$fix/profiles/$name.env"; set +a
  CB2_RESOLVED_AT=""
  if [ "${CB2_UPSTREAM_RESOLVE:-0}" = 1 ]; then
    CB2_UPSTREAM_IPS=$(getent ahosts "$CB2_UPSTREAM" | awk '$1 ~ /^[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+$/ {print $1}' | sort -u | tr '\\n' ' ' | sed 's/ $//')
    [ -n "$CB2_UPSTREAM_IPS" ] || { echo "profile: could not resolve $CB2_UPSTREAM"; return 1; }
    CB2_UPSTREAM_IP=${CB2_UPSTREAM_IPS%% *}; CB2_RESOLVED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  fi
  [ -n "${CB2_UPSTREAM_IP:-}" ] && [ -n "${CB2_UPSTREAM_IPS:-}" ] || { echo "profile: upstream address unset"; return 1; }
  if [ -n "${CB2_KEY_FILE:-}" ]; then
    [ -s "$CB2_KEY_FILE" ] || { echo "profile: key file missing or empty"; return 1; }
    [ "$(stat -c %a "$CB2_KEY_FILE")" = 400 ] || { echo "profile: key file must be mode 0400"; return 1; }
  fi
  export CB2_PROFILE=$name CB2_UPSTREAM CB2_UPSTREAM_IP CB2_UPSTREAM_IPS CB2_RESOLVED_AT CB2_MODEL CB2_KEY_FILE CB2_MIND_LANE CB2_MIND_PROVIDER CB2_MIND_KEY_ENV
}
# Key-leak COUNT over the given paths (files, recursive): grep takes the key file itself as its
# pattern file; nothing here reads the key. Empty key file setting -> 0.
cb2_key_leak_hits() {
  [ -n "${CB2_KEY_FILE:-}" ] || { echo 0; return; }
  local n=0 p
  for p in "$@"; do [ -e "$p" ] && n=$(( n + $(grep -rlFf "$CB2_KEY_FILE" "$p" 2>/dev/null | wc -l) )); done
  echo "$n"
}
# Environment leak COUNT for a container (its configured env, joined): 0 or 1.
cb2_env_leak_hits() {
  [ -n "${CB2_KEY_FILE:-}" ] || { echo 0; return; }
  docker inspect "$1" --format '{{join .Config.Env "\\n"}}' 2>/dev/null | grep -cFf "$CB2_KEY_FILE" || true
}
# Void / rerun bookkeeping: `cb2_rerun_prepare <system> <task> <out>` returns 0 when the leg may
# start: either nothing exists for it, or exactly one prior receipt exists, is VOID, and no _void1
# archive exists yet — in which case every prior output of the leg is renamed *_void1 (preserved).
cb2_rerun_prepare() {
  local sys=$1 t=$2 out=$3 rec="$3/receipts/${1}_${2}.json" x
  [ -e "$rec" ] || [ -e "$out/artifacts/${sys}_$t" ] || return 0
  [ -e "$out/receipts/${sys}_${t}_void1.json" ] && { echo "refusing: ${sys} $t already used its one rerun"; return 1; }
  python3 -c "import json,sys; d=json.load(open('$rec')); sys.exit(0 if d.get('void') is True else 1)" 2>/dev/null || { echo "refusing: ${sys} $t exists and is not void (one invocation per task)"; return 1; }
  for x in "artifacts/${sys}_$t" "homes/${sys}_$t" "proxy/${sys}_$t" "state/${sys}_$t"; do
    [ -e "$out/$x" ] && { mv "$out/$x" "$out/${x}_void1" || return 1; }
  done
  for x in "receipts/${sys}_$t.json" "receipts/${sys}_${t}_teardown.txt"; do
    [ -e "$out/$x" ] && { mv "$out/$x" "$out/${x%.*}_void1.${x##*.}" || return 1; }
  done
  for x in "$out"/raw/${sys}_${t}_*.txt; do
    [ -e "$x" ] || continue
    case "$x" in *_void1.txt) ;; *) mv "$x" "${x%.txt}_void1.txt" || return 1;; esac
  done
  echo "rerun: ${sys} $t first receipt was void; prior outputs preserved as *_void1"
}
""")

# ── proxy.sh: profile-driven upstream, optional key mount, receipt identity check ─────────────
rw("run/proxy.sh", [
    ("set -u\nCMD=$1; NAME=$2\n",
     "set -u\nCMD=$1; NAME=$2\nFIX=\"$(cd \"$(dirname \"$0\")/..\" && pwd)\"; . \"$FIX/run/profile.sh\"; cb2_profile_load \"$FIX\" || exit 1\nKEYMOUNT=(); [ -n \"$CB2_KEY_FILE\" ] && KEYMOUNT=(-v \"$CB2_KEY_FILE:/run/secrets/upstream.key:ro\" -e CB2_KEY_PATH=/run/secrets/upstream.key)\n"),
    ('    docker run -d --name "$NAME" --network cb2egress --dns 127.0.0.1 --add-host aig.mycluster.cyou:192.168.4.203 --read-only --tmpfs /tmp:size=64m \\\n      --memory 512m --cpus 1 --pids-limit 64 -v "$CD:/count" -e CB2_CAP=8 -e CB2_COUNT_FILE=/count/requests.json cb2-proxy >/dev/null || fail "container did not start"',
     '    docker run -d --name "$NAME" --network cb2egress --dns 127.0.0.1 --add-host "$CB2_UPSTREAM:$CB2_UPSTREAM_IP" --read-only --tmpfs /tmp:size=64m \\\n      --memory 512m --cpus 1 --pids-limit 64 -v "$CD:/count" "${KEYMOUNT[@]}" -e CB2_CAP=8 -e CB2_COUNT_FILE=/count/requests.json \\\n      -e CB2_UPSTREAM="$CB2_UPSTREAM" -e CB2_UPSTREAM_IP="$CB2_UPSTREAM_IP" -e CB2_UPSTREAM_IPS="$CB2_UPSTREAM_IPS" -e CB2_RESOLVED_AT="$CB2_RESOLVED_AT" -e CB2_PROFILE="$CB2_PROFILE" -e CB2_MODEL="$CB2_MODEL" cb2-proxy >/dev/null || fail "container did not start"'),
    ('''    python3 -c "import json,sys; d=json.load(open('$CD/requests.json')); sys.exit(0 if d.get('tls_hostname_verified') is True and d.get('model_requests')==0 and d.get('upstream_errors')==0 else 1)" || fail "receipt not clean or TLS not verified"''',
     '''    WANTKEY=$([ -n "$CB2_KEY_FILE" ] && echo True || echo False)\n    python3 -c "import json,sys; d=json.load(open('$CD/requests.json')); sys.exit(0 if d.get('tls_hostname_verified') is True and d.get('model_requests')==0 and d.get('upstream_errors')==0 and d.get('upstream')=='$CB2_UPSTREAM' and d.get('upstream_ip')=='$CB2_UPSTREAM_IP' and d.get('key_injected') is $WANTKEY else 1)" || fail "receipt not clean, TLS not verified, or profile mismatch"'''),
])

# ── proxy.py: key injection, identity, upstream HTTP errors, response-model tally, usage ─────
rw("proxy/proxy.py", [
    ('every request. Env: CB2_UPSTREAM (host), CB2_CAP (int), CB2_COUNT_FILE (path)."""',
     'every request. Env: CB2_UPSTREAM (host), CB2_UPSTREAM_IP, CB2_CAP (int), CB2_COUNT_FILE (path),\nCB2_KEY_PATH (optional: a file whose content replaces the Authorization header on EVERY forward —\nthe work containers then never hold the real key), CB2_PROFILE / CB2_MODEL / CB2_UPSTREAM_IPS /\nCB2_RESOLVED_AT (recorded). The receipt also tallies the `model` id of every model response and\nprovider-reported usage counts; bodies are never stored."""'),
    ('COUNT_FILE = os.environ.get("CB2_COUNT_FILE", "/count/requests.json")\n',
     'COUNT_FILE = os.environ.get("CB2_COUNT_FILE", "/count/requests.json")\nKEY_PATH = os.environ.get("CB2_KEY_PATH", "")\nKEY = open(KEY_PATH, encoding="utf-8").read().strip() if KEY_PATH else ""\n'),
    ('state = {"model_requests": 0, "refused_over_cap": 0, "forwarded_other": 0, "upstream_errors": 0, "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "cap": CAP, "by_path": {}}',
     'state = {"model_requests": 0, "refused_over_cap": 0, "forwarded_other": 0, "upstream_errors": 0, "upstream_http_errors": 0, "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "cap": CAP, "by_path": {},\n         "profile": os.environ.get("CB2_PROFILE", ""), "model_expected": os.environ.get("CB2_MODEL", ""), "upstream": UPSTREAM, "upstream_ip": UPSTREAM_IP,\n         "upstream_ips": os.environ.get("CB2_UPSTREAM_IPS", ""), "resolved_at": os.environ.get("CB2_RESOLVED_AT", ""), "key_injected": bool(KEY),\n         "response_models": {}, "usage": {"responses_with_usage": 0, "prompt_tokens": 0, "completion_tokens": 0}}'),
    ('        headers["Host"] = UPSTREAM\n',
     '        headers["Host"] = UPSTREAM\n        if KEY:\n            headers = {k: v for k, v in headers.items() if k.lower() != "authorization"}\n            headers["Authorization"] = "Bearer " + KEY\n'),
    ('        self.send_response(resp.status)\n        chunked = False',
     '        if is_model_request and resp.status >= 400:\n            with lock:\n                state["upstream_http_errors"] += 1\n                persist()\n        self.send_response(resp.status)\n        chunked = False\n        seen = bytearray()'),
    ('                if not chunk:\n                    break\n                if chunked:',
     '                if not chunk:\n                    break\n                if is_model_request and len(seen) < 4_000_000:\n                    seen.extend(chunk)\n                if chunked:'),
    ('        except Exception:\n            pass\n        finally:\n            conn.close()',
     '        except Exception:\n            pass\n        finally:\n            conn.close()\n        if is_model_request and seen:\n            self._tally(bytes(seen))\n\n    def _tally(self, raw):\n        """From a model response body: the `model` id (tallied) and provider-reported usage (summed)\n        — a JSON body, or SSE events. Counts only; the body is discarded."""\n        objs = []\n        try:\n            objs.append(json.loads(raw))\n        except Exception:\n            for line in raw.decode("utf-8", "replace").splitlines():\n                if line.startswith("data: ") and line[6:].strip() not in ("", "[DONE]"):\n                    try:\n                        objs.append(json.loads(line[6:]))\n                    except Exception:\n                        pass\n        models = {o.get("model") for o in objs if isinstance(o, dict) and isinstance(o.get("model"), str)}\n        usage = None\n        for o in objs:\n            if isinstance(o, dict) and isinstance(o.get("usage"), dict):\n                usage = o["usage"]\n        with lock:\n            for m in sorted(models):\n                state["response_models"][m[:80]] = state["response_models"].get(m[:80], 0) + 1\n            if not models:\n                state["response_models"]["(none)"] = state["response_models"].get("(none)", 0) + 1\n            if usage:\n                pt, ct = usage.get("prompt_tokens"), usage.get("completion_tokens")\n                if type(pt) is int and type(ct) is int:\n                    state["usage"]["responses_with_usage"] += 1\n                    state["usage"]["prompt_tokens"] += pt\n                    state["usage"]["completion_tokens"] += ct\n            persist()'),
])

# ── cb2net.sh: exclusive allowlist from the profile; probes parameterised ─────────────────────
rw("net/cb2net.sh", [
    ('set -u\nGW=192.168.4.203; HERE="$(cd "$(dirname "$0")" && pwd)"\n',
     'set -u\nHERE="$(cd "$(dirname "$0")" && pwd)"; . "$HERE/../run/profile.sh"; cb2_profile_load "$HERE/.." || exit 1\nGW=$CB2_UPSTREAM_IP\n'),
    ('iptables -C DOCKER-USER -s 172.30.1.0/24 -d $GW -p tcp --dport 443 -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -s 172.30.1.0/24 -d $GW -p tcp --dport 443 -j ACCEPT\n',
     '# upstream ACCEPTs: exactly the profile\'s resolved addresses; an ACCEPT for any other destination (a previous profile) is removed\nfor RULE in $(iptables -S DOCKER-USER | grep -E -- "^-A DOCKER-USER -s 172.30.1.0/24 -d [0-9./]+ -p tcp -m tcp --dport 443 -j ACCEPT$" | awk \'{print $6}\'); do\n  KEEP=0; for IP in $CB2_UPSTREAM_IPS; do [ "$RULE" = "$IP/32" ] && KEEP=1; done\n  [ $KEEP = 1 ] || iptables -D DOCKER-USER -s 172.30.1.0/24 -d "$RULE" -p tcp --dport 443 -j ACCEPT || { echo "could not delete stale upstream rule $RULE"; exit 1; }\ndone\nfor IP in $CB2_UPSTREAM_IPS; do\n  iptables -C DOCKER-USER -s 172.30.1.0/24 -d $IP -p tcp --dport 443 -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -s 172.30.1.0/24 -d $IP -p tcp --dport 443 -j ACCEPT\ndone\n'),
    ('echo "networks: cb2net internal=$(docker network inspect cb2net --format \'{{.Internal}}\') subnet=172.30.0.0/24; cb2egress subnet=172.30.1.0/24; bridges work=$BR_WORK egress=$BR_EGRESS"',
     'echo "profile: $CB2_PROFILE upstream=$CB2_UPSTREAM addresses=[$CB2_UPSTREAM_IPS] resolved_at=${CB2_RESOLVED_AT:-static}"\necho "networks: cb2net internal=$(docker network inspect cb2net --format \'{{.Internal}}\') subnet=172.30.0.0/24; cb2egress subnet=172.30.1.0/24; bridges work=$BR_WORK egress=$BR_EGRESS"'),
    ('docker run -d --name cb2probe-proxy --network cb2egress --dns 127.0.0.1 --add-host aig.mycluster.cyou:$GW -e CB2_CAP=1 -e CB2_COUNT_FILE=/tmp/c.json cb2-proxy >/dev/null',
     'docker run -d --name cb2probe-proxy --network cb2egress --dns 127.0.0.1 --add-host "$CB2_UPSTREAM:$GW" -e CB2_UPSTREAM="$CB2_UPSTREAM" -e CB2_UPSTREAM_IP="$GW" -e CB2_CAP=1 -e CB2_COUNT_FILE=/tmp/c.json cb2-proxy >/dev/null'),
    ('W=$(docker run --rm --name cb2probe-work --network cb2net --add-host aig.mycluster.cyou:$GW --dns 127.0.0.1 -v "$HERE/probe_work.py:/probe.py:ro" python:3.13-slim python3 /probe.py 2>/dev/null)',
     'W=$(docker run --rm --name cb2probe-work --network cb2net --dns 127.0.0.1 -e CB2_UPSTREAM_IP="$GW" -v "$HERE/probe_work.py:/probe.py:ro" python:3.13-slim python3 /probe.py 2>/dev/null)'),
])
rw("net/probe_work.py", [
    ('import socket\n', 'import os, socket\nUP = os.environ.get("CB2_UPSTREAM_IP", "192.168.4.203")\n'),
    ("gateway-tcp {tcp('192.168.4.203', 443)}", "gateway-tcp {tcp(UP, 443)}"),
])
rw("net/probe_proxy.py", [
    ('import socket, ssl\n', 'import os, socket, ssl\nUP = os.environ.get("CB2_UPSTREAM", "aig.mycluster.cyou")\n'),
    ("gateway-tls-verified {tls_verified('aig.mycluster.cyou')}", "gateway-tls-verified {tls_verified(UP)}"),
])

# ── shared receipt-side checks (python one-liners are awkward in bash; a tiny helper) ─────────
new("run/receipt_checks.py", """\"\"\"Profile-side receipt checks, counts only. Usage:
  receipt_checks.py <proxy requests.json> <expected model>
Prints: http_errors=<n> transport_errors=<n> model_ok=<true|false> models=<n distinct> usage_p=<n> usage_c=<n> usage_n=<n>
model_ok is true iff every tallied response model equals the expected model (a leg with no
tallied model at all is NOT ok — the identity check must have evidence).\"\"\"
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    want = sys.argv[2]
    models = d.get("response_models") or {}
    http_err = int(d.get("upstream_http_errors", -1)); trans = int(d.get("upstream_errors", -1))
    ok = bool(models) and all(k == want for k in models) and type(http_err) is int
    u = d.get("usage") or {}
    print(f"http_errors={http_err} transport_errors={trans} model_ok={str(ok).lower()} models={len(models)} usage_p={u.get('prompt_tokens', 0)} usage_c={u.get('completion_tokens', 0)} usage_n={u.get('responses_with_usage', 0)}")
except Exception:
    print("http_errors=-1 transport_errors=-1 model_ok=false models=0 usage_p=0 usage_c=0 usage_n=0")
""")

# ── hermes leg ────────────────────────────────────────────────────────────────────────────────
rw("run/hermes_leg.sh", [
    ('T=$1; OUT=${2:-/root/cb2/out}; WALL=1800; CAP=8\nFIX="$(cd "$(dirname "$0")/.." && pwd)"\n',
     'T=$1; OUT=${2:-/root/cb2/out}; WALL=1800; CAP=8\nFIX="$(cd "$(dirname "$0")/.." && pwd)"; . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1\ncb2_rerun_prepare hermes "$T" "$OUT" || exit 2\n'),
    ("printf 'model:\\n  default: qwen3.8:27b-q4_K_M\\n  provider: custom\\n  base_url: http://%s:8080/v1\\nagent:\\n  max_turns: %s\\n' \"$PIP\" \"$CAP\" > \"$H/config.yaml\"",
     "printf 'model:\\n  default: %s\\n  provider: custom\\n  base_url: http://%s:8080/v1\\nagent:\\n  max_turns: %s\\n' \"$CB2_MODEL\" \"$PIP\" \"$CAP\" > \"$H/config.yaml\""),
    ('RC=$?\nEND=$(date -u +%Y-%m-%dT%H:%M:%SZ); WALLS=$(( $(date +%s) - T0 ))\ndocker rm -f "$NAME" >/dev/null 2>&1\n',
     'RC=$?\nEND=$(date -u +%Y-%m-%dT%H:%M:%SZ); WALLS=$(( $(date +%s) - T0 ))\nENVLEAK=$(cb2_env_leak_hits "$NAME")\ndocker rm -f "$NAME" >/dev/null 2>&1\n'),
    ('DL=$(grep -ciE "pip install|pip3 install|npm install|npm i |apt-get|apt install|curl |wget " "$RAW/hermes_${T}_stdout.txt")\n',
     'DL=$(grep -ciE "pip install|pip3 install|npm install|npm i |apt-get|apt install|curl |wget " "$RAW/hermes_${T}_stdout.txt")\nLEAK=$(( ${ENVLEAK:-0} + $(cb2_key_leak_hits "$W" "$H" "$RAW/hermes_${T}_stdout.txt") ))\nread -r PHTTP PTRANS MODEL_OK NMODELS UP UC UN <<< "$(python3 "$FIX/run/receipt_checks.py" "$CD/requests.json" "$CB2_MODEL" | sed -E \'s/[a-z_]+=//g\')"\nVOID=false; { [ "$PHTTP" != 0 ] || [ "$PTRANS" != 0 ]; } && VOID=true\n'),
    ('DQ=false; [ "$VALID" = false ] && DQ=true; [ "$CALLS" -gt $CAP ] && DQ=true; [ "$DL" -gt 0 ] && DQ=true; [ $RC -ne 0 ] && DQ=true\n',
     'DQ=false; [ "$VALID" = false ] && DQ=true; [ "$CALLS" -gt $CAP ] && DQ=true; [ "$DL" -gt 0 ] && DQ=true; [ $RC -ne 0 ] && DQ=true; [ "$LEAK" != 0 ] && DQ=true; [ "$MODEL_OK" = true ] || DQ=true\n'),
    ('"download_or_install_lines":%s,"proxy_receipt_ok":%s,"disqualified":%s,"tree":"%s"}',
     '"download_or_install_lines":%s,"proxy_receipt_ok":%s,"profile":"%s","upstream":"%s","upstream_ips":"%s","resolved_at":"%s","model":"%s","model_ok":%s,"key_leak_hits":%s,"upstream_http_errors":%s,"upstream_transport_errors":%s,"usage_prompt_tokens":%s,"usage_completion_tokens":%s,"usage_responses":%s,"void":%s,"disqualified":%s,"tree":"%s"}'),
    ('"$TOKS" "$DL" "$RECEIPT_OK" "$DQ" "$HASH"',
     '"$TOKS" "$DL" "$RECEIPT_OK" "$CB2_PROFILE" "$CB2_UPSTREAM" "$CB2_UPSTREAM_IPS" "${CB2_RESOLVED_AT:-static}" "$CB2_MODEL" "$MODEL_OK" "$LEAK" "$PHTTP" "$PTRANS" "$UP" "$UC" "$UN" "$VOID" "$DQ" "$HASH"'),
])

# ── mind leg ──────────────────────────────────────────────────────────────────────────────────
rw("run/mind_leg.sh", [
    ('T=$1; OUT=${2:-/root/cb2/out}; WALL=1800\nFIX="$(cd "$(dirname "$0")/.." && pwd)"\n',
     'T=$1; OUT=${2:-/root/cb2/out}; WALL=1800\nFIX="$(cd "$(dirname "$0")/.." && pwd)"; . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1\ncb2_rerun_prepare mind "$T" "$OUT" || exit 2\n'),
    ('NAME="cb2-mind-$T"; PROXY="cb2proxy-mind-$T"; PIP=172.30.0.2\n',
     'NAME="cb2-mind-$T"; PROXY="cb2proxy-mind-$T"; PIP=172.30.0.2\n# model lane by profile: "local" = the owned endpoint as the local/private lane (v3 behaviour);\n# "roles" = YM_PRIMARY_BRAIN=<provider>:<model> behind YM_PROVIDER_BASE_URL_<PROVIDER> (the proxy),\n# a placeholder key in the container, no local lane (the scratch state holds nothing private).\nif [ "$CB2_MIND_LANE" = local ]; then\n  LANE=(-e YM_LOCAL_OLLAMA_URL="http://$PIP:8080" -e YM_LOCAL_OLLAMA_MODEL="$CB2_MODEL" -e YM_PRIVATE_PROVIDERS=ollama-local -e YM_HOUSEHOLD_PROVIDERS=ollama-local)\nelse\n  PU=$(echo "$CB2_MIND_PROVIDER" | tr \'a-z-\' \'A-Z_\')\n  LANE=(-e YM_PRIMARY_BRAIN="$CB2_MIND_PROVIDER:$CB2_MODEL" -e "YM_PROVIDER_BASE_URL_$PU=http://$PIP:8080/v1" -e "$CB2_MIND_KEY_ENV=none" -e YM_PRIVATE_PROVIDERS= -e YM_HOUSEHOLD_PROVIDERS="$CB2_MIND_PROVIDER,chain")\nfi\n'),
    ('  -e YM_OPERATOR=cb2 -e YM_TZ=Asia/Kolkata -e YM_LOCAL_OLLAMA_URL="http://$PIP:8080" -e YM_LOCAL_OLLAMA_MODEL=qwen3.8:27b-q4_K_M \\\n  -e YM_PRIVATE_PROVIDERS=ollama-local -e YM_HOUSEHOLD_PROVIDERS=ollama-local -e YM_INFER_PERMITS=2 \\\n',
     '  -e YM_OPERATOR=cb2 -e YM_TZ=Asia/Kolkata "${LANE[@]}" -e YM_INFER_PERMITS=2 \\\n'),
    ('docker logs "$NAME" > "$OUT/raw/mind_${T}_stdout.txt" 2>&1\ndocker rm -f "$NAME" >/dev/null 2>&1   # the parent stops the instance AFTER the driver wrote its receipt\n',
     'docker logs "$NAME" > "$OUT/raw/mind_${T}_stdout.txt" 2>&1\nENVLEAK=$(cb2_env_leak_hits "$NAME")\ndocker rm -f "$NAME" >/dev/null 2>&1   # the parent stops the instance AFTER the driver wrote its receipt\nLEAK=$(( ${ENVLEAK:-0} + $(cb2_key_leak_hits "$ST" "$OUT/raw/mind_${T}_stdout.txt" "$OUT/raw/mind_${T}_driver.txt") ))\nCHECKS=$(python3 "$FIX/run/receipt_checks.py" "$CD/requests.json" "$CB2_MODEL")\n'),
    ('python3 - "$ST/receipt.json" "$R/mind_$T.json" "$BIN_SHA" "$PROV" "$IMG" "$HASH" "$RC" "$T" "$CD/requests.json" <<\'EOF\'\nimport json, sys\nsrc, dst, bin_sha, prov, img, tree, rc, task, prx = sys.argv[1:]\n',
     'python3 - "$ST/receipt.json" "$R/mind_$T.json" "$BIN_SHA" "$PROV" "$IMG" "$HASH" "$RC" "$T" "$CD/requests.json" "$CB2_PROFILE" "$CB2_UPSTREAM" "$CB2_UPSTREAM_IPS" "${CB2_RESOLVED_AT:-static}" "$CB2_MODEL" "$LEAK" "$CHECKS" <<\'EOF\'\nimport json, sys\nsrc, dst, bin_sha, prov, img, tree, rc, task, prx, profile, upstream, ips, resolved_at, model, leak, checks = sys.argv[1:]\nck = dict(kv.split("=", 1) for kv in checks.split())\n'),
    ('d["disqualified"] = bool(d.get("disqualified")) or (not receipt_ok) or (not capture_ok) or syml > 0 or special > 0 or int(rc) != 0\n',
     'void = ck.get("http_errors") != "0" or ck.get("transport_errors") != "0"\nd.update({"profile": profile, "upstream": upstream, "upstream_ips": ips, "resolved_at": resolved_at, "model": model, "model_ok": ck.get("model_ok") == "true", "key_leak_hits": int(leak),\n          "upstream_http_errors": int(ck.get("http_errors", -1)), "upstream_transport_errors": int(ck.get("transport_errors", -1)),\n          "usage_prompt_tokens": int(ck.get("usage_p", 0)), "usage_completion_tokens": int(ck.get("usage_c", 0)), "usage_responses": int(ck.get("usage_n", 0)), "void": void})\nd["disqualified"] = bool(d.get("disqualified")) or (not receipt_ok) or (not capture_ok) or syml > 0 or special > 0 or int(rc) != 0 or int(leak) != 0 or ck.get("model_ok") != "true"\n'),
])

# ── smokes + cap test ─────────────────────────────────────────────────────────────────────────
rw("run/smoke_mind.sh", [
    ('FIX="$(cd "$(dirname "$0")/.." && pwd)"; OUT=$(mktemp -d /tmp/cb2-smoke-mind-XXXX)\n',
     'FIX="$(cd "$(dirname "$0")/.." && pwd)"; OUT=$(mktemp -d /tmp/cb2-smoke-mind-XXXX); . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1\nif [ "$CB2_MIND_LANE" = local ]; then\n  LANE=(-e YM_LOCAL_OLLAMA_URL=http://172.30.0.7:8080 -e YM_LOCAL_OLLAMA_MODEL="$CB2_MODEL" -e YM_PRIVATE_PROVIDERS=ollama-local -e YM_HOUSEHOLD_PROVIDERS=ollama-local); BOOTLINE="LOCAL primary + private lane active (ollama-local:$CB2_MODEL)"\nelse\n  PU=$(echo "$CB2_MIND_PROVIDER" | tr \'a-z-\' \'A-Z_\')\n  LANE=(-e YM_PRIMARY_BRAIN="$CB2_MIND_PROVIDER:$CB2_MODEL" -e "YM_PROVIDER_BASE_URL_$PU=http://172.30.0.7:8080/v1" -e "$CB2_MIND_KEY_ENV=none" -e YM_PRIVATE_PROVIDERS= -e YM_HOUSEHOLD_PROVIDERS="$CB2_MIND_PROVIDER,chain"); BOOTLINE="$CB2_MIND_PROVIDER:$CB2_MODEL"\nfi\n'),
    ('  -e YM_OPERATOR=cb2 -e YM_TZ=Asia/Kolkata -e YM_LOCAL_OLLAMA_URL=http://172.30.0.7:8080 -e YM_LOCAL_OLLAMA_MODEL=qwen3.8:27b-q4_K_M \\\n  -e YM_PRIVATE_PROVIDERS=ollama-local -e YM_HOUSEHOLD_PROVIDERS=ollama-local -e YM_INFER_PERMITS=2 \\\n',
     '  -e YM_OPERATOR=cb2 -e YM_TZ=Asia/Kolkata "${LANE[@]}" -e YM_INFER_PERMITS=2 \\\n'),
    ('LOCAL=$(docker logs cb2-mind-smoke 2>&1 | grep -c "LOCAL primary + private lane active (ollama-local:qwen3.8:27b-q4_K_M)")',
     'LOCAL=$(docker logs cb2-mind-smoke 2>&1 | grep -cF -- "$BOOTLINE")'),
    ('print(f"mind smoke: local-lane-line {local}', 'print(f"mind smoke [{d.get(\'profile\')} -> {d.get(\'upstream\')}]: brain-line {local}'),
])
rw("run/smoke_hermes.sh", [
    ('FIX="$(cd "$(dirname "$0")/.." && pwd)"; OUT=$(mktemp -d /tmp/cb2-smoke-hermes-XXXX)\n',
     'FIX="$(cd "$(dirname "$0")/.." && pwd)"; OUT=$(mktemp -d /tmp/cb2-smoke-hermes-XXXX); . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1\n'),
    ("printf 'model:\\n  default: qwen3.8:27b-q4_K_M\\n  provider: custom\\n  base_url: http://172.30.0.6:8080/v1\\nagent:\\n  max_turns: 2\\n' > \"$OUT/home/config.yaml\"",
     "printf 'model:\\n  default: %s\\n  provider: custom\\n  base_url: http://172.30.0.6:8080/v1\\nagent:\\n  max_turns: 2\\n' \"$CB2_MODEL\" > \"$OUT/home/config.yaml\""),
    ('print(f"hermes smoke: rc {rc}', 'print(f"hermes smoke [{d.get(\'profile\')} -> {d.get(\'upstream\')} models {d.get(\'response_models\')}]: rc {rc}'),
])
rw("net/captest.sh", [
    ('FIX="$(cd "$(dirname "$0")/.." && pwd)"; CD=$(mktemp -d /tmp/cb2-captest-XXXX)\n',
     'FIX="$(cd "$(dirname "$0")/.." && pwd)"; CD=$(mktemp -d /tmp/cb2-captest-XXXX); . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1\n'),
    ('docker run --rm --name cb2-captest-client --network cb2net --dns 127.0.0.1 -v "$FIX/net/captest_client.py:/c.py:ro" python:3.13-slim python3 /c.py',
     'docker run --rm --name cb2-captest-client --network cb2net --dns 127.0.0.1 -e CB2_MODEL="$CB2_MODEL" -v "$FIX/net/captest_client.py:/c.py:ro" python:3.13-slim python3 /c.py'),
    ('print(f"cap test receipt: accepted', 'print(f"cap test receipt [{d.get(\'profile\')} -> {d.get(\'upstream\')} models {d.get(\'response_models\')}]: accepted'),
])
rw("net/captest_client.py", [
    ('import json, urllib.request, urllib.error\nbody = json.dumps({"model": "qwen3.8:27b-q4_K_M",',
     'import json, os, urllib.request, urllib.error\nbody = json.dumps({"model": os.environ.get("CB2_MODEL", "qwen3.8:27b-q4_K_M"),'),
])
rw("MANIFEST.json", [
    ('  "id": "E.CB2",\n  "version": 3,\n',
     '  "id": "E.CB2-N",\n  "version": 4,\n  "derived_from": "fixtures/cb2 at d4febe6 (the frozen Qwen reading; untouched) by the recorded patch scratch/cb2n_patch.py",\n  "profiles": "profiles/<name>.env, loaded by run/profile.sh (CB2_PROFILE, default qwen). qwen = the v3 reading unchanged: owned gateway 192.168.4.203, no key injection, the Mind on its local lane. nim (E.CB2-N) = upstream integrate.api.nvidia.com with its IPv4 addresses resolved on the box at run start (allowlisted EXCLUSIVELY — ACCEPT rules for any other upstream are removed — and recorded with the resolution time in every receipt), the key file (mode 0400) mounted read-only into the PROXY container only and injected as the Authorization header on every forward, placeholder keys in both work containers, one model for both systems (z-ai/glm-5.2), the Mind via YM_PRIMARY_BRAIN=nim:<model> behind YM_PROVIDER_BASE_URL_NIM. Receipts gain profile/upstream/upstream_ips/resolved_at/model/model_ok/key_leak_hits/upstream_http_errors/upstream_transport_errors/usage_*/void; the proxy receipt gains upstream/upstream_ip/upstream_ips/resolved_at/key_injected/upstream_http_errors/response_models/usage. Key-leak scans hand the key FILE to grep as a pattern file (the key and its prefix never enter a variable, log or receipt); any hit in a work container\'s env, home, state, raw log or artifact disqualifies. Model identity: every tallied response model must equal the profile model or the leg is disqualified; a leg with no tallied model is disqualified too. Void = an upstream HTTP error (4xx/5xx) on a model request or a transport/TLS failure: infrastructure, not the agent; the first receipt and outputs are preserved as *_void1, exactly one same-leg rerun is allowed (run/profile.sh cb2_rerun_prepare), a second void refuses.",\n'),
])
# ── a distinct proxy image so the frozen cb2 image is never rebuilt ───────────────────────────
for p in ("run/proxy.sh", "net/cb2net.sh", "README.md", "MANIFEST.json"):
    s = io.open(R + p, encoding="utf-8").read()
    s2 = s.replace("cb2-proxy", "cb2n-proxy")
    io.open(R + p, "w", encoding="utf-8", newline="\n").write(s2)
    print(p, "image refs renamed:", s.count("cb2-proxy"))
print("cb2n patch applied to", R)
