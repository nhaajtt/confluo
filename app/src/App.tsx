import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { api, type Item, type Networks, type Settings } from "./api";
import { BlockMap } from "./canvas";
import { FlowPanel } from "./flow";
import { bytes, duration, host, parseLinks, parseRate, percent, rate, STATUS_TAG, VERDICTS } from "./format";
import {
  accent,
  ACCENTS,
  bridge,
  count,
  interfaces,
  items,
  reducedMotion,
  refreshNetwork,
  routes,
  samples,
  selected,
  setAccent,
  setBridge,
  setReducedMotion,
  setSelected,
  settings,
  setTheme,
  theme,
  totalRate,
} from "./store";

const [setupOpen, setSetupOpen] = createSignal(false);
const [message, setMessage] = createSignal<{ text: string; bad: boolean } | null>(null);

function say(text: string, bad = false) {
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

async function saveSettings(change: Partial<Settings>) {
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

function CommandBar() {
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
        WEFT<span>&gt;</span>
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

function toggle(item: Item) {
  if (item.status === "running" || item.status === "queued") void api.pause(item.id);
  else if (item.status === "paused" || item.status === "failed") void api.resume(item.id);
}

function Row(props: { item: Item; index: number }) {
  const it = () => props.item;
  const pct = () => percent(it());
  const active = () => it().status === "running";
  return (
    <li
      class="row"
      classList={{ sel: selected() === it().id, [it().status]: true }}
      role="option"
      aria-selected={selected() === it().id}
      tabindex={selected() === it().id ? 0 : -1}
      data-id={it().id}
      onClick={() => setSelected(it().id)}
      onDblClick={() => toggle(it())}
    >
      <span class="num">{String(props.index + 1).padStart(2, "0")}</span>
      <span class="name">
        <b title={it().file_name}>{it().file_name || host(it().urls[0])}</b>
        <small>
          {host(it().urls[0])}
          <Show when={it().urls.length > 1}> · {it().urls.length} sources</Show>
          {" · "}
          {it().size != null ? `${bytes(it().done)} / ${bytes(it().size)}` : bytes(it().done)}
        </small>
      </span>
      <span class="prog">
        <span class="bar" aria-hidden="true">
          <i style={{ width: `${pct() ?? 0}%` }} />
        </span>
        <span class="pct">{pct() == null ? "—" : `${pct()!.toFixed(pct()! >= 100 ? 0 : 1)}%`}</span>
      </span>
      <span class="speed">
        <Show when={active()} fallback={<span class="dim">{it().status === "failed" ? "error" : "—"}</span>}>
          {rate(it().snapshot?.rate)}
          <small>{duration(it().snapshot?.eta_secs)}</small>
        </Show>
      </span>
      <span class="tag" classList={{ on: active(), bad: it().status === "failed" }}>
        [{STATUS_TAG[it().status]}]
      </span>
    </li>
  );
}

function List() {
  let listEl: HTMLUListElement | undefined;
  const move = (delta: number) => {
    const i = items.findIndex((x) => x.id === selected());
    const next = items[Math.max(0, Math.min(items.length - 1, (i < 0 ? 0 : i) + delta))];
    if (!next) return;
    setSelected(next.id);
    queueMicrotask(() => listEl?.querySelector<HTMLElement>(`[data-id="${next.id}"]`)?.focus());
  };
  const onKey = (e: KeyboardEvent) => {
    const current = items.find((x) => x.id === selected());
    if (e.key === "ArrowDown") move(1);
    else if (e.key === "ArrowUp") move(-1);
    else if (e.key === "Home") move(-items.length);
    else if (e.key === "End") move(items.length);
    else if ((e.key === " " || e.key === "Enter") && current) toggle(current);
    else if (e.key === "Delete" && current) void api.remove(current.id, false);
    else return;
    e.preventDefault();
  };
  return (
    <section class="list" aria-label="Downloads">
      <header class="cols" aria-hidden="true">
        <span>No</span>
        <span>File</span>
        <span>Progress</span>
        <span>Speed</span>
        <span>State</span>
      </header>
      <Show
        when={items.length}
        fallback={
          <div class="empty">
            <pre aria-hidden="true">{"┌──────────────┐\n│  ░░░░░░░░░░  │\n└──────────────┘"}</pre>
            <p>Nothing here yet.</p>
            <p class="dim">Paste a link into the bar above and press Enter.</p>
          </div>
        }
      >
        <ul ref={listEl} role="listbox" aria-label="Downloads" onKeyDown={onKey}>
          <For each={items}>{(item, i) => <Row item={item} index={i()} />}</For>
        </ul>
      </Show>
    </section>
  );
}

function Stat(props: { label: string; value: string; sub?: string }) {
  return (
    <div class="stat">
      <span class="lbl">{props.label}</span>
      <span class="val">{props.value}</span>
      <Show when={props.sub}>
        <span class="sub">{props.sub}</span>
      </Show>
    </div>
  );
}

function Detail() {
  const item = createMemo(() => items.find((x) => x.id === selected()) ?? null);
  return (
    <aside class="detail" aria-label="Selected download">
      <Show when={item()} fallback={<p class="dim pad">Select a download to see how it is doing.</p>} keyed>
        {(it) => {
          const snap = () => it.snapshot;
          const pct = () => percent(it);
          const busy = () => it.status === "running" || it.status === "queued";
          return (
            <>
              <header>
                <span class="lbl">
                  {String(items.findIndex((x) => x.id === it.id) + 1).padStart(2, "0")} · {STATUS_TAG[it.status]}
                  <Show when={snap()?.phase && snap()!.phase !== "downloading"}> · {snap()!.phase.replaceAll("_", " ")}</Show>
                </span>
                <h2 title={it.file_name}>{it.file_name}</h2>
                <p class="dim url" title={it.urls[0]}>
                  {it.urls[0]}
                </p>
              </header>

              <div class="stats">
                <Stat label="Done" value={pct() == null ? "—" : `${pct()!.toFixed(1)}%`} sub={`${bytes(it.done)} / ${bytes(it.size)}`} />
                <Stat label="Speed" value={it.status === "running" ? rate(snap()?.rate) : "—"} sub={`${duration(snap()?.eta_secs)} left`} />
                <Stat label="Links" value={String(snap()?.connections ?? 0)} sub={`${snap()?.sources ?? it.urls.length} source${(snap()?.sources ?? it.urls.length) === 1 ? "" : "s"}`} />
              </div>

              <section>
                <h3 class="lbl">File map</h3>
                <BlockMap
                  map={snap()?.map ?? []}
                  done={it.status === "completed"}
                  label={`File map, ${pct()?.toFixed(0) ?? 0} percent complete`}
                />
              </section>

              <FlowPanel snap={snap} running={() => it.status === "running"} samples={() => samples(it.id)} />

              <Show when={snap()}>
                {(s) => (
                  <section>
                    <h3 class="lbl">Networks</h3>
                    <table class="nets">
                      <thead>
                        <tr>
                          <th scope="col">Network</th>
                          <th scope="col">Speed</th>
                          <th scope="col">Links</th>
                          <th scope="col">Carried</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={s().routes}>
                          {(r) => (
                            <tr classList={{ bad: r.health === "retrying" }}>
                              <td title={r.error ?? ""}>
                                {r.label}
                                <Show when={r.health === "retrying"}>
                                  <small> retrying{r.error ? `: ${r.error}` : ""}</small>
                                </Show>
                              </td>
                              <td>{rate(r.rate)}</td>
                              <td>{r.connections}</td>
                              <td>{bytes(r.bytes)}</td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                    <p class="verdict">
                      <span class="lbl">Limit</span> {VERDICTS[s().verdict]}.
                    </p>
                    <Show when={s().shared_uplink}>
                      <p class="note">These networks reach the internet through the same router, so adding them up cannot go faster than one of them.</p>
                    </Show>
                    <Show when={!s().resumable}>
                      <p class="note">This server cannot resume. If the download is interrupted it starts again from the beginning.</p>
                    </Show>
                    <For each={s().notes}>{(n) => <p class="note">{n}.</p>}</For>
                  </section>
                )}
              </Show>

              <Show when={it.error}>
                <p class="err" role="alert">
                  {it.error}
                </p>
              </Show>

              <footer class="actions">
                <Show when={it.status !== "completed"}>
                  <button class="primary" onClick={() => toggle(it)}>
                    {busy() ? "Pause" : it.status === "failed" ? "Try again" : "Resume"}
                  </button>
                </Show>
                <Show when={it.status === "completed"}>
                  <button class="primary" onClick={() => api.reveal(it.id).catch((e) => say(String(e), true))}>
                    Show in folder
                  </button>
                </Show>
                <button onClick={() => navigator.clipboard.writeText(it.urls[0] ?? "").then(() => say("Link copied."))}>
                  Copy link
                </button>
                <button class="danger" onClick={() => void api.remove(it.id, false)}>
                  {it.status === "completed" ? "Remove from list" : "Cancel and remove"}
                </button>
              </footer>
            </>
          );
        }}
      </Show>
    </aside>
  );
}

function Switch(props: { on: boolean; label: string; onChange: (on: boolean) => void }) {
  return (
    <button class="switch" role="switch" aria-checked={props.on} onClick={() => props.onChange(!props.on)}>
      <span aria-hidden="true">{props.on ? "[ON ]" : "[OFF]"}</span> {props.label}
    </button>
  );
}

function Setup() {
  let panel: HTMLDivElement | undefined;
  const s = () => settings()!;
  const mode = () => s().networks.mode;
  const chosen = () => (s().networks.mode === "only" ? (s().networks as { names: string[] }).names : []);
  const usable = createMemo(() => interfaces().filter((i) => i.up && i.addrs.length > 0));
  const setNetworks = (n: Networks) => void saveSettings({ networks: n }).then(refreshNetwork);
  const toggleNet = (name: string) => {
    const set = new Set(chosen());
    if (set.has(name)) set.delete(name);
    else set.add(name);
    setNetworks({ mode: "only", names: [...set] });
  };
  const close = () => setSetupOpen(false);
  onMount(() => {
    panel?.querySelector<HTMLElement>("button, input, select")?.focus();
    void refreshNetwork();
    void api.bridge().then(setBridge);
  });
  return (
    <div class="scrim" onClick={(e) => e.target === e.currentTarget && close()}>
      <div
        class="drawer"
        ref={panel}
        role="dialog"
        aria-modal="true"
        aria-label="Setup"
        onKeyDown={(e) => e.key === "Escape" && close()}
      >
        <header>
          <h2>Setup</h2>
          <button onClick={close} aria-label="Close setup">
            ✕
          </button>
        </header>

        <section>
          <h3 class="lbl">Downloads</h3>
          <label class="field">
            <span>Save to</span>
            <span class="pathrow">
              <code title={s().download_dir}>{s().download_dir}</code>
              <button
                onClick={async () => {
                  const dir = await pickFolder({ directory: true, defaultPath: s().download_dir });
                  if (typeof dir === "string") await saveSettings({ download_dir: dir });
                }}
              >
                Change
              </button>
            </span>
          </label>
          <label class="field">
            <span>At the same time</span>
            <input
              type="number"
              min="1"
              max="16"
              value={s().max_concurrent}
              onChange={(e) => void saveSettings({ max_concurrent: Math.max(1, Math.min(16, e.currentTarget.valueAsNumber || 1)) })}
            />
          </label>
          <label class="field">
            <span>Most links per network</span>
            <input
              type="number"
              min="1"
              max="64"
              value={s().connections_per_network}
              onChange={(e) =>
                void saveSettings({ connections_per_network: Math.max(1, Math.min(64, e.currentTarget.valueAsNumber || 1)) })
              }
            />
          </label>
          <label class="field">
            <span>Speed limit</span>
            <input
              type="text"
              placeholder="off"
              value={s().speed_limit ? rate(s().speed_limit).replace("/s", "") : ""}
              onChange={(e) => {
                const r = e.currentTarget.value.trim() ? parseRate(e.currentTarget.value) : 0;
                if (r == null) say("Speed limit: use a value like 500k or 2m.", true);
                else void saveSettings({ speed_limit: r });
              }}
            />
          </label>
        </section>

        <section>
          <h3 class="lbl">Networks</h3>
          <div class="seg" role="radiogroup" aria-label="Which networks to use">
            <button role="radio" aria-checked={mode() === "auto"} onClick={() => setNetworks({ mode: "auto" })}>
              Automatic
            </button>
            <button role="radio" aria-checked={mode() === "system"} onClick={() => setNetworks({ mode: "system" })}>
              System route
            </button>
            <button role="radio" aria-checked={mode() === "only"} onClick={() => setNetworks({ mode: "only", names: chosen() })}>
              Choose
            </button>
          </div>
          <p class="note">
            <Show when={mode() === "auto"}>
              Uses every network that has its own way out. With one network, or while a VPN is on, routing is left to the system so nothing goes around the VPN.
            </Show>
            <Show when={mode() === "system"}>Leaves routing to the operating system. One network at a time.</Show>
            <Show when={mode() === "only"}>
              Uses exactly the networks you tick. A ticked adapter is used directly, even while a VPN is on.
            </Show>
          </p>
          <ul class="ifaces">
            <For each={usable()}>
              {(i) => (
                <li>
                  <Show when={mode() === "only"} fallback={<span class="tag" classList={{ on: i.has_gateway }}>{i.has_gateway ? "[UP ]" : "[LAN]"}</span>}>
                    <button class="switch" role="switch" aria-checked={chosen().includes(i.name)} onClick={() => toggleNet(i.name)}>
                      <span aria-hidden="true">{chosen().includes(i.name) ? "[ON ]" : "[OFF]"}</span>
                    </button>
                  </Show>
                  <span class="ifname">
                    <b>{i.label}</b>
                    <small>
                      {i.medium}
                      {i.link_bps ? ` · ${(i.link_bps / 1e6).toFixed(0)} Mbit` : ""}
                      {i.has_gateway ? "" : " · no way out"}
                    </small>
                  </span>
                  <input
                    type="text"
                    class="lim"
                    placeholder="no limit"
                    aria-label={`Speed limit for ${i.label}`}
                    value={s().network_limits[i.name] ? rate(s().network_limits[i.name]).replace("/s", "") : ""}
                    onChange={(e) => {
                      const r = e.currentTarget.value.trim() ? parseRate(e.currentTarget.value) : 0;
                      if (r == null) return say("Network limit: use a value like 500k or 2m.", true);
                      const limits = { ...s().network_limits };
                      if (r) limits[i.name] = r;
                      else delete limits[i.name];
                      void saveSettings({ network_limits: limits });
                    }}
                  />
                </li>
              )}
            </For>
          </ul>
        </section>

        <section>
          <h3 class="lbl">Browser</h3>
          <Show when={bridge()} keyed>
            {(b) => (
              <>
                <p class="note">
                  <Show when={b.port} fallback="The browser link is off: its port is taken by another program.">
                    Listening on 127.0.0.1:{b.port}, reachable from this computer only. Paste this code into the extension to pair it:
                  </Show>
                </p>
                <div class="pathrow">
                  <code class="token">{b.token}</code>
                  <button onClick={() => navigator.clipboard.writeText(b.token).then(() => say("Pairing code copied."))}>Copy</button>
                </div>
                <Switch
                  on={b.autostart}
                  label="Start links from the browser right away"
                  onChange={(on) => void api.setBridgeAutostart(on).then(() => api.bridge().then(setBridge))}
                />
              </>
            )}
          </Show>
        </section>

        <section>
          <h3 class="lbl">Look</h3>
          <div class="seg" role="radiogroup" aria-label="Theme">
            <button role="radio" aria-checked={theme() === "dark"} onClick={() => setTheme("dark")}>
              Dark
            </button>
            <button role="radio" aria-checked={theme() === "light"} onClick={() => setTheme("light")}>
              Light
            </button>
          </div>
          <div class="swatches" role="radiogroup" aria-label="Accent colour">
            <For each={ACCENTS}>
              {(a) => (
                <button role="radio" aria-checked={accent() === a} data-swatch={a} onClick={() => setAccent(a)}>
                  <i aria-hidden="true" /> {a}
                </button>
              )}
            </For>
          </div>
          <Switch on={reducedMotion()} label="Reduce motion" onChange={setReducedMotion} />
        </section>
      </div>
    </div>
  );
}

export default function App() {
  return (
    <div class="app">
      <header class="top">
        <h1>
          WEFT <span>DL-01</span>
        </h1>
        <p class="strap">MULTI-NETWORK DOWNLOADS · EVERY BYTE ACCOUNTED FOR</p>
        <div class="meters" aria-live="off">
          <span>
            <b>↓ {rate(totalRate())}</b>
          </span>
          <span>
            {count("running")} running · {count("queued")} waiting
          </span>
          <button class="round" onClick={() => setTheme(theme() === "dark" ? "light" : "dark")} aria-label="Switch between dark and light" title={theme() === "dark" ? "Dark" : "Light"}>
            ◐
          </button>
          <button onClick={() => setSetupOpen(true)} aria-haspopup="dialog">
            ⚙ Setup
          </button>
        </div>
      </header>
      <CommandBar />
      <main>
        <List />
        <Detail />
      </main>
      <footer class="status">
        <span class="lbl">Net</span>
        <For each={routes()} fallback={<span class="bad">no network</span>}>
          {(r) => (
            <span>
              {r.label} <i class="tag on">{r.bound ? "[BOUND]" : "[SYSTEM]"}</i>
            </span>
          )}
        </For>
        <span class="grow" />
        <Show when={settings()?.speed_limit}>
          <span>limit {rate(settings()!.speed_limit)}</span>
        </Show>
        <span>
          browser <i class="tag" classList={{ on: !!bridge()?.port }}>{bridge()?.port ? `[:${bridge()!.port}]` : "[OFF]"}</i>
        </span>
        <span class="dim">v0.1.0</span>
      </footer>
      <Show when={setupOpen() && settings()}>
        <Setup />
      </Show>
    </div>
  );
}
