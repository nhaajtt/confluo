"""Abuses one download until it is done: the downloader is hard-killed and the
server taken away, again and again, at random moments.

    bench/.venv/Scripts/python bench/soak.py --ip <LAN address> --events 12

Passes when the finished file has the SHA-256 of an undisturbed download. Also
reports how many bytes the server had to send beyond the size of the file.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import random
import shutil
import subprocess
import tempfile
import time
from pathlib import Path

import bench

SIZE = "1g"
SIZE_BYTES = 1 << 30
SERVER_FLAGS = ["--rate-total", "40m"]


def sha(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ip", required=True)
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--events", type=int, default=12)
    ap.add_argument("--seed", type=int, default=1)
    args = ap.parse_args()
    rng = random.Random(args.seed)
    work = Path(tempfile.mkdtemp(prefix="weft-soak-"))
    ref_dir, out = work / "ref", work / "out"
    ref_dir.mkdir()
    out.mkdir()
    srv = bench.Server(args.ip, args.port, SIZE, [])
    url = srv.base + "/file"
    sent = 0
    log = []
    try:
        ref = bench.run_weft_cli(url, ref_dir)
        assert ref["ok"], ref
        want = sha(next(ref_dir.iterdir()))
        srv.close()
        srv = bench.Server(args.ip, args.port, SIZE, SERVER_FLAGS)
        cmd = [str(bench.WEFT), "get", url, "-o", str(out), "--json", "--give-up", "120"]
        t0 = time.perf_counter()
        events = 0
        result = None
        while result is None:
            p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
            wait = rng.uniform(0.3, 3.5)
            kind = rng.choice(["kill-client", "kill-client", "server-outage"]) if events < args.events else "none"
            started = time.perf_counter()
            done = 0
            for line in p.stdout:
                d = json.loads(line)
                if "result" in d:
                    result = d
                    break
                done = d.get("done", done)
                if kind != "none" and time.perf_counter() - started >= wait:
                    break
            if result is not None:
                break
            events += 1
            if kind == "kill-client":
                bench.kill_tree(p.pid)
                p.wait()
                log.append({"event": kind, "at_mib": round(done / bench.MIB, 1)})
            else:
                # The whole server process dies; the downloader keeps running and must
                # carry on by itself when a new server appears on the same address.
                sent += srv.stats()["body_bytes"]
                srv.close()
                time.sleep(rng.uniform(1.0, 4.0))
                srv = bench.Server(args.ip, args.port, SIZE, SERVER_FLAGS)
                for line in p.stdout:
                    d = json.loads(line)
                    if "result" in d:
                        result = d
                        break
                    if d.get("done", 0) > done + 8 * bench.MIB:
                        break
                recovered = result is not None or d.get("done", 0) > done
                log.append({"event": kind, "at_mib": round(done / bench.MIB, 1), "recovered_without_restart": recovered})
                if result is None:
                    # Recovery seen. The next round starts a fresh process, so this one goes.
                    bench.kill_tree(p.pid)
                    p.wait()
            print(log[-1], flush=True)
        seconds = time.perf_counter() - t0
        sent += srv.stats()["body_bytes"]
        ok = result.get("result") == "completed"
        intact = ok and sha(Path(result["path"])) == want
        leftovers = [f.name for f in out.iterdir() if ".weft" in f.name]
        summary = {
            "completed": ok,
            "intact": intact,
            "events": events,
            "seconds": round(seconds, 1),
            "refetched_mib": round(max(0, sent - SIZE_BYTES) / bench.MIB, 1),
            "refetched_percent": round(max(0, sent - SIZE_BYTES) * 100 / SIZE_BYTES, 2),
            "leftover_files": leftovers,
            "error": result.get("error"),
        }
        print(json.dumps(summary))
        outdir = bench.ROOT / "bench" / "results"
        outdir.mkdir(exist_ok=True)
        (outdir / f"soak-{time.strftime('%Y%m%d-%H%M%S')}.json").write_text(json.dumps({"summary": summary, "events": log}, indent=1))
        raise SystemExit(0 if intact and not leftovers else 1)
    finally:
        srv.close()
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
