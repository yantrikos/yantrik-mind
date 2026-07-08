#!/usr/bin/env python3
"""ym voice client — JARVIS on any of your machines, using THAT machine's GPU.

Two modes:
  local  (default) — STT + TTS run HERE (CUDA on the desktop, Apple Silicon on
          the MacBook, CPU otherwise). Only plain TEXT goes to the brain's
          /chat endpoint. Server needs NOTHING installed.
  server — send raw audio to the box's voice sidecar (deploy/voice_sidecar.py)
          for thin machines with no GPU. Set YM_VOICE_MODE=server.

Brain endpoint: the control server binds 127.0.0.1 on CT173, so tunnel it:
    ssh -N -L 8077:127.0.0.1:8077 root@192.168.4.90
then run this with YM_CHAT_URL=http://127.0.0.1:8077/chat (the default).

Setup (local mode):
    pip install sounddevice soundfile numpy requests faster-whisper kokoro-onnx
    # kokoro model (~340MB, one time) into ./voice/:
    #   https://github.com/thewh1teagle/kokoro-onnx/releases  -> kokoro-v1.0.onnx + voices-v1.0.bin

Push-to-talk: Enter to start, Enter to stop, reply is spoken back.
"""
import io
import os
import sys
import urllib.parse

import numpy as np
import requests
import sounddevice as sd
import soundfile as sf

MODE = os.environ.get("YM_VOICE_MODE", "local")
CHAT_URL = os.environ.get("YM_CHAT_URL", "http://127.0.0.1:8077/chat")
SIDECAR_URL = os.environ.get("YM_VOICE_URL", "http://192.168.4.90:8090/voice")
KEY = os.environ.get("YM_KEY", "")
VOICE = os.environ.get("YM_TTS_VOICE", "bm_george")
VDIR = os.environ.get("YM_VOICE_DIR", os.path.join(os.path.dirname(os.path.abspath(__file__)), "voice"))
SR = 16000

_whisper = None
_kokoro = None


def whisper():
    global _whisper
    if _whisper is None:
        from faster_whisper import WhisperModel
        try:
            _whisper = WhisperModel("small", device="cuda", compute_type="float16")
            print("(whisper: CUDA)")
        except Exception:
            _whisper = WhisperModel("small", device="cpu", compute_type="int8")
            print("(whisper: CPU)")
    return _whisper


def kokoro():
    global _kokoro
    if _kokoro is None:
        from kokoro_onnx import Kokoro
        _kokoro = Kokoro(os.path.join(VDIR, "kokoro-v1.0.onnx"), os.path.join(VDIR, "voices-v1.0.bin"))
    return _kokoro


def record() -> np.ndarray:
    input("🎙️  Enter to START talking…")
    chunks = []
    stream = sd.InputStream(samplerate=SR, channels=1, dtype="float32", callback=lambda d, *_: chunks.append(d.copy()))
    stream.start()
    input("…  Enter to STOP")
    stream.stop(); stream.close()
    return np.concatenate(chunks).flatten() if chunks else np.zeros(1, dtype="float32")


def speak(text: str):
    spoken = text.replace("**", "").replace("`", "").replace("•", ",").replace("#", "")
    if len(spoken) > 1200:
        spoken = spoken[:1200].rsplit(".", 1)[0] + "."
    samples, sr = kokoro().create(spoken, voice=VOICE, speed=1.05)
    sd.play(samples, sr)
    sd.wait()


def turn_local(audio: np.ndarray):
    segments, _ = whisper().transcribe(audio, language="en", vad_filter=True)
    transcript = " ".join(s.text.strip() for s in segments).strip()
    if not transcript:
        print("(heard nothing)")
        return
    print(f"you: {transcript}")
    r = requests.post(CHAT_URL, data=transcript.encode(), headers={"Content-Type": "text/plain"}, timeout=150)
    reply = r.text.strip()
    print(f"ym : {reply[:500]}")
    speak(reply)


def turn_server(audio: np.ndarray):
    buf = io.BytesIO()
    sf.write(buf, audio, SR, format="WAV")
    r = requests.post(SIDECAR_URL, data=buf.getvalue(), headers={"X-YM-Key": KEY}, timeout=180)
    if r.status_code != 200:
        print(f"[{r.status_code}] {r.text[:200]}")
        return
    uq = urllib.parse.unquote
    print(f"you: {uq(r.headers.get('X-Transcript', ''))}")
    print(f"ym : {uq(r.headers.get('X-Reply-Text', ''))[:500]}")
    data, sr = sf.read(io.BytesIO(r.content), dtype="float32")
    sd.play(data, sr)
    sd.wait()


def main():
    print(f"ym voice [{MODE}] → {'brain ' + CHAT_URL if MODE == 'local' else SIDECAR_URL}  (Ctrl+C quits)")
    while True:
        audio = record()
        if len(audio) < SR // 2:
            print("(too short)")
            continue
        print("… thinking …")
        try:
            (turn_local if MODE == "local" else turn_server)(audio)
        except Exception as e:
            print(f"(error: {e})")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(0)
