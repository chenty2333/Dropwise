# Online guards: contracts, lifecycle, and recovery

This is the design note for the mechanisms in `prospective/online-guards`.  It
separates what the code enforces from what the protocols report and from what
is still a proposal.

## The responsibility unit

A guard is attached to the thing whose next use must be decided: one
connection in the Skytable and PostgreSQL adapters.  A future multiplexed
adapter would attach one guard per connection for framing and one ledger
entry per stream/generation for association; the same `ObjId`/`Epoch` types
are intended for that split, but the split is **not implemented** (see
"Not implemented").

Each object has an `Epoch`.  An operation arms a ticket at its first poll,
before the first byte is written, and the ticket belongs to exactly one
epoch and operation id.  A completion that arrives for a different epoch or
operation cannot discharge the ticket.  This is the part that rejects a late
completion after reuse, rather than letting it settle a new duty.

## Phases

- `Ready`: no in-flight exchange; a standing obligation may still require
  cleanup before the qualification is `Clean`.
- `Busy`: exactly one armed ticket; a second `begin` is refused.
- `Suspect`: an armed ticket was abandoned (cancelled, I/O failure, or
  protocol failure).  The object may not be used.  The `Suspect` report
  carries the observations recorded up to the abandonment: write/read byte
  counts, their state (`none`/`partial`/`complete`), operation tag, and the
  declared effect contract.
- `Recovering`: exactly one `RecoveryPermit` owner.  Dropping the permit
  without a terminal outcome returns the object to `Suspect` (or, for a
  standing cleanup, to `Ready` with the obligation intact).  A second
  cancellation therefore cannot make a partially recovered object Ready.
- `Retired`: terminal; no future use.

The set of states is deliberately not a single dirty bit.  In particular,
`Suspect` does not imply "reconnect", and `Ready` does not imply "no
obligation": the qualification check also looks at standing obligations.

## Three observations, not one

The guard records three independent facts:

1. **Effect**: the abandoned operation may have applied.  This is declared by
   the API (`EffectContract::AtomicEffectMayApply` or `MustNotApply`), not
   inferred from the protocol.  A partially written request is only completed
   when the contract permits the effect and the adapter can finish *exactly*
   the original request.  Completing a request changes the outcome; it is an
   authority decision, not a protocol repair.
2. **Reuse qualification**: is the wire at a frame boundary and has the
   protocol's reset/cleanup been observed?  Skytable: a complete reply frame
   was consumed and no extra bytes remain.  PostgreSQL: `ReadyForQuery` with
   status `I`.
3. **Cleanup authority**: is there a standing obligation that another owner
   must discharge?  PostgreSQL `ReadyForQuery` with `T` or `E` is an open
   transaction; the object is returned for a `rollback` cleanup, not made
   Ready.  A `RecoveryKind::Cleanup` permit can clear the obligation only when
   the adapter mints a witness (for PostgreSQL, after observing `Z` with `I`).

A timeout, an unparsed byte, or a missing observation is `Unresolved` or
`Failed`, never `Recovered`.

## Ownership and liveness

The guard's phase and epoch answer "what did the protocol do?"; they do not
answer "is anyone still responsible for this object?".  A clean wire state and
an abandoned transaction can look identical, and a missing `Suspect` report is
not proof that no owner exists.

`OwnerClaim` is an RAII claim on one guard.  Its existence is positive,
in-process evidence that an owner is live; dropping it is the positive evidence
that authority was relinquished.  Claims may nest (an outer transaction and an
inner savepoint), and releasing one does not release the others.  A handoff is
all-or-nothing: it replaces the cohort and starts a new owner generation, so
older witnesses cannot authorise, block or release the new owner.  A leaked
claim is not inferred dead; it blocks recovery/reuse until the pool's ownership
grace expires and the object is retired.  A live owner can recover its own
object through `OwnerClaim::claim_recovery`; an unowned watchdog is refused
with `Refusal::Owned`.

The pool does not claim a root owner for a checked-out lease: the lease itself
is the borrower, and a root claim would mask the protocol's `Clean`
qualification.  Instead, a returned session with a live sub-owner is parked in
a deferred queue; the next acquisition reaps it if the owner released, or
retires it after the grace.  This is deliberately a local, in-process witness.
It does not observe an owner in another process or a task killed without
unwinding, and it is not a kernel or runtime hook.  Those cases stay
conservative: the object is not reused without an explicit protocol witness.

## Pool lifecycle

- The return path is unconditional: a dropped lease returns the session to
  the pool.  The guard decides whether the session is clean, needs cleanup,
  or is suspect.  The return path does not validate by probing; probing on
  return would consume a reply that belongs to the abandoned operation.
- Checkout only hands out a session whose guard is `Clean`; a session with a
  standing obligation is routed to recovery before it can be checked out.
- Suspect sessions go to a bounded quarantine queue with a deadline and an
  attempt limit.  The recovery worker owns the session for the duration of
  the attempt (and holds the exclusive permit).
- A recovery attempt can be cancelled by shutdown or by the deadline.  The
  adapter's `out`/`out_off`/`inbuf` and its per-attempt "frame consumed"/"ready
  observed" flags let a retry in the same process avoid sending a request
  twice.  The flags are in memory: a process crash loses them, and a retry
  re-reads or retires.  That is safe only because the object is still suspect;
  it is not a durable transaction log.
- The pool retires on a failed recovery, counts every failed attempt, and
  creates a replacement connection only through ordinary acquire.  Shutdown
  drains the queue itself and waits up to its grace budget for the worker;
  `forced` reports an expired wait, not a successful join. Worker publication
  checks closure under the same state lock as admission.

## Protocol plans

### Skytable (positional, no request ids)

- `discard_local`: only when `out_off == 0`, `inbuf` is empty, and the
  abandonment cause is not an I/O failure.  No bytes left the process and
  none were read; there is no exchange to drain.  This is a byte-count fact,
  not a timing guess, and a dead socket is not certified by it.
- `drain` / `drain_slow`: the request is fully on the wire; consume exactly
  one complete reply frame and require the input buffer to be empty
  afterwards.  Extra bytes mean the stream is not at the boundary the
  adapter can certify: fail, do not clear them.
- `finish_write_drain[_slow]`: only with `AtomicEffectMayApply`; finish the
  original request from `out_off`, then drain its reply.
- `retire`: close.  Never used to claim the operation did not apply.

The patched stock client (`prospective/phase4-exploration/clients/skytable/fix`)
uses one `poisoned` boolean armed before the first write and cleared only on a
complete reply. The B1 baseline here models that strategy in `RawSky`; it
does not execute the patched upstream crate: it prevents reuse, but cannot
distinguish "nothing was written" from "a reply is pending", so it reconnects
in both cases (measured below).

### PostgreSQL (typed messages, status byte, recovery command)

- `drain_reset`: the request is fully written; read messages until
  `ReadyForQuery`, discarding result rows and errors.  If the status is `T`
  or `E`, issue `ROLLBACK` and wait for `Z` with `I`.  Only then mint the
  witness.  `ROLLBACK` is the recovery command; `DISCARD ALL` inside a
  transaction fails with `25001` and leaves `E`, so it is not a rollback
  substitute.
- `rollback`: for a standing obligation with no pending reply (a completed
  `BEGIN` returned to the pool).  If a complete request's reply is still
  unread, `rollback` refuses; the plan must be `drain_reset`, because the first
  `Z` belongs to the abandoned exchange.  Send `ROLLBACK`, read to `Z` `I`,
  clear the obligation through the cleanup permit.
- `discard`: only from `I`; a session reset, not a transaction reset.
- `finish_drain_reset`: with `AtomicEffectMayApply`, finish the interrupted
  simple-query frame, then drain/reset.

The status byte is authoritative, but the local mirror can be stale until a
reply is read.  A cancelled `COMMIT` whose reply was never read is not turned
into success: the adapter either observes the server's `CommandComplete`/
`ReadyForQuery` (and then knows) or retires without a claim.

## Implemented vs. model-only vs. proposal

Implemented and exercised against real servers: guard ledger, tickets,
discharge/witness checks, quarantine, recovery worker, retry, shutdown, the
Skytable and PostgreSQL adapters, the B0/B1/B2/B3/B3f labs, the rule-lab
action harness.  Ownership claims and the pool's deferred-return path are
implemented and covered by unit/integration tests with a fake session; they are
not yet created by the protocol adapters themselves.

Model-only: the claim that a per-stream guard composes with a connection
guard under multiplexing; the exact cost model for choosing between
`drain_slow` and reconnect on a WAN; the rule learner's labels beyond the
states actually executed.

Proposal: a multiplexed adapter with per-stream generations and a
connection-level framing guard; a kernel/runtime-assisted hand-off witness for
owners in another process or after a non-unwinding kill; a policy that consumes
a calibrated cost model instead of a fixed timeout.
