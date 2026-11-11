// The live picture of one download: every network as a thread running into one
// stream, the combined speed, and the last minute of throughput split by network.

import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import type { RouteSnapshot, Snapshot } from "./api";
import { css, prepare, useRedraw } from "./canvas";
import { rate } from "./format";
import type { Sample } from "./store";
import { reducedMotion, tick } from "./store";

const SERIES = ["--s1", "--s2", "--s3", "--s4", "--s5", "--s6"];
const SPAN_MS = 60_000;

const LIMIT_SHORT: Record<Snapshot["verdict"], string> = {
  network: "network path",
  server_per_connection: "server, per link",
  server_refusing: "server refuses links",
  server_single_stream: "server, one link only",
  disk: "disk",
  user_limit: "your speed limit",
};

function seriesVar(i: number): string {
  return SERIES[i % SERIES.length]!;
}

/// "20.1" and "MiB/s" apart, so the number can be set large.
function split(n: number): [string, string] {
  const text = rate(n);
  const at = text.indexOf(" ");
  return at < 0 ? [text, ""] : [text.slice(0, at), text.slice(at + 1)];
}

/// The next "round" value above `v`: 1, 2, 3, 5, 8 times a power of 1024 or of ten below it.
function niceCeil(v: number): number {
  if (v <= 0) return 1024;
  let unit = 1;
  while (v / unit >= 1024) unit *= 1024;
  const x = v / unit;
  for (const step of [1, 2, 3, 5, 8, 10, 15, 20, 30, 50, 80, 100, 150, 200, 300, 500, 800, 1024]) {
    if (x <= step) return step * unit;
  }
  return 1024 * unit;
}

function fit(ctx: CanvasRenderingContext2D, text: string, width: number): string {
  if (ctx.measureText(text).width <= width) return text;
  let t = text;
  while (t.length > 1 && ctx.measureText(`${t}…`).width > width) t = t.slice(0, -1);
  return `${t}…`;
}

/// Networks on the left, one thread each, meeting in a single stream. A thread is
/// as thick as its share of the speed and its dashes move as fast as it carries.
function Threads(props: { routes: () => RouteSnapshot[]; total: () => number; live: () => boolean; label: string }) {
  let el: HTMLCanvasElement | undefined;
  let frame = 0;
  let last = 0;
  let phase = 0;

  const draw = (now = performance.now()) => {
    if (!el) return;
    const p = prepare(el);
    if (!p) return;
    const { ctx, w, h } = p;
    const routes = props.routes();
    const n = routes.length;
    const total = Math.max(1, props.total());
    const dt = last ? Math.min(0.1, (now - last) / 1000) : 0;
    last = now;
    if (props.live() && !reducedMotion()) phase += dt;

    const mono = css("--mono");
    const dim = css("--tt");
    const bad = css("--bad");
    const labelW = Math.min(132, w * 0.44);
    const nodeX = labelW + 10;
    const mergeX = w - 34;
    const mergeY = h / 2;

    routes.forEach((r, i) => {
      const y = n === 1 ? mergeY : 16 + (i * (h - 32)) / (n - 1);
      const color = r.health === "retrying" ? bad : css(seriesVar(i));
      const active = r.rate > 1 && r.health !== "retrying";
      const share = active ? Math.min(1, r.rate / total) : 0;

      ctx.textAlign = "right";
      ctx.textBaseline = "alphabetic";
      ctx.font = `10px ${mono}`;
      ctx.fillStyle = dim;
      ctx.fillText(fit(ctx, r.label.toUpperCase(), labelW - 4), labelW, y - 2);
      ctx.font = `600 11px ${mono}`;
      ctx.fillStyle = active || r.health === "retrying" ? color : dim;
      const under = r.health === "retrying" ? "retrying" : active ? rate(r.rate) : "idle";
      ctx.fillText(under, labelW, y + 11);

      ctx.beginPath();
      ctx.moveTo(nodeX, y);
      const span = mergeX - nodeX;
      ctx.bezierCurveTo(nodeX + span * 0.5, y, mergeX - span * 0.42, mergeY, mergeX - 9, mergeY);
      ctx.strokeStyle = color;
      ctx.lineCap = "round";
      ctx.globalAlpha = active ? 0.95 : 0.35;
      ctx.lineWidth = active ? 1.5 + 4.5 * share : 1.5;
      if (r.health === "retrying") ctx.setLineDash([2, 6]);
      else ctx.setLineDash([7, 7]);
      // Faster networks move their dashes faster, on a log scale so 1 and 100 MiB/s both read.
      const speed = active ? 14 + 22 * Math.log10(1 + r.rate / 65536) : 0;
      ctx.lineDashOffset = -phase * speed;
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.globalAlpha = 1;

      ctx.beginPath();
      ctx.arc(nodeX, y, 3.5, 0, Math.PI * 2);
      ctx.fillStyle = color;
      ctx.globalAlpha = active ? 1 : 0.5;
      ctx.fill();
      ctx.globalAlpha = 1;
    });

    // Where the threads meet, and the one stream that leaves.
    const flowing = props.total() > 1;
    ctx.beginPath();
    ctx.arc(mergeX, mergeY, 7, 0, Math.PI * 2);
    ctx.strokeStyle = flowing ? css("--ac") : dim;
    ctx.lineWidth = 1.5;
    ctx.stroke();
    if (flowing) {
      ctx.beginPath();
      ctx.arc(mergeX, mergeY, 2.5, 0, Math.PI * 2);
      ctx.fillStyle = css("--ac");
      ctx.fill();
    }
    ctx.textAlign = "center";
    ctx.font = `9px ${mono}`;
    ctx.fillStyle = dim;
    ctx.fillText(n > 1 ? "COMBINED" : "STREAM", mergeX - 4, mergeY - 14);
    ctx.beginPath();
    ctx.moveTo(mergeX + 9, mergeY);
    ctx.lineTo(w - 7, mergeY);
    ctx.strokeStyle = flowing ? css("--ts") : dim;
    ctx.lineWidth = 3;
    ctx.lineCap = "butt";
    ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(w, mergeY);
    ctx.lineTo(w - 8, mergeY - 5);
    ctx.lineTo(w - 8, mergeY + 5);
    ctx.closePath();
    ctx.fillStyle = flowing ? css("--ts") : dim;
    ctx.fill();
  };

  // The dashes only move while something is downloading and the window is visible.
  const loop = (now: number) => {
    frame = 0;
    // Twenty frames a second is smooth for drifting dashes and half the work of sixty.
    if (now - last >= 48) draw(now);
    if (props.live() && !reducedMotion() && !document.hidden) frame = requestAnimationFrame(loop);
  };
  const wake = () => {
    if (!frame && props.live() && !reducedMotion() && !document.hidden) frame = requestAnimationFrame(loop);
  };
  createEffect(() => {
    void props.live();
    void reducedMotion();
    last = 0;
    wake();
  });
  onMount(() => {
    document.addEventListener("visibilitychange", wake);
    onCleanup(() => document.removeEventListener("visibilitychange", wake));
  });
  onCleanup(() => cancelAnimationFrame(frame));
  useRedraw(
    () => el,
    () => {
      last = 0;
      draw();
    },
  );
  return (
    <canvas
      ref={el}
      class="threads"
      style={{ height: `${Math.max(84, props.routes().length * 34 + 16)}px` }}
      role="img"
      aria-label={props.label}
    />
  );
}

interface Hover {
  x: number;
  sample: Sample;
  ago: number;
}

/// The last minute, stacked by network, with the total on top.
function Throughput(props: {
  samples: () => Sample[];
  routeIds: () => string[];
  onHover: (h: Hover | null) => void;
  label: string;
}) {
  let el: HTMLCanvasElement | undefined;
  let pointer: number | null = null;

  const draw = () => {
    if (!el) return;
    const p = prepare(el);
    if (!p) return;
    const { ctx, w, h } = p;
    const data = props.samples();
    const ids = props.routeIds();
    const grid = css("--border");
    const dim = css("--tt");
    const mono = css("--mono");
    let peak = 0;
    let sum = 0;
    for (const s of data) {
      peak = Math.max(peak, s.total);
      sum += s.total;
    }
    const top = niceCeil(Math.max(peak * 1.08, 1024));
    const now = data.length ? data[data.length - 1]!.t : 0;
    const X = (t: number) => w - ((now - t) / SPAN_MS) * w;
    const Y = (v: number) => h - 1 - (Math.min(v, top) / top) * (h - 14);

    ctx.font = `10px ${mono}`;
    ctx.textBaseline = "bottom";
    ctx.textAlign = "left";
    for (let i = 1; i <= 2; i++) {
      const y = Math.round(Y((top * i) / 2)) + 0.5;
      ctx.strokeStyle = grid;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
      ctx.stroke();
      ctx.fillStyle = dim;
      ctx.fillText(rate((top * i) / 2), 4, y - 2);
    }
    const shown = data.filter((s) => X(s.t) >= -4);
    if (shown.length < 2) {
      props.onHover(null);
      return;
    }

    // One band per network, stacked so the top edge is the sum of them.
    const base = new Array<number>(shown.length).fill(0);
    ids.forEach((id, i) => {
      const next = shown.map((s, k) => base[k]! + (s.routes[id] ?? 0));
      ctx.beginPath();
      shown.forEach((s, k) => (k ? ctx.lineTo(X(s.t), Y(next[k]!)) : ctx.moveTo(X(s.t), Y(next[k]!))));
      for (let k = shown.length - 1; k >= 0; k--) ctx.lineTo(X(shown[k]!.t), Y(base[k]!));
      ctx.closePath();
      ctx.fillStyle = css(seriesVar(i));
      ctx.globalAlpha = 0.5;
      ctx.fill();
      ctx.globalAlpha = 1;
      next.forEach((v, k) => (base[k] = v));
    });

    ctx.beginPath();
    shown.forEach((s, k) => (k ? ctx.lineTo(X(s.t), Y(s.total)) : ctx.moveTo(X(s.t), Y(s.total))));
    ctx.strokeStyle = css("--tp");
    ctx.lineWidth = 1.5;
    ctx.lineJoin = "round";
    ctx.stroke();

    // The average over the window, as a dashed line with its name at the right edge.
    const avg = sum / data.length;
    if (avg > 0) {
      const y = Math.round(Y(avg)) + 0.5;
      ctx.setLineDash([3, 4]);
      ctx.strokeStyle = dim;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.textAlign = "right";
      ctx.fillStyle = dim;
      ctx.fillText("avg", w - 4, y - 2);
    }

    if (pointer == null) {
      props.onHover(null);
      return;
    }
    let near = shown[0]!;
    for (const s of shown) if (Math.abs(X(s.t) - pointer) < Math.abs(X(near.t) - pointer)) near = s;
    const x = Math.round(X(near.t)) + 0.5;
    ctx.strokeStyle = css("--border-h");
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, h);
    ctx.stroke();
    ctx.beginPath();
    ctx.arc(x, Y(near.total), 3, 0, Math.PI * 2);
    ctx.fillStyle = css("--tp");
    ctx.fill();
    props.onHover({ x, sample: near, ago: (now - near.t) / 1000 });
  };
  useRedraw(() => el, draw);
  const move = (e: PointerEvent) => {
    pointer = e.offsetX;
    requestAnimationFrame(draw);
  };
  const leave = () => {
    pointer = null;
    requestAnimationFrame(draw);
  };
  return <canvas ref={el} class="chart" role="img" aria-label={props.label} onPointerMove={move} onPointerLeave={leave} />;
}

export function FlowPanel(props: { snap: () => Snapshot | null | undefined; running: () => boolean; samples: () => Sample[] }) {
  const routes = () => props.snap()?.routes ?? [];
  const total = () => (props.running() ? (props.snap()?.rate ?? 0) : 0);
  const [hover, setHover] = createSignal<Hover | null>(null);
  const window60 = createMemo(() => {
    tick();
    const data = props.samples();
    let peak = 0;
    let sum = 0;
    for (const s of data) {
      peak = Math.max(peak, s.total);
      sum += s.total;
    }
    return { peak, avg: data.length ? sum / data.length : 0 };
  });
  const big = () => split(total());
  const shown = () => hover()?.sample;
  return (
    <section class="flow">
      <h3 class="lbl">Live flow</h3>
      <div class="flow-top">
        <Threads
          routes={routes}
          total={total}
          live={() => props.running() && total() > 1}
          label={`${routes().length} network${routes().length === 1 ? "" : "s"} carrying ${rate(total())} together`}
        />
        <div class="flow-read">
          <span class="lbl">Total speed</span>
          <span class="big">
            {big()[0]}
            <small>{big()[1]}</small>
          </span>
          <span class="sub">avg {rate(window60().avg)}</span>
          <span class="sub">peak {rate(window60().peak)}</span>
          <Show when={props.snap() && props.running()}>
            <span class="sub limit" title="What currently holds the speed back">
              limit: {LIMIT_SHORT[props.snap()!.verdict]}
            </span>
          </Show>
        </div>
      </div>
      <div class="flow-head">
        <span class="lbl">Throughput · last 60 s</span>
        <span class="sub">{hover() ? `${hover()!.ago.toFixed(0)} s ago · ${rate(hover()!.sample.total)}` : ""}</span>
      </div>
      <Throughput
        samples={props.samples}
        routeIds={() => routes().map((r) => r.id)}
        onHover={setHover}
        label="Download speed over the last minute, split by network"
      />
      <div class="flow-axis">
        <span>60 s ago</span>
        <span>now</span>
      </div>
      <ul class="flow-legend">
        <For each={routes()}>
          {(r, i) => {
            const v = () => shown()?.routes[r.id] ?? (props.running() ? r.rate : 0);
            const whole = () => shown()?.total ?? total();
            return (
              <li>
                <i style={{ background: `var(${seriesVar(i())})` }} />
                <span class="name">
                  {r.label}
                  <small>
                    {" "}
                    {r.health === "retrying" ? "retrying" : `${r.connections} link${r.connections === 1 ? "" : "s"}`}
                  </small>
                </span>
                <span class="num">{rate(v())}</span>
                <span class="num dim">{whole() > 1 ? `${Math.round((v() / whole()) * 100)}%` : "—"}</span>
              </li>
            );
          }}
        </For>
      </ul>
    </section>
  );
}
