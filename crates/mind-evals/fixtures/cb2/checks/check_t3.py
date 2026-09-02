"""E.CB2 T3 check — EXACT contract (briefs/T3.txt), run INSIDE the checker image on a writable copy.
Usage: python3 check_t3.py <artifact-copy-dir>. Exit 0 iff every check passes."""
import json, os, subprocess, sys, glob, datetime
d = sys.argv[1]; py = sys.executable
v = {"task": "T3", "checks": {}}
def check(k, ok, **extra): v["checks"][k] = {"pass": bool(ok), **extra}
def run(args, cwd, timeout=60):
    try:
        p = subprocess.run([py] + args, cwd=cwd, capture_output=True, text=True, timeout=timeout,
                           env={"PATH": os.environ.get("PATH", ""), "HOME": cwd, "PYTHONDONTWRITEBYTECODE": "1"})
        return p.returncode, p.stdout.strip(), p.stderr[-200:]
    except Exception as e:
        return 99, "", str(e)[:120]
tracker = os.path.join(d, "tracker.py")
check("tracker_py_present", os.path.exists(tracker))
tests = sorted(glob.glob(os.path.join(d, "test_*.py")))
check("test_files_present", len(tests) > 0, files=[os.path.basename(t) for t in tests])
rc, out, err = run(["-m", "pytest", "-q"], d, 180)
check("pytest_passes", rc == 0 and len(tests) > 0, tail=(out + err)[-200:])
# the CLI contract, every command its own process, on a fresh store directory
store = os.path.join(d, "cb2_store"); os.makedirs(store, exist_ok=True)
tp = os.path.abspath(tracker)
rc1, o1, _ = run([tp, "add", "Write the report"], store); check("add_prints_added_1", rc1 == 0 and o1 == "added #1", out=o1[:60])
rc2, o2, _ = run([tp, "add", "Call the bank"], store); check("add_prints_added_2", rc2 == 0 and o2 == "added #2", out=o2[:60])
rc3, o3, _ = run([tp, "list"], store)
check("list_two_open_lines", rc3 == 0 and o3.splitlines() == ["#1 [ ] Write the report", "#2 [ ] Call the bank"], out=o3[:120])
rc4, o4, _ = run([tp, "done", "1"], store); check("done_prints_done_1", rc4 == 0 and o4 == "done #1", out=o4[:60])
rc5, o5, _ = run([tp, "list"], store)
check("list_marks_done", rc5 == 0 and o5.splitlines() == ["#1 [x] Write the report", "#2 [ ] Call the bank"], out=o5[:120])
rc6, o6, _ = run([tp, "today"], store)
check("today_lists_open_tasks_added_today", rc6 == 0 and o6.splitlines() == ["#2 [ ] Call the bank"], out=o6[:120])
sp = os.path.join(store, "tasks.json")
ok_store = False
try:
    arr = json.load(open(sp, encoding="utf-8")); ok_store = isinstance(arr, list) and len(arr) == 2
except Exception:
    pass
check("tasks_json_is_a_two_item_array", ok_store)
v["pass"] = all(c["pass"] for c in v["checks"].values())
print(json.dumps(v, indent=1)); sys.exit(0 if v["pass"] else 1)
