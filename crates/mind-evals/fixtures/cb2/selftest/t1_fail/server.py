"""Self-test fixture: the T1 server with the JSON block omitted and the append dropped — must FAIL
the dashboard and append checks while still serving pages."""
import runpy, pathlib, sys
src = pathlib.Path(__file__).resolve().parent.parent / "t1_pass" / "server.py"
code = src.read_text(encoding="utf-8").replace("INCLUDE_JSON_BLOCK = True", "INCLUDE_JSON_BLOCK = False").replace("APPEND = True", "APPEND = False")
code = code.replace('STORE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data", "leads.json")',
                    'STORE = os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "data", "leads.json")')
exec(compile("import sys\n" + code, str(src), "exec"), {"__name__": "__main__", "__file__": str(src)})
