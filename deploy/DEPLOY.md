# Deploying yantrik-mind always-on

The mind currently runs on the laptop and dies when it sleeps. This makes it 24/7 on a Linux
host via **build-on-box** (most robust given the native deps: native-tls/openssl for IMAP/SMTP,
bundled SQLite). The source is **local-only** (no git remote), so we ship it by rsync.

## Why build-on-box (not cross-compile)
`imap` + `lettre` link `native-tls` → OpenSSL. Cross-compiling that from Windows→Linux is
fiddly and unverifiable; building on the target is one `apt install` + `cargo build`. The
backend is API-based (NanoGPT over HTTPS) so **no local model / GPU is needed** — any small
Linux CT/VM works.

## Host
Any Debian/Ubuntu host reachable on the LAN. Candidates: a fresh CT, or alongside the existing
Python JARVIS on CT167. Keep it SEPARATE from that service (different unit/user/port-free —
yantrik-mind has no inbound port; it long-polls Telegram outbound only).

## Steps

**1. Ship the source (from the laptop).** Both repos must land as siblings — the workspace
path-deps point at `../yantrik-companion`:
```
rsync -az --delete --exclude target --exclude '*.db' \
  /c/Users/sync/codes/yantrik-mind/        user@HOST:~/codes/yantrik-mind/
rsync -az --delete --exclude target \
  /c/Users/sync/codes/yantrik-companion/   user@HOST:~/codes/yantrik-companion/
```

**2. Build + install (on the box):**
```
cd ~/codes/yantrik-mind && bash deploy/build_on_box.sh
```
This installs system deps + rustup (if missing), `cargo build --release -p mind-core`, creates
the `yantrikmind` service user + `/var/lib/yantrik-mind` state dir, installs the systemd unit,
and seeds `/etc/yantrik-mind.env` from the template.

**3. Secrets (on the box):** edit `/etc/yantrik-mind.env` (chmod 600) with the real values —
mirror what we use locally (`keys.env`): `NANOGPT_KEY`, `YM_TELEGRAM_TOKEN`,
`YM_EMAIL`/`YM_EMAIL_PASSWORD` (gmail 16-char App Password, spaces stripped),
`YM_GITHUB_TOKEN`. Then:
```
sudo systemctl restart yantrik-mind
journalctl -u yantrik-mind -f      # expect: "telegram channel live as @th_ym_c1_bot"
```

**4. Move off the laptop.** Once the box says it's live, stop the laptop instance so only one
bot polls the Telegram token (two pollers fight over updates).

## Updating later
Re-run step 1 (rsync) + `cargo build --release -p mind-core` + `sudo systemctl restart
yantrik-mind`. (Binary swap is safe; the unit `Restart=always`.)

## Notes
- Recipes persist + recover across restarts (SQLite under `YM_DB`); a non-idempotent send is
  failed-visibly on recovery, never double-sent.
- No inbound network surface; web fetches are SSRF-guarded (can't hit the LAN).
- Quiet hours default 22:00–07:00 **host-local** — set the box timezone or `YM_QUIET_START/END`.
