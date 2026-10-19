# Results: a fail-nth prototype for Linux syscall handoff atomicity

**Status.** The first milestone is reached.  Six targets (SCM_RIGHTS `sendmsg`
plus five extensions) ran end to end in a fault-injection QEMU/KVM guest.  In
the reported campaign (5 boots per target, 30 boots, 105 injected trials) there
were **0 violations, 0 unknown verdicts and 0 unstable calibrations**.  No
kernel issue was confirmed, so there are no private issue details.

All numbers below come from the logs in
[`logs/20261019-050729-mass/`](logs/20261019-050729-mass/) (trials.csv,
summary.csv, aggregate.csv and the 30 raw serial logs).  Trace-format runs are
in `logs/20261019-050648-trace/` and are diagnostic only.  The test date is
2026-10-19; the guest kernel is 6.19.0 built from tag `v6.19`.

## 1. Milestone

The guest boots from an initramfs with:

- `CONFIG_FAULT_INJECTION`, `FAILSLAB`, `FAIL_PAGE_ALLOC`,
  `FAULT_INJECTION_DEBUG_FS`, `DEBUG_FS`;
- the debugfs filters opened up so order-0 and `__GFP_DIRECT_RECLAIM` /
  `__GFP_HIGHMEM` allocations can be injected;
- a static C harness as `/init`, no busybox, no disk.

The harness selftest succeeded in all 30 boots: writes of `N`, readback,
`getpid` (no consumption), and the documented `socketpair()` enumeration.  The
socketpair path showed **14 or 15** injectable sites across boots (25 boots
with 14, 5 with 15).  The milestone syscall, AF_UNIX `sendmsg` with
`SCM_RIGHTS`, then ran the full sequence: two `pre` samples, two `post`
samples, `N = 1, 2, ...` until readback was non-zero, forked survivor,
observer, and verdict.  Both baselines were identical sample-to-sample and
across all five boots.  See `01-scm_rights-rep1.log` and the other five SCM
logs.

## 2. Results table

"Contracted N" is the number of `N` values for which injection was observed in
one boot; the final no-injection `N` is one larger and read back `1` in all 30
boots.  Each target was booted five times.

| syscall | contract on error / partial | contract N | boots | injected trials | OK_PRE | OK_PARTIAL | OK_POST | violations | unknown | triage |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---|
| `sendmsg` + `SCM_RIGHTS` (`scm_rights`) | error ⇒ no byte, no fd delivered | 3 | 5 | 15 | 15 | 0 | 0 | 0 | 0 | clean |
| pipe `write` 8192 B (`pipe_write`) | error ⇒ no bytes; short write legal, only the prefix may appear | 2 | 5 | 10 | 5 | 5 | 0 | 0 | 0 | clean; N=2 is legal partial completion |
| tmpfs `pwrite` 4096 B (`tmpfs_pwrite`) | error ⇒ hole unchanged | 1 | 5 | 5 | 5 | 0 | 0 | 0 | 0 | clean |
| `epoll_ctl(ADD)` (`epoll_ctl_add`) | error ⇒ fd not in interest list | 3 | 5 | 15 | 15 | 0 | 0 | 0 | 0 | clean |
| `fcntl(F_SETLK)` (`fcntl_setlk`) | error ⇒ no lock held | 4 | 5 | 20 | 15 | 0 | 5 | 0 | 0 | clean; N=4 injected but completed (legal) |
| `accept4` on AF_UNIX (`accept4_unix`) | error ⇒ no accepted fd; queue preservation empirical | 8 | 5 | 40 | 40 | 0 | 0 | 0 | 0 | clean in 6.19 |
| **total** | | **21** | **30** | **105** | **95** | **5** | **5** | **0** | **0** | |

The returns and errnos were identical in all five boots:

| target | N | return | errno | verdict | observation |
|---|---:|---|---|---|---|
| scm_rights | 1 | -1 | ENOMEM (12) | OK_PRE | recv EAGAIN, no fd, FIONREAD 0 |
| scm_rights | 2, 3 | -1 | ENOBUFS (105) | OK_PRE | same |
| pipe_write | 1 | -1 | ENOMEM (12) | OK_PRE | read EAGAIN, no bytes |
| pipe_write | 2 | 4096 | 0 | OK_PARTIAL | survivor read exactly 4096 `P` bytes, FIONREAD 0, no extra byte |
| tmpfs_pwrite | 1 | -1 | ENOMEM (12) | OK_PRE | survivor's `S` plus 4095 zeros; size 8192, st_blocks 8 |
| epoll_ctl_add | 1, 2, 3 | -1 | ENOMEM (12) | OK_PRE | fdinfo has no tfd, both waits 0, DEL returns ENOENT |
| fcntl_setlk | 1 | -1 | ENOLCK (37) | OK_PRE | survivor read lock succeeds; no `/proc/locks` entry |
| fcntl_setlk | 2 | -1 | ENOMEM (12) | OK_PRE | same |
| fcntl_setlk | 3 | -1 | ENOLCK (37) | OK_PRE | same |
| fcntl_setlk | 4 | 0 | 0 | OK_POST | write lock held; survivor read lock EAGAIN; `/proc/locks` WRITE entry |
| accept4_unix | 1,2,3,6,7,8 | -1 | ENOMEM (12) | OK_PRE | survivor accepts the queued client and its token reaches the client |
| accept4_unix | 4,5 | -1 | ENFILE (23) | OK_PRE | same |

`ENFILE` is `sock_alloc()` returning NULL at `net/socket.c:1965-1967`.
`ENOBUFS` is the skb allocation path in `sock_alloc_send_pskb()`; the kernel
returns `-ENOBUFS`, not `-ENOMEM`, when that path fails.

## 3. What the survivors and observers saw

- **scm_rights.** The survivor was a forked child calling `recvmsg()` on the
  peer of the socketpair, installing the passed fd and reporting the byte,
  fd count, whether the received file's inode matched the sender's, and
  `FIONREAD`.  The parent independently called `FIONREAD` again.  Every error
  trial equalled the pre baseline exactly: `recv_rc=-1/errno=11`,
  `nfds=0`, `ino_match=0`, `FIONREAD=0`.  The post baseline was
  `recv_rc=1;byte=88;nfds=1;ino_match=1;FIONREAD=0`.  No byte and no fd were
  delivered when the sender reported an error.
- **pipe_write.** The survivor read up to 16 KiB; the parent used `FIONREAD`
  and a second nonblocking read.  At N=1 the page allocation failed before any
  copy: read EAGAIN, FIONREAD 0.  At N=2 the first 4096-byte page was already
  queued when the second page allocation failed; `pipe_write()` keeps the
  earlier `ret` and returns 4096.  The survivor read exactly 4096 `P` bytes,
  FIONREAD was 0 and a second read returned EAGAIN.  This is the interface's
  legal short write, not a violation.
- **tmpfs_pwrite.** The survivor wrote one `S` byte at offset 0, in the same
  page as the target write.  The parent opened a **second** fd to the file,
  `pread` 4096 bytes and `fstat`ed it.  On failure the page was instantiated by
  the survivor's own write: `S=1, P=0, Z=4095, size=8192, blocks=8`.  On success
  the page was `S=1, P=4095, Z=0`, same size/blocks.
- **epoll_ctl_add.** The survivor performed a nonblocking `epoll_wait()` after
  the watched pipe was made readable.  The parent read
  `/proc/self/fdinfo/<epfd>`, did a second `epoll_wait()` and then
  `EPOLL_CTL_DEL`.  On every failure: no `tfd:` line, both waits 0, DEL
  `ENOENT`.  The post baseline: one `tfd`, `tfd_ev=0x19` (the kernel adds
  `EPOLLERR|EPOLLHUP` to the registered mask), one event with data `0x1234`,
  second wait 1, DEL 0.  The observation string intentionally omits
  `fdinfo` length and raw fd numbers.
- **fcntl_setlk.** The survivor was a child process taking a read lock on the
  same inode.  The parent read `/proc/locks`, filtered by inode and its own
  pid.  On the three error sites the survivor's lock succeeded and
  `/proc/locks` had no entry.  At N=4 the target syscall returned success and
  the post state was real: write lock present, survivor's read lock EAGAIN.
- **accept4_unix.** A client was connected to a listening AF_UNIX socket
  before the target call.  The survivor accepted it (and wrote a token); the
  parent then read the token from the client socket.  At all eight error sites
  the survivor accepted the connection and the client received the token.
  No trial returned an error and left the client with EOF/reset.

## 4. Kernel source paths

The allocation and error paths below are from the v6.19 tree.  "Trace" means
the debugfs verbose dump was read in `logs/20261019-050648-trace/`; where the
trace was lost to console/poweroff, the site is inferred from the errno and
source and is marked as such.

| target | allocation / failure path | file:line | evidence |
|---|---|---|---|
| scm_rights | `scm_fp_copy()` `kmalloc(sizeof(struct scm_fp_list))`; caller `__scm_send()` | `net/core/scm.c:86`, `net/core/scm.c:194` | trace N=1: `__kmalloc_cache_noprof` ← `__scm_send` |
| scm_rights | `sock_alloc_send_pskb()` failure; `unix_scm_to_skb()`/`unix_attach_fds()` failure before enqueue; enqueue is later | `net/unix/af_unix.c:2430-2440`, `:1941-1993`, `:2481`, error labels `:2508-2514` | errno 105; source; N=2/3 trace dumps not captured |
| pipe_write | `anon_pipe_get_page()` → `alloc_page()`; failure keeps earlier `ret`, sets `-ENOMEM` only if nothing written; `ret += copied` | `fs/pipe.c:114-124`, `:515-519`, `:542`, function `:431` | trace N=1/2: `alloc_pages_*` ← `anon_pipe_write` |
| tmpfs_pwrite | `shmem_alloc_and_add_folio()` returns `-ENOMEM` before/at page-cache insert; `generic_perform_write()` returns status if `written==0` | `mm/shmem.c:1924-1988`, `:2584`; `mm/filemap.c:4314-4317`, `:4373-4376` | trace N=1: `shmem_alloc_folio` ← `shmem_get_folio_gfp` ← `shmem_write_begin` |
| epoll_ctl_add | `epi_cache` alloc, `attach_epitem()`/`ephead_cache`, `ep_ptable_queue_proc()`/`pwq_cache`; cleanup removes the item | `fs/eventpoll.c:1583`, `:1527-1542`, `:1368-1371`, `:1598-1604`, `:1655-1656`, insert `:1613`, ready list `:1668` | trace N=1/2 from `do_epoll_ctl`; N=3 `ep_ptable_queue_proc` ← `pipe_poll` |
| fcntl_setlk | request lock; context; two spare locks; simple insert uses `new_fl`, `new_fl2` only for a split | `fs/fcntl.c:496-500`; `fs/locks.c:2463-2470`, `:2498`, `:1210-1213`, `:1223-1224`, `:1391-1399`, `:1406-1410`, `:1431` | trace N=1 `fcntl_setlk`, N=2 `locks_get_lock_context` ← `posix_lock_inode`, N=3/4 `posix_lock_inode` |
| accept4_unix | `sock_alloc()` and `sock_alloc_file()` before `->accept()`; security hook; `ops->accept()`; `FD_ADD` (fd allocation plus `do_accept`) | `net/socket.c:1951-2009`, `:2022`; `fs/file.c:215`, `:252-271`; `net/unix/af_unix.c:1840-1885` | trace: `sock_alloc`/`sock_alloc_file` before `do_accept`; the fd-table trace shows `alloc_fd` in `__sys_accept4` without `do_accept`, i.e. the fd was reserved before the dequeue in this build |

The accept4 trace was noisy because printk/console frames interleave with the
stack dump; the N-to-site attribution there is only partially verified.  What
is verified is the observable result: every injected failure returned an error
and the pending connection was still acceptable by the survivor.

## 5. Stability

- **Calibrations.** Each target was booted 5 times; within each boot `pre` was
  sampled twice and `post` twice.  All 30 boots had `stable=1` (two samples
  agree).  Across the five boots there is exactly **one distinct `pre` string
  and one distinct `post` string per target** (checked against trials.csv).
- **Verdicts.** 105 injected trials: 95 `OK_PRE`, 5 `OK_PARTIAL`, 5 `OK_POST`,
  0 `VIOL_*`, 0 `UNKNOWN`.  There were no hangs, crashes or missing reports.
- **Stop condition.** In every boot, the first non-injected `N` read back `1`
  (the target made exactly `N-1` `should_fail()` calls), so the enumeration
  ended at the expected boundary.  Contracted N counts were identical in all
  five boots: 3, 2, 1, 3, 4, 8.
- **Fields removed for stability.** The raw accepted fd number and the
  `/proc/self/fdinfo` length varied with fd-table growth and inode digit width;
  those are not part of the observation (accept success is a boolean; epoll
  observations contain the tfd count and event mask, not the fd number).
- **Trace mode is not stable enough to judge.** Verbose `dump_stack()` output
  changes timing and console load; trace runs showed, for example, 2 SCM sites
  where the mass run showed 3.  Trace runs are therefore diagnostic and no
  verdict in this document uses them.

### Framework errors found during development (all fixed)

These were test-harness errors, not kernel findings.  They are recorded
because they are the main practical obstacle of the method.

1. **fail-nth fd is task-bound across fork.** `fail-nth` was opened once in
   PID 1 and inherited by the worker; writes from the worker changed the
   opener's task, not the worker's, so `sendmsg` had zero injectable sites.
   Fixed by reopening `/proc/self/task/<gettid>/fail-nth` in the task that runs
   `P`.
2. **First-touch page faults are injection points.** Filling/memsetting the
   target's user buffer after arming added 2 page-allocation sites to the pipe
   and 1 to tmpfs (and could even fail the userspace setup).  Fixed with a dry
   run of `P` per trial before the real one, which faults in stack/data pages.
3. **The harness's own allocations perturb N.** The survivor-report pipe was
   originally created after `P`; if the fd table was full it expanded it and
   consumed/added sites.  The pipe is now created before arming.  The accept
   target also fills the fd table to force an `alloc_fdtable()` site; without
   per-trial process isolation that expansion happened only on the first trial
   and site counts varied from 5 to 8.
4. **Raw fd numbers in observations.** The accept survivor reported the
   accepted fd number; it grew with the fd table (64, 128, ...), so identical
   states compared unequal.  Fixed by observing a success boolean.
5. **Stale partial-predicate context.** The pipe partial predicate read the
   observer context in the parent, but the observation was produced in the
   per-trial child.  It produced a false `VIOL_MIXED` at N=2.  Fixed by
   serializing the observer values in the outcome (the observation itself was
   always correct).
6. **Runner marked fast boots incomplete.** QEMU powered off before the host
   polled the log tail; the log did contain `HARNESS_DONE`.  Fixed by checking
   the file when the process exits.

The earlier trace campaign (2026-10-19 05:06:48) still shows `done=0` for two
logs because of error 6; the serial logs themselves end in `HARNESS_DONE`.  The
reported mass campaign is unaffected.

### False alarms triaged away

During development, the accept target's raw-fd observation produced
`VIOL_MIXED`, and the pipe partial predicate produced `VIOL_MIXED` at N=2.
Both were reproduced and traced to harness bugs 4 and 5 above; after the fixes
the same trials are `OK_PRE` and `OK_PARTIAL`.  They are not kernel violations.

## 6. Triage

### Confirmed kernel issues

**None.**  No trial produced an observation that violated its declared error
contract.  The specific shapes looked for were: error but the full effect is
present, success but no effect, and an observation that is neither baseline nor
the legal prefix.  None occurred.  Consequently
[`PRIVATE-NOTES.md`](PRIVATE-NOTES.md) contains no issue details.

### Contract-outside behavior (expected, not violations)

- **pipe N=2: short write.**  The interface allows a write to return fewer
  bytes than requested.  The observation is exactly the 4096-byte prefix and
  nothing else, so the kernel behaved correctly under the declared partial
  contract.  This does mean a strict "error/failure ⇒ atomic" checker would
  produce a false positive here; the prototype's partial predicate is the
  analogue of Dropwise's declared `Expect`.
- **fcntl N=4: injected failure, syscall succeeds.**  The failed allocation is
  the second spare `file_lock`; a simple whole-file insert needs only the
  first one, so the lock is acquired and the post baseline is correct.  A
  fault-injection harness that treats "injection happened" as "the syscall
  must fail" would misjudge this.
- **accept4: queue preservation.**  All eight errors left the connection
  acceptable.  Linux 6.19's `do_accept()` allocates the socket and file before
  `->accept()`, so these allocations fail before the dequeue.  The man page
  does not guarantee that an error leaves the pending connection queued; the
  observation is empirical.  The old comment at `net/socket.c:2025-2030`
  ("we open the socket then return an error") is stale for the observed
  allocation order.

### Test framework errors

Listed in §5.  The final campaign has none: 0 unstable calibrations, 0
unknowns, 0 missing logs.

### Unknown / unjudgeable

**0 of 105.**  The `UNKNOWN` class exists for baseline disagreement and could
not occur in the reported campaign.  The earlier site-count variability was
fixed before the campaign, not counted as unknown.

## 7. Feasibility conclusion

**The method is feasible and cheap; it is not yet broad.**  The verdict engine
worked as designed: with two calibrated baselines, a forked survivor and a
userspace observer, it classified 105 injected trials with no false alarms and
no unknowns, in a 30-boot campaign whose total wall time was about 30 seconds
(1.0 s per boot).  The harness is about 1,700 lines of C and needs only QEMU,
KVM and the fault-injection kernel.

The main obstacles, in order:

1. **Coverage.**  `fail-nth` reaches only `should_fail()` sites.  This kernel
   exposed 1–8 sites per target, all allocation failures.  The error branches
   that matter most (bad arguments, permission, signals, a canceled io_uring
   request, a full queue, a short userspace copy) are not reachable this way.
   `fail_function`, `fail_usercopy`, signal injection and io_uring cancellation
   would be the next mechanisms.
2. **N is not a stable site identifier.**  Even with per-trial isolation and a
   dry warmup, the allocation sequence can depend on cache/fd-table state and
   on tracing.  The final mass campaign is stable, but the stop-at-first-
   non-injected rule is fragile: it cannot distinguish "no more sites" from
   "this run has fewer sites than the next run".  A production harness should
   identify sites from traces or `fail_function`, or enumerate a probed upper
   bound and report coverage, rather than treat N as the site.
3. **Observability.**  The verdict only sees userspace-visible state.  A failed
   tmpfs write that instantiated a zero page, an unused lock context, or a
   kernel queue entry with no userspace reader is invisible; the prototype
   cannot judge internal partial state.
4. **Contracts.**  "Error ⇒ unchanged" is not universal (short writes, accept
   queue semantics, close-on-error).  The contract has to be declared and
   triaged per syscall, as with Dropwise's `Expect`; this is the human review
   bottleneck, not the harness.
5. **Disturbance.**  The harness itself can create injection points; every
   target needs a review of what is in the armed window (fd tables, buffer
   first touch, printk).  The dry-run and per-trial isolation were essential.

Recommended continuation: keep the baseline/survivor/observer engine, replace
the single mechanism with fail_function + signals + io_uring, use per-syscall
observed-state contracts, and grow the target set along shared objects where a
next user really exists (socket fd passing, pipes, epoll, locks, queued
accepts) before trying filesystem-wide state.

## 8. Cost

Measured pieces:

- Kernel source worktree: 1.7 GB; object tree 807 MB; `bzImage` 14 MB.
- Kernel build: about **3 minutes** with 16 cores (`defconfig` + options +
  `bzImage`; not otherwise instrumented).
- Guest boot: **1.0 s** wall per boot; 30 boots = **30.1 s** for the reported
  campaign.
- Guest compute per target per boot (`SUMMARY elapsed_ms`): 1–12 ms for SCM,
  1–11 ms pipe, 1–10 ms tmpfs, 1–12 ms epoll, 17–22 ms fcntl, 2–21 ms accept.
  Most of the wall time is QEMU boot, not the experiment.
- 105 injected trials, each with a fresh object, a dry run, two baseline
  samples and a forked survivor; the measured trial cost is in the milliseconds.
- Logs for the reported campaign: 160 KB.  Harness/build products: 5.6 MB.
- Total session wall clock for the prototype (source prep, kernel build,
  harness bring-up, six targets, the final campaign and these notes): roughly
  **35–45 minutes**; the split is approximately 5 min environment, 3 min build,
  20–25 min harness/target development and debugging, and 2 min of final
  campaigns.  The development portion is approximate; the build, boot and
  campaign numbers are from the logs.

## 9. Deviations and unfinished work

- Six targets total (SCM milestone + five extensions).  `fcntl` and `accept4`
  are outside the brief's candidate list; fcntl was chosen for a true external
  observer (`/proc/locks`) and accept for the dequeue-then-fail shape.
- The brief prescribed stopping at the first non-injected N.  The harness does
  exactly that.  It also runs probes with N=1000 and a dry `P` per trial; those
  are calibration/diagnostics and are not used as verdicts.
- Per-trial process isolation and the dry warmup are harness-level protections
  against the framework errors above, not changes to the judging rule.
- Signal interruption, io_uring cancellation, `fail_function`, `fail_usercopy`,
  real block filesystems, TCP, and multiple kernel versions were not tested.
- Trace attribution for SCM N=2/3 and accept sites is partial because console
  dumps were lost; exact allocation sites for those are marked above.
- No kernel issue was confirmed, so no private details exist; see
  [PRIVATE-NOTES.md](PRIVATE-NOTES.md).

## 10. Verification status

Trial counts, errnos, verdicts, baseline strings, boot counts and guest
`elapsed_ms` are taken directly from the logs under
`logs/20261019-050729-mass/`.  Kernel line numbers are from the v6.19 tree.
The kernel build time and the 35–45 minute session split are approximate and
not separately instrumented; the build, boot and campaign measurements cited
above are from timestamps and runner output.  Man-page statements are from the
local `accept(2)`, `sendmsg(2)`, `write(2)`, `pwrite(2)`, `epoll_ctl(2)` and
`fcntl(2)` pages; the accept queue-preservation statement is empirical, not a
man-page guarantee.  The allocation-site attribution for SCM N=2/3 and for the
accept fd-table site is partial because the trace console output was noisy or
lost; those rows say so.
