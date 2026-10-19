# Results round 2: signals, fail_function, and a confirmed EPOLLWAKEUP issue

**Status.** Round 2 kept round 1's judgement engine (two calibrated
baselines, a forked survivor on the same object, an independent observer) and
replaced the single injection mechanism with three: **fail-nth** (control),
**signal interruption of an interruptible wait**, and **fail_function**.  It
ran **1013 injected trials** in 70 QEMU/KVM boots across 14 target entries
(six round-1 entries plus eight new ones; 17 target×mechanism rows).  In the final campaign
there were **0 unknown verdicts, 0 unstable calibrations, 0 `OK_POISONED`
results, and 234 violations**, all 234 belonging to one confirmed, minor,
non-security kernel issue: `epoll_ctl(EPOLL_CTL_MOD)` ignores the failure of
`ep_create_wakeup_source()` and returns 0 with `EPOLLWAKEUP` advertised but no
wakeup source registered.

The confirmed issue is stable (10/10 v6.19 boots across two campaigns, plus
5/5 boots on a separately built v7.3-rc2; 5/5 for the `N=1` reproduction) and
is reproduced by two independent targets (`MOD` fails the contract; `ADD`
under the same injection returns `-ENOMEM` and leaves no item).  It is still
present in mainline v7.3-rc2 and in the v7.3-rc6 tree (`origin/master` at
`a90ee4305c4a`); the two source blobs are identical.  It is a
power-management functional defect, not a security issue, so per instruction
it is written directly here rather than in `PRIVATE-NOTES.md`.

All numbers below come from actual runs.  The final campaign is in
[`logs/20261019-round2-final/`](logs/20261019-round2-final/); the focused
EPOLLWAKEUP campaign is in
[`logs/20261019-epoll-wakeup-mass/`](logs/20261019-epoll-wakeup-mass/); the
per-injection-point traces are in
[`logs/20261019-125706-trace-n/`](logs/20261019-125706-trace-n/) (TLS and
overlay) and
[`logs/20261019-130011-trace-n/`](logs/20261019-130011-trace-n/) (epoll).
Round-1 [`RESULTS.md`](RESULTS.md) is unchanged.  Test date 2026-10-19; guest
kernel 6.19.0 from tag `v6.19` (`05f7e89ab973`).

## 1. Mechanisms

| mechanism | status | what ran | coverage of the mechanism |
|---|---|---|---|
| fail-nth (`/proc/<tid>/fail-nth`) | works | 893 injected trials over 11 target entries; every target enumerates N=1.. until the first non-injected N | 893/893 injected trials judged; per-N trace attempted for all 158 site values, 150 attributed (`§6`) |
| signal, no `SA_RESTART` | works | 120 trials: pipe blocked-before-write, pipe blocked-after-one-page, `mq_send` on a full queue; delays 0/200 µs/2 ms/20 ms after the thread was seen in state `S`, two reps each | **120/120** trials had `waited=1`: the P thread was observed sleeping in `/proc/self/task/<tid>/stat` before the signal was sent |
| fail_function | mechanism works, no target hit | 70/70 boots passed a selftest (`__x64_sys_getpid` injected, returned `-EINVAL`, `times` read back 0); 15 target-level attempts (three targets × five boots) | **0 target-level hits**; see §6.3 and §9 |
| fail-nth control | works | the same targets also ran plain fail-nth | see table §4 |

`OK_POISONED` was added to the verdict engine (and to the target predicates:
TLS `EIO`/`EBADMSG`/`EPIPE`, overlay `EIO`, mqueue `EBADF`/`ENOENT`), so a
survivor that is told the object is unusable is legal.  **No trial produced
that state**; all error trials were clean pre states.

## 2. Targets and contracts

The target selection follows the brief: pick the boundary between a generic
layer and a concrete protocol or module.  The EPOLLWAKEUP pair was added as a
priority target after the first full campaign.

| target | P | survivor Q | independent observer | declared contract |
|---|---|---|---|---|
| `tls_tx` | `setsockopt(SOL_TLS, TLS_TX)` on a TCP socket whose ULP is `tls`, TLS 1.2 AES-GCM-128 fixed test key | `send("P",5)` on the same socket | peer reads the wire bytes; `getsockopt(TLS_TX)` for the context | `setsockopt(2)` + kTLS `Documentation/networking/tls.rst`: on error there is no TX context, so Q's bytes are plaintext; success ⇒ a TLS record `17 03 03 ...` and a configured TX context |
| `tls_rx` | `setsockopt(SOL_TLS, TLS_RX)` with the matching key; peer has a working TLS TX context | `recv()` on the same socket | the bytes Q got; `getsockopt(TLS_RX)` | on error Q must still see raw ciphertext; success ⇒ Q decrypts `H` and RX is configured |
| `overlay_copyup` | `open(merged/f, O_RDWR)` on a lower-only file; lower and upper are tmpfs, workdir is tmpfs | `open(merged/f)` + `read(4096)` | direct read of `upper/f` (existence, length, first byte, mode), `stat` of lower and merged | `open(2)`: error ⇒ no partial upper copy; a **complete** upper copy left by a failed open is accepted as a legal pre-like internal copy-up artifact, but a partial upper is not |
| `mq_send_full` | `mq_send()` on a full queue (4 messages) | open a second mqd, drain exactly four | `mq_getattr(mq_curmsgs)`, then read a fifth message if present | `mq_send(3)`/POSIX: on error the message is not enqueued; on success the queue contains P's message |
| `pipe_sig_block` | blocking `write(8192)` to a completely full pipe | read up to 64 KiB | `FIONREAD` + a second read | `write(2)`: after `EINTR` no byte may have been queued |
| `pipe_sig_partial` | blocking `write(16384)` with 8192 bytes free: writes one page, then waits | read up to 128 KiB | `FIONREAD` + a second read | a short return is legal and only that prefix may be present; an error with a prefix present would be a violation |
| `epoll_mod_wakeup` | `epoll_ctl(MOD)` setting `EPOLLIN|EPOLLWAKEUP` on an item registered with `EPOLLIN` only | `epoll_wait` after the pipe is readable | `/proc/self/fdinfo/<epfd>` event mask + `/sys/kernel/debug/wakeup_sources` counts | success ⇒ the `EPOLLWAKEUP` guarantee is in effect, i.e. the per-epoll and per-item wakeup sources exist; error ⇒ no change |
| `epoll_add_wakeup` (control) | `epoll_ctl(ADD)` with `EPOLLIN|EPOLLWAKEUP` | `epoll_wait` | same | on error the fd is not in the interest list (and no item wakeup source exists); success ⇒ item plus sources |
| six round-1 targets | unchanged | unchanged | unchanged | as in [`RESULTS.md`](RESULTS.md) §2 |

Why these: TLS is the socket-layer/protocol boundary where a partly installed
TX or RX context changes what the next sender puts on the wire.  Overlayfs is
the VFS/stackable-fs boundary where copy-up is a multi-step internal operation
with an upper directory that a second process can inspect.  POSIX mqueues are
a user-visible queue with an atomic enqueue/dequeue contract.  Pipes are the
round-1 control for signal interruption.  `epoll_ctl(MOD)` is the
generic-syscall/PM boundary where the code has an explicit error path that one
caller checks and the other does not.

## 3. Confirmed issue: `epoll_ctl(MOD)` reports success without a wakeup source

### 3.1 Shape

`EPOLL_CTL_ADD` and `EPOLL_CTL_MOD` both need `ep_create_wakeup_source()` when
`EPOLLWAKEUP` is requested.  `ep_insert()` checks the return value and removes
the just-inserted item on failure.  `ep_modify()` first stores the new event
mask (including `EPOLLWAKEUP`), then calls `ep_create_wakeup_source()` and
**ignores its return value**:

- v6.19 `fs/eventpoll.c:1629-1635` (`ep_insert`): `error =
  ep_create_wakeup_source(epi); if (error) { ep_remove_safe(ep, epi); return
  error; }`
- v6.19 `fs/eventpoll.c:1706-1711` (`ep_modify`): `epi->event.events =
  event->events;` then `if (!ep_has_wakeup_source(epi))
  ep_create_wakeup_source(epi);` — no error check, and `ep_modify()` ends with
  `return 0`.
- v7.3-rc2 `fs/eventpoll.c:1926-1931` and `:1995-2000` still have exactly the
  same shape.  `v7.3-rc2:fs/eventpoll.c` and `origin/master` (v7.3-rc6,
  `a90ee4305c4a`) have the identical blob `e0c4bf88a838`.
- Allocation path: `ep_create_wakeup_source()` at v6.19
  `fs/eventpoll.c:1489-1509` calls `wakeup_source_register(NULL,
  "eventpoll")` for the per-epoll source and again for the per-item source;
  `wakeup_source_register()` at `drivers/base/power/wakeup.c:209-227` returns
  `NULL` when `wakeup_source_create()` (`:80-98`, `kzalloc`) or the sysfs
  device creation fails.

Consequence: `epoll_ctl(MOD)` returns 0, `fdinfo` advertises
`events: 0x20000019` (the `EPOLLWAKEUP` bit is set), but there is no wakeup
source for the epoll or for the item.  `ep_pm_stay_awake()` is a no-op when
`epi->ws` is NULL (`fs/eventpoll.c:703-709`), so the suspend-prevention
guarantee documented in `epoll_ctl(2)` is silently absent.  This is a
power-management functional bug, no memory corruption and no security impact.

### 3.2 Reproduction

Target `epoll_mod_wakeup`; `fail-nth N=1` is the first allocation in
`wakeup_source_register()`:

```
TRIAL target=epoll_mod_wakeup mech=failnth site=1 ret=0 errno=0 injected=1 \
  readback=0 verdict=VIOL_MIXED \
  obs=wait_rc=1;wait_errno=0;wait_ev=1;wait_data=4660|tfd=1;tfd_ev=20000019;\
      ws_total=11;ws_ep=0;ws_other=11
```

`ret=0` is `epoll_ctl` success.  `tfd_ev=20000019` is the `fdinfo` mask with
`EPOLLWAKEUP` (0x20000000) plus `EPOLLIN|EPOLLERR|EPOLLHUP` (0x19).  The
baseline `/sys/kernel/debug/wakeup_sources` has 11 unrelated sources; the
post baseline has 13 (a new `eventpoll` source and a new per-item source).
The injection leaves 11 and no `eventpoll` entry.

The ADD control under the corresponding wakeup-source allocation (`N=3` in
the ADD trace; the same `wakeup_source_register -> ep_create_wakeup_source`
stack) behaves correctly:

```
TARGET epoll_add_wakeup, injected N=3, 5/5 boots:
  ret=-1 errno=12 (ENOMEM), verdict OK_PRE,
  obs=...|tfd=0;tfd_ev=0;ws_total=11;ws_ep=0;ws_other=11
```

so the item is removed and no wakeup source is left behind.  The difference is
the unchecked return in `ep_modify()`.

### 3.3 Scale and stability

Final campaign (`logs/20261019-round2-final/`, 5 boots):

| target | injected N | MOD returns 0 | missing one source | both sources present | violations |
|---|---:|---:|---:|---:|---:|
| `epoll_mod_wakeup` | 257 | 257 | 234 (117 ep source, 117 item source) | 23 | **234** |
| `epoll_add_wakeup` | 275 | 30 | — | 30 | **0** |

The 23 `OK_POST` sites are allocations which the sysfs/uevent path tolerates
(not every injected allocation makes the registration fail).  Site counts per
boot were 50-52 for MOD and 55/55 for ADD; the expected variation is the
`kobject_uevent`/kernfs optional allocations.  The violation count was 45-47
per boot in a separate 5-boot run (`logs/20261019-epoll-wakeup-mass/`) and
47, 45, 45, 47, 45 in the final campaign.  `N=1` produced a violation in
5/5 boots in both campaigns; the ADD control at the wakeup allocation was
clean in 5/5.

Two shapes were observed, both from the same defect: 117 trials had no
per-epoll source (`ws_ep=0`), 117 had the per-epoll source but no per-item
source (`ws_ep=1, ws_other` unchanged).

### 3.4 Latest mainline and prior discussion

- **Runtime reproduction on v7.3-rc2.**  A second kernel was built from tag
  `v7.3-rc2` in a separate worktree (`~/dropwise-linux-work73/`) and both
  epoll targets were run for five boots
  (`logs/20261019-v73-epoll/`).  Result: `epoll_mod_wakeup` 257 injected
  sites, **234 violations**, 23 `OK_POST`; `epoll_add_wakeup` 271 sites,
  **0 violations** (243 `OK_PRE`, 28 `OK_POST`); 5/5 boots stable.  `N=1`
  on v7.3-rc2 returned 0 with `tfd_ev=20000019`, `ws_total=12, ws_ep=0`
  (v7.3-rc2 has one more unrelated wakeup source at boot than v6.19); the ADD
  control at the wakeup allocation returned `-ENOMEM` with no item.  The
  verdict counts are identical to v6.19 at the aggregate level.
- v7.3-rc2 and v7.3-rc6 (`origin/master`, `a90ee4305c4a`, 2026-10-04) have the
  same unchecked call; the eventpoll blobs are identical (`e0c4bf88a838`).
  The defect is therefore still present in the latest mainline tree available
  in the local repository at test time.
- The local kernel repository is a shallow clone and its per-tag histories do
  not contain the intermediate commits for `fs/eventpoll.c`, so a reliable
  `git log -S` over v6.19..v7.3 was not possible.  A web search for
  `ep_create_wakeup_source` / `ep_modify` / `EPOLLWAKEUP` found only the
  original 2012-2013 EPOLLWAKEUP discussions and unrelated bug reports; no
  report of this ignored return value was found.  This is "not found in a
  non-exhaustive search", not "provably unreported".
- No email, patch, issue or comment was sent anywhere.

### 3.5 Triage

**Confirmed kernel issue (minor, non-security).**  The declared contract is
`epoll_ctl(2)` success ⇒ `EPOLLWAKEUP` is in effect.  The kernel returns
success without registering the source.  This is exactly the round's
"P returns success but the observation does not equal post" violation class.
The fix would be to create the source before committing the new mask (or to
restore the old mask and return the error); that is a suggestion, not a patch
submitted anywhere.

## 4. Results table

Final campaign, 5 boots per target/mechanism, 1013 injected trials.
`OK_POISONED` is 0 everywhere and is omitted from the per-row list only
because no row used it.  "sites" is the number of injected N values summed
over five boots; "misses" is a fail_function injection-point attempt that was
not reached.

| target | mechanism | sites | trials | OK_PRE | OK_POST | OK_PARTIAL | OK_POISONED | violations | unknown |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| `scm_rights` | fail-nth | 15 | 15 | 15 | 0 | 0 | 0 | 0 | 0 |
| `pipe_write` | fail-nth | 10 | 10 | 5 | 0 | 5 | 0 | 0 | 0 |
| `tmpfs_pwrite` | fail-nth | 5 | 5 | 5 | 0 | 0 | 0 | 0 | 0 |
| `epoll_ctl_add` | fail-nth | 15 | 15 | 15 | 0 | 0 | 0 | 0 | 0 |
| `fcntl_setlk` | fail-nth | 20 | 20 | 15 | 5 | 0 | 0 | 0 | 0 |
| `accept4_unix` | fail-nth | 40 | 40 | 38 | 2 | 0 | 0 | 0 | 0 |
| `tls_tx` | fail-nth | 40 | 40 | 40 | 0 | 0 | 0 | 0 | 0 |
| `tls_tx` | fail_function | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| `tls_rx` | fail-nth | 50 | 50 | 50 | 0 | 0 | 0 | 0 | 0 |
| `tls_rx` | fail_function | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| `overlay_copyup` | fail-nth | 166 | 166 | 162 | 4 | 0 | 0 | 0 | 0 |
| `overlay_copyup` | fail_function | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| `mq_send_full` | signal | 40 | 40 | 40 | 0 | 0 | 0 | 0 | 0 |
| `pipe_sig_block` | signal | 40 | 40 | 40 | 0 | 0 | 0 | 0 | 0 |
| `pipe_sig_partial` | signal | 40 | 40 | 0 | 0 | 40 | 0 | 0 | 0 |
| `epoll_mod_wakeup` | fail-nth | 257 | 257 | 0 | 23 | 0 | 0 | **234** | 0 |
| `epoll_add_wakeup` | fail-nth | 275 | 275 | 245 | 30 | 0 | 0 | 0 | 0 |
| **total** | | **1013** | **1013** | **670** | **64** | **45** | **0** | **234** | **0** |

All 70 boots had `stable=1`; all 1013 judged trials had a matching baseline
pair.  The 234 violations are the one issue in §3, not 234 independent bugs.

First-round regression: all six round-1 targets still run and still pass
their round-1 contracts.  Site counts shifted slightly with the larger kernel
configuration and per-boot allocation state: `scm_rights` 3/3/3/3/3 in this
campaign (round 1: 3), `accept4_unix` 8 every boot (round 1: 8), `fcntl_setlk`
4, `epoll_ctl_add` 3, `pipe_write` 2, `tmpfs_pwrite` 1.  In an earlier 5-boot
run `scm_rights` showed 2 sites in one boot; in the final campaign `accept4`
had 2 `OK_POST` trials (injected allocation did not prevent the accept) where
round 1 had none.  Both are legal results and examples of the ordinal
instability discussed in §6.

## 5. What the new targets saw

- **`tls_tx`.**  Pre: Q's 5 bytes reach the peer as `50 50 50 50 50`
  (`plain=1`, `tx_conf=0`).  Post: the peer reads 34 bytes starting
  `17 03 03` (`tx_conf=1`).  All 40 injected N values left the socket
  plaintext.  A failure never left a configured TX context behind.
- **`tls_rx`.**  Pre: Q reads 16 bytes of raw TLS record (`b0=0x17, b1=0x03,
  b2=0x03`, `rx_conf=0`).  Post: Q's `recv` returns one byte `H` (0x48) and
  `rx_conf=1`.  All 50 injected N values left Q reading ciphertext.
- **`overlay_copyup`.**  162/166 error trials had no upper file: a true pre
  state.  Four injected trials in the final campaign, and seven in the earlier
  [`logs/20261019-round2-mass/`](logs/20261019-round2-mass/) run,
  returned error but left a **complete** upper copy (4096 bytes, first byte
  `L`, mode 0644) after the copy-up had already committed and a later
  `alloc_empty_backing_file()` (trace N=31:
  `kmem_cache_alloc_noprof <- alloc_empty_backing_file <- backing_file_open <-
  ovl_open_realfile <- ovl_open`) failed.  This is accepted by the target's
  `error_ok` as a legal pre-like internal artifact: the merged view has the
  same content and mode, and the next user sees the same file.  It is not
  counted as a violation; a partial upper copy would be.
- **`mq_send_full`.**  40/40 signal trials returned `EINTR` (errno 4); the
  survivor drained exactly the four filler messages and `mq_curmsgs` was 0;
  no fifth message was enqueued.  Post: the survivor drains `F0,F1,F2,P`.
- **`pipe_sig_block`.**  40/40 returned `EINTR` with 0 `P` bytes and all 65536
  filler bytes intact.  **`pipe_sig_partial`**: 40/40 returned exactly 8192
  after the first page was queued; survivor saw 57344 filler bytes followed
  by exactly 8192 `P` bytes, no extra byte.  All 120 signal trials had
  `waited=1`.
- **`epoll_mod_wakeup` / `epoll_add_wakeup`.**  See §3.  The `fdinfo` mask in
  every violating MOD trial already contained `EPOLLWAKEUP`; the survivor's
  `epoll_wait` still delivered the event (`wait_ev=1`, data 0x1234), which is
  why the API looked healthy from the event-delivery side.

## 6. Injection-point identification and coverage

### 6.1 N is not a stable identifier

`fail-nth` ordinals depend on per-boot allocation order.  Observed in this
round: MOD 50-52 sites, ADD 55, overlay 33 (one 34), `accept4` 8,
`scm_rights` 2-3.  Trace runs often had one or two fewer sites than the mass
run because a trace-only optional allocation did not happen.  Therefore every
site is identified by **the per-run ordinal N plus the trace stack**, and the
mass campaign is judged by ordinal only.

### 6.2 Per-N trace coverage

Each N was re-run in its own boot with `dw.n=N` and `failslab` /
`fail_page_alloc` verbose=2, so the log contains exactly one fault dump.
`trace_sites.py` reduces each dump to the fault capability and the first six
kernel frames.

| target | enumerated N | traced and attributed | coverage | missing |
|---|---:|---:|---:|---|
| `tls_tx` | 8 | 8 | 100% | — |
| `tls_rx` | 10 | 9 | 90% | N=10 did not inject in the trace run (mass did) |
| `overlay_copyup` | 33 | 32 | 97% | N=30 did not inject in the trace run |
| `epoll_mod_wakeup` | 52 | 49 | 94% | N=51/52 are optional uevent allocations not reached under tracing |
| `epoll_add_wakeup` | 55 | 52 | 95% | N=54/55 not reached under tracing |
| **all traced** | **158** | **150** | **94.9%** | 8 |

The two missing TLS/overlay sites are the same ones the mass run judged
`OK_POST`/error; the epoll misses are optional allocations.  The defect sites
are fully covered: every one of the 49 traced MOD sites is in the
`ep_create_wakeup_source` chain, and the ADD control has the same chain at
N=3.

Representative stacks (from `trace_sites.csv`):

```
tls_tx N=1   __kmalloc_cache_noprof <- tls_set_sw_offload <- tls_setsockopt <- do_sock_setsockopt
tls_tx N=2   __kmalloc_node_noprof <- crypto_alloc_tfmmem <- crypto_create_tfm_node <- tls_set_sw_offload
tls_tx N=8   __kmalloc_cache_noprof <- gf128mul_init_4k_lle <- ghash_setkey <- crypto_gcm_setkey
tls_rx N=1   __kmalloc_cache_noprof <- tls_set_sw_offload <- tls_setsockopt
overlay N=23 __kmalloc_noprof <- iter_file_splice_write <- ... <- ovl_copy_up_file
overlay N=24 fail_page_alloc: alloc_pages_mpol <- shmem_alloc_folio <- shmem_get_folio
overlay N=31 kmem_cache_alloc_noprof <- alloc_empty_backing_file <- backing_file_open <- ovl_open_realfile
epoll MOD N=1 __kmalloc_cache_noprof <- wakeup_source_register <- ep_create_wakeup_source <- do_epoll_ctl
epoll ADD N=3 __kmalloc_cache_noprof <- wakeup_source_register <- ep_create_wakeup_source <- do_epoll_ctl
```

The full mapping, including the 17 kernfs/device frames, is in
`trace_sites.csv` in the two trace directories.

### 6.3 fail_function coverage

The guest's `/sys/kernel/debug/fail_function/injectable` contains **985
symbols: 980 syscall wrappers and 5 non-syscall functions**:

```
__filemap_add_folio          ERRNO
should_failslab              ERRNO
should_fail_alloc_page       TRUE
should_fail_bio              ERRNO
should_fail_net_realloc_skb  TRUE
```

Every syscall wrapper is annotated (`include/linux/syscalls.h`,
`arch/x86/include/asm/syscall_wrapper.h`), but overriding a wrapper returns
before the syscall body runs and is a trivial pre state, so it was not used
as a target injection point.  The two target-relevant non-syscall candidates
were tried:

- `should_fail_net_realloc_skb` on `tls_tx`/`tls_rx`: registered, never called
  by `setsockopt` (5/5 boots, `times` stayed 1).
- `__filemap_add_folio` on overlay copy-up: registered, never called.  On
  tmpfs the upper file is written through `shmem_add_to_page_cache()`
  (`mm/shmem.c:881`), which has its own page-cache insertion and does not call
  `__filemap_add_folio()`.  This is a filesystem-path limitation, not a
  harness failure.

**Target-level fail_function coverage is therefore 0 of 2 candidates** (15
site-boot attempts, all misses), although the mechanism itself works: 70/70
boots injected `__x64_sys_getpid` with `-EINVAL`, and `times` read back 0.
Reaching the remaining error branches would need a block-backed overlay
(upper on a non-shmem fs) or additional `ALLOW_ERROR_INJECTION` annotations,
which were not added to the kernel.

The task-filter path of fail_function did not work on this kernel: with
`task-filter=1` and `make-it-fail=1` written by the same task (and by a fresh
child), `should_fail()` returned false and `times` stayed 1; with
`task-filter=0` the same probe injected immediately.  The cause was not
established (未核实), so all target runs use a global one-shot with
`times=1` written last, immediately before P.  The guest is otherwise idle, so
the window for another task to consume the shot is small; `times` is read back
after P and only 0 counts as an injection.

### 6.4 Signal coverage

The runner waits until the P thread is seen in state `S` in
`/proc/self/task/<tid>/stat` before applying the delay.  For every one of the
120 signal trials the log shows `waited=1`; the delay distribution is
0/200 µs/2 ms/20 ms after entering the wait, two trials each.  `pipe_sig_block`
and `mq_send_full` returned `EINTR`; `pipe_sig_partial` returned exactly the
8192-byte prefix.  There were no "signal too late" misses.

## 7. Stability

- 70/70 boots `done=1`; 70/70 `stable=1` (two pre samples equal, two post
  samples equal); 0 `UNKNOWN`; 0 hangs.
- Verdict reproducibility for the confirmed issue: 5/5 final-campaign boots
  had 45-47 MOD violations, and a separate 5-boot campaign had 47/45/45/47/45;
  the `N=1` reproduction and the ADD control were identical in all 10 boots.
- The only baseline strings are one distinct pre and one distinct post per
  target per boot; cross-boot pre/post strings match for every target (checked
  in `trials.csv`).
- N counts vary by ±1-2 for some targets (see §6.1).  This does not change a
  verdict in these runs because each mass trial is still judged against its
  own boot's baselines, but it is the main reason a production harness should
  identify sites by name rather than ordinal.

### Framework errors found and fixed during round 2

These were harness errors, not kernel findings.

1. **ADD setup flag was zeroed by `memset`.**  `epwake_add_setup()` set
   `c->add=1` and then `epwake_setup()` memset the context, so the "ADD"
   target actually ran MOD; the pre baseline showed a `tfd`, which exposed it.
   Fixed by initialising the context in the wrapper before calling the shared
   setup.
2. **Partial/poison predicates read the parent's copy of the context.**  The
   survivor and observer run in the trial child, so `&C` in the worker is
   stale.  This produced 8 false `VIOL_MIXED` for `pipe_sig_partial` (the
   observation was exactly the legal prefix) and 3 false `VIOL_ERR_POST` for
   overlay.  Fixed by serialising the needed values through `o->aux` and
   `o->kid` (same class of bug as round 1, bug 5).
3. **The signal `waited` flag was overwritten by the observer.**  It was stored
   in `o->aux[0]`, which `sigpipe_observer()` reuses for `FIONREAD`; all logs
   printed `waited=0` even though the state check had returned 1.  Fixed with
   a dedicated `outcome.waited` field, and the final campaign shows 120/120.
4. **fail_function task filter did not match.**  See §6.3; the workaround is a
   global one-shot.  Cause unresolved.
5. **`ff_read_injectable()` truncated the list at one `read()`.**  The list is
   985 lines (~64 KiB), and one procfs read returned only one page, so the
   first run saw 142 entries.  Fixed by reading until EOF.
6. **Trace parser assumed no timestamp prefix.**  Every console line starts
   with `[ 0.123]`, so the first trace pass attributed 0 frames; fixed by
   stripping the prefix before parsing.

## 8. Feasibility conclusion

The judgement engine keeps working: two calibrated baselines, a forked
survivor on the same object and an independent observer classified 1013
injected trials with 0 unknowns and 0 false positives.  The signal mechanism
is a real step up from round 1: it reaches EINTR and legal partial-write
states with the P thread provably in an interruptible wait (120/120
`waited=1`), and it needed no kernel change.  The epoll result is the first
confirmed "returns success, did not complete" defect in this prototype; it is
small, but it is exactly the shape the project is looking for and it was
found by the ordinary baseline/survivor/observer machinery, with a clean
ADD-vs-MOD control.

The main obstacles remain, in order:

1. **Coverage of the injection mechanism.**  fail-nth reaches only
   `should_fail()` sites; in practice these are slab and page allocations.
   The stock fail_function whitelist is tiny for non-syscall functions (5
   symbols here), and the one plausible overlay function is not on the tmpfs
   path.  Signals cover only interruptible waits.  Reaching the error branches
   that matter most still needs a bigger mechanism set or kernel annotations.
2. **Identifying the injection point.**  The N ordinal is not portable and
   varies by ±1-2 even in a controlled guest.  Per-N single-boot tracing gives
   94.9% attribution here, at the cost of one boot per site; it is good enough
   for a paper/prototype but not for a mass production harness.
3. **Contracts and observability.**  The overlay "complete upper copy after a
   failed open" shows that a legal internal state can differ from both
   baselines; the target needed an explicit `error_ok` predicate.  The
   observer sees only userspace-visible or debugfs state.
4. **Cost of target construction.**  A target with a meaningful survivor and
   an independent observer is still the expensive part; the judgement itself
   is cheap (milliseconds per trial, ~1.4 s per boot).

A standalone project is justified if it keeps the engine and invests in
mechanisms plus site naming; the biggest single obstacle is the lack of a
generic, nameable fault-injection interface that reaches protocol/module error
branches without a custom kernel.

## 9. Deviations and unfinished work

- Five logical target groups were run (TLS, overlay, mqueue, pipe signals,
  EPOLLWAKEUP), not the requested 3-4, because the EPOLLWAKEUP pair was added
  as a priority target after the first campaign.
- `io_uring` cancellation (optional mechanism 3) was not implemented.  No
  io_uring target was attempted.
- The round-1 `accept4` and `fcntl` targets were not extended with signal
  interruption; only pipe and mqueue got signal variants.  The round-1
  targets still run unchanged in fail-nth mode.
- fail_function target coverage is 0/2; the mechanism works but no non-trivial
  stock injectable function is on the tested paths.  `should_failslab` /
  `should_fail_alloc_page` were not used as target sites because they only
  duplicate allocation failure and, with the global one-shot, the arming path
  itself can consume the shot.
- The confirmed issue was reproduced at runtime on both v6.19 and v7.3-rc2,
  and inspected in v7.3-rc6 source; see §3.4.
- No prior discussion of this exact ignored return was found, but the local
  kernel history is shallow and the web search was not exhaustive.
- The overlay legal-artifact classification is a judgement call: a complete
  upper copy after a failed `open()` is treated as pre-like because the next
  user sees the same merged content and mode.  A reviewer could decide that
  the upper-directory side effect itself should be reported; it is not counted
  here.

## 10. Cost

Measured pieces (host: 16 cores, 30 GB, QEMU/KVM):

- v6.19 kernel build with the round-2 options: **2 min 27 s** measured from
  the build log creation (12:47:21) to `bzImage` (12:49:48); the source
  worktree was already present from round 1, so this was a config-change
  rebuild, not a cold build.  `bzImage` 14.2 MB; object tree + source outside
  the repository under `~/dropwise-linux-work/`.
- v7.3-rc2 kernel: source worktree creation plus a cold `defconfig` build in a
  fresh object tree took **2 min 56 s** (13:05:46 to 13:08:42); `bzImage`
  14.8 MB under `~/dropwise-linux-work73/`.  The five-boot v7.3-rc2 epoll
  cross-check took 6.1 s (MOD) + 6.0 s (ADD).
- Final campaign: 70 boots, summed QEMU wall time **95.9 s** (1.0-1.7 s per
  boot; the epoll targets are the slowest at ~1.7 s).  The guest-side target
  time is 1-12 ms for the round-1 targets, 10-70 ms for the signal targets
  (most of it the deliberate waits), and ~60-70 ms for the epoll targets.
- Site tracing: 158 single-N boots (51 TLS/overlay + 107 epoll), about
  **10 min** wall including the per-boot runner overhead; logs are 2.2 MB and
  4.7 MB.
- Logs for the final campaign: 952 KB; focused epoll campaign 352 KB; build
  products 6.5 MB in the repository.
- Total session wall clock for round 2 (environment check, kernel rebuild,
  harness extension, 14 targets, three mechanisms, campaigns, tracing,
  cross-check and these notes): approximately **2.5-3 hours**; the measured
  build and campaign pieces above are from timestamps and runner output, the
  rest is approximate.

## 11. Verification status

Trial counts, verdicts, baselines, signal `waited` flags, fail_function
`times` values, trace frames and coverage percentages are taken directly from
the logs under `logs/20261019-round2-final/`,
`logs/20261019-epoll-wakeup-mass/`, `logs/20261019-125706-trace-n/` and
`logs/20261019-130011-trace-n/`.  Kernel line numbers are from the v6.19 tree
in `~/dropwise-linux-work/linux`; the latest-mainline statement compares
`v7.3-rc2:fs/eventpoll.c` with `origin/master:fs/eventpoll.c` (identical blob
`e0c4bf88a838`).  The local kernel repository is shallow, so its git history
is not a complete record; the "no prior discussion found" statement is a
non-exhaustive search result.  The fail_function task-filter failure is
observed but not diagnosed (未核实).  No email, patch or issue was sent
anywhere.  Round-1 [`RESULTS.md`](RESULTS.md) was not modified.
