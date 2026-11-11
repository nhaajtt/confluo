// The two live drawings: the block map and the speed chart.
// Both redraw only when their data changes, never on a free-running loop.

import { createEffect, onCleanup, onMount } from "solid-js";
import type { Sample } from "./store";
import { tick } from "./store";

function css(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/// Sizes a canvas for the screen's pixel density and returns a context in CSS pixels.
function prepare(canvas: HTMLCanvasElement): { ctx: CanvasRenderingContext2D; w: number; h: number } | null {
  const rect = canvas.getBoundingClientRect();
  if (rect.width < 2 || rect.height < 2) return null;
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const w = Math.round(rect.width * dpr);
  const h = Math.round(rect.height * dpr);
  if (canvas.width !== w || canvas.height !== h) {
    canvas.width = w;
    canvas.height = h;
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, rect.width, rect.height);
  return { ctx, w: rect.width, h: rect.height };
}

function useRedraw(canvas: () => HTMLCanvasElement | undefined, draw: () => void) {
  let frame = 0;
  const schedule = () => {
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      draw();
    });
  };
  createEffect(() => {
    tick();
    schedule();
  });
  onMount(() => {
    const el = canvas();
    if (!el) return;
    const ro = new ResizeObserver(schedule);
    ro.observe(el);
    onCleanup(() => ro.disconnect());
  });
  onCleanup(() => cancelAnimationFrame(frame));
}

/// How complete each slice of the file is: one cell per slice, filled by its share.
export function BlockMap(props: { map: number[]; done: boolean; label: string }) {
  let el: HTMLCanvasElement | undefined;
  const draw = () => {
    if (!el) return;
    const p = prepare(el);
    if (!p) return;
    const { ctx, w, h } = p;
    const cells = props.map.length || 256;
    const cols = Math.max(16, Math.min(64, Math.floor(w / 9)));
    const rows = Math.ceil(cells / cols);
    const gap = 1;
    const cw = (w - gap * (cols - 1)) / cols;
    const ch = Math.min(cw, (h - gap * (rows - 1)) / rows);
    const empty = css("--border");
    const fill = css("--ac");
    for (let i = 0; i < cells; i++) {
      const x = (i % cols) * (cw + gap);
      const y = Math.floor(i / cols) * (ch + gap);
      const v = props.done ? 255 : (props.map[i] ?? 0);
      ctx.fillStyle = empty;
      ctx.fillRect(x, y, cw, ch);
      if (v > 0) {
        ctx.fillStyle = fill;
        ctx.globalAlpha = v >= 255 ? 1 : 0.35 + 0.5 * (v / 255);
        // A partly fetched slice fills from the left, like the bytes do.
        ctx.fillRect(x, y, v >= 255 ? cw : Math.max(1, cw * (v / 255)), ch);
        ctx.globalAlpha = 1;
      }
    }
  };
  useRedraw(() => el, draw);
  createEffect(() => {
    void props.map;
    void props.done;
    requestAnimationFrame(draw);
  });
  return <canvas ref={el} class="blockmap" role="img" aria-label={props.label} />;
}

/// Speed over the last minute: the total as a filled line, each network as a thin one.
export function SpeedChart(props: { samples: () => Sample[]; routeIds: string[]; label: string }) {
  let el: HTMLCanvasElement | undefined;
  const draw = () => {
    if (!el) return;
    const p = prepare(el);
    if (!p) return;
    const { ctx, w, h } = p;
    const data = props.samples();
    const grid = css("--border");
    const text = css("--tt");
    ctx.strokeStyle = grid;
    ctx.lineWidth = 1;
    for (let i = 1; i < 4; i++) {
      const y = Math.round((h * i) / 4) + 0.5;
      ctx.beginPath();
      ctx.moveTo(0, y);
      ctx.lineTo(w, y);
      ctx.stroke();
    }
    if (data.length < 2) return;
    let peak = 1;
    for (const s of data) peak = Math.max(peak, s.total);
    peak *= 1.15;
    const now = data[data.length - 1]!.t;
    const span = 60_000;
    const xy = (t: number, v: number): [number, number] => [w - ((now - t) / span) * w, h - (v / peak) * (h - 2) - 1];

    const line = (pick: (s: Sample) => number) => {
      ctx.beginPath();
      let started = false;
      for (const s of data) {
        const [x, y] = xy(s.t, pick(s));
        if (x < 0) continue;
        if (started) ctx.lineTo(x, y);
        else ctx.moveTo(x, y);
        started = true;
      }
    };

    if (props.routeIds.length > 1) {
      ctx.strokeStyle = css("--ac-soft");
      ctx.globalAlpha = 0.55;
      ctx.lineWidth = 1;
      for (const id of props.routeIds) {
        line((s) => s.routes[id] ?? 0);
        ctx.stroke();
      }
      ctx.globalAlpha = 1;
    }
    line((s) => s.total);
    ctx.strokeStyle = css("--ac");
    ctx.lineWidth = 1.5;
    ctx.stroke();
    const first = data.find((s) => xy(s.t, 0)[0] >= 0) ?? data[0]!;
    ctx.lineTo(w, h);
    ctx.lineTo(xy(first.t, 0)[0], h);
    ctx.closePath();
    ctx.fillStyle = css("--ac-dim");
    ctx.fill();

    ctx.fillStyle = text;
    ctx.font = `10px ${css("--mono")}`;
    ctx.textBaseline = "top";
    ctx.fillText(`${(peak / 1.15 / 1048576).toFixed(1)} MiB/s`, 4, 3);
  };
  useRedraw(() => el, draw);
  return <canvas ref={el} class="chart" role="img" aria-label={props.label} />;
}
