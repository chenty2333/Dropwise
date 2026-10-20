import asyncio
from .engine import Scenario, explore_observed, level_b1, level_retry, print_report

STATE = {"v": 0}

class S(Scenario):
    def __init__(self): self.conn = None
    async def setup(self): STATE["v"] = 0
    async def target(self):
        await asyncio.sleep(0.001)
        STATE["v"] = 1
        await asyncio.sleep(0.001)
        STATE["v"] = 2
    async def same(self): await asyncio.sleep(0.001); return "same-ok"
    async def different(self): await asyncio.sleep(0.001); return "diff-ok"
    async def survivor(self):
        await asyncio.sleep(0.001)
        STATE["v"] = 7
        return STATE["v"]
    async def observe(self): return STATE["v"]
    async def close(self): pass

async def main():
    print_report(await level_b1("selftest", S, reps=1))
    print_report(await level_retry("selftest", S, level="B2", retry="same", reps=1))
    print_report(await explore_observed("selftest", S, reps=1))

asyncio.run(main())
