"""Offline-discovery harness: synchronous interruption at psycopg CM boundaries.

Modes
-----
sweep     real timer signals over a range of offsets; state + successor effect
phases    one phase/injection/mode matrix on the original library
fix       the same matrix with the candidate repair installed
pool      sync pool release and the next checkout
contrast  legitimate nested savepoint vs retry inside an abandoned transaction
points    verify the source program points this harness pins

Every trial uses a fresh table, a fresh tested connection and an independent
observer connection.  Raw JSON lines are written to stdout; the runner keeps
them in a fresh logs directory.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import re
import sys
import time
import traceback
from dataclasses import dataclass
from pathlib import Path

import psycopg
from psycopg_pool import ConnectionPool

from . import fix, support
from .support import (EventLog, FrameProxy, SignalProbe, SigIntProbe, TraceRaise,
                      classify, client_state, connect, dsn, server_pid,
                      server_state, setup_table, table_rows, rows_set)

HERE = Path(__file__).resolve().parent
SITE = Path(psycopg.__file__).resolve().parent

TARGET, SURVIVOR, RETRY, VICTIM = "target", "survivor", "retry", "victim"
REUSE, PLAIN = "reuse", "plain"

# file, function, line, exact stripped source text
POINTS = {
    "entry_pre_send": ("transaction.py", "_enter_gen", 106,
                       "yield from self._conn._exec_command(command)"),
    "entry_after_send": ("_connection_base.py", "_exec_command", 478,
                         "results: list[PGresult] = (yield from generators.execute(self.pgconn))"),
    "entry_push": ("transaction.py", "_push_savepoint", 215,
                   "self._outer_transaction = self.pgconn.transaction_status == IDLE"),
    "exit_pre_commit": ("transaction.py", "_commit_gen", 140,
                        "yield from self._conn._exec_command(command)"),
    "wait_loop": ("connection.py", "wait", 495, "return waiting.wait("),
}


def check_points() -> list[str]:
    errors = []
    for name, (fname, func, line, text) in POINTS.items():
        path = SITE / fname
        try:
            lines = path.read_text().splitlines()
            actual = lines[line - 1].strip()
        except (OSError, IndexError) as exc:
            errors.append(f"{name}: cannot read {path}:{line}: {exc}")
            continue
        if actual != text:
            errors.append(f"{name}: {fname}:{line} is {actual!r}, expected {text!r}")
    return errors


def insert(cur, table: str, key: str, value: int):
    cur.execute(f"INSERT INTO {table} (k, v) VALUES (%s, %s)", (key, value))


def err(exc: BaseException) -> str:
    return f"{type(exc).__name__}: {exc}"


class Env:
    def __init__(self, pg_port: int, *, c2s_delay=0.0, s2c_delay=0.0, log_dir: str):
        self.pg_port = pg_port
        self.observer = connect(pg_port)
        frame_log = os.path.join(log_dir,
                                 f"frames-{os.getpid()}-{id(self)}.jsonl")
        self.proxy = FrameProxy(pg_port, c2s_delay=c2s_delay, s2c_delay=s2c_delay,
                                log_path=frame_log)
        self.proxy.start()
        self._n = 0

    def new_table(self) -> str:
        self._n += 1
        table = f"dw_od{os.getpid()}_{self._n}"
        setup_table(self.observer, table)
        return table

    def close(self):
        with contextlib.suppress(BaseException):
            self.observer.close()
        self.proxy.close()


def _client(port: int):
    return connect(port)


def make_injector(spec, probe):
    """Return a context manager that arms the requested interruption."""
    if spec.inject == "none":
        return contextlib.nullcontext()
    if spec.inject == "timer":
        return _TimerArmer(probe, spec.offset_s)
    if spec.inject == "sigint":
        return _SigIntArmer(probe, spec.offset_s)
    if spec.inject == "trace":
        return TraceRaise(spec.point_file, spec.point_func, spec.point_line)
    raise SystemExit(f"unknown injector {spec.inject}")


class _TimerArmer:
    def __init__(self, probe, offset):
        self.probe, self.offset = probe, offset

    def __enter__(self):
        self.probe.arm_timer(self.offset)
        return self

    def __exit__(self, *exc):
        self.probe.disarm()


class _SigIntArmer:
    def __init__(self, probe, offset):
        self.probe, self.offset = probe, offset

    def __enter__(self):
        self.probe.arm(self.offset)
        return self

    def __exit__(self, *exc):
        self.probe.disarm()


@dataclass
class Spec:
    phase: str
    inject: str = "none"
    offset_s: float = 0.1
    point_file: str = ""
    point_func: str = ""
    point_line: int = 0
    skip_target: bool = False


def run_target(conn, table, spec: Spec, probe, events: EventLog) -> dict:
    """Enter/execute/exit the target transaction under the requested injection."""
    out = {"outcome": "completed", "entered": False}
    injector = make_injector(spec, probe)
    try:
        if spec.phase == "entry":
            if spec.skip_target:
                out["outcome"] = "skipped"
                return out
            with injector:
                with conn.transaction():
                    out["entered"] = True
                    events.add("cm_entered")
                    insert(conn, table, TARGET, 1)
                    events.add("target_written")
            events.add("target_committed")
        elif spec.phase == "body":
            cm = conn.transaction()
            with cm:
                out["entered"] = True
                events.add("cm_entered")
                if spec.skip_target:
                    out["outcome"] = "skipped"
                else:
                    with injector:
                        conn.execute("SELECT pg_sleep(0.30)")
                    insert(conn, table, TARGET, 1)
                    events.add("target_written")
            events.add("cm_exited")
        elif spec.phase == "exit":
            cm = conn.transaction()
            cm.__enter__()
            out["entered"] = True
            events.add("cm_entered")
            if spec.skip_target:
                out["outcome"] = "skipped"
                cm.__exit__(None, None, None)
                return out
            insert(conn, table, TARGET, 1)
            events.add("target_written")
            with injector:
                cm.__exit__(None, None, None)
            events.add("cm_exited")
        else:
            raise SystemExit(f"unknown phase {spec.phase}")
    except BaseException as exc:
        out["outcome"] = type(exc).__name__
        out["error"] = err(exc)
        events.add("interrupted", exc=err(exc))
    finally:
        with contextlib.suppress(BaseException):
            injector.__exit__(None, None, None)
        if isinstance(injector, TraceRaise):
            out["landing"] = injector.landing.text if injector.fired else None
        else:
            out["landing"] = probe.landing.text or None
    return out


def landing_of(spec, probe, injector) -> str | None:
    if spec.inject == "trace":
        return getattr(injector, "landing", None).text if getattr(injector, "fired", False) else None
    return probe.landing.text or None


def run_successor(conn, table, mode: str, events: EventLog) -> dict:
    """One later use of the same handle: retry / survivor / victim."""
    res = {"result": "none"}
    key = {"retry": RETRY, "survivor": SURVIVOR, "victim": VICTIM}.get(mode)
    if key is None:
        return res
    events.add(f"{mode}_start")
    try:
        with conn.transaction():
            insert(conn, table, key, {"retry": 5, "survivor": 7, "victim": 9}[key])
        res["result"] = "ok"
        events.add(f"{mode}_ok")
    except BaseException as exc:
        res["result"] = err(exc)
        events.add(f"{mode}_error", exc=err(exc))
    res["client"] = client_state(conn)
    return res


def run_trial(pg_port: int, *, env: Env, spec: Spec, mode: str,
              proxy_port: int) -> dict:
    table = env.new_table()
    conn = _client(proxy_port)
    events = EventLog()
    probe = SigIntProbe() if spec.inject == "sigint" else SignalProbe()
    trial = {"spec": spec.__dict__, "mode": mode, "table": table,
             "events": events}
    try:
        trial["pid"] = server_pid(conn)
        events.add("connected", pid=server_pid(conn))
        target = run_target(conn, table, spec, probe, events)
        trial["target"] = target
        trial["client_after"] = client_state(conn)
        trial["server_after"] = (None if trial["client_after"].get("closed")
                                 else server_state(env.observer, trial["pid"]))
        trial["kind"] = classify(trial["client_after"], trial["server_after"])
        events.add("state_after", client=trial["client_after"],
                   server=trial["server_after"])

        if mode in ("retry", "survivor"):
            success = run_successor(conn, table, mode, events)
            trial[f"{mode}"] = success
            trial["client_after_successor"] = client_state(conn)
            trial["server_after_successor"] = (None if trial["client_after_successor"].get("closed")
                                               else server_state(env.observer, trial["pid"]))
        elif mode == "recovery":
            steps = []
            for name in ("rollback", "raw_rollback"):
                try:
                    if name == "rollback":
                        conn.rollback()
                    else:
                        conn.execute("ROLLBACK")
                    steps.append({"step": name, "result": "ok",
                                  "client": client_state(conn)})
                except BaseException as exc:
                    steps.append({"step": name, "result": err(exc),
                                  "client": client_state(conn)})
            # does the handle now satisfy the next legitimate use?
            try:
                with conn.transaction():
                    insert(conn, table, REUSE, 11)
                steps.append({"step": "reuse_transaction", "result": "ok",
                              "client": client_state(conn)})
            except BaseException as exc:
                steps.append({"step": "reuse_transaction", "result": err(exc),
                              "client": client_state(conn)})
            try:
                insert(conn, table, PLAIN, 13)
                steps.append({"step": "plain_insert", "result": "ok",
                              "client": client_state(conn)})
            except BaseException as exc:
                steps.append({"step": "plain_insert", "result": err(exc),
                              "client": client_state(conn)})
            steps.append({"step": "close", "client_before": client_state(conn)})
            with contextlib.suppress(BaseException):
                conn.close()
            try:
                conn2 = _client(proxy_port)
                steps.append({"step": "reconnect", "client": client_state(conn2),
                              "pid": server_pid(conn2)})
                with contextlib.suppress(BaseException):
                    conn2.close()
            except BaseException as exc:
                steps.append({"step": "reconnect", "result": err(exc)})
            trial["recovery"] = steps
        trial["rows"] = table_rows(env.observer, table)
        trial["rows_set"] = sorted(rows_set(trial["rows"]))
        events.add("rows", rows=trial["rows"])
    except BaseException as exc:
        trial["trial_error"] = err(exc)
        trial["trial_tb"] = traceback.format_exc().splitlines()[-6:]
    finally:
        with contextlib.suppress(BaseException):
            conn.close()
        trial["events"] = events.as_list()
    return trial


# --------------------------------------------------------------------------
# judging
# --------------------------------------------------------------------------

def judged_effect(mode: str, trial: dict, table: str) -> tuple[str, bool, str]:
    """Return (field, value, detail) for the caller-visible effect."""
    rows = rows_set(trial.get("rows", []))
    if mode == "retry":
        res = trial.get("retry", {}).get("result", "none")
        return "retry_visible", f"{RETRY}=5" in rows, f"retry_result={res}"
    if mode == "survivor":
        res = trial.get("survivor", {}).get("result", "none")
        return "survivor_visible", f"{SURVIVOR}=7" in rows, f"survivor_result={res}"
    if mode == "victim":
        res = trial.get("victim", {}).get("result", "none")
        return "victim_visible", f"{VICTIM}=9" in rows, f"victim_result={res}"
    return "state", True, ""


def verdict(mode: str, trial: dict, table: str, calibrated: bool) -> str:
    if mode in ("cancel", "recovery"):
        return "state-only"
    if trial.get("target", {}).get("outcome") == "completed" and mode in ("retry", "survivor"):
        return "no-interrupt"
    if trial.get("trial_error"):
        return "harness-error"
    if not calibrated:
        return "uncalibrated"
    field, value, detail = judged_effect(mode, trial, table)
    if value:
        return "post"
    # distinguish a loud failure from a silent wrong success
    if mode in ("retry", "survivor"):
        res = trial.get(mode, {}).get("result", "none")
        if res == "ok":
            return "silent-loss"
        return "explicit-failure"
    if mode == "victim":
        return "victim-failure"
    return "state-only"


# --------------------------------------------------------------------------
# commands
# --------------------------------------------------------------------------

def cmd_points(args) -> int:
    errors = check_points()
    for e in errors:
        print("POINT-ERROR", e)
    print(f"points={len(POINTS)} errors={len(errors)}")
    return 1 if errors else 0


def cmd_sweep(args) -> int:
    env = Env(args.port, s2c_delay=args.delay, log_dir=args.log_dir)
    offsets = [float(x) / 1e6 for x in args.offsets_us.split(",")]
    counts: dict[str, int] = {}
    by_offset: dict[float, dict] = {}
    try:
        for off in offsets:
            for rep in range(args.reps):
                spec = Spec("entry", inject="timer", offset_s=off)
                # one global probe instance is simpler; run_target builds one
                t = run_trial(args.port, env=env, spec=spec, mode=args.mode,
                              proxy_port=env.proxy.port)
                cls = t.get("kind", "?")
                target = t.get("target", {})
                landed = target.get("landing") or None
                effect = judged_effect(args.mode, t, t["table"])[1] if args.mode != "cancel" else None
                label = cls if target.get("outcome") == "KeyboardInterrupt" else \
                    ("completed" if target.get("outcome") == "completed" else "unrealized")
                record = {"offset_us": round(off * 1e6, 1), "rep": rep,
                          "outcome": target.get("outcome"), "class": cls,
                          "effect": effect, "landing": landed,
                          "rows": t.get("rows_set"), "mode": args.mode,
                          "target": target, "client": t.get("client_after"),
                          "server": t.get("server_after"),
                          "successor": t.get(args.mode) if args.mode != "cancel" else None,
                          "landing_error": t.get("trial_error")}
                print(json.dumps(record, default=str), flush=True)
                counts[label] = counts.get(label, 0) + 1
                slot = by_offset.setdefault(round(off * 1e6, 1),
                                            {"classes": {}, "effects": {}})
                slot["classes"][label] = slot["classes"].get(label, 0) + 1
                if effect is not None:
                    slot["effects"][str(effect)] = slot["effects"].get(str(effect), 0) + 1
        print("SUMMARY", json.dumps({"counts": counts, "by_offset": by_offset}))
    finally:
        env.close()
    return 0


def _run_matrix(args, *, apply_fix: bool) -> int:
    if apply_fix:
        fix.install()
    env = Env(args.port, s2c_delay=0.3, log_dir=args.log_dir)  # wait windows slow
    # trace arms do not need the delay, but a slow proxy also keeps replies
    # pending; it makes every trial ~1s, so traces use a second env.
    env_fast = Env(args.port, s2c_delay=0.01, log_dir=args.log_dir)
    summary: dict[str, dict] = {}
    calib_ok: dict[str, bool] = {}
    try:
        phases = [
            ("entry_wait", Spec("entry", inject="timer", offset_s=0.10), env),
            ("entry_pre_send", Spec("entry", inject="trace",
                                    point_file="transaction.py",
                                    point_func="_enter_gen",
                                    point_line=106), env_fast),
            ("entry_after_send", Spec("entry", inject="trace",
                                      point_file="_connection_base.py",
                                      point_func="_exec_command",
                                      point_line=478), env_fast),
            ("body", Spec("body", inject="timer", offset_s=0.05), env_fast),
            ("exit_pre_commit", Spec("exit", inject="trace",
                                     point_file="transaction.py",
                                     point_func="_commit_gen",
                                     point_line=140), env_fast),
            ("exit_wait", Spec("exit", inject="timer", offset_s=0.10), env),
        ]
        if args.phases:
            wanted = set(args.phases.split(","))
            phases = [p for p in phases if p[0] in wanted]
        for phase_name, spec, phase_env in phases:
            for mode in ("cancel", "retry", "survivor", "recovery"):
                if mode == "cancel" and args.no_cancel:
                    continue
                key = f"{phase_name}/{mode}"
                key_counts: dict[str, int] = {}
                calib_ok.setdefault(key, True)
                for rep in range(args.reps):
                    # calibrations: no injection (post) and skip-target (pre)
                    if rep < args.calib:
                        cspec = Spec(spec.phase, inject="none",
                                     skip_target=(rep % 2 == 1))
                        c = run_trial(args.port, env=phase_env, spec=cspec,
                                      mode=mode, proxy_port=phase_env.proxy.port)
                        field, val, detail = judged_effect(mode, c, c["table"])
                        print(json.dumps({"kind": "calibration", "key": key,
                                          "skip": cspec.skip_target, "field": field,
                                          "value": val, "detail": detail,
                                          "rows": c.get("rows_set")}), flush=True)
                        if not val:
                            calib_ok[key] = False
                        continue
                    t = run_trial(args.port, env=phase_env, spec=spec, mode=mode,
                                  proxy_port=phase_env.proxy.port)
                    v = verdict(mode, t, t["table"], calibrated=calib_ok[key])
                    t["verdict"] = v
                    field, val, detail = judged_effect(mode, t, t["table"])
                    t["judged"] = {"field": field, "value": val, "detail": detail}
                    print(json.dumps({"kind": "trial", "key": key,
                                      "phase": phase_name, "mode": mode,
                                      "target": t.get("target"),
                                      "kind_state": t.get("kind"),
                                      "client": t.get("client_after"),
                                      "server": t.get("server_after"),
                                      "judged": t["judged"], "verdict": v,
                                      "rows": t.get("rows_set"),
                                      "successor": t.get(mode) if mode in
                                      ("retry", "survivor") else None,
                                      "recovery": t.get("recovery"),
                                      "trial_error": t.get("trial_error")},
                                     default=str), flush=True)
                    key_counts[v] = key_counts.get(v, 0) + 1
                summary[key] = key_counts
    finally:
        env.close()
        env_fast.close()
        if apply_fix:
            fix.uninstall()
    print("SUMMARY", json.dumps(summary))
    return 0


def cmd_phases(args) -> int:
    return _run_matrix(args, apply_fix=False)


def cmd_fix(args) -> int:
    return _run_matrix(args, apply_fix=True)


def cmd_pool(args) -> int:
    if args.fix:
        fix.install()
    env = Env(args.port, s2c_delay=0.3, log_dir=args.log_dir)
    env_fast = Env(args.port, s2c_delay=0.01, log_dir=args.log_dir)
    summary = {}
    try:
        cases = [
            ("entry_wait", Spec("entry", inject="timer", offset_s=0.10), env),
            ("entry_pre_send", Spec("entry", inject="trace",
                                    point_file="transaction.py",
                                    point_func="_enter_gen",
                                    point_line=106), env_fast),
        ]
        for name, spec, e in cases:
            for rep in range(args.reps):
                table = e.new_table()
                pool = ConnectionPool(dsn(e.proxy.port), min_size=1, max_size=1,
                                      open=True)
                rec = {"kind": "pool", "case": name, "rep": rep}
                first_pid = None
                try:
                    try:
                        with pool.connection() as conn:
                            first_pid = server_pid(conn)
                            rec["first_pid"] = first_pid
                            t = run_target(conn, table, spec, SignalProbe(), EventLog())
                            rec["target"] = t
                            rec["client_in"] = client_state(conn)
                    except BaseException as exc:
                        rec["first_error"] = err(exc)
                    rec["pool_after_first"] = {k: v for k, v in
                                               pool.get_stats().items()
                                               if k in ("pool_size", "pool_available",
                                                        "connections_num")}
                    try:
                        with pool.connection() as conn2:
                            rec["second_pid"] = server_pid(conn2)
                            rec["same_backend"] = rec.get("first_pid") == rec["second_pid"]
                            rec["second_before"] = client_state(conn2)
                            succ = run_successor(conn2, table, "victim", EventLog())
                            rec["victim"] = succ
                            rec["second_after"] = client_state(conn2)
                            try:
                                insert(conn2, table, PLAIN, 13)
                                rec["second_plain"] = "ok"
                            except BaseException as exc:
                                rec["second_plain"] = err(exc)
                            rec["second_before_exit"] = client_state(conn2)
                    except BaseException as exc:
                        rec["second_outer"] = err(exc)
                    rec["rows"] = table_rows(e.observer, table)
                    rec["victim_visible"] = f"{VICTIM}=9" in rows_set(rec["rows"])
                    rec["verdict"] = ("post" if rec["victim_visible"]
                                      else "silent-loss" if rec.get("victim", {}).get("result") == "ok"
                                      else "explicit-failure")
                finally:
                    with contextlib.suppress(BaseException):
                        pool.close()
                print(json.dumps(rec, default=str), flush=True)
                s = summary.setdefault(name, {})
                s[rec["verdict"]] = s.get(rec["verdict"], 0) + 1
    finally:
        env.close()
        env_fast.close()
        if args.fix:
            fix.uninstall()
    print("SUMMARY", json.dumps(summary))
    return 0


FIELD_PHASES = {
    "entry_wait": ("timer", "entry", 0.10, 0.30),
    "entry_pre_send": ("trace", "entry", 106,
                       ("transaction.py", "_enter_gen"), 0.01),
    "exit_pre_commit": ("trace", "exit", 140,
                        ("transaction.py", "_commit_gen"), 0.01),
}


def cmd_fields(args) -> int:
    """Re-judge the sync trials with the same field-contract module as the
    async engine.  This is the method migration to a second, non-asyncio
    target; the field contract is declared per observable effect/state."""
    import sys
    retro = Path(__file__).resolve().parents[4] / "research" / "python-retro"
    if str(retro) not in sys.path:
        sys.path.insert(0, str(retro))
    from dropwise_py.engine import Expect
    from dropwise_py.fields import (Field, FieldContract, FieldKind,
                                    judge_trial)

    contract = FieldContract((
        Field("target.visible", FieldKind.EFFECT, Expect.ATOMIC),
        Field("successor.visible", FieldKind.STATE),
        Field("client.clean", FieldKind.STATE),
        Field("server.clean", FieldKind.STATE),
    ))
    summary = {}
    envs = {}
    try:
        for phase in args.phases.split(","):
            kind = FIELD_PHASES[phase]
            if kind[0] == "timer":
                _, _, offset, delay = kind
                if (delay) not in envs:
                    envs[delay] = Env(args.port, s2c_delay=delay,
                                      log_dir=args.log_dir)
                env = envs[delay]
                spec = Spec("entry", inject="timer", offset_s=offset)
            else:
                _, ph, line, (file, func), delay = kind
                if delay not in envs:
                    envs[delay] = Env(args.port, s2c_delay=delay,
                                      log_dir=args.log_dir)
                env = envs[delay]
                spec = Spec(ph, inject="trace", point_file=file,
                            point_func=func, point_line=line)
            for mode in ("retry", "survivor"):
                def obs(t):
                    rows = rows_set(t.get("rows", []))
                    client = t.get("client_after") or {}
                    server = t.get("server_after")
                    key = SURVIVOR if mode == "survivor" else RETRY
                    return {
                        "target.visible": f"{TARGET}=1" in rows,
                        "successor.visible": f"{key}={7 if key == SURVIVOR else 5}" in rows,
                        "client.clean": (client.get("tx") == "IDLE"
                                         and client.get("num_transactions") == 0
                                         and not client.get("closed")),
                        "server.clean": (server is None or
                                         (server.get("state") == "idle"
                                          and server.get("in_xact") == "false")),
                    }
                pre, post = [], []
                for _ in range(2):
                    c = run_trial(args.port, env=env,
                                  spec=Spec(spec.phase, inject="none",
                                            skip_target=True),
                                  mode=mode, proxy_port=env.proxy.port)
                    pre.append(obs(c))
                for _ in range(2):
                    c = run_trial(args.port, env=env,
                                  spec=Spec(spec.phase, inject="none"),
                                  mode=mode, proxy_port=env.proxy.port)
                    post.append(obs(c))
                for rep in range(args.reps):
                    t = run_trial(args.port, env=env, spec=spec, mode=mode,
                                  proxy_port=env.proxy.port)
                    judgment = judge_trial(contract, pre, post, obs(t))
                    rec = {"kind": "fields", "phase": phase, "mode": mode,
                           "rep": rep, "target": t.get("target"),
                           "status": judgment.status.value,
                           "coverage": judgment.coverage_complete,
                           "fields": {f.name: f.status.value
                                      for f in judgment.fields},
                           "rows": t.get("rows_set")}
                    print(json.dumps(rec, default=str), flush=True)
                    key = f"{phase}/{mode}"
                    slot = summary.setdefault(key, {})
                    slot[judgment.status.value] = slot.get(judgment.status.value, 0) + 1
    finally:
        for e in envs.values():
            e.close()
    print("SUMMARY", json.dumps(summary))
    return 0


def cmd_sigint(args) -> int:
    """Real SIGINT from a helper thread, same wait window as entry_wait."""
    env = Env(args.port, s2c_delay=0.3, log_dir=args.log_dir)
    summary = {}
    try:
        for rep in range(args.reps):
            spec = Spec("entry", inject="sigint", offset_s=0.10)
            t = run_trial(args.port, env=env, spec=spec, mode="survivor",
                          proxy_port=env.proxy.port)
            rec = {"kind": "sigint", "rep": rep,
                   "target": t.get("target"), "kind_state": t.get("kind"),
                   "client": t.get("client_after"), "server": t.get("server_after"),
                   "successor": t.get("survivor"),
                   "rows": t.get("rows_set"),
                   "survivor_visible": f"{SURVIVOR}=7" in rows_set(t.get("rows", []))}
            print(json.dumps(rec, default=str), flush=True)
            key = f"{t.get('kind')}/{rec['survivor_visible']}"
            summary[key] = summary.get(key, 0) + 1
    finally:
        env.close()
    print("SUMMARY", json.dumps(summary))
    return 0


def cmd_contrast(args) -> int:
    """Legitimate nested savepoint vs retry in an abandoned transaction."""
    env = Env(args.port, s2c_delay=0.3, log_dir=args.log_dir)
    out = {}
    try:
        # (a) legitimate outer transaction with a live owner.
        table = env.new_table()
        conn = _client(env.proxy.port)
        try:
            with conn.transaction():
                with conn.transaction():
                    insert(conn, table, "inner", 1)
                out["legit_inner_visible_inside"] = (
                    f"inner=1" in rows_set(table_rows(env.observer, table)))
            out["legit_inner_visible_after_outer"] = (
                f"inner=1" in rows_set(table_rows(env.observer, table)))
            out["legit_client"] = client_state(conn)
        finally:
            with contextlib.suppress(BaseException):
                conn.close()

        # (b) interrupted entry, then a retry through the official CM: same
        # client-visible success and same invisibility as the legitimate inner.
        table = env.new_table()
        conn = _client(env.proxy.port)
        probe = SignalProbe()
        try:
            try:
                probe.arm_timer(0.10)
                with conn.transaction():
                    insert(conn, table, TARGET, 1)
            except KeyboardInterrupt:
                pass
            finally:
                probe.disarm()
            out["abandoned_landing"] = probe.landing.text or None
            out["abandoned_client"] = client_state(conn)
            try:
                with conn.transaction():
                    insert(conn, table, RETRY, 5)
                out["abandoned_retry"] = "ok"
            except BaseException as exc:
                out["abandoned_retry"] = err(exc)
            out["abandoned_visible_after_retry"] = (
                f"retry=5" in rows_set(table_rows(env.observer, table)))
            out["abandoned_server"] = server_state(env.observer, server_pid(conn))
            out["abandoned_client_after_retry"] = client_state(conn)
        finally:
            with contextlib.suppress(BaseException):
                conn.close()
        # (c) a legitimate outer transaction that is dropped (never committed)
        # before exit: indistinguishable from (b) to the observer at this point.
        table = env.new_table()
        conn = _client(env.proxy.port)
        try:
            cm = conn.transaction()
            cm.__enter__()
            insert(conn, table, "inner", 1)
            out["dropped_client"] = client_state(conn)
            out["dropped_server"] = server_state(env.observer, server_pid(conn))
        finally:
            with contextlib.suppress(BaseException):
                conn.close()
        out["dropped_inner_visible"] = (
            "inner=1" in rows_set(table_rows(env.observer, table)))
        wire = [
            {"dir": f["dir"], "type": f["type"],
             "text": f.get("text"), "tag": f.get("tag"), "status": f.get("status")}
            for f in env.proxy.frames()
            if f.get("type") in ("Q", "P", "C", "Z", "CancelRequest")
        ]
        with open(os.path.join(args.log_dir, "contrast-frames.jsonl"), "w") as fh:
            for rec in wire:
                fh.write(json.dumps(rec) + "\n")
        out["wire_count"] = len(wire)
        out["wire_sql"] = [(r["type"], r["text"]) for r in wire
                           if r["type"] in ("Q", "P", "CancelRequest")]
        print(json.dumps(out, default=str))
    finally:
        env.close()
    return 0


def main(argv=None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, required=True, help="direct PostgreSQL port")
    p.add_argument("--log-dir", default=".")
    sub = p.add_subparsers(dest="cmd", required=True)

    sp = sub.add_parser("points")
    sp.set_defaults(func=cmd_points)

    sp = sub.add_parser("sweep")
    sp.add_argument("--offsets-us", default="0,5,10,15,20,30,40,60,80,120,200,500")
    sp.add_argument("--reps", type=int, default=6)
    sp.add_argument("--delay", type=float, default=0.01)
    sp.add_argument("--mode", default="survivor",
                    choices=["cancel", "retry", "survivor"])
    sp.set_defaults(func=cmd_sweep)

    sp = sub.add_parser("phases")
    sp.add_argument("--reps", type=int, default=5)
    sp.add_argument("--calib", type=int, default=2)
    sp.add_argument("--phases", default=None)
    sp.add_argument("--no-cancel", action="store_true")
    sp.set_defaults(func=cmd_phases)

    sp = sub.add_parser("fix")
    sp.add_argument("--reps", type=int, default=5)
    sp.add_argument("--calib", type=int, default=2)
    sp.add_argument("--phases", default=None)
    sp.add_argument("--no-cancel", action="store_true")
    sp.set_defaults(func=cmd_fix)

    sp = sub.add_parser("pool")
    sp.add_argument("--reps", type=int, default=3)
    sp.add_argument("--fix", action="store_true")
    sp.set_defaults(func=cmd_pool)

    sp = sub.add_parser("contrast")
    sp.set_defaults(func=cmd_contrast)

    sp = sub.add_parser("sigint")
    sp.add_argument("--reps", type=int, default=3)
    sp.set_defaults(func=cmd_sigint)

    sp = sub.add_parser("fields")
    sp.add_argument("--phases", default="entry_wait,exit_pre_commit")
    sp.add_argument("--reps", type=int, default=2)
    sp.set_defaults(func=cmd_fields)

    args = p.parse_args(argv)
    os.makedirs(args.log_dir, exist_ok=True)
    return args.func(args)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        sys.exit(main())
