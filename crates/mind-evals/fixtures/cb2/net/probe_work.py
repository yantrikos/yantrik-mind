"""Work-side containment probe (inside cb2net), raw TCP connects: the proxy must accept; the
gateway by IP, the internet, and the Docker host's SSH and HTTP ports must all refuse or time
out; DNS must not resolve."""
import socket


def tcp(ip, port, timeout=5):
    try:
        s = socket.create_connection((ip, port), timeout=timeout)
        s.close()
        return "ok"
    except Exception:
        return "blocked"


try:
    socket.gethostbyname("github.com")
    dns = "resolved"
except Exception:
    dns = "blocked"
print(f"proxy-tcp {tcp('172.30.0.9', 8080)} / gateway-tcp {tcp('192.168.4.203', 443)} / internet-tcp {tcp('1.1.1.1', 443)} "
      f"/ host-ssh-tcp {tcp('172.30.0.1', 22)} / host-http-tcp {tcp('172.30.0.1', 8088)} / dns {dns}")
