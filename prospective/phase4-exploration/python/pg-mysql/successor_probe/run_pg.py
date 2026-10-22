"""Run the successor probe against interrupted PostgreSQL transaction handles.

One trial is a fresh scenario:

    setup (proxy + tested handle + independent observer)
    -> cancel the official transaction CM at one observed suspension
    -> let the cancellation unwind
    -> run the nonce/transaction probe on the reused handle
    -> run the same probe on a newly created reference handle
    -> close the reused handle and re-read the probe table
    -> record the target table from the independent observer

The reference handle is a direct connection, not through the delay proxy.  It
has the same server, user, database and autocommit setting; the difference is
recorded.  Every trial gets a fresh two-table schema, so old and reference
writes cannot see each other.

Variants:

    original      no repair
    fix           the driver-level cancelled-entry repair already used in round 4
    reset         caller-side reset/reconnect after the cut (asyncpg reset,
                  psycopg reconnect)
    outer         a legitimate outer transaction is open when the probe runs
    session       legitimate session state (application_name, GUC, temp table,
                  prepared statement) is retained before the probe
    readonly      a legitimate read-only session makes the write probe
                  inapplicable; this is an unprobeable control, not a finding
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import sys
import time

import asyncpg

from cases.common import add_db_args, bound, close_quiet, err_text, must_ok
from cases.txphase_case import (
    CmScenario,
    _await_phased,
    install_asyncpg_enter_fix,
    install_psycopg_enter_fix,
)

from .probe import ProbePair, compare

BOUND = 5.0


def observer_dsn(args) -> str:
    return (
        f"postgresql://{args.user}:{args.password}@{args.host}:{args.port}/"
        f"{args.dbname}"
    )


class ProbeCmScenario(CmScenario):
    """CmScenario plus one bounded probe pair per trial.

    The target and setup are inherited unchanged.  The probe is deliberately
    outside the judged target/survivor observation, so it cannot make a
    differential verdict look different by adding writes to the target table.
    """

    def __init__(self, args, tag: str) -> None:
        super().__init__(args)
        self.probe_pair: ProbePair | None = None
        self.probe_tag = tag
        self.probe_marker = {"old": f"old-{tag}", "ref": f"ref-{tag}"}

    async def setup(self) -> None:
        await super().setup()
        self.probe_pair = ProbePair(observer_dsn(self.args), tag=self.probe_tag)
        await self.probe_pair.open()

    async def close(self) -> None:
        if self.probe_pair is not None:
            await close_quiet(self.probe_pair.close())
            self.probe_pair = None
        await super().close()

    async def _reference_connect(self):
        return await self._tested_connect(self.args.port)

    async def run_probe_old(self, *, allow_open_transaction: bool = False,
                            write: bool = True, probe_timeout: float = 2.0):
        assert self.probe_pair is not None
        return await self.probe_pair.run(
            "old", self.conn, self.driver,
            marker=self.probe_marker["old"],
            allow_open_transaction=allow_open_transaction,
            write=write, probe_timeout=probe_timeout,
        )

    async def run_probe_ref(self, *, probe_timeout: float = 2.0, write: bool = True):
        assert self.probe_pair is not None
        conn = await self._reference_connect()
        try:
            return await self.probe_pair.run(
                "ref", conn, self.driver,
                marker=self.probe_marker["ref"],
                probe_timeout=probe_timeout, write=write,
            )
        finally:
            with contextlib.suppress(BaseException):
                await conn.close()

    async def close_old_and_recheck(self) -> dict:
        assert self.probe_pair is not None
        out = {"close": "ok"}
        try:
            async with asyncio.timeout(BOUND):
                await self.conn.close()
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            out["close"] = err_text(exc)
        out["probe_persisted"] = await self.probe_pair.persisted_after_close(
            "old", self.probe_marker["old"]
        )
        out["target_rows_after_close"] = await bound(self._observer_rows())
        return out

    async def close_ref_and_recheck(self) -> dict:
        assert self.probe_pair is not None
        return {
            "probe_persisted": await self.probe_pair.persisted_after_close(
                "ref", self.probe_marker["ref"]
            )
        }

    async def _legit_outer_begin(self):
        tx = self.conn.transaction()
        await tx.__aenter__()
        return tx

    async def _legit_outer_rollback(self, tx) -> str:
        try:
            await tx.__aexit__(Exception, Exception("legit outer rollback"), None)
            return "ok"
        except BaseException as exc:
            return err_text(exc)

    async def probe_trial(self, k: int | None, variant: str) -> dict:
        rec: dict = {
            "driver": self.driver,
            "k": k,
            "variant": variant,
            "probe_tag": self.probe_tag,
        }
        started = time.monotonic()
        await asyncio.wait_for(self.setup(), 20)
        rec["setup"] = {"pid": self.pid, "diag": self.diag}
        outer_tx = None
        if variant == "session":
            # The session state is set before the target.  The target's body
            # cancellation rolls the transaction back; these settings remain.
            await self.conn.execute("SET application_name = 'dw-probe-session'")
            await self.conn.execute("CREATE TEMP TABLE dw_probe_tmp (x int)")
            await self.conn.execute("PREPARE dw_probe_stmt AS SELECT 1")
            rec["session"] = await self._probe_session_state()
        if k is None:
            tr, meta = await _await_phased(
                self.target(), cut_after=None, timeout=12,
                holder=self.holder, double=False,
            )
        else:
            tr, meta = await _await_phased(
                self.target(), cut_after=k, timeout=12,
                holder=self.holder, double=False,
            )
        rec["target"] = {
            "outcome": tr.outcome, "realized": tr.realized,
            "yields": tr.yields, "error": tr.error,
            "seconds": round(tr.seconds, 4),
        }
        rec["cut_phase"] = meta["cut_phase"]
        rec["second_phase"] = meta["second_phase"]
        await asyncio.sleep(0.08)  # bounded settle for the abandoned exchange
        rec["client_after_cut"] = self._client_state()
        rec["server_after_cut"] = await self._probe_server_sample()

        if variant in ("outer", "nested"):
            outer_tx = await self._legit_outer_begin()
            rec["outer_tx"] = "open"
            if variant == "nested":
                rec["inner_tx"] = await self._legit_outer_begin()

        if variant == "readonly":
            # Set the legitimate configuration *after* the target; setting it
            # before would make the target itself fail for the wrong reason.
            await self.conn.execute("SET default_transaction_read_only = on")
            rec["session"] = await self._probe_session_state()

        recovery = None
        if variant == "reset":
            self.recovery = "reconnect" if self.driver == "psycopg" else "reset"
            recovery = await bound(self._recover())
            rec["recovery"] = recovery

        pair = self.probe_pair
        assert pair is not None
        old = await self.run_probe_old(
            allow_open_transaction=(variant in ("outer", "nested"))
        )
        ref = await self.run_probe_ref()
        rec["probe_old"] = old.as_dict()
        rec["probe_ref"] = ref.as_dict()
        rec["probe_compare"] = compare(old, ref)
        rec["client_after_probe"] = self._client_state()
        rec["server_after_probe"] = await self._probe_server_sample()
        rec["probe_ref_persist"] = await self.close_ref_and_recheck()

        if outer_tx is not None:
            if variant == "nested" and rec.get("inner_tx") is not None:
                rec["inner_tx_rollback"] = await self._legit_outer_rollback(
                    rec["inner_tx"]
                )
            rec["outer_tx_rollback"] = await self._legit_outer_rollback(outer_tx)
            rec["client_after_outer_rollback"] = self._client_state()

        # Close the reused handle and re-read its probe table.  A transaction
        # that was not really committed loses the marker here (or was already
        # reported invisible).
        rec["old_close"] = await self.close_old_and_recheck()
        # The differential observation is taken only after the probe; the
        # probe's writes are in its own schema.
        rec["target_rows_final"] = rec["old_close"].get("target_rows_after_close")
        rec["seconds"] = round(time.monotonic() - started, 3)
        return rec

    async def _probe_server_sample(self) -> dict:
        try:
            return await self._server_sample()
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"error": err_text(exc)}

    async def _probe_session_state(self) -> dict:
        try:
            if self.driver == "psycopg":
                cur = await self.conn.execute(
                    "SELECT current_setting('application_name'), "
                    "current_setting('default_transaction_read_only'), "
                    "pg_backend_pid()"
                )
                row = await cur.fetchone()
                return {"app": row[0], "read_only": row[1], "pid": row[2]}
            rec = await self.conn.fetchrow(
                "SELECT current_setting('application_name') AS app, "
                "current_setting('default_transaction_read_only') AS ro, "
                "pg_backend_pid() AS pid"
            )
            return {"app": rec["app"], "read_only": rec["ro"], "pid": rec["pid"]}
        except asyncio.CancelledError:
            raise
        except BaseException as exc:
            return {"error": err_text(exc)}


async def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    add_db_args(ap)
    ap.add_argument("--driver", choices=["psycopg", "asyncpg"], required=True)
    ap.add_argument("--variant", default="original",
                    choices=["original", "fix", "reset", "outer", "nested",
                             "session", "readonly"])
    ap.add_argument("--cuts", default="", help="comma-separated k; empty = all")
    ap.set_defaults(reps=1)
    ap.add_argument("--out", default="-")
    args = ap.parse_args()

    if args.variant == "fix":
        if args.driver == "psycopg":
            install_psycopg_enter_fix()
        else:
            install_asyncpg_enter_fix()

    # Measure K first, on an isolated scenario.
    counter = ProbeCmScenario(args, tag=f"count-{args.driver}")
    try:
        await asyncio.wait_for(counter.setup(), 20)
        tr, _meta = await _await_phased(
            counter.target(), cut_after=None, timeout=12,
            holder=counter.holder, double=False,
        )
        k_total = tr.yields
        print("PROBE-COUNT " + json.dumps({
            "driver": args.driver, "variant": args.variant,
            "yields": k_total, "outcome": tr.outcome,
            "error": tr.error,
        }, sort_keys=True), flush=True)
    finally:
        await close_quiet(counter.close())
    if k_total < 1:
        print("PROBE-ERROR no target suspension", file=sys.stderr)
        return 1

    if args.cuts.strip():
        cuts = [int(x) for x in args.cuts.split(",") if x.strip()]
    else:
        cuts = list(range(1, k_total + 1))

    out = sys.stdout if args.out == "-" else open(args.out, "w", encoding="utf-8")
    failures = 0
    try:
        for rep in range(1, args.reps + 1):
            for k in cuts:
                tag = f"{args.driver}-{args.variant}-k{k}-r{rep}"
                sc = ProbeCmScenario(args, tag=tag)
                try:
                    rec = await asyncio.wait_for(
                        sc.probe_trial(k, args.variant), 30
                    )
                    rec["rep"] = rep
                except asyncio.CancelledError:
                    raise
                except BaseException as exc:
                    failures += 1
                    rec = {
                        "driver": args.driver, "k": k, "variant": args.variant,
                        "rep": rep, "trial_error": err_text(exc),
                        "trace": __import__("traceback").format_exc()[-1200:],
                    }
                rec = _sanitize(rec, sc.table)
                print("PROBE " + json.dumps(rec, default=str, sort_keys=True),
                      file=out, flush=True)
                _summary(rec)
    finally:
        if out is not sys.stdout:
            out.close()
    return 1 if failures else 0


def _summary(rec: dict) -> None:
    c = rec.get("probe_compare") or {}
    old = rec.get("probe_old") or {}
    ref = rec.get("probe_ref") or {}
    print(
        "PROBE-SUMMARY "
        f"driver={rec.get('driver')} variant={rec.get('variant')} "
        f"k={rec.get('k')} rep={rec.get('rep')} "
        f"phase={rec.get('cut_phase')} "
        f"target={((rec.get('target') or {}).get('outcome'))} "
        f"old={old.get('status')} ref={ref.get('status')} "
        f"compare={c.get('label')} "
        f"target_rows={rec.get('target_rows_final')} "
        f"seconds={rec.get('seconds')}",
        flush=True,
    )


def _sanitize(value, table: str):
    if isinstance(value, str):
        return value.replace(table, "<table>")
    if isinstance(value, dict):
        return {str(k): _sanitize(v, table) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_sanitize(v, table) for v in value]
    return value


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        sys.exit(asyncio.run(main()))
