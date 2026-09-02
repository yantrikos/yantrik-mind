"""E.CB2 Mind leg, ON THE BOX: a FRESH scratch instance per task (run/scratch_up.sh, torn down
after with a receipt), pair with its own code, submit ONE delegation, poll every 15 s, and
CANCEL (kill the instance) the moment the closed-schema accounting shows a 9th model request or
the wall passes 1800 s. Captures the declared output — the job's result text as RESULT.md and
every file the job added under the scratch web dir — into a read-only artifact; the receipt is
counts only. Usage: python3 mind_leg.py <T1|T2|T3> [out_root]"""
import json, sys, time, urllib.request, http.cookiejar, pathlib, shutil, os, subprocess, signal
BASE = "http://127.0.0.1:8091"; SCRATCH = pathlib.Path("/var/lib/ym-cb2"); WALL = 1800; CAP = 8
FIX = pathlib.Path(__file__).resolve().parent.parent
task = sys.argv[1]; out = pathlib.Path(sys.argv[2]) if len(sys.argv) > 2 else pathlib.Path("/root/cb2/out")
art = out / "artifacts" / f"mind_{task}"; rec = out / "receipts"; rec.mkdir(parents=True, exist_ok=True)
if art.exists():
    sys.exit(f"refusing: {art} exists (one invocation per task)")

def sh(cmd):
    return subprocess.run(cmd, shell=True, capture_output=True, text=True).stdout

def spend_rows():
    """Closed-schema read of the scratch instance's inference rows: v1 only, verdict in
    served|failed|refused, attempts:<n>. Anything else is counted as malformed, never as spend."""
    p = SCRATCH / "mind.db.decisions.jsonl"; req = 0; att = 0; bad = 0
    if not p.exists():
        return 0, 0, 0
    for line in open(p, encoding="utf-8"):
        try:
            ev = json.loads(line).get("event", {})
        except Exception:
            continue
        if ev.get("kind") != "inference_call":
            continue
        if ev.get("evaluator_id") != "inference-ledger-v1" or ev.get("verdict") not in ("served", "failed", "refused") \
           or not str(ev.get("outcome", "")).startswith("attempts:"):
            bad += 1; continue
        req += 1; att += int(ev["outcome"].split(":")[1])
    return req, att, bad

# fresh state per task
sh(f"bash {FIX}/run/scratch_down.sh >/dev/null 2>&1; true")
up = sh(f"bash {FIX}/run/scratch_up.sh"); print(up.strip()[:400])
time.sleep(3)
cj = http.cookiejar.CookieJar(); op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cj))
def call(method, path, body=None):
    req = urllib.request.Request(BASE + path, method=method, data=(json.dumps(body).encode() if body is not None else None),
                                 headers={"Content-Type": "application/json", "x-ym-web": "cb2-harness"})
    with op.open(req, timeout=60) as r:
        return r.status, r.read().decode("utf-8", "replace")
code = (SCRATCH / "web-pairing.code").read_text().strip()
st, _ = call("POST", "/api/pair", {"code": code, "name": "cb2-harness"}); print("pair:", st)
brief = (FIX / "briefs" / f"{task}.txt").read_text(encoding="utf-8").strip()
before_files = set(p for p in (SCRATCH / "public").rglob("*") if p.is_file())
name = f"cb2-{task.lower()}"; t0 = time.time(); started = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
st, _ = call("POST", "/api/agent", {"name": name, "task": brief}); print("submit:", st)
status, result, cancel = "running", "", None
while True:
    time.sleep(15)
    req, att, bad = spend_rows()
    if req > CAP:
        cancel = "cap"; break
    if time.time() - t0 > WALL:
        cancel = "timeout"; break
    try:
        _, tasks_out = call("GET", "/api/tasks"); jobs = json.loads(tasks_out)
        jobs = jobs if isinstance(jobs, list) else (jobs.get("jobs") or jobs.get("delegations") or [])
    except Exception:
        jobs = []
    mine = [j for j in jobs if isinstance(j, dict) and j.get("name") == name]
    if mine:
        status = mine[-1].get("status", "?"); result = mine[-1].get("result") or ""
        if status in ("done", "failed"):
            break
if cancel:
    pid = (SCRATCH / "pid").read_text().strip()
    os.kill(int(pid), signal.SIGKILL); status = f"cancelled:{cancel}"
finished = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()); wall = round(time.time() - t0, 1)
# declared output → the artifact (never the receipt)
art.mkdir(parents=True)
(art / "RESULT.md").write_text(result, encoding="utf-8")
new_files = [p for p in (SCRATCH / "public").rglob("*") if p.is_file() and p not in before_files]
for p in new_files:
    dst = art / p.relative_to(SCRATCH / "public"); dst.parent.mkdir(parents=True, exist_ok=True); shutil.copy2(p, dst)
for p in art.rglob("*"):
    if p.is_file(): os.chmod(p, 0o444)
req, att, bad = spend_rows()
tree = subprocess.run([sys.executable, str(FIX / "tools" / "tree_hash.py"), str(art)], capture_output=True, text=True).stdout.strip()
prov = sh("cd /root/codes/ym-autodeploy && git rev-parse --short HEAD").strip()
receipt = {"system": "mind", "task": task, "binary_provenance": prov, "started": started, "finished": finished, "wall_s": wall,
           "status": status, "files_added": len(new_files), "result_bytes": len(result.encode("utf-8")),
           "requests": req, "attempts": att, "malformed_rows": bad, "disqualified": req > CAP or cancel is not None, "tree": tree}
(rec / f"mind_{task}.json").write_text(json.dumps(receipt, indent=1))
print(json.dumps(receipt))
down = sh(f"bash {FIX}/run/scratch_down.sh"); (rec / f"mind_{task}_teardown.txt").write_text(down)
print(down.strip()[-300:])
