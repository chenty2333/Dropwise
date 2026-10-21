# async-pop 1.1.3 — not started (round 3)

Status: **not started**; no ladder. This is a record of a timebox stop, not a
result and not evidence about the client.

- The lead's mid-run timebox asked for a report within ~20–25 minutes and to
  prioritise skytable depth over adding modbus/pop.
- Pre-checks that did run (no async-pop scenario): the local
  `greenmail/standalone:2.1.14` image starts with
  `-Dgreenmail.setup.test.all -Dgreenmail.hostname=0.0.0.0
  -Dgreenmail.users=popuser:pass@example.com`, POP3 on mapped 3110 and REST on
  mapped 8080 work on loopback; a message seeded with Python `smtplib` appears
  at `GET /api/user/popuser@example.com/messages` with `subject` and
  `mimeMessage`. That REST view is a viable independent observer.
- Plan not executed: `async_pop::new(tokio TcpStream)` through a 50 ms
  `dropwise::net::Proxy`, `login` in setup, target `retr(1)`, survivor
  `stat`+`retr(1)`, observer REST; `login` as target and `rset`/reconnect as
  recovery contrasts. No client API/cancellation reading was done.
