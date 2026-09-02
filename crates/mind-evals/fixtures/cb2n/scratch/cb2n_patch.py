"""E.CB2-N: derive the profiled harness (cb2n) from a copy of the frozen cb2 fixtures.
Usage: python3 cb2n_patch.py <cb2n dir>   (the dir is a fresh copy of fixtures/cb2)
Exactness: scratch/rederive.sh copies cb2, applies this file, and diffs — the tree, this file's
copy under scratch/ and the README preface are all produced here, so nothing is hand-written.

Gates (Codex 17:52Z + 18:01Z + 18:13Z HOLD, ten items): cb2 untouched; key proxy-only, uid 10002 +
mode 0400, never in a variable (grep -Ff on the key file, counts only); addresses resolved ONCE
into an immutable run state consumed by every script; every attempted model POST consumes the
cap; void = 429/5xx or transport/TLS only, never a DQ by itself; model identity from successful
responses only; upstream read failures counted apart from client disconnects; strictly typed
receipt integers; profile-free proxy teardown; exclusive allowlist fail-closed; brain gate."""
import io, os, shutil, sys

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


# ── README preface (generated here so the tree re-derives exactly) ─────────────────────────────
_readme = io.open(R + "README.md", encoding="utf-8").read()
new("README.md", """# E.CB2-N — the profiled harness (derived; the frozen `cb2` tree is untouched)

This tree is `fixtures/cb2` at d4febe6 plus exactly the patch recorded in `scratch/cb2n_patch.py`;
`scratch/rederive.sh` proves it (copy cb2, apply the patch, diff). It adds `profiles/` (qwen | nim,
`.profile` because the repository ignores `*.env`), `run/profile.sh` (profile + ONE-TIME resolution
into an immutable `run_state.json`), `run/receipt_checks.py`, the proxy's key injection /
response-model tally / usage counts, the exclusive upstream allowlist (fail-closed), the key-leak,
model-identity and brain-gate disqualifiers, and the void/rerun rule. Images: `cb2n-proxy` (this
proxy), `cb2-hermes` / `cb2-mind` / `cb2-check` unchanged. Under the `qwen` profile every
behaviour of the frozen reading is reproduced; the `nim` profile is E.CB2-N (ledger, 2026-09-02).

""" + _readme)

new("profiles/qwen.profile", """# cb2n profile "qwen" — the frozen v3 reading's behaviour: the owned gateway, no key injection,
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
new("profiles/nim.profile", """# cb2n profile "nim" (E.CB2-N): NVIDIA NIM upstream, its IPv4 addresses resolved on the box ONCE
# per run into the immutable run state (allowlisted exclusively, recorded in every receipt); the
# key file (uid 10002, mode 0400) mounted read-only into the PROXY container only and injected as
# the Authorization header on every forward; both work containers hold placeholder keys. One
# model for both systems. The Mind runs with YM_PRIMARY_BRAIN=nim:<model> and all six roles equal
# to it, behind YM_PROVIDER_BASE_URL_NIM (the proxy).
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
# settings of profiles/${CB2_PROFILE:-qwen}.profile (.profile, not .env: the repository ignores
# *.env) THROUGH THE RUN STATE: the first loader call of a run writes
# ${CB2_RUN_STATE:-${CB2_OUT:-/root/cb2n/out}/run_state.json} (profile, upstream, the upstream's
# IPv4 addresses resolved exactly once when CB2_UPSTREAM_RESOLVE=1, the first address, the
# resolution time, the model) and makes it read-only; every later call — network script, proxy,
# legs, smokes, cap test — consumes that exact set and refuses a state written for another
# profile/upstream/model. A named key file must be uid 10002 (the proxy's user) and mode 0400;
# its content is read by nothing in these scripts — the proxy container gets it by bind mount,
# and the leak scan hands the FILE to grep as a pattern file, so the key never enters a variable.
cb2_profile_load() {
  local fix=$1 name=${CB2_PROFILE:-qwen} state=${CB2_RUN_STATE:-${CB2_OUT:-/root/cb2n/out}/run_state.json}
  [ -f "$fix/profiles/$name.profile" ] || { echo "profile: unknown profile '$name'"; return 1; }
  set -a; . "$fix/profiles/$name.profile"; set +a
  if [ -f "$state" ]; then
    local got
    got=$(python3 -c "import json,sys
d=json.load(open('$state'))
print(d['profile'], d['upstream'], d['model'], d['upstream_ip'], d['resolved_at'], '|'.join(d['upstream_ips']))" 2>/dev/null) || { echo "profile: run state unreadable: $state"; return 1; }
    read -r sp su sm sip sat sips <<< "$got"
    [ "$sp" = "$name" ] && [ "$su" = "$CB2_UPSTREAM" ] && [ "$sm" = "$CB2_MODEL" ] || { echo "profile: run state $state belongs to profile '$sp' ($su, $sm), not '$name' — use another out dir"; return 1; }
    CB2_UPSTREAM_IP=$sip; CB2_RESOLVED_AT=$sat; CB2_UPSTREAM_IPS=${sips//|/ }
  else
    CB2_RESOLVED_AT=static
    if [ "${CB2_UPSTREAM_RESOLVE:-0}" = 1 ]; then
      CB2_UPSTREAM_IPS=$(getent ahosts "$CB2_UPSTREAM" | awk '$1 ~ /^[0-9]+\\.[0-9]+\\.[0-9]+\\.[0-9]+$/ {print $1}' | sort -u | tr '\\n' ' ' | sed 's/ $//')
      [ -n "$CB2_UPSTREAM_IPS" ] || { echo "profile: could not resolve $CB2_UPSTREAM"; return 1; }
      CB2_UPSTREAM_IP=${CB2_UPSTREAM_IPS%% *}; CB2_RESOLVED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    fi
    [ -n "${CB2_UPSTREAM_IP:-}" ] && [ -n "${CB2_UPSTREAM_IPS:-}" ] || { echo "profile: upstream address unset"; return 1; }
    mkdir -p "$(dirname "$state")" || return 1
    python3 -c "import json,sys
json.dump({'profile': '$name', 'upstream': '$CB2_UPSTREAM', 'upstream_ips': '$CB2_UPSTREAM_IPS'.split(), 'upstream_ip': '$CB2_UPSTREAM_IP', 'resolved_at': '$CB2_RESOLVED_AT', 'model': '$CB2_MODEL'}, open('$state.tmp', 'w'), indent=1)" || return 1
    mv "$state.tmp" "$state" && chmod 444 "$state" || return 1
  fi
  if [ -n "${CB2_KEY_FILE:-}" ]; then
    [ -s "$CB2_KEY_FILE" ] || { echo "profile: key file missing or empty"; return 1; }
    [ "$(stat -c '%u %a' "$CB2_KEY_FILE")" = "10002 400" ] || { echo "profile: key file must be uid 10002 (the proxy user) and mode 0400"; return 1; }
  fi
  export CB2_PROFILE=$name CB2_RUN_STATE=$state CB2_UPSTREAM CB2_UPSTREAM_IP CB2_UPSTREAM_IPS CB2_RESOLVED_AT CB2_MODEL CB2_KEY_FILE CB2_MIND_LANE CB2_MIND_PROVIDER CB2_MIND_KEY_ENV
}
# Key-leak COUNT over the given paths (files, recursive): grep takes the key file itself as its
# pattern file; nothing here reads the key. Empty key file setting -> 0.
cb2_key_leak_hits() {
  [ -n "${CB2_KEY_FILE:-}" ] || { echo 0; return; }
  local n=0 p
  for p in "$@"; do [ -e "$p" ] && n=$(( n + $(grep -rlFf "$CB2_KEY_FILE" "$p" 2>/dev/null | wc -l) )); done
  echo "$n"
}
# Environment leak COUNT for a container (its configured env, one per line): 0 or more.
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
new("scratch/rederive.sh", """#!/bin/bash
# Exact re-derivation check: copy the frozen cb2 tree, apply the recorded patch, diff against this
# tree (the shipped Hermes archive is excluded: it is never committed). Exit non-zero on any diff.
set -u
HERE="$(cd "$(dirname "$0")/.." && pwd)"; SRC="${1:-$HERE/../cb2}"; T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
cp -r "$SRC/." "$T/" && python3 "$HERE/scratch/cb2n_patch.py" "$T" >/dev/null || { echo "patch failed"; exit 1; }
if diff -r --exclude='*.tar.gz' --exclude=__pycache__ "$T" "$HERE"; then echo "cb2n re-derives exactly from cb2 + scratch/cb2n_patch.py"; else echo "cb2n DOES NOT re-derive"; exit 1; fi
""")
new("run/receipt_checks.py", """\"\"\"Profile-side receipt checks, counts only (strictly typed). Usage:
  receipt_checks.py <proxy requests.json> <expected model>
Prints one line: http_errors=<n> transport_errors=<n> client_errors=<n> model_ok=<true|false>
models=<distinct> usage_p=<n> usage_c=<n> usage_n=<n>
Every count must be an exact non-negative int (bool/str/negative are rejected) and every
response_models value a positive int, else the fallback line (all -1 / false / 0) is printed.
model_ok is true iff at least one successful model response was tallied and every tallied model
equals the expected model.\"\"\"
import json, sys


def nn(x):
    return type(x) is int and x >= 0


def main():
    fallback = "http_errors=-1 transport_errors=-1 client_errors=-1 model_ok=false models=0 usage_p=0 usage_c=0 usage_n=0"
    try:
        d = json.load(open(sys.argv[1]))
        want = sys.argv[2]
        http_err, trans, client = d["upstream_http_errors"], d["upstream_errors"], d["upstream_client_errors"]
        models, usage = d["response_models"], d["usage"]
        typed = (nn(http_err) and nn(trans) and nn(client) and isinstance(models, dict)
                 and all(isinstance(k, str) and type(v) is int and v > 0 for k, v in models.items())
                 and isinstance(usage, dict) and all(nn(usage.get(k)) for k in ("responses_with_usage", "prompt_tokens", "completion_tokens")))
        if not typed:
            print(fallback)
            return
        ok = bool(models) and all(k == want for k in models)
        print(f"http_errors={http_err} transport_errors={trans} client_errors={client} model_ok={str(ok).lower()} models={len(models)} "
              f"usage_p={usage['prompt_tokens']} usage_c={usage['completion_tokens']} usage_n={usage['responses_with_usage']}")
    except Exception:
        print(fallback)


main()
""")

# ── proxy.sh: teardown first and profile-free; profile-driven up; receipt identity check ─────
rw("run/proxy.sh", [
    ("set -u\nCMD=$1; NAME=$2\n",
     "set -u\nCMD=$1; NAME=$2\n# teardown is unconditional and needs no profile, key or run state (a vanished key file must never strand a proxy)\nif [ \"$CMD\" = down ]; then docker rm -f \"$NAME\" >/dev/null 2>&1; echo \"proxy $NAME down\"; exit 0; fi\nFIX=\"$(cd \"$(dirname \"$0\")/..\" && pwd)\"; . \"$FIX/run/profile.sh\"; cb2_profile_load \"$FIX\" || exit 1\nKEYMOUNT=(); [ -n \"$CB2_KEY_FILE\" ] && KEYMOUNT=(-v \"$CB2_KEY_FILE:/run/secrets/upstream.key:ro\" -e CB2_KEY_PATH=/run/secrets/upstream.key)\n"),
    ('    docker run -d --name "$NAME" --network cb2egress --dns 127.0.0.1 --add-host aig.mycluster.cyou:192.168.4.203 --read-only --tmpfs /tmp:size=64m \\\n      --memory 512m --cpus 1 --pids-limit 64 -v "$CD:/count" -e CB2_CAP=8 -e CB2_COUNT_FILE=/count/requests.json cb2-proxy >/dev/null || fail "container did not start"',
     '    docker run -d --name "$NAME" --network cb2egress --dns 127.0.0.1 --add-host "$CB2_UPSTREAM:$CB2_UPSTREAM_IP" --read-only --tmpfs /tmp:size=64m \\\n      --memory 512m --cpus 1 --pids-limit 64 -v "$CD:/count" "${KEYMOUNT[@]}" -e CB2_CAP=8 -e CB2_COUNT_FILE=/count/requests.json \\\n      -e CB2_UPSTREAM="$CB2_UPSTREAM" -e CB2_UPSTREAM_IP="$CB2_UPSTREAM_IP" -e CB2_UPSTREAM_IPS="$CB2_UPSTREAM_IPS" -e CB2_RESOLVED_AT="$CB2_RESOLVED_AT" -e CB2_PROFILE="$CB2_PROFILE" -e CB2_MODEL="$CB2_MODEL" cb2-proxy >/dev/null || fail "container did not start"'),
    ('''    python3 -c "import json,sys; d=json.load(open('$CD/requests.json')); sys.exit(0 if d.get('tls_hostname_verified') is True and d.get('model_requests')==0 and d.get('upstream_errors')==0 else 1)" || fail "receipt not clean or TLS not verified"''',
     '''    WANTKEY=$([ -n "$CB2_KEY_FILE" ] && echo True || echo False)\n    python3 -c "import json,sys; d=json.load(open('$CD/requests.json')); sys.exit(0 if d.get('tls_hostname_verified') is True and d.get('model_requests')==0 and d.get('upstream_errors')==0 and d.get('upstream')=='$CB2_UPSTREAM' and d.get('upstream_ip')=='$CB2_UPSTREAM_IP' and d.get('key_injected') is $WANTKEY else 1)" || fail "receipt not clean, TLS not verified, or profile mismatch"'''),
])

# ── proxy.py: key injection, identity, error classes, response-model tally, usage ────────────
rw("proxy/proxy.py", [
    ('every request. Env: CB2_UPSTREAM (host), CB2_CAP (int), CB2_COUNT_FILE (path)."""',
     'every request. Env: CB2_UPSTREAM (host), CB2_UPSTREAM_IP, CB2_CAP (int), CB2_COUNT_FILE (path),\nCB2_KEY_PATH (optional: a file whose content replaces the Authorization header on EVERY forward —\nthe work containers then never hold the real key), CB2_PROFILE / CB2_MODEL / CB2_UPSTREAM_IPS /\nCB2_RESOLVED_AT (recorded). Error classes on model requests: upstream_http_errors = 429 or 5xx\n(infrastructure → void), upstream_client_errors = other 4xx (the caller\'s request; informational),\nupstream_errors = transport/TLS failures including a failed upstream body read; a client that\ndisconnects mid-stream is client_disconnects, never an upstream error. The receipt tallies the\n`model` id of every SUCCESSFUL model response and provider-reported usage counts; bodies are\nnever stored."""'),
    ('COUNT_FILE = os.environ.get("CB2_COUNT_FILE", "/count/requests.json")\n',
     'COUNT_FILE = os.environ.get("CB2_COUNT_FILE", "/count/requests.json")\nKEY_PATH = os.environ.get("CB2_KEY_PATH", "")\nKEY = open(KEY_PATH, encoding="utf-8").read().strip() if KEY_PATH else ""\n'),
    ('state = {"model_requests": 0, "refused_over_cap": 0, "forwarded_other": 0, "upstream_errors": 0, "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "cap": CAP, "by_path": {}}',
     'state = {"model_requests": 0, "refused_over_cap": 0, "forwarded_other": 0, "upstream_errors": 0, "upstream_http_errors": 0, "upstream_client_errors": 0, "client_disconnects": 0,\n         "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "cap": CAP, "by_path": {},\n         "profile": os.environ.get("CB2_PROFILE", ""), "model_expected": os.environ.get("CB2_MODEL", ""), "upstream": UPSTREAM, "upstream_ip": UPSTREAM_IP,\n         "upstream_ips": os.environ.get("CB2_UPSTREAM_IPS", ""), "resolved_at": os.environ.get("CB2_RESOLVED_AT", ""), "key_injected": bool(KEY),\n         "response_models": {}, "usage": {"responses_with_usage": 0, "prompt_tokens": 0, "completion_tokens": 0}}'),
    ('        headers["Host"] = UPSTREAM\n',
     '        headers["Host"] = UPSTREAM\n        if KEY:\n            headers = {k: v for k, v in headers.items() if k.lower() != "authorization"}\n            headers["Authorization"] = "Bearer " + KEY\n'),
    ('        self.send_response(resp.status)\n        chunked = False',
     '        if is_model_request and resp.status >= 400:\n            with lock:\n                state["upstream_http_errors" if (resp.status == 429 or resp.status >= 500) else "upstream_client_errors"] += 1\n                persist()\n        self.send_response(resp.status)\n        chunked = False\n        seen = bytearray()\n        client_gone = False'),
    ('        try:\n            while True:\n                chunk = resp.read(4096)\n                if not chunk:\n                    break\n                if chunked:\n                    self.wfile.write(f"{len(chunk):x}\\r\\n".encode() + chunk + b"\\r\\n")\n                else:\n                    self.wfile.write(chunk)\n                self.wfile.flush()\n            if chunked:\n                self.wfile.write(b"0\\r\\n\\r\\n")\n        except Exception:\n            pass\n        finally:\n            conn.close()',
     '        try:\n            while True:\n                try:\n                    chunk = resp.read(4096)\n                except Exception:\n                    # the UPSTREAM body read failed: a transport error (void class), model requests only\n                    if is_model_request:\n                        with lock:\n                            state["upstream_errors"] += 1\n                            persist()\n                    break\n                if not chunk:\n                    break\n                if is_model_request and resp.status < 400 and len(seen) < 4_000_000:\n                    seen.extend(chunk)\n                try:\n                    if chunked:\n                        self.wfile.write(f"{len(chunk):x}\\r\\n".encode() + chunk + b"\\r\\n")\n                    else:\n                        self.wfile.write(chunk)\n                    self.wfile.flush()\n                except Exception:\n                    client_gone = True   # the CLIENT went away: not an upstream fault\n                    break\n            if chunked and not client_gone:\n                try:\n                    self.wfile.write(b"0\\r\\n\\r\\n")\n                except Exception:\n                    client_gone = True\n        finally:\n            conn.close()\n        if client_gone:\n            with lock:\n                state["client_disconnects"] += 1\n                persist()\n        if is_model_request and resp.status < 400 and seen:\n            self._tally(bytes(seen))\n\n    def _tally(self, raw):\n        """From a SUCCESSFUL model response body: the `model` id (tallied) and provider-reported\n        usage (summed) — a JSON body, or SSE events. Counts only; the body is discarded."""\n        objs = []\n        try:\n            objs.append(json.loads(raw))\n        except Exception:\n            for line in raw.decode("utf-8", "replace").splitlines():\n                if line.startswith("data: ") and line[6:].strip() not in ("", "[DONE]"):\n                    try:\n                        objs.append(json.loads(line[6:]))\n                    except Exception:\n                        pass\n        models = {o.get("model") for o in objs if isinstance(o, dict) and isinstance(o.get("model"), str)}\n        usage = None\n        for o in objs:\n            if isinstance(o, dict) and isinstance(o.get("usage"), dict):\n                usage = o["usage"]\n        with lock:\n            for m in sorted(models):\n                state["response_models"][m[:80]] = state["response_models"].get(m[:80], 0) + 1\n            if not models:\n                state["response_models"]["(none)"] = state["response_models"].get("(none)", 0) + 1\n            if usage:\n                pt, ct = usage.get("prompt_tokens"), usage.get("completion_tokens")\n                if type(pt) is int and type(ct) is int and pt >= 0 and ct >= 0:\n                    state["usage"]["responses_with_usage"] += 1\n                    state["usage"]["prompt_tokens"] += pt\n                    state["usage"]["completion_tokens"] += ct\n            persist()'),
])

# ── cb2net.sh: exclusive allowlist from the run state, fail-closed; probes parameterised ─────
rw("net/cb2net.sh", [
    ('set -u\nGW=192.168.4.203; HERE="$(cd "$(dirname "$0")" && pwd)"\n',
     'set -u\nHERE="$(cd "$(dirname "$0")" && pwd)"; . "$HERE/../run/profile.sh"; cb2_profile_load "$HERE/.." || exit 1\nGW=$CB2_UPSTREAM_IP\n'),
    ('iptables -C DOCKER-USER -s 172.30.1.0/24 -d $GW -p tcp --dport 443 -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -s 172.30.1.0/24 -d $GW -p tcp --dport 443 -j ACCEPT\n',
     '# upstream ACCEPTs: exactly the run state\'s resolved addresses; an ACCEPT of this shape for any other destination (a previous profile) is removed\nfor RULE in $(iptables -S DOCKER-USER | grep -E -- "^-A DOCKER-USER -s 172.30.1.0/24 -d [0-9./]+ -p tcp -m tcp --dport 443 -j ACCEPT$" | awk \'{print $6}\'); do\n  KEEP=0; for IP in $CB2_UPSTREAM_IPS; do [ "$RULE" = "$IP/32" ] && KEEP=1; done\n  [ $KEEP = 1 ] || iptables -D DOCKER-USER -s 172.30.1.0/24 -d "$RULE" -p tcp --dport 443 -j ACCEPT || { echo "could not delete stale upstream rule $RULE"; exit 1; }\ndone\nfor IP in $CB2_UPSTREAM_IPS; do\n  iptables -C DOCKER-USER -s 172.30.1.0/24 -d $IP -p tcp --dport 443 -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -s 172.30.1.0/24 -d $IP -p tcp --dport 443 -j ACCEPT\ndone\n# FAIL CLOSED: every surviving ACCEPT for the egress subnet, whatever its shape, must name one of the resolved addresses as /32\nwhile read -r RULE; do\n  [ -n "$RULE" ] || continue\n  D=$(echo "$RULE" | grep -oE -- \'-d [0-9./]+\' | awk \'{print $2}\'); OK=0\n  for IP in $CB2_UPSTREAM_IPS; do [ "$D" = "$IP/32" ] && OK=1; done\n  [ $OK = 1 ] || { echo "CONTAINMENT NOT PROVEN: a foreign ACCEPT survives in DOCKER-USER: $RULE"; exit 1; }\ndone < <(iptables -S DOCKER-USER | grep -- "-s 172.30.1.0/24" | grep -- "-j ACCEPT")\n'),
    ('echo "networks: cb2net internal=$(docker network inspect cb2net --format \'{{.Internal}}\') subnet=172.30.0.0/24; cb2egress subnet=172.30.1.0/24; bridges work=$BR_WORK egress=$BR_EGRESS"',
     'echo "profile: $CB2_PROFILE upstream=$CB2_UPSTREAM addresses=[$CB2_UPSTREAM_IPS] resolved_at=$CB2_RESOLVED_AT run_state=$CB2_RUN_STATE"\necho "networks: cb2net internal=$(docker network inspect cb2net --format \'{{.Internal}}\') subnet=172.30.0.0/24; cb2egress subnet=172.30.1.0/24; bridges work=$BR_WORK egress=$BR_EGRESS"'),
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

# ── hermes leg ────────────────────────────────────────────────────────────────────────────────
rw("run/hermes_leg.sh", [
    ('T=$1; OUT=${2:-/root/cb2/out}; WALL=1800; CAP=8\nFIX="$(cd "$(dirname "$0")/.." && pwd)"\n',
     'T=$1; OUT=${2:-/root/cb2n/out}; WALL=1800; CAP=8\nFIX="$(cd "$(dirname "$0")/.." && pwd)"; export CB2_OUT="$OUT"; . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1\ncb2_rerun_prepare hermes "$T" "$OUT" || exit 2\n'),
    ("printf 'model:\\n  default: qwen3.8:27b-q4_K_M\\n  provider: custom\\n  base_url: http://%s:8080/v1\\nagent:\\n  max_turns: %s\\n' \"$PIP\" \"$CAP\" > \"$H/config.yaml\"",
     "printf 'model:\\n  default: %s\\n  provider: custom\\n  base_url: http://%s:8080/v1\\nagent:\\n  max_turns: %s\\n' \"$CB2_MODEL\" \"$PIP\" \"$CAP\" > \"$H/config.yaml\""),
    ('RC=$?\nEND=$(date -u +%Y-%m-%dT%H:%M:%SZ); WALLS=$(( $(date +%s) - T0 ))\ndocker rm -f "$NAME" >/dev/null 2>&1\n',
     'RC=$?\nEND=$(date -u +%Y-%m-%dT%H:%M:%SZ); WALLS=$(( $(date +%s) - T0 ))\nENVLEAK=$(cb2_env_leak_hits "$NAME")\ndocker rm -f "$NAME" >/dev/null 2>&1\n'),
    ('DL=$(grep -ciE "pip install|pip3 install|npm install|npm i |apt-get|apt install|curl |wget " "$RAW/hermes_${T}_stdout.txt")\n',
     'DL=$(grep -ciE "pip install|pip3 install|npm install|npm i |apt-get|apt install|curl |wget " "$RAW/hermes_${T}_stdout.txt")\nLEAK=$(( ${ENVLEAK:-0} + $(cb2_key_leak_hits "$W" "$H" "$RAW/hermes_${T}_stdout.txt") ))\nread -r PHTTP PTRANS PCLIENT MODEL_OK NMODELS UP UC UN <<< "$(python3 "$FIX/run/receipt_checks.py" "$CD/requests.json" "$CB2_MODEL" | sed -E \'s/[a-z_]+=//g\')"\n# VOID (infrastructure, never the agent\'s fault): an upstream 429/5xx on a model request or a transport/TLS failure\nVOID=false; { [ "$PHTTP" != 0 ] || [ "$PTRANS" != 0 ]; } && VOID=true\n'),
    ('DQ=false; [ "$VALID" = false ] && DQ=true; [ "$CALLS" -gt $CAP ] && DQ=true; [ "$DL" -gt 0 ] && DQ=true; [ $RC -ne 0 ] && DQ=true\n',
     '# disqualification: INDEPENDENT violations always; DEPENDENT ones (exit code, receipt zero-error shape, model identity) only when the leg is not void\nDQ_IND=false; [ "$VALID" = false ] && DQ_IND=true; [ "$CALLS" -gt $CAP ] && DQ_IND=true; [ "$DL" -gt 0 ] && DQ_IND=true; [ "$LEAK" != 0 ] && DQ_IND=true\nDQ_DEP=false; [ $RC -ne 0 ] && DQ_DEP=true; [ "$MODEL_OK" = true ] || DQ_DEP=true\n'),
    ("    ok=type(a) is int and type(r) is int and type(u) is int and 1<=a<=$CAP and r==0 and u==0 and t is True and a==int('$CALLS')",
     "    ok=type(a) is int and type(r) is int and type(u) is int and 1<=a<=$CAP and r==0 and t is True and a==int('$CALLS')"),
    ('[ "$RECEIPT_OK" = true ] || DQ=true\n', '[ "$RECEIPT_OK" = true ] || DQ_DEP=true\n'),
    ("echo \"$HASH\" | grep -Eq '^[0-9a-f]{64} files=[0-9]+ bytes=[0-9]+ symlinks=0 specials=0$' || DQ=true",
     "echo \"$HASH\" | grep -Eq '^[0-9a-f]{64} files=[0-9]+ bytes=[0-9]+ symlinks=0 specials=0$' || DQ_IND=true\nDQ=false; [ \"$DQ_IND\" = true ] && DQ=true; [ \"$VOID\" = false ] && [ \"$DQ_DEP\" = true ] && DQ=true"),
    ('"download_or_install_lines":%s,"proxy_receipt_ok":%s,"disqualified":%s,"tree":"%s"}',
     '"download_or_install_lines":%s,"proxy_receipt_ok":%s,"profile":"%s","upstream":"%s","upstream_ips":"%s","resolved_at":"%s","run_state":"%s","model":"%s","model_ok":%s,"key_leak_hits":%s,"upstream_http_errors":%s,"upstream_transport_errors":%s,"upstream_client_errors":%s,"usage_prompt_tokens":%s,"usage_completion_tokens":%s,"usage_responses":%s,"void":%s,"dq_independent":%s,"dq_dependent":%s,"disqualified":%s,"tree":"%s"}'),
    ('"$TOKS" "$DL" "$RECEIPT_OK" "$DQ" "$HASH"',
     '"$TOKS" "$DL" "$RECEIPT_OK" "$CB2_PROFILE" "$CB2_UPSTREAM" "$CB2_UPSTREAM_IPS" "$CB2_RESOLVED_AT" "$CB2_RUN_STATE" "$CB2_MODEL" "$MODEL_OK" "$LEAK" "$PHTTP" "$PTRANS" "$PCLIENT" "$UP" "$UC" "$UN" "$VOID" "$DQ_IND" "$DQ_DEP" "$DQ" "$HASH"'),
])

# ── mind leg ──────────────────────────────────────────────────────────────────────────────────
rw("run/mind_leg.sh", [
    ('T=$1; OUT=${2:-/root/cb2/out}; WALL=1800\nFIX="$(cd "$(dirname "$0")/.." && pwd)"\n',
     'T=$1; OUT=${2:-/root/cb2n/out}; WALL=1800\nFIX="$(cd "$(dirname "$0")/.." && pwd)"; export CB2_OUT="$OUT"; . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1\ncb2_rerun_prepare mind "$T" "$OUT" || exit 2\n'),
    ('NAME="cb2-mind-$T"; PROXY="cb2proxy-mind-$T"; PIP=172.30.0.2\n',
     'NAME="cb2-mind-$T"; PROXY="cb2proxy-mind-$T"; PIP=172.30.0.2\n# model lane by profile: "local" = the owned endpoint as the local/private lane (v3 behaviour);\n# "roles" = YM_PRIMARY_BRAIN=<provider>:<model> and all six roles equal to it, behind\n# YM_PROVIDER_BASE_URL_<PROVIDER> (the proxy), a placeholder key in the container, no local lane\n# (the scratch state holds nothing private).\nSPEC=""\nif [ "$CB2_MIND_LANE" = local ]; then\n  LANE=(-e YM_LOCAL_OLLAMA_URL="http://$PIP:8080" -e YM_LOCAL_OLLAMA_MODEL="$CB2_MODEL" -e YM_PRIVATE_PROVIDERS=ollama-local -e YM_HOUSEHOLD_PROVIDERS=ollama-local)\nelse\n  PU=$(echo "$CB2_MIND_PROVIDER" | tr \'a-z-\' \'A-Z_\'); SPEC="$CB2_MIND_PROVIDER:$CB2_MODEL"\n  LANE=(-e YM_PRIMARY_BRAIN="$SPEC" -e "YM_PROVIDER_BASE_URL_$PU=http://$PIP:8080/v1" -e "$CB2_MIND_KEY_ENV=none" -e YM_PRIVATE_PROVIDERS= -e YM_HOUSEHOLD_PROVIDERS="$CB2_MIND_PROVIDER,chain"\n        -e YM_ROLE_CHAT="$SPEC" -e YM_ROLE_RESEARCH="$SPEC" -e YM_ROLE_UTIL="$SPEC" -e YM_ROLE_VERIFY="$SPEC" -e YM_ROLE_CODE="$SPEC" -e YM_ROLE_CONSOLIDATE="$SPEC")\nfi\n'),
    ('  -e YM_OPERATOR=cb2 -e YM_TZ=Asia/Kolkata -e YM_LOCAL_OLLAMA_URL="http://$PIP:8080" -e YM_LOCAL_OLLAMA_MODEL=qwen3.8:27b-q4_K_M \\\n  -e YM_PRIVATE_PROVIDERS=ollama-local -e YM_HOUSEHOLD_PROVIDERS=ollama-local -e YM_INFER_PERMITS=2 \\\n',
     '  -e YM_OPERATOR=cb2 -e YM_TZ=Asia/Kolkata "${LANE[@]}" -e YM_INFER_PERMITS=2 \\\n'),
    ('docker logs "$NAME" > "$OUT/raw/mind_${T}_boot.txt" 2>&1 &\n',
     'docker logs "$NAME" > "$OUT/raw/mind_${T}_boot.txt" 2>&1 &\n'
     '# BRAIN GATE: under the roles lane the leg is aborted (disqualified, nothing graded) unless the\n'
     '# container env carries exactly six YM_ROLE_* equal to the spec plus YM_PRIMARY_BRAIN, no local\n'
     '# lane variable, no provider key other than the placeholder, and the boot log names the spec as\n'
     '# the cloud provider. Four booleans go into the receipt as brain_gate.\n'
     'BRAIN_GATE=\'{"lane":"local"}\'\n'
     'if [ "$CB2_MIND_LANE" = roles ]; then\n'
     '  ENVJ=$(docker inspect "$NAME" --format \'{{join .Config.Env "\\n"}}\')\n'
     '  ROLES=$(echo "$ENVJ" | grep -cE "^YM_ROLE_(CHAT|RESEARCH|UTIL|VERIFY|CODE|CONSOLIDATE)=$SPEC$"); PRIM=$(echo "$ENVJ" | grep -cF "YM_PRIMARY_BRAIN=$SPEC")\n'
     '  LOCALV=$(echo "$ENVJ" | grep -cE \'^(YM_LOCAL_OLLAMA_URL|YM_BRAIN_POOL)=\')\n'
     '  KEYS=$(echo "$ENVJ" | grep -cE \'^(NANOGPT_KEY|OLLAMA_CLOUD_KEY|MINIMAX_API_KEY|QWEN_API_KEY|OPEN_ROUTER_KEY|GROQ_API_KEY|CEREBRAS_API_KEY|GROK_API_KEY|ANTHROPIC_API_KEY|OPENAI_API_KEY)=\'); PLACE=$(echo "$ENVJ" | grep -cF "$CB2_MIND_KEY_ENV=none")\n'
     '  LABEL=0; for i in $(seq 1 30); do LABEL=$(docker logs "$NAME" 2>&1 | grep -cF "cloud provider \'$SPEC\'"); [ "$LABEL" != 0 ] && break; sleep 1; done\n'
     '  G1=$([ "$ROLES" = 6 ] && [ "$PRIM" = 1 ] && echo true || echo false); G2=$([ "$LOCALV" = 0 ] && echo true || echo false)\n'
     '  G3=$([ "$KEYS" = 0 ] && [ "$PLACE" = 1 ] && echo true || echo false); G4=$([ "$LABEL" = 1 ] && echo true || echo false)\n'
     '  BRAIN_GATE="{\\"roles_exact\\":$G1,\\"no_local_lane\\":$G2,\\"no_other_keys\\":$G3,\\"boot_label_exact\\":$G4}"\n'
     '  if [ "$G1$G2$G3$G4" != truetruetruetrue ]; then\n'
     '    echo "brain gate FAILED: $BRAIN_GATE — leg aborted, nothing graded"\n'
     '    printf \'{"system":"mind","task":"%s","status":"brain-gate-failed","brain_gate":%s,"disqualified":true,"void":false}\\n\' "$T" "$BRAIN_GATE" | tee "$R/mind_$T.json"; exit 5\n'
     '  fi\n'
     'fi\n'),
    ('docker logs "$NAME" > "$OUT/raw/mind_${T}_stdout.txt" 2>&1\ndocker rm -f "$NAME" >/dev/null 2>&1   # the parent stops the instance AFTER the driver wrote its receipt\n',
     'docker logs "$NAME" > "$OUT/raw/mind_${T}_stdout.txt" 2>&1\nENVLEAK=$(cb2_env_leak_hits "$NAME")\ndocker rm -f "$NAME" >/dev/null 2>&1   # the parent stops the instance AFTER the driver wrote its receipt\nLEAK=$(( ${ENVLEAK:-0} + $(cb2_key_leak_hits "$ST" "$OUT/raw/mind_${T}_stdout.txt" "$OUT/raw/mind_${T}_driver.txt") ))\nCHECKS=$(python3 "$FIX/run/receipt_checks.py" "$CD/requests.json" "$CB2_MODEL")\n'),
    ('python3 - "$ST/receipt.json" "$R/mind_$T.json" "$BIN_SHA" "$PROV" "$IMG" "$HASH" "$RC" "$T" "$CD/requests.json" <<\'EOF\'\nimport json, sys\nsrc, dst, bin_sha, prov, img, tree, rc, task, prx = sys.argv[1:]\n',
     'python3 - "$ST/receipt.json" "$R/mind_$T.json" "$BIN_SHA" "$PROV" "$IMG" "$HASH" "$RC" "$T" "$CD/requests.json" "$CB2_PROFILE" "$CB2_UPSTREAM" "$CB2_UPSTREAM_IPS" "$CB2_RESOLVED_AT" "$CB2_RUN_STATE" "$CB2_MODEL" "$LEAK" "$CHECKS" "$BRAIN_GATE" <<\'EOF\'\nimport json, sys\nsrc, dst, bin_sha, prov, img, tree, rc, task, prx, profile, upstream, ips, resolved_at, run_state, model, leak, checks, brain_gate = sys.argv[1:]\nck = dict(kv.split("=", 1) for kv in checks.split())\n'),
    ('    receipt_ok = type(acc) is int and type(ref) is int and type(p["upstream_errors"]) is int and acc >= 0 and ref >= 0 and acc <= 8 and ref == 0 and upe == 0 and tls\n',
     '    receipt_ok = type(acc) is int and type(ref) is int and type(p["upstream_errors"]) is int and acc >= 0 and ref >= 0 and acc <= 8 and ref == 0 and tls\n'),
    ('d["disqualified"] = bool(d.get("disqualified")) or (not receipt_ok) or (not capture_ok) or syml > 0 or special > 0 or int(rc) != 0\n',
     '# VOID (infrastructure): an upstream 429/5xx on a model request or a transport/TLS failure. INDEPENDENT violations\n# always disqualify; DEPENDENT ones (exit code, receipt shape, model identity) only when the leg is not void.\nvoid = ck.get("http_errors") != "0" or ck.get("transport_errors") != "0"\ndq_ind = bool(d.get("disqualified")) or (not capture_ok) or syml > 0 or special > 0 or int(leak) != 0\ndq_dep = (not receipt_ok) or int(rc) != 0 or ck.get("model_ok") != "true"\nd.update({"profile": profile, "upstream": upstream, "upstream_ips": ips, "resolved_at": resolved_at, "run_state": run_state, "model": model, "model_ok": ck.get("model_ok") == "true",\n          "key_leak_hits": int(leak), "brain_gate": json.loads(brain_gate), "upstream_http_errors": int(ck.get("http_errors", -1)), "upstream_transport_errors": int(ck.get("transport_errors", -1)),\n          "upstream_client_errors": int(ck.get("client_errors", -1)), "usage_prompt_tokens": int(ck.get("usage_p", 0)), "usage_completion_tokens": int(ck.get("usage_c", 0)),\n          "usage_responses": int(ck.get("usage_n", 0)), "void": void, "dq_independent": dq_ind, "dq_dependent": dq_dep})\nd["disqualified"] = dq_ind or ((not void) and dq_dep)\n'),
])

# ── smokes + cap test ─────────────────────────────────────────────────────────────────────────
rw("run/smoke_mind.sh", [
    ('FIX="$(cd "$(dirname "$0")/.." && pwd)"; OUT=$(mktemp -d /tmp/cb2-smoke-mind-XXXX)\n',
     'FIX="$(cd "$(dirname "$0")/.." && pwd)"; OUT=$(mktemp -d /tmp/cb2-smoke-mind-XXXX); . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1\nif [ "$CB2_MIND_LANE" = local ]; then\n  LANE=(-e YM_LOCAL_OLLAMA_URL=http://172.30.0.7:8080 -e YM_LOCAL_OLLAMA_MODEL="$CB2_MODEL" -e YM_PRIVATE_PROVIDERS=ollama-local -e YM_HOUSEHOLD_PROVIDERS=ollama-local); BOOTLINE="LOCAL primary + private lane active (ollama-local:$CB2_MODEL)"\nelse\n  PU=$(echo "$CB2_MIND_PROVIDER" | tr \'a-z-\' \'A-Z_\'); SPEC="$CB2_MIND_PROVIDER:$CB2_MODEL"\n  LANE=(-e YM_PRIMARY_BRAIN="$SPEC" -e "YM_PROVIDER_BASE_URL_$PU=http://172.30.0.7:8080/v1" -e "$CB2_MIND_KEY_ENV=none" -e YM_PRIVATE_PROVIDERS= -e YM_HOUSEHOLD_PROVIDERS="$CB2_MIND_PROVIDER,chain"\n        -e YM_ROLE_CHAT="$SPEC" -e YM_ROLE_RESEARCH="$SPEC" -e YM_ROLE_UTIL="$SPEC" -e YM_ROLE_VERIFY="$SPEC" -e YM_ROLE_CODE="$SPEC" -e YM_ROLE_CONSOLIDATE="$SPEC"); BOOTLINE="cloud provider \'$SPEC\'"\nfi\n'),
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

# ── MANIFEST: both profiles described without contradiction ──────────────────────────────────
rw("MANIFEST.json", [
    ('  "id": "E.CB2",\n  "version": 3,\n',
     '  "id": "E.CB2-N",\n  "version": 4,\n  "derived_from": "fixtures/cb2 at d4febe6 (the frozen Qwen reading; untouched) by the recorded patch scratch/cb2n_patch.py; scratch/rederive.sh proves the tree re-derives exactly",\n  "profiles": "profiles/<name>.profile, loaded by run/profile.sh (CB2_PROFILE, default qwen) THROUGH an immutable run state ($OUT/run_state.json: profile, upstream, resolved IPv4 addresses, first address, resolution time, model) written by the first loader call of a run and consumed unchanged by the network script, the proxy, both legs, the smokes and the cap test. qwen = the v3 reading unchanged: owned gateway 192.168.4.203, no key injection, the Mind on its local lane. nim (E.CB2-N) = upstream integrate.api.nvidia.com, its addresses resolved once (allowlisted EXCLUSIVELY: the network script fails closed if any other ACCEPT for the egress subnet survives), the key file (uid 10002, mode 0400) mounted read-only into the PROXY container only and injected as the Authorization header on every forward, placeholder keys in both work containers, one model for both systems (z-ai/glm-5.2), the Mind via YM_PRIMARY_BRAIN=nim:<model> with all six YM_ROLE_* equal to it behind YM_PROVIDER_BASE_URL_NIM (brain gate: env + boot log, fail closed). Key-leak scans hand the key FILE to grep as a pattern file (the key and its prefix never enter a variable, log or receipt); any hit in a work container\'s env, home, state, raw log or artifact disqualifies. Model identity: every model id tallied from SUCCESSFUL responses must equal the profile model, and at least one must exist, or the leg is disqualified (when not void). VOID = an upstream 429/5xx on a model request or a transport/TLS failure (a failed upstream body read included; a client disconnect is not): infrastructure, never a disqualification by itself; the first receipt and outputs are preserved as *_void1, exactly one same-leg rerun is allowed (run/profile.sh cb2_rerun_prepare), a second void refuses. Other 4xx are the caller\'s request errors: counted (upstream_client_errors), neither void nor rerun.",\n'),
    ('ACCEPT 172.30.1.0/24 → 192.168.4.203 tcp/443, DROP 172.30.1.0/24 → any.',
     'ACCEPT 172.30.1.0/24 → each address in the run state tcp/443 (qwen: 192.168.4.203; nim: the resolved integrate.api.nvidia.com addresses) and NOTHING else — any other ACCEPT for that subnet fails the script —, DROP 172.30.1.0/24 → any.'),
    ('forwards every request verbatim (streaming included) to 192.168.4.203:443 with Host aig.mycluster.cyou;',
     'forwards every request verbatim (streaming included) to the run state\'s upstream by hostname (qwen: aig.mycluster.cyou at 192.168.4.203; nim: integrate.api.nvidia.com at its resolved address, Authorization injected from the proxy-only key file) over hostname-verified TLS;'),
    ('YM_LOCAL_OLLAMA_URL = the run\'s proxy, YM_PRIVATE_PROVIDERS=YM_HOUSEHOLD_PROVIDERS=ollama-local, no cloud keys,',
     'lane by profile — qwen: YM_LOCAL_OLLAMA_URL = the run\'s proxy, YM_PRIVATE_PROVIDERS=YM_HOUSEHOLD_PROVIDERS=ollama-local; nim: YM_PRIMARY_BRAIN=nim:<model> and all six YM_ROLE_* = the same spec behind YM_PROVIDER_BASE_URL_NIM = the run\'s proxy, placeholder NVIDIA_API_KEY=none, no local lane, the brain gate enforced from the container env and the boot log — no other provider key in either lane,'),
    ('"config": "model.default qwen3.8:27b-q4_K_M, provider custom, base_url http://<run proxy>:8080/v1, agent.max_turns 8"',
     '"config": "model.default = the profile model (qwen: qwen3.8:27b-q4_K_M; nim: z-ai/glm-5.2), provider custom, base_url http://<run proxy>:8080/v1, agent.max_turns 8"'),
    ('"model": {"id": "qwen3.8:27b-q4_K_M", "endpoint": "https://aig.mycluster.cyou (192.168.4.203), reached only through the run\'s proxy", "rule": "the only model either system can reach — enforced by the network and the proxy, not by configuration"}',
     '"model": {"by_profile": {"qwen": "qwen3.8:27b-q4_K_M at https://aig.mycluster.cyou (192.168.4.203)", "nim": "z-ai/glm-5.2 at https://integrate.api.nvidia.com (addresses resolved once into the run state)"}, "endpoint": "reached only through the run\'s proxy", "rule": "the only model either system can reach — enforced by the network and the proxy, not by configuration; identical for both systems within a reading, checked from the successful responses\' model ids"}'),
    ('"invalidation": "a leg is disqualified on any refusal, a missing or malformed proxy receipt, tls_hostname_verified != true, upstream_errors > 0, any symlink or special filesystem node in the artifact, a non-zero driver exit, or the wall",',
     '"invalidation": "INDEPENDENT violations always disqualify: any refusal, a missing or malformed proxy receipt, tls_hostname_verified != true, a key-leak hit, any symlink or special filesystem node in the artifact, a download/install line, a failed brain gate, more calls than the cap. An upstream 429/5xx on a model request or a transport/TLS failure VOIDS the leg instead (infrastructure, one declared same-leg rerun). DEPENDENT violations — a non-zero exit (the wall included), a receipt failing its typed shape, a successful response whose model id differs from the profile model or no successful response at all — disqualify only when the leg is not void.",'),
    ('"downloads_installs": "impossible by network (internal network, no DNS, proxy forwards only to the gateway);',
     '"downloads_installs": "impossible by network (internal network, no DNS, proxy forwards only to the run state\'s upstream);'),
    ('"cost": {"both": "the proxy\'s model_requests per run (the same meter for both systems)",',
     '"cost": {"both": "the proxy\'s model_requests per run (the same meter for both systems) and, when the upstream reports usage, its prompt/completion token counts summed over successful responses",'),
    ('"a self-test whose failed-check set differs from the expected set at run time"]',
     '"a self-test whose failed-check set differs from the expected set at run time", "the real key readable from any work container (a key-leak hit anywhere)", "a successful response whose model id differs from the profile model", "a model differing between the two systems within a reading", "a NIM request before direct spend authorization from Pranab"]'),
])

# ── a distinct proxy image so the frozen cb2 image is never rebuilt ───────────────────────────
for p in ("run/proxy.sh", "net/cb2net.sh", "README.md", "MANIFEST.json"):
    s = io.open(R + p, encoding="utf-8").read()
    io.open(R + p, "w", encoding="utf-8", newline="\n").write(s.replace("cb2-proxy", "cb2n-proxy"))

# ── this file itself, recorded in the tree ────────────────────────────────────────────────────
os.makedirs(R + "scratch", exist_ok=True)
shutil.copyfile(os.path.abspath(__file__), R + "scratch/cb2n_patch.py")
print("cb2n patch applied to", R)
