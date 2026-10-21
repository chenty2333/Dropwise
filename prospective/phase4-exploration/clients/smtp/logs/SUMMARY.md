### Ladder table (from logs/LADDER.txt and per-rep logs)

| level | rep | plans | violations | unrealized | unsettled | baseline_errors | status | B2/B3 survivor values | M1 verdicts | hang? |
|---|---|---|---|---|---|---|---|---|---|---|
| B1 | 1 | 8 | 0 | 0 | 0 | 0 | 0 | - | - | 0 |
| B1 | 2 | 8 | 0 | 0 | 0 | 0 | 0 | - | - | 0 |
| B1 | 3 | 8 | 0 | 0 | 0 | 0 | 0 | - | - | 0 |
| B2 | 1 | 8 | 2 | 0 | 0 | 0 | 1 | Ok(250), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354) | - | 2 |
| B2 | 2 | 8 | 2 | 0 | 0 | 0 | 1 | Ok(250), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354) | - | 2 |
| B2 | 3 | 8 | 2 | 0 | 0 | 0 | 1 | Ok(250), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354) | - | 2 |
| B3 | 1 | 8 | 2 | 0 | 0 | 0 | 1 | Ok(250), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354) | - | 2 |
| B3 | 2 | 8 | 2 | 0 | 0 | 0 | 1 | Ok(250), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354) | - | 2 |
| B3 | 3 | 8 | 2 | 0 | 0 | 0 | 1 | Ok(250), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354), Ok(354) | - | 2 |
| M1 | 1 | 8 | 2 | 0 | 0 | 0 | 1 | - | pre=4 post=2 mixed=0 neither=0 unknown=0 | 2 |
| M1 | 2 | 8 | 2 | 0 | 0 | 0 | 1 | - | pre=4 post=2 mixed=0 neither=0 unknown=0 | 2 |
| M1 | 3 | 8 | 2 | 0 | 0 | 0 | 1 | - | pre=4 post=2 mixed=0 neither=0 unknown=0 | 2 |

### Per-plan M1 observations

**M1 rep 1**

- `1: #0@1:Immediate` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `2: #0@1:AfterWake` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `3: #0@2:Immediate` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `4: #0@2:AfterWake` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `5: #0@3:Immediate` -> none (not an observed run), settled: true; invariant: scenario did not finish within 20s (liveness); observed: 
- `6: #0@3:AfterWake` -> none (not an observed run), settled: true; invariant: scenario did not finish within 20s (liveness); observed: 
- `7: #0@4:Immediate` -> Post, settled: true; invariant: ok; observed: Observation { count: 2, messages: [MessageView { subject: "p4-smtp <run> target", message_id: "<<run>.target@dw.test>", body: "body <run> target" }, MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `8: #0@4:AfterWake` -> Post, settled: true; invariant: ok; observed: Observation { count: 2, messages: [MessageView { subject: "p4-smtp <run> target", message_id: "<<run>.target@dw.test>", body: "body <run> target" }, MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }

**M1 rep 2**

- `1: #0@1:Immediate` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `2: #0@1:AfterWake` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `3: #0@2:Immediate` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `4: #0@2:AfterWake` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `5: #0@3:Immediate` -> none (not an observed run), settled: true; invariant: scenario did not finish within 20s (liveness); observed: 
- `6: #0@3:AfterWake` -> none (not an observed run), settled: true; invariant: scenario did not finish within 20s (liveness); observed: 
- `7: #0@4:Immediate` -> Post, settled: true; invariant: ok; observed: Observation { count: 2, messages: [MessageView { subject: "p4-smtp <run> target", message_id: "<<run>.target@dw.test>", body: "body <run> target" }, MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `8: #0@4:AfterWake` -> Post, settled: true; invariant: ok; observed: Observation { count: 2, messages: [MessageView { subject: "p4-smtp <run> target", message_id: "<<run>.target@dw.test>", body: "body <run> target" }, MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }

**M1 rep 3**

- `1: #0@1:Immediate` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `2: #0@1:AfterWake` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `3: #0@2:Immediate` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `4: #0@2:AfterWake` -> Pre, settled: true; invariant: ok; observed: Observation { count: 1, messages: [MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `5: #0@3:Immediate` -> none (not an observed run), settled: true; invariant: scenario did not finish within 20s (liveness); observed: 
- `6: #0@3:AfterWake` -> none (not an observed run), settled: true; invariant: scenario did not finish within 20s (liveness); observed: 
- `7: #0@4:Immediate` -> Post, settled: true; invariant: ok; observed: Observation { count: 2, messages: [MessageView { subject: "p4-smtp <run> target", message_id: "<<run>.target@dw.test>", body: "body <run> target" }, MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }
- `8: #0@4:AfterWake` -> Post, settled: true; invariant: ok; observed: Observation { count: 2, messages: [MessageView { subject: "p4-smtp <run> target", message_id: "<<run>.target@dw.test>", body: "body <run> target" }, MessageView { subject: "p4-smtp <run> survivor", message_id: "<<run>.survivor@dw.test>", body: "body <run> survivor" }] }


### Violations and errors

LADDER-VIOLATION B2 rep1 [async-smtp-0.10.2] plan=#0@3:Immediate cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B2 rep1 [async-smtp-0.10.2] plan=#0@3:AfterWake cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B2 rep2 [async-smtp-0.10.2] plan=#0@3:Immediate cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B2 rep2 [async-smtp-0.10.2] plan=#0@3:AfterWake cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B2 rep3 [async-smtp-0.10.2] plan=#0@3:Immediate cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B2 rep3 [async-smtp-0.10.2] plan=#0@3:AfterWake cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B3 rep1 [async-smtp-0.10.2] plan=#0@3:Immediate cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B3 rep1 [async-smtp-0.10.2] plan=#0@3:AfterWake cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B3 rep2 [async-smtp-0.10.2] plan=#0@3:Immediate cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B3 rep2 [async-smtp-0.10.2] plan=#0@3:AfterWake cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B3 rep3 [async-smtp-0.10.2] plan=#0@3:Immediate cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION B3 rep3 [async-smtp-0.10.2] plan=#0@3:AfterWake cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION M1 rep1 [async-smtp-0.10.2] plan=#0@3:Immediate cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION M1 rep1 [async-smtp-0.10.2] plan=#0@3:AfterWake cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION M1 rep2 [async-smtp-0.10.2] plan=#0@3:Immediate cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION M1 rep2 [async-smtp-0.10.2] plan=#0@3:AfterWake cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION M1 rep3 [async-smtp-0.10.2] plan=#0@3:Immediate cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)
LADDER-VIOLATION M1 rep3 [async-smtp-0.10.2] plan=#0@3:AfterWake cancelled=1 verdict=None detail=scenario did not finish within 20s (liveness)

### Hang localisation

- B2 rep 1: survivor_start=9 survivor_done=7 (missing=2); stderr: 
Caused by:
  process didn't exit successfully: `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/smtp/target/debug/deps/levels-27c63d1134a93b87 level B2 1` (exit status: 1)
- B2 rep 2: survivor_start=9 survivor_done=7 (missing=2); stderr: 
Caused by:
  process didn't exit successfully: `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/smtp/target/debug/deps/levels-27c63d1134a93b87 level B2 2` (exit status: 1)
- B2 rep 3: survivor_start=9 survivor_done=7 (missing=2); stderr: 
Caused by:
  process didn't exit successfully: `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/smtp/target/debug/deps/levels-27c63d1134a93b87 level B2 3` (exit status: 1)
- B3 rep 1: survivor_start=9 survivor_done=7 (missing=2); stderr: 
Caused by:
  process didn't exit successfully: `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/smtp/target/debug/deps/levels-27c63d1134a93b87 level B3 1` (exit status: 1)
- B3 rep 2: survivor_start=9 survivor_done=7 (missing=2); stderr: 
Caused by:
  process didn't exit successfully: `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/smtp/target/debug/deps/levels-27c63d1134a93b87 level B3 2` (exit status: 1)
- B3 rep 3: survivor_start=9 survivor_done=7 (missing=2); stderr: 
Caused by:
  process didn't exit successfully: `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/smtp/target/debug/deps/levels-27c63d1134a93b87 level B3 3` (exit status: 1)
- M1 rep 1: survivor_start=0 survivor_done=0 (missing=2); stderr: 
Caused by:
  process didn't exit successfully: `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/smtp/target/debug/deps/levels-27c63d1134a93b87 level M1 1` (exit status: 1)
- M1 rep 2: survivor_start=0 survivor_done=0 (missing=2); stderr: 
Caused by:
  process didn't exit successfully: `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/smtp/target/debug/deps/levels-27c63d1134a93b87 level M1 2` (exit status: 1)
- M1 rep 3: survivor_start=0 survivor_done=0 (missing=2); stderr: 
Caused by:
  process didn't exit successfully: `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/smtp/target/debug/deps/levels-27c63d1134a93b87 level M1 3` (exit status: 1)
