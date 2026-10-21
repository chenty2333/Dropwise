"""Run with: DATABASE_URL=postgresql://... python psycopg_entry.py."""
import asyncio
import os
import uuid

async def main():
    dsn = os.environ['DATABASE_URL']
    import psycopg
    conn = await psycopg.AsyncConnection.connect(dsn, autocommit=True)
    observer = await psycopg.AsyncConnection.connect(dsn, autocommit=True)

    async def scalar(c, sql):
        return (await (await c.execute(sql)).fetchone())[0]
    table = 'entry_cancel_' + uuid.uuid4().hex
    try:
        await observer.execute(f'CREATE TABLE {table} (v integer)')

        async def interrupted_entry():
            asyncio.get_running_loop().call_soon(asyncio.current_task().cancel)
            async with conn.transaction():
                raise AssertionError('transaction body must not be entered')
        try:
            await asyncio.create_task(interrupted_entry())
        except asyncio.CancelledError:
            print('entry cancelled; body not entered')
        await asyncio.sleep(0.1)
        async with conn.transaction():
            await conn.execute(f'INSERT INTO {table} VALUES (42)')
        print('retry transaction exited normally')
        print('same connection:', await scalar(conn, f'SELECT count(*) FROM {table}'))
        print('independent observer:', await scalar(observer, f'SELECT count(*) FROM {table}'))
        try:
            await conn.rollback()
        except Exception as exc:
            print('rollback:', type(exc).__name__, str(exc))
    finally:
        await conn.close()
        await observer.execute(f'DROP TABLE IF EXISTS {table}')
        await observer.close()
asyncio.run(asyncio.wait_for(main(), 15))
