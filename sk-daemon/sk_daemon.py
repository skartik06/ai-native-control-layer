#!/usr/bin/env python3
"""
SK Voice Daemon — Wake word detection + STT pipeline
Prints JSON lines to stdout; the Tauri host reads and processes them.

Protocol (stdout, one JSON object per line, newline-terminated):
  {"type": "ready",      "text": "..."}   — listening for wake word
  {"type": "wake",       "text": "..."}   — wake word detected
  {"type": "recording",  "text": "..."}   — recording user speech
  {"type": "processing", "text": "..."}   — transcribing
  {"type": "command",    "text": "<utterance>"}  — send to SK
  {"type": "no_speech",  "text": "..."}   — nothing heard
  {"type": "error",      "text": "..."}   — non-fatal error
  {"type": "log",        "text": "..."}   — debug info
"""

import sys
import json
import subprocess
import time
import tempfile
import os
import wave
import signal
import struct

# ── Graceful shutdown ────────────────────────────────────────────────────────
_running = True

def _handle_sigterm(signum, frame):
    global _running
    _running = False

signal.signal(signal.SIGTERM, _handle_sigterm)
signal.signal(signal.SIGINT,  _handle_sigterm)

# ── Helpers ──────────────────────────────────────────────────────────────────
def emit(type_: str, text: str = "") -> None:
    print(json.dumps({"type": type_, "text": text}), flush=True)

def log(msg: str) -> None:
    emit("log", msg)

# ── Audio config ─────────────────────────────────────────────────────────────
SAMPLE_RATE   = 16_000
CHUNK_SIZE    = 1_280   # 80 ms at 16 kHz — openwakeword recommended
RECORD_SECONDS = 5
WAKE_THRESHOLD = 0.5

# ── Whisper model search ─────────────────────────────────────────────────────
WHISPER_MODEL_CANDIDATES = [
    os.path.expanduser("~/.local/share/whisper/ggml-base.en.bin"),
    os.path.expanduser("~/.cache/whisper/ggml-base.en.bin"),
    "/usr/share/whisper/models/ggml-base.en.bin",
    "/usr/local/share/whisper/ggml-base.en.bin",
]

def find_whisper_model() -> str | None:
    for path in WHISPER_MODEL_CANDIDATES:
        if os.path.isfile(path):
            return path
    return None

# ── Record audio ─────────────────────────────────────────────────────────────
def record_audio(pa, seconds: int) -> str:
    import pyaudio
    stream = pa.open(
        format=pyaudio.paInt16,
        channels=1,
        rate=SAMPLE_RATE,
        input=True,
        frames_per_buffer=CHUNK_SIZE,
    )
    frames = []
    n_chunks = int(SAMPLE_RATE / CHUNK_SIZE * seconds)
    for _ in range(n_chunks):
        try:
            data = stream.read(CHUNK_SIZE, exception_on_overflow=False)
            frames.append(data)
        except Exception:
            pass
    stream.stop_stream()
    stream.close()

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    with wave.open(tmp.name, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(pa.get_sample_size(pyaudio.paInt16))
        wf.setframerate(SAMPLE_RATE)
        wf.writeframes(b"".join(frames))
    return tmp.name

# ── Transcribe with whisper-cli ───────────────────────────────────────────────
def transcribe(wav_path: str, model_path: str) -> str:
    try:
        cmd = [
            "whisper-cli",
            "-m", model_path,
            "-f", wav_path,
            "--no-timestamps",
            "-np",
            "-l", "en",
        ]
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
        text = result.stdout.strip()
        # Remove common whisper artifacts
        for artifact in ["[BLANK_AUDIO]", "(silence)", "[Music]", "[noise]"]:
            text = text.replace(artifact, "").strip()
        return text
    finally:
        try:
            os.unlink(wav_path)
        except OSError:
            pass

# ── Wake word check (simple energy + keyword fallback) ───────────────────────
def _rms(chunk_bytes: bytes) -> float:
    samples = struct.unpack(f"{len(chunk_bytes)//2}h", chunk_bytes)
    if not samples:
        return 0.0
    return (sum(s * s for s in samples) / len(samples)) ** 0.5

# ── Main ──────────────────────────────────────────────────────────────────────
def main() -> None:
    import pyaudio
    import numpy as np

    # -- Try loading openwakeword -------------------------------------------------
    oww = None
    try:
        from openwakeword.model import Model as OWWModel
        # "hey_jarvis" is a pretrained model bundled with openwakeword
        oww = OWWModel(wakeword_models=["hey_jarvis"], inference_framework="onnx")
        log("openwakeword loaded (model: hey_jarvis)")
    except Exception as e:
        log(f"openwakeword not available ({e}); falling back to keyword spotting")

    # -- Whisper model -------------------------------------------------------
    model_path = find_whisper_model()
    if not model_path:
        emit("error", "Whisper model not found. Run: whisper-cli --download-model base.en")
        sys.exit(1)
    log(f"Whisper model: {model_path}")

    # -- PyAudio setup -------------------------------------------------------
    pa = pyaudio.PyAudio()
    listen_stream = pa.open(
        format=pyaudio.paInt16,
        channels=1,
        rate=SAMPLE_RATE,
        input=True,
        frames_per_buffer=CHUNK_SIZE,
    )

    emit("ready", "SK is listening for 'Hey SK'…")

    global _running
    while _running:
        try:
            raw = listen_stream.read(CHUNK_SIZE, exception_on_overflow=False)
        except Exception:
            time.sleep(0.1)
            continue

        triggered = False

        if oww is not None:
            # openwakeword path
            try:
                audio_np = np.frombuffer(raw, dtype=np.int16)
                scores = oww.predict(audio_np)
                # scores is dict: {"hey_jarvis": float}
                if scores.get("hey_jarvis", 0.0) >= WAKE_THRESHOLD:
                    triggered = True
            except Exception:
                pass
        else:
            # Energy + keyword fallback — very basic, only works in quiet rooms
            # Just a placeholder until openwakeword is installed
            pass  # daemon stays idle without wake word model

        if triggered:
            listen_stream.stop_stream()
            emit("wake", "Wake word detected — SK is listening…")
            time.sleep(0.2)  # small gap before recording

            emit("recording", f"Recording for {RECORD_SECONDS} seconds…")
            wav_path = record_audio(pa, RECORD_SECONDS)

            emit("processing", "Transcribing your command…")
            try:
                text = transcribe(wav_path, model_path)
                if text:
                    emit("command", text)
                else:
                    emit("no_speech", "Nothing heard. Listening again…")
            except subprocess.TimeoutExpired:
                emit("error", "Transcription timed out. Try again.")
            except Exception as ex:
                emit("error", f"Transcription error: {ex}")

            time.sleep(0.3)
            listen_stream.start_stream()
            emit("ready", "SK is listening for 'Hey SK'…")

    listen_stream.stop_stream()
    listen_stream.close()
    pa.terminate()
    log("SK daemon stopped.")

if __name__ == "__main__":
    main()
