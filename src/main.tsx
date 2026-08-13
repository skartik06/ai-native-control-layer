import { FormEvent, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

const SHORTCUT = "CommandOrControl+Space";
const isTauri = "__TAURI_INTERNALS__" in window;

// ── Types ──────────────────────────────────────────────────────────────────────

type IntentResult = { clarification_needed: boolean; clarification_question: string | null };
type ConfirmationPreview = { summary: string; expires_in_seconds: number };
type ProcessResult = { intent: IntentResult; execution: unknown | null; message: string; confirmation: ConfirmationPreview | null };
type ToolExecution = { tool: string; summary: string; data: unknown };
type AuditEntry = { id: number; timestamp: string; action: string; outcome: string; summary: string };
type RuntimeProfile = { profile: string; total_memory_gb: number; cpu_cores: number; summary: string };
type MemoryEntry = { id: number; created_at: string; category: string; memory_key: string; value: string };
type VoiceStatus = { text_to_speech_available: boolean; speech_to_text_available: boolean; summary: string };
type ChatEntry = { id: number; created_at: string; role: string; content: string };
type ReminderEntry = { id: number; created_at: string; due_at: string; message: string; completed: boolean };
type ParsedReminder = { due_at: string; message: string };
type WakeWordStatus = { enabled: boolean; summary: string };

function readableError(error: unknown) {
  return String(error).replace(/^Error:\s*/, "").replace(/^tauri::\S+\s*/, "");
}

function formatDue(isoString: string): string {
  try { return new Date(isoString).toLocaleString(); } catch { return isoString; }
}

// ── Main App ───────────────────────────────────────────────────────────────────

function App() {
  // Core
  const [input, setInput] = useState("");
  const [response, setResponse] = useState("");
  const [loading, setLoading] = useState(false);
  const [confirmation, setConfirmation] = useState<ConfirmationPreview | null>(null);
  const [mode, setMode] = useState<"control" | "chat">("control");
  const inputRef = useRef<HTMLInputElement>(null);

  // Profile & voice
  const [runtimeProfile, setRuntimeProfile] = useState<RuntimeProfile | null>(null);
  const [voiceStatus, setVoiceStatus] = useState<VoiceStatus | null>(null);
  const [wakeWordStatus, setWakeWordStatus] = useState<WakeWordStatus | null>(null);
  const [installedModels, setInstalledModels] = useState<string[] | null>(null);

  // Panels visibility
  const [showHistory, setShowHistory] = useState(false);
  const [showChatHistory, setShowChatHistory] = useState(false);
  const [showMemory, setShowMemory] = useState(false);
  const [showReminders, setShowReminders] = useState(false);
  const [showNetworks, setShowNetworks] = useState(false);

  // Audit history
  const [history, setHistory] = useState<AuditEntry[] | null>(null);
  const [historyLoading, setHistoryLoading] = useState(false);

  // Chat history
  const [chatHistory, setChatHistory] = useState<ChatEntry[] | null>(null);

  // Memory
  const [memory, setMemory] = useState<MemoryEntry[] | null>(null);
  const [memoryKey, setMemoryKey] = useState("");
  const [memoryValue, setMemoryValue] = useState("");

  // Reminders
  const [reminders, setReminders] = useState<ReminderEntry[] | null>(null);
  const [reminderMsg, setReminderMsg] = useState("");
  const [reminderDue, setReminderDue] = useState("");
  const [nlReminderText, setNlReminderText] = useState("");
  const [nlParsed, setNlParsed] = useState<ParsedReminder | null>(null);
  const [nlError, setNlError] = useState("");
  const [reminderLoading, setReminderLoading] = useState(false);

  // Networks
  const [savedNetworks, setSavedNetworks] = useState<string[] | null>(null);
  const [networksLoading, setNetworksLoading] = useState(false);

  // Push-to-talk
  const [pttActive, setPttActive] = useState(false);
  const [pttLoading, setPttLoading] = useState(false);
  const pttRef = useRef<MediaRecorder | null>(null);
  const pttChunksRef = useRef<Blob[]>([]);

  // Auto-speak
  const [autoSpeak, setAutoSpeak] = useState<boolean>(() => {
    try { return localStorage.getItem("sk_auto_speak") === "true"; } catch { return false; }
  });
  const [isSpeaking, setIsSpeaking] = useState(false);

  function toggleAutoSpeak() {
    setAutoSpeak((prev) => {
      const next = !prev;
      try { localStorage.setItem("sk_auto_speak", String(next)); } catch { /**/ }
      return next;
    });
  }

  // SK Voice Daemon
  const [daemonRunning, setDaemonRunning] = useState(false);
  const [daemonStatus, setDaemonStatus] = useState("Voice daemon off");
  const [serviceInstalled, setServiceInstalled] = useState(false);
  const [serviceActive, setServiceActive] = useState(false);

  // ── Bootstrap ──────────────────────────────────────────────────────────────

  useEffect(() => {
    if (!isTauri) return;
    const setup = async () => {
      const [{ getCurrentWindow }, { register }] = await Promise.all([
        import("@tauri-apps/api/window"),
        import("@tauri-apps/plugin-global-shortcut"),
      ]);
      const win = getCurrentWindow();
      await register(SHORTCUT, async (e) => {
        if (e.state !== "Pressed") return;
        if (await win.isVisible()) await win.hide();
        else { await win.show(); await win.setFocus(); inputRef.current?.focus(); }
      });
    };
    void setup();
    return () => {
      void import("@tauri-apps/plugin-global-shortcut").then(({ unregister }) => unregister(SHORTCUT));
    };
  }, []);

  useEffect(() => {
    if (!isTauri) return;
    import("@tauri-apps/api/core").then(({ invoke }) => {
      void invoke<RuntimeProfile>("get_runtime_profile").then(setRuntimeProfile).catch(() => {});
      void invoke<VoiceStatus>("get_voice_status").then(setVoiceStatus).catch(() => {});
      void invoke<WakeWordStatus>("get_wake_word_status").then(setWakeWordStatus).catch(() => {});
      void invoke<string[]>("get_installed_models").then(setInstalledModels).catch(() => {});
      void invoke<{ installed: boolean; active: boolean; enabled: boolean }>("get_sk_service_status")
        .then((s) => {
          setServiceInstalled(s.installed);
          setServiceActive(s.active);
          if (s.active) {
            setDaemonRunning(true);
            setDaemonStatus("SK voice daemon running (boot service)");
          }
        })
        .catch(() => {});
    });
  }, []);

  useEffect(() => {
    const onEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape" && isTauri) {
        void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => getCurrentWindow().hide());
      }
    };
    window.addEventListener("keydown", onEscape);
    return () => window.removeEventListener("keydown", onEscape);
  }, []);

  // Reminder delivery check every 60 s
  useEffect(() => {
    if (!isTauri) return;
    const check = () =>
      import("@tauri-apps/api/core").then(({ invoke }) => invoke<string[]>("deliver_due_reminders"));
    void check();
    const t = window.setInterval(() => void check(), 60_000);
    return () => window.clearInterval(t);
  }, []);

  // SK Daemon events
  useEffect(() => {
    if (!isTauri) return;
    let unlisten1: (() => void) | undefined;
    let unlisten2: (() => void) | undefined;
    import("@tauri-apps/api/event").then(({ listen }) => {
      // Daemon status events
      listen<{ type: string; text: string }>("sk://daemon", (event) => {
        const { type, text } = event.payload;
        setDaemonStatus(text);
        if (type === "stopped") setDaemonRunning(false);
        if (type === "starting" || type === "ready" || type === "wake" ||
            type === "recording" || type === "processing" || type === "processing_command") {
          setDaemonRunning(true);
        }
      }).then((fn) => { unlisten1 = fn; });

      // Voice commands — auto-submit as if user typed them
      listen<{ text: string }>("sk://voice-command", (event) => {
        const text = event.payload.text.trim();
        if (!text) return;
        setInput(text);
        setDaemonStatus(`SK heard: "${text}"`);
        // Auto-submit after a short delay so React re-renders the input
        setTimeout(() => {
          setInput("");
          setLoading(true);
          setResponse("");
          import("@tauri-apps/api/core").then(async ({ invoke }) => {
            try {
              const result = await invoke<ProcessResult>("process_request", { request: text });
              if (result.confirmation) {
                setConfirmation(result.confirmation);
                setResponse(result.message);
                void autoSpeakText(result.message);
              } else {
                const msg = result.intent.clarification_needed
                  ? `Clarification needed: ${result.message}`
                  : `${result.message}\n\n${JSON.stringify(result.execution, null, 2)}`;
                setResponse(msg);
                void autoSpeakText(result.message);
              }
            } catch (err) {
              setResponse(`Voice command error: ${String(err)}`);
            } finally {
              setLoading(false);
            }
          });
        }, 100);
      }).then((fn) => { unlisten2 = fn; });
    });
    return () => { unlisten1?.(); unlisten2?.(); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoSpeak, voiceStatus]);

  async function toggleDaemon() {
    if (!isTauri) return;
    const { invoke } = await import("@tauri-apps/api/core");
    try {
      if (daemonRunning) {
        await invoke<string>("stop_sk_daemon");
        setDaemonRunning(false);
        setDaemonStatus("Voice daemon stopped");
      } else {
        await invoke<string>("start_sk_daemon");
        setDaemonRunning(true);
        setDaemonStatus("SK daemon starting…");
      }
    } catch (err) {
      setDaemonStatus(`Daemon error: ${String(err)}`);
    }
  }

  async function toggleService() {
    if (!isTauri) return;
    const { invoke } = await import("@tauri-apps/api/core");
    try {
      if (serviceInstalled) {
        await invoke<string>("uninstall_sk_service");
        setServiceInstalled(false);
        setServiceActive(false);
        setDaemonRunning(false);
        setDaemonStatus("Boot service removed");
      } else {
        const msg = await invoke<string>("install_sk_service");
        setServiceInstalled(true);
        setServiceActive(true);
        setDaemonRunning(true);
        setDaemonStatus(msg.split("\n")[0]);
      }
    } catch (err) {
      setDaemonStatus(`Service error: ${String(err)}`);
    }
  }

  // ── Command submit ─────────────────────────────────────────────────────────

  async function submit(event: FormEvent) {
    event.preventDefault();
    const request = input.trim();
    if (!request || confirmation) return;
    if (!isTauri) {
      setResponse("Intent parsing requires the native app plus a local Ollama model. Start with `pnpm tauri dev` after running `ollama run qwen3:4b`.");
      return;
    }
    setLoading(true);
    setResponse("");
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      if (mode === "chat") {
        const reply = await invoke<string>("chat_with_assistant", { message: request });
        setResponse(reply);
        setInput("");
        void autoSpeakText(reply);
        return;
      }
      const result = await invoke<ProcessResult>("process_request", { request });
      if (result.confirmation) {
        setConfirmation(result.confirmation);
        setResponse(result.message);
        void autoSpeakText(result.message);
      } else {
        const msg = result.intent.clarification_needed
          ? `Clarification needed: ${result.message}`
          : `${result.message}\n\n${JSON.stringify(result.execution, null, 2)}`;
        setResponse(msg);
        void autoSpeakText(result.message);
      }
      setInput("");
    } catch (error) {
      setResponse(`Request could not be completed. ${readableError(error)}`);
    } finally {
      setLoading(false);
    }
  }

  async function stopCurrentRequest() {
    if (!isTauri || !loading) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setResponse(await invoke<string>("stop_request"));
    } catch (error) {
      setResponse(`Could not stop the request. ${readableError(error)}`);
    }
  }

  async function speakLatestResponse() {
    if (!isTauri || !response) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setIsSpeaking(true);
      await invoke<string>("speak_text", { text: response });
    } catch (error) {
      setResponse((r) => `${r}\n\nVoice unavailable: ${readableError(error)}`);
    } finally {
      setIsSpeaking(false);
    }
  }

  async function autoSpeakText(text: string) {
    if (!autoSpeak || !isTauri || !voiceStatus?.text_to_speech_available) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setIsSpeaking(true);
      await invoke<string>("speak_text", { text });
    } catch { /* silent — auto-speak is best-effort */ }
    finally { setIsSpeaking(false); }
  }

  async function confirmAction() {
    if (!isTauri || !confirmation) return;
    setLoading(true);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const result = await invoke<ToolExecution>("confirm_pending_action");
      const msg = `${result.summary}\n\n${JSON.stringify(result.data, null, 2)}`;
      setResponse(msg);
      setConfirmation(null);
      void autoSpeakText(result.summary);
    } catch (error) {
      setResponse(`Action not performed: ${String(error)}`);
      setConfirmation(null);
    } finally {
      setLoading(false);
    }
  }

  async function cancelAction() {
    if (!isTauri || !confirmation) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setResponse(await invoke<string>("cancel_pending_action"));
    } catch (error) {
      setResponse(`Could not cancel: ${String(error)}`);
    } finally {
      setConfirmation(null);
    }
  }

  // ── Push-to-talk ───────────────────────────────────────────────────────────

  async function startPTT() {
    if (!isTauri || !voiceStatus?.speech_to_text_available) return;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const recorder = new MediaRecorder(stream, { mimeType: "audio/webm" });
      pttChunksRef.current = [];
      recorder.ondataavailable = (e) => { if (e.data.size > 0) pttChunksRef.current.push(e.data); };
      recorder.onstop = async () => {
        stream.getTracks().forEach((t) => t.stop());
        const blob = new Blob(pttChunksRef.current, { type: "audio/webm" });
        const buf = await blob.arrayBuffer();
        const data = Array.from(new Uint8Array(buf));
        setPttLoading(true);
        try {
          const { invoke } = await import("@tauri-apps/api/core");
          const audioPath = await invoke<string>("save_temp_audio", { data });
          const text = await invoke<string>("transcribe_audio", { audioPath });
          setInput(text.trim());
          inputRef.current?.focus();
        } catch (error) {
          setResponse(`Voice transcription failed: ${readableError(error)}`);
        } finally {
          setPttLoading(false);
          setPttActive(false);
        }
      };
      recorder.start();
      pttRef.current = recorder;
      setPttActive(true);
    } catch (error) {
      setResponse(`Microphone access failed: ${readableError(error)}`);
    }
  }

  function stopPTT() {
    if (pttRef.current && pttRef.current.state !== "inactive") {
      pttRef.current.stop();
    }
  }

  // ── History ────────────────────────────────────────────────────────────────

  async function toggleHistory() {
    if (!isTauri) { setResponse("Audit history is available in the native desktop app."); return; }
    if (showHistory) { setShowHistory(false); setHistory(null); return; }
    setHistoryLoading(true);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setHistory(await invoke<AuditEntry[]>("get_audit_history", { limit: 20 }));
      setShowHistory(true);
    } catch (error) {
      setResponse(`Could not load audit history: ${readableError(error)}`);
    } finally {
      setHistoryLoading(false);
    }
  }

  async function toggleChatHistory() {
    if (!isTauri) return;
    if (showChatHistory) { setShowChatHistory(false); setChatHistory(null); return; }
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setChatHistory(await invoke<ChatEntry[]>("get_chat_history", { limit: 50 }));
      setShowChatHistory(true);
    } catch (error) {
      setResponse(`Could not load chat history. ${readableError(error)}`);
    }
  }

  async function clearChatHistory() {
    if (!isTauri) return;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("delete_chat_history");
    setChatHistory([]);
  }

  // ── Memory ─────────────────────────────────────────────────────────────────

  async function toggleMemory() {
    if (!isTauri) return;
    if (showMemory) { setShowMemory(false); setMemory(null); return; }
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setMemory(await invoke<MemoryEntry[]>("get_memory"));
      setShowMemory(true);
    } catch (error) {
      setResponse(`Could not load memory. ${readableError(error)}`);
    }
  }

  async function saveMemory(event: FormEvent) {
    event.preventDefault();
    if (!isTauri || !memoryKey.trim() || !memoryValue.trim()) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const entry = await invoke<MemoryEntry>("remember_preference", {
        category: "preference", memoryKey, value: memoryValue,
      });
      setMemory((cur) => (cur ? [entry, ...cur] : [entry]));
      setMemoryKey(""); setMemoryValue("");
    } catch (error) {
      setResponse(`Could not save memory. ${readableError(error)}`);
    }
  }

  async function removeMemory(id: number) {
    if (!isTauri) return;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("forget_memory", { id });
    setMemory((cur) => cur?.filter((e) => e.id !== id) ?? null);
  }

  async function clearMemory() {
    if (!isTauri) return;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("delete_all_memory");
    setMemory([]);
  }

  // ── Reminders ──────────────────────────────────────────────────────────────

  async function toggleReminders() {
    if (!isTauri) return;
    if (showReminders) { setShowReminders(false); setReminders(null); return; }
    setReminderLoading(true);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setReminders(await invoke<ReminderEntry[]>("get_reminders"));
      setShowReminders(true);
    } catch (error) {
      setResponse(`Could not load reminders. ${readableError(error)}`);
    } finally {
      setReminderLoading(false);
    }
  }

  async function parseNlReminder() {
    if (!isTauri || !nlReminderText.trim()) return;
    setNlError("");
    setNlParsed(null);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const parsed = await invoke<ParsedReminder>("parse_reminder_text", { text: nlReminderText });
      setNlParsed(parsed);
      setReminderDue(parsed.due_at.slice(0, 16)); // datetime-local format
      setReminderMsg(parsed.message);
    } catch (error) {
      setNlError(readableError(error));
    }
  }

  async function addReminder(event: FormEvent) {
    event.preventDefault();
    if (!isTauri || !reminderMsg.trim() || !reminderDue) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      // Convert datetime-local to RFC3339
      const dueAt = new Date(reminderDue).toISOString();
      const entry = await invoke<ReminderEntry>("create_reminder", { dueAt, message: reminderMsg.trim() });
      setReminders((cur) => (cur ? [entry, ...cur] : [entry]));
      setReminderMsg(""); setReminderDue(""); setNlParsed(null); setNlReminderText("");
    } catch (error) {
      setResponse(`Could not save reminder. ${readableError(error)}`);
    }
  }

  async function completeReminder(id: number) {
    if (!isTauri) return;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("complete_reminder", { id });
    setReminders((cur) => cur?.map((r) => r.id === id ? { ...r, completed: true } : r) ?? null);
  }

  async function deleteReminder(id: number) {
    if (!isTauri) return;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("delete_reminder", { id });
    setReminders((cur) => cur?.filter((r) => r.id !== id) ?? null);
  }

  // ── Networks ───────────────────────────────────────────────────────────────

  async function toggleNetworks() {
    if (!isTauri) return;
    if (showNetworks) { setShowNetworks(false); setSavedNetworks(null); return; }
    setNetworksLoading(true);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setSavedNetworks(await invoke<string[]>("get_saved_networks"));
      setShowNetworks(true);
    } catch (error) {
      setResponse(`Could not load saved networks. ${readableError(error)}`);
    } finally {
      setNetworksLoading(false);
    }
  }

  async function connectToNetwork(name: string) {
    if (!isTauri) return;
    setResponse(`Requesting connection to ${name}…`);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const result = await invoke<ProcessResult>("process_request", {
        request: `connect to wifi ${name}`,
      });
      if (result.confirmation) {
        setConfirmation(result.confirmation);
        setResponse(result.message);
      } else {
        setResponse(result.message);
      }
    } catch (error) {
      setResponse(`Wi-Fi connect failed: ${readableError(error)}`);
    }
  }

  async function disconnectFromNetwork(name: string) {
    if (!isTauri) return;
    setResponse(`Requesting disconnect from ${name}…`);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const result = await invoke<ProcessResult>("process_request", {
        request: `disconnect from wifi ${name}`,
      });
      if (result.confirmation) {
        setConfirmation(result.confirmation);
        setResponse(result.message);
      } else {
        setResponse(result.message);
      }
    } catch (error) {
      setResponse(`Wi-Fi disconnect failed: ${readableError(error)}`);
    }
  }

  // ── Render helpers ─────────────────────────────────────────────────────────

  const pttAvailable = isTauri && voiceStatus?.speech_to_text_available;
  const pttLabel = pttLoading ? "…" : pttActive ? "⏹ Stop" : "🎙 Speak";

  const modelLabel = installedModels
    ? installedModels.length > 0 ? installedModels[0] : "No model"
    : "Ollama?";

  // ── JSX ────────────────────────────────────────────────────────────────────

  return (
    <main className="overlay-shell">
      <section className="command-palette" aria-label="SK — AI Assistant">

        {/* ── Top bar ── */}
        <div className="brand-row">
          <span className="status-dot" aria-hidden="true" />
          <span className="brand-name">SK</span>
          <span className="profile-badge">
            {runtimeProfile ? `${runtimeProfile.profile.toUpperCase()} · ${modelLabel}` : "Initialising…"}
          </span>
          <div className="brand-actions">
            <button className={`panel-toggle ${showMemory ? "active" : ""}`} type="button" onClick={toggleMemory}>Memory</button>
            <button className={`panel-toggle ${showReminders ? "active" : ""}`} type="button" onClick={toggleReminders} disabled={reminderLoading}>Reminders</button>
            <button className={`panel-toggle ${showNetworks ? "active" : ""}`} type="button" onClick={toggleNetworks} disabled={networksLoading}>Networks</button>
            <button className={`panel-toggle ${showHistory ? "active" : ""}`} type="button" onClick={toggleHistory} disabled={historyLoading}>History</button>
            {loading && <button className="stop-button" type="button" onClick={stopCurrentRequest}>⏹ Stop</button>}
            <kbd title="Toggle window">Ctrl Space</kbd>
          </div>
        </div>

        {/* ── Intro ── */}
        <section className="assistant-intro">
          <p className="eyebrow">PRIVATE · LOCAL-FIRST · SAFETY-GATED</p>
          <h1>Hey, what can I do for you?</h1>
          {runtimeProfile && (
            <p className="runtime-summary">
              {runtimeProfile.summary} — {runtimeProfile.total_memory_gb} GB RAM · {runtimeProfile.cpu_cores} CPU cores
            </p>
          )}

          {/* Mode tabs */}
          <div className="mode-tabs">
            <button
              id="mode-control"
              className={`mode-tab ${mode === "control" ? "mode-tab-active" : ""}`}
              type="button"
              onClick={() => setMode("control")}
            >
              🖥 Control
            </button>
            <button
              id="mode-chat"
              className={`mode-tab ${mode === "chat" ? "mode-tab-active" : ""}`}
              type="button"
              onClick={() => setMode("chat")}
            >
              💬 Chat
            </button>
            <button className="mode-tab" type="button" onClick={toggleChatHistory}>
              {showChatHistory ? "Close chats" : "Chats"}
            </button>
          </div>

          {/* Capability row */}
          <div className="capability-row">
            <span className={`cap-badge ${voiceStatus?.text_to_speech_available ? "cap-ok" : "cap-warn"}`}
              title={voiceStatus?.summary}>
              {isSpeaking ? "🔊 SK is speaking…" : voiceStatus?.text_to_speech_available ? "🔊 TTS ready" : "🔇 TTS setup needed"}
            </span>
            <span className={`cap-badge ${voiceStatus?.speech_to_text_available ? "cap-ok" : "cap-warn"}`}
              title="Install whisper.cpp for voice input">
              {voiceStatus?.speech_to_text_available ? "🎙 STT ready" : "🎙 STT setup needed"}
            </span>
            <span className={`cap-badge ${wakeWordStatus?.enabled ? "cap-warn" : "cap-ok"}`}
              title={wakeWordStatus?.summary}>
              {wakeWordStatus?.enabled ? "👂 Wake-word ON" : "Wake-word off"}
            </span>
            <span className="cap-badge cap-ok" title="System health, files, and read-only queries">📊 System · Files</span>
            <span className="cap-badge cap-ok" title="Media controls via playerctl">🎵 Media</span>
            <span className="cap-badge cap-ok" title="Dark mode, brightness, volume, Wi-Fi">⚙ Settings</span>
            {/* Auto-speak toggle */}
            <button
              id="auto-speak-toggle"
              className={`cap-badge cap-toggle ${autoSpeak ? "cap-active" : ""}`}
              type="button"
              onClick={toggleAutoSpeak}
              title={autoSpeak ? "SK speaks automatically — click to turn off" : "Click to make SK speak every response automatically"}
              disabled={!voiceStatus?.text_to_speech_available}
            >
              {autoSpeak ? "🔈 Auto-speak ON" : "🔈 Auto-speak OFF"}
            </button>
            {/* Voice Daemon toggle */}
            <button
              id="voice-daemon-toggle"
              className={`cap-badge cap-toggle ${daemonRunning && !serviceActive ? "cap-active" : ""}`}
              type="button"
              onClick={toggleDaemon}
              title={daemonRunning ? `${daemonStatus} — click to stop` : "Start SK voice daemon (say 'Hey SK' to activate)"}
            >
              {daemonRunning && !serviceActive ? "👂 SK listening" : "👂 Voice daemon"}
            </button>
            {/* Boot autostart toggle */}
            <button
              id="boot-service-toggle"
              className={`cap-badge cap-toggle ${serviceInstalled ? "cap-active" : ""}`}
              type="button"
              onClick={toggleService}
              title={serviceInstalled
                ? "SK starts at login (click to disable)"
                : "Enable SK to start automatically at login"}
            >
              {serviceInstalled ? "🚀 Boot: ON" : "🚀 Boot: OFF"}
            </button>
          </div>
          {/* Daemon / service status line */}
          {(daemonRunning || daemonStatus !== "Voice daemon off") && (
            <p className="daemon-status">
              {serviceActive ? "⚡ " : ""}
              {daemonStatus}
            </p>
          )}
        </section>

        {/* ── Input form ── */}
        <form className="input-form" onSubmit={submit}>
          <input
            ref={inputRef}
            autoFocus
            value={input}
            onChange={(e) => setInput(e.target.value)}
            disabled={loading || Boolean(confirmation) || pttLoading}
            placeholder={
              pttLoading ? "SK is transcribing…" :
              pttActive  ? "SK is listening…" :
              mode === "chat"
                ? "Chat with SK — ask anything"
                : "Tell SK what to do… copy text · connect wifi · remind me in 10 min to stretch"
            }
            aria-label="Command input"
          />
          {pttAvailable && (
            <button
              id="ptt-button"
              className={`ptt-button ${pttActive ? "ptt-active" : ""} ${pttLoading ? "ptt-loading" : ""}`}
              type="button"
              onClick={pttActive ? stopPTT : startPTT}
              disabled={pttLoading || Boolean(confirmation)}
              title={pttActive ? "Stop recording and transcribe" : "Hold to speak (requires whisper.cpp)"}
              aria-label={pttActive ? "Stop recording" : "Start voice input"}
            >
              {pttLabel}
            </button>
          )}
        </form>

        {/* ── Confirmation gate ── */}
        {confirmation && (
          <section className="confirmation" aria-label="Confirm action">
            <div className="confirmation-preview">
              <span className="confirmation-icon">⚠</span>
              <span>{confirmation.summary}</span>
            </div>
            <small>Expires in {confirmation.expires_in_seconds} s. Nothing changes until you confirm.</small>
            <div className="confirmation-actions">
              <button id="cancel-action" type="button" onClick={cancelAction} disabled={loading}>Cancel</button>
              <button id="confirm-action" type="button" className="confirm" onClick={confirmAction} disabled={loading}>
                {loading ? "Applying…" : "Confirm"}
              </button>
            </div>
          </section>
        )}

        {/* ── Response area ── */}
        {response && (
          <div className="response-wrap">
            <div className="response-tools">
              <button
                type="button"
                onClick={speakLatestResponse}
                disabled={!voiceStatus?.text_to_speech_available || isSpeaking}
                title={voiceStatus?.text_to_speech_available ? "Make SK speak this response" : "Install spd-say or espeak-ng"}
                className={isSpeaking ? "speaking-active" : ""}
              >
                {isSpeaking ? "🔊 Speaking…" : "🔊 Speak"}
              </button>
              <button
                type="button"
                onClick={toggleAutoSpeak}
                disabled={!voiceStatus?.text_to_speech_available}
                className={autoSpeak ? "speaking-active" : ""}
                title="Toggle auto-speak for all responses"
              >
                {autoSpeak ? "Auto ON" : "Auto OFF"}
              </button>
            </div>
            <pre className="response" role="status">{response}</pre>
          </div>
        )}

        {/* ── Hint ── */}
        <p className="hint">
          {loading
            ? "SK is working — press Stop to cancel."
            : pttActive
            ? "SK is listening… press ⏹ Stop when done speaking."
            : mode === "chat"
            ? "Chat mode — SK will never perform system actions here. Switch to Control for that."
            : confirmation
            ? "SK is waiting — nothing changes until you confirm."
            : "Read-only queries run instantly. Settings, clipboard and launches need your confirmation."}
        </p>

        {/* ── Panels ── */}

        {/* Chat history */}
        {showChatHistory && (
          <section className="panel" aria-label="Local chat history">
            <div className="panel-heading">
              <strong>Local chat history</strong>
              <button type="button" onClick={clearChatHistory}>Delete all</button>
            </div>
            {!chatHistory || chatHistory.length === 0
              ? <p className="empty-state">No saved chat messages.</p>
              : <ul className="chat-history-list">
                  {chatHistory.map((e) => (
                    <li key={e.id} className={`chat-entry chat-${e.role}`}>
                      <div className="chat-meta"><code>{e.role}</code><time>{new Date(e.created_at).toLocaleString()}</time></div>
                      <p>{e.content}</p>
                    </li>
                  ))}
                </ul>
            }
          </section>
        )}

        {/* Audit history */}
        {showHistory && (
          <section className="panel" aria-label="Local audit history">
            <div className="panel-heading">
              <strong>Audit history</strong>
              <span className="panel-meta">Latest {history?.length ?? 0} events</span>
            </div>
            {!history || history.length === 0
              ? <p className="empty-state">No events recorded yet.</p>
              : <ul className="audit-list">
                  {history.map((e) => (
                    <li key={e.id} className="audit-entry">
                      <div><span className={`outcome outcome-${e.outcome}`}>{e.outcome.replace(/_/g, " ")}</span><code>{e.action}</code></div>
                      <p>{e.summary}</p>
                      <time>{new Date(e.timestamp).toLocaleString()}</time>
                    </li>
                  ))}
                </ul>
            }
          </section>
        )}

        {/* Memory */}
        {showMemory && (
          <section className="panel" aria-label="Opt-in assistant memory">
            <div className="panel-heading">
              <strong>Private memory</strong>
              <button type="button" onClick={clearMemory}>Delete all</button>
            </div>
            <p className="panel-note">Only save details you explicitly choose. Nothing is remembered automatically.</p>
            <form className="memory-form" onSubmit={saveMemory}>
              <input value={memoryKey} onChange={(e) => setMemoryKey(e.target.value)} placeholder="Preference, e.g. preferred browser" />
              <input value={memoryValue} onChange={(e) => setMemoryValue(e.target.value)} placeholder="Value, e.g. Firefox" />
              <button type="submit">Save</button>
            </form>
            {!memory || memory.length === 0
              ? <p className="empty-state">No saved memories.</p>
              : <ul className="memory-list">
                  {memory.map((e) => (
                    <li key={e.id}>
                      <div><code>{e.memory_key}</code><button type="button" onClick={() => void removeMemory(e.id)}>Forget</button></div>
                      <p>{e.value}</p>
                    </li>
                  ))}
                </ul>
            }
          </section>
        )}

        {/* Reminders */}
        {showReminders && (
          <section className="panel" aria-label="Reminders">
            <div className="panel-heading">
              <strong>Reminders</strong>
              <span className="panel-meta panel-note-inline">Fires while app is running</span>
            </div>
            <p className="panel-note">Parse natural language or set a specific date/time.</p>

            {/* NL input */}
            <div className="nl-reminder-row">
              <input
                value={nlReminderText}
                onChange={(e) => { setNlReminderText(e.target.value); setNlParsed(null); setNlError(""); }}
                placeholder="e.g. remind me in 10 minutes to stretch"
              />
              <button type="button" onClick={parseNlReminder} disabled={!nlReminderText.trim()}>Parse</button>
            </div>
            {nlError && <p className="nl-error">{nlError}</p>}
            {nlParsed && (
              <p className="nl-parsed">
                ✓ Parsed: <strong>{nlParsed.message}</strong> at <strong>{formatDue(nlParsed.due_at)}</strong> — review below then click Save.
              </p>
            )}

            {/* Reminder form */}
            <form className="reminder-form" onSubmit={addReminder}>
              <input
                type="datetime-local"
                value={reminderDue}
                onChange={(e) => setReminderDue(e.target.value)}
                required
              />
              <input
                value={reminderMsg}
                onChange={(e) => setReminderMsg(e.target.value)}
                placeholder="Reminder message"
                required
              />
              <button type="submit">Save reminder</button>
            </form>

            {!reminders || reminders.length === 0
              ? <p className="empty-state">No reminders.</p>
              : <ul className="reminder-list">
                  {reminders.map((r) => (
                    <li key={r.id} className={`reminder-entry ${r.completed ? "reminder-done" : ""}`}>
                      <div className="reminder-meta">
                        <time>{formatDue(r.due_at)}</time>
                        <div className="reminder-actions">
                          {!r.completed && <button type="button" onClick={() => void completeReminder(r.id)}>Done</button>}
                          <button type="button" onClick={() => void deleteReminder(r.id)}>Delete</button>
                        </div>
                      </div>
                      <p className="reminder-message">{r.message}</p>
                    </li>
                  ))}
                </ul>
            }
          </section>
        )}

        {/* Networks */}
        {showNetworks && (
          <section className="panel" aria-label="Saved Wi-Fi networks">
            <div className="panel-heading">
              <strong>Saved Wi-Fi networks</strong>
              <button type="button" onClick={toggleNetworks}>Close</button>
            </div>
            <p className="panel-note">
              Connect/Disconnect goes through the confirmation gate. Uses <code>nmcli</code> with fixed arguments — no shell execution.
            </p>
            {!savedNetworks || savedNetworks.length === 0
              ? <p className="empty-state">No saved profiles found. Run <code>nmcli connection show</code> to check.</p>
              : <ul className="network-list">
                  {savedNetworks.map((name) => (
                    <li key={name} className="network-entry">
                      <span className="network-name">📶 {name}</span>
                      <div className="network-actions">
                        <button type="button" onClick={() => void connectToNetwork(name)}>Connect</button>
                        <button type="button" onClick={() => void disconnectFromNetwork(name)}>Disconnect</button>
                      </div>
                    </li>
                  ))}
                </ul>
            }
          </section>
        )}

      </section>
    </main>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
