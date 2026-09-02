"""Proxy-side containment probe (inside the proxy container): a TLS connection to the gateway by
HOSTNAME with certificate and hostname verification must succeed; raw TCP to the internet and to
the Docker host's SSH and HTTP ports on both bridges must fail; DNS must not resolve."""
import os, socket, ssl
UP = os.environ.get("CB2_UPSTREAM", "aig.mycluster.cyou")


def tcp(ip, port, timeout=5):
    try:
        s = socket.create_connection((ip, port), timeout=timeout)
        s.close()
        return "ok"
    except Exception:
        return "blocked"


def tls_verified(host, port=443, timeout=8):
    try:
        ctx = ssl.create_default_context()      # CERT_REQUIRED + check_hostname
        with socket.create_connection((host, port), timeout=timeout) as raw:
            with ctx.wrap_socket(raw, server_hostname=host) as s:
                return "ok" if s.getpeercert() else "blocked"
    except Exception:
        return "blocked"


try:
    socket.gethostbyname("github.com")
    dns = "resolved"
except Exception:
    dns = "blocked"
host_ssh = "blocked" if tcp("172.30.1.1", 22) == "blocked" and tcp("172.30.0.1", 22) == "blocked" else "ok"
host_http = "blocked" if tcp("172.30.1.1", 8088) == "blocked" and tcp("172.30.0.1", 8088) == "blocked" and tcp("172.30.1.1", 8090) == "blocked" else "ok"
print(f"gateway-tls-verified {tls_verified(UP)} / internet-tcp {tcp('1.1.1.1', 443)} / host-ssh-tcp {host_ssh} / host-http-tcp {host_http} / dns {dns}")
