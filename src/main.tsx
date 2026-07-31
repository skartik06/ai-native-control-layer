import { FormEvent, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import "./styles.css";

const shortcut = "CommandOrControl+Space";
const isTauri = "__TAURI_INTERNALS__" in window;

type IntentResult = {
  clarification_needed: boolean;
  clarification_question: string | null;
};

type ProcessResult = {
  intent: IntentResult;
  execution: unknown | null;
  message: string;
  confirmation: ConfirmationPreview | null;
};

type ConfirmationPreview = {
  summary: string;
  expires_in_seconds: number;
};

type ToolExecution = {
  tool: string;
  summary: string;
  data: unknown;
};

type AuditEntry = {
  id: number;
  timestamp: string;
  action: string;
  outcome: string;
  summary: string;
};

type RuntimeProfile = {
  profile: string;
  total_memory_gb: number;
  cpu_cores: number;
  summary: string;
};

type MemoryEntry = {
  id: number;
  created_at: string;
  category: string;
  memory_key: string;
  value: string;
};

function readableError(error: unknown) {
  return String(error).replace(/^Error:\s*/, "");
}

function App() {
  const [input, setInput] = useState("");
  const [response, setResponse] = useState("");
  const [loading, setLoading] = useState(false);
  const [confirmation, setConfirmation] = useState<ConfirmationPreview | null>(null);
  const [history, setHistory] = useState<AuditEntry[] | null>(null);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [runtimeProfile, setRuntimeProfile] = useState<RuntimeProfile | null>(null);
  const [memory, setMemory] = useState<MemoryEntry[] | null>(null);
  const [memoryKey, setMemoryKey] = useState("");
  const [memoryValue, setMemoryValue] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!isTauri) return;
    const setupShortcut = async () => {
      const [{ getCurrentWindow }, { register }] = await Promise.all([
        import("@tauri-apps/api/window"), import("@tauri-apps/plugin-global-shortcut")
      ]);
      const appWindow = getCurrentWindow();
      await register(shortcut, async (event) => {
        if (event.state !== "Pressed") return;
        if (await appWindow.isVisible()) await appWindow.hide();
        else { await appWindow.show(); await appWindow.setFocus(); inputRef.current?.focus(); }
      });
    };
    void setupShortcut();
    return () => { void import("@tauri-apps/plugin-global-shortcut").then(({ unregister }) => unregister(shortcut)); };
  }, []);

  useEffect(() => {
    if (!isTauri) return;
    void import("@tauri-apps/api/core")
      .then(({ invoke }) => invoke<RuntimeProfile>("get_runtime_profile"))
      .then(setRuntimeProfile)
      .catch(() => setRuntimeProfile(null));
  }, []);

  useEffect(() => {
    const onEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && isTauri) void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => getCurrentWindow().hide());
    };
    window.addEventListener("keydown", onEscape);
    return () => window.removeEventListener("keydown", onEscape);
  }, []);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const request = input.trim();
    if (!request || confirmation) return;
    if (!isTauri) {
      setResponse("Intent parsing requires the native app plus a local Ollama model. Start it with `pnpm tauri dev` after running `ollama run qwen3:4b`.");
      return;
    }
    setLoading(true); setResponse("");
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const result = await invoke<ProcessResult>("process_request", { request });
      if (result.confirmation) {
        setConfirmation(result.confirmation);
        setResponse(result.message);
      } else {
        setResponse(result.intent.clarification_needed
          ? `Clarification needed: ${result.message}`
          : `${result.message}\n\n${JSON.stringify(result.execution, null, 2)}`);
      }
      setInput("");
    } catch (error) {
      setResponse(`Request could not be completed. ${readableError(error)}`);
    } finally { setLoading(false); }
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

  async function confirm() {
    if (!isTauri || !confirmation) return;
    setLoading(true);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const result = await invoke<ToolExecution>("confirm_pending_action");
      setResponse(`${result.summary}\n\n${JSON.stringify(result.data, null, 2)}`);
      setConfirmation(null);
    } catch (error) {
      setResponse(`Setting was not changed: ${String(error)}`);
      setConfirmation(null);
    } finally { setLoading(false); }
  }

  async function toggleHistory() {
    if (!isTauri) {
      setResponse("Audit history is available in the native desktop app.");
      return;
    }
    if (history) {
      setHistory(null);
      return;
    }
    setHistoryLoading(true);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setHistory(await invoke<AuditEntry[]>("get_audit_history", { limit: 20 }));
    } catch (error) {
      setResponse(`Could not load audit history: ${String(error)}`);
    } finally {
      setHistoryLoading(false);
    }
  }

  async function cancel() {
    if (!isTauri || !confirmation) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setResponse(await invoke<string>("cancel_pending_action"));
    } catch (error) {
      setResponse(`Could not cancel pending action: ${String(error)}`);
    } finally {
      setConfirmation(null);
    }
  }

  async function toggleMemory() {
    if (!isTauri) return;
    if (memory) { setMemory(null); return; }
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      setMemory(await invoke<MemoryEntry[]>("get_memory"));
    } catch (error) { setResponse(`Could not load memory. ${readableError(error)}`); }
  }

  async function saveMemory(event: FormEvent) {
    event.preventDefault();
    if (!isTauri || !memoryKey.trim() || !memoryValue.trim()) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const entry = await invoke<MemoryEntry>("remember_preference", { category: "preference", memoryKey, value: memoryValue });
      setMemory((current) => current ? [entry, ...current] : [entry]);
      setMemoryKey(""); setMemoryValue("");
    } catch (error) { setResponse(`Could not save memory. ${readableError(error)}`); }
  }

  async function removeMemory(id: number) {
    if (!isTauri) return;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("forget_memory", { id });
    setMemory((current) => current?.filter((entry) => entry.id !== id) ?? null);
  }

  async function clearMemory() {
    if (!isTauri) return;
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("delete_all_memory");
    setMemory([]);
  }

  return <main className="overlay-shell">
    <section className="command-palette" aria-label="AI Native Control Layer">
      <div className="brand-row"><span className="status-dot" aria-hidden="true" /><span>LINUX ASSISTANT</span><span className="profile-badge">{runtimeProfile ? `${runtimeProfile.profile.toUpperCase()} PROFILE · ALPHA` : "CHECKING PROFILE"}</span><button className="history-toggle" type="button" onClick={toggleMemory}>Memory</button><button className="history-toggle" type="button" onClick={toggleHistory} disabled={historyLoading}>{historyLoading ? "Loading..." : history ? "Close history" : "History"}</button>{loading && <button className="stop-button" type="button" onClick={stopCurrentRequest}>Stop</button>}<kbd>Ctrl Space</kbd></div>
      <section className="assistant-intro">
        <p className="eyebrow">PRIVATE · LOCAL-FIRST · SAFETY-GATED</p>
        <h1>What can I help you do?</h1>
        <p>Control your Linux desktop, inspect your system, and safely launch supported apps. Voice and opt-in personal memory are next.</p>
        {runtimeProfile && <p className="runtime-summary">{runtimeProfile.summary} Detected: {runtimeProfile.total_memory_gb} GB RAM · {runtimeProfile.cpu_cores} CPU cores.</p>}
        <div className="capability-row"><span>System health</span><span>Files</span><span>Apps</span><span>Settings</span><span>Audit trail</span></div>
      </section>
      <form onSubmit={submit}>
        <input ref={inputRef} autoFocus value={input} onChange={(event) => setInput(event.target.value)} disabled={loading || Boolean(confirmation)}
          placeholder="Try: open file manager, show Wi-Fi status, or turn on dark mode" aria-label="Command input" />
      </form>
      {confirmation && <section className="confirmation" aria-label="Confirm setting change">
        <p>Preview: {confirmation.summary}</p>
        <small>This expires in {confirmation.expires_in_seconds} seconds.</small>
        <div className="confirmation-actions">
          <button type="button" onClick={cancel} disabled={loading}>Cancel</button>
          <button type="button" className="confirm" onClick={confirm} disabled={loading}>Confirm change</button>
        </div>
      </section>}
      {response && <pre className="response" role="status">{response}</pre>}
      {history && <section className="audit-history" aria-label="Local audit history">
        <div className="audit-heading"><strong>Local audit history</strong><span>Latest {history.length} events</span></div>
        {history.length === 0 ? <p className="empty-history">No events recorded yet.</p> : <ul>{history.map((entry) => <li key={entry.id}>
          <div><span className={`outcome outcome-${entry.outcome}`}>{entry.outcome.replaceAll("_", " ")}</span><code>{entry.action}</code></div>
          <p>{entry.summary}</p><time>{new Date(entry.timestamp).toLocaleString()}</time>
        </li>)}</ul>}
      </section>}
      {memory && <section className="memory-panel" aria-label="Opt-in assistant memory">
        <div className="audit-heading"><strong>Private assistant memory</strong><button type="button" onClick={clearMemory}>Delete all</button></div>
        <p>Only save details you explicitly choose. Nothing is remembered automatically.</p>
        <form className="memory-form" onSubmit={saveMemory}><input value={memoryKey} onChange={(event) => setMemoryKey(event.target.value)} placeholder="Preference, e.g. preferred browser" /><input value={memoryValue} onChange={(event) => setMemoryValue(event.target.value)} placeholder="Value, e.g. Firefox" /><button type="submit">Save memory</button></form>
        {memory.length === 0 ? <p className="empty-history">No saved memories.</p> : <ul>{memory.map((entry) => <li key={entry.id}><div><code>{entry.memory_key}</code><button type="button" onClick={() => void removeMemory(entry.id)}>Forget</button></div><p>{entry.value}</p></li>)}</ul>}
      </section>}
      <p className="hint">{loading ? "Working — use Stop to cancel a waiting Ollama request." : confirmation ? "Nothing changes until you confirm." : "Read-only checks run automatically. App launches and settings need confirmation."}</p>
    </section>
  </main>;
}

createRoot(document.getElementById("root")!).render(<App />);
