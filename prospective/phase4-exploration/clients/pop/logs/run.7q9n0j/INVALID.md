# INVALID RUN — do not use

This was the first POP attempt against GreenMail 2.1.14. It is kept because raw
logs are never overwritten, but it is not a result:

- `run-ladder.sh` had a `rest_count` shell bug (`0\n0` count) that produced the
  `integer expected` warnings.
- `async_pop::login` against GreenMail hangs (chunked CAPA reply + the
  `decode_needs` bug); the manual `calibrate` path wrapped login without
  `bounded`, so `calibrate-post-1` hung, and the runner was killed.
- Only `preflight` and the two `calibrate-pre` samples completed. The preflight
  failed with "uncancelled login timed out after 2s", so no ladder exists here.

The valid run is `run.WKXVqY` (loopback POP3 fallback).
