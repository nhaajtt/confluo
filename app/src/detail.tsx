import { createMemo, For, Show } from "solid-js";
import { api } from "./api";
import { BlockMap } from "./canvas";
import { say } from "./command";
import { FlowPanel } from "./flow";
import { bytes, duration, percent, rate, STATUS_TAG, VERDICTS } from "./format";
import { toggle } from "./list";
import { items, samples, selected } from "./store";

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

export function Detail() {
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
