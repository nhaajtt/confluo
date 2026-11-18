import { createSignal, onCleanup, onMount } from "solid-js";
import { api, type Settings } from "./api";
import { parseLinks, parseRate, rate } from "./format";
import { items, setTheme, settings, theme } from "./store";

export const [setupOpen, setSetupOpen] = createSignal(false);
const [message, setMessage] = createSignal<{ text: string; bad: boolean } | null>(null);

export function say(text: string, bad = false) {
  setMessage({ text, bad });
}

async function addLinks(text: string): Promise<boolean> {
  const groups = parseLinks(text);
  if (!groups.length) return false;
  let added = 0;
  for (const urls of groups) {
    try {
      await api.add({ urls });
      added++;
    } catch (e) {
      say(String(e), true);
    }
  }
  if (added) say(added === 1 ? "Added 1 download." : `Added ${added} downloads.`);
  return true;
}

export async function saveSettings(change: Partial<Settings>) {
  const current = settings();
  if (!current) return;
  try {
    await api.setSettings({ ...current, ...change });
  } catch (e) {
    say(String(e), true);
  }
}

const HELP =
  "Paste a link and press Enter. Mirrors: link | mirror. Commands: pause all · resume all · clear · limit 2m · limit off · theme · setup";

async function runCommand(raw: string) {
  const text = raw.trim();
  if (!text) return;
  if (await addLinks(text)) return;
  const [cmd, ...rest] = text.toLowerCase().split(/\s+/);
  const arg = rest.join(" ");
  switch (cmd) {
    case "help":
    case "?":
      say(HELP);
      break;
    case "pause":
      for (const it of items) if (it.status === "running" || it.status === "queued") void api.pause(it.id);
      say("Paused everything.");
      break;
    case "resume":
    case "start":
      for (const it of items) if (it.status === "paused" || it.status === "failed") void api.resume(it.id);
      say("Resumed everything.");
      break;
    case "clear":
      await api.clearCompleted();
      say("Cleared finished downloads from the list. The files stay.");
      break;
    case "limit": {
      const r = parseRate(arg);
      if (r == null) say("Use: limit 500k, limit 2m, or limit off.", true);
      else {
        await saveSettings({ speed_limit: r });
        say(r ? `Speed limit: ${rate(r)}.` : "Speed limit off.");
      }
      break;
    }
    case "theme":
      setTheme(arg === "light" || arg === "dark" ? arg : theme() === "dark" ? "light" : "dark");
      break;
    case "setup":
    case "settings":
      setSetupOpen(true);
      break;
    default:
      say(`Not a link or a command: “${text}”. Type help.`, true);
  }
}

export function CommandBar() {
  let input: HTMLInputElement | undefined;
  const onKey = (e: KeyboardEvent) => {
    const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement;
    if ((e.key === "/" && !typing) || (e.key.toLowerCase() === "l" && e.ctrlKey)) {
      e.preventDefault();
      input?.focus();
    }
  };
  const onPaste = (e: ClipboardEvent) => {
    if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
    const text = e.clipboardData?.getData("text") ?? "";
    if (parseLinks(text).length) {
      e.preventDefault();
      void addLinks(text);
    }
  };
  onMount(() => {
    window.addEventListener("keydown", onKey);
    window.addEventListener("paste", onPaste);
  });
  onCleanup(() => {
    window.removeEventListener("keydown", onKey);
    window.removeEventListener("paste", onPaste);
  });
  return (
    <div class="cmd-wrap">
    <form
      class="cmd"
      onSubmit={(e) => {
        e.preventDefault();
        if (!input) return;
        void runCommand(input.value);
        input.value = "";
      }}
    >
      <label class="prompt" for="cmd-input">
        CONFLUO<span>&gt;</span>
      </label>
      <input
        id="cmd-input"
        ref={input}
        type="text"
        autocomplete="off"
        spellcheck={false}
        placeholder="paste a link, or type help"
        aria-describedby="cmd-out"
      />
      <span class="hint" aria-hidden="true">
        <kbd>/</kbd> focus <kbd>↵</kbd> run
      </span>
      <output id="cmd-out" class="cmd-out" classList={{ bad: message()?.bad }} aria-live="polite">
        {message()?.text ?? ""}
      </output>
    </form>
    </div>
  );
}
