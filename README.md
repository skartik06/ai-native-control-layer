# AI-Native System Control Layer

Linux-first Tauri desktop overlay with a `Ctrl+Space` global toggle, local Ollama-backed structured intent parsing, and a narrow set of safe, read-only system tools.

## Linux prerequisites

Install a current Rust toolchain, Node.js 20+, Ollama, and your distro's Tauri/WebKit and NetworkManager dependencies. On Debian/Ubuntu, see the [Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux).

For the optional settings adapters, install `brightnessctl` for brightness control. Dark mode and Do Not Disturb currently use GNOME's `gsettings` schemas; unsupported desktops return an error instead of guessing.

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

Settings changes, app launching, file deletion, package installation, and all other high-risk operations are not implemented. No low-risk tool accepts or executes a shell string from the model.

## Setting confirmation gate

The only medium-risk operation is a whitelisted `toggle_setting`. The backend validates both the setting and value before it shows a preview. A confirmation is kept only in backend memory, expires after 60 seconds, and is discarded on cancel or after one attempt. The frontend cannot modify the setting value after preview.

Simple on/off requests for Wi-Fi, dark mode, and Do Not Disturb are locally parsed before Ollama. This makes the safety preview reliable even when a small local model cannot produce complete JSON. The backend still applies the same whitelist and confirmation rules.

Current Linux adapters:

- Wi-Fi on/off through NetworkManager `nmcli`
- brightness from 0–100% through `brightnessctl`
- GNOME dark mode through `gsettings`
- GNOME Do Not Disturb through `gsettings`

## Verification

Run the frontend check locally:

```bash
pnpm build
```

Each push to `main` also runs a Linux compilation check in GitHub Actions.
