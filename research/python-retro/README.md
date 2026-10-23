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

## 当前开发环境（2026-10-23）

当前引擎面向 **CPython 3.15**，不再维护旧 3.13 环境。仓库根目录的
`.python-version` 选择 3.15，PostgreSQL/MySQL runner 默认使用根目录 `.venv`。
它们仍允许用 `PY`（个别脚本为 `DW_PYTHON`）显式选择解释器。历史数据中的
解释器、包版本和服务版本不回写为新版本。

从仓库根目录安装和检查：

```sh
uv venv
uv pip install -r research/python-retro/requirements-dev.txt
export PYTHONPATH="$PWD/prospective/phase4-exploration/python/pg-mysql:$PWD/research/python-retro"
.venv/bin/python -m pytest research/python-retro/tests
```

仅运行引擎的无服务测试不需要第三方包：

```sh
PYTHONPATH=research/python-retro python -m unittest discover -s research/python-retro/tests -v
```

3.15 把纯 Python Task 的登记与原生 `current_task()` / `all_tasks()` 分开了。
`engine.PureTask` 在每次执行期间登记正确的当前任务，并登记任务生命周期；
不改写全局 asyncio 函数。当前任务、超时、取消计数以及清理行为都有回归。
`_CutTask`、`_PhasedTask`、位置比较和阶段比较共用这个实现。

本轮 PostgreSQL 复验使用 3.15.1、PostgreSQL 18.6、asyncpg 0.32.0，
以及 `/home/ava/Worktrees/psycopg/partial-send/psycopg` 的
psycopg 3.3.7.dev1 源码，而不是把已发布 wheel 误称为补丁：

```sh
uv pip install --no-deps -e /home/ava/Worktrees/psycopg/partial-send/psycopg
```

psycopg binary 后端为 3.3.6，绑定 libpq 18.6。纯 Python 后端也使用该 wheel
自带的 libpq：本机没有系统 libpq，故在 `.venv/lib/libpq.so` 建立指向 bundled
libpq 的链接，并为测试进程及其子进程设置 `.venv/lib` 和
`psycopg_binary.libs` 的 `LD_LIBRARY_PATH`。这是本地测试依赖配置，不修改驱动的
库发现逻辑；有系统 libpq 的机器不需要这一步。上游扩展测试还需要 `pproxy`。

`pg-mysql/tests` 包含真实数据库负对照，完整运行需要本地 PostgreSQL；本轮用
Podman 启动仅绑定 loopback 的一次性数据库，没有重启系统 Docker 或业务服务。
阶段比较的当前判据、结果和范围见
[Round 5](../../prospective/phase4-exploration/ROUND5-RESULTS.md)。

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
