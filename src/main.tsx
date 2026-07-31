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

  return <main className="overlay-shell">
    <section className="command-palette" aria-label="AI Native Control Layer">
      <div className="brand-row"><span className="status-dot" aria-hidden="true" /><span>CONTROL LAYER</span><button className="history-toggle" type="button" onClick={toggleHistory} disabled={historyLoading}>{historyLoading ? "Loading..." : history ? "Close history" : "History"}</button>{loading && <button className="stop-button" type="button" onClick={stopCurrentRequest}>Stop</button>}<kbd>Ctrl Space</kbd></div>
      <form onSubmit={submit}>
        <input ref={inputRef} autoFocus value={input} onChange={(event) => setInput(event.target.value)} disabled={loading || Boolean(confirmation)}
          placeholder="Ask your computer anything..." aria-label="Command input" />
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
      <p className="hint">{loading ? "Working..." : confirmation ? "Nothing changes until you confirm." : "Linux MVP · read-only tools run automatically; changes require confirmation."}</p>
    </section>
  </main>;
}

createRoot(document.getElementById("root")!).render(<App />);
