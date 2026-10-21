#!/usr/bin/env python3
"""Host runner for the V4-1 real multicast delivery test.

Builds the standalone init (dw_mcast_delivery.c), makes an initramfs, and
boots the three kernels in isolated QEMU/KVM guests.  One boot runs every
scenario; each scenario runs in a fresh child with fresh sockets.
"""

import argparse
import datetime as dt
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent
WORK = Path.home() / "dropwise-linux-r4"
BUILD = ROOT / "build"
QEMU = shutil.which("qemu-system-x86_64")

KERNELS = [
    ("v619-stock", Path.home() / "dropwise-linux-work/build/arch/x86/boot/bzImage"),
    ("v73-stock", WORK / "bzImage-stock"),
    ("v73-fixed", WORK / "bzImage-fixed"),
]


def run(cmd, **kw):
    print("+ " + " ".join(str(c) for c in cmd), file=sys.stderr)
    subprocess.run([str(c) for c in cmd], check=True, **kw)


def build_init():
    BUILD.mkdir(parents=True, exist_ok=True)
    run(["gcc", "-O2", "-static", "-Wall", "-Wextra",
         "-Wno-unused-parameter", "-o", BUILD / "init",
         ROOT / "dw_mcast_delivery.c"])
    gen = BUILD / "gen_init_cpio"
    if not gen.exists():
        run(["gcc", "-O2", "-o", gen, WORK / "usr" / "gen_init_cpio.c"])
    spec = BUILD / "initramfs.spec"
    spec.write_text(f"""\
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
nod /dev/null 0666 0 0 c 1 3
nod /dev/kmsg 0600 0 0 c 1 11
dir /proc 0555 0 0
dir /sys 0555 0 0
dir /tmp 01777 0 0
file /init {BUILD / 'init'} 0755 0 0
""")
    initramfs = BUILD / "initramfs.cpio.gz"
    with open(initramfs, "wb") as f:
        p1 = subprocess.Popen([str(gen), str(spec)], stdout=subprocess.PIPE)
        p2 = subprocess.Popen(["gzip", "-1", "-c"], stdin=p1.stdout, stdout=f)
        p1.stdout.close()
        if p2.wait() or p1.wait():
            sys.exit("initramfs build failed")
    return initramfs


def valid_run(text):
    """A finished boot is usable only if every scenario and control ran.

    DELIVERY_MISS is an experimental finding, not a harness failure.
    """
    if not re.search(r"^MCAST_DONE\b", text, re.M):
        return False
    if re.search(r"^(?:NET_FAIL|MOUNT_FAIL|KNOB_FAIL|HARNESS_ERROR|CONTROL_FAIL|SCENARIO_ERROR)\b", text, re.M):
        return False
    ended = re.findall(r"^SCENARIO_END name=(\S+) status=(\d+)\b", text, re.M)
    expected = {"normal", "n1", "n2", "n2_cross", "ssm_n2", "invalid", "two_sockets"}
    return (len(ended) == len(expected) and {name for name, _ in ended} == expected
            and all(status == "0" for _, status in ended))


def boot(label, kernel, initramfs, outdir, timeout):
    logpath = outdir / f"{label}.log"
    cmd = [
        QEMU, "-machine", "q35,accel=kvm", "-cpu", "host",
        "-smp", "2", "-m", "1024",
        "-kernel", str(kernel), "-initrd", str(initramfs),
        "-append",
        f"console=ttyS0 panic=-1 oops=panic quiet loglevel=4 mcast.label={label}",
        "-display", "none", "-serial", "stdio", "-monitor", "none",
        "-no-reboot",
    ]
    (outdir / f"{label}.cmd").write_text(
        "date=" + dt.datetime.now().astimezone().isoformat() + "\n" +
        "cmd=" + " ".join(str(c) for c in cmd) + "\n")
    (outdir / f"{label}.meta").write_text(
        f"kernel={kernel}\n"
        f"start={dt.datetime.now().astimezone().isoformat()}\n")
    start = time.time()
    with open(logpath, "w", errors="replace") as log:
        proc = subprocess.Popen([str(c) for c in cmd], stdout=log,
                                stderr=subprocess.STDOUT)
        done = False
        try:
            while True:
                if proc.poll() is not None:
                    done = "MCAST_DONE" in logpath.read_text(errors="replace")
                    break
                if time.time() - start > timeout:
                    proc.send_signal(signal.SIGTERM)
                    try:
                        proc.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                    break
                try:
                    tail = logpath.read_text(errors="replace")[-32768:]
                except OSError:
                    tail = ""
                if "MCAST_DONE" in tail:
                    done = True
                    for _ in range(20):
                        if proc.poll() is not None:
                            break
                        time.sleep(0.1)
                    if proc.poll() is None:
                        proc.terminate()
                    break
                time.sleep(0.1)
        finally:
            if proc.poll() is None:
                proc.kill()
                proc.wait()
    done = done and valid_run(logpath.read_text(errors="replace"))
    elapsed = time.time() - start
    with open(outdir / f"{label}.meta", "a") as f:
        f.write(f"end={dt.datetime.now().astimezone().isoformat()}\n"
                f"wall_s={elapsed:.2f}\ndone={int(done)}\n")
    print(f"[{label}] done={int(done)} wall={elapsed:.2f}s log={logpath}")
    return done, logpath


def summarize(outdir):
    for path in sorted(outdir.glob("*.log")):
        label = path.stem
        for raw in path.read_text(errors="replace").splitlines():
            if raw.startswith(("DELIVERY ", "SCENARIO_END ", "STATE ",
                               "KERNEL ", "NET ", "MCAST_DONE")):
                print(f"{label}: {raw}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--timeout", type=int, default=90)
    ap.add_argument("--outdir", help="New output directory (must not already exist)")
    args = ap.parse_args()
    if args.outdir:
        outdir = Path(args.outdir)
        outdir.mkdir(parents=True, exist_ok=False)
    else:
        parent = REPO / "logs"
        parent.mkdir(parents=True, exist_ok=True)
        outdir = Path(tempfile.mkdtemp(prefix="mcast-review-", dir=parent))
    if not QEMU:
        sys.exit("qemu-system-x86_64 not found")
    initramfs = build_init()
    results = []
    for label, kernel in KERNELS:
        if not kernel.exists():
            sys.exit(f"kernel not found: {kernel}")
        done, _ = boot(label, kernel, initramfs, outdir, args.timeout)
        results.append((label, done))
    summarize(outdir)
    print("\nboots:")
    for label, done in results:
        print(f"  {label}: done={int(done)}")
    sys.exit(0 if all(d for _, d in results) else 1)


if __name__ == "__main__":
    main()
