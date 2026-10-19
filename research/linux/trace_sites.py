#!/usr/bin/env python3
"""Identify fail-nth injection sites one N at a time.

For every target/N in a mass campaign, boot the guest once with dw.n=N and
failslab/fail_page_alloc verbose=2, so the serial log contains exactly one
FAULT_INJECTION stack dump.  The dump is reduced to the fault capability and
the first few kernel frames.  Output: trace_sites.csv.
"""
import argparse
import csv
import datetime as dt
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
KV = re.compile(r"([A-Za-z0-9_]+)=(\S*)")
FRAME = re.compile(r"^\s+\??([A-Za-z_][A-Za-z0-9_.]*)\+0x")


def kv(line):
    return dict(KV.findall(line))


def strip_ts(line):
    m = re.match(r"^\[\s*[0-9.]+\]\s?(.*)$", line)
    return m.group(1) if m else line


def parse_log(path):
    raw = path.read_text(errors="replace").splitlines()
    lines = [strip_ts(x) for x in raw]
    blocks = []
    for i, line in enumerate(lines):
        if "FAULT_INJECTION: forcing a failure" not in line:
            continue
        cap = ""
        frames = []
        for j in range(i + 1, min(i + 60, len(lines))):
            l = lines[j]
            if l.startswith("name "):
                cap = l[5:].split(",")[0].strip()
            m = FRAME.match(l)
            if m:
                name = m.group(1)
                if name.startswith("?") or name in (
                        "dump_stack_lvl", "dump_stack", "should_fail_ex.cold",
                        "should_failslab", "should_fail_alloc_page"):
                    continue
                if name not in frames:
                    frames.append(name)
            if "entry_SYSCALL_64" in l or "Kernel panic" in l:
                break
        blocks.append((cap, frames[:6]))
    trial = {}
    for line in lines:
        if line.startswith("TRACE_ONE target="):
            trial = kv(line)
    return blocks, trial


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mass-dir", required=True,
                    help="mass run directory with trials.csv")
    ap.add_argument("--targets", default="tls_tx,tls_rx,overlay_copyup")
    ap.add_argument("--outdir", default="")
    ap.add_argument("--kernel", default="")
    ap.add_argument("--reparse", default="",
                    help="parse an existing trace-n directory, do not boot")
    args = ap.parse_args()

    mass = Path(args.mass_dir)
    rows = list(csv.DictReader(open(mass / "trials.csv")))
    maxn = {}
    for r in rows:
        if r.get("mech") != "failnth":
            continue
        t = r["target"]
        try:
            n = int(r["site"])
        except (KeyError, ValueError):
            continue
        maxn[t] = max(maxn.get(t, 0), n)
    targets = [t.strip() for t in args.targets.split(",") if t.strip()]
    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    outdir = Path(args.outdir) if args.outdir else ROOT / "logs" / f"{stamp}-trace-n"
    if args.reparse:
        outdir = Path(args.reparse)
    outdir.mkdir(parents=True, exist_ok=True)
    out_rows = []
    for target in targets:
        if target not in maxn:
            print(f"skip {target}: no failnth trials in mass csv", file=sys.stderr)
            continue
        for n in range(1, maxn[target] + 1):
            sub = outdir / f"{target}-n{n}"
            sub.mkdir(exist_ok=True)
            if not args.reparse:
                cmd = [str(ROOT / "run.py"), "--targets", target, "--reps", "1",
                       "--mode", "trace", "--dw-n", str(n),
                       "--outdir", str(sub), "--timeout", "60"]
                if args.kernel:
                    cmd += ["--kernel", args.kernel]
                print(f"+ {target} n={n}", flush=True)
                subprocess.run(cmd, check=False,
                               stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL)
            logs = list(sub.glob("*.log"))
            if not logs:
                out_rows.append({"target": target, "n": n,
                                 "cap": "", "frames": "",
                                 "identified": 0})
                continue
            blocks, trial = parse_log(logs[0])
            cap, frames = (blocks[0] if blocks else ("", []))
            out_rows.append({
                "target": target,
                "n": n,
                "cap": cap,
                "frames": " <- ".join(frames),
                "identified": 1 if blocks else 0,
                "ret": trial.get("ret", ""),
                "errno": trial.get("errno", ""),
                "log": str(logs[0]),
            })
            print(f"  cap={cap} frames={' <- '.join(frames[:3])}", flush=True)
    path = outdir / "trace_sites.csv"
    with open(path, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=["target", "n", "cap", "frames",
                                          "identified", "ret", "errno", "log"])
        w.writeheader()
        w.writerows(out_rows)
    print(f"wrote {path} ({sum(int(r['identified']) for r in out_rows)}"
          f"/{len(out_rows)} identified)")


if __name__ == "__main__":
    main()
