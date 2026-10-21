# python-retro：Dropwise 判定的 asyncio 移植与回溯检测

把 Dropwise 的取消注入 + 幸存者 + 独立观察者 + pre/post 校准移植到 Python
asyncio，对 redis-py、SQLAlchemy 和 aiohttp 的历史问题做回溯检测。
最终结果见 `RESULTS.md`，P1/P2 已完成分诊并归属已知 SQLAlchemy #6652（§5.6），
没有待上报的新问题。`PRIVATE-NOTES.md` 保留原始诊断，不对外发布。

## 目录

- `dropwise_py/engine.py`：引擎。用纯 Python `Task` 的 step 计数枚举
  “让出事件循环”的挂起点，第 k 次后 `Task.cancel()`；pre/post 各两次，
  不一致记 Unknown，等于 pre 或 post 通过，否则 Neither。B1/B2/B3/M1 都在
  这里。
- `dropwise_py/fields.py`：字段级观察契约（EFFECT/STATE、逐字段 Expect、
  覆盖缺口 Uncovered/Degenerate）。增量模块，不改变 `engine.py` 行为；
  `tests/test_fields.py` 为无服务测试，第五轮同步 psycopg harness 复用同一
  `judge_trial`。
- `cases/redis_case.py`：R1 `BLPOP`（4.5.3/4.5.4）、R2 pipeline
  （4.5.2/4.5.3）。R1 用服务端阻塞制造未读回复；R2 用本目录的延迟 TCP 代理。
- `cases/sqla_case.py`：SQLAlchemy 1.4.17/1.4.18 + asyncpg 0.23.0。纯取消
  阶梯（计划 2–6）和 issue #6592 自己的 timeout-BEGIN 注入。
- `cases/aiohttp_case.py`：3.14.0/3.14.1，进程内 aiohttp 服务器，四层 +
  `data_received` 插桩 + idle `sock_read` 诊断。
- `cases/common.py`：参数解析和延迟代理。
- `run-redis.sh` / `run-postgres.sh` / `run-aiohttp.sh`：起一次性容器、等待、
  跑三轮、写 `logs/`、退出时删容器。容器只绑 127.0.0.1。

## 环境

历史运行环境由 uv 建在仓库外 `~/dropwise-py-work/`，交卷后已删除；
以下是原版本记录，重跑需另行重建环境：

```
venvs/redis-4.5.2,4.5.3,4.5.4        Python 3.10.20
venvs/sqlalchemy-1.4.17,1.4.18       Python 3.9.25 + asyncpg 0.23.0
venvs/aiohttp-3.14.0,3.14.1          Python 3.13.13
```

服务端：`redis:8-alpine`（Redis 8.8.0）、`postgres:18-alpine`（PostgreSQL
18.4）。digest 和包版本见 `logs/environment.txt`。

## 历史运行入口

以下 runner 对应原正式实验，不包含全部后续分诊。SQLAlchemy 历史场景会吞掉
幸存者的取消；诊断更正见 `RESULTS.md` §8.2。不要直接复用到 Phase 4。
后续独立最小复现见 `repro_6652.py`，补丁在 `patches/`，分诊汇总在
`logs/triage-summary.txt`。本次收尾未修改实验代码或重新生成运行结果。

```sh
bash research/python-retro/run-redis.sh          # ROUNDS=3, TARGETS=blpop,pipeline
bash research/python-retro/run-postgres.sh       # ROUNDS=3, MODES=timeout-begin,ladder
bash research/python-retro/run-aiohttp.sh        # ROUNDS=3
```

runner 用固定 digest 校验镜像；若 tag 解析到别的 digest 会打印警告。Redis/
Postgres runner 在 EXIT trap 里 `docker rm -f`，实验结束时 `docker ps -a`
没有遗留容器（记录在 `logs/docker-ps-*.txt`）。
