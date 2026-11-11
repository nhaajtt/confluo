// The only place that talks to the Rust side.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Status = "queued" | "running" | "paused" | "completed" | "failed";
export type Phase = "probing" | "downloading" | "retrying" | "waiting_for_network" | "verifying";
export type Verdict =
  | "network"
  | "server_per_connection"
  | "server_refusing"
  | "server_single_stream"
  | "disk"
  | "user_limit";

export interface RouteSnapshot {
  id: string;
  label: string;
  bytes: number;
  rate: number;
  connections: number;
  target: number;
  health: "active" | "idle" | "retrying";
  error: string | null;
}

export interface Snapshot {
  phase: Phase;
  file_name: string;
  size: number | null;
  done: number;
  rate: number;
  eta_secs: number | null;
  connections: number;
  routes: RouteSnapshot[];
  map: number[];
  verdict: Verdict;
  shared_uplink: boolean;
  resumable: boolean;
  sources: number;
  steals: number;
  flush_ms: number;
  notes: string[];
}

export interface Item {
  id: number;
  urls: string[];
  dir: string;
  file_name: string;
  status: Status;
  error: string | null;
  path: string | null;
  size: number | null;
  done: number;
  added_at: number;
  finished_at: number | null;
  snapshot: Snapshot | null;
}

export type Networks = { mode: "auto" } | { mode: "system" } | { mode: "only"; names: string[] };

export interface Settings {
  download_dir: string;
  max_concurrent: number;
  connections_per_network: number;
  speed_limit: number;
  network_limits: Record<string, number>;
  networks: Networks;
}

export interface NewDownload {
  urls: string[];
  dir?: string;
  file_name?: string;
  headers?: [string, string][];
  checksum?: string;
  paused?: boolean;
}

export interface Interface {
  name: string;
  label: string;
  description: string;
  medium: string;
  up: boolean;
  has_gateway: boolean;
  link_bps: number;
  addrs: string[];
}

export interface Route {
  id: string;
  label: string;
  bound: boolean;
}

export interface BridgeInfo {
  port: number | null;
  token: string;
  autostart: boolean;
}

export const api = {
  list: () => invoke<Item[]>("list"),
  add: (n: NewDownload) => invoke<number>("add", { new: n }),
  pause: (id: number) => invoke<void>("pause", { id }),
  resume: (id: number) => invoke<void>("resume", { id }),
  remove: (id: number, deleteFile: boolean) => invoke<void>("remove", { id, deleteFile }),
  moveTo: (id: number, index: number) => invoke<void>("move_to", { id, index }),
  clearCompleted: () => invoke<void>("clear_completed"),
  getSettings: () => invoke<Settings>("get_settings"),
  setSettings: (settings: Settings) => invoke<void>("set_settings", { settings }),
  interfaces: () => invoke<Interface[]>("interfaces"),
  routes: () => invoke<Route[]>("routes"),
  bridge: () => invoke<BridgeInfo>("bridge"),
  setBridgeAutostart: (autostart: boolean) => invoke<void>("set_bridge_autostart", { autostart }),
  reveal: (id: number) => invoke<void>("reveal", { id }),
};

export function onEvents(handlers: {
  item: (i: Item) => void;
  removed: (id: number) => void;
  settings: (s: Settings) => void;
  resync: () => void;
}): Promise<UnlistenFn[]> {
  return Promise.all([
    listen<Item>("weft://item", (e) => handlers.item(e.payload)),
    listen<number>("weft://removed", (e) => handlers.removed(e.payload)),
    listen<Settings>("weft://settings", (e) => handlers.settings(e.payload)),
    listen("weft://resync", () => handlers.resync()),
  ]);
}
