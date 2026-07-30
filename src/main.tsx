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
};

function App() {
  const [input, setInput] = useState("");
  const [response, setResponse] = useState("");
  const [loading, setLoading] = useState(false);
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
    if (!request) return;
    if (!isTauri) {
      setResponse("Intent parsing requires the native app plus a local Ollama model. Start it with `pnpm tauri dev` after running `ollama run qwen3:4b`.");
      return;
    }
    setLoading(true); setResponse("");
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const result = await invoke<ProcessResult>("process_request", { request });
      setResponse(result.intent.clarification_needed
        ? `Clarification needed: ${result.message}`
        : `${result.message}\n\n${JSON.stringify(result.execution, null, 2)}`);
      setInput("");
    } catch (error) {
      setResponse(`Could not parse intent: ${String(error)}`);
    } finally { setLoading(false); }
  }

  return <main className="overlay-shell">
    <section className="command-palette" aria-label="AI Native Control Layer">
      <div className="brand-row"><span className="status-dot" aria-hidden="true" /><span>CONTROL LAYER</span><kbd>Ctrl Space</kbd></div>
      <form onSubmit={submit}>
        <input ref={inputRef} autoFocus value={input} onChange={(event) => setInput(event.target.value)} disabled={loading}
          placeholder="Ask your computer anything..." aria-label="Command input" />
      </form>
      {response && <pre className="response" role="status">{response}</pre>}
      <p className="hint">{loading ? "Interpreting request..." : "Linux MVP · read-only tools run automatically; changes require confirmation."}</p>
    </section>
  </main>;
}

createRoot(document.getElementById("root")!).render(<App />);
