"""E.CB2 T3 check — EXACT contract (briefs/T3.txt), run INSIDE the checker image (no network) on a
writable copy. Usage: python3 check_t3.py <copy-dir> [excerpts-file]. The verdict on stdout is
counts and booleans only; command outputs go to the excerpts file. Exit 0 iff every check passes."""
import json, os, subprocess, sys, glob
d = sys.argv[1]; exc_path = sys.argv[2] if len(sys.argv) > 2 else None; py = sys.executable
v = {"task": "T3", "checks": {}}; excerpts = []
def note(k, s): excerpts.append(f"[{k}] {str(s)[:300]}")
def check(k, ok, **counts): v["checks"][k] = {"pass": bool(ok), **counts}
def run(args, cwd, timeout=60):
    try:
        p = subprocess.run([py] + args, cwd=cwd, capture_output=True, text=True, timeout=timeout,
                           env={"PATH": os.environ.get("PATH", ""), "HOME": cwd, "PYTHONDONTWRITEBYTECODE": "1"})
        return p.returncode, p.stdout.strip(), p.stderr
    except Exception as e:
        return 99, "", str(e)
tracker = os.path.join(d, "tracker.py")
check("tracker_py_present", os.path.exists(tracker))
tests = sorted(glob.glob(os.path.join(d, "test_*.py")))
check("test_files_present", len(tests) > 0, count=len(tests))
rc, out, err = run(["-m", "pytest", "-q"], d, 180); note("pytest", out + err)
check("pytest_passes", rc == 0 and len(tests) > 0, rc=rc)
store = os.path.join(d, "cb2_store"); os.makedirs(store, exist_ok=True); tp = os.path.abspath(tracker)
steps = [
    ("add_prints_added_1", ["add", "Write the report"], ["added #1"]),
    ("add_prints_added_2", ["add", "Call the bank"], ["added #2"]),
    ("list_two_open_lines", ["list"], ["#1 [ ] Write the report", "#2 [ ] Call the bank"]),
    ("done_prints_done_1", ["done", "1"], ["done #1"]),
    ("list_marks_done", ["list"], ["#1 [x] Write the report", "#2 [ ] Call the bank"]),
    ("today_lists_open_tasks_added_today", ["today"], ["#2 [ ] Call the bank"]),
]
for name, args, want in steps:
    rc, out, err = run([tp] + args, store); note(name, out or err)
    check(name, rc == 0 and out.splitlines() == want, rc=rc, lines=len(out.splitlines()))
ok_store = False
try:
    arr = json.load(open(os.path.join(store, "tasks.json"), encoding="utf-8")); ok_store = isinstance(arr, list) and len(arr) == 2
except Exception as e:
    note("tasks_json", e)
check("tasks_json_is_a_two_item_array", ok_store)
v["pass"] = all(c["pass"] for c in v["checks"].values())
if exc_path:
    open(exc_path, "w", encoding="utf-8").write("\n".join(excerpts) + "\n")
print(json.dumps(v, indent=1)); sys.exit(0 if v["pass"] else 1)
