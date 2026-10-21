#!/usr/bin/env python3
"""Host-side runner for the Dropwise Linux fail-nth prototype.

Builds the in-VM harness and an initramfs, boots a QEMU/KVM guest with the
fault-injection kernel, captures the serial console, and turns the harness's
line-oriented output into trial/summary CSVs.

The kernel and its build tree are expected outside this repository, by default
under ~/dropwise-linux-work (see README.md).
"""

import argparse
import csv
import datetime as dt
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
WORK = Path.home() / "dropwise-linux-work"
DEFAULT_KERNEL = WORK / "build" / "arch" / "x86" / "boot" / "bzImage"
GEN_INIT_CPIO = WORK / "build" / "gen_init_cpio"
LINUX_SRC = WORK / "linux"
BUILD = ROOT / "build"
HARNESS_SRC = ROOT / "harness" / "dw_harness.c"
HARNESS = BUILD / "harness"
INITRAMFS = BUILD / "initramfs.cpio.gz"

ALL_TARGETS = [
    "scm_rights",
    "pipe_write",
    "tmpfs_pwrite",
    "epoll_ctl_add",
    "fcntl_setlk",
    "accept4_unix",
    "tls_tx",
    "tls_rx",
    "overlay_copyup",
    "mq_send_full",
    "pipe_sig_block",
    "pipe_sig_partial",
    "epoll_mod_wakeup",
    "epoll_add_wakeup",
    "epoll_mod_wakeup_ep",
    # round 4
    "mmap_fixed_replace",
    "mremap_fixed_dst",
    "mprotect_multi",
    "tmpfs_setxattr_replace",
    "tmpfs_rename_exchange",
    "tmpfs_fallocate",
    "pipe_set_size",
    "so_attach_filter",
    "ip_add_membership",
    "semtimedop_eintr",
    "fcntl_setlkw_eintr",
]

QEMU = shutil.which("qemu-system-x86_64")


def run(cmd, **kw):
    print("+ " + " ".join(str(c) for c in cmd), file=sys.stderr)
    return subprocess.run([str(c) for c in cmd], check=True, **kw)


def build_harness():
    BUILD.mkdir(parents=True, exist_ok=True)
    run([
        "gcc", "-O2", "-static", "-Wall", "-Wextra",
        "-Wno-unused-parameter", "-o", HARNESS, HARNESS_SRC,
    ])


def build_initramfs(extra_files=None):
    if not GEN_INIT_CPIO.exists():
        if not (LINUX_SRC / "usr" / "gen_init_cpio.c").exists():
            sys.exit(f"missing {GEN_INIT_CPIO} and {LINUX_SRC}/usr/gen_init_cpio.c")
        run(["gcc", "-O2", "-o", GEN_INIT_CPIO,
             LINUX_SRC / "usr" / "gen_init_cpio.c"])
    spec = BUILD / "initramfs.spec"
    lines = f"""\
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
nod /dev/null 0666 0 0 c 1 3
nod /dev/kmsg 0600 0 0 c 1 11
dir /proc 0555 0 0
dir /sys 0555 0 0
dir /tmp 01777 0 0
file /init {HARNESS} 0755 0 0
"""
    for guest, host, mode in extra_files or []:
        lines += f"file {guest} {host} {mode} 0 0\n"
    spec.write_text(lines)
    with open(INITRAMFS, "wb") as out:
        p1 = subprocess.Popen([str(GEN_INIT_CPIO), str(spec)],
                              stdout=subprocess.PIPE)
        p2 = subprocess.Popen(["gzip", "-1", "-c"], stdin=p1.stdout,
                              stdout=out)
        p1.stdout.close()
        rc2 = p2.wait()
        rc1 = p1.wait()
        if rc1 or rc2:
            sys.exit("initramfs build failed")


def boot_one(target, mode, logpath, kernel, timeout, dw_n=0):
    verbose = 1 if mode == "trace" else 0
    if mode == "trace":
        loglevel = "loglevel=7"
        quiet = ""
        timeout = max(timeout, 300)
    else:
        loglevel = "loglevel=4"
        quiet = "quiet "
    append = (
        f"console=ttyS0 panic=-1 oops=panic {quiet}{loglevel} "
        f"dw.target={target} dw.verbose={verbose}"
    )
    if dw_n:
        append += f" dw.n={dw_n}"
    cmd = [
        QEMU, "-machine", "q35,accel=kvm", "-cpu", "host",
        "-smp", "2", "-m", "1024",
        "-kernel", str(kernel), "-initrd", str(INITRAMFS),
        "-append", append,
        "-display", "none", "-serial", "stdio", "-monitor", "none",
        "-no-reboot",
    ]
    logpath.parent.mkdir(parents=True, exist_ok=True)
    start = time.time()
    with open(logpath, "w", errors="replace") as log:
        proc = subprocess.Popen([str(c) for c in cmd], stdout=log,
                                stderr=subprocess.STDOUT)
        done = False
        try:
            while True:
                if proc.poll() is not None:
                    try:
                        if "HARNESS_DONE" in logpath.read_text(errors="replace"):
                            done = True
                    except OSError:
                        pass
                    break
                if time.time() - start > timeout:
                    proc.terminate()
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                    break
                try:
                    size = logpath.stat().st_size
                    with open(logpath, "rb") as lf:
                        lf.seek(max(0, size - 16384))
                        tail = lf.read().decode("utf-8", "replace")
                except OSError:
                    tail = ""
                if "HARNESS_DONE" in tail:
                    done = True
                    # let QEMU power down on its own briefly
                    for _ in range(20):
                        if proc.poll() is not None:
                            break
                        time.sleep(0.1)
                    if proc.poll() is None:
                        proc.terminate()
                    break
                time.sleep(0.15)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
    elapsed = time.time() - start
    return done, elapsed, cmd


KV = re.compile(r"([A-Za-z0-9_]+)=(\S*)")


def kv(line):
    return dict(KV.findall(line))


def parse_log(path):
    data = {
        "selftest": [], "probes": [], "bases": [], "trials": [],
        "summaries": [], "errors": [], "trace_marks": [], "done": False,
        "ff_list": [], "ff_reg": [], "sigsites": [],
    }
    pre = post = None
    stable = None
    for raw in Path(path).read_text(errors="replace").splitlines():
        line = raw.strip()
        if "DWTRACE" in line:
            data["trace_marks"].append(line)
        if line.startswith("SELFTEST_SOCKETPAIR"):
            data["selftest"].append(kv(line))
        elif line.startswith("SELFTEST_OK"):
            data["selftest"].append(kv(line))
        elif line.startswith("SELFTEST_FAIL"):
            data["selftest"].append(kv(line))
        elif line.startswith("PROBE "):
            data["probes"].append(kv(line))
        elif line.startswith("BASE "):
            d = kv(line)
            data["bases"].append(d)
            if d.get("side") == "pre":
                pre = d.get("obs")
            elif d.get("side") == "post":
                post = d.get("obs")
        elif line.startswith("BASE_STABLE"):
            stable = kv(line).get("stable")
        elif line.startswith("TRIAL "):
            d = kv(line)
            d["baseline_pre"] = pre or ""
            d["baseline_post"] = post or ""
            d["stable"] = stable or ""
            data["trials"].append(d)
        elif line.startswith("TRIAL_END"):
            data.setdefault("trial_ends", []).append(line)
        elif line.startswith("FF_LIST "):
            data["ff_list"].append(line[8:])
        elif line.startswith("FF_REGISTER "):
            data["ff_reg"].append(kv(line))
        elif line.startswith("SUMMARY "):
            data["summaries"].append(kv(line))
        elif line.startswith(("HARNESS_ERROR", "TARGET_TIMEOUT",
                              "TARGET_WORKER_FAIL", "BASELINE_ERROR",
                              "BASELINE_HANG", "BASELINE_UNSTABLE",
                              "HARNESS_WARN", "MOUNT_FAIL", "KNOB_FAIL")):
            data["errors"].append(line)
        elif line == "HARNESS_DONE":
            data["done"] = True
    return data


def write_csvs(outdir, runs):
    trials_path = outdir / "trials.csv"
    fields = [
        "run", "mode", "target", "mech", "site", "n", "ret", "errno",
        "injected", "readback", "verdict", "stable", "obs", "baseline_pre",
        "baseline_post",
    ]
    with open(trials_path, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        w.writeheader()
        for r in runs:
            for t in r["parsed"]["trials"]:
                row = dict(t)
                row["run"] = r["run"]
                row["mode"] = r["mode"]
                row["target"] = t.get("target", r["target"])
                w.writerow(row)

    summary_path = outdir / "summary.csv"
    sfields = [
        "run", "mode", "target", "mech", "selftest_sites", "sites",
        "trials", "misses", "ok_pre", "ok_post", "ok_partial",
        "ok_poisoned", "violations", "unknown", "stable", "elapsed_ms",
        "wall_s", "done", "errors",
    ]
    with open(summary_path, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=sfields, extrasaction="ignore")
        w.writeheader()
        for r in runs:
            p = r["parsed"]
            sel = ""
            for e in p["selftest"]:
                if "socketpair_sites" in e:
                    sel = e["socketpair_sites"]
            for s in (p["summaries"] or [{}]):
                w.writerow({
                    "run": r["run"], "mode": r["mode"],
                    "target": s.get("target", r["target"]),
                    "mech": s.get("mech", ""),
                    "selftest_sites": sel,
                    "sites": s.get("sites", ""),
                    "trials": s.get("trials", ""),
                    "misses": s.get("misses", ""),
                    "ok_pre": s.get("ok_pre", ""),
                    "ok_post": s.get("ok_post", ""),
                    "ok_partial": s.get("ok_partial", ""),
                    "ok_poisoned": s.get("ok_poisoned", ""),
                    "violations": s.get("violations", ""),
                    "unknown": s.get("unknown", ""),
                    "stable": s.get("stable", ""),
                    "elapsed_ms": s.get("elapsed_ms", ""),
                    "wall_s": f"{r['wall_s']:.2f}",
                    "done": int(p["done"]),
                    "errors": " | ".join(p["errors"]),
                })
    return trials_path, summary_path


def aggregate(runs):
    by = {}
    for r in runs:
        p = r["parsed"]
        for s in (p["summaries"] or [{}]):
            key = (s.get("target", r["target"]), s.get("mech", ""))
            a = by.setdefault(key, {
                "target": key[0], "mech": key[1], "reps": 0, "done": 0,
                "stable": 0, "sites": 0, "trials": 0, "misses": 0,
                "ok_pre": 0, "ok_post": 0, "ok_partial": 0,
                "ok_poisoned": 0, "violations": 0, "unknown": 0,
                "wall": 0.0, "errors": [],
            })
            a["reps"] += 1
            a["done"] += int(p["done"])
            a["stable"] += int(s.get("stable", "0") == "1")
            for k in ("sites", "trials", "misses", "ok_pre", "ok_post",
                      "ok_partial", "ok_poisoned", "violations", "unknown"):
                a[k] += int(s.get(k, 0) or 0)
            a["wall"] += r["wall_s"]
            a["errors"].extend(p["errors"])
    return by


def print_aggregate(by, mode):
    print(f"\n{mode} results (sum over repeated boots, per target x mechanism)")
    print(f"{'target':18} {'mechanism':9} {'reps':>4} {'done':>4} {'stable':>6} "
          f"{'sites':>5} {'trials':>6} {'miss':>4} {'ok_pre':>6} {'ok_post':>7} "
          f"{'ok_part':>7} {'ok_pois':>7} {'viol':>5} {'unknown':>7} {'wall_s':>7}")
    for (target, mech), a in by.items():
        print(f"{target:18} {mech:9} {a['reps']:4d} {a['done']:4d} "
              f"{a['stable']:6d} {a['sites']:5d} {a['trials']:6d} "
              f"{a['misses']:4d} {a['ok_pre']:6d} {a['ok_post']:7d} "
              f"{a['ok_partial']:7d} {a['ok_poisoned']:7d} "
              f"{a['violations']:5d} {a['unknown']:7d} {a['wall']:7.2f}")
    errs = [(t, e) for (t, m), a in by.items() for e in a["errors"]]
    if errs:
        print("\nerrors/warnings:")
        for t, e in errs:
            print(f"  {t}: {e}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--kernel", default=str(DEFAULT_KERNEL))
    ap.add_argument("--targets", default="all",
                    help="comma list or 'all'")
    ap.add_argument("--mode", choices=["mass", "trace"], default="mass")
    ap.add_argument("--reps", type=int, default=1)
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--outdir", default="")
    ap.add_argument("--dw-n", type=int, default=0,
                    help="run exactly this fail-nth N (trace one site)")
    args = ap.parse_args()

    kernel = Path(args.kernel).expanduser()
    if not kernel.exists():
        sys.exit(f"kernel not found: {kernel} (see README.md)")
    if not QEMU:
        sys.exit("qemu-system-x86_64 not found")
    build_harness()
    build_initramfs()

    targets = ALL_TARGETS if args.targets == "all" else [
        t.strip() for t in args.targets.split(",") if t.strip()
    ]
    unknown = [t for t in targets if t not in ALL_TARGETS]
    if unknown:
        sys.exit(f"unknown targets: {unknown}")

    stamp = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
    outdir = Path(args.outdir) if args.outdir else ROOT / "logs" / f"{stamp}-{args.mode}"
    outdir.mkdir(parents=True, exist_ok=True)
    runs = []
    rid = 0
    for rep in range(args.reps):
        for target in targets:
            rid += 1
            logpath = outdir / f"{rid:02d}-{target}-rep{rep+1}.log"
            done, wall, cmd = boot_one(target, args.mode, logpath, kernel,
                                       args.timeout, args.dw_n)
            parsed = parse_log(logpath)
            runs.append({
                "run": rid, "rep": rep + 1, "target": target,
                "mode": args.mode, "log": str(logpath),
                "wall_s": wall, "done": done, "parsed": parsed,
            })
            for s in (parsed["summaries"] or [{}]):
                print(f"[{rid:02d}] {s.get('target', target):18} "
                      f"mech={s.get('mech','-'):8} rep={rep+1} "
                      f"done={int(done)} stable={s.get('stable','?')} "
                      f"sites={s.get('sites','?')} "
                      f"trials={s.get('trials','?')} "
                      f"pois={s.get('ok_poisoned','?')} "
                      f"viol={s.get('violations','?')} "
                      f"unknown={s.get('unknown','?')} wall={wall:.1f}s")
            if not done:
                print(f"      incomplete log: {logpath}", file=sys.stderr)

    write_csvs(outdir, runs)
    by = aggregate(runs)
    print_aggregate(by, args.mode)
    (outdir / "aggregate.csv").write_text(
        "target,mech,reps,done,stable,sites,trials,misses,ok_pre,ok_post,"
        "ok_partial,ok_poisoned,violations,unknown,wall_s\n" +
        "\n".join(
            f"{a['target']},{a['mech']},{a['reps']},{a['done']},"
            f"{a['stable']},{a['sites']},{a['trials']},{a['misses']},"
            f"{a['ok_pre']},{a['ok_post']},{a['ok_partial']},"
            f"{a['ok_poisoned']},{a['violations']},{a['unknown']},"
            f"{a['wall']:.2f}"
            for _, a in by.items()
        ) + "\n"
    )
    print(f"\nlogs: {outdir}")
    bad = [r for r in runs if not r["done"]]
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
