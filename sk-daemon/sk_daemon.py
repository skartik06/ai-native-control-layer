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
import argparse

# ── Graceful shutdown ────────────────────────────────────────────────────────
_running = True
_log_file = None  # Optional file handle for systemd log output

def _handle_sigterm(signum, frame):
    global _running
    _running = False

signal.signal(signal.SIGTERM, _handle_sigterm)
signal.signal(signal.SIGINT,  _handle_sigterm)

# ── Helpers ──────────────────────────────────────────────────────────────────
def emit(type_: str, text: str = "") -> None:
    msg = json.dumps({"type": type_, "text": text})
    print(msg, flush=True)
    if _log_file:
        try:
            _log_file.write(msg + "\n")
            _log_file.flush()
        except Exception:
            pass

def log(msg: str) -> None:
    emit("log", msg)

# ── Audio config ─────────────────────────────────────────────────────────────
SAMPLE_RATE   = 16_000
CHUNK_SIZE     = 1_280   # 80 ms at 16 kHz — openwakeword recommended
RECORD_SECONDS = 5
WAKE_THRESHOLD = 0.5

# ── Custom wake word verifier path ───────────────────────────────────────────
VERIFIER_PATH = os.path.expanduser(
    "~/.local/share/ai-native-control-layer/hey_sk_verifier.npz"
)


def load_custom_verifier():
    """Load DTW templates + threshold from saved .npz file."""
    if not os.path.isfile(VERIFIER_PATH):
        return None
    try:
        data = np.load(VERIFIER_PATH, allow_pickle=True)
        templates  = list(data["templates"])
        threshold  = float(data["threshold"][0])
        log(f"Custom 'Hey SK' verifier loaded ({len(templates)} templates, thr={threshold:.3f})")
        return {"templates": templates, "threshold": threshold}
    except Exception as ex:
        log(f"Could not load custom verifier: {ex}")
        return None


def _preemphasis(signal, coeff=0.97):
    return np.append(signal[0], signal[1:] - coeff * signal[:-1])


def _framing(signal, frame_len, hop_len):
    n_frames = 1 + (len(signal) - frame_len) // hop_len
    idx = (
        np.tile(np.arange(frame_len), (n_frames, 1))
        + np.tile(np.arange(n_frames) * hop_len, (frame_len, 1)).T
    )
    return signal[idx]


def _mel_filterbank(n_filt, n_fft, sr, fmin=80.0, fmax=8000.0):
    def hz2mel(f): return 2595 * np.log10(1 + f / 700)
    def mel2hz(m): return 700 * (10 ** (m / 2595) - 1)
    mel_pts = np.linspace(hz2mel(fmin), hz2mel(fmax), n_filt + 2)
    hz_pts  = mel2hz(mel_pts)
    bins    = np.floor((n_fft + 1) * hz_pts / sr).astype(int)
    fbank   = np.zeros((n_filt, n_fft // 2 + 1))
    for m in range(1, n_filt + 1):
        for k in range(bins[m - 1], bins[m]):
            fbank[m - 1, k] = (k - bins[m - 1]) / (bins[m] - bins[m - 1] + 1e-8)
        for k in range(bins[m], bins[m + 1]):
            fbank[m - 1, k] = (bins[m + 1] - k) / (bins[m + 1] - bins[m] + 1e-8)
    return fbank


def extract_mfcc_from_bytes(pcm_bytes: bytes, n_mfcc: int = 13) -> np.ndarray:
    """Extract MFCC from raw PCM16 bytes (pure numpy)."""
    samples = np.frombuffer(pcm_bytes, dtype=np.int16).astype(np.float32) / 32768.0
    samples = _preemphasis(samples)
    frame_len = int(SAMPLE_RATE * 0.025)
    hop_len   = int(SAMPLE_RATE * 0.010)
    n_fft     = 512
    n_filt    = 40
    # Pad if too short
    if len(samples) < frame_len:
        samples = np.pad(samples, (0, frame_len - len(samples)))
    frames   = _framing(samples, frame_len, hop_len)
    frames  *= np.hamming(frame_len)
    mag      = np.abs(np.fft.rfft(frames, n=n_fft))
    fbank    = _mel_filterbank(n_filt, n_fft, SAMPLE_RATE)
    mel_spec = np.log(np.dot(mag, fbank.T) + 1e-8)
    dct_mfcc = np.zeros((mel_spec.shape[0], n_mfcc))
    for k in range(n_mfcc):
        dct_mfcc[:, k] = np.sum(
            mel_spec * np.cos(np.pi * k / n_filt * (np.arange(n_filt) + 0.5)), axis=1
        )
    mean = dct_mfcc.mean(axis=0, keepdims=True)
    std  = dct_mfcc.std(axis=0, keepdims=True) + 1e-8
    return (dct_mfcc - mean) / std


def dtw_distance(a: np.ndarray, b: np.ndarray) -> float:
    n, m = len(a), len(b)
    D = np.full((n + 1, m + 1), np.inf)
    D[0, 0] = 0.0
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            cost = float(np.linalg.norm(a[i - 1] - b[j - 1]))
            D[i, j] = cost + min(D[i - 1, j], D[i, j - 1], D[i - 1, j - 1])
    return D[n, m] / (n + m)


def verify_with_dtw(pcm_bytes: bytes, verifier: dict) -> bool:
    """Return True if the audio matches the 'Hey SK' templates via DTW."""
    try:
        candidate = extract_mfcc_from_bytes(pcm_bytes)
        dists = [dtw_distance(candidate, t) for t in verifier["templates"][:10]]
        best  = min(dists)
        return best <= verifier["threshold"]
    except Exception:
        return True   # If verifier errors, let OWW decision stand

# ── Whisper model search ─────────────────────────────────────────────────────
WHISPER_MODEL_CANDIDATES = [
    # snap install whisper-cpp  (most common on Ubuntu)
    os.path.expanduser("~/snap/whisper-cpp/common/models/ggml-base.en.bin"),
    os.path.expanduser("~/snap/whisper-cpp/current/models/ggml-base.en.bin"),
    os.path.expanduser("~/snap/whisper-cpp/x1/models/ggml-base.en.bin"),
    # manual / script install
    os.path.expanduser("~/.local/share/whisper/ggml-base.en.bin"),
    os.path.expanduser("~/.cache/whisper/ggml-base.en.bin"),
    # system-wide
    "/usr/share/whisper/models/ggml-base.en.bin",
    "/usr/local/share/whisper/ggml-base.en.bin",
    "/usr/share/whisper-cpp/models/ggml-base.en.bin",
]

WHISPER_BINARIES = [
    "whisper-cpp.cli",   # snap install whisper-cpp
    "whisper-cli",       # whisper.cpp built from source
    "whisper-cpp",       # some distros
]

def find_whisper_binary() -> str | None:
    for binary in WHISPER_BINARIES:
        result = subprocess.run(["which", binary], capture_output=True, text=True)
        if result.returncode == 0 and result.stdout.strip():
            return binary
    return None

def find_whisper_model() -> str | None:
    # Also search snap data dir dynamically
    snap_base = os.path.expanduser("~/snap/whisper-cpp")
    if os.path.isdir(snap_base):
        for root, _, files in os.walk(snap_base):
            for f in files:
                if f == "ggml-base.en.bin":
                    return os.path.join(root, f)
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

# ── Transcribe with whisper-cpp ──────────────────────────────────────────────
def transcribe(wav_path: str, model_path: str) -> str:
    binary = find_whisper_binary() or "whisper-cpp.cli"
    try:
        cmd = [
            binary,
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
    global _log_file

    parser = argparse.ArgumentParser(description="SK Voice Daemon")
    parser.add_argument("--log-file", default="",
                        help="Also write JSON events to this file (for systemd mode)")
    args = parser.parse_args()

    if args.log_file:
        try:
            os.makedirs(os.path.dirname(args.log_file), exist_ok=True)
            _log_file = open(args.log_file, "a", encoding="utf-8")
        except Exception as e:
            print(f"Warning: could not open log file {args.log_file}: {e}", flush=True)

    import pyaudio
    import numpy as np

    # -- Custom verifier -------------------------------------------------
    verifier = load_custom_verifier()
    wake_label = "hey_sk" if verifier else "hey_jarvis"

    # -- Try loading openwakeword ----------------------------------------
    oww = None
    try:
        from openwakeword.model import Model as OWWModel
        oww = OWWModel(wakeword_models=["hey_jarvis"], inference_framework="onnx")
        model_info = "hey_jarvis + custom Hey-SK verifier" if verifier else "hey_jarvis"
        log(f"openwakeword loaded ({model_info})")
    except Exception as e:
        log(f"openwakeword not available ({e}); falling back to energy detection")

    # -- Whisper model -------------------------------------------------------
    whisper_bin = find_whisper_binary()
    if not whisper_bin:
        emit("error", "whisper-cpp not found. Run: sudo snap install whisper-cpp")
        sys.exit(1)
    log(f"Whisper binary: {whisper_bin}")

    model_path = find_whisper_model()
    if not model_path:
        emit("error", "Whisper model not found. Run: whisper-cpp.download-ggml-model base.en")
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
            # openwakeword base detection
            try:
                audio_np = np.frombuffer(raw, dtype=np.int16)
                scores = oww.predict(audio_np)
                if scores.get("hey_jarvis", 0.0) >= WAKE_THRESHOLD:
                    # Second stage: DTW verifier (if custom model trained)
                    if verifier:
                        # Collect ~1 sec of recent audio for DTW check
                        if not triggered:
                            if verify_with_dtw(raw, verifier):
                                triggered = True
                    else:
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
            wake_phrase = "'Hey SK'" if verifier else "'Hey Jarvis' (train 'Hey SK' in settings)"
        emit("ready", f"SK is listening for {wake_phrase}…")

    listen_stream.stop_stream()
    listen_stream.close()
    pa.terminate()
    log("SK daemon stopped.")

if __name__ == "__main__":
    main()
