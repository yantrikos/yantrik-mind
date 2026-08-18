#!/usr/bin/env bash
# REALTIME BAR WATCHER — one decoder, watching one strip, emitting only on change.
#
# Polling could never answer the question it was built for. Each tick paid for a stream
# resolution, a fresh connection, a seek and a vision call, so it had to be slow, so it missed
# precisely the short trades the whole thesis is about: a trader goes long at 10:31 and flat by
# 10:34, and a three-minute sampler sees flat at both ends and records that nothing happened.
# That is not a gap in the data, it is a bias toward the slow trades least like the ones in
# question.
#
# So: ONE long-lived ffmpeg attached to the stream, cropped to the position bar, with
# scene-detection running on THAT CROP ONLY. Nothing else in that strip moves, so a detected
# change is a trader changing state — an entry or an exit. Frames land in a spool directory and
# a vision call is spent only on those. Continuous coverage at roughly a second of resolution,
# for far less total cost than sampling every three minutes.
#
# The crop is the whole trick and it is configurable, because a broadcast can redesign its
# lower-third at any time and a hardcoded rectangle would then watch a decorative pixel forever
# while reporting healthy.
set -u

URL="${YM_TAPE_URL:-https://www.youtube.com/@TraderTVLive/live}"
SPOOL="${YM_BAR_SPOOL:-/var/lib/yantrik-mind/barspool}"
LOG="${YM_BAR_LOG:-/var/lib/yantrik-mind/bar-watch.log}"
# Fraction of the frame occupied by the position bar: full width, bottom ~12%.
CROP="${YM_BAR_CROP:-in_w:in_h*0.12:0:in_h*0.86}"
# How different the strip must look to count as an event. Low, because a LONG/SHORT badge
# lighting up is a small fraction of even this crop.
SCENE="${YM_BAR_SCENE:-0.02}"
MAX_SPOOL="${YM_BAR_MAX_SPOOL:-400}"

mkdir -p "$SPOOL"
say() { printf "%s | %s\n" "$(date -u +%FT%TZ)" "$1" >> "$LOG"; }

STREAM=$(timeout 90 yt-dlp -g -f "best[height<=720]/best" --no-playlist --no-warnings "$URL" 2>/dev/null | head -1)
if [ -z "$STREAM" ]; then
  say "not live (no stream resolved) — exiting; the supervisor will retry"
  exit 0
fi
say "attached to the stream; watching the bar crop=$CROP scene=$SCENE"

# -an: audio is a separate concern and decoding it here would be waste.
# The filter chain crops FIRST so scene-detection sees only the bar.
timeout "${YM_BAR_MAX_SECS:-21600}" ffmpeg -hide_banner -loglevel error \
  -i "$STREAM" -an \
  -vf "crop=${CROP},select='gt(scene,${SCENE})',scale=768:-1" \
  -vsync vfr -q:v 4 \
  "$SPOOL/bar_%05d.jpg" 2>>"$LOG"

say "decoder exited (stream ended, or the time cap was reached)"

# Keep the spool bounded. The tape is the durable artefact; these frames are transient evidence
# and an unbounded spool would fill the disk on a quiet day of flickering.
COUNT=$(ls -1 "$SPOOL"/bar_*.jpg 2>/dev/null | wc -l)
if [ "$COUNT" -gt "$MAX_SPOOL" ]; then
  ls -1t "$SPOOL"/bar_*.jpg | tail -n +"$((MAX_SPOOL + 1))" | xargs -r rm -f
  say "spool trimmed to $MAX_SPOOL"
fi
