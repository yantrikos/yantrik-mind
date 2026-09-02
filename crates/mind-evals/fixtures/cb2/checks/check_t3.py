"""T3 mechanical check — a command-line task tracker in Python. Runs in a FRESH COPY of the
artifact directory. Reports a JSON verdict; never edits the artifact."""
import json, os, shutil, subprocess, sys, tempfile, glob

def run(cmd, cwd, timeout=60):
    try:
        p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
        return p.returncode, (p.stdout + p.stderr)[-400:]
    except Exception as e:
        return 99, str(e)[:200]

src = sys.argv[1]
work = tempfile.mkdtemp(prefix="cb2-t3-")
shutil.copytree(src, work, dirs_exist_ok=True)
verdict = {"task": "T3", "checks": {}}
# tests
has_pytest = subprocess.run([sys.executable, "-c", "import pytest"], capture_output=True).returncode == 0
test_files = [f for f in glob.glob(os.path.join(work, "**", "*test*.py"), recursive=True) if "node_modules" not in f]
if not test_files:
    verdict["checks"]["tests"] = {"pass": False, "why": "no test file found"}
else:
    cmd = [sys.executable, "-m", "pytest", "-q"] if has_pytest else [sys.executable, "-m", "unittest", "discover", "-v"]
    rc, out = run(cmd, work, 180)
    verdict["checks"]["tests"] = {"pass": rc == 0, "runner": cmd[2], "tail": out[-300:]}
# the CLI entry
entry = None
for cand in ["tracker.py", "task.py", "tasks.py", "todo.py", "main.py", "cli.py", "app.py"]:
    p = os.path.join(work, cand)
    if os.path.exists(p):
        entry = [sys.executable, p]; break
if entry is None:
    for py in glob.glob(os.path.join(work, "*.py")):
        if "test" in os.path.basename(py):
            continue
        if "__main__" in open(py, encoding="utf-8", errors="replace").read():
            entry = [sys.executable, py]; break
if entry is None:
    verdict["checks"]["cli"] = {"pass": False, "why": "no CLI entry found"}
else:
    fresh = tempfile.mkdtemp(prefix="cb2-t3-store-")
    env = dict(os.environ, HOME=fresh, USERPROFILE=fresh)
    steps = {}
    for name, args in [("add", ["add", "Write the report"]), ("add2", ["add", "Call the bank"]), ("list", ["list"]), ("done", ["done", "1"]), ("today", ["today"])]:
        try:
            p = subprocess.run(entry + args, cwd=work, env=env, capture_output=True, text=True, timeout=60)
            steps[name] = {"rc": p.returncode, "out_len": len(p.stdout)}
        except Exception as e:
            steps[name] = {"rc": 99, "why": str(e)[:120]}
    # persistence across processes: a second `list` must still show the tasks
    try:
        p = subprocess.run(entry + ["list"], cwd=work, env=env, capture_output=True, text=True, timeout=60)
        steps["persist"] = {"rc": p.returncode, "mentions_task": ("report" in p.stdout.lower()) or ("bank" in p.stdout.lower())}
    except Exception as e:
        steps["persist"] = {"rc": 99, "why": str(e)[:120]}
    ok = all(s.get("rc") == 0 for s in steps.values()) and steps["persist"].get("mentions_task", False)
    verdict["checks"]["cli"] = {"pass": ok, "entry": os.path.basename(entry[1]), "steps": steps}
verdict["pass"] = all(c.get("pass") for c in verdict["checks"].values())
print(json.dumps(verdict, indent=1))
