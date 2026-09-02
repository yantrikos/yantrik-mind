"""Self-test fixture: the tracker with a wrong `done` message — must FAIL the contract check."""
import runpy, pathlib
src = pathlib.Path(__file__).resolve().parent.parent / "t3_pass" / "tracker.py"
code = src.read_text(encoding="utf-8").replace('print(f"done #{i}")', 'print(f"completed #{i}")')
exec(compile(code, str(src), "exec"), {"__name__": "__main__", "__file__": str(src), "sys": __import__("sys")})
