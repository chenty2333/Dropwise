trials=60 harness_errors=0 unrealized=0

| driver/arm | n | realized | client state | error (class/sqlstate) | successor ok/fail | retry visible | silent successor | target visible |
|---|---:|---:|---|---|---|---:|---:|---:|
| asyncpg/cancel_body | 3 | 3 | asyncpg:in_tx=False/tx=ROLLEDBACK x3 | CancelledError/None x3 | 3/0 | 3 | 0 | 0 |
| asyncpg/cancel_enter | 3 | 3 | asyncpg:in_tx=True/tx=FAILED/top=FAILED x3 | CancelledError/None x3 | 3/0 | 0 | 3 | 0 |
| asyncpg/cancel_exit_commit | 3 | 3 | asyncpg:in_tx=False/tx=FAILED x3 | cancelled x3 | 3/0 | 3 | 0 | 3 |
| asyncpg/normal | 3 | 3 | asyncpg:in_tx=False/tx=COMMITTED x3 | completed x3 | 0/0 | 0 | 0 | 3 |
| asyncpg/server_body_div | 3 | 3 | asyncpg:in_tx=False/tx=ROLLEDBACK x3 | DivisionByZeroError/22012 x3 | 3/0 | 3 | 0 | 0 |
| asyncpg/server_body_swallowed_commit | 3 | 3 | asyncpg:in_tx=False/tx=COMMITTED x3 | completed x3 | 3/0 | 3 | 0 | 0 |
| asyncpg/server_body_unique | 3 | 3 | asyncpg:in_tx=False/tx=ROLLEDBACK x3 | UniqueViolationError/23505 x3 | 3/0 | 3 | 0 | 0 |
| asyncpg/server_entry_savepoint | 3 | 3 | asyncpg:in_tx=False/tx=COMMITTED x3 | nested_entry=InFailedSQLTransactionError/25P02 x3 | 3/0 | 3 | 0 | 0 |
| asyncpg/server_entry_terminated | 3 | 3 | asyncpg:in_tx=False/tx=None x3 | entry=InterfaceError/None x3 | 3/0 | 3 | 0 | 0 |
| asyncpg/server_exit_commit_deferred | 3 | 3 | asyncpg:in_tx=False/tx=FAILED x3 | UniqueViolationError/23505 x3 | 3/0 | 3 | 0 | 0 |
| psycopg/cancel_body | 3 | 3 | psycopg:IDLE/n=0 x3 | CancelledError/None x3 | 3/0 | 3 | 0 | 0 |
| psycopg/cancel_enter | 3 | 3 | psycopg:INTRANS/n=1 x3 | CancelledError/None x3 | 3/0 | 0 | 3 | 0 |
| psycopg/cancel_exit_commit | 3 | 3 | psycopg:IDLE/n=0 x3 | cancelled x3 | 3/0 | 3 | 0 | 3 |
| psycopg/normal | 3 | 3 | psycopg:IDLE/n=0 x3 | completed x3 | 0/0 | 0 | 0 | 3 |
| psycopg/server_body_div | 3 | 3 | psycopg:IDLE/n=0 x3 | DivisionByZero/22012 x3 | 3/0 | 3 | 0 | 0 |
| psycopg/server_body_swallowed_commit | 3 | 3 | psycopg:IDLE/n=0 x3 | completed x3 | 3/0 | 3 | 0 | 0 |
| psycopg/server_body_unique | 3 | 3 | psycopg:IDLE/n=0 x3 | UniqueViolation/23505 x3 | 3/0 | 3 | 0 | 0 |
| psycopg/server_entry_savepoint | 3 | 3 | psycopg:INERROR/n=1 x3 | nested_entry=InFailedSqlTransaction/25P02 x3 | 0/3 | 0 | 0 | 0 |
| psycopg/server_entry_terminated | 3 | 3 | psycopg:UNKNOWN/n=1 x3 | entry=AdminShutdown/57P01 x3 | 3/0 | 3 | 0 | 0 |
| psycopg/server_exit_commit_deferred | 3 | 3 | psycopg:IDLE/n=0 x3 | UniqueViolation/23505 x3 | 3/0 | 3 | 0 | 0 |
