# Results round 3: can the EPOLLWAKEUP MOD failure happen for real, and what is the fix?

**Status.** The round-2 finding is confirmed and its practical impact is now
known.  Without any fault injection, `epoll_ctl(EPOLL_CTL_MOD)` with
`EPOLLWAKEUP` can return 0 with no wakeup source in a **SELinux-enabled**
system when the caller is in a memory cgroup at `memory.max` and survives the
charge failure.  The accounted allocation is not the kernfs node itself; it is
the `security.selinux` xattr that
`security_kernfs_init_security()` attaches to each new kernfs node, which
`simple_xattr_set()` allocates with `GFP_KERNEL_ACCOUNT`.

Measured on v7.3-rc2: **60/60** MOD calls returned 0 with both sources missing
across three boots with a loaded SELinux policy, labelled
`/sys/devices/virtual` and `/sys/class/wakeup`, `memory.max=1M`, and
`oom_score_adj=-1000`.  The same setup without SELinux,
or without the memory limit, gave **0/20** failures.  A near-limit variant
(`memory.max=current-4KiB`) gave **4/20** failures.  The same real trigger also
reproduces on v6.19: **20/20** failures.  This is therefore a real
power-management functional defect, not just a fault-injection artefact; it is
still not a security issue.

A minimal fix was written in the v7.3-rc2 tree
([`patches/0001-eventpoll-propagate-wakeup-source-creation-failure.patch`](patches/0001-eventpoll-propagate-wakeup-source-creation-failure.patch)).
With the fix, the round-2 harness has **0 violations** for
`epoll_mod_wakeup`, the ADD control and all other targets are unchanged, the
new per-eventpoll-source-present case is clean, the epoll selftest passes
**66/66**, and the real SELinux/memcg trigger now returns **-ENOMEM** instead
of 0 (20/20).  Nothing was emailed, committed or pushed.  No file under
`~/Worktrees/linux/` was touched.

Test date 2026-10-19.  Guest kernels: v6.19 (`05f7e89ab973`) and v7.3-rc2
(`df2908090cda`), QEMU/KVM, 2 vCPU, 1 GiB.  All numbers below are from the raw
logs under [`logs/`](logs/); the log directories are listed in §5.

## 1. What the source says: every failure point on the MOD wakeup path

`ep_modify()` ignores the return value of `ep_create_wakeup_source()` in both
trees, while `ep_insert()` checks it:

| shape | v6.19 | v7.3-rc2 |
|---|---|---|
| `ep_insert()` checks, removes the item, returns the error | `fs/eventpoll.c:1629-1635` | `fs/eventpoll.c:1925-1931` |
| `ep_modify()` stores the new mask/data first, calls `ep_create_wakeup_source()` and drops the result, returns 0 | `fs/eventpoll.c:1691-1760`, uncheck at `:1708-1712` | `fs/eventpoll.c:1980-2049`, uncheck at `:1997-2001` |
| `ep_create_wakeup_source()` itself | `fs/eventpoll.c:1489-1509` | `fs/eventpoll.c:1739-1759` |

The path below `ep_create_wakeup_source()` is:

```
ep_create_wakeup_source()
  wakeup_source_register(NULL, "eventpoll")        (per-eventpoll source)
  wakeup_source_register(NULL, dentry_name)        (per-item source)
    wakeup_source_create()
      kzalloc(struct wakeup_source)
      kstrdup_const(name)
      ida_alloc(&wakeup_ida)
    wakeup_source_sysfs_add()
      kzalloc(struct device)
      dev_set_name(dev, "wakeup%d", ws->id)
      device_add(dev)
        device_private_init()
        get_device_parent() -> class glue dir
        kobject_add() -> sysfs_create_dir_ns() -> kernfs_new_node()
          kstrdup_const(name)
          kmem_cache_zalloc(kernfs_node_cache)
          idr_preload() / idr_alloc_cyclic()
          security_kernfs_init_security()
            SELinux: security_context_to_sid() / ... , then
            kernfs_xattr_set() -> simple_xattr_set()
              simple_xattr_alloc()  (kvmalloc, GFP_KERNEL_ACCOUNT)
              kstrdup(name, GFP_KERNEL_ACCOUNT)
        device_create_file(uevent), class symlinks, 11 attribute files
        dpm_sysfs_add()/bus_add_device()  (no-ops here)
        kobject_uevent()                  (return value ignored)
```

### 1.1 Allocation flags and memcg accounting

| failure point | gfp / cache | memcg-charged? | what happens on failure |
|---|---|---|---|
| `struct wakeup_source` kzalloc, `drivers/base/power/wakeup.c:86` (v6.19 and v7.3; v7.3 uses `kzalloc_obj(*ws)`) | `GFP_KERNEL`, kmalloc normal cache | **No** | `wakeup_source_create()` returns NULL; `wakeup_source_register()` NULL; `ep_create_wakeup_source()` -ENOMEM |
| `kstrdup_const("eventpoll")`, `wakeup.c:90` | no allocation for the `.rodata` literal | n/a | never fails |
| `kstrdup_const(dentry name)`, `wakeup.c:90` | `GFP_KERNEL`, kmalloc normal | **No** | same; size controlled by the watched file name |
| `ida_alloc(&wakeup_ida, GFP_KERNEL)`, `wakeup.c:95` | `GFP_KERNEL`; `ida_bitmap`/xarray node; IDA flags are `XA_FLAGS_LOCK_IRQ\|XA_FLAGS_ALLOC`, **no `XA_FLAGS_ACCOUNT`** | **No** | `ida_alloc()` < 0; name+ws freed; NULL |
| `struct device` kzalloc, `drivers/base/power/wakeup_stats.c:144` (v7.3 `kzalloc_obj`) | `GFP_KERNEL` | **No** | `wakeup_source_sysfs_add()` -ENOMEM; `wakeup_source_free()` and NULL |
| `dev_set_name()` -> `kobject_set_name_vargs()` -> `kvasprintf_const(GFP_KERNEL)`, `lib/kobject.c:274` | `GFP_KERNEL`, kmalloc | **No** | `-ENOMEM`, same |
| `struct device_private` kzalloc, `drivers/base/core.c:3534` (v7.3 `:3606`) | `GFP_KERNEL` | **No** | `device_add()` -ENOMEM |
| glue `struct class_dir` kzalloc, `core.c:3223` (v7.3 `:3289-3297`) | `GFP_KERNEL` | **No** | `get_device_parent()` error; `device_add()` -ENOMEM |
| device dir / `uevent` / `subsystem` symlink / class symlink / 11 attribute files: `kernfs_new_node()` | `kernfs_node_cache` created with **`SLAB_PANIC` only** (`fs/kernfs/mount.c:462-464`, both trees), so no `SLAB_ACCOUNT`; names via `GFP_KERNEL` kstrdup | **No** | `kernfs_new_node()` NULL -> `sysfs_create_dir_ns()` -ENOMEM -> `kobject_add()` fails -> `device_add()` -ENOMEM |
| kernfs ino IDR: `idr_preload(GFP_KERNEL)` + `idr_alloc_cyclic(..., GFP_ATOMIC)` | `radix_tree_node_cachep` created `SLAB_PANIC\|SLAB_RECLAIM_ACCOUNT` (`lib/radix-tree.c:1601-1604`); `__GFP_ACCOUNT` explicitly cleared in the preload path | **No** | preload failure is harmless; idr failure -> node freed -> -ENOMEM |
| `security_kernfs_init_security()` -> SELinux `kmalloc(context, GFP_KERNEL)` | `GFP_KERNEL` | **No** | hook error -> node freed -> -ENOMEM |
| **SELinux xattr value and name**: `kernfs_xattr_set()` (`fs/kernfs/inode.c:307-323`) -> `simple_xattr_set()` -> `simple_xattr_alloc()` (`fs/xattr.c:1228-1238` v6.19, `:1258-1271` v7.3) and `kstrdup(name, GFP_KERNEL_ACCOUNT)` (`:1362` v6.19, `:1444` v7.3) | **`kvmalloc(..., GFP_KERNEL_ACCOUNT)` and `GFP_KERNEL_ACCOUNT`** | **Yes**, if the parent kernfs node has a `security.selinux` xattr | `simple_xattr_set()` -ENOMEM -> `kernfs_xattr_set()` -ENOMEM -> `__kernfs_new_node()` fails -> `device_add()` -ENOMEM -> `wakeup_source_register()` NULL -> `ep_create_wakeup_source()` -ENOMEM |
| `kobject_uevent()` allocations, `core.c:3670`/`:3742` | `GFP_KERNEL` | No | return value ignored; cannot fail `device_add()` |

The important correction to a pure static reading: `kernfs_node_cache` and the
radix-tree/IDA caches are **not** accounted, and neither are the ordinary slab
allocations.  The only charged allocation on the path is the SELinux
`security.selinux` xattr created for a new kernfs node when the parent kernfs
node is already labelled.  On a normal SELinux system, `/sys/class/wakeup` and
`/sys/devices/virtual` do carry that xattr; on this host:

```
$ getfattr -m - -d /sys/class/wakeup
security.selinux="system_u:object_r:sysfs_t:s0"
$ getfattr -m - -d /sys/devices/virtual
security.selinux="system_u:object_r:sysfs_t:s0"
```

The guest initramfs starts without an SELinux policy, so the xattr hook would
otherwise return early at `-ENODATA`.  For the experiments below the host
policy (`/etc/selinux/targeted/policy/policy.35`, 3,874,167 bytes) was loaded
into the guest with `/sys/fs/selinux/load`, `enforce` was set to 0 only so the
test could install the labels, and
`system_u:object_r:sysfs_t:s0` was set on `/sys/devices/virtual` and
`/sys/class/wakeup`.  This reproduces the labelled sysfs state of a real
SELinux system; it is not a fault-injection knob.

```
PROBE_SELINUX load size=3874167 written=3874167 errno=0
PROBE_SELINUX enforce=0
PROBE_SELINUX policyvers=35
PROBE_SELINUX setxattr_devices_virtual rc=0 errno=0
PROBE_SELINUX setxattr_class_wakeup rc=0 errno=0
PROBE_SELINUX getxattr_devices_virtual=system_u:object_r:sysfs_t:s0
```

`eventpoll_epi` and `eventpoll_pwq` are `SLAB_ACCOUNT` caches, but
`EPOLL_CTL_MOD` does not allocate an epitem or a poll-table entry; only the
setup `ADD` does.  This is why a tight `memory.max` does not fail MOD unless
the SELinux xattr path is present.

## 2. Task A: real-trigger attempts

All experiments are in the guest.  The observation is the round-2 one: MOD's
return value, `/proc/self/fdinfo/<epfd>` (the advertised event mask), and
`/sys/kernel/debug/wakeup_sources` (the registered `eventpoll` and per-item
sources).  A trial counts as a trigger when MOD returns 0 but the source count
does not increase as a successful MOD must.

### 2.1 memcg limit, no SELinux

Kernel: v7.3-rc2 with `CONFIG_MEMCG=y` (`~/dropwise-linux-work73/build-memcg`).
The guest mounts cgroup2, creates `/sys/fs/cgroup/dwprobe`, enables `memory`,
moves a child into it, allocates 32 MiB (current ~33.6-34.0 MiB), then sets
`memory.max=1M` (far below current; no headroom) and calls MOD with
`EPOLLIN|EPOLLWAKEUP`.  The child is `oom_score_adj=-1000` so a charge failure
must return `-ENOMEM` instead of killing it.

```
PROBE_MEMCG trial=0 ret=0 errno=0 ep=1 other=13 fdinfo_events=20000019 cur_before=33816576 max=1048576 sources=ok
...
PROBE_MEMCG summary trials=20 violations=0 ret_err=0 child_killed=0 base_total=12 base_ep=0
```

| variant | trials | MOD ret=0 with both sources | violations |
|---|---:|---:|---:|
| v7.3-rc2, max=current+4 KiB | 20 | 20 | 0 |
| v7.3-rc2, max=1 MiB | 20 | 20 | 0 |
| v6.19, max=1 MiB | 20 | 20 | 0 |

Permissions: root/PID 1 in the guest to mount cgroup2, create the cgroup and
write `memory.max`; a delegated cgroup would suffice for a normal user.
`EPOLLWAKEUP` needs `CAP_BLOCK_SUSPEND` (see §2.5).  Side effects: none in these
trials; the separate pressure child (below) is OOM-killed, but the MOD child is
not.

Conclusion: a memory limit alone cannot fail this path, because no allocation
on it is charged.  This is the result the source audit predicts.

### 2.2 memcg limit + SELinux labels: the real trigger

Kernel: same v7.3-rc2 MEMCG build (and the v6.19 MEMCG build).  After loading
the policy and setting the two sysfs labels, the same 20-trial experiment was
run.  Now the first new kernfs node needs an accounted `security.selinux`
xattr; `memory.max=1M` makes that charge fail; `oom_score_adj=-1000` keeps the
memcg OOM killer from selecting the child, so the charge returns `-ENOMEM`;
`device_add()` fails; `wakeup_source_register()` returns NULL;
`ep_create_wakeup_source()` returns -ENOMEM; the unpatched `ep_modify()`
ignores it and returns 0.

v7.3-rc2, three separate boots (60 trials):

```
PROBE_MEMCG trial=0 VERDICT=VIOL ret=0 errno=0 base_ep=0 base_other=12 ep=0 other=12 fdinfo_events=20000019 cur_before=33816576 max=1048576
...
PROBE_MEMCG summary trials=20 violations=20 ret_err=0 child_killed=0 base_total=12 base_ep=0
```

with one kernel message per failed device registration per boot:

```
[    0.507811] kobject: kobject_add_internal failed for wakeup (error: -12 parent: virtual)
...
20 occurrences per boot
```

The failing kobject in these runs is the class glue dir
(`kobject_add_internal failed for wakeup ... parent: virtual`), which is
created inside `device_add()`; the same accounted xattr is charged for every
new kernfs node, so a failure on a per-item node is the same class of failure.

| setup | trials | result |
|---|---:|---|
| v7.3-rc2, SELinux labels, `memory.max=1M` | 60 (3 boots) | **60 violations**; ret=0, `fdinfo_events=20000019`, `ws_ep=0`, no source |
| v7.3-rc2, SELinux labels, `memory.max=current-4KiB` | 20 | **4 violations**; 4 kernel `-12` messages |
| v6.19, SELinux labels, `memory.max=1M` | 20 | **20 violations**; base count 11; 20 kernel `-12` messages |
| v7.3-rc2, SELinux labels, no memcg limit | 20 | 0 violations (control) |
| v6.19/v7.3-rc2, no SELinux, `memory.max=1M` | 20 + 20 | 0 violations (control) |

Trigger rate: 100% when the cgroup is far over its limit and the task is
OOM-protected; 20% with only 4 KiB of headroom; 0% without SELinux or without
the limit.  The 1 MiB case is an over-limit cgroup (`max` below current), which
can happen when `memory.max` is lowered or a process with existing charges is
moved in; the `current-4KiB` case is the closer "just at the limit" situation
and is the more conservative rate.

Permissions:

- `EPOLLWAKEUP` requires `CAP_BLOCK_SUSPEND`.  Without it the kernel clears the
  flag in `ep_take_care_of_epollwakeup()` (v6.19 `fs/eventpoll.c:2211-2215`,
  v7.3-rc2 `:2539-2543`) and the MOD path is not taken.  Verified: an
  unprivileged child (`setuid(65534)`) gets ret=0, `fdinfo_events=0x19` (no
  `0x20000000`), and no wakeup source; a root control gets
  `fdinfo_events=0x20000019` and both sources (§2.5).
- `memory.max` needs root or ownership/delegation of the cgroup.
- `oom_score_adj=-1000` needs `CAP_SYS_RESOURCE`.  Without it, the memcg OOM
  killer usually kills the allocating child, so `epoll_ctl()` does not return.
  In the unprotected variant, 12/20 trials logged `child_killed_by=9` and the
  other 8 produced no result; the separate pressure child was always killed.
- Loading the policy and setting labels in this test needed root
  (`CAP_MAC_ADMIN`); on a real SELinux system the policy and sysfs labels are
  already present at boot.

Side effects:

- With the OOM-protected MOD child: no task was killed; the kernel logged the
  20 `kobject_add_internal failed ... -12` messages and the system stayed
  usable.
- The pressure control (`memory.max=1M`, 4 MiB user allocation, not
  OOM-protected) was killed by SIGKILL and `memory.events` went
  `oom_kill=0->1`; this is the intended side effect of the limit, confined to
  the test cgroup.
- Without OOM protection on the MOD child, the memcg OOM killer killed the
  child instead of letting the syscall return (12 explicit SIGKILL logs, 8
  no-result/harness errors).  No task outside the cgroup was killed.

### 2.3 Pending fatal signal

A ptrace experiment stopped a child at `epoll_ctl(2)` syscall entry, queued
`SIGKILL`, and resumed it.  Permissions: root/PID 1 and `CAP_SYS_PTRACE`
(the harness traces its own child).  In 10/10 trials the child was killed
before the syscall-exit ptrace stop; `fdinfo_events=0x19` and the source count
was unchanged, i.e. `ep_modify()` never ran in these trials.  Queuing SIGKILL at
entry is followed by the child being killed before the syscall-exit stop, so
this setup cannot produce a returning MOD with a missing source.

The source confirms why this is not an independent route:
`fatal_signal_pending()`/`task_is_dying()` is only consulted inside the memcg
charge path (`mm/memcontrol.c:106-110` v6.19, `:111-112` v7.3) and in
order>0/CMA reclaim paths, not in the order-0 `GFP_KERNEL` slab allocations on
this path.  If a fatal signal is combined with the SELinux xattr charge, the
charge can fail, but the task is killed at syscall exit and the return value
is not observable.  An OOM-victim task has the same limitation; not tested.

### 2.4 sysfs/device registration failures

`probe_sysfs` created 3000 `epoll` instances, each with an item added with
`EPOLLWAKEUP`, i.e. 3000 per-eventpoll and 3000 per-item wakeup sources
(6000 IDs, 3000 device registrations, ~45,000 kernfs nodes).  Permissions:
root/PID 1, including `CAP_SYS_ADMIN` for the remount and tmpfs tests; the
3000-source loop itself only needs the normal `CAP_BLOCK_SUSPEND`:

```
PROBE_SYSFS base total=12 ep=0 other=12 n=3000
PROBE_SYSFS created=3000 failed=0 elapsed_ms=33 ep=3000 other=3012 total=6012 truncated=0
PROBE_SYSFS after_close ep=0 other=12 total=12
```

No registration failure, no IDA exhaustion, no sysfs limit.  Other attempts:

| attempt | result |
|---|---|
| remount `/sys` read-only, then ADD with EPOLLWAKEUP | `rc=0`; ADD still succeeds (`ep=1 other=13`) |
| mount tmpfs over `/sys/class/wakeup`, create a file named `wakeup0`, then ADD | ADD still succeeds; the kernel's internal kernfs tree is not affected by the VFS mount |
| `rmdir("/sys/class/wakeup/wakeup0")` | `-1 ENOTDIR`; class entries are symlinks, not removable directories |
| duplicate `wakeup%d` name | impossible from userspace: the name comes from the global IDA and IDs are freed only after synchronous device removal |
| `-ENOSPC` from IDA or kernfs inode IDR | needs 2^31 live sources/nodes; not practical |

This matches the source: there is no userspace-controllable sysfs count limit,
and name collisions cannot be induced from a read-only sysfs tree.  A `-ENOMEM`
from a failed registration is possible (that is the memcg/SELinux case), but
not an `-EEXIST`/`-ENOSPC` limit.

### 2.5 Capability prerequisite

`probe_cap` on the v7.3-rc2 build:

```
PROBE_CAP base total=12 ep=0 other=12
PROBE_CAP root ret=0 errno=0 fdinfo_events=20000019 ep=1 other=13
PROBE_CAP unprivileged ret=0 errno=0 fdinfo_events=19 ep=0 other=12
```

The unprivileged child's `EPOLLWAKEUP` bit was silently cleared, so no wakeup
source was requested and no failure path exists.  This is a hard prerequisite,
not a mitigation.

### 2.6 Task A conclusion

- The MOD path has no charged allocations in a plain (non-SELinux) system.
  `memory.max`, fatal signals and sysfs limits cannot make it fail.
- In a SELinux system, the new kernfs node gets an accounted `security.selinux`
  xattr via `simple_xattr_set()` (`GFP_KERNEL_ACCOUNT`).  If the caller is in a
  memory cgroup at its limit and is not killed by the memcg OOM killer, that
  allocation fails and the unpatched MOD returns 0 with no source.
- Real-world impact statement: a process with `CAP_BLOCK_SUSPEND` that uses
  `EPOLL_CTL_MOD` to add `EPOLLWAKEUP`, in an SELinux system, in a memory
  cgroup at `memory.max`, and protected from the memcg OOM killer (or otherwise
  surviving the charge failure), can be told that the wakeup source is active
  when it is not.  The suspend-prevention guarantee of `EPOLLWAKEUP` is
  silently absent; there is no memory corruption or security impact.  The
  window is narrow but not merely theoretical: the trigger is a normal
  allocation failure in a labelled sysfs tree, not a fault-injection site.
- A later MOD retries the allocation, so a subsequent success can mask the
  window.

## 3. Task B: the fix

### 3.1 Choice and shape

Option chosen: **create the wakeup source before committing the new event
mask; on failure return the error with the old mask/data still in place**
(not "store the new mask, then restore it on failure").  Reasons:

1. There is no window in which fdinfo/another thread can observe the new
   `EPOLLWAKEUP` bit before the source exists.
2. There is no rollback of `event.data`/`event.events` and no need to reason
   about an event delivered during the rollback.
3. It is the same ordering as the already-correct `ep_insert()`.

The per-eventpoll source is shared state.  `ep_create_wakeup_source()` creates
`ep->ws` first and the per-item source second.  If this MOD created `ep->ws`
and the per-item creation then fails, no item can be using `ep->ws` (the
invariant is: if `ep->ws` is NULL, no `epi->ws` exists), so the fix unregisters
it and sets `ep->ws = NULL`.  If `ep->ws` already existed, it is used by
another item and must stay; the fix leaves it alone.  This also makes a failed
MOD observationally identical to the pre baseline in the round-2 harness,
which counts both `eventpoll` and per-item sources.

### 3.2 Patch

The full patch is
[`patches/0001-eventpoll-propagate-wakeup-source-creation-failure.patch`](patches/0001-eventpoll-propagate-wakeup-source-creation-failure.patch).
It changes only `fs/eventpoll.c` (29 insertions, 5 deletions) in the
v7.3-rc2 tree.  The core is:

```c
	if ((event->events & EPOLLWAKEUP) && !ep_has_wakeup_source(epi)) {
		bool created_ep_ws = !ep->ws;

		error = ep_create_wakeup_source(epi);
		if (error) {
			if (created_ep_ws && ep->ws) {
				wakeup_source_unregister(ep->ws);
				ep->ws = NULL;
			}
			return error;
		}
	}

	epi->event.events = event->events; /* need barrier below */
	epi->event.data = event->data; /* protected by mtx */
	if (!(epi->event.events & EPOLLWAKEUP) &&
	    ep_has_wakeup_source(epi))
		ep_destroy_wakeup_source(epi);
```

The source is still created before the mask is assigned, the existing
`ep_item_poll()` ordering and the `smp_mb()` barrier are unchanged, and the
removal case destroys the per-item source only after the new mask is in place.
The commit message is in the patch file; it deliberately has no `From:`,
`Signed-off-by` or other author information, per the task.  `checkpatch.pl`
reports `0 errors, 0 warnings`, 52 lines checked.

### 3.3 Build and fail-nth verification

Kernels built from the same v7.3-rc2 source:

- `~/dropwise-linux-work73/build` - unpatched, non-MEMCG (round-2 config).
- `~/dropwise-linux-work73/build-patched` - patched, non-MEMCG.
- `~/dropwise-linux-work73/build-memcg` - unpatched, `CONFIG_MEMCG=y` +
  SELinux (for Task A).
- `~/dropwise-linux-work73/build-patched-memcg` - patched, MEMCG + SELinux.

Command (5 repetitions, the 14 round-2 targets; the auxiliary new case was
run separately in §3.4):

```
cd research/linux
./run.py --kernel ~/dropwise-linux-work73/build-patched/arch/x86/boot/bzImage \
         --targets all --reps 5 --outdir logs/20261019-round3-patched-all5
```

Aggregate, unpatched vs patched (5 boots each; 70/70 boots `done=1`,
`stable=1` in both runs).  These campaigns used the 14 round-2 targets; the
auxiliary `epoll_mod_wakeup_ep` target was added afterwards and is run
separately in §3.4.  `OK_PARTIAL` is omitted from the table except for
`pipe_sig_partial` (40 in both runs):

| target | unpatched trials / OK_PRE / OK_POST / violations | patched trials / OK_PRE / OK_POST / violations |
|---|---|---|
| `epoll_mod_wakeup` | 255 / 0 / 27 / **228** | 255 / **228** / 27 / **0** |
| `epoll_add_wakeup` | 270 / 242 / 28 / 0 | 270 / 244 / 26 / 0 |
| `scm_rights` | 15 / 15 / 0 / 0 | 15 / 15 / 0 / 0 |
| `pipe_write` | 10 / 0 / 10 / 0 | 10 / 0 / 10 / 0 |
| `tmpfs_pwrite` | 5 / 5 / 0 / 0 | 5 / 5 / 0 / 0 |
| `epoll_ctl_add` | 15 / 15 / 0 / 0 | 15 / 15 / 0 / 0 |
| `fcntl_setlk` | 20 / 15 / 5 / 0 | 20 / 15 / 5 / 0 |
| `accept4_unix` | 40 / 40 / 0 / 0 | 40 / 40 / 0 / 0 |
| `tls_tx` (failnth) | 10 / 10 / 0 / 0 | 10 / 10 / 0 / 0 |
| `tls_rx` (failnth) | 20 / 20 / 0 / 0 | 20 / 20 / 0 / 0 |
| `overlay_copyup` (failnth) | 160 / 155 / 5 / 0 | 161 / 155 / 6 / 0 |
| `mq_send_full` | 40 / 40 / 0 / 0 | 40 / 40 / 0 / 0 |
| `pipe_sig_block` | 40 / 40 / 0 / 0 | 40 / 40 / 0 / 0 |
| `pipe_sig_partial` | 40 / 0 / 0 / 0 (40 `OK_PARTIAL`) | 40 / 0 / 0 / 0 (40 `OK_PARTIAL`) |

`epoll_add_wakeup` and the other 12 targets keep their verdict counts; the
only differences are the ±1 site/`OK_POST` variation already seen in round 2
(`overlay_copyup` 160 vs 161; ADD 242/28 vs 244/26).  No target changed from
clean to violating or vice versa.

The patched MOD contract holds in both directions:

- every injected failure now returns `-1/ENOMEM` and leaves the pre state:
  `tfd_ev=19`, `ws_ep=0`, `ws_other` baseline;
- every injected allocation that does not break registration still returns 0
  and leaves the post state: `tfd_ev=20000019`, `ws_ep=1`, one more item
  source.

Unpatched and patched at the same fail-nth sites (rep 1):

```
# unpatched, site 1
TRIAL ... site=1 ret=0 errno=0 ... verdict=VIOL_MIXED
  obs=...|tfd_ev=20000019;ws_total=12;ws_ep=0;ws_other=12

# patched, site 1
TRIAL ... site=1 ret=-1 errno=12 ... verdict=OK_PRE
  obs=...|tfd_ev=19;ws_total=12;ws_ep=0;ws_other=12

# patched, site 7 (allocation tolerated)
TRIAL ... site=7 ret=0 errno=0 ... verdict=OK_POST
  obs=...|tfd_ev=20000019;ws_total=14;ws_ep=1;ws_other=13
```

The cleanup branch is exercised by the sites where the unpatched run shows
`ws_ep=1, ws_other=12` (per-eventpoll source created, per-item source
missing): unpatched sites 27-31 returned 0 with `ws_ep=1`; patched sites
27-31 return `-1/ENOMEM` with `ws_ep=0`, i.e. the newly created per-eventpoll
source was removed again.  Where the unpatched run shows `ws_ep=0` (per-ep
creation itself failed), patched also returns the error and there is nothing
to clean up.

### 3.4 Per-eventpoll source already present

A new target, `epoll_mod_wakeup_ep`, installs a first item with
`EPOLLIN|EPOLLWAKEUP` (so `ep->ws` and one item source exist before P), then
MODs a second item to `EPOLLIN|EPOLLWAKEUP`.  Its observation includes the
target item's fdinfo mask and the source counts; the pre state is `ws_ep=1`,
`ws_other=baseline+1`, and the post state has one more item source.

| kernel | trials | OK_PRE | OK_POST | violations |
|---|---:|---:|---:|---:|
| unpatched v7.3-rc2 | 120 | 0 | 13 | **107** |
| patched v7.3-rc2 | 120 | **107** | 13 | **0** |

5/5 boots each, 0 unknown.  A patched error trial:

```
TRIAL ... ret=-1 errno=12 verdict=OK_PRE
  obs=...|tfd=2;tfd2_ev=19;ws_total=14;ws_ep=1;ws_other=13
```

The pre-existing per-eventpoll source is still there (`ws_ep=1`), the target
item's mask is still old (`tfd2_ev=19`), and no per-item source was added.
An unpatched trial in the same state returns 0 with
`tfd2_ev=20000019` and the same source counts, i.e. the old violation.

### 3.5 Real-trigger verification on the patched kernel

Patched MEMCG+SELinux kernel, same policy/labels/`memory.max=1M`/
`oom_score_adj=-1000`:

```
PROBE_MEMCG trial=0 ret=-1 errno=12 ep=0 other=12 fdinfo_events=19 cur_before=33816576 max=1048576
...
PROBE_MEMCG summary trials=20 violations=0 ret_err=20 child_killed=0 base_total=12 base_ep=0
```

The same SELinux xattr allocation still fails (20 kernel `-12` messages and
20 `device_add` failures), but the fix now returns `-ENOMEM` to userspace,
leaves `tfd_ev=19`, and leaves no wakeup source.  This is the desired
behaviour; the failure is no longer silent.

### 3.6 epoll selftest

`tools/testing/selftests/filesystems/epoll/epoll_wakeup_test.c` was built
statically (`gcc -static -O2 -pthread -I tools/testing/selftests ...`) and
run as `/init` in a minimal initramfs on both kernels:

```
# patched:   # PASSED: 66 / 66 tests passed.
#            # Totals: pass:66 fail:0 xfail:0 xpass:0 skip:0 error:0
# unpatched: # PASSED: 66 / 66 tests passed.
#            # Totals: pass:66 fail:0 xfail:0 xpass:0 skip:0 error:0
```

The kernel panics after the test because PID 1 exits; with `panic=-1` and
`-no-reboot` QEMU still exits 0.  No test failed or was skipped.

## 4. get_maintainer

Command:

```
cd ~/dropwise-linux-work73/linux
./scripts/get_maintainer.pl \
  /home/ava/Desktop/Dropwise/research/linux/patches/0001-eventpoll-propagate-wakeup-source-creation-failure.patch
```

Output:

```
Alexander Viro <viro@zeniv.linux.org.uk> (maintainer:FILESYSTEMS (VFS and infrastructure))
Christian Brauner <brauner@kernel.org> (maintainer:FILESYSTEMS (VFS and infrastructure))
Jan Kara <jack@suse.cz> (reviewer:FILESYSTEMS (VFS and infrastructure))
linux-fsdevel@vger.kernel.org (open list:FILESYSTEMS (VFS and infrastructure))
linux-kernel@vger.kernel.org (open list)
```

There is no epoll-specific entry in `MAINTAINERS`; the patch is in
`fs/eventpoll.c`, so the VFS maintainers/reviewer and the two lists are the
tool's answer.  `get_maintainer.pl --git fs/eventpoll.c` additionally lists
recent commit signers (Greg Kroah-Hartman, Rodrigo Vivi, Peter Zijlstra, Keith
Busch, Breno Leitao) but those are not maintainers of this file and were not
used as recipients.  Because the failure is in wakeup-source sysfs
registration, a sender may also consider a courtesy CC to the SUSPEND TO RAM
and DRIVER CORE lists:

```
./scripts/get_maintainer.pl drivers/base/power/wakeup.c
"Rafael J. Wysocki" <rafael@kernel.org> (maintainer:SUSPEND TO RAM)
Len Brown <lenb@kernel.org> (reviewer:SUSPEND TO RAM)
Pavel Machek <pavel@kernel.org> (reviewer:SUSPEND TO RAM)
Greg Kroah-Hartman <gregkh@linuxfoundation.org> (maintainer:DRIVER CORE, KOBJECTS, DEBUGFS AND SYSFS)
Danilo Krummrich <dakr@kernel.org> (maintainer:DRIVER CORE, KOBJECTS, DEBUGFS AND SYSFS)
linux-pm@vger.kernel.org (open list:SUSPEND TO RAM)
driver-core@lists.linux.dev (open list:DRIVER CORE, KOBJECTS, DEBUGFS AND SYSFS)
linux-kernel@vger.kernel.org (open list)
```

No mail was sent anywhere.  The patch is prepared only; the user adds author
information and decides whether to send it.

## 5. Commands and logs

Kernel builds:

```
# v7.3-rc2, unpatched non-MEMCG (round-2 config)
make O=$HOME/dropwise-linux-work73/build -j$(nproc) bzImage
# patched non-MEMCG
make O=$HOME/dropwise-linux-work73/build-patched -j$(nproc) bzImage
# unpatched MEMCG + SELinux
make O=$HOME/dropwise-linux-work73/build-memcg -j$(nproc) bzImage
# patched MEMCG + SELinux
make O=$HOME/dropwise-linux-work73/build-patched-memcg -j$(nproc) bzImage
# v6.19 unpatched MEMCG + SELinux
make O=$HOME/dropwise-linux-work/build-memcg -j$(nproc) bzImage
```

The MEMCG configs are the round-2 config plus `CONFIG_MEMCG=y` (and
`CONFIG_CGROUP_SCHED`, `CONFIG_CGROUPS`, `CONFIG_MEMCG_SWAP`) applied with
`scripts/config`; v7.3-rc2's normal config already has
`CONFIG_SECURITY_SELINUX=y`, `CONFIG_PM_SLEEP=y` and policy version 35
support.

Round-2 fail-nth campaigns:

```
./run.py --kernel ~/dropwise-linux-work73/build/arch/x86/boot/bzImage \
         --targets all --reps 5 --outdir logs/20261019-round3-unpatched-all5
./run.py --kernel ~/dropwise-linux-work73/build-patched/arch/x86/boot/bzImage \
         --targets all --reps 5 --outdir logs/20261019-round3-patched-all5
./run.py --kernel ...build/... --targets epoll_mod_wakeup_ep --reps 5 \
         --outdir logs/20261019-round3-unpatched-ep5
./run.py --kernel ...build-patched/... --targets epoll_mod_wakeup_ep --reps 5 \
         --outdir logs/20261019-round3-patched-ep5
```

Real-trigger probes (harness has `dw.target=probe_*`; `real_triggers.py` only
boots it and prints `PROBE_*` lines):

```
./real_triggers.py --kernel ...build-memcg/... --probe probe_memcg \
    --outdir logs/20261019-round3-probe-memcg-max1m
./real_triggers.py --kernel ...build-memcg/... --probe probe_memcg_selinux \
    --policy /etc/selinux/targeted/policy/policy.35 \
    --outdir logs/20261019-round3-probe-memcg-selinux-max1m-1   # also -2, -3
./real_triggers.py --kernel ...build-memcg/... --probe probe_memcg_selinux \
    --policy ... --outdir logs/20261019-round3-probe-memcg-selinux   # near-limit
./real_triggers.py --kernel ...build-patched-memcg/... --probe probe_memcg_selinux \
    --policy ... --outdir logs/20261019-round3-patched-memcg-selinux
./real_triggers.py --kernel ~/dropwise-linux-work/build-memcg/... \
    --probe probe_memcg --outdir logs/20261019-round3-v619-memcg-noselinux
./real_triggers.py --kernel ~/dropwise-linux-work/build-memcg/... \
    --probe probe_memcg_selinux --policy ... \
    --outdir logs/20261019-round3-v619-memcg-selinux
./real_triggers.py --kernel ...build-memcg/... --probe probe_selinux_mod \
    --policy ... --outdir logs/20261019-round3-probe-selinux-mod
./real_triggers.py --kernel ...build-memcg/... --probe probe_memcg_selinux_oom \
    --policy ... --outdir logs/20261019-round3-probe-memcg-selinux-oom
./real_triggers.py --kernel ...build/... --probe probe_cap --probe probe_sigkill \
    --outdir logs/20261019-round3-probe-smoke
./real_triggers.py --kernel ...build/... --probe probe_sysfs \
    --outdir logs/20261019-round3-probe-sysfs
./real_triggers.py --kernel ...build-memcg/... --probe probe_selinux \
    --policy ... --outdir logs/20261019-round3-probe-selinux-load
```

Selftest:

```
gcc -static -O2 -pthread -I tools/testing/selftests \
    -I tools/testing/selftests/filesystems/epoll \
    tools/testing/selftests/filesystems/epoll/epoll_wakeup_test.c \
    -o /tmp/epoll_wakeup_test
# /init is /tmp/epoll_wakeup_test; run under QEMU with -no-reboot
```

Logs (all under `research/linux/logs/`):

- `20261019-round3-unpatched-all5/`, `20261019-round3-patched-all5/` -
  the 5-repetition all-target campaigns.
- `20261019-round3-unpatched-ep5/`, `20261019-round3-patched-ep5/` - the
  per-eventpoll-source-present target.
- `20261019-round3-probe-memcg-max1m/`,
  `20261019-round3-probe-memcg/` - non-SELinux memcg controls.
- `20261019-round3-probe-memcg-selinux-max1m-1/` ... `-3/` - 60/60 v7.3-rc2
  real-trigger runs.
- `20261019-round3-probe-memcg-selinux/` - near-limit 4/20 run.
- `20261019-round3-probe-memcg-selinux-oom/` - OOM-unprotected side-effect
  run.
- `20261019-round3-patched-memcg-selinux/` - patched real-trigger run.
- `20261019-round3-v619-memcg-selinux/`,
  `20261019-round3-v619-memcg-noselinux/` - v6.19 cross-checks.
- `20261019-round3-probe-selinux-load/`,
  `20261019-round3-probe-selinux-mod/` - policy-load and no-memcg control.
- `20261019-round3-probe-sysfs/`, `20261019-round3-probe-smoke/` - sysfs,
  capability and ptrace probes.
- `20261019-round3-selftest-patched.log`,
  `20261019-round3-selftest-unpatched.log`.

## 6. Deviations, limits and unfinished work

- **The trigger is narrow.**  It needs SELinux labels on the parent kernfs
  node, a memory cgroup at its limit, and a task that survives the memcg OOM
  charge (we used `oom_score_adj=-1000`, which needs `CAP_SYS_RESOURCE`).
  Without the OOM protection the child is killed and MOD does not return.
  We did not test a real distro with systemd delegation; the test uses an
  initramfs and the host policy.35 loaded by hand.
- **Only policy.35 was tested.**  The host policy was loaded successfully on
  both v6.19 and v7.3-rc2 (`policyvers=35`), and labels were installed with
  `setxattr` in permissive mode.  Other policies, enforcing mode, and a
  system that already has labels at boot were not exercised; the failure is
  in the kernel's label propagation, not in the userspace setup.
- **Near-limit rate is low.**  With `memory.max=current-4KiB` only 4/20 trials
  failed; with `memory.max=1M` all 20 (and 60/60 over three boots) failed.
  The rate depends on how much chargeable memory is in use when the xattr is
  allocated; the 4/20 case is the "just at the limit" example.
- **Fatal signal / OOM victim.**  The ptrace test only shows that queuing
  SIGKILL at syscall entry aborts the syscall before `ep_modify()`; it does
  not produce a confirmed in-body fatal-signal return.  The source analysis
  says `task_is_dying()` can affect the memcg charge, but the task dies at
  syscall exit; no separate user-visible trigger was demonstrated.
- **Global memory exhaustion** (the only way to fail the unaccounted slab
  allocations) was not tested.  With OOM enabled the kernel kills a victim and
  retries; with all tasks OOM-disabled it panics ("System is deadlocked on
  memory"), so there is no clean 0-return observation.  The SELinux/memcg path
  is the practical one.
- **v6.19 was tested unpatched only.**  The patch is written and built for
  v7.3-rc2 as requested; a v6.19 backport was not built or run.
- **No upstream search or contact in this round.**  No email, patch
  submission, issue or comment was sent.  The patch is not committed or
  pushed; the v7.3-rc2 source tree has only the working-tree change.
- **No changes to the Dropwise library.**  Only
  `research/linux/harness/dw_harness.c` (probes and the new target),
  `research/linux/run.py` (extra initramfs file support and the new target
  name), `research/linux/real_triggers.py` (new runner) and
  `research/linux/patches/` were added/changed.  `RESULTS.md` and
  `RESULTS-round2.md` are untouched.

## 7. Verification status

- The 60/60, 20/20, 4/20 and 20/20 numbers are from the raw serial logs listed
  above; the 20-trial summaries and the `kobject_add_internal failed ...
  (error: -12)` counts are direct greps of those logs.
- The fail-nth aggregates are the `aggregate.csv` files in
  `20261019-round3-unpatched-all5/` and `20261019-round3-patched-all5/`;
  every boot had `done=1` and `stable=1`.
- The cleanup evidence is the same-N comparison in
  `13-epoll_mod_wakeup-rep1.log`: unpatched sites 27-31 have `ws_ep=1` and
  return 0; patched sites 27-31 return `-1/ENOMEM` with `ws_ep=0`.
- The selftest counts are from the `# Totals:` lines in the two selftest logs.
- `checkpatch.pl` output is from the patch file; `get_maintainer.pl` output is
  from the v7.3-rc2 tree.
- Kernel line numbers are from the two local trees at the tags above; v7.3-rc2
  `fs/eventpoll.c` and `drivers/base/power/wakeup.c` are the source of the
  patched build.  The `kobject_uevent()` early-return note is static analysis,
  not a runtime result.
- Not verified: real distro/SELinux enforcing behavior, other policies, a
  confirmed in-body fatal-signal return, global-OOM allocation failure,
  v6.19 patched build.


## v2

Test date: 2026-10-19. This section supersedes the v1 cleanup rationale and
source-count expectations in §§3 and 7 above; the original sections and v1
patch are retained as historical results.

### Changes and source-tree state

The actual kernel checkout is `/home/ava/dropwise-linux-work73/linux/`.
Before editing, Git showed HEAD at v7.3-rc2, a shallow repository, and only
`fs/eventpoll.c` modified. Its diff exactly matched the v1 patch. No commit
was made. The v2 working-tree diff is **14 insertions, 5 deletions**, only in
`fs/eventpoll.c`:

- Remove `created_ep_ws` and all failure-time unregister/reset logic.
- Create the per-item wakeup source before assigning either `epi->event`
  field; return the creation error immediately on failure.
- Retain the shared per-eventpoll source until `ep_free()`, just like ADD.
  In v7.3-rc2, `ep_create_wakeup_source()` creates `ep->ws` first;
  `ep_insert()` removes the item on failure but does not unregister `ep->ws`;
  `ep_free()` unregisters it. A failed MOD retaining the same inactive,
  reusable source is harmless, not an item-state rollback failure.
- Update the message accordingly and add the verified Fixes tag.

Artifact:
[`patches/v2-0001-eventpoll-propagate-wakeup-source-creation-failure.patch`](patches/v2-0001-eventpoll-propagate-wakeup-source-creation-failure.patch).
The v1 file is unchanged. Neither patch has new author headers or a
Signed-off-by line.

**No-commit packaging detail:** `git format-patch -v2 -1 --stdout
--zero-commit --no-signature HEAD` supplied the v2 mail framing. Since
`format-patch` normally operates on commits and this task explicitly forbids
committing, the working-tree `git diff --stat` and `git diff` payload and
updated message were substituted, and the unrelated HEAD author/date/from
headers were removed. No temporary commit or commit object was created. This
is a format-patch-framed working-tree export, not an unedited format-patch
export of a new commit. `git apply --reverse --check` against the patched
tree succeeds.

### Fixes verification

The shallow checkout cannot establish the 2012 history. Read-only GitHub API
queries against `torvalds/linux` were used instead:

```
gh api 'repos/torvalds/linux/commits?path=fs/eventpoll.c&since=2012-01-01T00:00:00Z&until=2012-09-01T00:00:00Z&per_page=100'
gh api repos/torvalds/linux/commits/4d7e30d98939a0340022ccd49325a3d70f7e0238
gh api 'repos/torvalds/linux/contents/fs/eventpoll.c?ref=4d7e30d98939a0340022ccd49325a3d70f7e0238'
```

The [commit](https://github.com/torvalds/linux/commit/4d7e30d98939a0340022ccd49325a3d70f7e0238)
is by Arve Hjønnevåg, author date 2012-05-01, with the complete subject used
below. Its diff **adds** the unchecked call in `ep_modify()` (hunk
`@@ -1229,6 +1288,12 @@`), not merely a flag definition. The resulting
[`fs/eventpoll.c`, lines 1288–1296](https://github.com/torvalds/linux/blob/4d7e30d98939a0340022ccd49325a3d70f7e0238/fs/eventpoll.c#L1288-L1296)
has:

```c
	epi->event.events = event->events;
	pt._key = event->events;
	epi->event.data = event->data; /* protected by mtx */
	if (epi->event.events & EPOLLWAKEUP) {
		if (!epi->ws)
			ep_create_wakeup_source(epi);
	} else if (epi->ws) {
		ep_destroy_wakeup_source(epi);
	}
```

The helper already returns `-ENOMEM` on registration failure at lines
1118–1133, whereas `ep_modify()` returns 0 at line 1327. Thus the original
EPOLLWAKEUP commit, not a later refactor, introduced the ignored error:

```
Fixes: 4d7e30d98939 ("epoll: Add a flag, EPOLLWAKEUP, to prevent suspend while epoll events are ready")
```

### Shared ADD/MOD error rule

The old ADD-only `epwake_add_error_ok()` already explicitly permitted a
cached per-epoll source, checking that the interest list was empty and
`epoll_wait()` returned 0. MOD instead required exact observation-string
equality, so a harmless `ws_ep=1` cache would have been called a violation.

Both now use **one** `epwake_error_ok()`:

- On error, allow `ws_ep` 0 or 1 for these fresh-epoll targets; require
  `ws_other` to equal its per-trial pre-operation count and
  `ws_total == ws_ep + ws_other`. No new per-item source is permitted.
- ADD must still have `tfd=0`, no mask, and no returned event.
- MOD must still have `tfd=1`, old mask `0x19`, and deliver the old
  `EPOLLIN` event with data `0x77` (119), not the requested `0x1234` (4660).
- Success still requires exact post-baseline equality; baseline stability
  and generic Unknown handling are unchanged. Source-count reads that fail
  or fill the buffer are not accepted as valid counts.

This is not a MOD-only relaxation: ADD's existing cache allowance is shared
with MOD and tightened to check the per-item-source count explicitly.
In the **v1 ADD data**, 119 failed trials already had `ws_ep=1`,
`ws_other=12`, `ws_total=13`, an empty interest list and no delivered event
(the other 125 errors had `ws_ep=0`). Example: v1 all5, run 14, site 29.
In the **v2 ADD data**, 118 errors have that same cached-source shape and
125 have no per-epoll source; all are clean under the shared rule. V2 MOD
has 115 cached-source errors and 116 errors without the source.

Representative v2 observations (all5, repetition 1):

```
MOD site=27 ret=-1 errno=12 verdict=OK_PRE
  wait_rc=1;wait_errno=0;wait_ev=1;wait_data=119|tfd=1;tfd_ev=19;ws_total=13;ws_ep=1;ws_other=12
ADD site=29 ret=-1 errno=12 verdict=OK_PRE
  wait_rc=0;wait_errno=0;wait_ev=0;wait_data=0|tfd=0;tfd_ev=0;ws_total=13;ws_ep=1;ws_other=12
```

An initial attempt at the new predicate incorrectly read child-local context
from the judging parent. It was stopped and discarded, not counted below.
The corrected observer transports the ADD/MOD flag and pre-source count
through the existing outcome `aux` fields; the complete campaign was rerun.
As a negative control with this final harness, the unpatched kernel still
produces **46 violations / 51 MOD trials** (5 OK_POST), while ADD has
**0 / 54** (48 OK_PRE, 6 OK_POST). The new rule does not hide the original
success-without-a-source bug.

### Injection results

Both v2 configurations were rebuilt successfully using the existing
`build-patched` and `build-patched-memcg` configurations. Runtime checks
below ran only in QEMU/KVM guests, with the round-3 scripts/harness.

```
cd /home/ava/Desktop/Dropwise/research/linux
./run.py --kernel /home/ava/dropwise-linux-work73/build-patched/arch/x86/boot/bzImage --targets all --reps 5 --outdir logs/20261019-round3-v2-all5
```

**75/75 boots completed and were stable**, without harness errors. All
measured mechanisms have zero violations and zero Unknown. V1 comparison
is the existing round-3 `patched-all5` plus `patched-ep5` outputs, not a new
v1 run. PRE/POST/PARTIAL below are the harness's OK verdict counts:

| target | mechanism | v1 trials / PRE / POST / PARTIAL | v2 trials / PRE / POST / PARTIAL | v2 violations / unknown |
|---|---|---|---|---|
| `scm_rights` | failnth | 15 / 15 / 0 / 0 | 14 / 14 / 0 / 0 | 0 / 0 |
| `pipe_write` | failnth | 10 / 0 / 10 / 0 | 10 / 0 / 10 / 0 | 0 / 0 |
| `tmpfs_pwrite` | failnth | 5 / 5 / 0 / 0 | 5 / 5 / 0 / 0 | 0 / 0 |
| `epoll_ctl_add` | failnth | 15 / 15 / 0 / 0 | 15 / 15 / 0 / 0 | 0 / 0 |
| `fcntl_setlk` | failnth | 20 / 15 / 5 / 0 | 20 / 15 / 5 / 0 | 0 / 0 |
| `accept4_unix` | failnth | 40 / 40 / 0 / 0 | 40 / 40 / 0 / 0 | 0 / 0 |
| `tls_tx` | failnth | 10 / 10 / 0 / 0 | 10 / 10 / 0 / 0 | 0 / 0 |
| `tls_tx` | failfunc | 0 / 0 / 0 / 0 | 0 / 0 / 0 / 0 | 0 / 0 |
| `tls_rx` | failnth | 20 / 20 / 0 / 0 | 21 / 20 / 1 / 0 | 0 / 0 |
| `tls_rx` | failfunc | 0 / 0 / 0 / 0 | 0 / 0 / 0 / 0 | 0 / 0 |
| `overlay_copyup` | failnth | 161 / 155 / 6 / 0 | 160 / 156 / 4 / 0 | 0 / 0 |
| `overlay_copyup` | failfunc | 0 / 0 / 0 / 0 | 0 / 0 / 0 / 0 | 0 / 0 |
| `mq_send_full` | signal | 40 / 40 / 0 / 0 | 40 / 40 / 0 / 0 | 0 / 0 |
| `pipe_sig_block` | signal | 40 / 40 / 0 / 0 | 40 / 40 / 0 / 0 | 0 / 0 |
| `pipe_sig_partial` | signal | 40 / 0 / 0 / 40 | 40 / 0 / 0 / 40 | 0 / 0 |
| `epoll_mod_wakeup` | failnth | 255 / 228 / 27 / 0 | 256 / 231 / 25 / 0 | 0 / 0 |
| `epoll_add_wakeup` | failnth | 270 / 244 / 26 / 0 | 270 / 243 / 27 / 0 | 0 / 0 |
| `epoll_mod_wakeup_ep` | failnth | 120 / 107 / 13 / 0 | 120 / 107 / 13 / 0 | 0 / 0 |

ADD and all other targets retain the same clean/violating classification as
v1; the individual site and PRE/POST counts are **not exactly identical**.
For the non-epoll count differences, an additional five-boot run of
`scm_rights,tls_rx,overlay_copyup` with the **same v2 kernel and final
harness** gave respectively `15/15/0/0`, `20/20/0/0`, and `161/155/6/0`
(trials/PRE/POST/PARTIAL), exactly matching their v1 aggregates. All 15
repeat boots completed/stabilized with zero violations/Unknown. This
confirms run-to-run variation without changing the patch; the specific
allocation-stack cause of each changed site was **未核实** in this task.
The three fail-function mechanisms each still have 0 injected trials and
5 misses; those are not positive coverage claims.

### Real trigger and selftest

```
python3 real_triggers.py --kernel /home/ava/dropwise-linux-work73/build-patched-memcg/arch/x86/boot/bzImage --probe probe_memcg_selinux --policy /etc/selinux/targeted/policy/policy.35 --outdir logs/20261019-round3-v2-memcg-selinux
```

Policy.35 was loaded, the two sysfs labels installed, and the same
`memory.max=1M`, `oom_score_adj=-1000` setup used. **20/20** trials returned
userspace `ret=-1, errno=12` (kernel `-ENOMEM`); all retained
`fdinfo_events=19`, `ep=0`, `other=12`. Summary:

```
PROBE_MEMCG summary trials=20 violations=0 ret_err=20 child_killed=0 base_total=12 base_ep=0
```

The separate pressure control was killed by SIGKILL and `oom_kill=0->1`, as
in v1; no MOD test child was killed. The real-trigger probe observes the
mask, not the data field; old-data preservation is observed in the injection
campaign above. Real distro/enforcing-mode behavior remains **未核实**.

`tools/testing/selftests/filesystems/epoll/Makefile` lists only
`epoll_wakeup_test`. It was rebuilt statically from this checkout and run
as `/init` under the v2 non-MEMCG guest, as in round 3. Output:

```
# PASSED: 66 / 66 tests passed.
# Totals: pass:66 fail:0 xfail:0 xpass:0 skip:0 error:0
```

After the totals, the successful PID 1 exits with code 0, so the guest logs
`Attempted to kill init! exitcode=0x00000000`; `panic=1` plus `-no-reboot`
ends QEMU. This is the direct-/init test-runner shutdown, not a failing
selftest or a claim of panic-free boot. QEMU exited 0.

### checkpatch and get_maintainer

Commands run **from the v7.3-rc2 kernel checkout**:

```
scripts/checkpatch.pl --strict /home/ava/Desktop/Dropwise/research/linux/patches/v2-0001-eventpoll-propagate-wakeup-source-creation-failure.patch
```

Unmodified strict output (exit 1):

```
ERROR: Missing Signed-off-by: line(s)

total: 1 errors, 0 warnings, 0 checks, 37 lines checked

NOTE: For some of the reported defects, checkpatch may be able to
      mechanically convert to the typical style using --fix or --fix-inplace.

/home/ava/Desktop/Dropwise/research/linux/patches/v2-0001-eventpoll-propagate-wakeup-source-creation-failure.patch has style problems, please review.

NOTE: If any of the errors are false positives, please report
      them to the maintainer, see CHECKPATCH in MAINTAINERS.
```

The sole error is the deliberately absent Signed-off-by requested for the
user to add. It was not silently ignored or fixed by inventing an identity.
With only that check disabled:

```
scripts/checkpatch.pl --strict --no-signoff /home/ava/Desktop/Dropwise/research/linux/patches/v2-0001-eventpoll-propagate-wakeup-source-creation-failure.patch
total: 0 errors, 0 warnings, 0 checks, 37 lines checked

/home/ava/Desktop/Dropwise/research/linux/patches/v2-0001-eventpoll-propagate-wakeup-source-creation-failure.patch has no obvious style problems and is ready for submission.
```

Thus all code/message style checks pass: **0 errors, 0 warnings, 0 checks,
37 lines checked**; raw `--strict` alone is not claimed to pass.

```
scripts/get_maintainer.pl /home/ava/Desktop/Dropwise/research/linux/patches/v2-0001-eventpoll-propagate-wakeup-source-creation-failure.patch
Alexander Viro <viro@zeniv.linux.org.uk> (maintainer:FILESYSTEMS (VFS and infrastructure))
Christian Brauner <brauner@kernel.org> (maintainer:FILESYSTEMS (VFS and infrastructure))
Jan Kara <jack@suse.cz> (reviewer:FILESYSTEMS (VFS and infrastructure))
"Rafael J. Wysocki" <rjw@rjwysocki.net> (blamed_fixes:1/1=100%)
NeilBrown <neil@brown.name> (blamed_fixes:1/1=100%)
"Arve Hjønnevåg" <arve@android.com> (blamed_fixes:1/1=100%)
linux-fsdevel@vger.kernel.org (open list:FILESYSTEMS (VFS and infrastructure))
linux-kernel@vger.kernel.org (open list)
```

These are the script's current outputs, including identities derived from
the Fixes tag; no recipient was contacted.

### Outputs and final boundaries

Runtime outputs are the existing runners' normal logs/CSVs under:

- `logs/20261019-round3-v2-all5/`
- `logs/20261019-round3-v2-repeat5/`
- `logs/20261019-round3-v2-unpatched-control/`
- `logs/20261019-round3-v2-memcg-selinux/`
- `logs/20261019-round3-v2-selftest.log`

`git diff --check` and the kernel-tree diff check pass. V1 is retained,
`RESULTS-round2.md` is unchanged, no Dropwise library code changed, and
nothing under `/home/ava/Worktrees/linux/` was modified. No commit, push,
email, submission, issue or comment was made. All QEMU guests were stopped;
final process inspection found no remaining QEMU process.

### First public submission (2026-10-19)

After explicit user authorization, the submission was reduced to a concise
commit message and the inaccurate `ep_insert() has the same ordering`
comment was replaced. Removing comments from the final source and the
VM-tested v2 source gives identical text; no functional change was made
and the runtime tests above were not rerun for this comment-only revision.

A single signed commit was created on `codex/eventpoll-mod-wakeup-error`
in `/home/ava/dropwise-linux-work73/linux/`, with `Assisted-by: LLM`.
`git format-patch -1 --base=HEAD^` generated the first public `[PATCH]`;
test results were placed below `---`, and SMTP headers were added without
changing the inline body. Internal v1 and v2 artifacts remain unchanged.
The final email is at
`/home/ava/Desktop/linux/outgoing/eventpoll-wakeup/0001-eventpoll-propagate-wakeup-source-creation-failure-i.patch`.
Full `scripts/checkpatch.pl --strict` passes with **0 errors, 0 warnings,
0 checks, 36 lines checked**, and reverse-apply checking succeeds.

Sent from `hi@tychen.cc` to Alexander Viro, Christian Brauner and Jan Kara,
with the filesystem/kernel lists, Rafael J. Wysocki, Arve Hjønnevåg and
NeilBrown copied, as selected by `get_maintainer.pl`. SMTP accepted all
recipients (`refused={}`). Read-only IMAP `BODY.PEEK` verified the direct
self-copy in INBOX and confirmed its decoded inline body matches the
outgoing message. Message-ID:
`<179240281988.1700986.8309527041821666235@tychen.cc>`.
This confirms SMTP acceptance and self-copy delivery, not list delivery or
maintainer acceptance. No Git branch was pushed and no file under
`/home/ava/Worktrees/linux/` was modified.
