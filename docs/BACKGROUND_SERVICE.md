# Background Service & Wake-Word Architecture

This document describes how to run the AI Native Control Layer as a persistent
systemd user service and the opt-in wake-word detection feature.

---

## Running as a systemd Service

The package installs `packaging/ai-native-control-layer.service` into
`~/.config/systemd/user/`. After installation:

```bash
systemctl --user daemon-reload
systemctl --user enable --now ai-native-control-layer
journalctl --user -u ai-native-control-layer -f   # live logs
```

The service starts automatically at login (after `graphical-session.target`)
and restarts on failure.

---

## Feature Flags (Environment Variables)

Set these in the `[Service]` block (uncomment in the `.service` file):

| Variable | Default | Purpose |
|---|---|---|
| `WAKE_WORD_ENABLED` | `false` | Enable always-on STT wake-word detection |
| `OLLAMA_MODEL` | auto-detected | Override the Ollama model used for intent parsing |
| `OLLAMA_TIMEOUT_MS` | `30000` | Timeout for Ollama requests in ms |

---

## Wake-Word Detection (Opt-In, Off by Default)

> [!CAUTION]
> Wake-word detection requires keeping a microphone stream open continuously.
> **Only enable this if you understand and accept the privacy implications.**

### Architecture

```
Microphone stream
     │
     ▼
whisper-stream (whisper.cpp streaming binary)
     │  Detects a user-defined keyword phrase
     ▼
stdin pipe → ai-native-control-layer IPC socket (planned)
     │
     ▼
parse_intent → confirmation gate → execute
```

### Current Status

The wake-word pipeline is **foundational only** in this release.  The backend
exposes `get_wake_word_status` (reads `WAKE_WORD_ENABLED`) and the frontend
displays the status in the capability row.  Full always-on streaming integration
is planned for a future release.

### Prerequisites (when feature is fully implemented)

```bash
# 1. Install whisper.cpp with streaming support
sudo apt install whisper-cpp        # Ubuntu 24.10+ with streaming binary
# or build from source:
# https://github.com/ggerganov/whisper.cpp#whisper-stream

# 2. Download a model
whisper-cli --download-model base.en
# Placed in: ~/.local/share/whisper.cpp/models/ggml-base.en.bin

# 3. Enable in service
systemctl --user edit ai-native-control-layer
# Add:
# [Service]
# Environment=WAKE_WORD_ENABLED=true
```

---

## Push-to-Talk (PTT) — Available Now

PTT is live in the current release. Press **🎙 Speak** in the UI to record,
then **⏹ Stop** to transcribe and paste into the command bar.

### Prerequisites

```bash
# Install whisper.cpp
sudo apt install whisper-cpp         # Ubuntu 24.10+
# or build from source

# Download a model (run once)
whisper-cli --download-model base.en

# Verify
whisper-cli --help
```

The backend looks for the model in:
- `~/.local/share/whisper.cpp/models/ggml-base.en.bin`
- `~/whisper.cpp/models/ggml-base.en.bin`
- `~/.local/share/whisper.cpp/models/ggml-small.en.bin`
- `/usr/share/whisper-cpp/models/ggml-base.en.bin`
- `/usr/local/share/whisper-cpp/models/ggml-base.en.bin`

Audio is saved to the app data directory (`~/.local/share/ai-native-control-layer/`)
temporarily, transcribed, then immediately deleted.

---

## Privacy Guarantees

- All processing is local. No data leaves the machine.
- Transcription temp files are deleted immediately after use.
- Wake-word detection is **off by default** and requires explicit opt-in.
- The audit log records all control actions locally (SQLite in app data dir).
