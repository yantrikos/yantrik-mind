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
import asyncio
from fastapi import FastAPI, Request, Response, WebSocket, WebSocketDisconnect

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


def _decode_via_ffmpeg(raw: bytes):
    """Transcode ANY container (m4a/AAC/AMR/mp3/…) to 16k mono float32 via ffmpeg stdin→stdout.
    Returns (np.ndarray, 16000) or (None, 0) if ffmpeg is unavailable/fails."""
    import subprocess
    try:
        p = subprocess.run(
            ["ffmpeg", "-hide_banner", "-loglevel", "error", "-i", "pipe:0",
             "-ar", "16000", "-ac", "1", "-f", "wav", "pipe:1"],
            input=raw, capture_output=True, timeout=30,
        )
        if p.returncode != 0 or len(p.stdout) < 100:
            return None, 0
        data, sr = sf.read(io.BytesIO(p.stdout), dtype="float32")
        return data, sr
    except Exception:
        return None, 0


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
    # --- ears --- decode format-tolerantly: libsndfile handles WAV/FLAC/OGG (iOS sends WAV);
    # anything it can't read (Android m4a/AAC, which MediaRecorder forces) is transcoded via ffmpeg.
    try:
        data, sr = sf.read(io.BytesIO(raw), dtype="float32")
    except Exception:
        data, sr = _decode_via_ffmpeg(raw)
        if data is None:
            return Response("unsupported audio format (need wav/m4a/aac; ffmpeg missing?)", status_code=415)
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


# ---------------- CONTINUOUS VOICE (Tier 1: hands-free, VAD-endpointed, streaming TTS) ----------------
# Protocol (ws /ws/voice?key=<device-token>):
#   client → server: BINARY frames = PCM16 mono 16kHz audio (any chunk size; server reframes to 20ms)
#   client → server: TEXT  {"type":"barge"}  — user started talking over the reply; abort TTS now
#   client → server: TEXT  {"type":"bye"}    — close
#   server → client: TEXT  {"type":"listening"} | {"type":"transcript","text":..} |
#                          {"type":"thinking"} | {"type":"reply","text":..} |
#                          {"type":"speaking_done"} | {"type":"error","text":..}
#   server → client: BINARY = WAV audio chunk (one per sentence) to play in order
FRAME_MS = 20
FRAME_BYTES = int(16000 * 2 * FRAME_MS / 1000)   # 640 bytes = 320 samples * 2
SILENCE_HANGOVER_MS = 700                          # trailing silence that ends an utterance
MIN_UTTERANCE_MS = 300                             # ignore blips shorter than this
MAX_UTTERANCE_MS = 20000                           # hard cap


def _sentences(text: str):
    # split reply into speakable chunks so TTS can stream sentence-by-sentence
    clean = text.replace("**", "").replace("`", "").replace("#", "").replace("•", ",")
    parts = re.split(r'(?<=[.!?])\s+', clean)
    out, buf = [], ""
    for p in parts:
        p = p.strip()
        if not p:
            continue
        buf = (buf + " " + p).strip() if buf else p
        if len(buf) >= 60 or p[-1:] in ".!?":
            out.append(buf)
            buf = ""
    if buf:
        out.append(buf)
    return out[:12]  # cap spoken length


@app.websocket("/ws/voice")
async def ws_voice(ws: WebSocket):
    # auth via ?key= (WebSocket clients can't always set headers)
    token = ws.query_params.get("key", "")
    if not (device_label(token) or (not KEY and not os.path.exists(DEVICE_TOKENS_PATH))):
        await ws.close(code=4401)
        return
    await ws.accept()
    import webrtcvad
    vad = webrtcvad.Vad(2)  # 0..3 aggressiveness
    pcm = bytearray()        # rolling raw bytes to reframe to 20ms
    speech = bytearray()     # accumulated speech PCM for the current utterance
    in_speech = False
    silence_ms = 0
    speech_ms = 0
    abort = {"tts": False}

    async def send_json(obj):
        try:
            await ws.send_json(obj)
        except Exception:
            pass

    async def speak(reply_text):
        # stream TTS sentence-by-sentence; stop if barge-in flagged
        abort["tts"] = False
        for sent in _sentences(reply_text):
            if abort["tts"]:
                break
            try:
                samples, out_sr = await asyncio.to_thread(kokoro().create, sent, VOICE, 1.05)
            except Exception:
                continue
            buf = io.BytesIO()
            sf.write(buf, samples, out_sr, format="WAV")
            if abort["tts"]:
                break
            try:
                await ws.send_bytes(buf.getvalue())
            except Exception:
                return
        await send_json({"type": "speaking_done"})

    async def handle_utterance(utter: bytes):
        data = np.frombuffer(utter, dtype=np.int16).astype(np.float32) / 32768.0
        try:
            segs, _ = await asyncio.to_thread(lambda: list(whisper().transcribe(data, language="en", vad_filter=False)[0]))
        except Exception as e:
            await send_json({"type": "error", "text": f"stt: {e}"})
            return
        transcript = " ".join(x.text.strip() for x in segs).strip()
        if not transcript:
            return
        await send_json({"type": "transcript", "text": transcript})
        await send_json({"type": "thinking"})
        try:
            reply = await asyncio.to_thread(_brain_call, transcript)
        except Exception as e:
            await send_json({"type": "error", "text": f"brain: {e}"})
            return
        await send_json({"type": "reply", "text": reply})
        await speak(reply)

    await send_json({"type": "listening"})
    try:
        while True:
            msg = await ws.receive()
            if msg.get("type") == "websocket.disconnect":
                break
            if msg.get("text"):
                import json as _json
                try:
                    ctl = _json.loads(msg["text"])
                except Exception:
                    continue
                if ctl.get("type") == "barge":
                    abort["tts"] = True
                elif ctl.get("type") == "bye":
                    break
                continue
            chunk = msg.get("bytes")
            if not chunk:
                continue
            pcm.extend(chunk)
            while len(pcm) >= FRAME_BYTES:
                frame = bytes(pcm[:FRAME_BYTES])
                del pcm[:FRAME_BYTES]
                try:
                    voiced = vad.is_speech(frame, 16000)
                except Exception:
                    voiced = False
                if voiced:
                    if not in_speech:
                        in_speech = True
                        speech = bytearray()
                        speech_ms = 0
                    speech.extend(frame)
                    speech_ms += FRAME_MS
                    silence_ms = 0
                    if speech_ms >= MAX_UTTERANCE_MS:
                        utter = bytes(speech)
                        in_speech = False
                        await handle_utterance(utter)
                        await send_json({"type": "listening"})
                elif in_speech:
                    speech.extend(frame)   # keep trailing silence for natural endpoint
                    silence_ms += FRAME_MS
                    if silence_ms >= SILENCE_HANGOVER_MS:
                        utter = bytes(speech)
                        in_speech = False
                        if speech_ms >= MIN_UTTERANCE_MS:
                            await handle_utterance(utter)
                        await send_json({"type": "listening"})
    except WebSocketDisconnect:
        pass
    except Exception:
        pass
    finally:
        try:
            await ws.close()
        except Exception:
            pass


def _brain_call(text: str) -> str:
    req = urllib.request.Request(BRAIN, data=text.encode(), headers={"Content-Type": "text/plain"})
    with urllib.request.urlopen(req, timeout=150) as r:
        return r.read().decode("utf-8", "replace").strip()


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=8090)
