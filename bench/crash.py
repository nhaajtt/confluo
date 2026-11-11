"""Hard-kills each downloader half way through and measures what the kill cost.

    bench/.venv/Scripts/python bench/crash.py --ip <LAN address> --runs 3

The server counts every byte it sends. Whatever it sent beyond the size of the
file was fetched twice (or was in flight when the process died).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

import bench

SIZE = 200 * bench.MIB
KILL_AT = 100 * bench.MIB


def sha(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def weft(url: str, out: Path) -> dict:
    cmd = [str(bench.WEFT), "get", url, "-o", str(out), "--json", "--give-up", "120"]
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    done_at_kill = 0
    for line in p.stdout:
        d = json.loads(line)
        if d.get("done", 0) >= KILL_AT:
            done_at_kill = d["done"]
            bench.kill_tree(p.pid)
            break
    p.wait()
    t0 = time.perf_counter()
    again = subprocess.run(cmd + ["-q"], capture_output=True, text=True, timeout=600)
    last = json.loads(again.stdout.strip().splitlines()[-1])
    if last.get("result") != "completed":
        return {"ok": False, "error": last.get("error", "resume failed")}
    return {"ok": True, "file": last["path"], "done_at_kill": done_at_kill, "resume_seconds": time.perf_counter() - t0}


def baseline(url: str, out: Path) -> dict:
    p = subprocess.run(
        ["node", str(bench.BASELINE_DRIVER), "--url", url, "--dir", str(out), "--timeout-sec", "600", "--kill-at-bytes", str(KILL_AT)],
        capture_output=True,
        text=True,
        cwd=str(bench.BASELINE_DRIVER.parent),
        timeout=900,
    )
    try:
        last = json.loads(p.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        return {"ok": False, "error": "no result from driver"}
    if not last.get("ok"):
        return {"ok": False, "error": last.get("error", "failed")}
    return {"ok": True, "file": last["file"], "done_at_kill": KILL_AT, "resumed": last.get("resumed")}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ip", required=True)
    ap.add_argument("--port", type=int, default=8760)
    ap.add_argument("--runs", type=int, default=3)
    args = ap.parse_args()
    srv = bench.Server(args.ip, args.port, "200m", ["--rate-total", "20m"])
    work = Path(tempfile.mkdtemp(prefix="weft-crash-"))
    rows = []
    try:
        # The reference copy, fetched once without any interruption.
        ref_dir = work / "ref"
        ref_dir.mkdir()
        ref = bench.run_weft_cli(srv.base + "/file", ref_dir)
        assert ref["ok"], ref
        want = sha(next(ref_dir.iterdir()))
        for name, fn in (("weft-cli", weft), ("baseline", baseline)):
            for i in range(args.runs):
                out = work / f"{name}-{i}"
                out.mkdir()
                srv.reset()
                try:
                    r = fn(srv.base + "/file", out)
                except Exception as e:
                    r = {"ok": False, "error": repr(e)}
                time.sleep(0.5)
                sent = srv.stats()["body_bytes"]
                if r.get("ok"):
                    r["intact"] = sha(Path(r["file"])) == want
                    r["refetched_mib"] = max(0, sent - SIZE) / bench.MIB
                r["subject"] = name
                rows.append(r)
                print(name, i + 1, {k: (round(v, 2) if isinstance(v, float) else v) for k, v in r.items() if k != "file"}, flush=True)
                shutil.rmtree(out, ignore_errors=True)
    finally:
        srv.close()
        shutil.rmtree(work, ignore_errors=True)
    outdir = bench.ROOT / "bench" / "results"
    outdir.mkdir(exist_ok=True)
    (outdir / f"crash-{time.strftime('%Y%m%d-%H%M%S')}.json").write_text(json.dumps(rows, indent=1))


if __name__ == "__main__":
    main()
