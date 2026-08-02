# Direct Ubuntu GNOME test checklist

This is the primary test environment for the Full Desktop profile.

## Install prerequisites

```bash
sudo apt update
sudo apt install -y \
  build-essential curl git \
  libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev \
  libxdo-dev libssl-dev patchelf \
  libnotify-bin playerctl \
  speech-dispatcher espeak-ng \
  wl-clipboard xclip \
  wmctrl \
  network-manager \
  whisper-cpp           # Ubuntu 24.10+; or build from source

curl -fsSL https://ollama.com/install.sh | sh
curl https://sh.rustup.rs -sSf | sh
```

Restart the terminal after Rust installs, then clone and run:

```bash
git clone https://github.com/skartik06/ai-native-control-layer.git
cd ai-native-control-layer
corepack enable
pnpm install
ollama pull qwen3:4b

# Download a whisper model for PTT (optional)
whisper-cli --download-model base.en

OLLAMA_MODEL=qwen3:4b pnpm tauri dev
```

---

## Acceptance checks

### A. Read-only / low-risk (no confirmation required)
1. `show my system information` → returns data immediately, no confirmation.
2. `what is on my clipboard` → shows clipboard content.
3. `find large files in my home folder` → returns list, no changes.
4. `show network status` → displays connection info.

### B. Settings & media (confirmation required)
5. `turn on dark mode` → preview shown; **Confirm** applies via `gsettings`.
6. `pause music` / `next song` → preview; **Confirm** uses `playerctl`.
7. `set volume to 60` → preview; **Confirm** applies.

### C. Clipboard write (new — confirmation required)
8. `copy hello world to clipboard` → preview "Copy 11 character(s)"; **Confirm** writes via `wl-copy` or `xclip`.
9. Verify by pressing Ctrl+V in any text editor.
10. `set clipboard to my email address` → same flow.

### D. Wi-Fi connect / disconnect (new — confirmation required)
11. Open **Networks** panel → saved NetworkManager profiles listed.
12. Press **Connect** on a known SSID → confirmation preview; **Confirm** runs `nmcli connection up id <name>`.
13. `disconnect from wifi HomeNetwork` → preview shown; **Confirm** runs `nmcli connection down id HomeNetwork`.
14. Shell-injection attempts (`; rm -rf /`) in the name field are rejected at parse time.

### E. Reminders (new)
15. Open **Reminders** panel.
16. Type `remind me in 2 minutes to test reminders` → **Parse** → due time prefilled; click **Save reminder**.
17. Wait ~2 min → desktop notification fires.
18. Mark done and verify ~~strikethrough~~ style; delete and verify removal.

### F. Voice / Push-to-talk (new)
19. Ensure `whisper-cli --help` works.
20. `get_voice_status` reports `speech_to_text_available = true` in the capability row.
21. Click **🎙 Speak** → button pulses red; speak a command; click **⏹ Stop**.
22. Transcribed text appears in input field; submit runs the command.

### G. App launch & notifications (existing)
23. `open firefox` → preview; **Confirm** launches.
24. `send notification "build complete"` → preview; **Confirm** sends via `notify-send`.

### H. Chat mode
25. Switch to **Chat** mode; ask "hi" and "what can you do?" → natural replies, no system actions.
26. **Stop** button cancels an in-flight Ollama request.

### I. UI
27. `Ctrl+Space` toggles window visibility.
28. `Escape` hides window.
29. **Memory** panel: save, use in chat, forget.
30. **Audit history**: shows all events with timestamps.

---

## Known boundaries

- Wake-word (always-on listening) is **off by default** and foundational only; set `WAKE_WORD_ENABLED=true` in the service file when fully implemented.
- This app never deletes files, installs packages, or executes model-generated shell commands.
- All `nmcli`, `gsettings`, `playerctl`, `wl-copy`, and `xclip` calls use fixed, whitelisted arguments — no shell pass-through.
