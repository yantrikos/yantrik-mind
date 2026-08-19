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
#
# THE FIRST LIVE RUN found the failure that theory missed. The rectangle was aimed correctly at the
# lower-third — but that strip also carries a SCROLLING TICKER TAPE, two live webcam thumbnails and
# a live price box, all of which change every frame. Scene-detection fired continuously: 776 frames
# in 25 seconds, and "emit only on change" degenerated into "emit everything" while every log line
# still said healthy. So the crop now excludes the tape (shorter), the logo and the outer price box
# (narrower), and a drawbox masks the one webcam that sits BETWEEN the two badge groups where no
# rectangle can exclude it. What survives is the part that only changes when a position changes.
#
# Constants aimed at someone else's layout are perishable by nature, so the durable protection is
# not better numbers — it is NOTICING. A crop watching something that always moves produces an
# absurd change RATE, which is measurable without knowing anything about the broadcast's design. So
# the watcher checks its own rate early and stops with a loud reason, because a wrong rectangle that
# spools garbage silently is worse than one that fails: the tape it builds looks richer while being
# pure noise.
set -u

URL="${YM_TAPE_URL:-https://www.youtube.com/@TraderTVLive/live}"
SPOOL="${YM_BAR_SPOOL:-/var/lib/yantrik-mind/barspool}"
LOG="${YM_BAR_LOG:-/var/lib/yantrik-mind/bar-watch.log}"
# The position badges only — verified frame-by-frame against a live broadcast, not guessed. Short
# enough to clear the scrolling ticker tape below, narrow enough to drop the logo and the live price
# box at the edges.
CROP="${YM_BAR_CROP:-in_w*0.72:in_h*0.075:in_w*0.16:in_h*0.86}"
# One webcam sits BETWEEN the two badge groups, where no rectangle can exclude it. Painted out
# before scene-detection sees it. Empty disables the mask.
MASK="${YM_BAR_MASK:-drawbox=x=iw*0.45:y=0:w=iw*0.10:h=ih:color=black:t=fill,}"
# A crop aimed at something that always moves emits every frame. Above this many change-frames in
# the first minute, the rectangle is wrong — stop and SAY so, rather than spool noise that makes the
# tape look richer than it is.
MAX_PER_MIN="${YM_BAR_MAX_PER_MIN:-40}"
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
  -vf "crop=${CROP},${MASK}select='gt(scene,${SCENE})',scale=768:-1" \
  -vsync vfr -q:v 4 \
  "$SPOOL/bar_%05d.jpg" 2>>"$LOG" &
FFPID=$!

# SANITY: a correct rectangle changes a handful of times an hour, because a trader takes a position
# a handful of times an hour. Anything near the frame rate means the crop is watching a clock, a
# tape, or a face — measurable without knowing anything about this broadcast's design.
BEFORE=$(ls -1 "$SPOOL"/bar_*.jpg 2>/dev/null | wc -l)
sleep 60
AFTER=$(ls -1 "$SPOOL"/bar_*.jpg 2>/dev/null | wc -l)
RATE=$((AFTER - BEFORE))
if [ "$RATE" -gt "$MAX_PER_MIN" ]; then
  kill "$FFPID" 2>/dev/null
  say "STOPPING: $RATE change-frames in the first minute (limit $MAX_PER_MIN). The crop is watching something that always moves — a scrolling tape, a clock, or a webcam — so every frame reads as an event. Re-aim YM_BAR_CROP/YM_BAR_MASK at the position badges; a correct crop changes a few times an HOUR."
  exit 1
fi
say "rate ok: $RATE change-frame(s) in the first minute — the crop is watching something that mostly holds still"
wait "$FFPID" 2>/dev/null

say "decoder exited (stream ended, or the time cap was reached)"

# Keep the spool bounded. The tape is the durable artefact; these frames are transient evidence
# and an unbounded spool would fill the disk on a quiet day of flickering.
COUNT=$(ls -1 "$SPOOL"/bar_*.jpg 2>/dev/null | wc -l)
if [ "$COUNT" -gt "$MAX_SPOOL" ]; then
  ls -1t "$SPOOL"/bar_*.jpg | tail -n +"$((MAX_SPOOL + 1))" | xargs -r rm -f
  say "spool trimmed to $MAX_SPOOL"
fi
