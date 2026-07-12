#!/usr/bin/env bash
# yantrik-mind installer — gets the binary onto a Debian/Ubuntu box, creates
# the service (installed DISABLED + STOPPED), then hands off to the in-binary
# first-run wizard which links your Telegram bot and starts the service.
#
# Design (researched 2026-07-12, converged with gpt-5.6-sol): install.sh stays
# dumb; every human decision lives in `ym setup` so it is re-runnable, and the
# install ends at a doorway — your companion greeting you in Telegram, not a
# config summary.
#
#   curl -fsSL https://<host>/install.sh | sudo bash
#
# Non-interactive: set YM_TELEGRAM_TOKEN + NANOGPT_KEY in the environment and
# `ym setup` runs unattended.
#
# HONEST SCOPE (v1): targets Debian 12 / Ubuntu 22.04+ on amd64. The binary is
# a prebuilt artifact (source-build is a separate expert path — the workspace
# path-depends on two sibling private repos and is not a one-liner). Point
# YM_ARTIFACT_URL at the release asset, or drop the binary at
# /opt/yantrik-mind/mind-core yourself before running.
set -euo pipefail

APP=/opt/yantrik-mind
STATE=/var/lib/yantrik-mind
BIN="$APP/mind-core"
ENVF=/etc/yantrik-mind.env
UNIT=/etc/systemd/system/yantrik-mind.service
USER_NAME=yantrikmind
ARTIFACT_URL="${YM_ARTIFACT_URL:-}"

c_sig='\033[38;5;179m'; c_ok='\033[38;5;42m'; c_err='\033[38;5;203m'; c_dim='\033[2m'; c_off='\033[0m'
say() { printf "${c_sig}==>${c_off} %s\n" "$1"; }
ok()  { printf "  ${c_ok}✓${c_off} %s\n" "$1"; }
die() { printf "  ${c_err}✗ %s${c_off}\n" "$1" >&2; exit 1; }

# ---- 0. preflight -----------------------------------------------------------
[ "$(id -u)" = "0" ] || die "run as root (curl … | sudo bash)."
. /etc/os-release 2>/dev/null || true
case "${ID:-}${ID_LIKE:-}" in *debian*|*ubuntu*) : ;; *) printf "  ${c_err}!${c_off} untested on '${ID:-unknown}' — proceeding, but v1 targets Debian/Ubuntu.\n" ;; esac
[ "$(uname -m)" = "x86_64" ] || die "v1 ships an amd64 binary; this host is $(uname -m)."
command -v systemctl >/dev/null || die "systemd not found. yantrik-mind v1 requires systemd (LXC: enable it, or use the foreground diagnostic path)."

# ---- 1. runtime deps (the binary links native-tls/openssl + sqlite) ---------
say "Installing runtime libraries"
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq ca-certificates libssl3 qrencode >/dev/null 2>&1 || apt-get install -y -qq ca-certificates openssl qrencode >/dev/null 2>&1 || true
ok "runtime libraries ready"

# ---- 2. the binary ----------------------------------------------------------
say "Placing the binary"
mkdir -p "$APP"
if [ -x "$BIN" ] && [ -z "$ARTIFACT_URL" ]; then
  ok "using the binary already at $BIN"
elif [ -n "$ARTIFACT_URL" ]; then
  tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT
  curl -fSL --progress-bar "$ARTIFACT_URL" -o "$tmp" || die "download failed from $ARTIFACT_URL"
  if [ -n "${YM_ARTIFACT_SHA256:-}" ]; then
    echo "${YM_ARTIFACT_SHA256}  $tmp" | sha256sum -c - >/dev/null 2>&1 || die "checksum mismatch — refusing to install a tampered binary."
    ok "checksum verified"
  fi
  install -m 0755 "$tmp" "$BIN"
  ok "installed $BIN"
else
  die "no binary. Set YM_ARTIFACT_URL to the release asset, or place it at $BIN first."
fi

# ---- 3. service user + state ------------------------------------------------
say "Creating the service account + state directory"
id "$USER_NAME" >/dev/null 2>&1 || useradd -r -s /usr/sbin/nologin -d "$STATE" "$USER_NAME"
mkdir -p "$STATE"; chown -R "$USER_NAME:$USER_NAME" "$STATE"
[ -f "$ENVF" ] || { install -m 600 /dev/null "$ENVF"; }
ok "user '$USER_NAME' + $STATE"

# ---- 4. systemd unit — installed DISABLED + STOPPED -------------------------
# `ym setup` owns the transition to running (it must own getUpdates
# exclusively while linking; a running poller would fight it).
say "Installing the service (stopped — setup will start it)"
cat > "$UNIT" <<UNIT
[Unit]
Description=yantrik-mind — your family's companion
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$USER_NAME
EnvironmentFile=$ENVF
ExecStart=$BIN
Restart=always
RestartSec=3
StateDirectory=yantrik-mind

[Install]
WantedBy=multi-user.target
UNIT
systemctl daemon-reload
ok "yantrik-mind.service installed (not started)"

# ---- 5. hand off to the first-run wizard ------------------------------------
say "Starting first-run setup"
echo
# Run the wizard as the service user so files it writes are owned correctly,
# but keep root for the systemctl start it performs at the end.
exec "$BIN" setup
