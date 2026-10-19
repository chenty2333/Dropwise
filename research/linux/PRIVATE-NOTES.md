# Private notes

Round 1 (2026-10-19, Linux 6.19, 105 injected trials): no confirmed kernel
issue, so no security-sensitive details exist.

Round 2 (2026-10-19, Linux 6.19, 1013 injected trials, plus a source
cross-check of v7.3-rc2/v7.3-rc6): there is no security-sensitive issue.
The one confirmed functional defect — `epoll_ctl(EPOLL_CTL_MOD)` ignoring the
failure of `ep_create_wakeup_source()` and returning 0 with `EPOLLWAKEUP`
advertised but no wakeup source — is a minor power-management bug and was
classified by the requester as not security-relevant.  Per that instruction it
is documented publicly in [RESULTS-round2.md](RESULTS-round2.md) §3, not here.

The public results are in [RESULTS.md](RESULTS.md) (round 1) and
[RESULTS-round2.md](RESULTS-round2.md) (round 2).  This file intentionally
contains no speculative or unconfirmed finding.
