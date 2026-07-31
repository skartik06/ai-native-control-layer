# AI-Native System Control Layer

Linux-first Tauri desktop overlay with a `Ctrl+Space` global toggle, local Ollama-backed structured intent parsing, and a narrow set of safe, read-only system tools.

## Linux prerequisites

Install a current Rust toolchain, Node.js 20+, Ollama, and your distro's Tauri/WebKit and NetworkManager dependencies. On Debian/Ubuntu, see the [Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux).

For the optional settings adapters, install `brightnessctl` for brightness control and `playerctl` for media playback controls. Dark mode and Do Not Disturb currently use GNOME's `gsettings` schemas; unsupported desktops return an error instead of guessing.

## Run

```bash
pnpm install
pnpm tauri dev
```

`Ctrl+Space` toggles the overlay. Some Linux desktop environments reserve that shortcut; if registration fails, change `shortcut` in `src/main.tsx` to an unused accelerator.

## Free local AI setup (Step 2)

Install [Ollama](https://ollama.com/download), then download the free local model once:

```bash
ollama run qwen3:4b
```

After its first response, stop it with `Ctrl+C`; Ollama keeps the model available locally. Then run the app:

```bash
pnpm tauri dev
```

`qwen3:4b` is a ~2.5 GB download. Optionally set `OLLAMA_MODEL` to another locally downloaded model. The backend writes raw input and model output to its local app-data debug log as required for development; the log is not uploaded anywhere by the app.

If `OLLAMA_MODEL` is not set, the app now detects installed Ollama models and prefers Qwen3 (including `qwen3:1.7b` for small CPU-only VMs). Set `OLLAMA_MODEL` only when you want to force a particular installed model.

The intent request disables Qwen3 thinking and limits its context/output to the small size required for structured JSON. This avoids wasting CPU and RAM in virtual machines. The local response timeout is 180 seconds by default; to allow up to ten minutes for a request:

```bash
OLLAMA_TIMEOUT_SECONDS=600 OLLAMA_MODEL=qwen3:4b pnpm tauri dev
```

## Current MVP scope

The action planner independently validates every model response. It rejects unrecognised parameters, confidence below 0.9, and a model-selected risk tier that does not match the tool.

Read-only tools that can run automatically on Linux are:

- file search inside the current user's home directory (up to 50 results, limited walk depth)
- system storage, memory, CPU load, and running applications
- large-file listing inside the current user's home directory
- NetworkManager Wi-Fi status through `nmcli`
- recent systemd service logs through `journalctl`

Settings changes, whitelisted application launches, and media playback changes require confirmation. File deletion, package installation, and all other high-risk operations are not implemented. No low-risk tool accepts or executes a shell string from the model.

## Setting confirmation gate

Medium-risk operations are whitelisted settings, application launches, and media playback controls. The backend validates every parameter before it shows a preview. A confirmation is kept only in backend memory, expires after 60 seconds, and is discarded on cancel or after one attempt. The frontend cannot modify the planned action after preview.

Simple on/off requests for Wi-Fi, dark mode, and Do Not Disturb are locally parsed before Ollama. This makes the safety preview reliable even when a small local model cannot produce complete JSON. The backend still applies the same whitelist and confirmation rules.

Current Linux adapters:

- Wi-Fi on/off through NetworkManager `nmcli`
- brightness from 0–100% through `brightnessctl`
- GNOME dark mode through `gsettings`
- GNOME Do Not Disturb through `gsettings`
- play, pause, next, and previous track through `playerctl`

Application and media phrases such as `open firefox`, `opn files`, `pause music`, and `next song` are locally parsed before Ollama, then presented for confirmation. This preserves responsiveness even on a low-resource VM.

## Local audit history (Step 5)

Every parsed request is recorded in a local SQLite database: clarifications, rejections, confirmation previews, cancellations, expirations, tool starts, successes, and failures. Each event stores the validated action, risk tier, parameters, outcome, summary, and (where applicable) tool result. The database remains on the device in the app's OS data directory as `audit.sqlite3`; the app does not upload it. The backend exposes a bounded `get_audit_history` command for the future history UI.

## Verification

Run the frontend check locally:

```bash
pnpm build
```

Each push to `main` also runs a Linux compilation check in GitHub Actions.

## Linux release packages (Step 7)

The **Package Linux desktop app** GitHub Actions workflow produces a Debian package (`.deb`) and portable AppImage. Run it manually from the repository's **Actions** tab, or create and push a version tag such as `v0.1.0`. Download the `ai-native-control-layer-linux` artifact from the completed run. On Debian/Kali/Ubuntu, install the downloaded Debian package with:

```bash
sudo apt install ./ai-native-control-layer_0.1.0_amd64.deb
```

The packaged app still needs a local Ollama service and selected model; for a CPU-only VM, start it with `OLLAMA_MODEL=qwen3:1.7b ai-native-control-layer`.

## Start with the desktop session

After installing the Debian package, enable the user-level assistant service once:

```bash
mkdir -p ~/.config/systemd/user
cp /usr/share/doc/ai-native-control-layer/ai-native-control-layer.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ai-native-control-layer.service
```

The service starts the overlay when the user signs in and restarts it if it crashes. Disable it at any time with `systemctl --user disable --now ai-native-control-layer.service`.
