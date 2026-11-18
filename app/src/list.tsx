import { For, Show } from "solid-js";
import { api, type Item } from "./api";
import { bytes, duration, host, percent, rate, STATUS_TAG } from "./format";
import { items, selected, setSelected } from "./store";

export function toggle(item: Item) {
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

export function List() {
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
