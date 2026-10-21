# V4-1 multicast delivery logs (2026-10-21)

Three isolated QEMU/KVM boots, all new, all ending in `MCAST_DONE`:

| file | kernel | SHA-256 (from `.meta`) | wall |
|---|---|---|---:|
| `v619-stock.log` | v6.19.0 stock | `9329ad93...4b3a6b1` | 7.12 s |
| `v73-stock.log` | 7.3.0-rc2 stock | `cb7cebbc...d38484` | 7.02 s |
| `v73-fixed.log` | 7.3.0-rc2 + patch 0002 | `2c818f1d...3d490f` | 5.41 s |

Exact commands are in the sibling `.cmd` files; SHA-256s and start/end
times are in the `.meta` files.  The machine-readable result matrix is
`matrix.csv`.

## Guest configuration

- `lo` only, ifindex 1, `127.0.0.1/8`, up/running.
- `/proc/sys/net/ipv4/conf/lo/route_localnet=1`, so a looped-back
  multicast packet with source `127.0.0.1` survives
  `ip_mc_validate_source()` (`net/ipv4/route.c:1753-1754`).
- `/proc/net/route` main table is empty except for its header
  (`ROUTE table=main snapshot=Iface Destination ...`); no `239/8` or
  default route is added.
- Sender: `IP_MULTICAST_IF={127.0.0.1, ifindex=1}`, loop=1, ttl=1.
- Receiver: UDP bound to `INADDR_ANY:45678` (second receiver `:45679`).
- Join: `239.1.2.3`, `imr_address=127.0.0.1`, `imr_ifindex=0`;
  `ip_mc_find_dev()` resolves to `lo`.

## Scenarios

`normal`, `n1`, `n2`, `n2_cross`, `ssm_n2`, `invalid`, `two_sockets`.
Each runs in a fresh forked child with fresh sockets.  The `n2` variant
arms per-task `fail-nth` immediately before `IP_ADD_MEMBERSHIP`; no
allocation or output occurs between the arm and the call.  A unicast
datagram to the same receiver is the socket/route control; a normal join
followed by a multicast send is the positive control.

## Source

- Test: `../../mcast-delivery/dw_mcast_delivery.c`
  - sender/join: `make_sender()` line 266, `do_join()` line 289
  - receiver: `recv_one()` line 434, `unicast_control()` line 460,
    `mcast_delivery()` line 488
- Runner: `../../mcast-delivery/run.py`
- Audit and full interpretation: `../../RESULTS-round4-multicast.md`

Host command, from `research/linux`:

```
python3 mcast-delivery/run.py --timeout 90
```
