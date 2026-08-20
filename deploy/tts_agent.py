#!/usr/bin/env python3
"""VOICE — a resident synthesiser, because a voice that loads per sentence cannot hold a conversation.

Measured on this box before anything was built around it: loading the model costs ~1.8s, and
synthesis once it is loaded costs 144-287ms for two to four seconds of speech — nine to nineteen
times realtime. Those two numbers decide the whole design. Spawning `piper` per utterance pays the
1.8s every time, which is roughly the length of the sentence being spoken, so every reply would
arrive after a pause long enough to feel like a fault. Holding the model in memory turns the same
work into 200ms, which is under the gap a person leaves between turns.

So this is a daemon: one process, model resident, JSON per line on stdin, one JSON line back on
stdout. It mirrors browser_agent.js deliberately — that pattern is already understood here, and a
second protocol would be a second thing to debug at 3am.

    {"say": "text", "id": "opt"}   -> {"ok": true, "id": "...", "wav": "<base64>", "ms": 190}
    {"ping": 1}                    -> {"ok": true, "ready": true}

The audio comes back as base64 rather than a file path on purpose. A path means a temp file, which
means cleanup, which means a cleanup bug leaves the disk full of half-spoken sentences; and the
caller almost always wants to stream the bytes onward rather than read them off disk.
"""
import base64
import io
import json
import os
import sys
import time
import wave

MODEL = os.environ.get("YM_TTS_MODEL", "/opt/yantrik-mind/voices/en_US-amy-medium.onnx")


def emit(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def main():
    # onnxruntime tries to pin thread affinity and fails noisily on this host; saying how many
    # threads to use avoids the error without changing the timings measured above.
    os.environ.setdefault("OMP_NUM_THREADS", "4")
    try:
        from piper import PiperVoice
    except Exception as e:
        emit({"ok": False, "fatal": "piper is not installed: %s" % e})
        return 1
    if not os.path.exists(MODEL):
        emit({"ok": False, "fatal": "no voice model at %s" % MODEL})
        return 1

    t0 = time.time()
    voice = PiperVoice.load(MODEL)
    emit({"ok": True, "ready": True, "load_ms": int((time.time() - t0) * 1000), "model": MODEL})

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except Exception as e:
            emit({"ok": False, "error": "bad json: %s" % e})
            continue
        if req.get("ping"):
            emit({"ok": True, "ready": True})
            continue
        text = (req.get("say") or "").strip()
        rid = req.get("id")
        if not text:
            emit({"ok": False, "id": rid, "error": "nothing to say"})
            continue
        try:
            t = time.time()
            chunks, rate, ch, sw = [], 22050, 1, 2
            for c in voice.synthesize(text):
                chunks.append(c.audio_int16_bytes)
                rate, ch, sw = c.sample_rate, c.sample_channels, c.sample_width
            raw = b"".join(chunks)
            buf = io.BytesIO()
            with wave.open(buf, "wb") as w:
                w.setnchannels(ch)
                w.setsampwidth(sw)
                w.setframerate(rate)
                w.writeframes(raw)
            wav = buf.getvalue()
            emit({
                "ok": True,
                "id": rid,
                "ms": int((time.time() - t) * 1000),
                "secs": round(len(raw) / float(rate * ch * sw), 2),
                "wav": base64.b64encode(wav).decode("ascii"),
            })
        except Exception as e:
            # A failed sentence must never kill the voice: the next one is probably fine, and a
            # daemon that exits on one bad input takes the whole conversation down with it.
            emit({"ok": False, "id": rid, "error": str(e)})
    return 0


if __name__ == "__main__":
    sys.exit(main())
