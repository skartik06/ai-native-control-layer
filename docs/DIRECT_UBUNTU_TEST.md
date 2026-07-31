# Direct Ubuntu GNOME test checklist

This is the primary test environment for the Full Desktop profile. A VM remains useful for Lite-profile regression checks, but CPU-only VMs are not a performance benchmark for local language models.

## Install prerequisites

```bash
sudo apt update
sudo apt install -y build-essential curl git libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev libssl-dev patchelf libnotify-bin playerctl speech-dispatcher espeak-ng
curl -fsSL https://ollama.com/install.sh | sh
curl https://sh.rustup.rs -sSf | sh
```

Restart the terminal after Rust installs, then clone the repository and use a suitable local model. `qwen3:4b` is the normal Full profile baseline; use a model that fits the available RAM/VRAM.

```bash
git clone https://github.com/skartik06/ai-native-control-layer.git
cd ai-native-control-layer
corepack enable
pnpm install
ollama pull qwen3:4b
OLLAMA_MODEL=qwen3:4b pnpm tauri dev
```

## Acceptance checks

1. `show my system information` returns read-only data without confirmation.
2. `open firefox` displays a preview; **Confirm** launches the browser.
3. `turn on dark mode` displays a preview; **Confirm** changes the GNOME setting.
4. `pause music` or `next song` displays a preview; **Confirm** uses `playerctl` when a supported player is active.
5. `remind me to drink water` displays a preview; **Confirm** sends a desktop notification.
6. In Chat mode, `hi`, `thanks`, and `what can you do?` reply immediately. Longer chat requires the local Ollama model and can be stopped with **Stop**.
7. After a response, **Speak response** works when `speech-dispatcher` or `espeak-ng` is installed.

## Known boundaries

- Push-to-talk transcription and wake-word listening are not yet shipped; `whisper.cpp` detection is present so the feature can be added without a cloud dependency.
- This app does not delete files, install packages, or execute model-produced shell commands.
- Application launch, settings, media, and notifications always use an explicit confirmation preview.
