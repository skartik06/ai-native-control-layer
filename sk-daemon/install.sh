#!/usr/bin/env bash
# SK Daemon — install script
# Run once on Ubuntu: bash sk-daemon/install.sh

set -e
echo "==> Installing SK daemon dependencies..."

# System packages
sudo apt-get update -qq
sudo apt-get install -y \
    python3-pip \
    python3-pyaudio \
    portaudio19-dev \
    python3-numpy \
    whisper-cpp \
    alsa-utils

# ── Piper TTS (natural voice) ──────────────────────────────────────────────
echo "==> Installing piper TTS..."
PIPER_DIR="$HOME/.local/share/piper"
PIPER_BIN="$HOME/.local/bin/piper"
mkdir -p "$PIPER_DIR" "$HOME/.local/bin"

if ! command -v piper &>/dev/null; then
    # Download piper binary (amd64 Linux)
    PIPER_RELEASE="https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_linux_x86_64.tar.gz"
    TMP_TAR=$(mktemp /tmp/piper_XXXXXX.tar.gz)
    echo "  Downloading piper binary..."
    curl -L -o "$TMP_TAR" "$PIPER_RELEASE" 2>/dev/null && \
        tar -xzf "$TMP_TAR" -C "$HOME/.local/" && \
        chmod +x "$HOME/.local/piper/piper" && \
        ln -sf "$HOME/.local/piper/piper" "$PIPER_BIN" && \
        echo "  piper installed at $PIPER_BIN" || \
        echo "  piper download failed — install manually from https://github.com/rhasspy/piper/releases"
    rm -f "$TMP_TAR"
fi

# Download piper voice model (en_US-lessac-medium ~60 MB)
VOICE_MODEL="$PIPER_DIR/en_US-lessac-medium.onnx"
VOICE_CONFIG="$PIPER_DIR/en_US-lessac-medium.onnx.json"
if [ ! -f "$VOICE_MODEL" ]; then
    echo "  Downloading en_US-lessac-medium voice model (~60 MB)..."
    BASE="https://huggingface.co/rhasspy/piper-voices/resolve/main/en/en_US/lessac/medium"
    curl -L -o "$VOICE_MODEL"  "$BASE/en_US-lessac-medium.onnx"  2>/dev/null
    curl -L -o "$VOICE_CONFIG" "$BASE/en_US-lessac-medium.onnx.json" 2>/dev/null
    echo "  Voice model saved to $VOICE_MODEL"
else
    echo "  Piper voice model already present."
fi

# Python packages
pip3 install --user \
    openwakeword \
    onnxruntime

# Download whisper base.en model if not present
WHISPER_DIR="$HOME/.local/share/whisper"
MODEL_PATH="$WHISPER_DIR/ggml-base.en.bin"

if [ ! -f "$MODEL_PATH" ]; then
    echo "==> Downloading whisper base.en model (~150 MB)..."
    mkdir -p "$WHISPER_DIR"
    # Try whisper-cli download first
    if command -v whisper-cli &>/dev/null; then
        whisper-cli --download-model base.en -d "$WHISPER_DIR" 2>/dev/null || true
    fi
    # Fallback: direct download
    if [ ! -f "$MODEL_PATH" ]; then
        curl -L -o "$MODEL_PATH" \
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"
    fi
    echo "==> Whisper model saved to $MODEL_PATH"
else
    echo "==> Whisper model already present: $MODEL_PATH"
fi

# Download openwakeword hey_jarvis model
echo "==> Pre-downloading openwakeword hey_jarvis model..."
python3 -c "
from openwakeword.model import Model
Model(wakeword_models=['hey_jarvis'], inference_framework='onnx')
print('hey_jarvis model ready.')
" 2>/dev/null || echo "(Model will be downloaded on first daemon start)"

echo ""
echo "==> SK daemon install complete!"
echo ""
echo "To start the daemon:"
echo "  python3 sk-daemon/sk_daemon.py"
echo ""
echo "Or enable it inside the SK app using the 'Start Voice Daemon' button."
