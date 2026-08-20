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
import threading
import time
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
_turn_no = [0]


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


# ── CONTINUOUS LISTENING ──────────────────────────────────────────────────────────────────────
# Push-to-talk means turn-taking belongs to a key, not to the conversation, and it also means the
# barge-in machinery has no trigger: you can only interrupt while not holding the button.
#
# The whole difficulty is deciding when a person has FINISHED. Cut too early and you talk over them
# mid-thought, which is the rudest failure a listener can have; wait too long and every reply lands
# late. People pause mid-sentence to think, so the threshold is silence measured AFTER speech has
# actually started, not a global timer.
#
# Two numbers, and they are asymmetric on purpose. START needs sustained sound so a cough or a door
# does not open a turn. END needs a pause long enough to be a full stop rather than a breath —
# roughly 700ms, which is above the ~200ms gap inside a sentence and below the point where a caller
# thinks the line died.
SILENCE_RMS = float(os.environ.get("YM_VAD_SILENCE", "0.012"))
SPEECH_START_MS = int(os.environ.get("YM_VAD_START_MS", "180"))
SPEECH_END_MS = int(os.environ.get("YM_VAD_END_MS", "700"))
MAX_UTTERANCE_S = float(os.environ.get("YM_VAD_MAX_S", "30"))


def rms(block) -> float:
    return float(np.sqrt(np.mean(np.square(block))) or 0.0)


def listen_until_done(on_speech_start=None) -> np.ndarray:
    """Wait for speech, record it, and return when the speaker stops.

    `on_speech_start` fires the moment sound begins — that is the barge-in hook: the mind may be
    mid-sentence, and the person starting to talk is the signal to shut up.
    """
    block_ms = 30
    frames_needed_start = max(1, SPEECH_START_MS // block_ms)
    frames_needed_end = max(1, SPEECH_END_MS // block_ms)
    chunks, loud_run, quiet_run, started = [], 0, 0, False
    fired = False
    stream = sd.InputStream(samplerate=SR, channels=1, dtype="float32",
                            blocksize=int(SR * block_ms / 1000))
    stream.start()
    try:
        t_start = time.time()
        while True:
            block, _ = stream.read(int(SR * block_ms / 1000))
            level = rms(block)
            if level >= SILENCE_RMS:
                loud_run += 1
                quiet_run = 0
                if started:
                    chunks.append(block.copy())
                elif loud_run >= frames_needed_start:
                    started = True
                    chunks.append(block.copy())
                    if on_speech_start and not fired:
                        fired = True
                        on_speech_start()   # you started talking: stop talking
            else:
                loud_run = 0
                if started:
                    chunks.append(block.copy())
                    quiet_run += 1
                    if quiet_run >= frames_needed_end:
                        break
            if started and time.time() - t_start > MAX_UTTERANCE_S:
                break
    finally:
        stream.stop()
        stream.close()
    return np.concatenate(chunks).flatten() if chunks else np.zeros(1, dtype="float32")


def record() -> np.ndarray:
    if os.environ.get("YM_PUSH_TO_TALK") == "1":
        return record_ptt()
    print("🎙️  listening… (just talk; Ctrl+C quits)")
    return listen_until_done(on_speech_start=interrupt)


def record_ptt() -> np.ndarray:
    input("🎙️  Enter to START talking…")
    chunks = []
    stream = sd.InputStream(samplerate=SR, channels=1, dtype="float32", callback=lambda d, *_: chunks.append(d.copy()))
    stream.start()
    input("…  Enter to STOP")
    stream.stop(); stream.close()
    return np.concatenate(chunks).flatten() if chunks else np.zeros(1, dtype="float32")


def play(samples, sr):
    # Windows: PortAudio's default output can be a different device than the one Windows actually
    # uses (earbuds vs speakers) — winsound plays via the Windows default, which is what you hear.
    if sys.platform == "win32":
        import tempfile
        import winsound
        p = os.path.join(tempfile.gettempdir(), "ym_reply.wav")
        sf.write(p, samples, sr)
        winsound.PlaySound(p, winsound.SND_FILENAME)
    else:
        sd.play(samples, sr)
        sd.wait()


# Lines said while the answer is still coming. Rendered ONCE at startup: the model's first token
# lands about a second after you stop talking, and a caller starts to think the line dropped at
# roughly that point. Synthesising a hold line on demand would put it in the same queue as the
# answer, which is the one place it must never be.
_HOLDS = ["Mm.", "One sec.", "Hang on.", "Let me look.", "Right, checking."]
_hold_cache = []
_stop_speaking = threading.Event()


def prerender_holds():
    """Pay the synthesis cost at boot so the gap later costs nothing."""
    for h in _HOLDS:
        try:
            _hold_cache.append(kokoro().create(h, voice=VOICE, speed=1.05))
        except Exception:
            pass


def play_hold(n: int):
    if _hold_cache:
        s, sr = _hold_cache[n % len(_hold_cache)]
        play(s, sr)


def sentences(text: str):
    """Split into pieces that can be spoken as they are ready.

    The FIRST piece is deliberately short. The gap before any sound is where a conversation dies,
    and a listener forgives a brief opening clause far more readily than silence. It is also the
    unit of interruption: a reply synthesised as one block cannot be stopped, because by then the
    audio exists and is already playing.
    """
    out, cur = [], ""
    for w in text.split():
        cur = (cur + " " + w).strip()
        limit = 60 if not out else 180
        if (w.endswith((".", "!", "?")) and len(cur) >= 12) or len(cur) >= limit:
            out.append(cur)
            cur = ""
    if cur.strip():
        out.append(cur.strip())
    return out


def speak(text: str):
    """Speak in pieces, stopping the moment an interruption is signalled."""
    _stop_speaking.clear()
    # The server composes for the ear when told the channel is voice; this is a last-ditch tidy for
    # anything that still arrives with markup, NOT a substitute for asking for speech.
    spoken = text.replace("**", "").replace("`", "").replace("•", ",").replace("#", "")
    for piece in sentences(spoken):
        if _stop_speaking.is_set():
            break
        try:
            samples, sr = kokoro().create(piece, voice=VOICE, speed=1.05)
        except Exception:
            break
        if _stop_speaking.is_set():
            # An interruption during synthesis must not still play, or you talk and are answered
            # over anyway.
            break
        play(samples, sr)


def interrupt():
    _stop_speaking.set()


def turn_local(audio: np.ndarray):
    segments, _ = whisper().transcribe(audio, language="en", vad_filter=True)
    transcript = " ".join(s.text.strip() for s in segments).strip()
    if not transcript:
        print("(heard nothing)")
        return
    print(f"you: {transcript}")
    # Declare the channel so the reply is COMPOSED for the ear — short, answer first, no markup —
    # rather than a written briefing with its bullets stripped off afterwards.
    holder = threading.Thread(target=play_hold, args=(_turn_no[0],), daemon=True)
    holder.start()
    r = requests.post(
        CHAT_URL,
        data=transcript.encode(),
        headers={"Content-Type": "text/plain", "X-YM-Voice": "1"},
        timeout=150,
    )
    holder.join(timeout=3)
    _turn_no[0] += 1
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
    play(data, sr)


def main():
    print(f"ym voice [{MODE}] → {'brain ' + CHAT_URL if MODE == 'local' else SIDECAR_URL}  (Ctrl+C quits)")
    if MODE == "local":
        print("(warming the voice…)")
        prerender_holds()   # pay it once here, never in the middle of a conversation
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
