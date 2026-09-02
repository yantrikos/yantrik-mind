import subprocess, sys, os, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))


def run(*args, cwd):
    return subprocess.run([sys.executable, os.path.join(HERE, "tracker.py"), *args], cwd=cwd,
                          capture_output=True, text=True).stdout.strip()


def test_add_list_done_today():
    d = tempfile.mkdtemp()
    assert run("add", "Write the report", cwd=d) == "added #1"
    assert run("list", cwd=d) == "#1 [ ] Write the report"
    assert run("done", "1", cwd=d) == "done #1"
    assert run("list", cwd=d) == "#1 [x] Write the report"
    assert run("today", cwd=d) == ""
