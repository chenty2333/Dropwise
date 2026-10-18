# Follow-up for lettre/lettre#1170

Posted to [#1170](https://github.com/lettre/lettre/issues/1170#issuecomment-6252875230), with a [short reference in #1171](https://github.com/lettre/lettre/pull/1171#issuecomment-6252876602).

## Follow-up: a cancelled send can also hide a delivery failure

I found another consequence of reusing the connection: the next `send` can return `Ok` even though the server rejects that message.

With lettre 0.11.23, I reproduced this using a small in-process SMTP server that accepts the recipient but returns `554` after reading the terminating `<CRLF>.<CRLF>`. On a clean connection, the caller gets an error with code `554`. After an earlier send is cancelled, the caller can instead get an `Ok` response with code `354`.

The replies are shifted by one command:

1. The cancelled send leaves an unread `250` reply on the connection.
2. The pool's `NOOP` reads that stale reply and considers the connection healthy.
3. The next message's `MAIL FROM`, `RCPT TO`, and `DATA` each read the preceding command's `250` reply.
4. After writing the body, the client reads the `354` reply to `DATA`. `read_response` accepts it as positive, so `send` returns `Ok` without reading the actual result of the message submission.
5. The server's `554` is consumed by the following pool health check. The connection is discarded, but the rejection is never returned to the caller who sent the message.

The test warms a pool with one connection, starts a message with a 16-byte body, then drops its send future while waiting for an early command reply. It next sends the message configured for rejection, followed by a normal message. The server records which messages it accepted or rejected independently of the client result. Each command reply is delayed by 10 ms to make cancellation during the wait easier to exercise.

Across three runs with different cancellation points, every run included cases where the rejected message returned `Ok(354)` and the following normal message succeeded. The existing hang also still occurs at other cancellation points.

For comparison, I also tested rejection at `RCPT TO` with `550`. In the one-reply-shift cases, that error was read by the following command and still reached the caller; I saw no false success in that variant. The rejection after the message body is different: `send` has already returned before it reads that reply.

The fix in #1171 addresses this too. It sets `sending` before `MAIL FROM` and clears it after the transaction succeeds. If the future is dropped mid-transaction, the pool discards the connection and skips `QUIT` rather than reusing it. With that change, all three runs returned the expected `554` for the rejected message, with no false successes or hangs.

This was tested with the in-process server, not a real MTA configured to reject after the message body. The important result is that the server records a rejection while the caller receives `Ok`; it is not just an unexpected response code.
