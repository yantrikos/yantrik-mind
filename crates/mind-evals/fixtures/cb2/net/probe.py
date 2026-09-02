"""Containment probe, run inside the cb2net network: the owned gateway must answer, the open
internet must not, and DNS must not resolve."""
import urllib.request, socket


def try_url(u):
    try:
        urllib.request.urlopen(u, timeout=8)
        return "ok"
    except Exception as e:
        s = str(e).lower()
        blocked = any(k in s for k in ("timed out", "errno", "name or service", "unreachable", "temporary failure", "no address"))
        return "blocked" if blocked else f"ok?({s[:60]})"


g = try_url("https://aig.mycluster.cyou/v1/models")
i = try_url("https://1.1.1.1/")
try:
    socket.gethostbyname("github.com")
    d = "resolved"
except Exception:
    d = "blocked"
print(f"gateway {g} / internet {i} / dns {d}")
