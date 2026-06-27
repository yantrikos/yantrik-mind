#!/usr/bin/env bash
# Build yantrik-mind on a fresh Debian/Ubuntu Linux host and install it as a service.
# Source is local-only (no git remote), so it must already be rsync'd to the box first —
# see DEPLOY.md. Run this ON THE BOX as a sudo-capable user.
#
#   yantrik-mind/ and yantrik-companion/ must be SIBLINGS (path deps point at ../yantrik-companion).
set -euo pipefail

SRC="${SRC:-$HOME/codes}"          # parent dir holding yantrik-mind + yantrik-companion
APP=/opt/yantrik-mind
STATE=/var/lib/yantrik-mind

echo "==> system deps"
sudo apt-get update -y
sudo apt-get install -y build-essential pkg-config libssl-dev curl

echo "==> rust toolchain (if missing)"
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
fi

echo "==> build (release)"
cd "$SRC/yantrik-mind"
cargo build --release -p mind-core

echo "==> install binary + service user + state dir"
id yantrikmind >/dev/null 2>&1 || sudo useradd --system --home "$APP" --shell /usr/sbin/nologin yantrikmind
sudo mkdir -p "$APP" "$STATE"
sudo cp target/release/mind-core "$APP/mind-core"
sudo chown -R yantrikmind:yantrikmind "$APP" "$STATE"

echo "==> systemd unit"
sudo cp deploy/yantrik-mind.service /etc/systemd/system/yantrik-mind.service
if [ ! -f /etc/yantrik-mind.env ]; then
  sudo cp deploy/yantrik-mind.env.example /etc/yantrik-mind.env
  sudo chmod 600 /etc/yantrik-mind.env
  echo "!! Edit /etc/yantrik-mind.env with real secrets, then: sudo systemctl restart yantrik-mind"
fi
sudo systemctl daemon-reload
sudo systemctl enable --now yantrik-mind
echo "==> done. Logs: journalctl -u yantrik-mind -f"
