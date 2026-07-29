# AI-Native System Control Layer

Step 2: a Tauri overlay with a `Ctrl+Space` global toggle and local Ollama-backed structured intent parsing. It intentionally has no OS tools or action execution yet.

## Linux prerequisites

Install a current Rust toolchain, Node.js 20+, and your distro's Tauri/WebKit build dependencies. On Debian/Ubuntu, see the [Tauri Linux prerequisites](https://v2.tauri.app/start/prerequisites/#linux).

## Run

```bash
npm install
npm run tauri dev
```

`Ctrl+Space` toggles the overlay. Some Linux desktop environments reserve that shortcut; if registration fails, change `shortcut` in `src/main.tsx` to an unused accelerator.

## Free local AI setup (Step 2)

Install [Ollama for Windows](https://ollama.com/download/windows), then download the free local model once:

PowerShell:

```powershell
ollama run qwen3:4b
```

After its first response, stop it with `Ctrl+C`; Ollama keeps the model available locally. Then run the app:

```powershell
pnpm tauri dev
```

`qwen3:4b` is a ~2.5 GB download. Optionally set `OLLAMA_MODEL` to another locally downloaded model. The backend writes raw input and model output to its local app-data debug log as required for this development step; the log is not uploaded anywhere by the app.
