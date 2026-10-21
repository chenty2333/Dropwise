# PostgreSQL 18 wire protocol: verified simple-query facts and transaction recovery

**Executed:** 2026-10-22 02:16–02:19 JST (2026-10-21 17:16–17:19 UTC)
**Server:** PostgreSQL **18.4** (`server_version_num = 180004`), `postgres:18-alpine`, x86_64 musl
**Protocol used:** PostgreSQL v3.0 (`196608`), unencrypted TCP, simple Query only, trust authentication
**Probe:** disposable Docker container on `127.0.0.1:55432`, removed at the end

**Verification labels used below**

- **OBSERVED** — measured against the live container in this run, with the scripts/outputs named in §10.
- **DOCS** — taken from the official PostgreSQL 18 documentation cited in §10; not independently re-derived here.
- **INFERRED** — follows from framing/protocol rules but was not separately executed.
- **UNVERIFIED** — not tested in this run.

The primary steps were executed. The only intentionally unexecuted variants are listed in §10.3.

---

## 1. Environment, image, startup and cleanup

### 1.1 Image was already local

`docker images` before starting the container contained:

```text
postgres:18-alpine 1b1689b20d16 4 months ago 434MB
```

`docker image inspect` gave:

```text
image_id=sha256:1b1689b20d16a014a3d195653381cf2caa75a41a92d93b255a9d6ea29fd353aa
created=2026-06-16T23:00:10.312931057Z
architecture=amd64  os=linux
```

No image pull occurred. **(OBSERVED)**

### 1.2 Port 55432 was free

Before `docker run`, this produced only the `ss` header, no listener:

```console
$ ss -ltn 'sport = :55432'
State Recv-Q Send-Q Local Address:Port Peer Address:PortProcess
```

So the required port **55432** was used; no alternative port was needed. **(OBSERVED)**

### 1.3 Container command and readiness

Exact command used:

```bash
docker run -d --rm --name pgonlineprobe \
  -e POSTGRES_PASSWORD=pw \
  -e POSTGRES_HOST_AUTH_METHOD=trust \
  -p 127.0.0.1:55432:5432 postgres:18-alpine
```

Results:

| Fact | Observed value |
|---|---|
| `docker run` elapsed | 0.246 s |
| Container ID | `52741c0993589f31d3ec9e0a5f82ff1c2e27743d17843b381cc6a09f74935711` |
| Created | `2026-10-21T17:16:04.318158862Z` |
| Started | `2026-10-21T17:16:04.422552056Z` |
| First successful `pg_isready` | `2026-10-21T17:16:05.833138454Z` |
| Readiness wait after `docker run` returned | **1.300 s** |
| `pg_isready` output | `127.0.0.1:5432 - accepting connections` |
| Published address | `127.0.0.1:55432 -> 5432/tcp` only |

Readiness was polled with:

```bash
docker exec pgonlineprobe pg_isready -h 127.0.0.1 -p 5432 -U postgres
```

**(OBSERVED)**

The server version was verified directly:

```text
PostgreSQL 18.4 on x86_64-pc-linux-musl, compiled by gcc (Alpine 15.2.0) 15.2.0, 64-bit
18.4
180004
```

`pg_hba.conf` contained `host all all all trust` (and local trust lines), which is why
the startup exchange produced `AuthenticationOk` without a password. **(OBSERVED)**

### 1.4 Cleanup

At the end:

```console
$ docker rm -f pgonlineprobe
pgonlineprobe

$ docker ps -a --filter name=pgonlineprobe
# (no output)

$ ss -ltn 'sport = :55432'
# (only the header; no listener)
PORT_55432=FREE
```

No container named `pgonlineprobe` remains and port 55432 is free. Unrelated pre-existing
containers were left alone. **(OBSERVED)**

---

## 2. Byte-level framing

All integers in the v3 protocol are **big-endian / network byte order**. **(OBSERVED, DOCS)**

### 2.1 Backend message header

Every ordinary backend message after startup:

```text
offset  size  content
0       1     type byte (ASCII)
1       4     int32 length
5       n     payload; n = length - 4
```

Crucially, **`length` includes the 4-byte length field itself but excludes the type byte**.
The total number of bytes on the wire is therefore `1 + length`.

Example, the terminal idle status:

```text
5a 00000005 49
^  ^        ^
Z  len=5    status 'I' (0x49)
```

Total frame size = 6 bytes, payload size = 1 byte. **(OBSERVED)**

### 2.2 StartupMessage

A StartupMessage has **no type byte**:

```text
offset  size  content
0       4     int32 length (includes itself)
4       4     int32 protocol version
8       ...   parameter name/value pairs, each a NUL-terminated string
...     1     0x00 terminator after the last value
```

The exact startup packet sent by the probe was **41 bytes**:

```text
00000029 00030000 7573657200 706f73746772657300
646174616261736500 706f73746772657300 00
```

Annotated:

| Bytes | Meaning |
|---|---|
| `00000029` | length = 41 |
| `00030000` | protocol 3.0 = 196608 |
| `75736572 00` | `user\0` |
| `706f737467726573 00` | `postgres\0` |
| `6461746162617365 00` | `database\0` |
| `706f737467726573 00` | `postgres\0` |
| `00` | final parameter terminator |

Full hex for direct comparison:

```text
00000029000300007573657200706f73746772657300646174616261736500706f7374677265730000
```

The server accepted 3.0 and did **not** send `NegotiateProtocolVersion`. **(OBSERVED)**

> **PG18 note (DOCS):** PostgreSQL 18 also supports protocol **3.2**; libpq still defaults
> to 3.0 for compatibility. In 3.2 the cancellation secret is variable-length, whereas in
> 3.0 it is 4 bytes. This probe is entirely 3.0 and observed a 4-byte secret. Do not copy
> the 4-byte assumption to a client that negotiates 3.2.

Reference: [PostgreSQL 18, Protocol Versions](https://www.postgresql.org/docs/18/protocol-overview.html).

### 2.3 Simple Query message

A `Q` message:

```text
Byte1  'Q' = 0x51
Int32  length = 4 + len(SQL bytes) + 1
String SQL text, NUL-terminated
```

Examples actually sent:

| SQL | Length | Full frontend frame |
|---|---:|---|
| `SELECT 1` (8 chars) | 13 | `510000000d53454c454354203100` |
| `BEGIN` (5) | 10 | `510000000a424547494e00` |
| `SELECT 42` (9) | 14 | `510000000e53454c45435420343200` |
| `ROLLBACK` (8) | 13 | `510000000d524f4c4c4241434b00` |
| `DISCARD ALL` (11) | 16 | `51000000104449534341524420414c4c00` |
| `COMMIT` (6) | 11 | `510000000b434f4d4d495400` |
| `SELECT pg_backend_pid()` (23) | 28 | `510000001c53454c4543542070675f6261636b656e645f706964282900` |

For `SELECT 1`: `length = 4 + 8 + 1 = 13`; total on wire = `1 + 13 = 14`.
**(OBSERVED)**

A single `Query` can contain multiple SQL statements separated by semicolons; the server
still sends one final `ReadyForQuery` for the whole string. This report only tested
single-statement strings. **(DOCS; multi-statement not tested here)**

### 2.4 Backend message type bytes observed

| Type | Hex | Meaning | Payload layout observed/needed |
|---|---:|---|---|
| `R` | `0x52` | Authentication request/OK | Int32 auth code; `0` = AuthenticationOk |
| `S` | `0x53` | ParameterStatus | two NUL-terminated strings: name, value |
| `K` | `0x4b` | BackendKeyData | Int32 pid + secret bytes (4 bytes in v3.0) |
| `Z` | `0x5a` | ReadyForQuery | exactly Byte1 status |
| `T` | `0x54` | RowDescription | Int16 field count + descriptors |
| `D` | `0x44` | DataRow | Int16 field count + length/value pairs |
| `C` | `0x43` | CommandComplete | NUL-terminated command tag |
| `E` | `0x45` | ErrorResponse | field-code/string pairs + `0x00` terminator |
| `N` | `0x4e` | NoticeResponse | same field layout as ErrorResponse |

The `I` message type (`0x49`, EmptyQueryResponse) is documented but was not exercised.
**(OBSERVED for the table; EmptyQueryResponse UNVERIFIED)**

---

## 3. Startup exchange

The startup packet above produced **18 backend frames** before the connection was usable:
`R`, 15 × `S`, `K`, `Z`. **(OBSERVED)**

| # | Type | Declared length | Payload summary |
|---:|---|---:|---|
| 1 | `R` | 8 | auth code `0` (AuthenticationOk) |
| 2 | `S` | 23 | `in_hot_standby=off` |
| 3 | `S` | 25 | `integer_datetimes=on` |
| 4 | `S` | 17 | `TimeZone=UTC` |
| 5 | `S` | 27 | `IntervalStyle=postgres` |
| 6 | `S` | 32 | `search_path="$user", public` |
| 7 | `S` | 20 | `is_superuser=on` |
| 8 | `S` | 22 | `application_name=` (empty) |
| 9 | `S` | 38 | `default_transaction_read_only=off` |
| 10 | `S` | 26 | `scram_iterations=4096` |
| 11 | `S` | 23 | `DateStyle=ISO, MDY` |
| 12 | `S` | 35 | `standard_conforming_strings=on` |
| 13 | `S` | 35 | `session_authorization=postgres` |
| 14 | `S` | 25 | `client_encoding=UTF8` |
| 15 | `S` | 24 | `server_version=18.4` |
| 16 | `S` | 25 | `server_encoding=UTF8` |
| 17 | `K` | 12 | pid `128`, secret `3326645211` (4 bytes) |
| 18 | `Z` | 5 | status `'I'` (`0x49`) |

Exact representative frames:

```text
R:  52 00000008 00000000
S:  53 00000017 696e5f686f745f7374616e646279006f666600
K:  4b 0000000c 00000080 c64893db        # final probe connection, pid=128
Z:  5a 00000005 49                       # 'I' idle
```

`K` payload is two 4-byte values for protocol 3.0: pid `128` = `00000080`, secret
`3326645211` = `c64893db`. In PG18 protocol 3.2 the secret is variable length; this
client used 3.0. **(OBSERVED; 3.2 docs)**

`S` payload for `in_hot_standby=off` has length 23 and payload:

```text
696e5f686f745f7374616e646279006f666600
"in_hot_standby\0" "off\0"
```

The full initial parameter map was:

```json
{
  "DateStyle": "ISO, MDY",
  "IntervalStyle": "postgres",
  "TimeZone": "UTC",
  "application_name": "",
  "client_encoding": "UTF8",
  "default_transaction_read_only": "off",
  "in_hot_standby": "off",
  "integer_datetimes": "on",
  "is_superuser": "on",
  "scram_iterations": "4096",
  "search_path": "\"$user\", public",
  "server_encoding": "UTF8",
  "server_version": "18.4",
  "session_authorization": "postgres",
  "standard_conforming_strings": "on"
}
```

### 3.1 `client_encoding` and `DateStyle`

**For the planned client (integers and text only), neither needs to be set.** **(OBSERVED + INFERRED)**

- Default `client_encoding=UTF8` and `server_encoding=UTF8`.
- All SQL sent here was ASCII; all DataRow values were ASCII text (`"1"`, `"42"`, `"128"`).
- Simple query always returns retrieved values in **text format** unless a binary cursor
  is involved. The `RowDescription` format code was `0` (text).
- `DateStyle=ISO, MDY` is irrelevant if no `date`/`timestamp` values are exchanged.
- If desired for robustness, the StartupMessage can include an extra parameter pair
  `client_encoding\0UTF8\0`; not required in this environment. Do not add a trailing
  duplicate terminator by mistake: the final single `0x00` terminates the whole parameter
  list.

---

## 4. Simple-query sequence and exact statuses

The probe sent each `Query`, then read **until `ReadyForQuery`**, recording every frame.
“Length” below is the v3 declared length (excluding the type byte). **(OBSERVED)**

| # | Query | Backend frames in exact order | Final Z |
|---:|---|---|---|
| 1 | `SELECT 1` | `T(33)` one field `?column?` oid 23, typlen 4, typmod -1, fmt 0; `D(11)` value `"1"`; `C(13)` tag `"SELECT 1"`; `Z(5)` | **I** |
| 2 | `BEGIN` | `C(10)` tag `"BEGIN"`; `Z(5)` | **T** |
| 3 | `SELECT 1` | `T(33)`; `D(11)` value `"1"`; `C(13)` tag `"SELECT 1"`; `Z(5)` | **T** |
| 4 | `ROLLBACK` | `C(13)` tag `"ROLLBACK"`; `Z(5)` | **I** |
| 5 | `DISCARD ALL` (outside tx) | `C(16)` tag `"DISCARD ALL"`; `Z(5)` | **I** |
| 6 | `COMMIT` (no tx) | `N(102)` WARNING SQLSTATE `25P01`; `C(11)` tag `"COMMIT"`; `Z(5)` | **I** |
| 7 | `BEGIN` | `C(10)` tag `"BEGIN"`; `Z(5)` | **T** |
| 8 | `SELEC 1` (syntax error) | `E(93)` SQLSTATE `42601`; `Z(5)` | **E** |
| 9 | `ROLLBACK` (from aborted tx) | `C(13)` tag `"ROLLBACK"`; `Z(5)` | **I** |
| 10 | `BEGIN` | `C(10)` tag `"BEGIN"`; `Z(5)` | **T** |
| 11 | `DISCARD ALL` (inside tx) | `E(118)` SQLSTATE `25001`; `Z(5)` | **E** |
| 12 | `ROLLBACK` (from E) | `C(13)` tag `"ROLLBACK"`; `Z(5)` | **I** |
| 13 | `DISCARD ALL` (after rollback) | `C(16)` tag `"DISCARD ALL"`; `Z(5)` | **I** |
| 14 | `SELECT pg_backend_pid()` | `T(39)` field `pg_backend_pid` oid 23; `D(13)` value `"128"`; `C(13)` tag `"SELECT 1"`; `Z(5)` | **I** |

Key observations:

- `SELECT 1` outside a transaction ends in **I**; the same query inside a transaction ends
  in **T**.
- `BEGIN` returns only `CommandComplete("BEGIN")` then `Z('T')`; there is no
  RowDescription/DataRow.
- `ROLLBACK` from both **T** and **E** is accepted, returns
  `CommandComplete("ROLLBACK")`, and ends with **I**.
- `DISCARD ALL` outside/after rollback succeeds and ends **I**.
- `DISCARD ALL` inside a transaction fails with SQLSTATE **25001**, and the failed command
  leaves the transaction status **E**. It does **not** roll back for you.
- `COMMIT` with no transaction gives a WARNING, but still succeeds and returns **I**.
- `CommandComplete` tag is **not** a copy of the SQL. `SELECT pg_backend_pid()` has tag
  `"SELECT 1"` because one row was returned.

### 4.1 Exact payloads for the common replies

`SELECT 1` — declared lengths and payload hex (no type byte):

```text
T len=33 payload=0001 3f636f6c756d6e3f 00000000 0000 00000017 0004 ffffffff 0000
D len=11 payload=0001 00000001 31
C len=13 payload=53454c454354203100             # "SELECT 1\0"
Z len=5  payload=49                            # 'I'
```

Decoded `T`:
- `0001` = 1 field
- `?column?\0` = field name
- table OID `0`, attribute number `0`
- type OID `0x17` = 23 (`int4`)
- type length `4`
- type modifier `-1` (`0xffffffff`)
- format code `0` (text)

Decoded `D`: 1 field; value length `1`; value byte `0x31` = `"1"`.

`BEGIN` / `ROLLBACK` / `DISCARD ALL` command tags:

```text
C BEGIN:       43 0000000a 424547494e00
C ROLLBACK:    43 0000000d 524f4c4c4241434b00
C DISCARD ALL: 43 00000010 4449534341524420414c4c00
```

`ReadyForQuery` exact frames seen:

```text
idle (I):          5a 00000005 49
in transaction (T):5a 00000005 54
failed tx (E):     5a 00000005 45
```

### 4.2 `SELECT pg_backend_pid()`

```text
T len=39 payload=0001 70675f6261636b656e645f70696400 00000000 0000 00000017 0004 ffffffff 0000
D len=13 payload=0001 00000003 313238           # "128"
C len=13 payload=53454c454354203100             # tag "SELECT 1"
Z len=5  payload=49                            # 'I'
```

This is an `int4` but its DataRow is still the text bytes `"128"`, not a binary integer.
The field name is exactly `pg_backend_pid`. **(OBSERVED)**

---

## 5. Error, notice and status details

### 5.1 ErrorResponse / NoticeResponse layout

Both are sequences of fields, each:

```text
Byte1 field_type
String value, NUL-terminated
```

A zero field-type byte terminates the message; no string follows. Fields may appear in any
order and unknown fields must be skipped. Observed fields were `S`, `V`, `C`, `M`, `P`,
`F`, `L`, `R` (and no others). **(OBSERVED; layout DOCS)**

Reference: [PostgreSQL 18, Error and Notice Message Fields](https://www.postgresql.org/docs/18/protocol-error-fields.html).

### 5.2 Syntax error inside a transaction

After `BEGIN` (status T), sending `SELEC 1` produced exactly:

```text
E len=93
payload=534552524f5200564552524f5200433432363031004d73796e746178206572726f72206174206f72206e656172202253454c45432200503100467363616e2e6c004c3132333600527363616e6e65725f79796572726f720000
Z len=5 payload=45                               # 'E'
```

Decoded fields:

```text
S = ERROR
V = ERROR
C = 42601
M = syntax error at or near "SELEC"
P = 1
F = scan.l
L = 1236
R = scanner_yyerror
```

The query did **not** produce a CommandComplete. The transaction is now aborted; final
status is **E**. A subsequent `ROLLBACK` produced:
`C(13) "ROLLBACK"` + `Z(5) 'I'`. **(OBSERVED)**

### 5.3 `DISCARD ALL` inside an open transaction

After `BEGIN` (status T), sending `DISCARD ALL` produced exactly:

```text
E len=118
payload=534552524f5200564552524f5200433235303031004d4449534341524420414c4c2063616e6e6f742072756e20696e736964652061207472616e73616374696f6e20626c6f636b0046786163742e63004c33363534005250726576656e74496e5472616e73616374696f6e426c6f636b0000
Z len=5 payload=45                               # 'E'
```

Decoded:

```text
S = ERROR
V = ERROR
C = 25001
M = DISCARD ALL cannot run inside a transaction block
F = xact.c
L = 3654
R = PreventInTransactionBlock
```

This is an error, **not** a no-op, and it changes the transaction status from T to E.
**(OBSERVED)**

### 5.4 `COMMIT` with no transaction

After an earlier `ROLLBACK`, status was I. Sending `COMMIT` produced:

```text
N len=102
payload=535741524e494e4700565741524e494e4700433235503031004d7468657265206973206e6f207472616e73616374696f6e20696e2070726f67726573730046786163742e63004c343135320052456e645472616e73616374696f6e426c6f636b0000
C len=11 payload=434f4d4d495400                  # "COMMIT\0"
Z len=5 payload=49                               # 'I'
```

Decoded notice fields:

```text
S = WARNING
V = WARNING
C = 25P01
M = there is no transaction in progress
F = xact.c
L = 4152
R = EndTransactionBlock
```

So an idle `COMMIT` is not a protocol-level error: it warns, returns tag `COMMIT`, and
status I. **(OBSERVED)**

### 5.5 Related supplementary status checks

- `ROLLBACK` when already idle: `N(108)` SQLSTATE `25P01` warning
  “there is no transaction in progress”, then `C(13) "ROLLBACK"`, `Z('I')`. Thus an
  unconditional rollback is legal, though noisy.
- `COMMIT` while status is `E`: one `C(13)` with tag **`"ROLLBACK"`**, then `Z('I')`.
  PostgreSQL rolls back the aborted transaction, but the returned tag is `ROLLBACK`, not
  `COMMIT`. Use explicit `ROLLBACK` for clarity.
- `ROLLBACK` from T and E both return `C("ROLLBACK")` and `Z('I')`; verified multiple
  times in the main sequence. **(OBSERVED)**

ReadyForQuery status meanings (DOCS; byte values observed):

```text
'I' = 0x49  idle, not in a transaction block
'T' = 0x54  in a transaction block
'E' = 0x45  failed transaction block; queries rejected until block is ended
```

Reference: [PostgreSQL 18, Message Formats — ReadyForQuery](https://www.postgresql.org/docs/18/protocol-message-formats.html).

---

## 6. Desynchronization: forgetting the BEGIN reply

### 6.1 Root cause

The simple-query protocol has:

- **no request IDs**,
- **no per-query tag on rows**, and
- response framing only by message type/length.

The server does not know or care whether the client has read previous replies. If the
client sends a new `Query` while old reply frames remain unread, those old frames are at the
front of the byte stream. A client that reads “a reply” after the new query can therefore
consume the old reply. There is no resynchronization marker in the simple-query reply
stream other than `ReadyForQuery`; a partial frame leaves the parser misaligned at the byte
level.

### 6.2 Scenario A — BEGIN reply entirely unread (executed)

Script `/tmp/pgwire_desync.py`, scenario A:

```text
C->S Q 'BEGIN'       (read nothing)
C->S Q 'SELECT 42'
client then reads until the next Z and treats it as SELECT 42's response:
  C len=10 frame=430000000a424547494e00  tag="BEGIN"
  Z len=5  frame=5a0000000554           status='T'
```

The client-observed status is `T`, which is “correct” for the server's transaction state,
but it was the `BEGIN` response, not the `SELECT 42` response. The actual
`SELECT 42` reply was still queued. On the next query:

```text
C->S Q 'SELECT 43'
client reads until the next Z:
  T len=33 frame=540000002100013f636f6c756d6e3f00000000000000000000170004ffffffff0000
  D len=12 frame=440000000c0001000000023432   values=["42"]
  C len=13 frame=430000000d53454c454354203100 tag="SELECT 1"
  Z len=5  frame=5a0000000554                  status='T'
```

The client just asked for `43` and received **`42`**, with no indication that the rows
belong to the previous query. **(OBSERVED)**

Wire order (and the real result) was:

```text
C(BEGIN), Z(T), T(SELECT 42), D("42"), C("SELECT 1"), Z(T), ...
```

The server executed `SELECT 42` **inside** the still-open transaction. It was not blocked by
the client's failure to read. **(OBSERVED)**

### 6.3 Scenario B — skip exactly one frame (executed)

Script scenario B discarded exactly one frame, `C("BEGIN")`, then sent `SELECT 42`:

```text
deliberately discarded: 430000000a424547494e00

Q SELECT 42 -> next Z read:
  Z len=5 frame=5a0000000554 status='T'
```

Only `BEGIN`'s `ReadyForQuery` remained, so even a client that reads only until a Z gets a
frame that appears to be a complete (if empty) response to `SELECT 42`. The next query then
reads the real `SELECT 42` rows:

```text
Q SELECT 43 -> reads:
  T(33), D(12) ["42"], C(13) "SELECT 1", Z(T)
```

This is the simplest literal “skip one message” reproduction of the off-by-one-reply
desynchronization. **(OBSERVED)**

### 6.4 “Late by a few bytes” / partial frame

If a client consumes only part of a frame — for example, 5 header bytes but not the
payload, or all but the last byte — the next type byte it reads is actually a payload byte,
not a message type. Since the next 4 bytes are also not the intended length, the parser can
produce an unknown type or an absurd length. There is no delimiter or resync point in the
simple-query stream; `ReadyForQuery` itself is just another framed message.

**This sub-frame variant was not separately executed**; the conclusion follows directly from
the single-byte type + length framing and is marked **INFERRED**. The all-or-nothing
one-frame variants above were executed.

### 6.5 Practical recovery

- If the server is in **T** or **E** and the byte stream is still correctly aligned:
  send `Query("ROLLBACK")`, read until `Z`, require status **I**.
- If the reply stream is already desynchronized, **a new Query does not clear old replies**.
  You must either drain exactly the outstanding replies (if you can account for every
  outstanding `Query`/`Z` pair) or, much more simply, **close the connection and reconnect**.
  A `CancelRequest` also does not remove already-queued result frames. **(INFERRED from the
  framing/ordering rules; not separately tested.)**
- For a Rust client, never infer message boundaries from TCP `read()` boundaries. Use an
  exact-read function for 1 type byte, 4 length bytes, and `length - 4` payload bytes.
- Read until `Z` for every query and accept `S`/`N` anywhere in the stream; do not assume
  the reply is exactly `[T,D,C,Z]`.
- If a read times out or returns < 5 bytes, the stream is not safely resumable. Close.

---

## 7. Abandoned transaction recovery and what `DISCARD ALL` does

### 7.1 `ROLLBACK` is the recovery command

**OBSERVED**, in multiple sessions:

| Starting status | Query | Reply | Ending status |
|---|---|---|---|
| `T` (normal open tx) | `ROLLBACK` | `C("ROLLBACK")`, `Z('I')` | **I** |
| `E` (failed tx from syntax error) | `ROLLBACK` | `C("ROLLBACK")`, `Z('I')` | **I** |
| `E` (failed tx from `DISCARD ALL`) | `ROLLBACK` | `C("ROLLBACK")`, `Z('I')` | **I** |
| `I` (already idle) | `ROLLBACK` | `N(25P01)` warning, `C("ROLLBACK")`, `Z('I')` | **I** |

So `ROLLBACK` is accepted from both `T` and `E`, returns status **I**, and is even legal
(though warning-producing) when already idle. **(OBSERVED)**

`DISCARD ALL` is legal after rollback: it produced `C(16) "DISCARD ALL"` and `Z('I')`.
It is **not** a recovery command while the session is in T or E. **(OBSERVED)**

### 7.2 The prompt's premise about what `DISCARD ALL` does not reset

Contrary to the examples in the task prompt, PostgreSQL's `DISCARD ALL` **does reset**
prepared statements, temporary tables, current-role/session-authorization state, and GUCs.
The current role was changed with `SET ROLE` and was reset in this run; the
`SET SESSION AUTHORIZATION` part of the equivalence is DOCS (not separately exercised with
a different session user). The official PG18 documentation states that `DISCARD ALL` is
currently equivalent to:

```text
CLOSE ALL;
SET SESSION AUTHORIZATION DEFAULT;
RESET ALL;
DEALLOCATE ALL;
UNLISTEN *;
SELECT pg_advisory_unlock_all();
DISCARD PLANS;
DISCARD TEMP;
DISCARD SEQUENCES;
```

Reference: [PostgreSQL 18, DISCARD](https://www.postgresql.org/docs/18/sql-discard.html).

This was verified concretely in one session. Before `DISCARD ALL`:

```text
current_user       = discard_probe_role   (after SET ROLE)
session_user       = postgres
statement_timeout  = 7s
application_name   = discard-probe
client_encoding    = LATIN1
search_path        = pg_catalog
temp table tt      = 1 row
prepared stmt p    = present
backend pid        = 133
```

Then `DISCARD ALL` succeeded and its reply stream was:

```text
C(16) "DISCARD ALL"
S search_path="$user", public
S application_name=
S client_encoding=UTF8
S is_superuser=on
Z(5) 'I'
```

Afterwards, in the **same connection**:

```text
current_user       = postgres
session_user       = postgres
statement_timeout  = 0
application_name   =
client_encoding    = UTF8
search_path        = "$user", public
backend pid        = 133
SELECT count(*) FROM tt       -> ERROR 42P01: relation "tt" does not exist
EXECUTE p(1)                  -> ERROR 26000: prepared statement "p" does not exist
pg_prepared_statements p count= 0
to_regclass('pg_temp.tt') IS NULL -> true
```

(The session had not acquired any advisory locks or listened on any channels, so those were
not used as reset evidence; the documented `UNLISTEN *` and
`pg_advisory_unlock_all()` behavior is DOCS-only in this report.)

All values above were observed. Note the `S` messages after `CommandComplete`: a correct
client must continue to `Z`, not stop at `C`.

### 7.3 What `DISCARD ALL` does **not** reset

**OBSERVED:**

- **The connection/session identity itself.** The backend remained alive with the same
  `pg_backend_pid()` (133 before and after). No new `K` frame is sent; the client must keep
  the startup cancellation key.
- **`backend_start`.** A second clean test showed pid `166` and
  `2026-10-21 17:18:35.937449+00` before and after; identical after `DISCARD ALL`.
- **Persistent database objects/data.** A normal table row remained:
  `CREATE TABLE discard_probe_persistent(x int); INSERT ... (99); DISCARD ALL;`
  then `SELECT ...` still returned `99`.
- **An open transaction.** It cannot run inside one; it errors (25001) and leaves status E.
  It does not roll back automatically. This is the important operational exception.

**INFERRED / DOCS, not separately tested:**

- **Already-desynchronized wire state.** It cannot repair a client that has unread frames;
  the command's own responses would be appended behind them.
- **Database-level/global state** such as roles, grants, and other sessions' state; the
  documented reset is session-scoped.
- **Sequence counters and other persistent database state** are not reset by `DISCARD ALL`;
  `DISCARD SEQUENCES` only discards cached session sequence state, not stored values.

### 7.4 Minimal recovery recipe for an abandoned INTRANS session

1. If socket/framing is aligned: send one simple `Query` containing `ROLLBACK`.
2. Read messages until `ReadyForQuery`; accept `N` (including 25P01) and `C("ROLLBACK")`.
3. Require the `Z` status byte to be **`'I'`**.
4. If desired, then send `DISCARD ALL`; it will succeed from I and return I. This is
   optional for transaction recovery.
5. If the client has any doubt about frame alignment, close and reconnect rather than
   trying to `DISCARD ALL` or issue more queries.

Do not use `DISCARD ALL` as a rollback: it fails inside a transaction and makes the state
worse (T → E).

---

## 8. Supplementary: CancelRequest bytes and observed behavior

This was not required by the numbered steps but is directly relevant to the
transaction-entry cancellation experiment. **(OBSERVED; canceling `BEGIN` itself is
UNVERIFIED.)**

`CancelRequest` has **no type byte** and is sent on a **new connection**:

```text
Int32 length = 16
Int32 cancel code = 80877102  (0x04d2162e)
Int32 target backend pid
Byten secret (4 bytes in protocol 3.0)
```

Exact packet used:

```text
00000010 04d2162e 000000a7 28ba95b2
```

That is: pid 167, secret 683316658. The cancel socket was closed after sending; it
received no reply. On the original connection, after `BEGIN` (status T) and while
`SELECT pg_sleep(5)` was executing:

```text
T(33)                            # RowDescription for pg_sleep, sent before the error
E(104) 57014 "canceling statement due to user request"
Z(5) 'E'
```

Then `ROLLBACK` returned `C("ROLLBACK")` + `Z('I')`, and `SELECT 1` succeeded with status I.
The error payload's exact head/semantics:

```text
C = 57014
M = canceling statement due to user request
```

Implications:

- Store `K`'s pid and secret at startup if cancellation may be used. In protocol 3.0 the
  secret is 4 bytes; in 3.2 the docs specify a variable-length secret.
- A cancel request is fire-and-forget on a separate TCP connection; the original connection
  still emits `ErrorResponse` + `ReadyForQuery`.
- Canceling an in-flight statement inside a transaction leaves the transaction status `E`;
  you still need `ROLLBACK` to return to I.
- Whether a CancelRequest arriving during the very short `BEGIN` command has any effect is
  timing-dependent and was not established here.

References: [CancelRequest / BackendKeyData](https://www.postgresql.org/docs/18/protocol-message-formats.html),
[protocol versions / 3.2 secret change](https://www.postgresql.org/docs/18/protocol-overview.html).

---

## 9. Rust implementation checklist

The following is the minimal, non-guessing implementation consistent with the observations.

### 9.1 Startup

```rust
// startup body: protocol 196608, user, database, final NUL
let mut body = Vec::new();
body.extend_from_slice(&196608u32.to_be_bytes());
body.extend_from_slice(b"user\0postgres\0");
body.extend_from_slice(b"database\0postgres\0");
body.push(0);
let mut frame = ((body.len() + 4) as u32).to_be_bytes().to_vec();
frame.extend_from_slice(&body);
```

Then read ordinary typed messages until `Z`:

- `R` length 8, code 0 = AuthenticationOk. With trust auth this is the only auth frame.
  A general client must handle non-zero auth requests or fail clearly.
- `S`: two NUL-terminated strings; record or ignore. It can also arrive later, including
  after `SET`/`DISCARD ALL` responses.
- `K`: pid + secret (4 bytes in v3.0). Save for CancelRequest.
- `Z`: one status byte; startup is complete.

Do not assume exactly 15 ParameterStatus messages or a fixed order.

### 9.2 Query and reply loop

```rust
// Query
let q = sql.as_bytes();
let mut frame = vec![b'Q'];
frame.extend_from_slice(&((4 + q.len() + 1) as u32).to_be_bytes());
frame.extend_from_slice(q);
frame.push(0);
```

Read exactly:

```text
1 byte type
4 bytes length (u32 big-endian)
length - 4 bytes payload
```

For each query:

- accumulate/emit `T`, `D`, `C`, `S`, `N`, `E`;
- **stop only when `Z` is read**;
- read the `Z` status byte;
- if `E` occurred, status should be `'E'` or (depending on context) the connection may be
  closed for fatal errors; in this probe errors were non-fatal and followed by `Z`.

Recommended state machine:

```text
Send Q
loop:
  read frame
  match type:
    'T' -> parse RowDescription (optional for ints/text)
    'D' -> parse DataRow text values
    'C' -> parse command tag
    'N' -> log notice, continue
    'S' -> record ParameterStatus, continue
    'E' -> record SQLSTATE/message, continue to Z
    'Z' -> capture status and end this query
    other -> protocol error; close connection
```

There is no query ID and no way to match a `D` to a specific SQL string other than ordering.
This is why a lost/forgotten reply cannot be repaired by sending another query.

### 9.3 Values needed

`RowDescription` field descriptor (after the Int16 field count), for each field:

```text
String name, NUL-terminated
Int32  table OID
Int16  attribute number
Int32  type OID
Int16  type size
Int32  type modifier
Int16  format code
```

`DataRow`:

```text
Int16 field count
per field:
  Int32 value length
  if length == -1: NULL (no bytes)
  else: exactly length bytes of text
```

For the two integer queries here: type OID 23 (`int4`), format code 0 (text), value bytes
are ASCII (`"1"`, `"42"`, pid). Parse the ASCII decimal; no binary decoding needed.

`CommandComplete` is a NUL-terminated tag, not the SQL. `SELECT pg_backend_pid()` returns
tag `"SELECT 1"`. Tags observed: `SELECT 1`, `BEGIN`, `ROLLBACK`, `DISCARD ALL`, `COMMIT`,
`SET`, `CREATE TABLE`, etc.

### 9.4 Status handling

- After startup: **I**.
- After successful `BEGIN`: **T**.
- Failed command inside a transaction: **E**.
- After successful `ROLLBACK`: **I**.
- A failed command outside a transaction generally still ends **I** (e.g. `SELECT * FROM
  missing_table`), because there was no explicit transaction block. The syntax-error test was
  deliberately run *inside* BEGIN, which is why it produced E. This external-vs-inside
  distinction is important: a bare `E` error response does **not** always mean status E.
  **(OBSERVED for the inside-tx case; the outside case is DOCS/INFERRED and was not
  separately run.)**
- `DISCARD ALL` only works from I. Sent from T it errors and leaves E; from E, roll back
  first (the E case with `DISCARD ALL` itself was not separately sent).

### 9.5 No need for encoding/DateStyle in this scope

Use UTF-8 (or ASCII) SQL and expect text DataRows. Keep `client_encoding=UTF8` from the
server's ParameterStatus, or explicitly request it at startup. Do not format dates/timestamps
in this experiment. `DateStyle` is irrelevant for integer/text-only traffic.

---

## 10. Reproduction, raw outputs, sources and unverified items

### 10.1 Scripts used (all under `/tmp`)

| Script | Purpose | Output |
|---|---|---|
| `/tmp/pgwire_probe.py` | startup + all required simple queries, JSON and textual trace | `/tmp/pgwire_probe.out`, `/tmp/pgwire_probe.json` |
| `/tmp/pgwire_desync.py` | forget/skip BEGIN reply, send SELECT 42 then 43 | `/tmp/pgwire_desync.out` |
| `/tmp/pgwire_discard.py` | `DISCARD ALL` reset/no-reset checks | `/tmp/pgwire_discard.out`, `/tmp/pgwire_discard.json` |
| `/tmp/pgwire_extra.py` | idle ROLLBACK, COMMIT in E | `/tmp/pgwire_extra.out` |
| `/tmp/pgwire_cancel.py` | supplementary CancelRequest test | `/tmp/pgwire_cancel.out` |
| `/tmp/pg_discard_persist.sql` | pid/backend_start/persistent-row check via psql | `/tmp/pg_discard_persist.out` |

All used Python stdlib (`socket`, `struct`, `json`) only. The container was reached over
TCP at `127.0.0.1:55432`.

Representative commands:

```bash
python3 /tmp/pgwire_probe.py
python3 /tmp/pgwire_desync.py
python3 /tmp/pgwire_discard.py
python3 /tmp/pgwire_extra.py
python3 /tmp/pgwire_cancel.py
docker exec -i pgonlineprobe psql -X -A -t -U postgres -d postgres < /tmp/pg_discard_persist.sql
```

### 10.2 Key raw stdout

Startup/probe:

```text
C->S StartupMessage len=41 hex=00000029000300007573657200706f73746772657300646174616261736500706f7374677265730000
S->C startup replies:
  R(len=8,frame=9)
  S(len=23,frame=24) in_hot_standby='off'
  ...
  K(len=12,frame=13) pid=128 secret=3326645211
  Z(len=5,frame=6) status='I'
  ParameterStatus map: {... "client_encoding": "UTF8", "server_version": "18.4" ...}
```

Status summary in order (fourteen queries):

```json
[
  {"sql":"SELECT 1","status_after":"I"},
  {"sql":"BEGIN","status_after":"T"},
  {"sql":"SELECT 1","status_after":"T"},
  {"sql":"ROLLBACK","status_after":"I"},
  {"sql":"DISCARD ALL","status_after":"I"},
  {"sql":"COMMIT","status_after":"I"},
  {"sql":"BEGIN","status_after":"T"},
  {"sql":"SELEC 1","status_after":"E"},
  {"sql":"ROLLBACK","status_after":"I"},
  {"sql":"BEGIN","status_after":"T"},
  {"sql":"DISCARD ALL","status_after":"E"},
  {"sql":"ROLLBACK","status_after":"I"},
  {"sql":"DISCARD ALL","status_after":"I"},
  {"sql":"SELECT pg_backend_pid()","status_after":"I"}
]
```

Desync scenario A, verbatim excerpt:

```text
A1: C->S Q 'BEGIN' frame_hex=510000000a424547494e00
  (deliberately reads NOTHING: BEGIN's C and Z remain in the socket)
A2: C->S Q 'SELECT 42' frame_hex=510000000e53454c45435420343200
client reads until next Z and calls this SELECT 42's response:
  C(len=10,frame=11) frame_hex=430000000a424547494e00 payload_hex=424547494e00
    C(len=10,frame=11) tag='BEGIN'
  Z(len=5,frame=6) frame_hex=5a0000000554 payload_hex=54
    Z(len=5,frame=6) status='T'
A3: C->S Q 'SELECT 43' ...
client reads ... these are actually SELECT 42's queued replies:
  T(33) ... D(12) values=['42'] C(13) tag='SELECT 1' Z(T)
```

`DISCARD ALL` before/after excerpt from psql:

```text
before|166|2026-10-21 17:18:35.937449+00
CREATE TABLE
INSERT 0 1
DISCARD ALL
after|166|2026-10-21 17:18:35.937449+00
persistent_row|99
DROP TABLE
```

### 10.3 Not executed / UNVERIFIED

- Protocol **3.2** startup and its variable-length `BackendKeyData`/CancelRequest secret.
- TLS/SSL negotiation, GSSAPI, SCRAM/password authentication (server used trust).
- A sub-frame/partial-byte desynchronization; only whole-frame skips were executed. The
  partial-frame conclusion is inferred from framing.
- Canceling the `BEGIN` command itself; only cancellation of an in-flight `SELECT` inside an
  open transaction was exercised.
- Multi-statement `Query` strings (multiple commands in one `Q`; one `Z` expected per docs).
- Empty `Query` (`I`/EmptyQueryResponse), COPY, extended query protocol, notifications,
  prepared statements via the extended Parse/Bind protocol. SQL-level `PREPARE` was tested
  for DISCARD behavior.
- Very large messages / multiple TCP packets per DataRow.
- The outside-transaction error status (I) was not separately captured in this run.

### 10.4 Official documentation consulted

The documentation pages were retrieved from the official PostgreSQL 18 site during this
run. (`web_fetch` could not resolve `www.postgresql.org` because the environment's DNS
returned a non-public address; `curl` to the same official URLs succeeded. The facts above
are primarily direct observations, with docs used for protocol/status semantics.)

- [Frontend/Backend Protocol — Protocol Versions](https://www.postgresql.org/docs/18/protocol-overview.html)
- [Frontend/Backend Protocol — Message Formats](https://www.postgresql.org/docs/18/protocol-message-formats.html)
- [Frontend/Backend Protocol — Flow (Simple Query, startup)](https://www.postgresql.org/docs/18/protocol-flow.html)
- [Error and Notice Message Fields](https://www.postgresql.org/docs/18/protocol-error-fields.html)
- [SQL DISCARD](https://www.postgresql.org/docs/18/sql-discard.html)

---

## 11. Bottom line for the cancellation experiment

1. A successful `BEGIN` is `C("BEGIN")`, `Z('T')`; there is no transaction ID in the wire
   response.
2. If the client fails to consume those frames, the next query can appear to complete using
   the old `C`/`Z`. The server has already executed the next query in the open transaction;
   its real result sits behind the old frames. There are no request IDs to recover ordering.
3. The simplest legal state recovery is `ROLLBACK`; it works from both T and E and returns I.
   It also works (with a 25P01 warning) from I.
4. `DISCARD ALL` is **not** a rollback. It fails inside a transaction with SQLSTATE 25001 and
   leaves the transaction at E. After a successful `ROLLBACK`, it is legal and does reset
   prepared statements, temp tables, the current role (and per docs the session
   authorization), GUCs, etc.
5. A correctly implemented Rust client must exact-read typed frames, handle `S`/`N` at any
   point, read to `Z`, and close/reconnect on any framing uncertainty. For a v3.0 client,
   parse the 4-byte cancel secret from `K`; for 3.2, parse the variable-length secret.
