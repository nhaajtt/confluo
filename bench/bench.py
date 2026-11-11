"""Runs the same downloads through Weft and Baseline and records time, CPU, memory and server-side counters.

    bench/.venv/Scripts/python bench/bench.py --ip 10.10.99.106 --runs 3

Everything is local: the file server is `weft-testsrv`, bound to one of this
machine's own LAN addresses (Baseline binds its sockets to real adapters and cannot
reach 127.0.0.1). Results go to bench/results/.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import urllib.request
from pathlib import Path

import psutil

ROOT = Path(__file__).resolve().parent.parent
REL = ROOT / "target" / "release"
WEFT = REL / "weft.exe"
WEFT_APP = REL / "weft-app.exe"
TESTSRV = REL / "weft-testsrv.exe"
BASELINE_DRIVER = ROOT / "ref" / "baseline-drive.mjs"
MIB = 1 << 20

SCENARIOS = {
    # name: (size, server flags, what it shows)
    "unlimited": ("1g", [], "local server, no limit: engine and disk overhead"),
    "per-connection-cap": ("200m", ["--rate-conn", "1m"], "server caps each connection at 1 MiB/s"),
    "link-cap": ("200m", ["--rate-total", "20m"], "one 20 MiB/s bottleneck shared by all connections"),
    "no-range": ("200m", ["--no-ranges", "--rate-total", "40m"], "server without Range support"),
    "busy-server": ("200m", ["--max-conns", "4", "--rate-conn", "2m"], "server allows 4 connections, 2 MiB/s each"),
    "slow-first-byte": ("200m", ["--latency", "150", "--rate-conn", "4m"], "150 ms before every response, 4 MiB/s per connection"),
}


class Monitor:
    """Samples CPU time and memory of a process tree until stopped."""

    def __init__(self, pid: int):
        self.pid = pid
        self.cpu: dict[int, float] = {}
        self.peak_rss = 0
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def _sample(self):
        try:
            root = psutil.Process(self.pid)
            procs = [root] + root.children(recursive=True)
        except psutil.Error:
            return
        rss = 0
        for p in procs:
            try:
                t = p.cpu_times()
                self.cpu[p.pid] = max(self.cpu.get(p.pid, 0.0), t.user + t.system)
                rss += p.memory_info().rss
            except psutil.Error:
                pass
        self.peak_rss = max(self.peak_rss, rss)

    def _run(self):
        while not self._stop.is_set():
            self._sample()
            self._stop.wait(0.2)

    def stop(self) -> tuple[float, int]:
        self._sample()
        self._stop.set()
        self._thread.join()
        return sum(self.cpu.values()), self.peak_rss


def kill_tree(pid: int):
    subprocess.run(["taskkill", "/F", "/T", "/PID", str(pid)], capture_output=True)


def http_json(url: str, data: bytes | None = None, headers: dict | None = None, timeout=3):
    req = urllib.request.Request(url, data=data, headers=headers or {})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read() or b"{}")


class Server:
    def __init__(self, ip: str, port: int, size: str, flags: list[str]):
        self.base = f"http://{ip}:{port}"
        self.proc = subprocess.Popen(
            [str(TESTSRV), "--bind", f"{ip}:{port}", "--size", size, *flags],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        for _ in range(50):
            try:
                http_json(self.base + "/reset")
                return
            except OSError:
                time.sleep(0.1)
        raise RuntimeError("test server did not start")

    def reset(self):
        http_json(self.base + "/reset")

    def stats(self) -> dict:
        return http_json(self.base + "/stats")

    def close(self):
        self.proc.kill()
        self.proc.wait()


def run_weft_cli(url: str, out: Path, extra: list[str] | None = None) -> dict:
    t0 = time.perf_counter()
    p = subprocess.Popen(
        [str(WEFT), "get", url, "-o", str(out), "-q", "--json", "--give-up", "120", *(extra or [])],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
    )
    mon = Monitor(p.pid)
    stdout, _ = p.communicate(timeout=900)
    cpu, rss = mon.stop()
    wall = time.perf_counter() - t0
    last = json.loads(stdout.strip().splitlines()[-1])
    if last.get("result") != "completed":
        return {"ok": False, "error": last.get("error", "failed")}
    return {"ok": True, "seconds": last["seconds"], "process_seconds": wall, "bytes": last["size"], "cpu_seconds": cpu, "peak_rss": rss}


def start_weft_app(state: Path, out: Path) -> tuple[subprocess.Popen, str, float]:
    state.mkdir(parents=True, exist_ok=True)
    settings = {
        "settings": {"download_dir": str(out), "max_concurrent": 3, "connections_per_network": 32, "speed_limit": 0,
                     "network_limits": {}, "networks": {"mode": "auto"}},
        "next_id": 1,
        "items": [],
    }
    (state / "downloads.json").write_text(json.dumps(settings))
    (state / "bridge-autostart").write_text("1")
    env = dict(os.environ, WEFT_STATE_DIR=str(state))
    t0 = time.perf_counter()
    p = subprocess.Popen([str(WEFT_APP)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            if http_json("http://127.0.0.1:47610/v1/ping", timeout=0.5).get("app") == "weft":
                break
        except OSError:
            time.sleep(0.02)
    else:
        kill_tree(p.pid)
        raise RuntimeError("Weft app did not start")
    startup = time.perf_counter() - t0
    token = (state / "bridge-token").read_text().strip()
    return p, token, startup


def run_weft_app(url: str, out: Path) -> dict:
    state = Path(tempfile.mkdtemp(prefix="weft-bench-state-"))
    p, token, startup = start_weft_app(state, out)
    try:
        time.sleep(1.5)  # let the window finish loading, as a user would
        mon = Monitor(p.pid)
        t0 = time.perf_counter()
        http_json(
            "http://127.0.0.1:47610/v1/offer",
            data=json.dumps({"url": url}).encode(),
            headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        )
        deadline = time.time() + 900
        item = None
        while time.time() < deadline:
            try:
                items = json.loads((state / "downloads.json").read_text()).get("items", [])
            except (OSError, ValueError):
                items = []
            if items and items[0]["status"] in ("completed", "failed"):
                item = items[0]
                break
            time.sleep(0.02)
        seconds = time.perf_counter() - t0
        cpu, rss = mon.stop()
        if not item or item["status"] != "completed":
            return {"ok": False, "error": (item or {}).get("error", "timed out")}
        return {"ok": True, "seconds": seconds, "bytes": item["size"], "cpu_seconds": cpu, "peak_rss": rss, "startup_seconds": startup}
    finally:
        kill_tree(p.pid)
        time.sleep(0.5)
        shutil.rmtree(state, ignore_errors=True)


def run_baseline(url: str, out: Path, extra: list[str] | None = None) -> dict:
    p = subprocess.Popen(
        ["node", str(BASELINE_DRIVER), "--url", url, "--dir", str(out), "--timeout-sec", "900", *(extra or [])],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        cwd=str(BASELINE_DRIVER.parent),
    )
    monitors: list[Monitor] = []

    def watch_stderr():
        for line in p.stderr:
            if line.startswith("PID "):
                monitors.append(Monitor(int(line.split()[1])))

    t = threading.Thread(target=watch_stderr, daemon=True)
    t.start()
    stdout, _ = p.communicate(timeout=1200)
    cpu = rss = 0
    for m in monitors:
        c, r = m.stop()
        cpu += c
        rss = max(rss, r)
    try:
        last = json.loads(stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        return {"ok": False, "error": "no result from driver"}
    if not last.get("ok"):
        return {"ok": False, "error": last.get("error", "failed")}
    return {"ok": True, "seconds": last["seconds"], "bytes": last["bytes"], "cpu_seconds": cpu, "peak_rss": rss,
            "startup_seconds": last.get("startup_seconds")}


SUBJECTS = {"weft-cli": run_weft_cli, "weft-app": run_weft_app, "baseline": run_baseline}


def summarise(runs: list[dict]) -> dict:
    good = [r for r in runs if r.get("ok")]
    if not good:
        return {"ok": 0, "runs": len(runs), "error": runs[0].get("error") if runs else "no runs"}
    med = lambda k: statistics.median([r[k] for r in good if r.get(k) is not None]) if any(r.get(k) is not None for r in good) else None
    secs = [r["seconds"] for r in good]
    return {
        "ok": len(good),
        "runs": len(runs),
        "seconds": med("seconds"),
        "seconds_min": min(secs),
        "seconds_max": max(secs),
        "mib_per_sec": statistics.median([r["bytes"] / r["seconds"] / MIB for r in good]),
        "cpu_seconds": med("cpu_seconds"),
        "peak_rss_mib": (med("peak_rss") or 0) / MIB,
        "startup_seconds": med("startup_seconds"),
        "server_requests": med("server_requests"),
        "server_peak_connections": med("server_peak_connections"),
        "overhead_percent": med("overhead_percent"),
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ip", required=True, help="a LAN address of this machine to serve on")
    ap.add_argument("--port", type=int, default=8750)
    ap.add_argument("--runs", type=int, default=3)
    ap.add_argument("--scenarios", default=",".join(SCENARIOS))
    ap.add_argument("--subjects", default=",".join(SUBJECTS))
    args = ap.parse_args()

    work = Path(tempfile.mkdtemp(prefix="weft-bench-"))
    results: dict = {"when": time.strftime("%Y-%m-%d %H:%M:%S"), "machine": {"cpu": os.cpu_count(), "ram_gib": round(psutil.virtual_memory().total / 2**30)}, "runs": args.runs, "scenarios": {}}
    try:
        for name in args.scenarios.split(","):
            size, flags, about = SCENARIOS[name]
            srv = Server(args.ip, args.port, size, flags)
            url = srv.base + "/file"
            entry = {"about": about, "size": size, "subjects": {}}
            try:
                for subject in args.subjects.split(","):
                    runs = []
                    for i in range(args.runs):
                        out = work / f"{name}-{subject}-{i}"
                        out.mkdir(parents=True)
                        srv.reset()
                        try:
                            r = SUBJECTS[subject](url, out)
                        except Exception as e:  # a crashed subject is a result, not a reason to stop
                            r = {"ok": False, "error": repr(e)}
                        time.sleep(0.3)
                        st = srv.stats()
                        if r.get("ok"):
                            r["server_requests"] = st["requests"]
                            r["server_peak_connections"] = st["peak_active"]
                            r["overhead_percent"] = max(0.0, (st["body_bytes"] - r["bytes"]) * 100 / r["bytes"])
                        runs.append(r)
                        print(f"{name:20s} {subject:9s} run {i + 1}: " + (f"{r['seconds']:.2f}s cpu {r['cpu_seconds']:.1f}s rss {r['peak_rss'] / MIB:.0f} MiB req {r['server_requests']}" if r.get("ok") else f"FAILED {r.get('error')}"), flush=True)
                        shutil.rmtree(out, ignore_errors=True)
                    entry["subjects"][subject] = {"summary": summarise(runs), "runs": runs}
            finally:
                srv.close()
            results["scenarios"][name] = entry
    finally:
        shutil.rmtree(work, ignore_errors=True)

    outdir = ROOT / "bench" / "results"
    outdir.mkdir(exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    (outdir / f"{stamp}.json").write_text(json.dumps(results, indent=1))
    lines = [f"Benchmark {results['when']} (median of {args.runs} runs)", ""]
    lines.append("| Scenario | Subject | Time (s) | Range | MiB/s | CPU (s) | Peak RAM (MiB) | Requests | Peak conns | Extra bytes sent |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|")
    for name, entry in results["scenarios"].items():
        for subject, data in entry["subjects"].items():
            s = data["summary"]
            if not s["ok"]:
                lines.append(f"| {name} | {subject} | failed: {s.get('error')} | | | | | | | |")
                continue
            lines.append(
                f"| {name} | {subject} | {s['seconds']:.2f} | {s['seconds_min']:.2f}-{s['seconds_max']:.2f} | {s['mib_per_sec']:.1f} | {s['cpu_seconds']:.1f} | {s['peak_rss_mib']:.0f} | {s['server_requests']:.0f} | {s['server_peak_connections']:.0f} | {s['overhead_percent']:.1f}% |"
            )
    (outdir / f"{stamp}.md").write_text("\n".join(lines) + "\n")
    print("\n".join(lines))


if __name__ == "__main__":
    sys.exit(main())
