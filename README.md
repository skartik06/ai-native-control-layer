# AI-Native System Control Layer

Linux-first Tauri desktop overlay with a `Ctrl+Space` global toggle, local Ollama-backed structured intent parsing, and a narrow set of safe, read-only system tools.

## Linux prerequisites

Install a current Rust toolchain, Node.js 20+, Ollama, and your distro's Tauri/WebKit and NetworkManager dependencies. On Debian/Ubuntu, see the [Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux).

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

## Current MVP scope

The action planner independently validates every model response. It rejects unrecognised parameters, confidence below 0.9, and a model-selected risk tier that does not match the tool.

Read-only tools that can run automatically on Linux are:

- file search inside the current user's home directory (up to 50 results, limited walk depth)
- system storage, memory, CPU load, and running applications
- large-file listing inside the current user's home directory
- NetworkManager Wi-Fi status through `nmcli`
- recent systemd service logs through `journalctl`

Settings changes, app launching, file deletion, package installation, and all other high-risk operations are not implemented. No low-risk tool accepts or executes a shell string from the model.

## Verification

Run the frontend check locally:

```bash
pnpm build
```

Each push to `main` also runs a Linux compilation check in GitHub Actions.
