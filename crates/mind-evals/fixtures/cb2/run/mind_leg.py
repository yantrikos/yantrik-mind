"""E.CB2 Mind leg (MANIFEST.json systems.mind). Runs ON the staging box against the scratch
instance's loopback console: pair once with the scratch instance's own code, submit ONE
delegation per task, poll until done/failed or the wall limit, snapshot what the job wrote
under the scratch web dir (read-only copy, hashed), and write a counts-only receipt.
Usage: python3 mind_leg.py <T1|T2|T3> [wall_s] [out_root]"""
import json, sys, time, urllib.request, http.cookiejar, pathlib, shutil, hashlib, os, subprocess
BASE = "http://127.0.0.1:8091"
SCRATCH = pathlib.Path("/var/lib/ym-cb2")
FIX = pathlib.Path(__file__).resolve().parent.parent
task = sys.argv[1]
wall_s = int(sys.argv[2]) if len(sys.argv) > 2 else 1800
out = pathlib.Path(sys.argv[3]) if len(sys.argv) > 3 else pathlib.Path("/root/cb2/out")
(out / "receipts").mkdir(parents=True, exist_ok=True)
art = out / "artifacts" / f"mind_{task}"
if art.exists():
    sys.exit(f"refusing: {art} exists (one invocation per task)")
brief = (FIX / "briefs" / f"{task}.txt").read_text(encoding="utf-8").strip()
cj = http.cookiejar.CookieJar()
op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cj))

def call(method, path, body=None):
    req = urllib.request.Request(BASE + path, method=method,
                                 data=(json.dumps(body).encode() if body is not None else None),
                                 headers={"Content-Type": "application/json", "x-ym-web": "cb2-harness"})
    with op.open(req, timeout=60) as r:
        return r.status, r.read().decode("utf-8", "replace")

code_file = SCRATCH / "web-pairing.code"
if code_file.exists():
    st, _ = call("POST", "/api/pair", {"code": code_file.read_text().strip(), "name": "cb2-harness"})
    print("pair:", st)
else:
    print("pair: registration closed (already paired this instance)")
rows_before = sum(1 for l in open(SCRATCH / "mind.db.decisions.jsonl", encoding="utf-8") if '"kind":"inference_call"' in l) if (SCRATCH / "mind.db.decisions.jsonl").exists() else 0
before_files = set(p for p in (SCRATCH / "public").rglob("*") if p.is_file())
name = f"cb2-{task.lower()}"
t0 = time.time(); started = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
st, submit_out = call("POST", "/api/agent", {"name": name, "task": brief})
print("submit:", st, submit_out[:120].replace("\n", " "))
status, result = "unknown", ""
while time.time() - t0 < wall_s:
    time.sleep(15)
    try:
        st, tasks_out = call("GET", "/api/tasks")
        jobs = json.loads(tasks_out)
        jobs = jobs if isinstance(jobs, list) else (jobs.get("jobs") or jobs.get("delegations") or [])
    except Exception:
        jobs = []
    mine = [j for j in jobs if isinstance(j, dict) and j.get("name") == name]
    if mine:
        status = mine[-1].get("status", "?"); result = mine[-1].get("result") or ""
        if status in ("done", "failed"):
            break
finished = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()); wall = round(time.time() - t0, 1)
# snapshot: every file the job added under the scratch web dir (the page path publishes there)
art.mkdir(parents=True)
new_files = [p for p in (SCRATCH / "public").rglob("*") if p.is_file() and p not in before_files]
for p in new_files:
    dst = art / p.relative_to(SCRATCH / "public"); dst.parent.mkdir(parents=True, exist_ok=True); shutil.copy2(p, dst)
for p in art.rglob("*"):
    if p.is_file(): os.chmod(p, 0o444)
rows_after = sum(1 for l in open(SCRATCH / "mind.db.decisions.jsonl", encoding="utf-8") if '"kind":"inference_call"' in l) if (SCRATCH / "mind.db.decisions.jsonl").exists() else 0
tree = subprocess.run([sys.executable, str(FIX / "tools" / "tree_hash.py"), str(art)], capture_output=True, text=True).stdout.strip()
requests = rows_after - rows_before
receipt = {"system": "mind", "task": task, "started": started, "finished": finished, "wall_s": wall, "status": status,
           "result_len": len(result), "result_head": result[:160], "files": len(new_files), "inference_requests": requests,
           "disqualified": requests > 8, "tree": tree}
(out / "receipts" / f"mind_{task}.json").write_text(json.dumps(receipt, indent=1))
print(json.dumps(receipt))
