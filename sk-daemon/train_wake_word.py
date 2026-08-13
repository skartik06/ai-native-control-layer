#!/usr/bin/env python3
"""
SK Wake Word Trainer
Builds a DTW-based custom verifier from recorded "Hey SK" samples,
and optionally generates extra synthetic samples via piper TTS.

Output: ~/.local/share/ai-native-control-layer/hey_sk_verifier.npz
        (numpy archive containing MFCC templates for DTW matching)

Prints JSON progress lines to stdout.
"""

import sys
import json
import os
import wave
import subprocess
import tempfile
import glob
import argparse
import struct

import numpy as np

SAMPLE_RATE  = 16_000
SAMPLES_DIR  = os.path.expanduser("~/.local/share/ai-native-control-layer/hey_sk_samples")
OUTPUT_DIR   = os.path.expanduser("~/.local/share/ai-native-control-layer")
OUTPUT_FILE  = os.path.join(OUTPUT_DIR, "hey_sk_verifier.npz")

PIPER_VARIANTS = [
    "hey s k",
    "hey ess kay",
    "hey sk",
    "hey s. k.",
    "hey, s k",
]
PIPER_LENGTH_SCALES = [0.8, 0.9, 1.0, 1.1, 1.2]


def emit(type_: str, text: str = "", progress: int = 0) -> None:
    print(json.dumps({"type": type_, "text": text, "progress": progress}), flush=True)


# ── MFCC feature extraction (pure numpy, no librosa dependency) ────────────────

def _preemphasis(signal: np.ndarray, coeff: float = 0.97) -> np.ndarray:
    return np.append(signal[0], signal[1:] - coeff * signal[:-1])


def _framing(signal: np.ndarray, frame_len: int, hop_len: int) -> np.ndarray:
    n_frames = 1 + (len(signal) - frame_len) // hop_len
    indices = (
        np.tile(np.arange(frame_len), (n_frames, 1))
        + np.tile(np.arange(n_frames) * hop_len, (frame_len, 1)).T
    )
    return signal[indices]


def _mel_filterbank(n_filt: int, n_fft: int, sr: int,
                    fmin: float = 80.0, fmax: float = 8000.0) -> np.ndarray:
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


def extract_mfcc(wav_path: str, n_mfcc: int = 13) -> np.ndarray:
    """Extract MFCC features from a WAV file (pure numpy)."""
    with wave.open(wav_path, "rb") as wf:
        sr     = wf.getframerate()
        n_chan = wf.getnchannels()
        raw    = wf.readframes(wf.getnframes())

    # Decode PCM16
    samples = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    if n_chan == 2:
        samples = samples[::2]  # mono

    # Resample to 16 kHz if needed (simple decimation — good enough for MFCC)
    if sr != SAMPLE_RATE and sr > 0:
        ratio   = SAMPLE_RATE / sr
        new_len = int(len(samples) * ratio)
        samples = np.interp(np.linspace(0, len(samples) - 1, new_len),
                            np.arange(len(samples)), samples)

    samples = _preemphasis(samples)

    frame_len = int(SAMPLE_RATE * 0.025)   # 25 ms
    hop_len   = int(SAMPLE_RATE * 0.010)   # 10 ms
    n_fft     = 512
    n_filt    = 40

    frames   = _framing(samples, frame_len, hop_len)
    frames  *= np.hamming(frame_len)
    mag      = np.abs(np.fft.rfft(frames, n=n_fft))
    fbank    = _mel_filterbank(n_filt, n_fft, SAMPLE_RATE)
    mel_spec = np.dot(mag, fbank.T)
    mel_spec = np.log(mel_spec + 1e-8)

    # DCT
    n_frames  = mel_spec.shape[0]
    dct_mfcc  = np.zeros((n_frames, n_mfcc))
    for k in range(n_mfcc):
        dct_mfcc[:, k] = np.sum(
            mel_spec * np.cos(np.pi * k / n_filt * (np.arange(n_filt) + 0.5)),
            axis=1,
        )
    # Z-score normalise per coefficient
    mean = dct_mfcc.mean(axis=0, keepdims=True)
    std  = dct_mfcc.std(axis=0, keepdims=True) + 1e-8
    return (dct_mfcc - mean) / std


# ── DTW distance ──────────────────────────────────────────────────────────────

def dtw_distance(a: np.ndarray, b: np.ndarray) -> float:
    """Compute symmetric DTW distance between two MFCC sequences."""
    n, m = len(a), len(b)
    D = np.full((n + 1, m + 1), np.inf)
    D[0, 0] = 0.0
    for i in range(1, n + 1):
        for j in range(1, m + 1):
            cost = np.linalg.norm(a[i - 1] - b[j - 1])
            D[i, j] = cost + min(D[i - 1, j], D[i, j - 1], D[i - 1, j - 1])
    return float(D[n, m]) / (n + m)


# ── Piper synthetic sample generation ─────────────────────────────────────────

def find_piper_model() -> str | None:
    home = os.path.expanduser("~")
    candidates = [
        f"{home}/.local/share/piper/en_US-lessac-medium.onnx",
        f"{home}/.local/share/piper/en_US-lessac-high.onnx",
        f"{home}/.local/share/piper/en_US-ryan-high.onnx",
        f"{home}/.local/share/piper/en_US-ryan-medium.onnx",
        "/usr/share/piper/voices/en_US-lessac-medium.onnx",
    ]
    for c in candidates:
        if os.path.isfile(c):
            return c
    return None


def generate_piper_samples(piper_model: str, out_dir: str, count: int = 25) -> list[str]:
    """Generate synthetic 'Hey SK' samples using piper TTS."""
    os.makedirs(out_dir, exist_ok=True)
    paths = []
    idx = 0
    for variant in PIPER_VARIANTS:
        for scale in PIPER_LENGTH_SCALES:
            if idx >= count:
                break
            out_file = os.path.join(out_dir, f"synth_{idx:03d}.wav")
            cmd = (
                f"echo '{variant}' | piper --model {piper_model} "
                f"--length-scale {scale:.1f} "
                f"--output-file {out_file} 2>/dev/null"
            )
            result = subprocess.run(["sh", "-c", cmd], capture_output=True, timeout=15)
            if result.returncode == 0 and os.path.isfile(out_file):
                paths.append(out_file)
            idx += 1
        if idx >= count:
            break
    return paths


# ── Main training ─────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--synth-only", action="store_true",
                        help="Use only piper-generated samples (no personal recordings)")
    args = parser.parse_args()

    emit("start", "SK wake word trainer starting…", 0)

    # 1. Collect personal recordings
    personal = sorted(glob.glob(os.path.join(SAMPLES_DIR, "*.wav")))
    emit("log", f"Found {len(personal)} personal recording(s).", 5)

    if not personal and not args.synth_only:
        emit("error", "No personal samples found. Record at least 1 sample first.")
        sys.exit(1)

    all_wav_paths = list(personal)

    # 2. Generate piper synthetic samples (always augment with TTS)
    piper_model = find_piper_model()
    if piper_model:
        emit("log", f"Generating synthetic samples via piper…", 10)
        synth_dir = tempfile.mkdtemp(prefix="sk_train_")
        synth_paths = generate_piper_samples(piper_model, synth_dir, count=25)
        all_wav_paths.extend(synth_paths)
        emit("log", f"Generated {len(synth_paths)} synthetic samples.", 30)
    else:
        emit("warn", "Piper not found — training on personal recordings only.")

    if not all_wav_paths:
        emit("error", "No training samples available.")
        sys.exit(1)

    # 3. Extract MFCC features
    emit("log", "Extracting MFCC features…", 35)
    templates = []
    for i, path in enumerate(all_wav_paths):
        try:
            mfcc = extract_mfcc(path)
            templates.append(mfcc)
        except Exception as ex:
            emit("warn", f"Skipping {os.path.basename(path)}: {ex}")
        prog = 35 + int(i / len(all_wav_paths) * 40)
        if i % 5 == 0:
            emit("progress", f"Processing sample {i+1}/{len(all_wav_paths)}…", prog)

    if not templates:
        emit("error", "Feature extraction failed for all samples.")
        sys.exit(1)

    # 4. Compute DTW threshold from pairwise distances
    emit("log", "Computing DTW decision threshold…", 78)
    distances = []
    sample_set = templates[:min(len(templates), 20)]   # cap to avoid O(n²) blowup
    for i in range(len(sample_set)):
        for j in range(i + 1, len(sample_set)):
            distances.append(dtw_distance(sample_set[i], sample_set[j]))

    if distances:
        threshold = float(np.mean(distances) + np.std(distances))
    else:
        threshold = 5.0   # sensible default

    emit("log", f"DTW threshold: {threshold:.4f}", 85)

    # 5. Save verifier model
    os.makedirs(OUTPUT_DIR, exist_ok=True)
    np.savez_compressed(
        OUTPUT_FILE,
        templates=np.array(templates, dtype=object),
        threshold=np.array([threshold]),
        n_mfcc=np.array([13]),
        sample_rate=np.array([SAMPLE_RATE]),
    )

    emit("done",
         f"✅ 'Hey SK' model saved ({len(templates)} templates, threshold={threshold:.3f}). "
         f"Restart SK daemon to activate.",
         100)

    # Clean synthetic temp dir
    if piper_model:
        import shutil
        try:
            shutil.rmtree(synth_dir, ignore_errors=True)
        except Exception:
            pass


if __name__ == "__main__":
    main()
