# AI-Native System Control Layer

Privacy-first Linux desktop overlay — `Ctrl+Space` global toggle, local
Ollama-backed intent parsing, and a narrow set of safe system tools.
All compute stays on your machine. No data leaves.

[![CI](https://github.com/skartik06/ai-native-control-layer/actions/workflows/verify-linux.yml/badge.svg)](https://github.com/skartik06/ai-native-control-layer/actions/workflows/verify-linux.yml)

---

## Features

| Category | Action | Notes |
|---|---|---|
| **Read-only** | System info, file search, large-file list, logs, network status | No confirmation |
| **Clipboard** | Read clipboard | No confirmation |
| **Clipboard write** | `copy <text> to clipboard` | Confirmation required; uses `wl-copy` / `xclip` |
| **Wi-Fi** | Connect / disconnect saved profiles | Confirmation; uses `nmcli` fixed args |
| **Settings** | Dark mode, Wi-Fi toggle, brightness, volume, Do Not Disturb | Confirmation; GNOME `gsettings` |
| **Media** | Play, pause, next, previous | Confirmation; `playerctl` |
| **Launch** | File manager, browser, terminal, calendar | Confirmation; fixed app list |
| **Notifications** | Desktop notification | Confirmation; `notify-send` |
| **Screenshot** | Screen capture to Pictures | Confirmation |
| **Window control** | Focus, minimize, maximize, close | Confirmation; `wmctrl` |
| **Reminders** | Natural-language + date/time | Fires while app is running |
| **Push-to-talk** | Local STT → command bar | Uses `whisper.cpp` locally |
| **Chat mode** | Conversational Q&A | Never executes system actions |
| **Memory** | Opt-in preference memory | Explicitly saved only, deletable |

---

## Linux prerequisites

### Required

```bash
sudo apt update
sudo apt install -y \
  build-essential curl git \
  libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev \
  libxdo-dev libssl-dev patchelf

# Rust toolchain
curl https://sh.rustup.rs -sSf | sh

# Node.js (20+) via nvm or apt
sudo apt install -y nodejs npm
npm install -g pnpm@10 corepack
```

### Ollama (AI model backend)

```bash
curl -fsSL https://ollama.com/install.sh | sh
ollama pull qwen3:4b        # ~2.5 GB, runs on CPU
```

### Optional — full feature set

```bash
# Media controls
sudo apt install -y playerctl

# Settings (GNOME)
sudo apt install -y brightnessctl

# Desktop notifications
sudo apt install -y libnotify-bin

# Clipboard write
sudo apt install -y wl-clipboard xclip   # install both; app picks the right one

# Window control
sudo apt install -y wmctrl

# Text-to-speech
sudo apt install -y speech-dispatcher espeak-ng

# Voice push-to-talk (local, no cloud)
sudo apt install -y whisper-cpp          # Ubuntu 24.10+
whisper-cli --download-model base.en     # run once to fetch the model
```

---

## Quick start

```bash
git clone https://github.com/skartik06/ai-native-control-layer.git
cd ai-native-control-layer
corepack enable && pnpm install
pnpm tauri dev
```

Press **Ctrl+Space** to toggle the overlay. Some desktop environments
reserve this; change `SHORTCUT` in `src/main.tsx` if registration fails.

### Model selection

The app auto-detects installed Ollama models and prefers Qwen3. To force a
specific model:

```bash
OLLAMA_MODEL=qwen3:1.7b pnpm tauri dev    # lighter, for small VMs
OLLAMA_TIMEOUT_MS=60000 pnpm tauri dev    # 60 s Ollama timeout
```

---

## Safety model

- **No shell pass-through.** Every tool call uses a fixed, whitelisted
  argument list. The model cannot construct or inject shell strings.
- **Confirmation gate.** All state-changing actions (settings, clipboard,
  Wi-Fi, launches) show a preview and expire after 60 seconds.
- **Risk tiers enforced.** The planner independently re-validates every
  model response and rejects unrecognised parameters, low confidence
  (< 0.9), or a mismatched risk tier.
- **Local-only.** Intent parsing, chat, STT, TTS, reminders, memory, and
  audit logging all run on your machine.

---

## Running as a background service

See [docs/BACKGROUND_SERVICE.md](docs/BACKGROUND_SERVICE.md) for systemd
setup, optional environment flags, and wake-word architecture notes.

---

## Test checklist

See [docs/DIRECT_UBUNTU_TEST.md](docs/DIRECT_UBUNTU_TEST.md) for the full
acceptance test checklist.

---

## Architecture

```
Ctrl+Space
    │
    ▼
React frontend (src/main.tsx)
    │ Tauri IPC invoke
    ▼
parse_intent_internal (src-tauri/src/main.rs)
    │
    ├─ Local parsers (fast-path, no Ollama needed)
    │    clipboard write · wifi connect/disconnect ·
    │    screenshot · clipboard read · window control ·
    │    toggle · media · notification · launch
    │
    └─ Ollama (qwen3:4b, structured JSON, think=false)
         │
         ▼
     validate_intent → planned_risk → plan_intent
         │
         ▼
     PendingConfirmation (60 s TTL)
         │ User confirms
         ▼
     execute (nmcli / gsettings / playerctl / wl-copy / wmctrl …)
         │
         ▼
     AuditLog (SQLite, local app-data dir)
```

---

## Scope boundaries

- File deletion, package installation, and all high-risk operations are not implemented.
- Application launch is limited to a fixed whitelist (file manager, browser, terminal, calendar).
- Wi-Fi actions only work with saved NetworkManager connection profiles.
- Settings adapters (dark mode, Do Not Disturb) require GNOME; other desktops return a clear error.
- Wake-word detection is **off by default** (see BACKGROUND_SERVICE.md).
