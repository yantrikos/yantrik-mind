#!/bin/sh
# One-time setup for the Yantrik Mind's proposal driver (E.SELF1). Run ONCE, with sudo, from the folder
# that holds this script, ym-propose.py and the two unit files:
#
#     sudo sh setup-self-improve.sh
#
# What it changes, and nothing else:
#   /srv/yantrik/self-improve             2750 pranab:yantrik-minds  the scout writes findings.jsonl here;
#                                                                     the mind account can read, not write
#   /srv/yantrik/self-improve/proposals   2770 pranab:yantrik-minds  the mind writes proposals here (0644);
#                                                                     pranab reads them; nobody else can enter
#   /var/lib/yantrik-mind/self-improve    0700 yantrik-mind           the driver's own state (what it has seen)
#   /usr/local/lib/yantrik-mind-propose/ym-propose.py                 the driver
#   /etc/systemd/system/yantrik-mind-propose.{service,timer}          every 2 h, 07:15-21:15, as yantrik-mind
# Your home stays closed to the mind account.
set -eu
[ "$(id -u)" = 0 ] || { echo "run it with sudo"; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
person=${SUDO_USER:-pranab}
for f in ym-propose.py yantrik-mind-propose.service yantrik-mind-propose.timer; do
  [ -f "$here/$f" ] || { echo "missing $here/$f"; exit 1; }
done
getent passwd "$person" >/dev/null || { echo "no account $person"; exit 1; }
getent passwd yantrik-mind >/dev/null || { echo "no yantrik-mind account: install the Mind first"; exit 1; }
getent group yantrik-minds >/dev/null || { echo "no yantrik-minds group"; exit 1; }

say() { echo "  $*"; }
echo "Setting up the Yantrik Mind's proposals"
install -d -m 0755 -o root -g root /srv/yantrik
install -d -m 2750 -o "$person" -g yantrik-minds /srv/yantrik/self-improve
say "/srv/yantrik/self-improve  2750 $person:yantrik-minds"
install -d -m 2770 -o "$person" -g yantrik-minds /srv/yantrik/self-improve/proposals
say "/srv/yantrik/self-improve/proposals  2770 $person:yantrik-minds"
install -d -m 0700 -o yantrik-mind -g yantrik-minds /var/lib/yantrik-mind/self-improve
say "/var/lib/yantrik-mind/self-improve  0700 yantrik-mind"
install -d -m 0755 -o root -g root /usr/local/lib/yantrik-mind-propose
install -m 0755 -o root -g root "$here/ym-propose.py" /usr/local/lib/yantrik-mind-propose/ym-propose.py
say "/usr/local/lib/yantrik-mind-propose/ym-propose.py"
install -m 0644 -o root -g root "$here/yantrik-mind-propose.service" /etc/systemd/system/yantrik-mind-propose.service
install -m 0644 -o root -g root "$here/yantrik-mind-propose.timer" /etc/systemd/system/yantrik-mind-propose.timer
systemctl daemon-reload
systemctl enable --now yantrik-mind-propose.timer >/dev/null
say "yantrik-mind-propose.timer: $(systemctl is-active yantrik-mind-propose.timer), next $(systemctl show -p NextElapseUSecRealtime --value yantrik-mind-propose.timer)"
echo "Done. Run it now with: sudo systemctl start yantrik-mind-propose.service; read it with: journalctl -u yantrik-mind-propose -n 20"
