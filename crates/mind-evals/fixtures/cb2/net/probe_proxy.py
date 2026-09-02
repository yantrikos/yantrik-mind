"""Proxy-side containment probe (inside the proxy container): the gateway by IP answers; the
internet, the Docker host's services and DNS must all be unreachable."""
import http.client, ssl, socket, urllib.request


def https_ip(ip, host, path, timeout=6):
    try:
        c = http.client.HTTPSConnection(ip, 443, timeout=timeout, context=ssl.create_default_context())
        c.request("GET", path, headers={"Host": host})
        c.getresponse()
        return "ok"
    except Exception:
        return "blocked"


def reach(u, timeout=4):
    try:
        urllib.request.urlopen(u, timeout=timeout)
        return "ok"
    except urllib.error.HTTPError:
        return "ok"
    except Exception:
        return "blocked"


gw = https_ip("192.168.4.203", "aig.mycluster.cyou", "/v1/models")
inet = https_ip("1.1.1.1", "one.one.one.one", "/")
host = "blocked" if all(reach(u) == "blocked" for u in ("http://172.30.1.1:8090/", "http://172.30.1.1:8088/", "http://172.30.0.1:8090/")) else "REACHABLE"
try:
    socket.gethostbyname("github.com")
    dns = "resolved"
except Exception:
    dns = "blocked"
print(f"gateway {gw} / internet {inet} / host-service {host} / dns {dns}")
