"""Work-side containment probe (inside cb2net): only the proxy answers; the gateway directly, the
internet, the Docker host's services and DNS must all be unreachable."""
import urllib.request, socket


def reach(u, timeout=6):
    try:
        urllib.request.urlopen(u, timeout=timeout)
        return "ok"
    except urllib.error.HTTPError:
        return "ok"          # the service answered (any HTTP status counts as reachable)
    except Exception:
        return "blocked"


proxy = reach("http://cb2probe-proxy:8080/v1/models")
gw = reach("https://aig.mycluster.cyou/v1/models")
inet = reach("https://1.1.1.1/")
# the host on both bridges: cb2net gateway address and the staging console / page ports
host = "blocked" if all(reach(u, 4) == "blocked" for u in ("http://172.30.0.1:8090/", "http://172.30.0.1:8088/", "http://172.30.0.1:22/")) else "REACHABLE"
try:
    socket.gethostbyname("github.com")
    dns = "resolved"
except Exception:
    dns = "blocked"
print(f"proxy {proxy} / gateway-direct {gw} / internet {inet} / host-service {host} / dns {dns}")
