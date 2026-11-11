// Application state. One store, updated by events from the engine.

import { createSignal } from "solid-js";
import { createStore, produce, reconcile } from "solid-js/store";
import { api, onEvents, type BridgeInfo, type Interface, type Item, type Route, type Settings } from "./api";

export interface Sample {
  t: number;
  total: number;
  routes: Record<string, number>;
}

const HISTORY = 240; // one minute at four samples a second

export const [items, setItems] = createStore<Item[]>([]);
export const [settings, setSettings] = createSignal<Settings | null>(null);
export const [routes, setRoutes] = createSignal<Route[]>([]);
export const [interfaces, setInterfaces] = createSignal<Interface[]>([]);
export const [bridge, setBridge] = createSignal<BridgeInfo | null>(null);
export const [selected, setSelected] = createSignal<number | null>(null);
/// Bumped whenever a history buffer changes, so canvases know to redraw.
export const [tick, setTick] = createSignal(0);

const history = new Map<number, Sample[]>();

export function samples(id: number): Sample[] {
  return history.get(id) ?? [];
}

function record(item: Item) {
  const snap = item.snapshot;
  if (!snap || item.status !== "running") {
    if (item.status !== "running" && item.status !== "paused") history.delete(item.id);
    return;
  }
  let buf = history.get(item.id);
  if (!buf) {
    buf = [];
    history.set(item.id, buf);
  }
  const per: Record<string, number> = {};
  for (const r of snap.routes) per[r.id] = r.rate;
  buf.push({ t: performance.now(), total: snap.rate, routes: per });
  if (buf.length > HISTORY) buf.splice(0, buf.length - HISTORY);
  setTick((n) => n + 1);
}

function upsert(item: Item) {
  record(item);
  const i = items.findIndex((x) => x.id === item.id);
  if (i < 0) {
    setItems(items.length, item);
    if (selected() == null) setSelected(item.id);
  } else {
    setItems(i, reconcile(item));
  }
}

async function loadList() {
  const list = await api.list();
  setItems(reconcile(list));
  const first = list[0];
  if (selected() == null && first) setSelected(first.id);
}

export async function refreshNetwork() {
  const [r, i] = await Promise.all([api.routes(), api.interfaces()]);
  setRoutes(r);
  setInterfaces(i);
}

export async function init() {
  await onEvents({
    item: upsert,
    removed: (id) => {
      history.delete(id);
      setItems(produce((list) => {
        const i = list.findIndex((x) => x.id === id);
        if (i >= 0) list.splice(i, 1);
      }));
      if (selected() === id) setSelected(items[0]?.id ?? null);
    },
    settings: (s) => {
      setSettings(s);
      void refreshNetwork();
    },
    resync: () => void loadList(),
  });
  const [s, b] = await Promise.all([api.getSettings(), api.bridge()]);
  setSettings(s);
  setBridge(b);
  await Promise.all([loadList(), refreshNetwork()]);
  // Interfaces come and go; a slow poll keeps the footer honest without events.
  setInterval(() => void refreshNetwork(), 5000);
}

export function totalRate(): number {
  let sum = 0;
  for (const it of items) if (it.status === "running" && it.snapshot) sum += it.snapshot.rate;
  return sum;
}

export function count(status: Item["status"]): number {
  let n = 0;
  for (const it of items) if (it.status === status) n++;
  return n;
}

// ---- Look and feel, kept in localStorage (applied before first paint by theme-init.js). ----

export const ACCENTS = ["neon", "cyan", "orange", "green", "pink", "amber"] as const;
export type Accent = (typeof ACCENTS)[number];

function stored(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* not persisted, still applied */
  }
}

export const [theme, setThemeSignal] = createSignal<"dark" | "light">(
  stored("weft.theme", "dark") === "light" ? "light" : "dark",
);
export const [accent, setAccentSignal] = createSignal<Accent>(
  (ACCENTS as readonly string[]).includes(stored("weft.accent", "neon"))
    ? (stored("weft.accent", "neon") as Accent)
    : "neon",
);
export const [reducedMotion, setReducedSignal] = createSignal(stored("weft.motion", "") === "reduced");

export function setTheme(t: "dark" | "light") {
  document.documentElement.dataset.theme = t;
  store("weft.theme", t);
  setThemeSignal(t);
  setTick((n) => n + 1);
}

export function setAccent(a: Accent) {
  document.documentElement.dataset.accent = a;
  store("weft.accent", a);
  setAccentSignal(a);
  setTick((n) => n + 1);
}

export function setReducedMotion(on: boolean) {
  if (on) document.documentElement.dataset.motion = "reduced";
  else delete document.documentElement.dataset.motion;
  store("weft.motion", on ? "reduced" : "");
  setReducedSignal(on);
}
