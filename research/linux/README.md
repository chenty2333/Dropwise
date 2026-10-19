# Linux syscall handoff-atomicity prototype

A feasibility prototype that applies Dropwise's judgement to Linux syscalls:
after a syscall **P** is interrupted by an injected allocation failure, does the
shared object left for the next user look like "P never ran", "P completed", or
exactly the partial state its interface allows?

The prototype runs in a QEMU/KVM guest with a purpose-built 6.19 kernel and
`CONFIG_FAULT_INJECTION`.  It uses the per-task `fail-nth` interface, two
calibrated baselines, a forked survivor on the same shared object, and an
independent observer.  See [RESULTS.md](RESULTS.md) for what was measured and
[PRIVATE-NOTES.md](PRIVATE-NOTES.md) for the private status line.

## Layout

```
harness/dw_harness.c        in-guest harness and target definitions
harness/Makefile            builds build/harness (static)
run.py                      host runner: initramfs, QEMU, log/CSV parsing
prepare_kernel.sh           detached kernel worktree + config + bzImage
kernel.config.fragment      reference config fragment
logs/                       raw serial logs and CSVs from the reported runs
build/                      local build products (harness, initramfs, spec)
```

Kernel source and its object tree are **outside** this repository, by default
under `~/dropwise-linux-work/` as required for this prototype.

## Host and kernel

Measured host: 16 cores, 30 GB RAM, `/dev/kvm`, QEMU 10.2.2, Fedora 44, gcc
with static glibc.  The guest kernel is Linux 6.19.0, built from tag `v6.19`
(commit `05f7e89ab973`) in a **detached** worktree created from the existing
main kernel repository (`/home/ava/Desktop/linux/linux`).  No existing
worktree, branch or commit under `~/Worktrees/linux/` was modified; the new
worktree is `~/dropwise-linux-work/linux`.

Build:

```sh
research/linux/prepare_kernel.sh
# -> ~/dropwise-linux-work/build/arch/x86/boot/bzImage
```

The script does `make O=~/dropwise-linux-work/build defconfig`, applies the
options in [kernel.config.fragment](kernel.config.fragment) with
`scripts/config`, runs `olddefconfig`, and builds `bzImage` with `-j$(nproc)`.
The build used about 1.7 GB for the source worktree and 0.8 GB for the object
tree on this host and took about three minutes with 16 cores.

The important options are `CONFIG_FAULT_INJECTION`, `CONFIG_FAILSLAB`,
`CONFIG_FAIL_PAGE_ALLOC`, `CONFIG_FAULT_INJECTION_DEBUG_FS` and
`CONFIG_DEBUG_FS`.  `CONFIG_FAIL_FUTEX` and the 9p/virtio options are enabled as
requested but are not needed by the prototype: the guest boots from an external
initramfs with no disk, and the harness does not use futexes.

### The runtime filters matter

The capability defaults filter out exactly the allocations this prototype
needs: `failslab.ignore_gfp_reclaim` and `fail_page_alloc.ignore_gfp_reclaim`
default to true, `fail_page_alloc.ignore_gfp_highmem` defaults to true, and
`fail_page_alloc.min_order` defaults to 1, so order-0 pages and
`__GFP_DIRECT_RECLAIM`/`__GFP_HIGHMEM` allocations never reach
`should_fail()`.  The harness writes these knobs before any experiment:

```
/sys/kernel/debug/failslab/ignore-gfp-wait             0
/sys/kernel/debug/failslab/cache-filter                0
/sys/kernel/debug/fail_page_alloc/ignore-gfp-wait      0
/sys/kernel/debug/fail_page_alloc/ignore-gfp-highmem   0
/sys/kernel/debug/fail_page_alloc/min-order            0
```

`fail-nth` itself takes precedence over probability, interval, times and space;
the per-capability filters above still apply.

### fail-nth

The harness opens `/proc/self/task/<gettid>/fail-nth` **in the task that will
run the target syscall**, writes `N`, invokes `P`, then reads the file back.
`0` means the N-th `should_fail()` call was reached and returned true; a
positive value means it was not reached.  An inherited fd still refers to
the task that opened it, so each worker and each per-trial child reopens its
own file.  This was a real early bug (see
RESULTS.md, framework errors).

Before every campaign the harness runs a selftest: set/readback with no
syscall, set 1000 + `getpid` + readback, and the documented `socketpair()`
enumeration.  On this kernel the socketpair path showed 14–15 injectable sites
across boots.

## Running

```sh
cd research/linux
./run.py --targets all --reps 5          # mass verdicts
./run.py --targets all --reps 1 --mode trace
```

`run.py` builds the static harness, generates the initramfs with the kernel's
`gen_init_cpio` (device nodes `/dev/console`, `/dev/null`, `/dev/kmsg`; `/init`
is the harness), boots one QEMU/KVM guest per target per repetition, and parses
the serial output.  Defaults expect the kernel at
`~/dropwise-linux-work/build/arch/x86/boot/bzImage`; use `--kernel` otherwise.

QEMU is started as:

```
qemu-system-x86_64 -machine q35,accel=kvm -cpu host -smp 2 -m 1024 \
  -kernel .../bzImage -initrd build/initramfs.cpio.gz \
  -append "console=ttyS0 panic=-1 oops=panic quiet loglevel=4 \
           dw.target=<target> dw.verbose=0" \
  -display none -serial stdio -monitor none -no-reboot
```

Each boot takes about 1.0 s wall.  Outputs go to `logs/<timestamp>-<mode>/`:
`trials.csv`, `summary.csv`, `aggregate.csv`, and one serial log per boot.
The harness itself prints `SELFTEST_*`, `PROBE`, `BASE`, `BASE_STABLE`,
`TRIAL`, `TRIAL_END`, `SUMMARY`, `TARGET_END` and `HARNESS_DONE` lines.

## Harness mechanics

`dw_harness.c` runs as PID 1.  It mounts `proc`, `sysfs`, `debugfs` and a tmpfs,
configures the filters, runs the selftest, and runs each target in a worker
process.  Every trial in `run_trial()` is isolated in a fresh child process
(`run_trial_isolated()`):

1. create the survivor-report pipe **before** `fail-nth` is armed, so its fd
   allocation cannot consume an injection point;
2. run a dry `P` once on a throwaway object to fault in the userspace stack and
   data pages (first-touch page faults are `should_fail()` calls too);
3. `setup()` a fresh shared object;
4. arm `fail-nth=N`, call `P`, read back immediately;
5. if injected, fork the survivor, let it perform its operation on the same
   object, and collect a fixed-size report over the pipe;
6. run the observer in the parent and tear the object down.

Each target provides `setup`, `P`, an optional `before_survivor`, `survivor`,
`observer`, an optional `partial_ok` predicate, and `teardown`.  Baselines are
two `pre` runs (`P` skipped) and two `post` runs (`P` not injected).  A plan is
judged by `P`'s return value:

- error → the observation must equal `pre`;
- complete success → it must equal `post`;
- short/partial → the target's `partial_ok` must accept exactly the returned
  prefix; matching `pre` or `post` instead is a violation.

Any other observation is `VIOL_*`; two baseline samples that disagree make the
whole target `UNKNOWN` and are never counted as a violation.  Observations are
canonical strings with raw fds, pids and inode numbers removed (booleans or
semantic values only), so they compare across runs.

`--mode trace` sets the two `verbose` debugfs knobs to 2 and writes a
`DWTRACE target=... n=...` marker to `/dev/kmsg` before arming.  Console traces
are useful for attributing an injection but are not used for any verdict; the
kernel's console output can interleave with userspace output and is sometimes
cut off by poweroff.

## Targets

The milestone target is `sendmsg` with `SCM_RIGHTS`; the five extensions are
pipe write, tmpfs pwrite, epoll_ctl ADD, fcntl SETLK and accept4.  Pipe,
tmpfs and epoll come from the brief's candidate directions; fcntl and accept
were added because they have a `/proc/locks` observer and a dequeue-then-fail
path worth checking, respectively.

| target | shared object | survivor | observer | declared error contract |
|---|---|---|---|---|
| `scm_rights` | AF_UNIX `socketpair`; one byte plus one fd | child `recvmsg` on the peer | FIONREAD after the receive; received fd's inode compared with the sender's | error ⇒ no byte and no fd delivered |
| `pipe_write` | pipe, 8 KiB write (two pages) | child `read` of up to 16 KiB | FIONREAD plus a second nonblocking read | error ⇒ no bytes; a short return is legal and only that prefix may appear |
| `tmpfs_pwrite` | sparse tmpfs file, 4 KiB into a hole at offset 0 | child `pwrite` of one byte at offset 0 (same page) | second `open` + `pread` 4 KiB + `fstat` (`st_size`, `st_blocks`) | error ⇒ the hole is unchanged (survivor's own byte is present) |
| `epoll_ctl_add` | epoll fd watching a pipe read end | child `epoll_wait` after the pipe is made readable | `/proc/self/fdinfo/<epfd>` tfd/events, a second `epoll_wait`, then DEL | error ⇒ fd not in the interest list |
| `fcntl_setlk` | tmpfs file, whole-file write lock | child `F_SETLK` read lock on the same inode | `/proc/locks` filtered by inode and pid | error ⇒ no lock held |
| `accept4_unix` | listening AF_UNIX socket with one queued client | child `accept4` and a one-byte token | parent `recv` on the client socket, `getsockopt(SO_ERROR)` | error ⇒ no accepted fd; queue preservation is empirical, not man-page-guaranteed |

Why these: they cover a passed fd, a page allocation under a byte-stream API, a
page-cache write, an interest-list registration with several allocation sites,
a lock holder observed outside the syscall, and a dequeue-then-fail candidate.
The full source paths and contracts are in [RESULTS.md](RESULTS.md).

## Limitations

- `fail-nth` only reaches `should_fail()` call sites: slab, page allocation and
  futex (plus other capabilities if enabled).  It does not inject invalid
  arguments, permission failures, signals, or io_uring cancellation, and it
  cannot count allocations made asynchronously by another task.
- The ordinal `N` is not a stable name for an allocation site.  The sequence
  depends on cache/fd-table state, and tracing perturbs it.  The harness makes
  the final mass runs stable with per-trial isolation and a dry warmup, but the
  stop-at-first-not-injected rule can still miss a site that appears later.
  `N` values in RESULTS.md are "the N-th injectable point in that run", not
  portable identifiers.
- The verdict speaks only about the observation the harness can see.  A zero
  page instantiated in the page cache, an unused lock context, or a queue entry
  invisible to userspace is not observed.
- The accept target fills the fd table to force an `alloc_fdtable()` site; this
  is a harness construction, not a property of ordinary callers.
- One kernel version, one architecture, one LTS-ish tag; no signal
  interruption, no `io_uring`, no real block filesystem, no network stack.

## Deviations from the brief

- Six targets total: the SCM milestone plus five extensions (the brief asked
  for 3–5 extensions).  `fcntl` and `accept4` are outside the candidate list;
  the reason is above.
- Per-trial process isolation, the dry warmup and the pre-armed report pipe are
  harness-level additions.  They do not change the judging rule; they remove
  injection points that came from the harness's own page faults and fd
  allocations.  Without them, the same `N` mapped to different sites and
  probe/site counts varied (see RESULTS.md).
- `--mode trace` is diagnostic only; trace runs are not part of the reported
  verdict counts.
- No confirmed kernel issue was found, so there are no private issue details;
  `PRIVATE-NOTES.md` contains only that status.
