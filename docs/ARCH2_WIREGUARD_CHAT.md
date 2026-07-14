# ARCH-2 WireGuard chat ingress — deployment

The daemon can serve a **member-only `/chat` endpoint over WireGuard** so a paired phone reaches the
same running mind (shared memory) from anywhere. This is the second channel the authorization kernel
(ARCH-1/2/3) was built to enable. It reuses the device-trust gate; the operator console (`/cli`)
**stays loopback-only and is never network-reachable**.

Went through a gpt-5.6-sol redteam (verdict rid `019f5e16`). The load-bearing point: **binding an
address does not prove a packet arrived through WireGuard** — the firewall must enforce that. The
daemon fails closed on bad config, but it cannot prove your firewall; the steps below are required.

## What the daemon does (code)

- `spawn_chat_server` starts a **separate listener** (default port `8078`) that registers **only**
  `POST /chat` and content-free `GET /status`. There is no `/cli` route on it.
- Member devices only: an **operator** credential is refused (`403`) on this socket; no `X-YM-Person`
  delegation is honored. Turns run Principal-scoped as the device's bound person.
- Fail-closed config — the listener refuses to start unless:
  - `YM_CHAT_BIND` parses as a **concrete, non-loopback, non-wildcard** `IpAddr` (the WG interface IP), and
  - `YM_CHAT_HOST` is set to the **canonical authority** (e.g. `10.7.0.1:8078`).
- HTTP hardening: canonical-Host check, any present `Origin` is refused (native-only policy — *not* a
  security boundary), duplicate `Host`/`Authorization`/`Content-Length` and any `Transfer-Encoding`
  rejected, 32 KiB header cap, 64 KiB body cap, one request per connection, 20 s read deadline, a
  global in-flight connection cap, `Connection: close`.

### Environment

```
YM_CHAT_BIND=10.7.0.1        # the daemon host's WireGuard interface IP (required to enable)
YM_CHAT_HOST=10.7.0.1:8078   # canonical Host the phone must send (required; anti-rebinding)
YM_CHAT_PORT=8078            # optional, default 8078
```

## What you MUST configure (deployment — the daemon can't prove this)

1. **WireGuard** with a **unique `/32` per phone** in `AllowedIPs` (so per-source policy/limits are
   meaningful and a token can't be replayed from a wider prefix):
   ```
   # /etc/wireguard/wg0.conf (server)
   [Interface]
   Address = 10.7.0.1/24
   ListenPort = 51820
   PrivateKey = <server-priv>

   [Peer]                    # Asha's phone
   PublicKey = <phone-pub>
   AllowedIPs = 10.7.0.2/32
   ```

2. **Firewall: the chat port is reachable ONLY through `wg0`.** Match the input interface, not just
   the address (binding the address is not sufficient — sol #5). Default-drop, and keep it fail-closed
   across restarts.
   ```
   # nftables
   table inet ym {
     chain input {
       type filter hook input priority 0; policy drop;
       iif "lo" accept
       ct state established,related accept
       udp dport 51820 accept                      # WireGuard handshake
       iifname "wg0" tcp dport 8078 accept          # chat ONLY via wg0
       tcp dport 8078 drop                          # everything else to 8078: drop
     }
   }
   ```
   Verify: the port answers over `wg0` and is unreachable on every LAN/physical interface, for BOTH
   IPv4 and IPv6.

3. **Pair the phone as a MEMBER device** from the local console:
   ```
   ym device pair asha-phone --person asha
   ```
   Store the printed token in the phone's **platform keystore/Keychain** — never in browser storage.
   Revoke anytime: `ym device revoke <id>`.

4. **Phone client requirements** (client-side; the server can't enforce these):
   - Pin the WireGuard endpoint; **disable HTTP redirects** (a bearer-preserving redirect to an
     attacker URL is the key exfil path — sol #5).
   - Send `Authorization: Bearer <token>` and `Host: <YM_CHAT_HOST>`; never attach the token to any
     other origin.
   - Do not run the chat inside a WebView that could auto-attach the token.

## Scope (honest)

This is a WireGuard-reachable, **bearer-authenticated JSON-over-HTTP `/chat`** for a native app —
**not** a browser web UI. A browser interactive UI (HttpOnly/SameSite cookies + CSRF tokens + CSP +
served HTML on a separate trusted origin) is a later ARCH-4 increment. `/status` is the content-free
status surface. The operator console remains loopback-only.
