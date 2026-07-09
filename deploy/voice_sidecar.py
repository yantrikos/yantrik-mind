#!/usr/bin/env python3
"""ym voice sidecar — the decoupled ears+mouth of yantrik-mind.

POST /voice  (body: WAV/OGG audio, header X-YM-Key)
  -> faster-whisper STT (local)
  -> POST 127.0.0.1:8077/chat  (the live brain, plain text)
  -> Kokoro TTS (local, British male)
  -> 200 WAV audio reply, X-Transcript / X-Reply-Text headers (utf-8 %-encoded)

GET /voice/health -> {"stt": true, "tts": true}

Everything stays on home hardware: audio never leaves the LAN, matching the
privacy lanes. Clients are dumb push-to-talk scripts (clients/ym_voice.py).
"""
import io
import json
import os
import urllib.parse
import urllib.request

import numpy as np
import soundfile as sf
import uvicorn
from fastapi import FastAPI, Request, Response

app = FastAPI()
KEY = os.environ.get("YM_WORKER_KEY", "")
BRAIN = os.environ.get("YM_BRAIN_URL", "http://127.0.0.1:8077/chat")
VOICE = os.environ.get("YM_TTS_VOICE", "bm_george")
VDIR = os.environ.get("YM_VOICE_DIR", "/opt/yantrik-mind/voice")

# Per-device tokens: JSON {"<token>": "<device-label>"} at YM_DEVICE_TOKENS path. Each client
# (phone, laptop) gets its own revocable token — the first brick of the security layer, arriving
# exactly when access leaves the LAN. The shared YM_WORKER_KEY still works as a dev fallback.
DEVICE_TOKENS_PATH = os.environ.get("YM_DEVICE_TOKENS", "/etc/yantrik-mind.devices.json")


def device_label(token: str) -> str | None:
    """Return the device label for a token (per-device file first, shared key fallback), else None."""
    if not token:
        return None
    try:
        with open(DEVICE_TOKENS_PATH) as fh:
            toks = json.load(fh)
        if token in toks:
            return toks[token]
    except Exception:
        pass
    if KEY and token == KEY:
        return "shared-dev-key"
    return None


def authorize(request: Request) -> str | None:
    """Accept X-YM-Key header or Bearer token. Returns device label if authorized, else None.
    If no auth is configured at all (no shared key, no device file), allow (LAN dev mode)."""
    tok = request.headers.get("x-ym-key", "")
    if not tok:
        auth = request.headers.get("authorization", "")
        if auth.lower().startswith("bearer "):
            tok = auth[7:].strip()
    lbl = device_label(tok)
    if lbl:
        return lbl
    if not KEY and not os.path.exists(DEVICE_TOKENS_PATH):
        return "lan-open"  # nothing configured → don't lock the owner out on first run
    return None

_whisper = None
_kokoro = None


def whisper():
    global _whisper
    if _whisper is None:
        from faster_whisper import WhisperModel
        _whisper = WhisperModel("small", device="cpu", compute_type="int8")
    return _whisper


def kokoro():
    global _kokoro
    if _kokoro is None:
        from kokoro_onnx import Kokoro
        _kokoro = Kokoro(os.path.join(VDIR, "kokoro-v1.0.onnx"), os.path.join(VDIR, "voices-v1.0.bin"))
    return _kokoro


@app.get("/voice/health")
def health():
    ok_stt = os.path.isdir(os.path.expanduser("~/.cache")) is not None
    ok_tts = os.path.exists(os.path.join(VDIR, "kokoro-v1.0.onnx"))
    return {"stt": ok_stt, "tts": ok_tts, "voice": VOICE}


@app.post("/chat")
async def chat(request: Request):
    """Text path for the mobile app: plain-text body → the live brain → plain-text reply.
    One authenticated surface (per-device token) so the app only ever talks to :8090."""
    if not authorize(request):
        return Response("unauthorized", status_code=401)
    msg = (await request.body()).decode("utf-8", "replace").strip()
    if not msg:
        return Response("empty message", status_code=400)
    req = urllib.request.Request(BRAIN, data=msg.encode(), headers={"Content-Type": "text/plain"})
    try:
        with urllib.request.urlopen(req, timeout=150) as r:
            reply = r.read().decode("utf-8", "replace")
    except Exception as e:
        return Response(f"brain unreachable: {e}", status_code=502)
    return Response(reply, media_type="text/plain; charset=utf-8")


@app.post("/voice")
async def voice(request: Request):
    if not authorize(request):
        return Response("unauthorized", status_code=401)
    raw = await request.body()
    if len(raw) < 1000:
        return Response("audio too short", status_code=400)
    # --- ears ---
    data, sr = sf.read(io.BytesIO(raw), dtype="float32")
    if data.ndim > 1:
        data = data.mean(axis=1)
    if sr != 16000:
        idx = np.linspace(0, len(data) - 1, int(len(data) * 16000 / sr)).astype(np.int64)
        data, sr = data[idx], 16000
    segments, _info = whisper().transcribe(data, language="en", vad_filter=True)
    transcript = " ".join(s.text.strip() for s in segments).strip()
    if not transcript:
        return Response("could not transcribe", status_code=422)
    # --- brain (the same live mind telegram talks to) ---
    req = urllib.request.Request(BRAIN, data=transcript.encode(), headers={"Content-Type": "text/plain"})
    with urllib.request.urlopen(req, timeout=150) as r:
        reply = r.read().decode("utf-8", "replace").strip()
    # strip markdown-ish noise for speech
    spoken = (reply.replace("**", "").replace("`", "").replace("•", ",").replace("#", ""))
    if len(spoken) > 1200:
        spoken = spoken[:1200].rsplit(".", 1)[0] + "."
    # --- mouth ---
    samples, out_sr = kokoro().create(spoken, voice=VOICE, speed=1.05)
    buf = io.BytesIO()
    sf.write(buf, samples, out_sr, format="WAV")
    q = urllib.parse.quote
    return Response(
        buf.getvalue(),
        media_type="audio/wav",
        headers={"X-Transcript": q(transcript), "X-Reply-Text": q(reply[:2000])},
    )


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=8090)
