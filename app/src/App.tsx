import mark from "./mark.png";
import { For, Show } from "solid-js";
import { CommandBar, setupOpen, setSetupOpen } from "./command";
import { Detail } from "./detail";
import { rate } from "./format";
import { List } from "./list";
import { Setup } from "./setup";
import { bridge, count, routes, settings, setTheme, theme, totalRate } from "./store";

export default function App() {
  return (
    <div class="app">
      <header class="top">
        <h1>
          <img src={mark} alt="" width="26" height="26" />
          CONFLUO <span>DL-01</span>
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
