import { createMemo, For, onMount, Show } from "solid-js";
import { open as pickFolder } from "@tauri-apps/plugin-dialog";
import { api, type Networks } from "./api";
import { saveSettings, say, setSetupOpen } from "./command";
import { parseRate, rate } from "./format";
import {
  accent,
  ACCENTS,
  bridge,
  interfaces,
  reducedMotion,
  refreshNetwork,
  setAccent,
  setBridge,
  setReducedMotion,
  settings,
  setTheme,
  theme,
} from "./store";

function Switch(props: { on: boolean; label: string; onChange: (on: boolean) => void }) {
  return (
    <button class="switch" role="switch" aria-checked={props.on} onClick={() => props.onChange(!props.on)}>
      <span aria-hidden="true">{props.on ? "[ON ]" : "[OFF]"}</span> {props.label}
    </button>
  );
}

export function Setup() {
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
