"""E.CB2-HTTP: drives proxy.upstream_connection through both schemes and the refusal. Exits 1 on any
disagreement. Runs outside the containers; imports the proxy module without starting it."""
import http.client, importlib.util, os, ssl, sys
HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("cb2proxy", os.path.join(HERE, "..", "proxy", "proxy.py"))
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
bad = 0
c = m.upstream_connection("https", "aig.mycluster.cyou", 443, 30, ssl.create_default_context())
ok = isinstance(c, http.client.HTTPSConnection) and c.port == 443; print("https -> HTTPSConnection:443", ok); bad |= not ok
c = m.upstream_connection("http", "192.168.4.35", 11434, 30, None)
ok = type(c) is http.client.HTTPConnection and c.port == 11434; print("http  -> HTTPConnection:11434", ok); bad |= not ok
try:
    m.upstream_connection("ftp", "x", 21, 30, None); print("ftp   -> refused", False); bad = 1
except ValueError:
    print("ftp   -> refused", True)
sys.exit(1 if bad else 0)
