"""Minimal repro for SQLAlchemy #6652 (1.4.0-1.4.18): a cancelled task leaves
the asyncpg connection in a pool with an open transaction.  Run with
--mode begin (P1: next transaction becomes SAVEPOINT) or --mode commit
(P2: connection never returned to the pool).  This patches only the
in-process adapter, not any file."""
import argparse, asyncio, sys
import asyncpg
from sqlalchemy import text
from sqlalchemy.ext.asyncio import create_async_engine
from sqlalchemy.dialects.postgresql.asyncpg import AsyncAdapt_asyncpg_connection

KEY = "repro6652"
MODE = None
ARM = False
TARGET = None
LOOP = None

_orig_start = AsyncAdapt_asyncpg_connection._start_transaction
_orig_commit = AsyncAdapt_asyncpg_connection.commit

async def _start(self):
    if ARM and MODE == "begin":
        LOOP.call_soon(TARGET.cancel)
    return await _orig_start(self)

def _commit(self):
    if ARM and MODE == "commit":
        LOOP.call_soon(TARGET.cancel)
    return _orig_commit(self)

AsyncAdapt_asyncpg_connection._start_transaction = _start
AsyncAdapt_asyncpg_connection.commit = _commit

async def main(url):
    global ARM, TARGET, LOOP
    LOOP = asyncio.get_running_loop()
    raw = url.replace("postgresql+asyncpg://", "postgresql://", 1)
    obs = await asyncpg.connect(raw)
    await obs.execute("CREATE TABLE IF NOT EXISTS repro6652 (id text primary key, v int not null)")
    await obs.execute("INSERT INTO repro6652 VALUES ($1,0) ON CONFLICT (id) DO UPDATE SET v=0", KEY)
    eng = create_async_engine(url, pool_size=1, max_overflow=0)
    async with eng.begin() as conn:
        await conn.execute(text("SELECT 1"))

    async def target():
        async with eng.begin() as conn:
            await conn.execute(text("UPDATE repro6652 SET v=1 WHERE id=:k"), {"k": KEY})
    TARGET = asyncio.create_task(target())
    ARM = True
    try:
        await asyncio.wait_for(TARGET, 10)
        print("target completed (cancel not delivered)", flush=True)
    except BaseException as e:
        print("target:", type(e).__name__, flush=True)
    ARM = False
    print("pool after target:", eng.pool.status(), flush=True)

    t0 = asyncio.get_running_loop().time()
    try:
        async with eng.begin() as conn:
            await conn.execute(text("UPDATE repro6652 SET v=7 WHERE id=:k"), {"k": KEY})
        write = "ok"
    except BaseException as e:
        write = f"ERR {type(e).__name__}: {e}"
    print(f"survivor write={write} elapsed={asyncio.get_running_loop().time()-t0:.2f}s", flush=True)
    try:
        async with eng.connect() as conn:
            own = (await conn.execute(text("SELECT v FROM repro6652 WHERE id=:k"), {"k": KEY})).scalar()
    except BaseException as e:
        own = f"ERR {type(e).__name__}: {e}"
    print("own:", own, "observer:", await obs.fetchval("SELECT v FROM repro6652 WHERE id=$1", KEY), flush=True)
    print("pool final:", eng.pool.status(), flush=True)
    await eng.dispose(); await obs.close()

if __name__ == "__main__":
    p = argparse.ArgumentParser(); p.add_argument("--url", required=True); p.add_argument("--mode", choices=("begin","commit"), required=True)
    a = p.parse_args(); MODE = a.mode
    try: asyncio.run(main(a.url))
    except BaseException as e: print("EXIT", type(e).__name__, e, flush=True)
