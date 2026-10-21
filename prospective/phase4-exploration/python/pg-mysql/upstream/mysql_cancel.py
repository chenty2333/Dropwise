"""MYSQL_PORT=3306 MYSQL_PASSWORD=... python mysql_cancel.py"""
import asyncio
import os

import mysql.connector
from mysql.connector import aio


async def main():
    config = dict(host='127.0.0.1', port=int(os.environ['MYSQL_PORT']),
                  user='root', password=os.environ['MYSQL_PASSWORD'], autocommit=True)
    conn = await aio.connect(**config)
    observer = await aio.connect(**config)
    cur = await conn.cursor(buffered=True)
    obs = await observer.cursor(buffered=True)
    try:
        print('connector:', mysql.connector.__version__)
        pending = asyncio.create_task(cur.execute('SELECT SLEEP(0.5) AS cancelled_value'))
        # Confirm the command reached the server before cancelling its caller.
        for _ in range(100):
            await obs.execute('SHOW PROCESSLIST')
            if any(row[0] == conn.connection_id and row[7] ==
                   'SELECT SLEEP(0.5) AS cancelled_value' for row in await obs.fetchall()):
                break
            await asyncio.sleep(0.005)
        else:
            await pending
            raise RuntimeError('did not observe the in-flight query; retry')
        pending.cancel()
        try:
            await pending
        except asyncio.CancelledError:
            print('query cancelled')
        print('unread_result:', conn.unread_result)
        await cur.execute('SELECT 42 AS next_value')
        print('next query result:', await cur.fetchall())
        print('next query columns:', cur.column_names)
    finally:
        await conn.close()
        await obs.close()
        await observer.close()


asyncio.run(asyncio.wait_for(main(), 15))
