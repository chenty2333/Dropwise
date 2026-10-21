# V4-1 multicast delivery and patch audit (2026-10-21)

Addendum to `RESULTS-round4.md`, section 4 (V4-1
`ip_add_membership`).  This file adds evidence only: the existing
campaign logs and the existing sections of `RESULTS-round4.md` were not
modified.  No commit, push, email, patch submission, or upstream contact
was made.  The test ran in isolated QEMU/KVM guests; `~/dropwise-linux-r4`
was used read-only apart from the already present test build, and no host
network setup was changed.

New logs: `logs/20261021-r4-mcast-delivery/`.
New test source/runner: `mcast-delivery/dw_mcast_delivery.c`,
`mcast-delivery/run.py`.

## 1. Why a second test was needed

The round-4 harness observed `IP_ADD_MEMBERSHIP` return 0 with the
socket-side membership present (`IP_MSFILTER` succeeds) and the
device-side group missing (`/proc/net/igmp` has no `030201EF`).  It did
not send a datagram.  "No delivery" was therefore an inference from the
missing device group, not an observed result.  This audit tests the
actual receive path with a real UDP datagram, and checks whether the
patch's clean `-ENOBUFS` failure also leaves no state behind.

The test distinguishes three things that can each stop a multicast
datagram:

1. a network-configuration failure (no route/interface, a wrong
   `IP_MULTICAST_IF`, or a loopback source rejected by source
   validation);
2. the injected kernel failure (the device group is absent);
3. an unrelated socket/bind failure.

The normal, N=1, N=2 and SSM scenarios therefore send a unicast control
to the same receiver and, where relevant, also run a normal-join positive
control.  The cross-socket scenario uses its own before-close successful
delivery as the control.

## 2. Delivery matrix (observed)

All numbers are from the logged runs.  `users=-1` means the target group
is absent from `/proc/net/igmp`; the all-hosts `010000E0` entry remains.
`mcast` is received/sent; `unicast` is received/sent.  Errno numbers:
105 `ENOBUFS`, 99 `EADDRNOTAVAIL`, 98 `EADDRINUSE`, 22 `EINVAL`,
19 `ENODEV`.

| kernel | scenario | join | `IP_MSFILTER` | group users | unicast | mcast | conclusion |
|---|---|---|---:|---:|---:|---:|---|
| v6.19 stock | normal | `0` | `0` | `1` | 1/1 | **4/4** | positive control, real delivery |
| v7.3-rc2 stock | normal | `0` | `0` | `1` | 1/1 | **4/4** | positive control, real delivery |
| v7.3-rc2 fixed | normal | `0` | `0` | `1` | 1/1 | **4/4** | patch does not change normal join |
| v6.19 stock | fail-nth N=2 | **`0`** | **`0`** | **absent** | 1/1 | **0/4** | success without device group; no delivery |
| v7.3-rc2 stock | fail-nth N=2 | **`0`** | **`0`** | **absent** | 1/1 | **0/4** | same |
| v7.3-rc2 fixed | fail-nth N=2 | **`-1`/105** | `-1`/99 | absent | 1/1 | 0/4 | clean failure, no socket state |
| v7.3-rc2 fixed | N=2 then normal join | `0` | `0` | `1` | 1/1 | **2/2** | failed call left nothing behind |
| all three | fail-nth N=1 | `-1`/105 | `-1`/99 | absent | 1/1 | 0/2 | clean `sock_kmalloc` failure |
| all three | N=1 then normal join | `0` | `0` | `1` | 1/1 | **2/2** | failure is recoverable by a later join |
| v6.19 stock | `IP_ADD_SOURCE_MEMBERSHIP` N=2 | **`0`** | `0` | absent | 1/1 | 0/2 | same silent success on the SSM path |
| v7.3-rc2 stock | `IP_ADD_SOURCE_MEMBERSHIP` N=2 | **`0`** | `0` | absent | 1/1 | 0/2 | same |
| v7.3 fixed | `IP_ADD_SOURCE_MEMBERSHIP` N=2 | **`-1`/105** | `-1`/99 | absent | 1/1 | 0/2 | clean failure |
| v6.19 stock | invalid group `127.0.0.1` | `-1`/22 | `-1`/99 | absent | n/a | n/a | `EINVAL`, no state |
| v7.3 fixed | invalid ifindex `9999` | `-1`/19 | `-1`/99 | absent | n/a | n/a | `ENODEV`, no state |

The machine-readable table is
`logs/20261021-r4-mcast-delivery/matrix.csv`.

### 2.1 Exact evidence snippets

Normal positive control (`v73-stock.log`):

```
STATE tag=normal_join ret=0 errno=0 injected=0 readback=-1 msf=0 msf_errno=0 igmp_users=1
UNICAST_SEND tag=normal_post ret=13 errno=0 dst=127.0.0.1:45678
UNICAST_RECV tag=normal_post got=1 src=127.0.0.1:38851 bytes=13
DELIVERY tag=normal_joined multicast_sent=4 multicast_recv=4 expected=4
```

Stock N=2 (`v73-stock.log`; v6.19 is the same):

```
STATE tag=n2 ret=0 errno=0 injected=1 readback=0 msf=0 msf_errno=0 igmp_users=-1
UNICAST_SEND tag=n2 ret=4 errno=0 dst=127.0.0.1:45678
UNICAST_RECV tag=n2 got=1 src=127.0.0.1:46470 bytes=4
MCAST_SEND tag=n2_after_inject seq=0 ret=19 errno=0 dst=239.1.2.3:45678
MCAST_RECV tag=n2_after_inject seq=0 got=0
...
DELIVERY tag=n2_after_inject multicast_sent=4 multicast_recv=0 expected=4
DELIVERY_MISS tag=n2_after_inject recv=0 expected=4
DUPJOIN tag=n2 ret=-1 errno=98
IGMP tag=n2_dupjoin present=0 users=-1 snapshot=...010000E0 1...
IGMP tag=n2_after_1s present=0 users=-1 snapshot=...010000E0 1...
```

Fixed N=2 (`v73-fixed.log`):

```
STATE tag=n2 ret=-1 errno=105 injected=1 readback=0 msf=-1 msf_errno=99 igmp_users=-1 msf_means=no_socket_membership
UNICAST_RECV tag=n2 got=1 src=127.0.0.1:41110 bytes=4
DELIVERY tag=n2_after_inject multicast_sent=4 multicast_recv=0 expected=4
REJOIN tag=n2 ret=0 errno=0
IGMP tag=n2_rejoin present=1 users=1 snapshot=...030201EF 1...
DELIVERY tag=n2_rejoined multicast_sent=2 multicast_recv=2 expected=2
```

Clean N=1 (`v73-stock.log`; same on v6.19 and fixed):

```
STATE tag=n1 ret=-1 errno=105 injected=1 readback=0 msf=-1 msf_errno=99 igmp_users=-1 msf_means=no_socket_membership
DELIVERY tag=n1_no_join multicast_sent=2 multicast_recv=0 expected=2
REJOIN tag=n1 ret=0 errno=0
DELIVERY tag=n1_rejoined multicast_sent=2 multicast_recv=2 expected=2
```

## 3. Configuration and controls

- Guest: QEMU/KVM, 2 vCPU, 1 GiB, no NIC other than `lo`; exact command
  lines are in `v619-stock.cmd`, `v73-stock.cmd`, `v73-fixed.cmd`.
  Kernel SHA-256s are in the matching `.meta` files.
- `lo`: index 1, `127.0.0.1/8`, `IFF_UP|IFF_RUNNING`; logged as
  `NET lo_ifindex=1 lo_up=1 addr=127.0.0.1 route_localnet=1`.
- `/proc/sys/net/ipv4/conf/lo/route_localnet=1` before any test.  This is
  deliberate: a looped-back multicast packet has source `127.0.0.1`, and
  `ip_mc_validate_source()` rejects a loopback source as `IP_LOCALNET`
  when `route_localnet=0` (`net/ipv4/route.c:1753-1754`).  Without this,
  the positive control would fail for a reason unrelated to the group.
  The same setting is used in every scenario.
- Sender: `IP_MULTICAST_IF` = `{127.0.0.1, ifindex=1}`,
  `IP_MULTICAST_LOOP=1`, `IP_MULTICAST_TTL=1`.
- Route state: `/proc/net/route` main table contained only its header in
  all three boots (`ROUTE table=main snapshot=Iface Destination ...`); no
  `239.0.0.0/8` or default route was added.  The sender selects `lo`
  explicitly through `IP_MULTICAST_IF`; the 4/4 normal delivery shows that
  this is sufficient for this test, and it avoids a route lookup failure
  being mistaken for the injection effect.
- Receiver: UDP socket bound to `INADDR_ANY:45678` (second receiver:
  `:45679`).  Join uses the same shape as the round-4 harness:
  `imr_multiaddr=239.1.2.3`, `imr_address=127.0.0.1`, `imr_ifindex=0`;
  `ip_mc_find_dev()` resolves that to `lo` and rewrites the ifindex
  (`net/ipv4/igmp.c:2004-2022`).
- Every scenario runs in a fresh forked child with fresh sockets; the
  parent waits for the child and then logs `/proc/net/igmp` again.  A
  unicast datagram to `127.0.0.1:45678` from the same sender to the same
  receiver is sent before and after the join.  It is received in 100% of
  runs (15/15 logged unicast sends), so a missing multicast datagram is
  not a broken socket, bind, or route.
- The injected child arms
  `/proc/self/task/<tid>/fail-nth` immediately before the tested
  `setsockopt`, and no `printf`, allocation, or socket call occurs
  between the arm and the `setsockopt`.  `readback=0` is the
  fail-nth-consumed proof, as in round 4.

### 3.1 Why the datagram is actually dropped when the group is missing

The receive path checks the device-side group before it reaches UDP
socket lookup:

```
net/ipv4/route.c:2546   our = ip_check_mc_rcu(in_dev, daddr, saddr,
net/ipv4/route.c:2565       ) {
net/ipv4/route.c:2566           reason = ip_route_input_mc(...);
net/ipv4/route.c:2569   return reason;
```

`ip_check_mc_rcu()` walks `in_dev->mc_list` (`net/ipv4/igmp.c:2912-2934`).
With no device group and no multicast forwarding, `our` is 0 and
`ip_route_input_rcu()` returns the default drop reason without calling
`ip_route_input_mc()`.  The socket-side `inet->mc_list` is checked later
by `ip_mc_sf_allow()` (`net/ipv4/igmp.c:2851-2856`), so socket-side
membership alone is not enough.  This is why the observed 0/4 is a
device-group effect, not a test artifact.

## 4. Patch audit

Patch: `patches/0002-ipv4-igmp-report-interface-group-allocation-failure.patch`.
Base: v7.3-rc2 `df2908090cda368b01ff43709f51890076c56157`.
`git apply --check --reverse` on `~/dropwise-linux-r4` succeeds; the
patch is exactly the applied diff (26 insertions, 9 deletions).  The
fixed kernel used in the test is `bzImage-fixed` from that tree.

### 4.1 Allocation/free pairing and `sock_kfree_s` size

- `iml = sock_kmalloc(sk, sizeof(*iml), GFP_KERNEL)`
  (`net/ipv4/igmp.c:2393`) charges `sk_omem_alloc` by `sizeof(*iml)`
  (`net/core/sock.c:2889-2906`).
- On `____ip_mc_inc_group()` failure, the patch does
  `sock_kfree_s(sk, iml, sizeof(*iml))` (`net/ipv4/igmp.c:2406`).
  `sock_kfree_s()` kfree()s and subtracts exactly the supplied size
  (`net/core/sock.c:2929-2944`), so the size matches and the socket
  option-memory accounting is balanced.
- `iml` is not linked at that point: the patch moved `iml->next_rcu` and
  `rcu_assign_pointer(inet->mc_list, iml)` after the inc call
  (`net/ipv4/igmp.c:2409-2410`).  `sock_kfree_s()` is therefore a
  non-RCU free of an object no reader can reach; no `kfree_rcu()` is
  needed.  `iml->sflist` is still NULL at this point
  (`net/ipv4/igmp.c:2398`), so there is no inner source list to free.
- Success path unchanged: `iml` is linked once, later freed by
  `ip_mc_drop_socket()` with the same size, `kfree_rcu()`
  (`net/ipv4/igmp.c:2895-2906`).
- The existing-group branch (`WRITE_ONCE(im->users, im->users + 1)`,
  `net/ipv4/igmp.c:1601-1604`) returns 0; no allocation and no free
  changes.

### 4.2 Error path under RTNL

`IP_ADD_MEMBERSHIP` is one of the options for which
`setsockopt_needs_rtnl()` returns true (`net/ipv4/ip_sockglue.c:640-657`),
and `do_ip_setsockopt()` takes `rtnl_lock()` before the switch
(`net/ipv4/ip_sockglue.c:1076-1078`).  `__ip_mc_join_group()` has
`ASSERT_RTNL()` (`net/ipv4/igmp.c:2361`).  The failure path, including
`sock_kfree_s()`, is therefore under RTNL, same as before the patch.
No new lock ordering or sleepable operation was introduced.

### 4.3 Duplicate join, existing group, and source-filter callers

- Duplicate join on the *same* socket hits the pre-existing
  `err = -EADDRINUSE` loop before any allocation
  (`net/ipv4/igmp.c:2382-2388`).  Test: `DUPJOIN ... errno=98` on both
  stock and fixed; no state change.  The patch does not touch this.
- The `users++` branch is for a *different* socket joining an existing
  device group.  Test `two_sockets`: users 1 -> 2, duplicate same-socket
  join `EADDRINUSE` leaves 2, close of the second socket -> 1, and the
  first socket still receives multicast (1/1).  This is unchanged on the
  fixed kernel.  The patch's return type returns 0 on this branch
  (`net/ipv4/igmp.c:1601-1604`), so existing-group joins still succeed.
- `IP_ADD_SOURCE_MEMBERSHIP` calls `ip_mc_join_group_ssm()`, i.e. the
  same `__ip_mc_join_group()` with `MCAST_INCLUDE`
  (`net/ipv4/ip_sockglue.c:1304-1312`).  The patch propagates
  `-ENOBUFS`; the caller breaks out before `ip_mc_source()`.  Test
  confirms: stock returns 0 with no device group, fixed returns
  `-1/ENOBUFS` with no state.
- On the stock kernel the SSM problem is worse than just the join: if
  the join "succeeds" without a device group, `ip_mc_source()` reaches
  `ip_mc_add_src()`, which returns `-ESRCH` when the group is missing
  (`net/ipv4/igmp.c:2283-2286`), but that return is ignored at
  `net/ipv4/igmp.c:2631-2632` and `err = 0` was set just before.  The
  test's stock SSM result is therefore `ret=0`, socket filter present,
  device group absent, no delivery.  The patch avoids this by failing
  the join before the source list is built.
- `MCAST_JOIN_GROUP` calls `ip_mc_join_group()` directly
  (`net/ipv4/ip_sockglue.c:863`), so the new error propagates.
  `MCAST_JOIN_SOURCE_GROUP` also propagates any error except
  `-EADDRINUSE` (`net/ipv4/ip_sockglue.c:756-758`), so `-ENOBUFS`
  propagates before `ip_mc_source()`.  These callers were read, not
  separately tested.
- The patch keeps `void __ip_mc_inc_group()` / `void
  ip_mc_inc_group()` unchanged; all non-join callers keep the old
  behavior.  Callers found: `ip_mc_up()` (`net/ipv4/igmp.c:1959`),
  GRE tunnel setup (`net/ipv4/ip_gre.c:965`), bridge all-snoopers group
  (`net/bridge/br_multicast.c:4239`), and InfiniBand CMA
  (`drivers/infiniband/core/cma.c:445`).  None has an error channel to
  userspace at that point.

### 4.4 Publication ordering and readers

Order in the patched join:

```
____ip_mc_inc_group()          -> device group allocated and published
                                   (net/ipv4/igmp.c:1626-1636)
if (err) { free iml; return; } -> no socket-side entry
iml->next_rcu = inet->mc_list;
rcu_assign_pointer(inet->mc_list, iml);  -> socket-side entry published
```

So a socket-side `iml` is never published without the device group
having been created.  `iml` is fully initialized (`memcpy`, `sflist`,
`sfmode`) before either publication.

Read side:

- `ip_mc_sf_allow()` walks `inet->mc_list` under RCU
  (`net/ipv4/igmp.c:2851-2856`).
- `ip_check_mc_rcu()` walks `in_dev->mc_list` under RCU
  (`net/ipv4/igmp.c:2912-2934`).
- Writers (`join`, `leave`, `drop_socket`, `ip_mc_up/down`) run under
  RTNL; RCU readers are not serialized by RTNL.  The patch's ordering
  means the only possible transient during a successful join is
  "device group visible, socket entry not yet", which is benign because
  no socket can receive yet.  It removes the persistent stock state in
  which the socket entry exists indefinitely while the device group does
  not.
- Notification order also changes: `igmp_group_added()` and
  `RTM_NEWMULTICAST` for the device group happen before the socket entry
  is linked.  A netlink listener can therefore observe the device group
  before the socket membership; the reverse (socket-only) is no longer
  produced by a failing join.
- Residual, not tested: whether a lockless reader that observes the new
  `inet->mc_list` entry is guaranteed to observe the earlier
  `in_dev->mc_list` store on every weakly ordered architecture.  Both
  sides use RCU publication (`rcu_assign_pointer()` at
  `net/ipv4/igmp.c:1628` and `:2410`), and the stores are ordered on
  x86, but this audit did not run a memory-model test.  RTNL alone does
  not order RCU readers; the safety argument here is "device group
  created before socket entry is published", not "all RCU readers see
  both atomically".

### 4.5 The void wrappers and the "next query repairs it" claim

The patch comment says the interface-up/init wrappers can ignore the
error because "a missing group is corrected by the next IGMP query".
That claim is not supported by the code:

- `igmp_heard_query()` processes a query by iterating
  `for_each_pmc_rcu(in_dev, im)` over the *existing device-side* list
  (`net/ipv4/igmp.c:1102-1109`).  A group that failed to allocate is not
  in that list, so the query never visits it and cannot create it.
- `ip_mc_up()` first calls `ip_mc_inc_group(in_dev, IGMP_ALL_HOSTS)`
  and then iterates `for_each_pmc_rtnl(in_dev, pmc)` over the existing
  device list (`net/ipv4/igmp.c:1952-1966`).  It does not scan
  `inet->mc_list`; a missing group is not re-added there either.
- `ip_mc_add_src()` returns `-ESRCH` when the device group is missing
  (`net/ipv4/igmp.c:2283-2286`), and the caller in `ip_mc_source()`
  ignores it (`net/ipv4/igmp.c:2631-2632`), so a source-filter update is
  not a repair path either.

Test evidence is consistent: after the stock N=2 failure, a 1-second
wait left the group absent, and a duplicate join on the same socket
returned `EADDRINUSE` without creating it.  The only observed recovery
was a *different* socket's normal join, which created the device group
with users=1; that is a new membership, not a query/report repair.

For the wrapper callers, a later `ip_mc_up()` (an interface down/up
transition) is a retry of the same allocation, so that is a possible
recovery for the all-hosts group at interface-up.  It is not a query
repair, and it is not tested here because there is no second interface
in the guest.  For a failed socket join, there is no equivalent retry:
duplicate join returns `EADDRINUSE`, and a query never sees the missing
group.

Conclusion for the patch: keeping the wrappers `void` is a defensible
minimal-scope choice because those callers cannot report errors, but the
comment's recovery claim is false and should be replaced.  A correct
statement is: these callers cannot report the failure; the group may
remain absent until another successful allocation path (a new join, or
another interface-up), and no automatic query/report repair is
guaranteed.  The patch does not attempt to fix that pre-existing
limitation.

### 4.6 Cross-socket consequence of publishing `iml` before the inc

The stock N=2 state is not only "the socket cannot receive".  The
socket-side `iml` exists without a device-side reference.  On close,
`ip_mc_drop_socket()` unconditionally decrements the device group for
every `iml` (`net/ipv4/igmp.c:2895-2906`), and
`__ip_mc_dec_group()` removes the group when `users` reaches 0
(`net/ipv4/igmp.c:1844-1853`).

The `n2_cross` scenario demonstrates the consequence:

```
# stock v6.19 / v7.3-rc2
STATE tag=n2_cross ret=0 ... msf=0 igmp_users=-1
SECOND_JOIN tag=n2_cross ret=0 errno=0
IGMP tag=n2_cross_second_joined present=1 users=1
DELIVERY tag=n2_cross_before_close multicast_sent=2 multicast_recv=2 expected=2
MSF tag=n2_cross_fd2_after_close_first rc=0 errno=0
IGMP tag=n2_cross_after_close_first present=0 users=-1
DELIVERY tag=n2_cross_after_close multicast_sent=2 multicast_recv=0 expected=2

# fixed v7.3-rc2
STATE tag=n2_cross ret=-1 errno=105 ... msf=-1 igmp_users=-1
SECOND_JOIN tag=n2_cross ret=0 errno=0
IGMP tag=n2_cross_second_joined present=1 users=1
MSF tag=n2_cross_fd2_after_close_first rc=0 errno=0
IGMP tag=n2_cross_after_close_first present=1 users=1
DELIVERY tag=n2_cross_after_close multicast_sent=2 multicast_recv=2 expected=2
```

That is: the failed-inc socket's close removes a group that a second
socket successfully joined, while the second socket's `IP_MSFILTER`
still succeeds.  The second socket then receives nothing.  This is a
stronger user-visible consequence than the original "no delivery" and a
direct reason the patch must both (a) propagate the error and (b) not
publish `iml` on failure.  It is still reached only via fail-nth in this
audit; no real-world frequency is claimed.

## 5. Classification: observed facts versus inference

**Observed (new logs):**

- Normal join on the configured `lo` delivers real UDP multicast:
  4/4 on v6.19 stock, v7.3-rc2 stock, and v7.3-rc2 fixed.
- Stock v6.19 and v7.3-rc2 at N=2: `IP_ADD_MEMBERSHIP` returns 0;
  `IP_MSFILTER` returns 0; the target group is absent from
  `/proc/net/igmp`; unicast works; multicast is 0/4.  Duplicate join is
  `EADDRINUSE`; the group is still absent after 1 s.
- Fixed v7.3-rc2 at N=2: returns `-1/ENOBUFS`; `IP_MSFILTER` returns
  `-1/EADDRNOTAVAIL`; no group; unicast works; multicast 0/4.  A normal
  join afterwards returns 0, creates the group with users=1, and
  multicast delivers 2/2.
- N=1 on all three kernels: `-1/ENOBUFS`, no socket/device state,
  multicast 0/2 before a normal join and 2/2 after.
- Stock `IP_ADD_SOURCE_MEMBERSHIP` at N=2 also returns 0 with no device
  group and no delivery; fixed returns `-1/ENOBUFS` with no state.
- Invalid group (`127.0.0.1`) returns `-1/EINVAL`; invalid ifindex
  returns `-1/ENODEV`; neither leaves state.
- Two normal sockets: group users 1 -> 2; duplicate same-socket join
  `EADDRINUSE`; close second -> users 1; first still receives.
- Stock cross-socket close removes the second socket's group (users 1
  -> absent) while its `IP_MSFILTER` still succeeds; fixed does not.

**Inference (supported by code, not directly observed):**

- The missing device group is why the datagram is dropped; the receive
  route check in `net/ipv4/route.c:2546-2569` runs before UDP socket
  lookup and `ip_check_mc_rcu()` reads only `in_dev->mc_list`.
- No IGMP report is sent when the device group is absent
  (`____ip_mc_inc_group()` returns before `igmp_group_added()`), but
  this audit did not capture IGMP packets.  The round-4 report's "no
  IGMP report" is a code inference, not a new observation.
- A query cannot repair the missing group, for the code reason in
  section 4.5; no query packet was sent in this test.
- Frequency in real deployments is not established.  The only trigger
  used is `fail-nth` on an ordinary `GFP_KERNEL` allocation; no
  memory-pressure trigger was constructed.

**Classification:** confirmed contract violation on the injected failure
path (success without the joined group, now with real no-delivery), plus
an observable cross-socket premature-removal consequence.  It is not
contract-external: `ip(7)` defines `IP_ADD_MEMBERSHIP` as joining the
group on the interface, and no documented exception allows success
without the interface group.  It is fault-injection-only as far as
reachability evidence goes; this audit does not claim real-world
frequency, memory corruption, or security impact.

## 6. Corrections to the round-4 wording

Append-only corrections; the original text was not edited.

1. **§0 / §4.1 "no delivery"**: now directly supported by 0/4 real UDP
   datagrams on both stock kernels and 4/4 in the positive control.
   Cite this file/logs.  The statement "`/proc/net/igmp` empty" should
   be "the target group is absent; the all-hosts `010000E0` entry
   remains" — the file itself is not empty.
2. **§4.2/§4.4 "until the socket is closed"**: closing the stock N=2
   socket is not neutral.  It can decrement and remove a device group
   created by another socket (section 4.6).  The patch prevents this by
   not publishing `iml` on failure.
3. **§4.4 "a missing group there is corrected by the next IGMP query"**:
   unsupported and contradicted by `igmp_heard_query()` /
   `ip_mc_up()` iterating only existing device groups.  Replace with "not
   recovered by a query/report; a later successful allocation path (a
   new join, or another interface-up for the all-hosts group) may create
   it."  The same sentence in the patch's commit message should be
   corrected before any submission; the code behavior is unaffected.
4. **§4.4 "existing-group `users++` branch"**: clarify that a duplicate
   join on the same socket returns `EADDRINUSE` before that branch; the
   branch is for a second socket joining the same group.
5. **§4.5 "no socket/device state"**: now directly proven for the fixed
   N=2 (msf `-1/99`, group absent, delivery 0/4, normal join afterwards
   works 2/2).
6. **"no IGMP report"**: keep as inference unless a packet capture is
   added; this audit did not capture IGMP.
7. **Patch commit message, "Everything runs under RTNL, so no other
   thread can observe the half-built membership"**: overbroad.  RTNL
   serializes writers, not RCU readers.  After this patch no half-built
   `iml` is published at all, which is the real reason the failure path
   is safe; the sentence should say that rather than claim RTNL excludes
   every reader.

## 7. Limits and remaining uncertainty

- Only `lo`; no veth/NIC/hardware multicast filter.  The receive drop is
  in generic IPv4 input, but hardware-filter and IGMP-report behavior on
  a real device were not measured.
- `route_localnet=1` is part of the test configuration so the loopback
  source survives source validation.  It is applied identically to
  positive and injected scenarios; it is not the cause of the 0/4.
- No IGMP packet capture; no query packet was sent.  The "query cannot
  repair" conclusion is code reading, not a packet-level test.
- The wrapper/all-hosts failure path was not injected (no second
  interface); the audit covers its code only.
- fail-nth site identity is the same inference as round 4 (N ordering
  plus error behavior), not a kernel stack trace.  `readback=0` proves
  the N-th `should_fail()` was consumed in these runs.
- The cross-socket effect is demonstrated on stock v6.19 and v7.3-rc2;
  no claim about how often a real allocation failure produces the
  initial half-state.
- Upstream search was limited to local refs and public web search.
  `origin/master` (f47d2236, 2026-10-19) still has the void signature
  and the socket-publish-before-inc ordering; no equivalent fix was
  found there.  `lore.kernel.org` queries returned HTTP 403 in this
  environment, so a definitive archive search was not possible.

## 8. Baseline/upstream status

- Patch base is exactly v7.3-rc2 `df2908090cda` (`git log -1`), and
  `git apply --check --reverse` succeeds against the modified worktree;
  the stat is 26 insertions/9 deletions in `net/ipv4/igmp.c`.
- Local `origin/master` at `f47d223627f8` (2026-10-19) still has
  `static void ____ip_mc_inc_group(...)` and publishes
  `iml` before the inc call, i.e. the same pattern.  Local `master`
  (`1fc5a74b108f`, 2026-09-05) is the same.
- No commit in the locally fetched history matches this fix; the only
  recent igmp change found locally is `2987ee196c88 igmp: convert struct
  ip_sf_list to RCU`, which is unrelated.
- Public web search did not find an equivalent `____ip_mc_inc_group`
  error-propagation fix; the closest hits were the historical helper
  refactor `a1c4cd67840e` and a syzkaller leak report, neither of which
  is this issue.  `lore.kernel.org` returned 403 for direct queries.
  This is a "not found in the fetched sources", not a proof that no
  upstream patch exists.  No upstream contact was made.

## 9. Reproduction and artifacts

Host commands (from `research/linux`):

```
python3 mcast-delivery/run.py --timeout 90
```

The runner compiles `mcast-delivery/dw_mcast_delivery.c` static, builds
an initramfs with `gen_init_cpio`, boots the three kernels under
QEMU/KVM, and writes one log per kernel.  Exact QEMU commands are in the
`.cmd` files; kernel SHA-256s and start/end times are in the `.meta`
files.

Logs (all new):

```
logs/20261021-r4-mcast-delivery/v619-stock.log
logs/20261021-r4-mcast-delivery/v73-stock.log
logs/20261021-r4-mcast-delivery/v73-fixed.log
logs/20261021-r4-mcast-delivery/*.cmd
logs/20261021-r4-mcast-delivery/*.meta
logs/20261021-r4-mcast-delivery/matrix.csv
logs/20261021-r4-mcast-delivery/README.md
```

Boot timestamps (JST): v6.19 stock 22:55:59-22:56:06 (7.12 s),
v7.3-rc2 stock 22:56:06-22:56:13 (7.02 s), v7.3-rc2 fixed
22:56:13-22:56:19 (5.41 s).  All three ended with `MCAST_DONE` and
powered off; no QEMU process remained.

Test source/runner:

```
mcast-delivery/dw_mcast_delivery.c
mcast-delivery/run.py
```

In the test source: sender/join `make_sender()` line 266 and
`do_join()` line 289; receiver `recv_one()` line 434, unicast control
`unicast_control()` line 460, multicast delivery `mcast_delivery()` line
488.

Cleanup: all QEMU processes exited; no host network, route, sysctl,
kernel worktree, or existing log was modified.  Added files are this
report, the two test sources, and the new log directory; generated build
artifacts are under `mcast-delivery/build/`.  `RESULTS-round4.md` only
received the append-only section 10 pointer.
