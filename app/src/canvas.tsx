// Canvas helpers and the block map.
// Drawings redraw only when their data changes, never on a free-running loop.

import { createEffect, onCleanup, onMount } from "solid-js";
import { tick } from "./store";

export function css(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

/// Sizes a canvas for the screen's pixel density and returns a context in CSS pixels.
export function prepare(canvas: HTMLCanvasElement): { ctx: CanvasRenderingContext2D; w: number; h: number } | null {
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

export function useRedraw(canvas: () => HTMLCanvasElement | undefined, draw: () => void) {
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
