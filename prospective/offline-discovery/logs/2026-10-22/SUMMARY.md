
### Original library: phase x mode

| phase | mode | n | verdicts | state after interrupt |
|---|---|---|---|---|
| body | cancel | 3 | state-only x3 | clean |
| body | recovery | 3 | state-only x3 | clean |
| body | retry | 3 | post x3 | clean |
| body | survivor | 3 | post x3 | clean |
| entry_after_send | cancel | 3 | state-only x3 | unread_reply |
| entry_after_send | recovery | 3 | state-only x3 | unread_reply |
| entry_after_send | retry | 3 | explicit-failure x3 | unread_reply |
| entry_after_send | survivor | 3 | explicit-failure x3 | unread_reply |
| entry_pre_send | cancel | 3 | state-only x3 | pre_send_local_ahead |
| entry_pre_send | recovery | 3 | state-only x3 | pre_send_local_ahead |
| entry_pre_send | retry | 3 | explicit-failure x3 | pre_send_local_ahead |
| entry_pre_send | survivor | 3 | explicit-failure x3 | pre_send_local_ahead |
| entry_wait | cancel | 3 | state-only x3 | wait_intrans |
| entry_wait | recovery | 3 | state-only x3 | wait_intrans |
| entry_wait | retry | 3 | silent-loss x3 | wait_intrans |
| entry_wait | survivor | 3 | silent-loss x3 | wait_intrans |
| exit_pre_commit | cancel | 3 | state-only x3 | intrans_no_local_owner |
| exit_pre_commit | recovery | 3 | state-only x3 | intrans_no_local_owner |
| exit_pre_commit | retry | 3 | silent-loss x3 | intrans_no_local_owner |
| exit_pre_commit | survivor | 3 | silent-loss x3 | intrans_no_local_owner |
| exit_wait | cancel | 3 | state-only x3 | clean |
| exit_wait | recovery | 3 | state-only x3 | clean |
| exit_wait | retry | 3 | post x3 | clean |
| exit_wait | survivor | 3 | post x3 | clean |

calibration failures: 0

### Candidate fix installed: same injections

| phase | mode | n | verdicts | state after interrupt |
|---|---|---|---|---|
| body | cancel | 3 | state-only x3 | clean |
| body | recovery | 3 | state-only x3 | clean |
| body | retry | 3 | post x3 | clean |
| body | survivor | 3 | post x3 | clean |
| entry_after_send | cancel | 3 | state-only x3 | closed |
| entry_after_send | recovery | 3 | state-only x3 | closed |
| entry_after_send | retry | 3 | explicit-failure x3 | closed |
| entry_after_send | survivor | 3 | explicit-failure x3 | closed |
| entry_pre_send | cancel | 3 | state-only x3 | clean |
| entry_pre_send | recovery | 3 | state-only x3 | clean |
| entry_pre_send | retry | 3 | post x3 | clean |
| entry_pre_send | survivor | 3 | post x3 | clean |
| entry_wait | cancel | 3 | state-only x3 | clean |
| entry_wait | recovery | 3 | state-only x3 | clean |
| entry_wait | retry | 3 | post x3 | clean |
| entry_wait | survivor | 3 | post x3 | clean |
| exit_pre_commit | cancel | 3 | state-only x3 | clean |
| exit_pre_commit | recovery | 3 | state-only x3 | clean |
| exit_pre_commit | retry | 3 | post x3 | clean |
| exit_pre_commit | survivor | 3 | post x3 | clean |
| exit_wait | cancel | 3 | state-only x3 | clean |
| exit_wait | recovery | 3 | state-only x3 | clean |
| exit_wait | retry | 3 | post x3 | clean |
| exit_wait | survivor | 3 | post x3 | clean |

calibration failures: 0

### Original library: recovery attempts after the interrupt

| phase | n | rollback() | raw ROLLBACK | reuse transaction() | plain insert | rows |
|---|---|---|---|---|---|---|
| body | 3 | 3 ok / 0 err | 3 ok / 0 err | 3 ok / 0 err | 3 ok / 0 err | [('plain=13', 'reuse=11')] |
| entry_after_send | 3 | 0 ok / 3 err | 0 ok / 3 err | 0 ok / 3 err | 0 ok / 3 err | [()] |
| entry_pre_send | 3 | 0 ok / 3 err | 3 ok / 0 err | 0 ok / 3 err | 3 ok / 0 err | [('plain=13',)] |
| entry_wait | 3 | 0 ok / 3 err | 3 ok / 0 err | 0 ok / 3 err | 3 ok / 0 err | [('plain=13',)] |
| exit_pre_commit | 3 | 3 ok / 0 err | 3 ok / 0 err | 3 ok / 0 err | 3 ok / 0 err | [('plain=13', 'reuse=11')] |
| exit_wait | 3 | 3 ok / 0 err | 3 ok / 0 err | 3 ok / 0 err | 3 ok / 0 err | [('plain=13', 'reuse=11', 'target=1')] |

### Timing sweep (real SIGALRM, survivor mode)

| offset us | n | state classes | survivor effect |
|---|---|---|---|
| 2 | 5 | clean x5 | True x5 |
| 5 | 5 | clean x5 | True x5 |
| 10 | 5 | clean x5 | True x5 |
| 15 | 5 | clean x3, pre_send_local_ahead x2 | True x3, False x2 |
| 20 | 5 | clean x2, pre_send_local_ahead x3 | True x2, False x3 |
| 25 | 5 | unread_reply x1, pre_send_local_ahead x4 | False x5 |
| 30 | 5 | clean x1, unread_reply x3, pre_send_local_ahead x1 | True x1, False x4 |
| 40 | 5 | unread_reply x3, pre_send_local_ahead x2 | False x5 |
| 60 | 5 | unread_reply x2, wait_intrans x3 | False x5 |
| 100 | 5 | wait_intrans x5 | False x5 |
| 200 | 5 | wait_intrans x5 | False x5 |
| 500 | 5 | wait_intrans x5 | False x5 |

### Sync pool: release and the next checkout

| build | case | n | same backend reused | next transaction() | victim visible | verdicts |
|---|---|---|---|---|---|---|
| fix | entry_pre_send | 3 | 3/3 | ok x3 | True x3 | post x3 |
| fix | entry_wait | 3 | 3/3 | ok x3 | True x3 | post x3 |
| original | entry_pre_send | 3 | 3/3 | AssertionError:  x3 | False x3 | explicit-failure x3 |
| original | entry_wait | 3 | 0/3 | ok x3 | True x3 | post x3 |

### Real SIGINT from a helper thread (entry wait)

| n | states | survivor visible |
|---|---|---|
| 3 | {'wait_intrans': 3} | {'False': 3} |

### Legitimate nesting vs abandoned transaction

- legit_inner_visible_inside: False
- legit_inner_visible_after_outer: True
- abandoned_client: {'tx': 'INTRANS', 'num_transactions': 1, 'closed': False, 'pgconn_status': '0'}
- abandoned_retry: ok
- abandoned_visible_after_retry: False
- abandoned_server: {'state': 'idle in transaction', 'in_xact': 'true', 'xid': '2771', 'query': 'RELEASE "_pg3_2"'}
- dropped_client: {'tx': 'INTRANS', 'num_transactions': 1, 'closed': False, 'pgconn_status': '0'}
- dropped_server: {'state': 'idle in transaction', 'in_xact': 'true', 'xid': '2774', 'query': 'INSERT INTO dw_od923838_3 (k, v) VALUES ($1, $2)'}
- dropped_inner_visible: False
- wire SQL: [['Q', 'BEGIN'], ['Q', 'SAVEPOINT "_pg3_2"'], ['P', 'INSERT INTO dw_od923838_1 (k, v) VALUES ($1, $2)'], ['Q', 'RELEASE "_pg3_2"'], ['Q', 'COMMIT'], ['Q', 'BEGIN'], ['CancelRequest', None], ['Q', 'SAVEPOINT "_pg3_2"'], ['P', 'INSERT INTO dw_od923838_2 (k, v) VALUES ($1, $2)'], ['Q', 'RELEASE "_pg3_2"'], ['Q', 'BEGIN'], ['P', 'INSERT INTO dw_od923838_3 (k, v) VALUES ($1, $2)']]

### Field-contract re-judgment of the same sync trials

| phase/mode | n | overall | per-field statuses |
|---|---|---|---|
| entry_pre_send/retry | 2 | {'Violation': 2} | target.visible: {'Pre': 2}; successor.visible: {'Violation': 2}; client.clean: {'Violation': 2}; server.clean: {'Holds': 2} |
| entry_pre_send/survivor | 2 | {'Violation': 2} | target.visible: {'Pre': 2}; successor.visible: {'Violation': 2}; client.clean: {'Violation': 2}; server.clean: {'Holds': 2} |
| entry_wait/retry | 2 | {'Violation': 2} | target.visible: {'Pre': 2}; successor.visible: {'Violation': 2}; client.clean: {'Violation': 2}; server.clean: {'Violation': 2} |
| entry_wait/survivor | 2 | {'Violation': 2} | target.visible: {'Pre': 2}; successor.visible: {'Violation': 2}; client.clean: {'Violation': 2}; server.clean: {'Violation': 2} |
| exit_pre_commit/retry | 2 | {'Violation': 2} | target.visible: {'Pre': 2}; successor.visible: {'Violation': 2}; client.clean: {'Violation': 2}; server.clean: {'Violation': 2} |
| exit_pre_commit/survivor | 2 | {'Violation': 2} | target.visible: {'Pre': 2}; successor.visible: {'Violation': 2}; client.clean: {'Violation': 2}; server.clean: {'Violation': 2} |
