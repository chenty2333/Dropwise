# Linux syscall handoff atomicity — round 4

Unfrozen exploration, round 4.  Round 1-3 results are in
`RESULTS.md` / `RESULTS-round2.md` / `RESULTS-round3.md` and were not
re-run or modified.  Everything below was produced by the existing
`harness/dw_harness.c` + `run.py` + QEMU/KVM flow, with fresh logs only.

Kernel under test (campaign): **v6.19**, commit
`05f7e89ab9731565d8a62e3b5d1ec206485eeb0b`.
Fix validation kernel: **v7.3-rc2**, commit
`df2908090cda368b01ff43709f51890076c56157`, built in a new detached
worktree `~/dropwise-linux-r4` (no other tree touched, no commit).

## 0. Summary

* 11 new executable targets in 5 subsystems (VMA, tmpfs file metadata,
  pipe/fd, socket, IPC/record-lock): 9 with `fail-nth` and 2 with the
  interrupted-wait signal mechanism.  The existing 15 targets are
  unchanged.
* 55 campaign boots / 303 trials / 0 misses / 0 harness errors / 0
  unstable baselines.  40 signal trials per signal target (4 delays x 2
  reps x 5 boots), all 40 injected (`waited=1`, `EINTR`).
* **One confirmed contract violation, new root cause**:
  `IP_ADD_MEMBERSHIP` returns 0 while the interface never got the group
  (`____ip_mc_inc_group()` kzalloc failure is discarded because the
  function returns `void`).  Socket-side membership exists, device-side
  group does not: no IGMP report, no delivery, `/proc/net/igmp` empty.
  5/5 boots at N=2.  Fixed by a 26-line patch; fixed kernel 0/5
  violations, same-tree regression otherwise identical.
* Three **contract-external** partial/destructive effects on error, kept
  as concrete counterexamples but not bugs and not fixed:
  `mmap(MAP_FIXED)` error leaves a gap (5/5 boots),
  `mremap(MREMAP_FIXED)` error leaves the destination unmapped (5/5),
  `mprotect()` across three VMAs fails after changing the first two
  (10/10 injection sites on v6.19).
* Seven clean targets (negative results with counts): tmpfs
  `setxattr` replace, tmpfs `RENAME_EXCHANGE`, tmpfs `fallocate`,
  `F_SETPIPE_SZ`, `SO_ATTACH_FILTER`, `semtimedop` + signal,
  `fcntl(F_SETLKW)` + signal.

## 1. Method

### 1.1 Harness additions

`harness/dw_harness.c`: 11 new `struct target` instances appended to
`all_targets[]` after the existing 15.  `run.py`: the same 11 names
appended to `ALL_TARGETS`.  No existing target body, table entry,
judge, runner mechanism or boot parameter changed.

New helpers: `dw_read_file()`, `dw_map_probe()` (`mincore` then content
read, so a missing VMA is reported instead of faulting),
`dw_maps_perms()` (protection of the VMA containing an address, parsed
from `/proc/self/maps`, used instead of raw addresses in observations).

Targets, declared contracts and observers:

| # | target | subsystem | P | contract (error => effect equal to not having run, unless documented) | independent observer |
|---|--------|-----------|---|----------------------------------------------------------------------|----------------------|
| 1 | `mmap_fixed_replace` | VMA | `mmap(addr,4096,RW,PRIVATE\|ANON\|FIXED)` over a populated page | success gives the new zero page; error must leave the old mapping | survivor + parent `mincore`/byte |
| 2 | `mremap_fixed_dst` | VMA | `mremap(src,4096,4096,MAYMOVE\|FIXED,dst)` with dst populated | success moves src to dst; error must leave both as they were | survivor + parent probe of both |
| 3 | `mprotect_multi` | VMA | `mprotect(base,3*4096,PROT_READ)` over private/shared/private VMAs | success changes all three; error must change none | `/proc/self/maps` perms at 3 pages |
| 4 | `tmpfs_setxattr_replace` | tmpfs metadata | `fsetxattr(fd,"user.dw","BBBBBBBB",8,0)` replacing "AAAA" | success replaces the value, error keeps 4-byte "AAAA" | second `open`+`fgetxattr` |
| 5 | `tmpfs_rename_exchange` | tmpfs metadata | `renameat2(a,b,RENAME_EXCHANGE)` | success exchanges contents, error keeps both names/contents | independent read of both paths |
| 6 | `tmpfs_fallocate` | tmpfs metadata | `fallocate(fd,0,0,65536)` on an empty file | success allocates and extends, error keeps size 0 | `fstat` size/blocks + `pread` at both ends |
| 7 | `pipe_set_size` | pipe/fd | `fcntl(F_SETPIPE_SZ,131072)` on a 64 KiB pipe holding 4096 bytes | success installs the new ring and keeps the bytes, error keeps old ring | `F_GETPIPE_SZ`, `FIONREAD`, drain and count |
| 8 | `so_attach_filter` | socket | `SO_ATTACH_FILTER` (pass-all) replacing drop-all on AF_UNIX dgram | success replaces the filter, error keeps the old filter deciding delivery | peer sends, survivor/observer `recv`/`FIONREAD` |
| 9 | `ip_add_membership` | socket/IP | `IP_ADD_MEMBERSHIP` 239.1.2.3 on lo | success means the group is joined: socket membership **and** interface group | `IP_MSFILTER` (socket) + `/proc/net/igmp` (device) |
| 10 | `semtimedop_eintr` | IPC | `semtimedop` 2 ops (sem0-1, sem1-1), sem1 empty, 500 ms timeout | interrupted wait performs no operation at all | survivor + parent `GETVAL` both |
| 11 | `fcntl_setlkw_eintr` | record locks | `fcntl(F_SETLKW)` write lock blocked by an external holder | `EINTR` means no lock was acquired | holder killed, forked survivor `F_SETLK` non-blocking |

`semtimedop_eintr` and `fcntl_setlkw_eintr` are the two signal-shape
targets ("interrupted but state must not advance").  `ip_add_membership`
is the "returns success but a required effect is missing" shape.

### 1.2 Mechanisms and judging (unchanged from round 3)

* `fail-nth`: for each fresh object, `N` is written to
  `/proc/<tid>/fail-nth`, P is executed, and the value is read back;
  `readback == 0` proves the N-th `should_fail()` was consumed, so the
  trial is counted ("injection hit").  Enumeration stops at the first N
  that is not injected, or at `MAX_N`.
* Signal: P runs in a thread, the runner waits until the thread is
  sleeping (`waited=1`), then sends `SIGUSR1` with no `SA_RESTART`, at
  0/200/2000/20000 us, twice each per boot.
* Judging: fresh-object pre baseline (P skipped) and post baseline (P
  executed), 2 samples each, unstable => UNKNOWN; error => PRE; full
  success => POST; anything else is a violation.  Observation strings
  contain no pids, fds or raw addresses.
* Allocations performed by the harness (report pipe, survivor fork,
  observer) happen before arming or after disarming `fail-nth`, so they
  cannot consume an injection point.

### 1.3 Command lines and logs

```
# campaign (5 boots per fail-nth target, 5 boots / 40 trials per signal target)
python3 run.py --targets mmap_fixed_replace,mremap_fixed_dst,mprotect_multi,
  tmpfs_setxattr_replace,tmpfs_rename_exchange,tmpfs_fallocate,pipe_set_size,
  so_attach_filter,ip_add_membership,semtimedop_eintr,fcntl_setlkw_eintr \
  --reps 5 --outdir logs/20261021-r4-mass --timeout 180

# per-N single-boot trace of the confirmed violation
python3 run.py --targets ip_add_membership --mode trace --dw-n 2 \
  --outdir logs/20261021-r4-trace-n2

# existing 15 targets on v6.19 (no code change regression)
python3 run.py --targets scm_rights,pipe_write,tmpfs_pwrite,epoll_ctl_add,
  fcntl_setlk,accept4_unix,tls_tx,tls_rx,overlay_copyup,mq_send_full,
  pipe_sig_block,pipe_sig_partial,epoll_mod_wakeup,epoll_add_wakeup,
  epoll_mod_wakeup_ep --reps 1 --outdir logs/20261021-r4-regress-v619

# fix validation: same tree, stock vs patched v7.3-rc2
python3 run.py --kernel ~/dropwise-linux-r4/bzImage-stock \
  --targets ip_add_membership --reps 5 --outdir logs/20261021-r4-fixed-orig
python3 run.py --kernel ~/dropwise-linux-r4/bzImage-fixed \
  --targets ip_add_membership --reps 5 --outdir logs/20261021-r4-fixed-patched
# same-tree full regression (all 26 targets, 1 boot each)
... --kernel ~/dropwise-linux-r4/bzImage-stock  --outdir logs/20261021-r4-stockregress
... --kernel ~/dropwise-linux-r4/bzImage-fixed  --outdir logs/20261021-r4-fixedregress
```

Log directories (all new):
`logs/20261021-r4-smoke`, `-smoke2`, `-mass`, `-trace-n2`,
`-regress-v619`, `-stockregress`, `-fixedregress`, `-fixed-orig`,
`-fixed-patched`.  No round-1/2/3 log was touched.

## 2. Candidate screening

~20 candidate operations were screened before implementation.  "No
fallible step" means the state-changing commit has no allocation or
other failable operation after the old state stops being valid, so
there is nothing to inject.  "No distinguishable post" means a full
success and the error state cannot both be established on a fresh
object (needed for calibration).

| candidate | contract-relevant failure path | observable | decision |
|-----------|--------------------------------|------------|----------|
| `mmap(MAP_FIXED)` over a VMA | PTE clear + VMA detach before `vm_area_alloc`/`vma_iter_prealloc`; abort leaves a gap | mincore + page content | accepted, contract-external result (V4-2) |
| `mremap(MREMAP_FIXED)` | `do_munmap(dst)` before `copy_vma` allocations | both addresses | accepted, contract-external (V4-3) |
| `mprotect()` multi-VMA | loop applies VMA 1..k-1 before VMA k split allocation fails | maps perms per page | accepted, contract-external (V4-4/5) |
| tmpfs `setxattr` replace | `simple_xattr_alloc`+`kstrdup` before write_lock/rb_replace | second `fgetxattr` | accepted, clean |
| tmpfs `RENAME_EXCHANGE` | two `mtree_store` + rollback; dirents swapped last | both names' contents | accepted, clean |
| tmpfs `fallocate` | page loop, `shmem_undo_range` rollback, i_size only on success | size/blocks/bytes | accepted, clean |
| `F_SETPIPE_SZ` | `pipe_resize_ring` kcalloc before copying/commit | size + queued bytes | accepted, clean |
| `SO_ATTACH_FILTER` replace | new prog/filter allocated before `rcu_assign_pointer` | peer delivery through filter | accepted, clean |
| `IP_ADD_MEMBERSHIP` | `sock_kmalloc(iml)` then **void** `____ip_mc_inc_group` kzalloc | `IP_MSFILTER` + `/proc/net/igmp` | accepted, **violation** (V4-1) |
| `semtimedop` + signal | `perform_atomic_semop` validates all then applies all; EINTR unlinks queue | both `GETVAL` | accepted, clean |
| `fcntl(F_SETLKW)` + signal | wait interrupted; lock installed only on grant | forked survivor `F_SETLK` | accepted, clean |
| `signalfd4` mask update | direct assignment under `siglock`, no allocation | signal delivery set | rejected: no fallible step |
| `timerfd_settime` | timers committed after validation, no allocation in commit | timer state/tick | rejected: no fallible step |
| `F_SETLEASE` replace | new `file_lock` allocated before any replace | lease owner/type | rejected: old lease never retired by the failing call |
| `dup2`/`dup3` replace | `close` cannot fail; no allocation after commit | fd contents | rejected: no fallible step |
| `inotify_add_watch` existing watch | existing watch's mask updated in place; no allocation | event delivery | rejected: no fallible step |
| `TCP_CONGESTION` | `tcp_set_congestion_control` validates/reuses before commit | `getsockopt` | rejected: no state change before error |
| `SO_RCVBUF` | direct `sk_rcvbuf` write, allocation done in advance | `getsockopt` | rejected: no fallible step |
| `SO_BINDTODEVICE` | name copied into local buffer before commit | `getsockopt` | rejected: no fallible step |
| `TCP_ULP` / TLS handshake | `tls_init` only builds global protos; per-socket unchanged on error; round 2 already covers TLS | per-socket TLS state | rejected: no new shape |
| tmpfs plain `rename` | `simple_offset_add` before `d_move`; failure happens before commit | both names | rejected: already covered by exchange target |
| tmpfs `link` | no failure after `d_instantiate` commit | both names | rejected: no fallible step after commit |
| `msgsnd`/`mq_send` + signal | to block the queue must be full, so a full-success post cannot coexist with the blocking pre | queue contents | rejected: no distinguishable post |
| `msgrcv`/`mq_timedreceive` + signal | to block there must be no message; pre and error are identical | queue contents | rejected: no distinguishable post |
| `flock` + signal | flock locks are on the open file description, shared across `fork`, so the survivor cannot distinguish "P holds" from "external holder holds" | survivor `flock` | rejected: lock owner not observable; POSIX record lock target used instead |
| `shmem_rename2` plain rename | offset add before dirent commit | both names | rejected: duplicate of exchange screening |

## 3. Results (v6.19, 5 boots per target)

303 trials, 0 misses, 0 unknown, 0 harness errors.  `sites` is the
number of injected fail-nth sites found per boot (the enumeration's
last N is the first non-injected call); for signal targets it is the 4
delays x 2 reps = 8 trials per boot.

| target | mech | boots | sites/boot | trials | ok_pre | ok_post | partial | **viol** | unknown | classification |
|--------|------|-------|-----------|--------|--------|---------|---------|----------|---------|----------------|
| `mmap_fixed_replace` | fail-nth | 5 | 1,1,1,1,1 | 5 | 0 | 0 | 0 | **5** | 0 | contract-external (V4-2) |
| `mremap_fixed_dst` | fail-nth | 5 | 3 each | 15 | 0 | 10 | 0 | **5** | 0 | contract-external (V4-3) |
| `mprotect_multi` | fail-nth | 5 | 3,3,4,3,3 | 16 | 0 | 6 | 0 | **10** | 0 | contract-external (V4-4/5) |
| `tmpfs_setxattr_replace` | fail-nth | 5 | 3 each | 15 | 15 | 0 | 0 | 0 | 0 | clean |
| `tmpfs_rename_exchange` | fail-nth | 5 | 2 each | 10 | 10 | 0 | 0 | 0 | 0 | clean |
| `tmpfs_fallocate` | fail-nth | 5 | 17 each | 85 | 80 | 5 | 0 | 0 | 0 | clean (rollback works) |
| `pipe_set_size` | fail-nth | 5 | 1 each | 5 | 5 | 0 | 0 | 0 | 0 | clean |
| `so_attach_filter` | fail-nth | 5 | 10,10,10,10,11 | 51 | 43 | 8 | 0 | 0 | 0 | clean |
| `ip_add_membership` | fail-nth | 5 | 5,4,4,4,4 | 21 | 5 | 11 | 0 | **5** | 0 | **confirmed violation (V4-1)** |
| `semtimedop_eintr` | signal | 5 | 8 each | 40 | 40 | 0 | 0 | 0 | 0 | clean |
| `fcntl_setlkw_eintr` | signal | 5 | 8 each | 40 | 40 | 0 | 0 | 0 | 0 | clean |

Signal detail: 40/40 trials per target were injected (`waited=1`,
`ret=-1`, `errno=EINTR`), 10 per delay per target.  `semtimedop` left
`s0=1;s1=0` in all 40; `F_SETLKW` left `holder_killed=1;surv_lock_rc=0`
in all 40.

### 3.1 Injection-site summary

| target | injected sites | failures that leave PRE | successes that leave POST | violating sites |
|--------|----------------|-------------------------|---------------------------|-----------------|
| `ip_add_membership` | N=1 `sock_kmalloc`; N=2 `kzalloc` in `____ip_mc_inc_group`; N>=3 later IGMP report allocations | N=1 (`-ENOBUFS`) | N>=3 | **N=2** (returns 0, device group missing) |
| `mmap_fixed_replace` | N=1 `__mmap_new_vma` allocation | - | - | **N=1** (`-ENOMEM`, gap) |
| `mremap_fixed_dst` | N=1..3 (v6.19: 1 violating, 2 non-fatal) | - | N=2,3 | **N=1** (v7.3-rc2: N=1,2) |
| `mprotect_multi` | N=1,2 at the third VMA's split/merge allocation (v7.3-rc2: 3) | - | N=3 | **N=1,2** (first two VMAs changed) |
| `tmpfs_fallocate` | 17 folio/size sites; 16 fatal, 1 non-fatal (large-folio fallback) | 16 | 1 | none |
| `so_attach_filter` | 10-11 sites in the prog/filter attach path; 8-9 fatal, 1-2 non-fatal | 8-9 | 1-2 | none |

## 4. Confirmed violation V4-1 — `IP_ADD_MEMBERSHIP` success without the group

### 4.1 Exact evidence

Boot `logs/20261021-r4-mass/09-ip_add_membership-rep1.log`:

```
BASE target=ip_add_membership side=pre sample=1 obs=surv_msf=-1;surv_errno=99|obs_msf=-1;obs_errno=99;igmp_group=0
BASE target=ip_add_membership side=post sample=1 obs=surv_msf=0;surv_errno=0|obs_msf=0;obs_errno=0;igmp_group=1
BASE_STABLE target=ip_add_membership stable=1 pre=same post=same
TRIAL target=ip_add_membership mech=failnth site=1 ret=-1 errno=105 injected=1 readback=0 verdict=OK_PRE obs=surv_msf=-1;surv_errno=99|obs_msf=-1;obs_errno=99;igmp_group=0
TRIAL target=ip_add_membership mech=failnth site=2 ret=0 errno=0 injected=1 readback=0 verdict=VIOL_MIXED obs=surv_msf=0;surv_errno=0|obs_msf=0;obs_errno=0;igmp_group=0
TRIAL target=ip_add_membership mech=failnth site=3 ret=0 errno=0 injected=1 readback=0 verdict=OK_POST obs=surv_msf=0;surv_errno=0|obs_msf=0;obs_errno=0;igmp_group=1
```

* pre: no socket membership (`IP_MSFILTER` -EADDRNOTAVAIL = 99), no
  device group (`igmp_group=0`).
* post: socket membership present, device group present.
* N=2: P returns **0** (success), `IP_MSFILTER` succeeds, but
  `igmp_group=0`.  The state is neither pre nor post => `VIOL_MIXED`.
* 5/5 boots show exactly this at N=2; N=1 is the clean `sock_kmalloc`
  error, N>=3 are full successes.

Per-N single-boot trace (`logs/20261021-r4-trace-n2/`):

```
DWTRACE target=ip_add_membership n=2
TRACE_ONE target=ip_add_membership n=2 ret=0 errno=0 injected=1 readback=0 obs=surv_msf=0;surv_errno=0|obs_msf=0;obs_errno=0;igmp_group=0
```

`readback=0` is the fail-nth proof that the N=2 `should_fail()` was
consumed by this task during P; the observation is from a fresh object
in that same boot.

### 4.2 Attribution

`net/ipv4/igmp.c` (v6.19):

* `__ip_mc_join_group()` line 2261.  It allocates the socket-side
  `ip_mc_socklist` with `sock_kmalloc()` (line 2296), links it into
  `inet->mc_list` (line 2304), then calls
  `____ip_mc_inc_group(in_dev, addr, mode, GFP_KERNEL)` (line 2305)
  and unconditionally sets `err = 0`.
* `____ip_mc_inc_group()` line 1504 is `static void`; its
  `im = kzalloc(sizeof(*im), gfp)` (line 1535) does
  `if (!im) goto out;` and the function returns silently (line 1563
  `out: return;`).  The interface-side `ip_mc_list` is never created:
  no `igmp_group_added()`, no IGMP report, no `/proc/net/igmp` entry.
* The same helper is used by `__ip_mc_inc_group()` for interface-up and
  init callers, which cannot report an error.

So P's own return value (0) disagrees with the required effect.  The
allocation was reached after the socket list was published, and the
error path neither unpublishes it nor reports the failure.

Independent confirmation: the same signature was reproduced with a
minimal initramfs (not this harness) on both v6.19 and v7.3-rc2 —
n=1: `ret=-1 errno=105`, no group; n=2: `ret=0 errno=0`, no group;
n=3: `ret=0`, group present.  The v7.3-rc2 code has the same
`kzalloc_obj(*im, gfp)` / silent return (`net/ipv4/igmp.c:1601` there).

### 4.3 Contract and classification

Declared contract: `setsockopt(IP_ADD_MEMBERSHIP)` success means the
group is joined for this socket on the chosen interface, i.e. the
socket can receive the group's traffic.  `ip(7)` defines
`IP_ADD_MEMBERSHIP` as "Join a multicast group", with
`imr_multiaddr` "the address of the multicast group the application
wants to join" and `imr_ifindex` "the interface index of the interface
that should join/leave the `imr_multiaddr` group".  A successful join
that leaves the interface without the group is not usable: no IGMP
report is sent and the device's multicast filter has no entry for the
group.

Classification: **confirmed contract violation, success-without-effect
shape**, not contract-external.  No documented exception says a
successful `IP_ADD_MEMBERSHIP` may omit the interface group.  No memory
corruption, no security impact; functional only.  Reachability in our
runs is fault injection (`fail-nth`) of an ordinary `GFP_KERNEL`
allocation; a real-memory-pressure trigger was not constructed in the
guest, so real-world frequency is not claimed.

### 4.4 Fix

New detached worktree `~/dropwise-linux-r4` (v7.3-rc2
`df2908090cda`, config copied from the existing v7.3-rc2 build,
`olddefconfig` reported "No change to .config").  Patch:
`patches/0002-ipv4-igmp-report-interface-group-allocation-failure.patch`
(applied to the worktree, uncommitted):

* `____ip_mc_inc_group()` returns `int`: 0 on success (including the
  existing-group `users++` branch), `-ENOBUFS` when `kzalloc_obj()`
  fails.
* `__ip_mc_join_group()` calls it **before** `iml->next_rcu =
  inet->mc_list; rcu_assign_pointer(...)`, and on error does
  `sock_kfree_s(sk, iml, sizeof(*iml))` and returns the error.  The
  whole path is under RTNL, so no other thread can observe the
  half-built membership.
* The exported `void __ip_mc_inc_group()` / `ip_mc_inc_group()` wrappers
  keep their void API (interface-up/init callers cannot fail; a missing
  group there is corrected by the next IGMP query) and carry an explicit
  comment.

The fix is deliberately minimal: it changes only the join path's
ordering and error propagation; other callers' semantics are unchanged.

### 4.5 Original vs fixed (same tree, same config)

| kernel | boots | trials | ok_pre | ok_post | violations |
|--------|-------|--------|--------|---------|------------|
| v7.3-rc2 stock (`bzImage-stock`, build #1) | 5 | 20 | 5 | 10 | **5** (N=2 each boot) |
| v7.3-rc2 + patch (`bzImage-fixed`, build #2) | 5 | 20 | **10** | 10 | **0** |

N=2 before / after:

```
# stock
TRIAL target=ip_add_membership site=2 ret=0 errno=0 injected=1 readback=0 verdict=VIOL_MIXED obs=surv_msf=0;surv_errno=0|obs_msf=0;obs_errno=0;igmp_group=0
# fixed
TRIAL target=ip_add_membership site=2 ret=-1 errno=105 injected=1 readback=0 verdict=OK_PRE obs=surv_msf=-1;surv_errno=99|obs_msf=-1;obs_errno=99;igmp_group=0
```

N=1 stays `-ENOBUFS`/`OK_PRE`; N=3/N=4 stay `0`/`OK_POST` with the
group present.  So the fix converts the silent success into the
documented error and leaves the normal path unchanged.

### 4.6 Regression

All 26 targets (15 existing + 11 new), 1 boot each, on the same tree:

* stock v7.3-rc2: 26 targets, identical verdict pattern to the v6.19
  campaign for every round-4 target (ip violation at N=2; mremap 2
  violations, mprotect 3, mmap 1; all clean targets clean), and the
  known round-3 eventpoll violations (45/22) because this tree does not
  carry the round-3 eventpoll patch.
* fixed v7.3-rc2: identical to stock for all 25 non-igmp targets
  (verdict counts and per-site classifications unchanged; only
  `sites` varies by +/-1 in a few targets, e.g. `scm_rights` 2 vs 3,
  which is normal allocation-count noise and has no verdict effect).
  `ip_add_membership` changed exactly as intended (viol 1 -> 0,
  ok_pre 1 -> 2).
* The 15 existing targets on v6.19 with the unchanged harness show the
  same behaviour as recorded in rounds 1-3 (the epoll round-3 findings
  are present on unpatched kernels, as expected).

## 5. Contract-external partial effects on error

These are reported as concrete counterexamples to "error => no effect",
but they are **not** bugs and are not fixed: the destructive step is
documented, and a maintainer statement (David Hildenbrand, LKML
triage) says mprotect partial changes across VMAs are not a bug, and
that mmap/mremap `*_FIXED` error semantics never guaranteed the
destination mapping survives.

### 5.1 V4-2 `mmap(MAP_FIXED)` error leaves a gap (5/5 boots, N=1)

```
BASE pre  obs=surv_mapped=1;surv_errno=0;surv_byte=165|obs_mapped=1;obs_errno=0;obs_byte=165
BASE post obs=surv_mapped=1;surv_errno=0;surv_byte=0|obs_mapped=1;obs_errno=0;obs_byte=0
TRIAL site=1 ret=-1 errno=12 injected=1 readback=0 verdict=VIOL_MIXED obs=surv_mapped=0;surv_errno=12;surv_byte=-1|obs_mapped=0;obs_errno=12;obs_byte=-1
```

Source: `__mmap_setup()` (`mm/vma.c:2386`) gathers/clears the old PTEs
via `vms_clean_up_area()` -> `vms_clear_ptes()`, which sets
`vms->clear_ptes = false` (`mm/vma.c:1279`, `:1541`).
`__mmap_new_vma()`'s allocation then fails; `__mmap_region()` aborts to
`vms_abort_munmap_vmas()` (`mm/vma.c:2334`), and because
`vms->clear_ptes` is false it writes a NULL gap with
`mas_store_gfp(..., GFP_KERNEL|__GFP_NOFAIL)` (`:2351`) and completes
the munmap.  PTE clearing is the one-way step; the VMA cannot be
reattached.  `mmap(2)` documents that `MAP_FIXED` discards overlapping
mappings, so the error result (old mapping gone, new mapping not
installed) is within the documented destructive semantics.

### 5.2 V4-3 `mremap(MREMAP_FIXED)` error loses the destination (5/5 boots, N=1 on v6.19; N=1,2 on v7.3-rc2)

```
BASE pre  obs=surv_src=1:90;dst=1:195|obs_src=1:90;dst=1:195
BASE post obs=surv_src=0:-1;dst=1:90|obs_src=0:-1;dst=1:90
TRIAL site=1 ret=-1 errno=12 injected=1 readback=0 verdict=VIOL_MIXED obs=surv_src=1:90;dst=0:-1|obs_src=1:90;dst=0:-1
```

Source: `mremap_to()` (`mm/mremap.c:1367`) calls
`do_munmap(mm, new_addr, new_len, ...)` before `move_vma()`
(`:1270`); a later `copy_vma()`/`copy_vma_and_data()` allocation
failure (`:1197`) returns `-ENOMEM` with the destination already gone
and the source still in place.  `mremap(2)` says a previous mapping at
`new_address` "is unmapped", so losing the destination is the
documented effect of the `MREMAP_FIXED` request, not an atomicity bug.

### 5.3 V4-4/V4-5 `mprotect()` across three VMAs (10/10 sites on v6.19; 3/3 on v7.3-rc2)

```
BASE pre  obs=p0=rw-p;p1=rw-s;p2=rw-p
BASE post obs=p0=r--p;p1=r--s;p2=r--p
TRIAL site=1 ret=-1 errno=12 injected=1 readback=0 verdict=VIOL_MIXED obs=p0=r--p;p1=r--s;p2=rw-p
TRIAL site=2 ret=-1 errno=12 injected=1 readback=0 verdict=VIOL_MIXED obs=p0=r--p;p1=r--s;p2=rw-p
```

Source: `do_mprotect_pkey()` (`mm/mprotect.c:801`) walks the range and
calls `mprotect_fixup()` (`:695`) per VMA; the third VMA needs a split
(or merge) allocation, which fails with `-ENOMEM` after the first two
VMAs have already been changed.  The maintainer statement that this is
not a bug ("all but the last VMA will have their permissions changed")
makes this contract-external.  There is no documented all-or-nothing
guarantee for a multi-VMA `mprotect()`.

## 6. Clean targets (negative results)

These are useful because they show the kernel's usual
"allocate first, commit last" ordering and exercise the same
observations that would have caught a violation.

* **tmpfs `setxattr`** (3 sites/boot, 15/15 `OK_PRE`):
  `simple_xattr_set()` allocates `simple_xattr_alloc(value)` and
  `kstrdup(name)` before taking the write lock and replacing the
  rb-node; ENOMEM leaves "AAAA" untouched.
* **tmpfs `RENAME_EXCHANGE`** (2 sites/boot, 10/10 `OK_PRE`):
  `simple_offset_rename_exchange()` swaps dirents only after both
  `mtree_store()` calls; the second failure is rolled back by a third
  store, and a single injected failure leaves both names intact.
* **tmpfs `fallocate`** (17 sites/boot, 80 `OK_PRE` + 5 `OK_POST`):
  `shmem_fallocate()` undoes added folios via `shmem_undo_range()` and
  writes `i_size` only after the loop succeeds.  The 5 `OK_POST`
  sites are non-fatal large-folio allocation fallbacks (allocation
  failure recovered by a smaller folio), not lost state.
* **`F_SETPIPE_SZ`** (1 site/boot, 5/5 `OK_PRE`): `pipe_resize_ring()`
  `kcalloc()`s the new ring before copying and committing; ENOMEM
  leaves the old ring and the 4096 queued bytes.
* **`SO_ATTACH_FILTER`** (10-11 sites/boot, 43 `OK_PRE` + 8 `OK_POST`):
  the new `sk_filter`/prog is allocated before `rcu_assign_pointer()`;
  all fatal sites leave the old drop-all filter deciding delivery.
* **`semtimedop` + signal** (40/40): `perform_atomic_semop()` validates
  all operations before applying any; the EINTR path unlinks the queue
  without touching `semval`.  All 40 interrupted trials left
  `s0=1;s1=0`.
* **`fcntl(F_SETLKW)` + signal** (40/40): the lock is installed only
  when the wait is granted; on EINTR the forked survivor can take the
  lock immediately after the external holder is killed
  (`surv_lock_rc=0` in all 40).  This is the strongest direct test of
  the "unfinished wait not delivered to reuse" shape.

## 7. Framework issues found and fixed during this round

1. **`mremap_fixed_dst` survivor encoding bug** (found in smoke
   `logs/20261021-r4-smoke2`): the observation printed the destination
   byte where the destination `mapped` flag belonged
   (`dst=195:0` instead of `dst=1:195`).  Fixed before the campaign;
   the smoke log is kept as evidence.  No campaign result used the
   buggy encoding.
2. **`tmpfs_fallocate` enumeration saturation**: a 1 MiB range creates
   256 folio allocation sites, hitting `MAX_N=256`, so the loop never
   observed a non-injected N.  Shrunk to 64 KiB => 17 sites and a
   natural stop at N=18.  Not a correctness bug, but it made the
   "stop at first non-injected" calibration meaningless for that
   target.
3. `mprotect_multi`'s survivor performs no operation; the independent
   observer is the parent's `/proc/self/maps` read.  This is a
   deviation from the usual "survivor performs Q" pattern, recorded
   here; it does not affect the judged state (the parent's read is
   independent of P's return value).
4. No other harness errors: 0 `HARNESS_ERROR`, 0 `TARGET_TIMEOUT`, 0
   `BASELINE_UNSTABLE`, 0 `KNOB_FAIL`, 0 `MOUNT_FAIL` across all 55
   campaign logs, the trace boot, and the 36 validation/regression
   boots.

## 8. Which findings fit "unfinished signal not delivered to a reuse decision"

* **Fit exactly**: `fcntl_setlkw_eintr` and `semtimedop_eintr` are the
  two direct tests.  The signal arrives while P is sleeping; the
  question is whether the interrupted wait's incomplete state (lock
  request, semaphore operations) is later treated as if it had
  completed.  Both are clean: no lock is installed on EINTR, and the
  two-operation semop applies nothing.  So the shape exists in the
  kernel and the kernel handles it correctly in these two places.
* **Does NOT fit**: `ip_add_membership` is not signal-related.  It is
  the broader "state changed before a fallible step; the error path
  does not restore it" shape, in its extreme form: the fallible step
  fails, the failure is *never reported*, and the socket-side state is
  published before the device-side effect exists.  No unfinished wait
  and no reuse decision are involved; the decision is the caller's
  reliance on the syscall's success return.
* **Does NOT fit**: `mmap(MAP_FIXED)`, `mremap(MREMAP_FIXED)` and
  multi-VMA `mprotect` are "destructive step before a fallible step"
  with the destruction documented; they are contract-external, not
  unfinished-signal bugs, and were not fixed.
* **Converse pattern**: the clean targets (setxattr, exchange,
  fallocate, pipe size, socket filter) are "allocate the replacement
  first, commit last"; they are the safe ordering that the fixed igmp
  path now follows for the interface group.  The round-3 eventpoll fix
  and this round's igmp fix share the principle: a state flag/effect
  must be set before the exchange and cleared only on full completion,
  never published before the fallible step.

## 9. Cost, deviations, limits

* Campaign: 55 boots, 303 trials, ~49 s of QEMU wall time; 11 smoke
  boots; 15 existing-target boots on v6.19; 26+26 same-tree
  regression boots; 10 fix-validation boots; 1 trace boot.  Total
  QEMU time well under 3 minutes plus kernel builds.
* Kernel builds: full stock v7.3-rc2 bzImage ~3-4 min on 16 cores from
  a clean worktree/build dir; patched rebuild (igmp.o + vmlinux link)
  ~40 s.  Only `bzImage` was built.
* All runs use 5 boots per fail-nth target and 40 signal trials per
  signal target (4 delays x 2 reps x 5 boots), all injected.
* Limits: the confirmed bug was reached only by fault injection; no
  real memory-pressure trigger was constructed in the guest.  The
  `fail-nth` site identity is inferred from the N ordering, the error
  codes, and the source, not from a kernel stack trace (the guest has
  no ftrace stack capture in this run); the parent's standalone
  minimal-initramfs run on v6.19 and v7.3-rc2 independently confirms
  the same three N signatures.  The mmap/mremap/mprotect results are
  classified contract-external based on the documented `*_FIXED`
  semantics and the relayed maintainer statement; no stable URL for
  that message was captured.
* No commits, no emails, no patch submission.  `~/Worktrees/linux/`,
  `~/dropwise-linux-work/linux` and `~/dropwise-linux-work73/linux`
  were not modified (the new worktree was added from the v7.3-rc2
  repository as detached `~/dropwise-linux-r4`).
* Artifacts:
  * `harness/dw_harness.c` (11 new targets), `run.py` (`ALL_TARGETS`),
  * `patches/0002-ipv4-igmp-report-interface-group-allocation-failure.patch`,
  * `~/dropwise-linux-r4/bzImage-stock`, `~/dropwise-linux-r4/bzImage-fixed`,
  * logs as listed in 1.3.
