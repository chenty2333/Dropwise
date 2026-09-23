# Journal (append-only)

- 2026-09-23T15:26:37Z: PREREGISTRATION.md v1 frozen (sha256 83aded9b4fe65079b9e08f24c5b429f85ad4c534bc286bfc9e089a7b3dace8eb). No Dropwise run against T1-T3 before this entry.
- 2026-09-23T15:35:43Z: freeze committed as d2641dc (PREREGISTRATION.md sha256 unchanged).
- 2026-09-23T15:37:24Z: amendment A1 (C2(b) operationalisation), before any run.
- 2026-09-23T15:38:15Z: T1 (S1.1-S1.6) and T2 (S2.1a/b-S2.5 + positive control) scenarios committed as 834896d; compiled, not run.
- 2026-09-23T15:38:22Z: first run of T2 started (./run.sh t2-bb8).
- 2026-09-23T15:38:41Z: T2 first run done (3/3 runs identical). bb8 0.9.1: 0 violations in all scenarios (S2.1b has no Pending boundary: 0 plans). Positive control: violations in S2.1a, S2.2, S2.4, S2.5 (all C2(a) capacity lost), none in S2.1b, S2.3.
- 2026-09-23T15:38:41Z: first run of T1 started (./run.sh t1-tungstenite).
- 2026-09-23T15:38:50Z: T1 first run done (3/3 runs identical). tokio-tungstenite 0.30.0: 0 violations in S1.1-S1.6. Plans per scenario: 3, 30, 3, 18, 3, 6 (Pending boundaries occur only where next() waits for data). No positive control defined for T1 (section 5).
