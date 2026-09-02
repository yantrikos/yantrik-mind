"""E.CB2 Mind driver v3 — runs INSIDE the cb2-mind container. Waits for the console, pairs with
the instance's own code, submits ONE delegation, polls every 10 s and STOPS (never kills its
own container — the parent does that after this receipt) at done/failed, at the first proxy
refusal (a ninth request was attempted → cap hit), or at 1800 s. Declared output → /state/
artifact (RESULT.md + files added under the web dir); receipt → /state/receipt.json, counts only,
closed-schema accounting fail-closed; a missing proxy receipt disqualifies."""
import json, os, pathlib, shutil, sys, time, urllib.request, http.cookiejar, hashlib
# The disqualification decision lives in verdict.py so it can be exercised without a graded leg —
# `selftest/verdict_cases.py` drives it through every classification. THIS import is what makes
# those cases a test of the shipped rule rather than a test of a copy of it.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from verdict import classify as _classify
T = sys.argv[1]; COUNT_DIR = sys.argv[2]
BASE = "http://127.0.0.1:8091"; STATE = pathlib.Path("/state"); WALL = 1800; CAP = 8
FIX = pathlib.Path("/fixtures")
cj = http.cookiejar.CookieJar(); op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cj))


def call(method, path, body=None, timeout=60):
    req = urllib.request.Request(BASE + path, method=method, data=(json.dumps(body).encode() if body is not None else None),
                                 headers={"Content-Type": "application/json", "x-ym-web": "cb2-harness"})
    with op.open(req, timeout=timeout) as r:
        return r.status, r.read().decode("utf-8", "replace")


def spend_rows():
    """Closed schema, fail-closed: a row that is not a v1 inference_call with verdict in
    served|failed|refused and a numeric attempts:<n> is counted as malformed (disqualifying)."""
    p = STATE / "mind.db.decisions.jsonl"; req = att = bad = 0
    if not p.exists():
        # NOT (0, 0, 0): "the mind's own spend log was not there" and "the mind made no calls" are
        # different facts and were the same number in the receipt. -1 is unmistakably not a count,
        # and a missing log is an INDEPENDENT violation below — the analogue of Hermes's invalid log.
        return -1, -1, -1
    for line in open(p, encoding="utf-8"):
        try:
            ev = json.loads(line).get("event", {})
        except Exception:
            bad += 1; continue
        if ev.get("kind") != "inference_call":
            continue
        try:
            assert ev.get("evaluator_id") == "inference-ledger-v1" and ev.get("verdict") in ("served", "failed", "refused")
            n = int(str(ev.get("outcome", "")).split("attempts:")[1])
        except Exception:
            bad += 1; continue
        req += 1; att += n
    return req, att, bad


def proxy_count():
    """(accepted, refused, disconnects, present). Absent or unreadable → (-1, -1, -1, False)."""
    try:
        d = json.load(open(os.path.join(COUNT_DIR, "requests.json")))
        return (int(d.get("model_requests", -1)), int(d.get("refused_over_cap", -1)),
                int(d.get("client_disconnects", -1)), True)
    except Exception:
        return -1, -1, -1, False


for _ in range(60):
    try:
        urllib.request.urlopen(BASE + "/", timeout=3); break
    except urllib.error.HTTPError:
        break
    except Exception:
        time.sleep(1)
code = (STATE / "web-pairing.code").read_text().strip()
st, _ = call("POST", "/api/pair", {"code": code, "name": "cb2-harness"}); print("pair:", st)
brief = (FIX / "briefs" / f"{T}.txt").read_text(encoding="utf-8").strip()
def snapshot():
    """path -> sha256 of every regular file under the web dir (symlinks excluded)."""
    out = {}
    for p in (STATE / "public").rglob("*"):
        if p.is_file() and not p.is_symlink():
            out[p] = hashlib.sha256(p.read_bytes()).hexdigest()
    return out


before = snapshot()
name = f"cb2-{T.lower()}"; t0 = time.time(); started = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
st, _ = call("POST", "/api/agent", {"name": name, "task": brief}); print("submit:", st)
status, result, stop = "running", "", None
routed_kind = ""
while True:
    remaining = WALL - (time.time() - t0)
    if remaining <= 0:
        stop = "timeout"; break
    time.sleep(min(10, max(0.1, remaining)))
    _, refused, _, present = proxy_count()
    if refused > 0:
        stop = "cap"; break
    if time.time() - t0 > WALL:
        stop = "timeout"; break
    try:
        _, out = call("GET", "/api/tasks", timeout=max(1, min(30, int(WALL - (time.time() - t0))))); jobs = json.loads(out)
        jobs = jobs if isinstance(jobs, list) else (jobs.get("jobs") or jobs.get("delegations") or [])
    except Exception:
        jobs = []
    mine = [j for j in jobs if isinstance(j, dict) and j.get("name") == name]
    if mine:
        status = mine[-1].get("status", "?"); result = mine[-1].get("result") or ""
        routed_kind = mine[-1].get("kind") or routed_kind
        if status in ("done", "failed"):
            break
if stop:
    status = f"stopped:{stop}"
finished = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()); wall = round(time.time() - t0, 1)
art = STATE / "artifact"; art.mkdir(exist_ok=True)
(art / "RESULT.md").write_text(result, encoding="utf-8")
after = snapshot()
added = [p for p, h in after.items() if before.get(p) != h]   # created OR content-changed since the baseline
for p in added:
    dst = art / p.relative_to(STATE / "public"); dst.parent.mkdir(parents=True, exist_ok=True); shutil.copy2(p, dst)
# Proxy FIRST, own log SECOND. Both are read after the job ended, but a request accepted at the
# proxy writes its ledger row only when it completes, so reading the log last narrows the window in
# which the two disagree for a reason that is not misconduct.
accepted, refused, disconnects, present = proxy_count(); req, att, bad = spend_rows()
# THE AGREEMENT CHECK, which the manifest promises for BOTH systems and only Hermes was held to:
# every model request that left the box must appear in the agent's own accounting. `ledger_attempts`
# is the comparable number — one attempt is one HTTP request, while one `inference_call` row may
# carry several. Dependent, exactly as `a == CALLS` is dependent on the Hermes side.
dq_ind, dq_dep, accounting_agrees = _classify(
    present=present, ledger_requests=req, ledger_attempts=att, ledger_malformed=bad,
    accepted=accepted, refused=refused, stop=stop, cap=CAP)
receipt = {"system": "mind", "task": T, "started": started, "finished": finished, "wall_s": wall, "status": status,
           "stop_reason": stop or "",
           "files_added": len(added), "result_bytes": len(result.encode("utf-8")), "routed_kind": routed_kind,
           "ledger_requests": req, "ledger_attempts": att, "ledger_malformed": bad,
           "proxy_receipt_present": present, "proxy_accepted": accepted, "proxy_refused": refused,
           "proxy_attempted": (accepted + refused) if present else -1,
           # Was the literal -1 — a placeholder that read as a measurement of zero disconnects
           # having been impossible to obtain. The proxy counts them; this reports what it counted.
           "proxy_client_disconnects": disconnects,
           "accounting_agrees": accounting_agrees,
           # The driver's own reasons, split; the rule itself is verdict.classify above.
           "dq_independent": dq_ind,
           "dq_dependent": dq_dep,
           # `stop is not None` stays: a cap stop or a wall stop ends the leg whatever else holds.
           "disqualified": dq_ind or dq_dep or stop is not None}
(STATE / "receipt.json").write_text(json.dumps(receipt, indent=1)); print(json.dumps(receipt))
