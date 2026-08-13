#!/usr/bin/env python3
"""
SK Wake Word Recorder
Records N short audio samples for custom wake word training.
Prints JSON progress to stdout for Tauri to consume.

Usage:
  python3 record_wake_word.py --sample-num 1
  python3 record_wake_word.py --sample-num 2
  ...
  python3 record_wake_word.py --sample-num 5
"""

import sys
import json
import os
import wave
import time
import argparse
import struct

SAMPLE_RATE   = 16_000
CHUNK_SIZE    = 1_024
RECORD_SECONDS = 2          # Each sample is 2 seconds
SAMPLES_DIR   = os.path.expanduser(
    "~/.local/share/ai-native-control-layer/hey_sk_samples"
)


def emit(type_: str, text: str = "", extra: dict = None) -> None:
    obj = {"type": type_, "text": text}
    if extra:
        obj.update(extra)
    print(json.dumps(obj), flush=True)


def _rms(data: bytes) -> float:
    samples = struct.unpack(f"{len(data)//2}h", data)
    return (sum(s * s for s in samples) / max(len(samples), 1)) ** 0.5


def record_sample(sample_num: int) -> str:
    import pyaudio
    pa = pyaudio.PyAudio()

    emit("countdown", f"Sample {sample_num}/5 — say 'Hey SK' in 3…", {"count": 3})
    time.sleep(1)
    emit("countdown", f"Sample {sample_num}/5 — say 'Hey SK' in 2…", {"count": 2})
    time.sleep(1)
    emit("countdown", f"Sample {sample_num}/5 — say 'Hey SK' in 1…", {"count": 1})
    time.sleep(0.5)

    emit("recording", f"🎙 Recording sample {sample_num} — say 'Hey SK' now!")

    stream = pa.open(
        format=pyaudio.paInt16,
        channels=1,
        rate=SAMPLE_RATE,
        input=True,
        frames_per_buffer=CHUNK_SIZE,
    )

    frames = []
    n_chunks = int(SAMPLE_RATE / CHUNK_SIZE * RECORD_SECONDS)
    peak_rms = 0.0
    for _ in range(n_chunks):
        try:
            data = stream.read(CHUNK_SIZE, exception_on_overflow=False)
            frames.append(data)
            rms = _rms(data)
            if rms > peak_rms:
                peak_rms = rms
        except Exception:
            pass

    stream.stop_stream()
    stream.close()
    pa.terminate()

    if peak_rms < 200:
        emit("warn", f"Sample {sample_num}: very quiet — make sure your mic is working.")

    os.makedirs(SAMPLES_DIR, exist_ok=True)
    out_path = os.path.join(SAMPLES_DIR, f"hey_sk_{sample_num:02d}.wav")

    with wave.open(out_path, "wb") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(SAMPLE_RATE)
        wf.writeframes(b"".join(frames))

    emit("done", f"✅ Sample {sample_num} saved.", {"path": out_path, "sample_num": sample_num})
    return out_path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sample-num", type=int, required=True,
                        help="Which sample number to record (1-5)")
    args = parser.parse_args()

    if not (1 <= args.sample_num <= 10):
        emit("error", "sample-num must be between 1 and 10")
        sys.exit(1)

    try:
        record_sample(args.sample_num)
    except Exception as ex:
        emit("error", f"Recording failed: {ex}")
        sys.exit(1)


if __name__ == "__main__":
    main()
