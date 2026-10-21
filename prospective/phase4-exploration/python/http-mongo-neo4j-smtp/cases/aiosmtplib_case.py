"""aiosmtplib 5.1.3: one complete send on a long-lived SMTP instance.

Target:    SMTP.send_message(target message) through the tested SMTP object.
Survivor:  a second send on the SAME SMTP object (same mailbox/connection),
           then NOOP on that object as the same-connection readback.
Observer:  GreenMail REST API, direct to 8080 (bypasses the SMTP object and the
           delay proxy); the mailbox is created fresh per scenario instance.
Pending:   read-only DelayProxy in front of SMTP 3025, 50 ms per chunk.
Bound:     2 s per operation.

The compared observation carries the survivor's concrete SMTP reply codes, the
NOOP code, and the observer's subject list (token -> <run>).  The target's own
outcome is printed as a separate DIAG line, not compared.
"""

from __future__ import annotations

import asyncio
import contextlib
import email.message
from email.utils import make_msgid
from urllib.parse import urlparse

import aiosmtplib
import httpx

from .support import (
    BOUND,
    ENGINE_TIMEOUT,
    DelayProxy,
    Expect,
    Scenario,
    base_parser,
    bounded,
    compact,
    explore_observed,
    level_b1,
    level_retry,
    norm,
    print_full_report,
    unique,
)


def _code(resp) -> object:
    """Concrete SMTP reply code, or the bounded error string."""
    if isinstance(resp, str):
        return resp
    try:
        return int(resp.code)
    except Exception:  # noqa: BLE001
        return repr(resp)


def _send_result(value) -> object:
    if isinstance(value, str):
        return value
    try:
        codes, final = value
        return {
            "recipients": {addr: _code(resp) for addr, resp in codes.items()},
            "final": _code(final),
        }
    except Exception:  # noqa: BLE001
        return repr(value)


def _message(token: str, label: str, sender: str, recipient: str) -> email.message.EmailMessage:
    msg = email.message.EmailMessage()
    msg["From"] = sender
    msg["To"] = recipient
    msg["Subject"] = f"{token} {label}"
    msg["Message-ID"] = f"<{token}.{label}@dw.test>"
    msg.set_content(f"body {token} {label}")
    return msg


class SmtpScenario(Scenario):
    def __init__(self, smtp_host: str, smtp_port: int, rest_url: str) -> None:
        self.smtp_host = smtp_host
        self.smtp_port = smtp_port
        self.rest_url = rest_url
        self.token = ""
        self.user = ""
        self.proxy: DelayProxy | None = None
        self.smtp: aiosmtplib.SMTP | None = None
        self.observer: httpx.AsyncClient | None = None

    async def setup(self) -> None:
        self.token = unique("smtp")
        login = self.token
        self.user = f"{login}@example.com"
        self.proxy = DelayProxy(self.smtp_host, self.smtp_port, delay=0.05)
        proxy_port = await self.proxy.start()
        self.observer = httpx.AsyncClient(
            base_url=self.rest_url, trust_env=False, timeout=httpx.Timeout(BOUND)
        )
        # Fresh GreenMail user per scenario instance: no earlier run's message
        # can enter this observation.
        created = await bounded(
            self.observer.post(
                "/api/user",
                json={"email": self.user, "login": login, "password": "pass"},
            ),
            "create-user",
        )
        self.smtp = aiosmtplib.SMTP(
            hostname="127.0.0.1", port=proxy_port, timeout=BOUND
        )
        connected = await bounded(self.smtp.connect(), "smtp-connect")
        noop = await bounded(self.smtp.noop(), "smtp-noop")
        print(
            f"DIAG setup token={self.token} user={self.user} proxy={proxy_port} "
            f"create={getattr(created, 'status_code', created)} connect={_code(connected)} "
            f"noop={_code(noop)}",
            flush=True,
        )

    async def close(self) -> None:
        if self.smtp is not None:
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(self.smtp.quit(), BOUND)
            with contextlib.suppress(BaseException):
                self.smtp.close()
        if self.observer is not None:
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(self.observer.aclose(), BOUND)
        if self.proxy is not None:
            with contextlib.suppress(BaseException):
                await asyncio.wait_for(self.proxy.close(), BOUND)

    async def _send(self, label: str):
        assert self.smtp is not None
        msg = _message(self.token, label, self.user, self.user)
        result = await bounded(
            self.smtp.send_message(msg, sender=self.user, recipients=[self.user]),
            f"send-{label}",
        )
        return _send_result(result)

    async def target(self):
        result = await self._send("target")
        print(f"DIAG target token={self.token} result={result!r}", flush=True)
        return result

    async def same(self):
        # Identical arguments to the target (same message id/subject).
        return await self._send("target")

    async def different(self):
        return await self._send("b3")

    async def _mailbox(self):
        resp = await bounded(
            self.observer.get(f"/api/user/{self.user}/messages"), "mailbox"
        )
        if isinstance(resp, str):
            return resp
        if resp.status_code != 200:
            return f"ERR mailbox status {resp.status_code}"
        rows = resp.json() or []
        subjects = []
        for row in rows:
            subject = row.get("subject") or ""
            subjects.append(norm(subject, self.token))
        return sorted(subjects)

    async def survivor(self):
        sent = await self._send("survivor")
        noop = await bounded(self.smtp.noop(), "survivor-noop")
        mailbox = await self._mailbox()
        print(
            f"DIAG survivor token={self.token} send={sent!r} "
            f"noop={_code(noop)} mailbox={mailbox!r}",
            flush=True,
        )
        return {
            "send": norm(sent, self.token),
            "noop": _code(noop),
            "mailbox": mailbox,
            "own": _norm_codes(sent),
        }

    async def observe(self):
        return {"mailbox": await self._mailbox()}


def _norm_codes(sent) -> object:
    if not isinstance(sent, dict) or "recipients" not in sent:
        return sent
    return {"final": sent.get("final"), "n_recipients": len(sent.get("recipients") or {})}


async def run_levels(args, levels, plans) -> None:
    smtp = urlparse(args.url)
    if smtp.scheme not in ("smtp", ""):
        raise SystemExit(f"not an smtp URL: {args.url}")
    rest = args.http or "http://127.0.0.1:8080"
    factory = lambda: SmtpScenario(smtp.hostname or "127.0.0.1", smtp.port or 3025, rest)

    reports = []
    for level in levels:
        print(f"\n=== LEVEL {level} ===", flush=True)
        if level == "B1":
            report = await level_b1("aiosmtplib", factory, reps=args.reps,
                                    timeout=ENGINE_TIMEOUT, plans=plans)
        elif level in ("B2", "B3"):
            report = await level_retry(
                "aiosmtplib", factory, level=level,
                retry="same" if level == "B2" else "different",
                reps=args.reps, timeout=ENGINE_TIMEOUT, plans=plans,
            )
        elif level == "M1":
            report = await explore_observed(
                "aiosmtplib", factory, expect=Expect.ATOMIC,
                reps=args.reps, timeout=ENGINE_TIMEOUT, plans=plans,
            )
        else:
            raise SystemExit(f"unknown level {level}")
        print_full_report(report)
        print(compact(report), flush=True)
        reports.append((level, report))

    print("\n=== FINAL ===", flush=True)
    for level, report in reports:
        print(compact(report), flush=True)
        print(f"violations_by_plan={[(t.plan, t.rep) for t in report.violations()]}", flush=True)
        print(f"unrealized_by_plan={[(t.plan, t.rep) for t in report.unrealized()]}", flush=True)


async def main() -> None:
    parser = base_parser(__doc__)
    parser.add_argument("--http", required=True, help="GreenMail REST base URL")
    parser.add_argument("--plans", default=None)
    args = parser.parse_args()
    levels = [x.strip() for x in args.levels.split(",") if x.strip()]
    plans = [int(x) for x in args.plans.split(",")] if args.plans else None
    print(
        f"case=aiosmtplib version={aiosmtplib.__version__} url={args.url} "
        f"rest={args.http} reps={args.reps} levels={levels}",
        flush=True,
    )
    await run_levels(args, levels, plans)


if __name__ == "__main__":
    with contextlib.suppress(KeyboardInterrupt):
        asyncio.run(main())
