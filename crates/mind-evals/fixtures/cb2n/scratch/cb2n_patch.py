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
CB2_MODEL=openai/gpt-oss-120b
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
  # The key is validated FIRST: a missing or wrongly-owned key must not leave a resolved run state
  # pinned behind it, or a later corrected load would silently inherit the first resolution.
  if [ -n "${CB2_KEY_FILE:-}" ]; then
    [ -s "$CB2_KEY_FILE" ] || { echo "profile: key file missing or empty"; return 1; }
    [ "$(stat -c '%u %a' "$CB2_KEY_FILE")" = "10002 400" ] || { echo "profile: key file must be uid 10002 (the proxy user) and mode 0400"; return 1; }
  fi
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
  CB2_RUN_STATE_SHA=$(sha256sum "$state" | cut -c1-64)
  export CB2_PROFILE=$name CB2_RUN_STATE=$state CB2_RUN_STATE_SHA CB2_UPSTREAM CB2_UPSTREAM_IP CB2_UPSTREAM_IPS CB2_RESOLVED_AT CB2_MODEL CB2_KEY_FILE CB2_MIND_LANE CB2_MIND_PROVIDER CB2_MIND_KEY_ENV
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
# start: either nothing exists for it, or exactly one prior receipt exists that is PURELY void —
# void true, disqualified false AND dq_independent false — and no _void1 archive exists yet, in
# which case every prior output of the leg is renamed *_void1 (preserved). A leg that broke a rule
# of its own never earns a rerun, however the infrastructure behaved alongside it.
cb2_rerun_prepare() {
  local sys=$1 t=$2 out=$3 rec="$3/receipts/${1}_${2}.json" x
  [ -e "$rec" ] || [ -e "$out/artifacts/${sys}_$t" ] || return 0
  [ -e "$out/receipts/${sys}_${t}_void1.json" ] && { echo "refusing: ${sys} $t already used its one rerun"; return 1; }
  python3 -c "import json,sys
d=json.load(open('$rec'))
sys.exit(0 if (d.get('void') is True and d.get('disqualified') is False and d.get('dq_independent') is not True) else 1)" 2>/dev/null || { echo "refusing: ${sys} $t exists and is not a PURE infrastructure void (one invocation per task)"; return 1; }
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
new("net/audit_probe.sh", """#!/bin/bash
# Does the containment audit actually FAIL when it should? Seeds a stray DOCKER-USER rule naming our
# subnet, runs net/cb2net.sh, and requires it to refuse with the stray-rule message; then removes the
# stray rule, runs it again, and requires it to pass. A fail-closed check that cannot be seen failing
# is not evidence (the first version of this audit shipped with a grep pattern that matched nothing).
# Root, on the box, between runs only: it edits iptables and refuses if anything is attached.
set -u
HERE="$(cd "$(dirname "$0")/.." && pwd)"
for NET in cb2net cb2egress; do
  A=$(docker network inspect $NET --format '{{len .Containers}}' 2>/dev/null || echo 0)
  [ "$A" = 0 ] || { echo "refusing: $NET has $A attached container(s)"; exit 2; }
done
# The seeded rule is a /32 HOST rule. The purge loop in cb2net.sh matches only the /24 forms and\n# the CB2-EGRESS jump, so this one SURVIVES the rebuild and reaches the audit, which is the point:\n# a /24 rule would simply be remediated and the probe would prove nothing about the audit.\nSTRAY=(-s 172.30.1.9/32 -d 8.8.8.8 -p tcp --dport 443 -j ACCEPT)
cleanup() { while iptables -C DOCKER-USER "${STRAY[@]}" 2>/dev/null; do iptables -D DOCKER-USER "${STRAY[@]}" || break; done; }
trap cleanup EXIT
iptables -I DOCKER-USER 1 "${STRAY[@]}" || { echo "could not seed the stray rule"; exit 2; }
OUT=$(bash "$HERE/net/cb2net.sh" 2>&1); RC=$?
cleanup
if [ $RC -eq 0 ]; then echo "AUDIT DID NOT FIRE: cb2net.sh accepted a stray DOCKER-USER rule"; exit 1; fi
STRAY_MSG="CONTAINMENT NOT PROVEN: a stray DOCKER-USER rule still names our subnets"\n# the EXACT message, not any refusal: a generic match once accepted a run that failed for an unrelated\n# reason (a malformed expected-policy string), which is how a broken audit passed for a broken reason.\necho "$OUT" | grep -qF "$STRAY_MSG" || { echo "AUDIT FIRED FOR THE WRONG REASON (rc=$RC):"; echo "$OUT" | tail -8; exit 1; }
echo "with a stray rule: refused (rc=$RC), with the exact stray-rule message"
OUT2=$(bash "$HERE/net/cb2net.sh" 2>&1); RC2=$?
[ $RC2 -eq 0 ] || { echo "AUDIT IS NOT REPEATABLE: a clean run failed (rc=$RC2):"; echo "$OUT2" | tail -5; exit 1; }
echo "$OUT2" | grep -q "containment proven" || { echo "clean run did not print containment proven"; exit 1; }
echo "without it: proven (rc=0). audit probe PASS"
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
Prints one line:
  valid=<true|false> http_errors=<n> transport_errors=<n> client_errors=<n> disconnects=<n>
  accepted=<n> refused=<n> model_ok=<true|false> models=<distinct> usage_p=<n> usage_c=<n> usage_n=<n>

`valid` is the load-bearing field: it is true only when the receipt exists and EVERY count is an
exact non-negative int (a bool, a string or a negative fails) and every response_models value is a
positive int. A caller must treat valid=false as an INDEPENDENT disqualification — a missing or
malformed receipt is not evidence of an infrastructure fault, so it can never be a void. When
valid is false every other field is a placeholder (-1 / false / 0) and means nothing.

model_ok is true iff at least one SUCCESSFUL model response was tallied and every tallied model
equals the expected model.\"\"\"
import json, sys

FALLBACK = ("valid=false http_errors=-1 transport_errors=-1 client_errors=-1 disconnects=-1 "
            "accepted=-1 refused=-1 model_ok=false models=0 usage_p=0 usage_c=0 usage_n=0")


def nn(x):
    return type(x) is int and x >= 0


def main():
    try:
        d = json.load(open(sys.argv[1]))
        want = sys.argv[2]
        http_err, trans = d["upstream_http_errors"], d["upstream_errors"]
        client, disc = d["upstream_client_errors"], d["client_disconnects"]
        acc, ref = d["model_requests"], d["refused_over_cap"]
        models, usage = d["response_models"], d["usage"]
        valid = (all(nn(x) for x in (http_err, trans, client, disc, acc, ref))
                 and isinstance(models, dict)
                 and all(isinstance(k, str) and type(v) is int and v > 0 for k, v in models.items())
                 and isinstance(usage, dict)
                 and all(nn(usage.get(k)) for k in ("responses_with_usage", "prompt_tokens", "completion_tokens")))
        if not valid:
            print(FALLBACK)
            return
        ok = bool(models) and all(k == want for k in models)
        print(f"valid=true http_errors={http_err} transport_errors={trans} client_errors={client} disconnects={disc} "
              f"accepted={acc} refused={ref} model_ok={str(ok).lower()} models={len(models)} "
              f"usage_p={usage['prompt_tokens']} usage_c={usage['completion_tokens']} usage_n={usage['responses_with_usage']}")
    except Exception:
        print(FALLBACK)


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
     '''# The egress policy lives in its OWN chain, rebuilt from scratch every run and verified rule for
# rule. Editing DOCKER-USER in place could only ever check the rules it knew to look for, and said
# nothing about their ORDER — a permissive rule sitting above them would have passed the audit.
iptables -N CB2-EGRESS 2>/dev/null || true
iptables -F CB2-EGRESS
iptables -A CB2-EGRESS -d 172.30.1.0/24 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
for IP in $CB2_UPSTREAM_IPS; do iptables -A CB2-EGRESS -s 172.30.1.0/24 -d "$IP" -p tcp --dport 443 -j ACCEPT; done
iptables -A CB2-EGRESS -s 172.30.1.0/24 -j DROP
# every DOCKER-USER rule that mentions our subnets goes, including previous jumps; then ONE jump at position 1
for _ in $(seq 1 32); do
  N=$(iptables -L DOCKER-USER --line-numbers -n | grep -E "172\\\\.30\\\\.[01]\\\\.0/24|CB2-EGRESS" | head -1 | awk \'{print $1}\')
  [ -z "$N" ] && break
  iptables -D DOCKER-USER "$N" || { echo "could not delete DOCKER-USER rule $N"; exit 1; }
done
iptables -I DOCKER-USER 1 -j CB2-EGRESS
# FAIL CLOSED: the chain must be EXACTLY the expected policy, in order, and the jump must be first.
NL=$\'\\n\'   # command substitution STRIPS trailing newlines, so the expected policy is joined explicitly
EXPECT="-N CB2-EGRESS${NL}-A CB2-EGRESS -d 172.30.1.0/24 -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT"
for IP in $CB2_UPSTREAM_IPS; do EXPECT="$EXPECT${NL}-A CB2-EGRESS -s 172.30.1.0/24 -d $IP/32 -p tcp -m tcp --dport 443 -j ACCEPT"; done
EXPECT="$EXPECT${NL}-A CB2-EGRESS -s 172.30.1.0/24 -j DROP"
GOT=$(iptables -S CB2-EGRESS)
[ "$GOT" = "$EXPECT" ] || { echo "CONTAINMENT NOT PROVEN: CB2-EGRESS is not the expected policy"; echo "--- got"; echo "$GOT"; echo "--- expected"; echo "$EXPECT"; exit 1; }
[ "$(iptables -S DOCKER-USER | sed -n 2p)" = "-A DOCKER-USER -j CB2-EGRESS" ] || { echo "CONTAINMENT NOT PROVEN: the CB2-EGRESS jump is not the first DOCKER-USER rule"; iptables -S DOCKER-USER; exit 1; }
[ "$(iptables -S DOCKER-USER | grep -c -- \'172\\.30\\.\')" = 0 ] || { echo "CONTAINMENT NOT PROVEN: a stray DOCKER-USER rule still names our subnets"; iptables -S DOCKER-USER | grep -- \'172\\.30\\.\'; exit 1; }
'''),
    # the DROP and the established-ACCEPT now live INSIDE CB2-EGRESS, in a verified order; leaving
    # these two in DOCKER-USER would put unaudited rules above the jump
    ('iptables -C DOCKER-USER -s 172.30.1.0/24 -j DROP 2>/dev/null || iptables -I DOCKER-USER 1 -s 172.30.1.0/24 -j DROP\n', ''),
    ('iptables -C DOCKER-USER -d 172.30.1.0/24 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -d 172.30.1.0/24 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT\n', ''),
    ('echo "networks: cb2net internal=$(docker network inspect cb2net --format \'{{.Internal}}\') subnet=172.30.0.0/24; cb2egress subnet=172.30.1.0/24; bridges work=$BR_WORK egress=$BR_EGRESS"',
     'echo "profile: $CB2_PROFILE upstream=$CB2_UPSTREAM addresses=[$CB2_UPSTREAM_IPS] resolved_at=$CB2_RESOLVED_AT run_state=$CB2_RUN_STATE"\necho "networks: cb2net internal=$(docker network inspect cb2net --format \'{{.Internal}}\') subnet=172.30.0.0/24; cb2egress subnet=172.30.1.0/24; bridges work=$BR_WORK egress=$BR_EGRESS"'),
    ('docker run -d --name cb2probe-proxy --network cb2egress --dns 127.0.0.1 --add-host aig.mycluster.cyou:$GW -e CB2_CAP=1 -e CB2_COUNT_FILE=/tmp/c.json cb2-proxy >/dev/null',
     'docker run -d --name cb2probe-proxy --network cb2egress --dns 127.0.0.1 --add-host "$CB2_UPSTREAM:$GW" -e CB2_UPSTREAM="$CB2_UPSTREAM" -e CB2_UPSTREAM_IP="$GW" -e CB2_CAP=1 -e CB2_COUNT_FILE=/tmp/c.json cb2-proxy >/dev/null'),
    ('W=$(docker run --rm --name cb2probe-work --network cb2net --add-host aig.mycluster.cyou:$GW --dns 127.0.0.1 -v "$HERE/probe_work.py:/probe.py:ro" python:3.13-slim python3 /probe.py 2>/dev/null)',
     'W=$(docker run --rm --name cb2probe-work --network cb2net --dns 127.0.0.1 -e CB2_UPSTREAM_IP="$GW" -v "$HERE/probe_work.py:/probe.py:ro" python:3.13-slim python3 /probe.py 2>/dev/null)'),
    ('iptables -S DOCKER-USER | grep "172.30" | sed \'s/^/rule: /\'; iptables -S INPUT | grep -E "$BR_WORK|$BR_EGRESS" | sed \'s/^/rule: /\'',
     'iptables -S CB2-EGRESS | sed \'s/^/rule: /\'; iptables -S DOCKER-USER | sed -n 2p | sed \'s/^/rule: /\'; iptables -S INPUT | grep -E "$BR_WORK|$BR_EGRESS" | sed \'s/^/rule: /\''),
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
    # A proxy that cannot come up (TLS preflight, listener, address) is INFRASTRUCTURE, before the
    # agent was ever invoked: void, not disqualified — otherwise the invalidation rule and the void
    # rule contradict each other on the same fault.
    ("printf '{\"system\":\"hermes\",\"task\":\"%s\",\"status\":\"proxy-not-ready\",\"disqualified\":true}\\n' \"$T\"",
      "printf '{\"system\":\"hermes\",\"task\":\"%s\",\"status\":\"proxy-not-ready\",\"void\":true,\"dq_independent\":false,\"dq_dependent\":false,\"disqualified\":false}\\n' \"$T\""),
    ('T=$1; OUT=${2:-/root/cb2/out}; WALL=1800; CAP=8\nFIX="$(cd "$(dirname "$0")/.." && pwd)"\n',
     'T=$1; OUT=${2:-/root/cb2n/out}; WALL=1800; CAP=8\nFIX="$(cd "$(dirname "$0")/.." && pwd)"; export CB2_OUT="$OUT"; . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1\ncb2_rerun_prepare hermes "$T" "$OUT" || exit 2\n'),
    ("printf 'model:\\n  default: qwen3.8:27b-q4_K_M\\n  provider: custom\\n  base_url: http://%s:8080/v1\\nagent:\\n  max_turns: %s\\n' \"$PIP\" \"$CAP\" > \"$H/config.yaml\"",
     "printf 'model:\\n  default: %s\\n  provider: custom\\n  base_url: http://%s:8080/v1\\nagent:\\n  max_turns: %s\\n' \"$CB2_MODEL\" \"$PIP\" \"$CAP\" > \"$H/config.yaml\""),
    ('RC=$?\nEND=$(date -u +%Y-%m-%dT%H:%M:%SZ); WALLS=$(( $(date +%s) - T0 ))\ndocker rm -f "$NAME" >/dev/null 2>&1\n',
     'RC=$?\nEND=$(date -u +%Y-%m-%dT%H:%M:%SZ); WALLS=$(( $(date +%s) - T0 ))\nENVLEAK=$(cb2_env_leak_hits "$NAME")\ndocker rm -f "$NAME" >/dev/null 2>&1\n'),
    ('DL=$(grep -ciE "pip install|pip3 install|npm install|npm i |apt-get|apt install|curl |wget " "$RAW/hermes_${T}_stdout.txt")\n',
     'DL=$(grep -ciE "pip install|pip3 install|npm install|npm i |apt-get|apt install|curl |wget " "$RAW/hermes_${T}_stdout.txt")\n'
     'LEAK=$(( ${ENVLEAK:-0} + $(cb2_key_leak_hits "$W" "$H" "$RAW/hermes_${T}_stdout.txt") ))\n'
     'read -r RC_VALID PHTTP PTRANS PCLIENT PDISC RC_ACC RC_REF MODEL_OK NMODELS UP UC UN <<< "$(python3 "$FIX/run/receipt_checks.py" "$CD/requests.json" "$CB2_MODEL" | sed -E \'s/[a-z_]+=//g\')"\n'
     '# VOID (infrastructure, never the agent\'s fault): a TYPED receipt showing an upstream 429/5xx on a\n'
     '# model request or a transport/TLS failure. An untyped or missing receipt is NOT evidence of an\n'
     '# infrastructure fault — it is an independent disqualification below.\n'
     'VOID=false\n'
     'if [ "$RC_VALID" = true ] && { [ "$PHTTP" != 0 ] || [ "$PTRANS" != 0 ]; }; then VOID=true; fi\n'),
    ('DQ=false; [ "$VALID" = false ] && DQ=true; [ "$CALLS" -gt $CAP ] && DQ=true; [ "$DL" -gt 0 ] && DQ=true; [ $RC -ne 0 ] && DQ=true\n',
     '# disqualification: INDEPENDENT violations always — including the CAP EVIDENCE, which must stand\n'
     '# on its own: a refusal or an over-cap count is the agent exceeding its budget whatever the\n'
     '# upstream was doing at the time. DEPENDENT ones (exit code/wall, receipt shape, model identity)\n'
     '# only when the leg is not void.\n'
     'DQ_IND=false; [ "$VALID" = false ] && DQ_IND=true; [ "$CALLS" -gt $CAP ] && DQ_IND=true; [ "$DL" -gt 0 ] && DQ_IND=true; [ "$LEAK" != 0 ] && DQ_IND=true\n'
     '[ "$RC_VALID" = true ] || DQ_IND=true          # missing/malformed proxy receipt: independent, never a void\n'
     'if [ "$RC_VALID" = true ]; then { [ "$RC_REF" -gt 0 ] || [ "$RC_ACC" -gt $CAP ]; } && DQ_IND=true; fi\n'
     'DQ_DEP=false; [ $RC -ne 0 ] && DQ_DEP=true; [ "$MODEL_OK" = true ] || DQ_DEP=true\n'),
    ("    ok=type(a) is int and type(r) is int and type(u) is int and 1<=a<=$CAP and r==0 and u==0 and t is True and a==int('$CALLS')",
     "    ok=type(a) is int and type(r) is int and type(u) is int and 1<=a<=$CAP and r==0 and t is True and a==int('$CALLS')"),
    ('[ "$RECEIPT_OK" = true ] || DQ=true\n', '[ "$RECEIPT_OK" = true ] || DQ_DEP=true\n'),
    ("echo \"$HASH\" | grep -Eq '^[0-9a-f]{64} files=[0-9]+ bytes=[0-9]+ symlinks=0 specials=0$' || DQ=true",
     "echo \"$HASH\" | grep -Eq '^[0-9a-f]{64} files=[0-9]+ bytes=[0-9]+ symlinks=0 specials=0$' || DQ_IND=true\nDQ=false; [ \"$DQ_IND\" = true ] && DQ=true; [ \"$VOID\" = false ] && [ \"$DQ_DEP\" = true ] && DQ=true"),
    ('"download_or_install_lines":%s,"proxy_receipt_ok":%s,"disqualified":%s,"tree":"%s"}',
     '"download_or_install_lines":%s,"proxy_receipt_ok":%s,"profile":"%s","upstream":"%s","upstream_ips":"%s","resolved_at":"%s","run_state":"%s","run_state_sha256":"%s","model":"%s","model_ok":%s,"receipt_valid":%s,"key_leak_hits":%s,"upstream_http_errors":%s,"upstream_transport_errors":%s,"upstream_client_errors":%s,"client_disconnects":%s,"usage_prompt_tokens":%s,"usage_completion_tokens":%s,"usage_responses":%s,"void":%s,"dq_independent":%s,"dq_dependent":%s,"disqualified":%s,"tree":"%s"}'),
    ('"$TOKS" "$DL" "$RECEIPT_OK" "$DQ" "$HASH"',
     '"$TOKS" "$DL" "$RECEIPT_OK" "$CB2_PROFILE" "$CB2_UPSTREAM" "$CB2_UPSTREAM_IPS" "$CB2_RESOLVED_AT" "$CB2_RUN_STATE" "$CB2_RUN_STATE_SHA" "$CB2_MODEL" "$MODEL_OK" "$RC_VALID" "$LEAK" "$PHTTP" "$PTRANS" "$PCLIENT" "$PDISC" "$UP" "$UC" "$UN" "$VOID" "$DQ_IND" "$DQ_DEP" "$DQ" "$HASH"'),
])

# ── mind leg ──────────────────────────────────────────────────────────────────────────────────
rw("run/mind_leg.sh", [
    ("printf '{\"system\":\"mind\",\"task\":\"%s\",\"status\":\"proxy-not-ready\",\"disqualified\":true}\\n' \"$T\"",
      "printf '{\"system\":\"mind\",\"task\":\"%s\",\"status\":\"proxy-not-ready\",\"void\":true,\"dq_independent\":false,\"dq_dependent\":false,\"disqualified\":false}\\n' \"$T\""),
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
     'python3 - "$ST/receipt.json" "$R/mind_$T.json" "$BIN_SHA" "$PROV" "$IMG" "$HASH" "$RC" "$T" "$CD/requests.json" "$CB2_PROFILE" "$CB2_UPSTREAM" "$CB2_UPSTREAM_IPS" "$CB2_RESOLVED_AT" "$CB2_RUN_STATE" "$CB2_RUN_STATE_SHA" "$CB2_MODEL" "$LEAK" "$CHECKS" "$BRAIN_GATE" <<\'EOF\'\nimport json, sys\nsrc, dst, bin_sha, prov, img, tree, rc, task, prx, profile, upstream, ips, resolved_at, run_state, run_state_sha, model, leak, checks, brain_gate = sys.argv[1:]\nck = dict(kv.split("=", 1) for kv in checks.split())\n'),
    ('    receipt_ok = type(acc) is int and type(ref) is int and type(p["upstream_errors"]) is int and acc >= 0 and ref >= 0 and acc <= 8 and ref == 0 and upe == 0 and tls\n',
     '    receipt_ok = type(acc) is int and type(ref) is int and type(p["upstream_errors"]) is int and acc >= 0 and ref >= 0 and acc <= 8 and ref == 0 and tls\n'),
    # a receipt the driver never wrote is missing EVIDENCE, not an infrastructure fault
    ('    d = {"system": "mind", "task": task, "status": "driver-failed", "disqualified": True}\n',
     '    d = {"system": "mind", "task": task, "status": "driver-failed", "dq_independent": True, "disqualified": True}\n'),
    ('d["disqualified"] = bool(d.get("disqualified")) or (not receipt_ok) or (not capture_ok) or syml > 0 or special > 0 or int(rc) != 0\n',
     '# VOID (infrastructure) needs a TYPED proxy receipt showing an upstream 429/5xx on a model request or a\n# transport/TLS failure. A missing or malformed receipt is not that evidence: it is an INDEPENDENT violation.\n# Independent violations always disqualify (the driver\'s own independent reasons, capture, symlinks/specials,\n# a key leak, an untyped receipt); DEPENDENT ones (the wall or a non-zero exit, the receipt\'s zero-refusal\n# shape, model identity) only when the leg is not void.\nvalid = ck.get("valid") == "true"\nvoid = valid and (ck.get("http_errors") != "0" or ck.get("transport_errors") != "0")\ndq_ind = bool(d.get("dq_independent")) or (not capture_ok) or syml > 0 or special > 0 or int(leak) != 0 or (not valid)\ndq_dep = bool(d.get("dq_dependent")) or (not receipt_ok) or int(rc) != 0 or ck.get("model_ok") != "true"\nd.update({"profile": profile, "upstream": upstream, "upstream_ips": ips, "resolved_at": resolved_at, "run_state": run_state, "run_state_sha256": run_state_sha,\n          "model": model, "model_ok": ck.get("model_ok") == "true", "receipt_valid": valid, "key_leak_hits": int(leak), "brain_gate": json.loads(brain_gate),\n          "upstream_http_errors": int(ck.get("http_errors", -1)), "upstream_transport_errors": int(ck.get("transport_errors", -1)),\n          "upstream_client_errors": int(ck.get("client_errors", -1)), "client_disconnects": int(ck.get("disconnects", -1)),\n          "usage_prompt_tokens": int(ck.get("usage_p", 0)), "usage_completion_tokens": int(ck.get("usage_c", 0)),\n          "usage_responses": int(ck.get("usage_n", 0)), "void": void, "dq_independent": dq_ind, "dq_dependent": dq_dep})\nd["disqualified"] = dq_ind or ((not void) and dq_dep)\n'),
])

# ── mind driver: name its OWN disqualification reasons, split independent from dependent ─────
rw("run/mind_driver.py", [
    # The routed kind belongs in the receipt: with two executors the router may refine the floor,
    # and a leg that produced nothing because it ran as `research` is a routing OUTCOME, not a
    # build failure. Nothing downstream could tell those apart (review 4, finding 5).
    ('status, result, stop = "running", "", None',
     'status, result, stop = "running", "", None\nrouted_kind = ""'),
    ('        status = mine[-1].get("status", "?"); result = mine[-1].get("result") or ""',
     '        status = mine[-1].get("status", "?"); result = mine[-1].get("result") or ""\n        routed_kind = mine[-1].get("kind") or routed_kind'),
    ('"files_added": len(added), "result_bytes": len(result.encode("utf-8")),',
     '"files_added": len(added), "result_bytes": len(result.encode("utf-8")), "routed_kind": routed_kind,'),
    ('''           "disqualified": (not present) or bad > 0 or refused > 0 or accepted > CAP or stop is not None}''',
     '''           "proxy_client_disconnects": -1,
           # The driver's own reasons, split. INDEPENDENT: evidence the run broke a rule of its own —
           # a missing or malformed proxy receipt, a refusal (a ninth request was attempted), an
           # over-cap count, a malformed ledger row. These stand whatever the upstream was doing.
           # DEPENDENT: the wall. A transport outage that ends in a timeout must not become an
           # independent violation, or an infrastructure void would also disqualify the leg.
           "dq_independent": (not present) or bad > 0 or refused > 0 or accepted > CAP,
           "dq_dependent": stop == "timeout",
           "disqualified": (not present) or bad > 0 or refused > 0 or accepted > CAP or stop is not None}'''),
    ('''receipt = {"system": "mind", "task": T, "started": started, "finished": finished, "wall_s": wall, "status": status,''',
     '''receipt = {"system": "mind", "task": T, "started": started, "finished": finished, "wall_s": wall, "status": status,
           "stop_reason": stop or "",'''),
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
    # The binary changed under this claim: a `code`-classified brief is no longer refused, it is
    # floored onto an executor the box has. Leaving the old sentence in the manifest that accompanies
    # a graded receipt would describe a system that no longer exists (review 4, finding 4).
    ("\"known_limit\": \"no code executor on this binary: a brief routed to `code` reads 'not runnable as configured'\"",
     "\"known_limit\": \"the coder needs an API key this scratch instance does not have, so the executors present are `page` and `research`. Since E.PORT1-B a brief the classifier calls `code` is NOT refused: the deterministic floor is constrained to the executors that exist, so it runs on `page`. The router may still refine the kind within its budget, so the receipt records the kind the job actually ran as, and a leg that produced no artifact because it was routed to `research` is a routing outcome rather than a build failure.\""),
    ('"proxy_start": "fail-closed: the leg aborts (receipt status proxy-not-ready, disqualified) unless the proxy started',
     '"proxy_start": "fail-closed and PRE-INVOCATION: the leg aborts with a receipt of status proxy-not-ready, void true and disqualified false (nothing was graded, and the one declared rerun applies) unless the proxy started'),
    ('  "id": "E.CB2",\n  "version": 3,\n',
     '  "id": "E.CB2-N",\n  "version": 4,\n  "derived_from": "fixtures/cb2 at d4febe6 (the frozen Qwen reading; untouched) by the recorded patch scratch/cb2n_patch.py; scratch/rederive.sh proves the tree re-derives exactly",\n  "profiles": "profiles/<name>.profile, loaded by run/profile.sh (CB2_PROFILE, default qwen) THROUGH an immutable run state ($OUT/run_state.json: profile, upstream, resolved IPv4 addresses, first address, resolution time, model) written by the first loader call of a run and consumed unchanged by the network script, the proxy, both legs, the smokes and the cap test. qwen = the v3 reading unchanged: owned gateway 192.168.4.203, no key injection, the Mind on its local lane. nim (E.CB2-N) = upstream integrate.api.nvidia.com, its addresses resolved once (allowlisted EXCLUSIVELY: the network script fails closed if any other ACCEPT for the egress subnet survives), the key file (uid 10002, mode 0400) mounted read-only into the PROXY container only and injected as the Authorization header on every forward, placeholder keys in both work containers, one model for both systems (openai/gpt-oss-120b), the Mind via YM_PRIMARY_BRAIN=nim:<model> with all six YM_ROLE_* equal to it behind YM_PROVIDER_BASE_URL_NIM (brain gate: env + boot log, fail closed). Key-leak scans hand the key FILE to grep as a pattern file (the key and its prefix never enter a variable, log or receipt); any hit in a work container\'s env, home, state, raw log or artifact disqualifies. Model identity: every model id tallied from SUCCESSFUL responses must equal the profile model, and at least one must exist, or the leg is disqualified (when not void). VOID = a TYPED proxy receipt showing an upstream 429/5xx on a model request or a transport/TLS failure (a failed upstream body read included; a client disconnect is counted separately and is not): infrastructure, never a disqualification by itself. A MISSING OR MALFORMED receipt is not that evidence: it is an independent disqualification. Cap evidence (a refusal, or accepted > cap) is independent too and stands whatever the upstream was doing; the Mind driver splits its own reasons the same way, independent (missing/malformed proxy receipt, a refusal, over-cap, a malformed ledger row) against dependent (the wall). A void leg preserves its first receipt and outputs as *_void1 and may be rerun exactly once, and only when it is PURELY void (void true, disqualified false, dq_independent false); a second void refuses. Other 4xx are the request errors of the caller: counted (upstream_client_errors), neither void nor rerun. A proxy that cannot come up writes a pre-invocation receipt with void true and disqualified false: nothing was graded. Egress policy lives in its own iptables chain CB2-EGRESS, flushed and rebuilt every run and jumped to from DOCKER-USER position 1 (established/related, one ACCEPT per resolved address on tcp/443, then DROP); the audit compares that chain rule for rule against the expected policy and fails closed on any difference, on a jump that is not first, or on any surviving DOCKER-USER rule naming the subnets. Each profile uses its own output root (/root/cb2n/out-qwen, /root/cb2n/out-nim), and every receipt carries the path and SHA-256 of the run state.",\n'),
    ('DOCKER-USER: ACCEPT established/related → 172.30.1.0/24, ACCEPT 172.30.1.0/24 → 192.168.4.203 tcp/443, DROP 172.30.1.0/24 → any.',
     'the egress policy is NOT a set of loose DOCKER-USER rules but a dedicated chain CB2-EGRESS, flushed and rebuilt every run and reached by ONE jump inserted at DOCKER-USER position 1; its rules in order are ACCEPT established/related → 172.30.1.0/24, then one ACCEPT 172.30.1.0/24 → <address> tcp/443 per address in the run state (qwen: 192.168.4.203; nim: the resolved integrate.api.nvidia.com addresses), then DROP 172.30.1.0/24 → any. The audit fails closed unless the chain matches that policy rule for rule, the jump is the FIRST DOCKER-USER rule, and no other DOCKER-USER rule names either subnet; net/audit_probe.sh proves the last of those by seeding a stray rule and requiring the audit to reject it.'),
    ('forwards every request verbatim (streaming included) to 192.168.4.203:443 with Host aig.mycluster.cyou;',
     'forwards every request verbatim (streaming included) to the run state\'s upstream by hostname (qwen: aig.mycluster.cyou at 192.168.4.203; nim: integrate.api.nvidia.com at its resolved address, Authorization injected from the proxy-only key file) over hostname-verified TLS;'),
    ('YM_LOCAL_OLLAMA_URL = the run\'s proxy, YM_PRIVATE_PROVIDERS=YM_HOUSEHOLD_PROVIDERS=ollama-local, no cloud keys,',
     'lane by profile — qwen: YM_LOCAL_OLLAMA_URL = the run\'s proxy, YM_PRIVATE_PROVIDERS=YM_HOUSEHOLD_PROVIDERS=ollama-local; nim: YM_PRIMARY_BRAIN=nim:<model> and all six YM_ROLE_* = the same spec behind YM_PROVIDER_BASE_URL_NIM = the run\'s proxy, placeholder NVIDIA_API_KEY=none, no local lane, the brain gate enforced from the container env and the boot log — no other provider key in either lane,'),
    ('"config": "model.default qwen3.8:27b-q4_K_M, provider custom, base_url http://<run proxy>:8080/v1, agent.max_turns 8"',
     '"config": "model.default = the profile model (qwen: qwen3.8:27b-q4_K_M; nim: openai/gpt-oss-120b), provider custom, base_url http://<run proxy>:8080/v1, agent.max_turns 8"'),
    ('"model": {"id": "qwen3.8:27b-q4_K_M", "endpoint": "https://aig.mycluster.cyou (192.168.4.203), reached only through the run\'s proxy", "rule": "the only model either system can reach — enforced by the network and the proxy, not by configuration"}',
     '"model": {"by_profile": {"qwen": "qwen3.8:27b-q4_K_M at https://aig.mycluster.cyou (192.168.4.203)", "nim": "openai/gpt-oss-120b at https://integrate.api.nvidia.com (addresses resolved once into the run state)"}, "endpoint": "reached only through the run\'s proxy", "rule": "the only model either system can reach — enforced by the network and the proxy, not by configuration; identical for both systems within a reading, checked from the successful responses\' model ids"}'),
    ('"invalidation": "a leg is disqualified on any refusal, a missing or malformed proxy receipt, tls_hostname_verified != true, upstream_errors > 0, any symlink or special filesystem node in the artifact, a non-zero driver exit, or the wall",',
     '"invalidation": "Three classifications with a PRECEDENCE, not three disjoint buckets: an independent violation and an infrastructure void can hold at once, and the violation still disqualifies. INDEPENDENT violations disqualify unconditionally: an untyped or missing proxy receipt (receipt_valid false), a refusal or accepted > cap in that receipt, more calls than the cap in the own log of the agent, a download or install line, a key-leak hit, a symlink or special filesystem node, a failed capture, a failed brain gate, and the independent reasons the Mind driver reports (missing or malformed proxy receipt, a refusal seen, accepted > cap, a malformed ledger row). VOID (infrastructure, not the agent): a typed receipt showing an upstream 429/5xx on a model request or a transport/TLS failure, and a proxy that could not start at all (pre-invocation: void true, disqualified false, nothing graded). A void SUPPRESSES ONLY the dependent class and never rescues an independent violation. DEPENDENT violations disqualify only when the leg is not void: a non-zero exit including the wall, the agreement checks on the proxy receipt (1 <= accepted <= cap, refused 0, tls_hostname_verified true, accepted == the calls in the own log of the agent), and model identity.",'),
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

# ── review-5 corrections: the Mind leg is held to the SAME accounting rule as the Hermes leg ──
# Three defects an adversarial review found in this harness, all of them ways the comparison was not
# a comparison. (1) MANIFEST.json already promised "accepted == the calls in the own log of the
# agent" as a dependent violation for BOTH systems, and only the Hermes leg implemented it — the
# Mind's own spend ledger went into the receipt and was compared to nothing. (2) The Hermes receipt
# demands `1 <= accepted`; the Mind's accepted zero, so a Mind leg that never reached the model could
# pass a check its opponent could not. (3) `spend_rows()` returned zeros for a MISSING log, making
# "the mind could not account for itself" indistinguishable from "the mind made no calls", and
# `proxy_client_disconnects` was the literal -1 — a placeholder in the shape of a measurement. Plus
# `routed_kind`, promised by the schema and absent on the three exit paths that abort before the
# driver runs, so a reader indexing it crashed on exactly the legs whose failure it wanted to read.
rw("run/mind_driver.py", [
    ('    p = STATE / "mind.db.decisions.jsonl"; req = att = bad = 0\n    if not p.exists():\n        return 0, 0, 0',
     '    p = STATE / "mind.db.decisions.jsonl"; req = att = bad = 0\n    if not p.exists():\n        # NOT (0, 0, 0): "the mind\'s own spend log was not there" and "the mind made no calls" are\n        # different facts and were the same number in the receipt. -1 is unmistakably not a count,\n        # and a missing log is an INDEPENDENT violation below — the analogue of Hermes\'s invalid log.\n        return -1, -1, -1'),
    ('def proxy_count():\n    """(accepted, refused, present). Absent or unreadable → (-1, -1, False): fail closed."""\n    try:\n        d = json.load(open(os.path.join(COUNT_DIR, "requests.json")))\n        return int(d.get("model_requests", -1)), int(d.get("refused_over_cap", -1)), True\n    except Exception:\n        return -1, -1, False',
     'def proxy_count():\n    """(accepted, refused, disconnects, present). Absent or unreadable → (-1, -1, -1, False)."""\n    try:\n        d = json.load(open(os.path.join(COUNT_DIR, "requests.json")))\n        return (int(d.get("model_requests", -1)), int(d.get("refused_over_cap", -1)),\n                int(d.get("client_disconnects", -1)), True)\n    except Exception:\n        return -1, -1, -1, False'),
    ('    _, refused, present = proxy_count()',
     '    _, refused, _, present = proxy_count()'),
    ('req, att, bad = spend_rows(); accepted, refused, present = proxy_count()',
     "# Proxy FIRST, own log SECOND. Both are read after the job ended, but a request accepted at the\n# proxy writes its ledger row only when it completes, so reading the log last narrows the window in\n# which the two disagree for a reason that is not misconduct.\naccepted, refused, disconnects, present = proxy_count(); req, att, bad = spend_rows()\n# THE AGREEMENT CHECK, which the manifest promises for BOTH systems and only Hermes was held to:\n# every model request that left the box must appear in the agent's own accounting. `ledger_attempts`\n# is the comparable number — one attempt is one HTTP request, while one `inference_call` row may\n# carry several. Dependent, exactly as `a == CALLS` is dependent on the Hermes side.\naccounting_agrees = present and att >= 0 and att == accepted"),
    ('           "proxy_client_disconnects": -1,',
     '           # Was the literal -1 — a placeholder that read as a measurement of zero disconnects\n           # having been impossible to obtain. The proxy counts them; this reports what it counted.\n           "proxy_client_disconnects": disconnects,\n           "accounting_agrees": accounting_agrees,'),
    ('           "dq_independent": (not present) or bad > 0 or refused > 0 or accepted > CAP,\n           "dq_dependent": stop == "timeout",\n           "disqualified": (not present) or bad > 0 or refused > 0 or accepted > CAP or stop is not None}',
     '           # `req < 0` is the missing spend log: the mind cannot account for itself at all.\n           "dq_independent": (not present) or bad > 0 or req < 0 or refused > 0 or accepted > CAP,\n           # `accepted < 1` matches Hermes\'s `1 <= a`: a leg that reached the model zero times is\n           # not a fast win, and the receipt used to accept it on the Mind side alone.\n           "dq_dependent": stop == "timeout" or (not accounting_agrees) or accepted < 1,\n           "disqualified": (not present) or bad > 0 or req < 0 or refused > 0 or accepted > CAP\n                           or (not accounting_agrees) or accepted < 1 or stop is not None}'),
])
rw("run/mind_leg.sh", [
    ('"status":"proxy-not-ready",',
     '"status":"proxy-not-ready","routed_kind":null,'),
    ('"status":"brain-gate-failed","brain_gate":%s,',
     '"status":"brain-gate-failed","brain_gate":%s,"routed_kind":null,'),
    ('    d = {"system": "mind", "task": task, "status": "driver-failed", "dq_independent": True, "disqualified": True}',
     '    # `routed_kind` is in the schema, so it is present on EVERY exit path. It was absent on the\n    # three that abort before the driver writes a receipt, and any reader indexing the field\n    # crashed on exactly the legs whose failure it was trying to read.\n    d = {"system": "mind", "task": task, "status": "driver-failed", "routed_kind": None,\n         "dq_independent": True, "disqualified": True}'),
    ('    receipt_ok = type(acc) is int and type(ref) is int and type(p["upstream_errors"]) is int and acc >= 0 and ref >= 0 and acc <= 8 and ref == 0 and tls',
     '    # `1 <= acc`, not `acc >= 0`: Hermes\'s receipt check requires at least one model request and\n    # the Mind\'s accepted zero, so a leg that never reached the model passed on one side only.\n    receipt_ok = type(acc) is int and type(ref) is int and type(p["upstream_errors"]) is int and 1 <= acc <= 8 and ref >= 0 and ref == 0 and tls'),
])

# ── review-5, part two: the receipt decision becomes testable ─────────────────────────────────
# The three fixes above were made inline in the driver's receipt literal, where the previous three
# defects had also lived — a rule that decides who wins a benchmark and that no test could reach
# without running a full graded leg. So it moves into `run/verdict.py` as a pure function, the
# driver imports it (which is what keeps the cases a test of the SHIPPED rule rather than of a copy
# of it), and `selftest/verdict_cases.py` drives it through every classification, clean case first.
new("run/verdict.py", '"""The Mind leg\'s disqualification decision, as a PURE function.\n\nIt lived inline in the driver\'s receipt literal, where it could not be tested, and an adversarial\nreview found three separate ways it was wrong: the Mind was not held to the agreement check the\nmanifest promises for both systems, it alone could pass with zero model requests, and a missing\nspend log counted as zero calls. All three were invisible because nothing could exercise the\ndecision without running a whole graded leg. Here it takes numbers and returns booleans, and\n`selftest/verdict_cases.py` drives it through every classification.\n\nThe taxonomy (MANIFEST.json, unchanged): INDEPENDENT violations disqualify unconditionally — the\nrun broke a rule of its own whatever the upstream was doing. DEPENDENT violations disqualify only\nwhen the leg is not void, because an infrastructure outage must not be charged to the agent. A\nvoid never rescues an independent violation, and this function never decides voidness: that is\nthe proxy receipt\'s business, upstream, in `receipt_checks.py`.\n"""\nCAP = 8\n\n\ndef classify(*, present, ledger_requests, ledger_attempts, ledger_malformed,\n             accepted, refused, stop, cap=CAP):\n    """present: the proxy receipt was readable. ledger_*: the mind\'s own spend log (-1 = the log\n    was absent, which is not a count). accepted/refused: the proxy\'s counts. stop: None, "cap" or\n    "timeout". Returns (dq_independent, dq_dependent, accounting_agrees)."""\n    log_missing = ledger_requests < 0 or ledger_attempts < 0\n\n    # One attempt is one HTTP request to the provider; one `inference_call` row may carry several,\n    # so ATTEMPTS is the number comparable to what the proxy counted — the exact analogue of\n    # `accepted == the calls in the agent\'s own log` that the Hermes leg has always applied.\n    accounting_agrees = present and not log_missing and ledger_attempts == accepted\n\n    dq_independent = (\n        (not present)                # no proxy receipt: never a void, always a violation\n        or log_missing               # the mind cannot account for itself at all\n        or ledger_malformed > 0      # a row that is not a v1 inference_call\n        or refused > 0               # a ninth request was attempted: the cap was hit\n        or accepted > cap            # more requests left the box than the budget allowed\n    )\n    dq_dependent = (\n        stop == "timeout"            # the wall\n        or (not accounting_agrees)   # the two accountings disagree\n        or accepted < 1              # never reached the model; Hermes\'s receipt requires 1 <= a\n    )\n    return dq_independent, dq_dependent, accounting_agrees\n')
new("selftest/verdict_cases.py", '"""Drives run/verdict.py through every classification. Prints one line per case and exits 1 on any\ndisagreement. Runs inside the checker image with no network.\n\nEach case names the flags it expects, so a change that makes the decision more lenient in one place\nfails here instead of quietly letting a leg through. The clean case is first and must be clean: a\nsuite whose only cases are failures cannot notice a rule that rejects everything."""\nimport sys, os\nsys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "run"))\nfrom verdict import classify, CAP\n\nBASE = dict(present=True, ledger_requests=3, ledger_attempts=3, ledger_malformed=0,\n            accepted=3, refused=0, stop=None)\n\nCASES = [\n    # name,                       overrides,                              ind,   dep,   agrees\n    ("clean",                     {},                                     False, False, True),\n    ("no_proxy_receipt",          dict(present=False),                    True,  True,  False),\n    ("spend_log_absent",          dict(ledger_requests=-1,\n                                       ledger_attempts=-1,\n                                       ledger_malformed=-1),              True,  True,  False),\n    ("malformed_ledger_row",      dict(ledger_malformed=1),               True,  False, True),\n    ("cap_refusal",               dict(refused=1, stop="cap"),            True,  False, True),\n    ("over_cap_accepted",         dict(accepted=CAP + 1,\n                                       ledger_attempts=CAP + 1,\n                                       ledger_requests=CAP + 1),          True,  False, True),\n    # The three the review found. Each must be a DEPENDENT violation and nothing more: an upstream\n    # outage that produces them must be voidable, exactly as on the Hermes side.\n    ("accounting_disagrees",      dict(ledger_attempts=4),                False, True,  False),\n    ("zero_model_requests",       dict(accepted=0, ledger_attempts=0,\n                                       ledger_requests=0),                False, True,  True),\n    ("wall",                      dict(stop="timeout"),                   False, True,  True),\n    # Retries: one inference_call row, two HTTP requests. ATTEMPTS is what the proxy counted, so\n    # this agrees — comparing `ledger_requests` instead would fail an honest leg.\n    ("retry_counts_as_attempts",  dict(ledger_requests=2, ledger_attempts=3),\n                                                                          False, False, True),\n]\n\nbad = 0\nfor name, over, want_ind, want_dep, want_agree in CASES:\n    args = dict(BASE); args.update(over)\n    ind, dep, agree = classify(**args)\n    ok = (ind, dep, agree) == (want_ind, want_dep, want_agree)\n    if not ok:\n        bad = 1\n    print(f"{name}: {\'agree\' if ok else \'DISAGREE\'} "\n          f"got=(ind={ind},dep={dep},agrees={agree}) want=(ind={want_ind},dep={want_dep},agrees={want_agree})")\nsys.exit(bad)\n')
rw("run/mind_driver.py", [
    ('import json, os, pathlib, shutil, sys, time, urllib.request, http.cookiejar, hashlib',
     'import json, os, pathlib, shutil, sys, time, urllib.request, http.cookiejar, hashlib\n# The disqualification decision lives in verdict.py so it can be exercised without a graded leg —\n# `selftest/verdict_cases.py` drives it through every classification. THIS import is what makes\n# those cases a test of the shipped rule rather than a test of a copy of it.\nsys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))\nfrom verdict import classify as _classify'),
    ('accounting_agrees = present and att >= 0 and att == accepted',
     'dq_ind, dq_dep, accounting_agrees = _classify(\n    present=present, ledger_requests=req, ledger_attempts=att, ledger_malformed=bad,\n    accepted=accepted, refused=refused, stop=stop, cap=CAP)'),
    ('           # `req < 0` is the missing spend log: the mind cannot account for itself at all.\n           "dq_independent": (not present) or bad > 0 or req < 0 or refused > 0 or accepted > CAP,\n           # `accepted < 1` matches Hermes\'s `1 <= a`: a leg that reached the model zero times is\n           # not a fast win, and the receipt used to accept it on the Mind side alone.\n           "dq_dependent": stop == "timeout" or (not accounting_agrees) or accepted < 1,\n           "disqualified": (not present) or bad > 0 or req < 0 or refused > 0 or accepted > CAP\n                           or (not accounting_agrees) or accepted < 1 or stop is not None}',
     '           "dq_independent": dq_ind,\n           "dq_dependent": dq_dep,\n           # `stop is not None` stays: a cap stop or a wall stop ends the leg whatever else holds.\n           "disqualified": dq_ind or dq_dep or stop is not None}'),
    ("           # The driver's own reasons, split. INDEPENDENT: evidence the run broke a rule of its own —\n           # a missing or malformed proxy receipt, a refusal (a ninth request was attempted), an\n           # over-cap count, a malformed ledger row. These stand whatever the upstream was doing.\n           # DEPENDENT: the wall. A transport outage that ends in a timeout must not become an\n           # independent violation, or an infrastructure void would also disqualify the leg.\n",
     "           # The driver's own reasons, split; the rule itself is verdict.classify above.\n"),
])
rw("selftest/selftest.sh", [
    ('S=$(mktemp -d /tmp/cb2-tree-self-XXXX)',
     '# The receipt decision, driven through every classification without a graded leg.\nif python3 "$FIX/selftest/verdict_cases.py"; then echo "verdict_cases: agree"; else echo "verdict_cases: DISAGREE"; BAD=1; fi\nS=$(mktemp -d /tmp/cb2-tree-self-XXXX)'),
])

rw("MANIFEST.json", [
    ('plus a special-node safety probe; each fixture must produce a verdict',
     "plus a special-node safety probe and selftest/verdict_cases.py (the Mind leg's disqualification rule, driven through every classification: clean, no proxy receipt, absent spend log, malformed ledger row, cap refusal, over-cap, accounting disagreement, zero model requests, the wall, and a retry that agrees because attempts and not rows are compared); each fixture must produce a verdict"),
    ('run receipts carry counts, ids, hashes (artifact tree, binary, archive), image ids, timings and statuses only;',
     'run receipts carry counts, ids, hashes (artifact tree, binary, archive), image ids, timings and statuses only; the Mind receipt carries accounting_agrees beside ledger_attempts and proxy_accepted, so the agreement this manifest requires of BOTH systems is visible in the receipt and not merely asserted (it was implemented on the Hermes leg alone until an adversarial review found it missing);'),
])

# ── the binary under test is a parameter, not the deployed one by construction ────────────────
# mind_leg.sh mounted /opt/yantrik-mind/mind-core, the binary the live companion runs. Testing a
# code change therefore REQUIRED replacing production's binary, which is a separate decision with a
# separate authority. It defaults to the same path, so a reading that means to test the deployed
# binary still does, and `binary_provenance` now names the checkout the file was built from and
# marks it dirty when that checkout has uncommitted changes.
rw("run/mind_leg.sh", [
    ('BIN_SHA=$(sha256sum /opt/yantrik-mind/mind-core | cut -c1-64); PROV=$(cd /root/codes/ym-autodeploy && git rev-parse --short HEAD)',
     '# WHICH BINARY IS UNDER TEST is a parameter, and it defaults to the deployed one. It used to be\n# hard-coded to /opt/yantrik-mind/mind-core, which silently coupled a graded reading to a\n# production deploy: testing a code change meant replacing the binary the live companion would pick\n# up on its next restart. The two decisions are unrelated and are now separable. CB2_MIND_SOURCE is\n# the checkout the binary was built from, so `binary_provenance` names the commit that actually\n# produced this file rather than whatever HEAD the deploy clone happens to sit at.\nBIN=${CB2_MIND_BINARY:-/opt/yantrik-mind/mind-core}; SRC=${CB2_MIND_SOURCE:-/root/codes/ym-autodeploy}\n[ -x "$BIN" ] || { echo "refusing: CB2_MIND_BINARY=$BIN is not an executable file"; exit 2; }\nBIN_SHA=$(sha256sum "$BIN" | cut -c1-64); PROV=$(cd "$SRC" && git rev-parse --short HEAD)\n# A provenance claim is only worth recording if the tree it names matches the tree that was built.\nPROV_CLEAN=$(cd "$SRC" && git status --porcelain | wc -l)\n[ "$PROV_CLEAN" = 0 ] || PROV="$PROV+dirty"'),
    ('  -v /opt/yantrik-mind/mind-core:/mind-core:ro',
     '  -v "$BIN":/mind-core:ro'),
])

rw("run/smoke_mind.sh", [
    ('  -v /opt/yantrik-mind/mind-core:/mind-core:ro',
     '  -v "${CB2_MIND_BINARY:-/opt/yantrik-mind/mind-core}":/mind-core:ro'),
    ('# No-model Mind smoke: boot the containerised staging binary through a run proxy, pair from',
     '# No-model Mind smoke: boot the binary under test (CB2_MIND_BINARY, default the deployed one --\n# the SAME parameter mind_leg.sh takes, so a preflight cannot smoke a different file than the leg\n# it is clearing) through a run proxy, pair from'),
])

# ── E.CB2-N reading 4: the request cap becomes ONE resolved number ────────────────────────────
# It was five independent literals: the proxy's 429 threshold, the Hermes leg's CAP, the Mind
# driver's CAP, the Mind leg's receipt check and the manifest's prose. Five places holding one
# number can drift apart and nothing notices. It now enters the immutable run state beside profile,
# upstream and model; a state written at one cap refuses a load asking for another, so a reading
# cannot change its own budget halfway through; and the Mind receipt carries the cap the PROXY
# enforced rather than a number the reader re-derived. Unset is 8, so the existing profiles are
# unchanged in effect and reading 3 stays reproducible.
rw('run/profile.sh', [
    ('  set -a; . "$fix/profiles/$name.profile"; set +a',
     '  set -a; . "$fix/profiles/$name.profile"; set +a\n  # THE CAP IS ONE NUMBER. It was five independent literals — the proxy\'s 429, the Hermes leg, the\n  # Mind driver, the Mind leg\'s receipt check and the manifest\'s prose — which can drift apart with\n  # nothing noticing. It now enters the run state beside profile, upstream and model, so every\n  # consumer reads the same resolved value and a state written at one cap refuses a load at another.\n  # Unset means 8, so the existing profiles are unchanged in effect.\n  CB2_CAP=${CB2_CAP:-8}\n  case "$CB2_CAP" in \'\'|*[!0-9]*) echo "profile: CB2_CAP must be a positive integer"; return 1;; esac\n  [ "$CB2_CAP" -ge 1 ] || { echo "profile: CB2_CAP must be at least 1"; return 1; }'),
    ('print(d[\'profile\'], d[\'upstream\'], d[\'model\'], d[\'upstream_ip\'], d[\'resolved_at\'], \'|\'.join(d[\'upstream_ips\']))" 2>/dev/null) || { echo "profile: run state unreadable: $state"; return 1; }\n    read -r sp su sm sip sat sips <<< "$got"\n    [ "$sp" = "$name" ] && [ "$su" = "$CB2_UPSTREAM" ] && [ "$sm" = "$CB2_MODEL" ] || { echo "profile: run state $state belongs to profile \'$sp\' ($su, $sm), not \'$name\' — use another out dir"; return 1; }',
     'print(d[\'profile\'], d[\'upstream\'], d[\'model\'], d[\'upstream_ip\'], d[\'resolved_at\'], \'|\'.join(d[\'upstream_ips\']), d.get(\'cap\', 8))" 2>/dev/null) || { echo "profile: run state unreadable: $state"; return 1; }\n    read -r sp su sm sip sat sips scap <<< "$got"\n    [ "$sp" = "$name" ] && [ "$su" = "$CB2_UPSTREAM" ] && [ "$sm" = "$CB2_MODEL" ] || { echo "profile: run state $state belongs to profile \'$sp\' ($su, $sm), not \'$name\' — use another out dir"; return 1; }\n    [ "$scap" = "$CB2_CAP" ] || { echo "profile: run state $state was written at cap $scap, not $CB2_CAP — a reading may not change its budget mid-run; use another out dir"; return 1; }'),
    ("json.dump({'profile': '$name', 'upstream': '$CB2_UPSTREAM', 'upstream_ips': '$CB2_UPSTREAM_IPS'.split(), 'upstream_ip': '$CB2_UPSTREAM_IP', 'resolved_at': '$CB2_RESOLVED_AT', 'model': '$CB2_MODEL'}",
     "json.dump({'profile': '$name', 'upstream': '$CB2_UPSTREAM', 'upstream_ips': '$CB2_UPSTREAM_IPS'.split(), 'upstream_ip': '$CB2_UPSTREAM_IP', 'resolved_at': '$CB2_RESOLVED_AT', 'model': '$CB2_MODEL', 'cap': int('$CB2_CAP')}"),
    ('CB2_MODEL CB2_KEY_FILE CB2_MIND_LANE CB2_MIND_PROVIDER CB2_MIND_KEY_ENV',
     'CB2_MODEL CB2_CAP CB2_KEY_FILE CB2_MIND_LANE CB2_MIND_PROVIDER CB2_MIND_KEY_ENV'),
])
rw('run/proxy.sh', [
    ('-e CB2_CAP=8 ',
     '-e CB2_CAP="${CB2_CAP:-8}" '),
])
rw('run/hermes_leg.sh', [
    ('T=$1; OUT=${2:-/root/cb2n/out}; WALL=1800; CAP=8',
     'T=$1; OUT=${2:-/root/cb2n/out}; WALL=1800; CAP=${CB2_CAP:-8}   # resolved by the run state, not a literal'),
])
rw('run/mind_driver.py', [
    ('T = sys.argv[1]; COUNT_DIR = sys.argv[2]',
     "# argv[3] is the run state's cap. It is an ARGUMENT rather than a default in this file because a\n# literal here is a fifth place the number can drift; a missing argument means the caller is old\n# and 8 is what every caller meant before the cap became configurable.\nT = sys.argv[1]; COUNT_DIR = sys.argv[2]"),
    ('BASE = "http://127.0.0.1:8091"; STATE = pathlib.Path("/state"); WALL = 1800; CAP = 8',
     'BASE = "http://127.0.0.1:8091"; STATE = pathlib.Path("/state"); WALL = 1800\nCAP = int(sys.argv[3]) if len(sys.argv) > 3 else 8'),
])
rw('run/mind_leg.sh', [
    ('docker exec "$NAME" python3 /fixtures/run/mind_driver.py "$T" /count',
     'docker exec "$NAME" python3 /fixtures/run/mind_driver.py "$T" /count "$CB2_CAP"'),
    ('    receipt_ok = type(acc) is int and type(ref) is int and type(p["upstream_errors"]) is int and 1 <= acc <= 8 and ref >= 0 and ref == 0 and tls',
     '    receipt_ok = type(acc) is int and type(ref) is int and type(p["upstream_errors"]) is int and 1 <= acc <= cap and ref >= 0 and ref == 0 and tls'),
    ('src, dst, bin_sha, prov, img, tree, rc, task, prx, profile, upstream, ips, resolved_at, run_state, run_state_sha, model, leak, checks, brain_gate = sys.argv[1:]',
     'src, dst, bin_sha, prov, img, tree, rc, task, prx, profile, upstream, ips, resolved_at, run_state, run_state_sha, model, leak, checks, brain_gate, cap_s = sys.argv[1:]\n# The cap the PROXY enforced, carried through rather than re-derived by this reader: two places\n# deciding what the budget was is how a receipt comes to disagree with the run it describes.\ncap = int(cap_s)'),
    ('"$LEAK" "$CHECKS" "$BRAIN_GATE" <<\'EOF\'',
     '"$LEAK" "$CHECKS" "$BRAIN_GATE" "$CB2_CAP" <<\'EOF\''),
    ('          "model": model, "model_ok": ck.get("model_ok") == "true",',
     '          "cap": cap, "model": model, "model_ok": ck.get("model_ok") == "true",'),
])
new("profiles/nim-cap24.profile", '# cb2n profile "nim-cap24" (E.CB2-N reading 4): identical to "nim" except for CB2_CAP. The cap of\n# 8 was equal to Hermes\'s configured agent.max_turns 8, so eight turns cost at least eight\n# requests and any retry was a guaranteed violation — a budget that made one competitor\'s own\n# turn limit unsatisfiable. 24 is three times that limit: the cap stops runaway spend and no\n# longer binds normal operation. "nim" is left byte-identical so reading 3 stays reproducible.\n# Original header follows.\n# cb2n profile "nim" (E.CB2-N): NVIDIA NIM upstream, its IPv4 addresses resolved on the box ONCE\n# per run into the immutable run state (allowlisted exclusively, recorded in every receipt); the\n# key file (uid 10002, mode 0400) mounted read-only into the PROXY container only and injected as\n# the Authorization header on every forward; both work containers hold placeholder keys. One\n# model for both systems. The Mind runs with YM_PRIMARY_BRAIN=nim:<model> and all six roles equal\n# to it, behind YM_PROVIDER_BASE_URL_NIM (the proxy).\nCB2_UPSTREAM=integrate.api.nvidia.com\nCB2_UPSTREAM_IP=\nCB2_UPSTREAM_IPS=\nCB2_UPSTREAM_RESOLVE=1\nCB2_MODEL=openai/gpt-oss-120b\nCB2_KEY_FILE=/root/cb2/secrets/nim.key\nCB2_MIND_LANE=roles\nCB2_MIND_PROVIDER=nim\nCB2_MIND_KEY_ENV=NVIDIA_API_KEY\nCB2_CAP=24\n')

new("selftest/cap_cases.sh", '#!/bin/bash\n# The request cap is ONE number carried by the run state. These cases prove the three things that\n# make that true, using a synthetic profile in a temporary fixtures tree so nothing touches a real\n# profile, a real key file or a real output root. Exit non-zero on any disagreement.\n#\n# They exist because the cap used to be five independent literals. A number duplicated in five\n# places is not a parameter, it is five parameters that happen to agree today.\nset -u\nHERE="$(cd "$(dirname "$0")" && pwd)"; BAD=0\nT=$(mktemp -d /tmp/cb2-cap-self-XXXX); trap \'rm -rf "$T"\' EXIT\nmkdir -p "$T/fixtures/profiles"\ncat > "$T/fixtures/profiles/synthetic.profile" <<\'PROF\'\nCB2_UPSTREAM=cap.test.invalid\nCB2_UPSTREAM_IP=203.0.113.7\nCB2_UPSTREAM_IPS=203.0.113.7\nCB2_UPSTREAM_RESOLVE=0\nCB2_MODEL=cap/test-model\nCB2_MIND_LANE=roles\nCB2_MIND_PROVIDER=nim\nCB2_MIND_KEY_ENV=NVIDIA_API_KEY\nPROF\n. "$HERE/../run/profile.sh"\n\nsay() { if [ "$2" = "$3" ]; then echo "$1: agree [$2]"; else echo "$1: DISAGREE got=[$2] want=[$3]"; BAD=1; fi; }\n\n# 1. Unset means 8 — the existing profiles are unchanged in effect.\n( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/s1.json"; unset CB2_CAP\n  cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo "$CB2_CAP" || echo LOADFAIL ) > "$T/o1"\nsay "cap_defaults_to_8" "$(cat "$T/o1")" "8"\n\n# 2. A profile that names a cap gets that cap, and it lands in the run state.\n( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/s2.json" CB2_CAP=24\n  cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo "$CB2_CAP" || echo LOADFAIL ) > "$T/o2"\nsay "cap_24_is_honoured" "$(cat "$T/o2")" "24"\nsay "cap_is_in_the_run_state" \\\n    "$(python3 -c "import json;print(json.load(open(\'$T/s2.json\')).get(\'cap\'))" 2>/dev/null)" "24"\n\n# 3. A run state written at one cap REFUSES a load asking for another. A reading may not change\n#    its own budget halfway through — that is the difference between a budget and a negotiation.\n( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/s2.json" CB2_CAP=8\n  cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo LOADED || echo REFUSED ) > "$T/o3"\nsay "a_reading_cannot_change_its_own_budget" "$(cat "$T/o3")" "REFUSED"\n\n# 4. ...and the same cap reloads cleanly, so the refusal is about the cap and not about reloading.\n( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/s2.json" CB2_CAP=24\n  cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo LOADED || echo REFUSED ) > "$T/o4"\nsay "the_same_cap_reloads" "$(cat "$T/o4")" "LOADED"\n\n# 5. A cap that is not a positive integer is refused rather than coerced to something.\nfor bad in 0 -1 abc 3.5 ""; do\n  ( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/s5-$RANDOM.json" CB2_CAP="$bad"\n    cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo LOADED || echo REFUSED ) > "$T/o5"\n  # An EMPTY CB2_CAP is the unset case and correctly becomes 8; the rest must be refused.\n  want=REFUSED; [ -z "$bad" ] && want=LOADED\n  say "cap_rejects_${bad:-empty}" "$(cat "$T/o5")" "$want"\ndone\n\n# 6. No literal 8 is left deciding the budget in any consumer.\nF="$HERE/.."\nfor f in run/hermes_leg.sh run/mind_driver.py run/proxy.sh; do\n  n=$(grep -cE \'CAP *= *8|CB2_CAP=8|cap *= *8\' "$F/$f" 2>/dev/null || true)\n  say "no_hard_coded_cap_in_$(basename "$f")" "$n" "0"\ndone\nexit $BAD\n')
rw("selftest/selftest.sh", [
    ('# The receipt decision, driven through every classification without a graded leg.',
     '# The request cap: one number, carried by the run state, refusing to change mid-reading.\nif bash "$FIX/selftest/cap_cases.sh"; then echo "cap_cases: agree"; else echo "cap_cases: DISAGREE"; BAD=1; fi\n# The receipt decision, driven through every classification without a graded leg.'),
])

# ── this file itself, recorded in the tree ────────────────────────────────────────────────────
os.makedirs(R + "scratch", exist_ok=True)
shutil.copyfile(os.path.abspath(__file__), R + "scratch/cb2n_patch.py")
print("cb2n patch applied to", R)
