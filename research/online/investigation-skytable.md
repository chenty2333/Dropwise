# skytable 0.8.12: simple-query wire protocol, drop state, and the poison patch

Investigation date: 2026-10-22. Read-only source inspection plus one disposable
live server (`skytable/skytable:v0.8.4`). No cargo build was run. The live
container and its `/tmp` data dir were removed.

**Short answers.** Handshake: 6-byte `H\0\0\0\0\0` block + length-prefixed
username/password, one fixed 4-byte reply, no separate auth round trip. A
simple query is one `S<total>\n<q_window>\n<payload>` frame in one `write_all`;
a pipeline is a `P<total>\n` header plus N frames in **two** `write_all`s.
Replies are type-tagged, with no outer length and no request id. Stock: a drop
leaves the socket desynchronised, so the next call reads the old reply (stale)
or a mid-frame remainder. Patched: a single `poisoned` bool fails the next call
closed and the bb8 managers discard the slot; it cannot distinguish
full-write/pending from partial-write and does not drain. A tokio-only client
can handshake and send `S23\n20\nsysctl report status` (reply `0x12`) in ~40
lines. The v0.8.4 image is present and runs on an ephemeral loopback port with
a disposable data dir.

## 0. Paths, and a correction to the tree labels

| label | path | what it actually is |
|---|---|---|
| `STOCK` | `/home/ava/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/skytable-0.8.12` | untouched crates.io 0.8.12 source |
| `PATCH` | `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/skytable/fix/skytable-0.8.12` | **already patched** 0.8.12 source |
| `PROBE` | `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/skytable/fix/p4e-skytable-fixed` | probe binary crate, *not* the patched library |
| `LIFECYCLE` | `/home/ava/Desktop/Dropwise/prospective/phase4-exploration/clients/skytable` | harness, `run-lifecycle.sh`, logs, `RESULTS-FRAGMENT-DEEP.md` |

**The task labels are inverted.** `fix/skytable-0.8.12` is not a fresh copy:
`LIFECYCLE/fix/README.md:69-72` says "The unit tests run in the patched copy"
and "The copy is already patched". A diff of `PATCH/src/io/aio.rs` against
`STOCK/src/io/aio.rs` shows the `poisoned` / `PoisonOnDrop` additions. The
"patched copy" directory `fix/p4e-skytable-fixed` is a probe binary crate
whose source is only `src/bin/sky-ladder.rs` and `src/bin/sky-lifecycle.rs`
(plus manifest/lock/target); its manifest says:

```toml
[patch.crates-io]
skytable = { path = "../skytable-0.8.12" }
```
(`PROBE/Cargo.toml:17-18`; `skytable = "=0.8.12"` is at `:12`)

Consequently all stock line references below are to `STOCK`; all patched line
references are to `PATCH`. This is material: citing `fix/skytable-0.8.12` as
stock would make the bug look absent.

Protocol identity (from `STOCK src/io/aio.rs:44-50`): `Skyhash/2.0`,
query mode `QTDEX-1A/BQL-S1`, auth plugin `pwd`.

Live probe that produced all "observed" bytes below: container
`skytable/skytable:v0.8.4`, image id
`sha256:823e1a390a55ddcc5579e27af58cf6f92d92a7735c5fd4fc3551673b22d08f41`,
internal TCP endpoint `0.0.0.0:24003`, published to an ephemeral
`127.0.0.1:<port>`, disposable data dir, root password
`investigate-pass-0123` (21 chars). Server logged
`listening on tcp@0.0.0.0:24003`. Python/probe conversation below.

## 1. Wire protocol of one simple query

### 1.1 Connect: handshake (no separate auth round trip)

`Config::connect_async()` creates the TCP stream and immediately calls
`TcpConnection::_handshake` (`STOCK src/io/aio.rs:84-93`, `:138-147`). The
handshake is one `write_all` followed by one exact 4-byte read:

```rust
async fn _handshake(mut self, cfg: &Config) -> ClientResult<Self> {
    let handshake = ClientHandshake::new(cfg);
    self.con.write_all(handshake.inner()).await?;
    let mut resp = [0u8; 4];
    self.con.read_exact(&mut resp).await?;
    match ServerHandshake::parse(resp)? {
        ServerHandshake::Error(e) => return Err(ConnectionSetupError::HandshakeError(e).into()),
        ServerHandshake::Okay(_suggestion) => return Ok(self),
    }
}
```
(`STOCK src/io/aio.rs:138-147`)

The bytes are the 6-byte version block, then `pushlen(username)`,
`pushlen(password)`, username, password
(`STOCK src/protocol/handshake.rs:31-35`, `:43-51`):

```rust
Self::V2_0 => [b'H', 0, 0, 0, 0, 0],
```
```rust
fn _new(hs: [u8; 6], cfg: &Config) -> Self {
    let mut v = Vec::with_capacity(6 + cfg.username().len() + cfg.password().len() + 5);
    v.extend(hs);
    pushlen!(v, cfg.username().len());
    pushlen!(v, cfg.password().len());
    v.extend(cfg.username().as_bytes());
    v.extend(cfg.password().as_bytes());
    Self(v.into_boxed_slice())
}
```

`pushlen!` is just ASCII decimal + LF (`STOCK src/macros.rs:39-46`):

```rust
macro_rules! pushlen {
    ($buf:expr, $len:expr) => {{
        let mut buf = ::itoa::Buffer::new();
        let r = ::itoa::Buffer::format(&mut buf, $len);
        $buf.extend(str::as_bytes(r));
        $buf.push(b'\n');
    }};
}
```

So for `root` + 21-char password the exact 36-byte handshake observed was:

```text
48 00 00 00 00 00 | 34 0a | 32 31 0a | 72 6f 6f 74 | 69 6e 76 65 73 74 69 67 61 74 65 2d 70 61 73 73 2d 30 31 32 33
H  \0 \0 \0 \0 \0     4 \n    21 \n    r o o t       investigate-pass-0123
```

The server replied exactly four bytes, `48 00 00 00` (`H 0 0 0`). The parser is
fixed-width and has no session id (`STOCK src/protocol/handshake.rs:62-73`):

```rust
pub fn parse(v: [u8; 4]) -> ClientResult<Self> {
    Ok(match v {
        [b'H', 0, 0, msg] => Self::Okay(msg),
        [b'H', 0, 1, msg] => Self::Error(msg),
        _ => {
            return Err(Error::ConnectionSetupErr(
                ConnectionSetupError::InvalidServerHandshake,
            ))
        }
    })
}
```

Handshake complexity is **low**: one request, one fixed 4-byte reply, no
challenge/salt/hash, and no further auth message. The password is sent raw
(TLS is the only encryption layer). Overhead is 6 bytes + decimal lengths + LF
per credential: 36 bytes for `root`/21-char, 38 bytes for `root`/23-char.
A wrong password yields `H 0 1 <code>` and
`ConnectionSetupError::HandshakeError(code)`; no query can be sent on that
connection.

### 1.2 A simple query is one frame, one `write_all`

`Query::write_packet` builds the whole packet in the connection's `buf`
(`STOCK src/query.rs:99-122`):

```rust
pub(crate) fn write_packet(&self, buf: &mut impl Write) -> io::Result<()> {
    /*
        [[total packet size][query window]][[dataframe][qframe]]
        ^meta1            ^meta2           ^payload
    */
    let mut query_window_buffer = itoa::Buffer::new();
    let query_window_str = query_window_buffer.format(self.q_window);
    let total_packet_size = query_window_str.len() + 1 + self.buf.len();
    let mut total_packet_size_buffer = itoa::Buffer::new();
    let total_packet_size_str = total_packet_size_buffer.format(total_packet_size);
    // segment 1: meta
    buf.write_all(b"S")?;
    buf.write_all(total_packet_size_str.as_bytes())?;
    buf.write_all(b"\n")?;
    // segment 2: variable meta
    buf.write_all(query_window_str.as_bytes())?;
    buf.write_all(b"\n")?;
    // segment 3: payload
    buf.write_all(&self.buf)?;
    Ok(())
}
```

Wire shape:

```text
S<total>\n<q_window>\n<payload>
```

* `<total>` = `len(decimal(q_window)) + 1 + len(payload)`, i.e. the number of
  bytes **after the first LF**. It includes the `<q_window>` digits and their
  LF, and the payload (query text + encoded parameters).
* `<q_window>` = number of query-string bytes (the `?` placeholders count;
  parameters are appended after the query text). `Query` stores
  `q_window` separately from the parameter tail (`STOCK src/query.rs:50-54`).
* Payload = query text followed by zero or more encoded parameters
  (`STOCK src/query.rs:271-380`).

For the simplest supported command, no schema needed:

```text
53 32 33 0a 32 30 0a 73 79 73 63 74 6c 20 72 65 70 6f 72 74 20 73 74 61 74 75 73
S  2  3  \n 2  0  \n s  y  s  c  t  l     r  e  p  o  r  t     s  t  a  t  u  s
```

i.e. `S23\n20\nsysctl report status` (27 bytes on the wire; the size field says
23 bytes after the first LF). Server reply was the single byte `12` (Empty).
This was observed live.

The socket write is a **single** `write_all` of the complete encoded packet
(`STOCK src/io/aio.rs:183-187`):

```rust
pub async fn query(&mut self, q: &Query) -> ClientResult<Response> {
    self.buf.clear();
    q.write_packet(&mut self.buf).unwrap();
    self.con.write_all(&self.buf).await?;
    self.buf.clear();
```

"Single" means one `write_all` call; `write_all` itself may issue multiple
`poll_write` syscalls, and cancellation can happen inside it (see §1.6c).

### 1.3 Pipelining: header and body are two `write_all` calls

`Pipeline::push` encodes each query as
`<q_window>\n<param_len>\n<query+params>` (`STOCK src/query.rs:164-179`).
`execute_pipeline` writes `P<pipeline-body-len>\n` and then the concatenated
body (`STOCK src/io/aio.rs:149-181`):

```rust
pub async fn execute_pipeline(&mut self, pipeline: &Pipeline) -> ClientResult<Vec<Response>> {
    self.buf.clear();
    self.buf.push(b'P');
    // packet size
    self.buf
        .extend(itoa::Buffer::new().format(pipeline.buf().len()).as_bytes());
    self.buf.push(b'\n');
    // write
    self.con.write_all(&self.buf).await?;
    self.con.write_all(pipeline.buf()).await?;
    self.buf.clear();
```

So a pipeline of one no-parameter `sysctl` query is:

```text
P25\n20\n0\nsysctl report status
```

(`P` + body length 25; body = `20\n` + `0\n` + 20 query bytes). The body length
is **not** including `P25\n`. The two `write_all`s are a cancellation point
between header and body. In contrast, a simple `query` is one `write_all` of
the whole frame.

### 1.4 Reply framing: no outer length, type-tagged, positional

There is no reply size header. The first byte is the response type; the rest
is decoded structurally (`STOCK src/protocol/mod.rs:90-129`):

```rust
match self.next() {
    // TODO(@ohsayan): this is reserved!
    0x0F => DecodeState::Error(ProtocolError::InvalidServerResponseUnknownDataType),
    0x10 => self.complete_error(),
    0x11 => self.complete_row(ValueStream::initialize(&self)),
    0x12 => DecodeState::Completed(Response::Empty),
    0x13 => self.complete_rows(MultiValueStream::initialize(&self)),
    code => match PendingValue::next_value_with_code(&mut self, code) {
```

Response layouts:

| first byte | meaning | remaining bytes |
|---|---|---|
| `0x10` | server error | `u16` error code, little-endian |
| `0x11` | one row | column count (decimal + LF), then that many values |
| `0x12` | Empty | none |
| `0x13` | multiple rows | stream count + LF, per-stream size + LF, then values |
| `0x00`-`0x0E` | a scalar/list value | see value codes below |
| `0x0F` | reserved/unknown | client protocol error |

Value codes (`STOCK src/protocol/mod.rs:480-503`):

| code | value | encoding |
|---|---|---|
| `0x00` | Null | none |
| `0x01` | Bool | one byte 0/1 |
| `0x02`..`0x05` | u8/u16/u32/u64 | ASCII decimal + LF |
| `0x06`..`0x09` | i8/i16/i32/i64 | ASCII decimal + LF |
| `0x0A`,`0x0B` | f32/f64 | ASCII decimal + LF |
| `0x0C` | binary | length decimal + LF, then raw bytes |
| `0x0D` | string | length decimal + LF, then UTF-8 bytes |
| `0x0E` | list | element count decimal + LF, then values |

A row reply is `0x11` + column-count line + values; a multi-row reply is
`0x13` + stream-count line + stream-size line + values. Pipelined replies are
just those response encodings concatenated in request order; the client
supplies the expected count (`pipeline.query_count()`) to
`Decoder::validate_pipe` (`STOCK src/protocol/pipe.rs:42-76`).

Live examples:

```text
sysctl report status  ->  0x12
```

```text
S41\n34\nselect * from inv1.kv where id = ?\x06\x31\x0a\x6b   (45 bytes)
  -> 11 32 0a 0d 31 0a 6b 05 34 32 34 32 0a
     ^^ ^^^^^ ^^ ^^^^^ ^^ ^^ ^^^^^^^^^^^^
     Row  "2\n" Str "1\n" "k" u64 "4242\n"
```

That select reply means: `Response::Row(["k", 4242])`, confirmed live.

### 1.5 Which functions write and read

| role | function | exact I/O |
|---|---|---|
| connect (TCP) | `Config::connect_async` -> `TcpConnection::_handshake` | `TcpStream::connect`; one `write_all(handshake)`; one `read_exact(4)` (`STOCK src/io/aio.rs:86-93`, `:138-147`) |
| connect (TLS) | `Config::connect_tls_async` -> `_handshake` | TLS handshake, then same skytable handshake (`:96-121`) |
| simple query | `TcpConnection::query` | one `write_all` of the `S...` packet; repeated `read` into an 8 KiB stack chunk, appended to `self.buf` (`:183-212`) |
| parsed query | `TcpConnection::query_parse` | delegates to `query`, then `FromResponse` (`:214-216`) |
| pipeline | `TcpConnection::execute_pipeline` | two `write_all`s (header, body); repeated `read` (`:149-181`) |
| decode | `Decoder::validate_response` / `validate_pipe` | pure in-memory decoder over `self.buf` + local cursor (`STOCK src/protocol/mod.rs:90-129`; `STOCK src/protocol/pipe.rs:42-76`) |
| pool validation | `ConnectionMgrTcp::is_valid` (bb8) | `query_parse::<()>(sysctl report status)` (`STOCK src/pool.rs:117-119`) |

The sync client uses the same bytes with `std::io::{Read, Write}`; it has no
async drop point and is not covered by the patch (`STOCK src/io/sync.rs:133-206`).
The read chunk size is `BUFSIZE = 8 * 1024` (`STOCK src/lib.rs:147`).

### 1.6 What happens if the query future is dropped

Cancellation is only possible at an `.await`. The relevant awaits are
`write_all` (one for query, two for pipeline), `read`, and the connect
handshake. The decoder itself is synchronous within a poll.

| drop point | stock 0.8.12 | patched copy |
|---|---|---|
| **(a) before first poll** | async fn body never runs; no bytes, no state change; connection reusable | guard not created; `poisoned` unchanged (still usable only if it was already clean) |
| **(b) request fully written, reply only partly (or not) read** | no flag. Partial reply bytes are in `self.buf`; unread bytes stay in the kernel socket. Next call clears `buf`, writes a new request, then reads the old reply/remainder as its own. | guard is armed; drop sets `poisoned`; next call returns `IoError(ConnectionAborted, "skytable: connection poisoned by an incomplete operation")` without writing |
| **(c) during a partial `write_all`** | no flag. A prefix of the request may be on the wire; for a simple query, `self.buf` still holds the full encoded packet until the write completes. Next call clears `buf` and writes a fresh request behind the truncated one; server sees a malformed/concatenated packet (prior deep run observed server error 6, "illegal client packet"). | guard is armed; drop sets `poisoned`; next call fails closed |

Additional detail for (b): cancellation at `read` means the read future was
`Pending`; any bytes already returned by earlier `read` calls were appended to
`self.buf`, but the decoder cursor is local and is lost. If the future is
dropped after `write_all` completed, there is no separate cancellation point
between the write and the read: the next await is the read itself, so the state
is exactly "request may be complete, reply not fully consumed".

For (c): if zero bytes happened to be written, the connection might physically
be recoverable, but neither the stock nor patched client knows how many bytes
were accepted. The patch deliberately treats even that case as poisoned.

Evidence that this is not theoretical: the live lifecycle run
`LIFECYCLE/logs/run.7m5uyt/` (pipeline of a one-row select, 10 ms cut, 50 ms
server->client delay) produced:

```text
stock-direct.log:  LIFECYCLE label=direct outcome=Stale stale_value=0
fixed-direct.log:  LIFECYCLE label=direct outcome=Blocked
stock-pool2.log:   LIFECYCLE label=pool2 outcome=Stale stale_value=0
fixed-pool2.log:   LIFECYCLE label=pool2 outcome=Fresh
```

The deeper wire trace is in `LIFECYCLE/logs/run.CncmjF/` and is summarized in
`LIFECYCLE/RESULTS-FRAGMENT-DEEP.md:145-160` (direct read-stage shift) and
`:308-345` (counterexamples). I did not re-run those cuts in this session.

## 2. Internal state of `TcpConnection` after a drop

Stock struct (`STOCK src/io/aio.rs:124-129`):

```rust
pub struct TcpConnection<C: AsyncWriteExt + AsyncReadExt + Unpin> {
    con: C,
    buf: Vec<u8>,
}
```

Patched adds only `poisoned: bool` (`PATCH src/io/aio.rs:126-133`).

* `buf` is the connection's only persistent byte buffer. It starts at
  `Vec::with_capacity(BUFSIZE)` (`STOCK src/io/aio.rs:131-137`) and is used both
  to build the request and to accumulate reply bytes.
* There is **no stored read position**. `cursor`, `state`, and `expected` are
  locals of the `query` future (`STOCK src/io/aio.rs:188-190`); pipeline's
  `cursor`/`MRespState` are locals of `execute_pipeline` (`:161-162`). When the
  future is dropped, the decoder position is gone.
* On drop during read, `buf` remains with every byte that was read and
  appended so far (possibly a partial response), and the socket receive queue
  still holds the bytes not yet read. `buf` is not cleared or truncated on
  drop.
* On drop during a simple query's write, `buf` still contains the full encoded
  request because `buf.clear()` happens only after `write_all` returns
  (`STOCK src/io/aio.rs:184-187`); a prefix may already be on the wire. For a
  pipeline, the header write is followed by `buf.clear()` before the body is
  written from `pipeline.buf()` (`STOCK src/io/aio.rs:156-159`), so a drop
  during the body write leaves `buf` empty but the caller-owned pipeline body
  still intact; a prefix may be on the wire.
* After a *successful* query, `buf` also still contains the reply bytes; it is
  cleared at the start of the next call. That is why a reply plus an
  accidentally coalesced extra response can lose the extra bytes at the next
  `buf.clear()`.

The next `query` call in stock (`STOCK src/io/aio.rs:183-212`) executes:

```rust
self.buf.clear();
q.write_packet(&mut self.buf).unwrap();
self.con.write_all(&self.buf).await?;
self.buf.clear();
let mut state = RState::default();
let mut cursor = 0;
```

The `clear()` discards any partial old reply prefix; the decoder starts at
position 0, state `Initial`.

It never waits for, drains, or discards the old reply from the socket. On the
next read, the oldest queued bytes are the old reply (or its unread remainder),
and the decoder has no way to know they belong to a previous request.

Why the old reply can be accepted as the new one:

1. TCP on one connection is a single ordered byte stream. The server handles
   queries on a connection serially and emits replies in request order.
2. The client writes the new request *before* reading, so the old reply is
   already ahead of the new reply in the stream (or arrives first).
3. The response encoding is self-describing but carries **no request id,
   sequence number, transaction id, or correlation field**. `Query` has only
   `buf`/`param_cnt`/`q_window` (`STOCK src/query.rs:50-54`); the request frame
   has only size/query-window/payload; `Response` is only
   `Empty|Value|Row|Rows|Error(u16)` (`STOCK src/response.rs:139-150`).
   The handshake has only version + credentials. Association is purely
   positional.
4. A stale `0x12` from a completed update/select-empty is byte-identical to a
   valid `sysctl report status` reply. A stale row reply is a valid row reply
   for the next row select. The decoder therefore returns it as `Ok`.

This is exactly what the stock pool does. Its bb8 manager sends
`sysctl report status` during checkout validation and hard-codes
`has_broken=false` (`STOCK src/pool.rs:117-123`):

```rust
async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Self::Error> {
    conn.query_parse::<()>(&query!(QUERY_SYSCTL_STATUS)).await
}
fn has_broken(&self, _: &mut Self::Connection) -> bool {
    false
}
```

A cancelled pooled operation returns the connection to the pool. There are two
cases:

* With `test_on_check_out(true)`, validation sends `sysctl` and expects Empty.
  If the stale reply is `0x12` (e.g. the cancelled operation was an update),
  the validation accepts it and hands the still-desynchronised stream to the
  next user; the user's query then consumes the validation reply, one behind.
  If the stale reply is a Row, `query_parse::<()>` fails, bb8 records
  `ClosedInvalid` and discards/recreates the slot, so the next user gets a
  fresh connection; the old connection is discarded, not resynchronised.
* With `test_on_check_out(false)` there is no validation at all; the next user
  reads the stale reply directly. The lifecycle's `pool2` and `tls-pool` runs
  use exactly this setting (`sky-lifecycle.rs:226-227` and `:265-266`), which
  is why stock reports `Stale` there.

The detailed pooled trace (update/Empty case) is
`LIFECYCLE/RESULTS-FRAGMENT-DEEP.md:194-229`; the live logs are the `Stale(0)`
lines quoted in §1.6.

Partial-read nuance: if the client had already read a prefix of the old reply
and the future is dropped, the next call's `buf.clear()` discards that prefix
while the remaining bytes are still in the kernel. The new decoder then starts
in the middle of an old frame. Depending on the byte values this is a protocol
error or a plausible-looking wrong response; either way the stream is
unusable. If the client read the whole old reply plus part of the next reply
in one `read`, the decoder completes on the old reply and the extra bytes stay
in `buf` until the next `clear()`, where they are lost.

## 3. The patched copy: `poisoned` + `PoisonOnDrop`

### 3.1 Flag and guard

`PATCH src/io/aio.rs:126-149`:

```rust
pub struct TcpConnection<C: AsyncWriteExt + AsyncReadExt + Unpin> {
    con: C,
    buf: Vec<u8>,
    /// Set when a query or pipeline future was dropped before it consumed the whole
    /// reply. The stream position is then unknown; every later operation on
    /// this connection fails instead of reading another request's reply.
    poisoned: bool,
}

struct PoisonOnDrop<'a> {
    poisoned: &'a mut bool,
    armed: bool,
}

impl Drop for PoisonOnDrop<'_> {
    fn drop(&mut self) {
        if self.armed {
            *self.poisoned = true;
        }
    }
}
```

`new` initializes `poisoned: false` (`PATCH src/io/aio.rs:151-158`).
`is_poisoned()` and `poisoned_error()` are private to the crate
(`:162-170`); `is_poisoned` is `pub(crate)`, so the patch adds **no public
API** and no new error variant. Direct users only observe
`Error::IoError(ConnectionAborted, "skytable: connection poisoned by an
incomplete operation")`.

### 3.2 Exact arming and disarming points

`query` (`PATCH src/io/aio.rs:238-280`):

```rust
pub async fn query(&mut self, q: &Query) -> ClientResult<Response> {
    if self.poisoned {
        return Err(Self::poisoned_error());
    }
    // Split the borrow so the guard can watch `poisoned` while the body
    // uses the socket and the response buffer.
    let Self { con, buf, poisoned } = self;
    let mut guard = PoisonOnDrop {
        poisoned,
        armed: true,
    };
    buf.clear();
    q.write_packet(buf).unwrap();
    con.write_all(buf).await?;
    buf.clear();
```

The only disarm is immediately before the successful return
(`PATCH src/io/aio.rs:267-271`):

```rust
DecodeState::Completed(resp) => {
    guard.armed = false;
    return Ok(resp);
}
```

`execute_pipeline` has the identical pattern: early `poisoned` check, guard
armed before `buf.clear()` (`PATCH src/io/aio.rs:188-196`), disarm only on
`PipelineResult::Completed` (`:219-222`), and an armed early return on
`PipelineResult::Error` (`:227`).

Consequences:

* Arming happens on the **first poll of a clean connection**, after the entry
  check, before any byte is written. A future dropped before first poll never runs the body and does
  not poison (`unpolled_query_does_not_poison`,
  `unpolled_pipeline_does_not_poison`, `PATCH src/io/aio.rs:495-510`).
* A complete success **or a complete business/server error** disarms: both are
  `DecodeState::Completed` / `PipelineResult::Completed`; a `Response::Error`
  is a complete reply, not a decode error, so the connection is not poisoned.
* Every other exit leaves the guard armed and poisons: `?` on
  `write_all`/`read`; explicit EOF (`read` returns 0) at `:259-261`/
  `:212-214`; protocol error at `:277`; pipeline error at `:227`.
* `query_parse` delegates to `query` (`:282-284`). A `FromResponse` parse
  failure happens after `query` disarmed, so it does **not** poison; the reply
  was fully consumed, which is correct.
* `_handshake` is not guarded (`:171-180`). A failed connect is discarded
  anyway; there is no user-visible connection to poison.
* `reset_buffer` only calls `shrink_to_fit` (`:285-289`); it does not reset
  `poisoned`.

### 3.3 The pool manager

The async TCP manager (`PATCH src/pool.rs:121-132`) and the async TLS manager
(`:172-183`) both do:

```rust
async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Self::Error> {
    if conn.is_poisoned() {
        return Err(Error::IoError(std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            "skytable: connection poisoned by an incomplete operation",
        )));
    }
    conn.query_parse::<()>(&query!(QUERY_SYSCTL_STATUS)).await
}
fn has_broken(&self, conn: &mut Self::Connection) -> bool {
    conn.is_poisoned()
}
```

How bb8 uses this (verified in `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/bb8-0.9.1/`):

* `inner.rs:149` calls `has_broken` in `put_back` on **every** return. If true,
  the connection is dropped, `ClosedBroken` is recorded, and capacity is
  replenished; it is not put back. This is why the patch works even with
  `test_on_check_out(false)`.
* `inner.rs:108-110` calls `is_valid` on checkout only when
  `test_on_check_out` is true. A poisoned connection returns the error before
  the `sysctl` write, so bb8 records `ClosedInvalid` and retries/creates another.
* `api.rs:531-541` (`Drop for PooledConnection`) calls `put_back`, so the
  `has_broken` check happens when the `PooledConnection` is dropped (after the
  cancelled future's guard has already set the flag).

Net result: a cancelled pooled operation is discarded at return; the next
checkout creates a fresh connection. Live `fixed-pool2`/`fixed-tls-pool`
outcomes are `Fresh`, versus `Stale(0)` for stock (`LIFECYCLE/logs/run.7m5uyt/`).

**Limits:** only the **async (bb8)** TCP and TLS managers are patched. The sync
`r2d2` managers are unchanged and still return `false` from `has_broken`
(`PATCH src/pool.rs:101-113` for TCP, `:152-164` for TLS). The sync path has no
async drop point; `LIFECYCLE/fix/README.md:90` says it is unchanged.

### 3.4 Does the patch distinguish "full write + reply pending" from "partial write"?

**No.** There is a single boolean and a single error. The guard is armed before
`buf.clear()` and before the first write, so:

* full packet written, reply not fully read -> `poisoned = true`;
* partial write, future dropped -> `poisoned = true`;
* I/O error, EOF, protocol error -> `poisoned = true`.

There is no byte counter, no "write completed" flag, no "reply started" flag,
and no distinct error kind/message. `PoisonOnDrop` holds only
`&mut bool`; it has no access to `con`, so it cannot know write progress.

### 3.5 Does it drain, close, or resynchronise?

**No.** The poisoned path returns before any `read`; `PoisonOnDrop::drop` only
sets the bool. There is no `shutdown`, no `drain`, no discard loop, no
reconnect, and no request replay in the patch (a grep for
`shutdown`/`drain`/`request_id`/`sequence` in `PATCH src/` found only test
helper `poll_shutdown` methods and no protocol ID). The socket stays open until
the `ConnectionAsync` value is dropped: the pool does this when `has_broken`
discards the slot; a direct user must drop the handle and reconnect. The
cancelled request may already have executed on the server; the patch neither
replays nor rolls it back (`LIFECYCLE/fix/README.md:79-90`).

## 4. Minimal byte-level client for a real v0.8.4 server

The handshake/auth is simple enough that a tokio-only client is ~40 lines; the
`skytable` crate is not required. This is the smallest useful probe: handshake,
then `sysctl report status`, then read the one-byte Empty reply.

```rust
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn pushlen(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(n.to_string().as_bytes());
    out.push(b'\n');
}

async fn open(host: &str, port: u16, user: &str, pass: &str) -> std::io::Result<TcpStream> {
    let mut s = TcpStream::connect((host, port)).await?;
    let mut hs = vec![b'H', 0, 0, 0, 0, 0];
    pushlen(&mut hs, user.len());
    pushlen(&mut hs, pass.len());
    hs.extend_from_slice(user.as_bytes());
    hs.extend_from_slice(pass.as_bytes());
    s.write_all(&hs).await?;
    let mut reply = [0u8; 4];
    s.read_exact(&mut reply).await?;
    assert_eq!(&reply[..3], &b"H\x00\x00"[..], "handshake failed: {reply:02x?}");
    Ok(s)
}

fn query_packet(q: &str, params: &[u8]) -> Vec<u8> {
    let qw = q.len();
    let total = qw.to_string().len() + 1 + qw + params.len();
    let mut out = vec![b'S'];
    out.extend_from_slice(total.to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(qw.to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(q.as_bytes());
    out.extend_from_slice(params);
    out
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut s = open("127.0.0.1", 2003, "root", "investigate-pass-0123").await?;
    s.write_all(&query_packet("sysctl report status", &[])).await?;
    let mut kind = [0u8; 1];
    s.read_exact(&mut kind).await?;
    assert_eq!(kind[0], 0x12, "expected Empty");
    Ok(())
}
```

`Cargo.toml`:

```toml
[package]
name = "sky-min"
version = "0.0.0"
edition = "2021"

[dependencies]
tokio = { version = "=1.54.0", features = ["rt-multi-thread", "macros", "net", "io-util"] }
```

I did not compile this snippet (the task forbade cargo builds); it uses only
stable tokio 1.x APIs and the byte layout verified live above. The server
handshake, request, and response bytes were produced by the Python probe, not
by this Rust code. The `2003` in `main` is the skytable default port; for the
§5 command use `24003`.

Exact bytes for the server from §5 (root + 21-char password):

```text
48 00 00 00 00 00 34 0a 32 31 0a 72 6f 6f 74 69 6e 76 65 73 74 69 67 61 74 65 2d 70 61 73 73 2d 30 31 32 33
-> reply 48 00 00 00
```

Query (no params):

```text
53 32 33 0a 32 30 0a 73 79 73 63 74 6c 20 72 65 70 6f 72 74 20 73 74 61 74 75 73
-> reply 0x12
```

For a select, append the encoded parameters and include them in `<total>`.
The exact live example:

```text
S41\n34\nselect * from inv1.kv where id = ?\x06\x31\x0a\x6b
-> 11 32 0a 0d 31 0a 6b 05 34 32 34 32 0a
```

The reply is a self-describing `0x11` row, not length-prefixed as a whole.
For general queries the probe needs an incremental decoder for the layouts in
§1.4; reading one byte is only enough for `sysctl`/`Empty`.

Parameter encodings (client side, `STOCK src/query.rs:271-407`):
`string` = `0x06 <len>\n <bytes>`; `u64` = `0x02 <decimal>\n`; binary =
`0x05 <len>\n <bytes>`; bool = `0x01 <0|1>`; null = `0x00`; `QList` =
`0x07 <values> ]` (`0x5d`). Those are request parameter type codes, distinct
from the response value codes in §1.4.

### 4.1 Is `skytable` available as an already-downloaded crates.io dependency?

Yes. Exact locations:

```text
source: /home/ava/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/skytable-0.8.12
crate:  /home/ava/.cargo/registry/cache/index.crates.io-1949cf8c6b5b557f/skytable-0.8.12.crate
index:  /home/ava/.cargo/registry/index/index.crates.io-1949cf8c6b5b557f/.cache/sk/yt/skytable
```

`sky-derive-0.2.3` is present in the same source/cache roots. The exact
versions resolved by `PROBE/Cargo.lock` are:

```text
bb8 0.9.1, itoa 1.0.18, native-tls 0.2.18, r2d2 0.8.10,
sky-derive 0.2.3, skytable 0.8.12, tokio 1.54.0, tokio-native-tls 0.3.1
```

All of those `.crate` files and source dirs are present, and the existing
`PROBE/target/release/sky-lifecycle` was compiled against
`PATCH/src/...` (its `.d` file lists the patched source files); that build also
shows the native-tls/OpenSSL toolchain is available on this host. Therefore:

* a new crate that pins the lockfile versions can build **offline** against
  the cached stock 0.8.12 (and cached tokio 1.54.0), and
* a new crate that needs the patched behavior must use
  `skytable = { path = ".../fix/skytable-0.8.12" }` or the same
  `[patch.crates-io]` stanza; a plain `skytable = "=0.8.12"` resolves to the
  stock registry copy.

Caveat: I did not run `cargo build --offline` (forbidden by the task). The
static evidence is complete for the exact lock versions; a fresh unpinned
resolve could pick a newer index version whose `.crate` is not cached, so pin
`=1.54.0` (or copy `PROBE/Cargo.lock`) rather than `tokio = "1"`. Exact
new-crate offline resolution is therefore **UNVERIFIED**, but the required
inputs are present.

## 5. Real server via docker

Yes. The image is present locally:

```text
docker image inspect skytable/skytable:v0.8.4
  id:   sha256:823e1a390a55ddcc5579e27af58cf6f92d92a7735c5fd4fc3551673b22d08f41
  digest: skytable/skytable@sha256:823e1a390a55ddcc5579e27af58cf6f92d92a7735c5fd4fc3551673b22d08f41
  entrypoint: /usr/local/bin/start-server.sh
```

`run-lifecycle.sh` starts it with an overridden entrypoint and CLI flags
(`LIFECYCLE/run-lifecycle.sh:57-64`):

```sh
CID="$(docker run -d --rm --name "$NAME" \
  -v "$CERTDIR:/certs:ro,z" \
  -p 127.0.0.1::2003 -p 127.0.0.1::2004 \
  --entrypoint skyd skytable/skytable:v0.8.4 \
  --mode dev --auth-root-password "$PASS" \
  --endpoint tcp@0.0.0.0:2003 --endpoint tls@0.0.0.0:2004 \
  --tlscert /certs/cert.pem --tlskey /certs/key.pem \
  --tls-passphrase /certs/pass.txt)"
```

It waits for readiness and discovers the published loopback ports
(`LIFECYCLE/run-lifecycle.sh:66-69`, `:75-76`):

```sh
for _ in $(seq 1 80); do
  if docker logs "$CID" 2>&1 | grep -q "listening on"; then READY=1; break; fi
  sleep 0.25
done
...
TCP_PORT="$(docker port "$CID" 2003/tcp | head -1 | sed 's/.*://')"
TLS_PORT="$(docker port "$CID" 2004/tcp | head -1 | sed 's/.*://')"
```

So the script's "non-default port" is an **ephemeral host port** on
`127.0.0.1` (observed `32902`/`32903` in
`LIFECYCLE/logs/run.7m5uyt/environment.txt`), while the container endpoints
remain 2003/2004. That is sufficient for a disposable probe: the host port is
not 2003 and is loopback-only.

I also verified a fully non-default setup (internal port 24003, ephemeral host
port, disposable host data dir) with this exact command:

```sh
DATA="$(mktemp -d /tmp/dw-sky-data.XXXXXX)"; chmod 777 "$DATA"
CID="$(docker run -d --rm --name dw-sky-probe \
  -v "$DATA:/var/lib/skytable:z" \
  -p 127.0.0.1::24003 \
  --entrypoint skyd skytable/skytable:v0.8.4 \
  --mode dev --auth-root-password 'investigate-pass-0123' \
  --endpoint tcp@0.0.0.0:24003)"
# wait for: docker logs "$CID" | grep 'listening on'
PORT="$(docker port "$CID" 24003/tcp | head -1 | sed 's/.*://')"
```

Server log confirmed `listening on tcp@0.0.0.0:24003`; Docker published
`127.0.0.1:32909`; the handshake/query probe in §4 succeeded against
`127.0.0.1:$PORT`.

Data dir notes:

* `run-lifecycle.sh` does **not** mount a data dir; the container's data is
  disposable because of `--rm`. The image's `WORKDIR`/data root is
  `/var/lib/skytable`; `skyd` creates `data/` and `gns.db-tlog` there.
* To use an explicit host data dir, mount a fresh `mktemp -d` at
  `/var/lib/skytable`. On this SELinux host the mount needs `:z`
  (`-v "$DATA:/var/lib/skytable:z"`). Without it `skyd` failed with
  `init system failure: locking PID file; io error: Permission denied
  (os error 13)`; with `:z` it started and created `data/`.
* Container-created files are root-owned; `rm -rf "$DATA"` as the host user
  may fail. Remove via a helper container, e.g.
  `docker run --rm -v "$DATA:/x:z" --entrypoint sh skytable/skytable:v0.8.4 -c 'rm -rf /x/*'`,
  then `rmdir`.
* A fixed host port can be published with
  `-p 127.0.0.1:24003:2003` (standard Docker syntax; not separately tested
  here). The image's default entrypoint, if not overridden, runs
  `/usr/local/bin/start-server.sh`, which reads
  `/var/lib/skytable/config.yaml`, rewrites `127.0.0.1` to `0.0.0.0`, and
  generates a random root password printed in the logs. Override with
  `--entrypoint skyd` for a known password.

## 6. Verification status

**Verified live in this session**

* Image presence/id/digest, server start on `tcp@0.0.0.0:24003`, disposable
  data dir with `:z`, ephemeral loopback publish.
* 36-byte handshake for `root`/21-char password -> exactly `48 00 00 00`.
* `S23\n20\nsysctl report status` -> exactly `0x12`.
* `S41\n34\nselect * from inv1.kv where id = ?\x06\x31\x0a\x6b` -> exactly
  `11 32 0a 0d 31 0a 6b 05 34 32 34 32 0a` (Row, string `k`, u64 `4242`).
* The image entrypoint script (`start-server.sh`) and `skyd --help`/
  `--mode dev --auth-root-password --endpoint` options were inspected; the
  default entrypoint itself was not run.

**Verified from source (not re-executed)**

* All framing/size/decoder/line references in §1-§3.
* Stock missing invalidation, local decoder state, positional replies, no
  request id.
* Patch arming/disarming, error-path poisoning, no drain/close/distinction.
* bb8 `has_broken` on put_back and `is_valid` on checkout; r2d2 managers
  unchanged.

**Verified by prior artifacts, not re-run here**

* Stock `Stale(0)` and fixed `Blocked`/`Fresh` lifecycle outcomes:
  `LIFECYCLE/logs/run.7m5uyt/stock-*.log`, `fixed-*.log`; summary in
  `LIFECYCLE/RESULTS-FRAGMENT-DEEP.md:463-488`.
* Deep wire shift and error-6 partial-write observation:
  `LIFECYCLE/RESULTS-FRAGMENT-DEEP.md:145-192`, `:308-345`; raw logs
  `LIFECYCLE/logs/run.CncmjF/`, `run.0b97xn/`.

**UNVERIFIED / limits**

* No byte-level wire capture of a drop during read/write in this session; the
  stock outcomes are source-derived and backed by the prior lifecycle/deep
  logs.
* No `cargo build --offline` was run for a new crate; see §4.1.
* Server-side parsing of a truncated request concatenated with a new one was
  not isolated byte-for-byte here; prior deep run reports server error 6
  (`illegal client packet`) for the truncated-write case.
* Pool behavior under every bb8 setting was not live-tested here; the
  `has_broken`/`is_valid` call sites are verified in bb8 0.9.1 source.
* The sync (`r2d2`) path is unchanged and has no poison guard.
