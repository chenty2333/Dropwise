"""Protocol & responsibility stage definitions and classification rules.

This module formalizes the protocol and responsibility stages of async database
transactions without relying on yield counters (k), fault injection labels,
or expected outcomes.
"""

from __future__ import annotations

import enum


class Stage(str, enum.Enum):
    ENTER_SEND = "enter_send"         # Client writing BEGIN to wire
    ENTER_WAIT = "enter_wait"         # Client waiting for server ack of BEGIN
    BODY_SEND = "body_send"           # Client writing query / params (backpressure)
    BODY_FETCH = "body_fetch"         # Client waiting for / reading query response
    COMMIT_SEND = "commit_send"       # Client writing COMMIT to wire
    COMMIT_WAIT = "commit_wait"       # Client waiting for server ack of COMMIT
    ROLLBACK_WAIT = "rollback_wait"   # Client writing/waiting for ROLLBACK
    SP_ENTER = "sp_enter"             # Client creating savepoint (nested tx enter)
    SP_RELEASE = "sp_release"         # Client releasing savepoint (nested tx exit)
    RECOVERY_DRAIN = "recovery_drain" # Client in cancellation recovery (_try_cancel/drain)
    UNKNOWN = "unknown"               # State ambiguous or not classifiable


class StageClassifier:
    """Classifies the current execution stage based on observable driver signals.

    Observable signals:
    1. Caller method context: what driver API is currently in flight:
       - 'enter' (e.g. AsyncTransaction.__aenter__)
       - 'body' (e.g. cursor.execute or streaming fetch)
       - 'commit' (e.g. clean AsyncTransaction.__aexit__)
       - 'rollback' (e.g. exceptional AsyncTransaction.__aexit__)
       - 'sp_enter' (nested savepoint enter)
       - 'sp_release' (nested savepoint exit)
       - 'recovery' (cancellation recovery hook)
    2. Connection transaction status:
       - IDLE (0), ACTIVE (1), INTRANS (2), INERROR (3)
    3. Socket I/O readiness:
       - WAIT_W (write readiness, client pushing to socket buffer)
       - WAIT_R (read readiness, client waiting for server response)
       - None (not suspended on socket wait, e.g. CPU or internal coroutine step)

    Ambiguities:
    If caller method context is missing (pure black-box socket tap), WAIT_R while
    ACTIVE is ambiguous between BEGIN, user query, and COMMIT.
    """

    @staticmethod
    def classify(
        method_context: str | None,
        tx_status: int | None,
        io_readiness: int | None,
    ) -> Stage:
        # Check recovery first
        if method_context == "recovery":
            return Stage.RECOVERY_DRAIN

        is_write = bool(io_readiness and (io_readiness & 2))  # WAIT_W
        is_read = bool(io_readiness and (io_readiness & 1))   # WAIT_R

        if method_context == "enter":
            if is_write:
                return Stage.ENTER_SEND
            if is_read:
                return Stage.ENTER_WAIT
            return Stage.UNKNOWN

        if method_context == "body":
            if is_write:
                return Stage.BODY_SEND
            if is_read:
                return Stage.BODY_FETCH
            return Stage.UNKNOWN

        if method_context == "commit":
            if is_write:
                return Stage.COMMIT_SEND
            if is_read:
                return Stage.COMMIT_WAIT
            return Stage.UNKNOWN

        if method_context == "rollback":
            return Stage.ROLLBACK_WAIT

        if method_context == "sp_enter":
            return Stage.SP_ENTER

        if method_context == "sp_release":
            return Stage.SP_RELEASE

        # Ambiguous / unclassified
        return Stage.UNKNOWN
