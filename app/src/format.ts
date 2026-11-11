import type { Item, Verdict } from "./api";

const UNITS = ["B", "KiB", "MiB", "GiB", "TiB"];

export function bytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  let v = n;
  let u = 0;
  while (v >= 1024 && u < UNITS.length - 1) {
    v /= 1024;
    u++;
  }
  return `${u === 0 ? v.toFixed(0) : v >= 100 ? v.toFixed(0) : v.toFixed(1)} ${UNITS[u]}`;
}

export function rate(n: number | null | undefined): string {
  if (!n || n < 1) return "0 B/s";
  return `${bytes(n)}/s`;
}

export function duration(secs: number | null | undefined): string {
  if (secs == null || !Number.isFinite(secs)) return "—";
  const s = Math.max(0, Math.round(secs));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, "0")}s`;
  return `${Math.floor(s / 3600)}h ${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}m`;
}

export function percent(item: Item): number | null {
  if (item.status === "completed") return 100;
  if (!item.size) return null;
  return Math.min(100, (item.done / item.size) * 100);
}

/// "2m", "500k", "1.5g", "off" -> bytes per second. null when it is not a rate.
export function parseRate(text: string): number | null {
  const t = text.trim().toLowerCase();
  if (t === "off" || t === "none" || t === "0") return 0;
  const m = /^(\d+(?:\.\d+)?)\s*([kmg])?(?:i?b)?(?:\/s)?$/.exec(t);
  if (!m) return null;
  const mul = m[2] === "g" ? 1024 ** 3 : m[2] === "m" ? 1024 ** 2 : m[2] === "k" ? 1024 : 1;
  return Math.round(Number(m[1]) * mul);
}

export function host(url: string | undefined): string {
  try {
    return url ? new URL(url).host : "";
  } catch {
    return "";
  }
}

export const VERDICTS: Record<Verdict, string> = {
  network: "The network path is the limit",
  server_per_connection: "The server caps each connection; extra connections are adding speed",
  server_refusing: "The server refuses more connections",
  server_single_stream: "The server cannot split or resume this file: one connection only",
  disk: "The disk cannot write faster",
  user_limit: "Your speed limit is the limit",
};

export const STATUS_TAG: Record<Item["status"], string> = {
  queued: "WAIT",
  running: "RUN",
  paused: "HOLD",
  completed: "DONE",
  failed: "FAIL",
};

/// Splits pasted text into links. A line may hold a link and its mirrors separated by " | ".
export function parseLinks(text: string): string[][] {
  const out: string[][] = [];
  for (const line of text.split(/[\r\n]+/)) {
    const group = line
      .split(/\s*\|\s*|\s+/)
      .map((s) => s.trim())
      .filter((s) => /^https?:\/\/\S+$/i.test(s));
    if (!group.length) continue;
    if (line.includes("|")) out.push(group);
    else for (const u of group) out.push([u]);
  }
  return out;
}
