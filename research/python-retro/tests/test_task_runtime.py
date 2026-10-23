"""The injection task must have ordinary asyncio task identity on CPython 3.15."""

import asyncio
import unittest

from dropwise_py.engine import PureTask, _CutTask


class TaskRuntimeTests(unittest.IsolatedAsyncioTestCase):
    async def test_current_task_and_timeout(self):
        parent = asyncio.current_task()

        async def target():
            current = asyncio.current_task()
            self.assertIs(current, task)
            self.assertIn(current, asyncio.all_tasks())
            async with asyncio.timeout(1):
                await asyncio.sleep(0)
                self.assertIs(asyncio.current_task(), current)
            with self.assertRaises(TimeoutError):
                async with asyncio.timeout(0):
                    await asyncio.sleep(0)
            self.assertEqual(current.cancelling(), 0)

        task = PureTask(target())
        await task
        self.assertIs(asyncio.current_task(), parent)
        self.assertNotIn(task, asyncio.all_tasks())

    async def test_cancel_count_visible_in_cleanup(self):
        observed = []

        async def target():
            try:
                await asyncio.sleep(0)
            finally:
                observed.append(asyncio.current_task().cancelling())
                await asyncio.sleep(0)
                observed.append(asyncio.current_task().cancelling())

        task = _CutTask(target(), cut_after=1)
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertEqual(observed, [1, 1])
        self.assertTrue(task.cut_requested)
