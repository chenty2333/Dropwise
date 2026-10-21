# Sending notes: ipv4: igmp: report interface group allocation failure on join

Status: **sent to netdev, 2026-10-22**, as `[PATCH net]`. SMTP accepted all
recipients; the received self-copy has an identical patch body. Message-ID: `<179260356668.911829.6576504063198285556@tychen.cc>`.
The outgoing mail was generated with `git format-patch` from the signed
commit in `/home/ava/dropwise-linux-r4`; strict checkpatch passed.
The adjacent patch remains the original preparation draft. The notes below
describe preparation; submission does not mean maintainer acceptance.

Patch: `0002-ipv4-igmp-report-interface-group-allocation-failure.patch`
(`net/ipv4/igmp.c` only, 28 insertions, 9 deletions).
Tested base: v7.3-rc2 `df2908090cda` + this patch (`bzImage-fixed`, SHA-256
`b1e2f81f7f8d1860106d609a0d3028df2366df22e7a51e1e3008710c8d45814b`);
controls were v6.19 `05f7e89ab973` and v7.3-rc2 stock.

## Target and routing

- Tree: **net-next** (`[PATCH net-next]`).  The same patch applies to `net`;
  use `[PATCH net]` if the maintainers prefer the fixes tree.  It is an old
  defect, not a recent regression, so net-next is the default.
- No `Fixes:` tag: the silent failure predates the 2019 helper split
  `9fb20801dab4` ("net: Fix ip_mc_{dec,inc}_group allocation context"), and I
  did not identify a single introducing commit.  No `Cc: stable`.
- `To: netdev@vger.kernel.org`
- `Cc:` from `scripts/get_maintainer.pl --no-git --no-roles <patch>`:
  - David Ahern <dsahern@kernel.org>
  - Ido Schimmel <idosch@nvidia.com>
  - "David S. Miller" <davem@davemloft.net>
  - Eric Dumazet <edumazet@google.com>
  - Jakub Kicinski <kuba@kernel.org>
  - Paolo Abeni <pabeni@redhat.com>
  - Simon Horman <horms@kernel.org> (reviewer)
  - linux-kernel@vger.kernel.org

`get_maintainer.pl` returns both the NETWORKING [IPv4/IPv6] and NETWORKING
[GENERAL] maintainers for `net/ipv4/igmp.c`; the list above is its output.

## Upstream check

- Current mainline master `a804c5742bff` (2026-10-21), current net-next and
  current net still contain `static void ____ip_mc_inc_group(...)` and still
  publish `inet->mc_list` before the inc call.  This is **source inspection**
  of those trees; no current-tree kernel was built or booted.
- `git apply --check` succeeds against clean v7.3-rc2, master, net-next and
  net; `git apply --reverse --check` succeeds on the built worktree.  The
  applied v7.3-rc2 file is byte-identical to the worktree.
- No equivalent fix was found in the fetched sources or public web search.
  Direct `lore.kernel.org` queries are blocked in this environment (HTTP 403 /
  Anubis bot check), so this is "not found in the fetched sources", not proof
  that no upstream patch or report exists.  No maintainer confirmation was
  obtained and no upstream contact was made.

## Checks run

```
scripts/checkpatch.pl --strict --no-signoff \
  research/linux/patches/0002-ipv4-igmp-report-interface-group-allocation-failure.patch
# total: 0 errors, 0 warnings, 0 checks, 73 lines checked
```

The missing `Signed-off-by` is expected and intentional in the draft; do not
treat the `--no-signoff` run as a signed patch check.

QEMU/KVM matrix (existing harness, new logs under
`research/linux/logs/20261022-r4-mcast-closure/`):

- 3 kernels (`v6.19` stock, `v7.3-rc2` stock, `v7.3-rc2` + patch), 10
  scenarios each; all boots `done=1`, all scenario statuses 0, no harness or
  control failures.
- Stock N=2: `IP_ADD_MEMBERSHIP` returns 0, no device group, 0/4 datagrams;
  fixed: `-ENOBUFS`, no socket/device state, later join 2/2.
- Stock cross-socket: closing the half-joined socket removes the second
  socket's device group and its delivery drops to 0/2; fixed keeps 2/2.
- Normal join 4/4, second-socket `users++` path 1/1, source-filtered join
  allowed 2/2 and filtered 0/2, and two sockets joining the same source-
  filtered group all behave the same on stock and fixed.
- Wrapper/all-hosts: at fail-nth N=14 the interface-up returns 0 with
  `010000E0` absent and no error on all three kernels; a later up repairs it.
  This is the legacy void wrapper, deliberately unchanged by this patch.  The
  test identifies the site by effect, not a kernel stack trace.

Full details: `research/linux/RESULTS-round4-multicast.md` section 10.

## What is not covered

- Loopback only; no veth/NIC/hardware multicast filter and no IGMP packet
  capture.  The all-hosts test uses a private netns and `lo`, not a second
  device.
- fail-nth only; no real memory-pressure trigger.
- The wrapper/all-hosts path has no error channel in this patch; the group can
  remain missing until a later successful up.
- `ip_mc_add_src()` failure on the existing-group `users++` path is still
  ignored (pre-existing, out of scope).
- No weak-memory litmus test; the ordering argument is the RCU
  release/acquire pair plus RTNL serialization, not a memory-model result.
- The patch was built and booted on v7.3-rc2 only; the current-tree apply
  checks are textual.

## Before sending (user action)

1. Choose net-next or net, rebase/apply the patch there, and re-check the
   conflict-free apply.
2. Add your own `Signed-off-by:` line (the repository's configured author is
   `Tianyi Chen <hi@tychen.cc>`; the draft deliberately has no author, date or
   sign-off).  The sign-off is your attestation; do not add `Reported-by` or
   `Tested-by` for this work, because there is no third-party report or
   tester.
3. Re-run `checkpatch.pl --strict` on the signed patch; the only expected
   error in the unsigned draft should disappear.
4. Send.  The local test harness is evidence, not part of the patch; offer it
   on request rather than attaching it to the first mail.

Draft command (do not run from this workspace):

```
git send-email --to=netdev@vger.kernel.org \
  --cc=dsahern@kernel.org --cc=idosch@nvidia.com \
  --cc=davem@davemloft.net --cc=edumazet@google.com \
  --cc=kuba@kernel.org --cc=pabeni@redhat.com \
  --cc=horms@kernel.org --cc=linux-kernel@vger.kernel.org \
  0002-ipv4-igmp-report-interface-group-allocation-failure.patch
```
