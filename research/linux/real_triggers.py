#!/usr/bin/env python3
"""Run the round-3 real-trigger probes in the QEMU/KVM guest.

These are not fault-injection targets: they try to make the wakeup-source
allocation fail through memcg limits, a pending fatal signal, sysfs/device
registration pressure, or a missing capability.  The in-guest harness prints
PROBE_* lines; this script only boots it and prints the log tail.

Usage:
    ./real_triggers.py --kernel ~/dropwise-linux-work73/build-memcg/.../bzImage \
        --probe probe_memcg
"""

import argparse
import datetime as dt
import sys
from pathlib import Path

import run


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--kernel", required=True)
    ap.add_argument("--probe", action="append", required=True,
                    help="probe name, repeatable")
    ap.add_argument("--outdir", default="")
    ap.add_argument("--timeout", type=int, default=300)
    ap.add_argument("--policy", default="",
                    help="host SELinux binary policy to put at /policy.bin")
    args = ap.parse_args()

    kernel = Path(args.kernel).expanduser()
    if not kernel.exists():
        sys.exit(f"kernel not found: {kernel}")

    extra = []
    if args.policy:
        policy = Path(args.policy).expanduser()
        if not policy.exists():
            sys.exit(f"policy not found: {policy}")
        extra.append(("/policy.bin", policy, "0644"))

    run.build_harness()
    run.build_initramfs(extra)

    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    outdir = (Path(args.outdir) if args.outdir
              else run.ROOT / "logs" / f"{stamp}-probes")
    outdir.mkdir(parents=True, exist_ok=True)

    bad = 0
    for probe in args.probe:
        logpath = outdir / f"{probe}.log"
        print(f"== {probe} ==", flush=True)
        done, wall, cmd = run.boot_one(probe, "mass", logpath, kernel,
                                       args.timeout)
        print(f"done={int(done)} wall={wall:.2f}s log={logpath}")
        print("cmd: " + " ".join(str(c) for c in cmd))
        text = logpath.read_text(errors="replace").splitlines()
        for line in text:
            if line.startswith(("PROBE", "HARNESS_ERROR", "MOUNT_FAIL",
                                "KNOB_FAIL", "CMDLINE", "HARNESS_DONE")):
                print(line)
        if not done:
            bad += 1
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
