# Linux Personal Assistant Architecture

## Product direction

The project is evolving from a command palette into a Linux-native personal assistant. It must feel immediate for routine tasks, be useful without an internet connection, and remain explicit about what it knows and what it changes.

The initial launch target is a direct Linux desktop installation: Ubuntu GNOME first, followed by Kali and other Debian-family desktops where the required adapters are available. A constrained VM profile will reuse the same codebase later.

## Core interaction loop

```text
voice or text input
        ↓
fast local router ──→ deterministic safe actions
        ↓                         ↓
local language model ──→ plan + clarification + response
        ↓
policy and confirmation gate
        ↓
Linux capability adapter
        ↓
spoken/text result + local audit event
```

Routine actions must not wait for a language model when the input is clear. The existing local parsers are the first version of this router.

## Profiles

### Full Linux

- Direct install with a modern Linux desktop.
- Local speech recognition, text-to-speech, wake/push-to-talk controls, full history and optional memory.
- Prefer a GPU-capable local model when available; allow a user-selected cloud provider later.

### Lite VM

- Same policy engine and UI, with smaller local models and deterministic intent parsing for common actions.
- Push-to-talk rather than a permanent wake-word listener.
- Minimal stored history and resource-aware timeouts.

## Privacy and memory

Memory is opt-in. It stores only explicit user-approved facts and preferences, such as a preferred browser or a project folder. Every memory entry must show its source, be editable, and be removable individually or with one **Delete all memory** action. Raw conversation text is not promoted to memory automatically.

Audit history remains local and records actions, confirmations, failures, and cancellations. It is separate from personal memory.

## Safety model

- Read-only tools can run automatically after validation.
- Launching apps and changing settings require a preview and explicit confirmation.
- Commands never execute a shell string supplied by the model.
- Unsupported requests receive a clear explanation and a safe alternative.
- Voice uses the same planner and confirmation gate as text.

## Delivery milestones

1. Assistant shell, runtime profile detection, local history and status.
2. Push-to-talk transcription and spoken replies.
3. Capability plugins: app/window control, files, settings, notifications, media, and system health.
4. Opt-in memory with review/delete controls.
5. Full Linux packaging and release.
6. Lite VM tuning and packaging.
