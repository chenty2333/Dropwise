# Python asyncio 回溯检测：redis-py、aiohttp、SQLAlchemy + asyncpg

> 2026-10-19。实验代码在本目录；Python 环境和 venv 在仓库外
> `~/dropwise-py-work/`（交卷后已删除）。原始输出在 `logs/`。本文 3.4 节的“实验前预期”
> 在正式三轮运行之前定稿（约 2026-10-19 14:50）；其中标注“预实验”的条目
> 是在小规模探针之后、正式运行之前补的。第 5 节及以后是运行后填写的实际
> 结果。所有数字来自 `logs/` 里的实际输出。
>
> 结论先行：redis-py R1（BLPOP）的取消注入在修复前报出、修复后消失，
> 且只有 M1 抓到；R2（pipeline）没有复现，不能据此评价 #2641。
> SQLAlchemy #6592 在 Python 3.9 的“纯取消”路径上不是修复对象：1.4.18
> 修的是 BEGIN 内的 `asyncio.TimeoutError`，用 issue 自己的注入方式能
> 复现（1.4.17 报、1.4.18 干净）。纯取消另有一个 1.4.17/1.4.18 都存在的
> “幸存者写对观察者不可见”现象，后续已归属已知 #6652，1.4.19 修复；
> P1/P2 最终分诊及补丁对照见 §5.6，均非新发现。
> aiohttp #12795 指定的修复（3.14.1）与取消无关，取消场景在 3.14.0/3.14.1
> 都干净；issue 真正的 ~2 ms 症状是 idle 池化连接上的 `sock_read` 定时器
> （#12954，3.14.2 才修）。方法不是 Rust 专属，但“挂起点枚举”只覆盖
> await 点，覆盖不了同步回调里发生的状态转移。

**收尾更新（2026-10-21）：** 已将 10 月 19 日最后一轮分诊同步到本文；本次未重跑实验。
原正式阶梯及实验前预期保留，后续分诊不冒充原先的预期或完整四层复跑。

## 1. 移植的判定方法

Dropwise 在 Rust 里做的事，逐项对应到 asyncio：

| Rust | Python asyncio |
|---|---|
| 目标 future 每次返回 `Pending` 是一个挂起点 | 目标任务每次让出事件循环是一个挂起点：纯 Python `Task` 的 `_Task__step` 在任务未完成时计数 |
| 计划：在第 k 个 Pending 处取消 | 第 k 次挂起后调用 `Task.cancel()` |
| 取消后让被取消的工作 settle | 目标 `Task` 被 `await` 到结束，`finally`/`except` 清理执行；不做固定 sleep（见 3.3） |
| 幸存者通过同一个池对同一对象做有副作用的操作 | 同一 `Scenario` 实例里的池/引擎，对同一个 key/行操作 |
| 独立观察者绕过池读状态 | 单独连接/会话，不经过被测池 |
| 两条基线：不执行 P（pre）、完整执行 P（post），各两次 | 同左；两次样本不一致记为 `Unknown` |
| 观察等于 pre 或 post 通过，否则违反 | 同左；`Expect::Atomic` |
| B1/B2/B3 | 取消后停止 / 同参数重试 / 换同类操作，只判是否返回 `Ok` |

**取消方式的选择：`Task.cancel()`，不向内层协程 `throw(CancelledError)`。**
理由：这才是生产代码里发生的事；它会取消任务当前等待的 future，Python
3.11+ 还会设置 `Task.cancelling()`，库的清理代码（例如 redis-py 4.5.4 的
`asyncio.shield` 修复、aiohttp 的 `uncancel` 判断）会读这个状态。手工
`throw` 会绕过这些状态，测到的就不是库实际面对的取消。

**挂起点计数。** engine 使用 `asyncio.tasks._PyTask`（纯 Python `Task`）的
子类，覆盖名称改写后的 `_Task__step`。每完成一次 step 且任务未完成，
`yields += 1`；在第 k 次后立即 `cancel()`。计数是运行时行为，不是源码里的
`.await` 数量，因此包含库内部的 await；这也意味着有真实 I/O 时 k 的数量
会随调度/网络变化（见 3.3、第 8 节）。计划未实际取消到目标时记为
`unrealized`，不算违反。

## 2. 版本与依据

原正式检测版本从 PyPI 安装、不带补丁；后续 §5.6 的修复/回退对照在依赖副本上进行，单独标记。

### 2.1 redis-py

| 版本 | PyPI 上传时间 | 与修复的关系 | 依据 |
|---|---|---|---|
| 4.5.2 | 2023-03-20T10:01:54Z | #2641 之前的最后一个版本 | PyPI JSON |
| 4.5.3 | 2023-03-22T16:15:28Z | 包含 #2641：`Pipeline.execute` 加 `asyncio.shield`，`CancelledError` 时 `conn.disconnect(nowait=True)`；只覆盖 pipeline | 逐行对比 4.5.2/4.5.3 的 `redis/asyncio/client.py`；[PR #2641](https://github.com/redis/redis-py/pull/2641) 的 diff 与 4.5.3 一致 |
| 4.5.4 | 2023-03-29T13:31:06Z | 包含 #2666：`Redis.execute_command` 加 `asyncio.shield` 和 `_try_send_command_parse_response`，覆盖非 pipeline 命令（CVE-2023-28859） | 4.5.4 的 `client.py:503-533`；[release v4.5.4](https://github.com/redis/redis-py/releases/tag/v4.5.4)；NVD CVE-2023-28859 的 `versionEndExcluding: 4.5.4` |

上游：
[redis-py#2624](https://github.com/redis/redis-py/issues/2624)（原始报告）、
[#2665](https://github.com/redis/redis-py/issues/2665)（4.5.3 仍可复现，
说明 #2641 只修 pipeline）、[#2666](https://github.com/redis/redis-py/pull/2666)
（4.5.4 的通用修复）。CVE 见
[CVE-2023-28858](https://nvd.nist.gov/vuln/detail/CVE-2023-28858)、
[CVE-2023-28859](https://nvd.nist.gov/vuln/detail/CVE-2023-28859)。

本案例的两个子目标因此分开：
- **R1 `blpop`（主）**：非 pipeline 的 `BLPOP`，服务端阻塞、回复未读时取消。
  4.5.3 仍漏洞（#2665 明确说 #2641 只修了 pipeline），4.5.4 修好。
  这对应 CVE-2023-28859。
- **R2 `pipeline`（辅）**：`MULTI; SET; GET; EXEC`，对应 #2641 和
  CVE-2023-28858；4.5.2 → 4.5.3。

CVE-2023-28858 的 NVD 原文明确说“4.3.6、4.4.3、4.5.3 是 changing the
behavior for pipeline operations；数据泄漏的通用问题见 CVE-2023-28859”。
所以把 4.5.2→4.5.3 当作整个 #2624 的完整修复是不准确的；本文两个子目标
都跑，分别下结论。

### 2.2 aiohttp

上游：[aio-libs/aiohttp#12795](https://github.com/aio-libs/aiohttp/issues/12795)、
修复 commit [4b2e470](https://github.com/aio-libs/aiohttp/commit/4b2e47079231cf3ec7546372ffa073ad19f1ba9c)；
真正相关的 idle 定时器是 [#12954](https://github.com/aio-libs/aiohttp/pull/12954)
（3.14.2）。

| 版本 | PyPI 上传时间 | 与修复的关系 | 依据 |
|---|---|---|---|
| 3.14.0 | 2026-06-01T19:36:39Z | 回归版本：`data_received` 的 `feed_data` 外层是 `except Exception` | 安装后读 `aiohttp/client_proto.py:325` |
| 3.14.1 | 2026-06-07T21:05:37Z | 包含 commit `4b2e470` / issue #12795 的修复：`except BaseException`，并在非 `Exception` 时重新抛出 | 安装后读 `client_proto.py:325`；[commit 4b2e470](https://github.com/aio-libs/aiohttp/commit/4b2e47079231cf3ec7546372ffa073ad19f1ba9c)；CHANGES.rst 3.14.1 条目 “Fixed CancelledError not closing a connection”。注意：commit diff 同时改 `connection_lost` 和 `data_received`，但 PyPI 3.14.1 只有 `data_received` 是 `BaseException`，`connection_lost` 仍是 `except Exception`（安装后逐行核对） |

### 2.3 SQLAlchemy + asyncpg

上游：[sqlalchemy/sqlalchemy#6592](https://github.com/sqlalchemy/sqlalchemy/issues/6592)、
修复 commit [2fa3931](https://github.com/sqlalchemy/sqlalchemy/commit/2fa39312b2c878b717d4c4f41380e5a8931eb374)。

| 版本 | PyPI 上传时间 | 与修复的关系 | 依据 |
|---|---|---|---|
| SQLAlchemy 1.4.17 | 2021-05-29T20:26:36Z | #6592 之前的最后一个版本；`base.py` 用 `not isinstance(e, Exception)` 判定 exit exception | 安装后读 `sqlalchemy/engine/base.py:1880` |
| SQLAlchemy 1.4.18 | 2021-06-10T18:26:35Z | 包含 commit `2fa3931`：新增 `util.is_exit_exception()`，把 `asyncio.TimeoutError`、`asyncio.CancelledError` 加进去 | 安装后读 `util/_concurrency_py3k.py:27`、`engine/base.py:1880`；[commit 2fa3931](https://github.com/sqlalchemy/sqlalchemy/commit/2fa39312b2c878b717d4c4f41380e5a8931eb374) |
| asyncpg 0.23.0 | 2021-05-17T19:02:32Z | 与 issue 相同的 DBAPI 版本；不是修复对象 | PyPI；issue 的 “Versions” 段写 0.23.0 |

**一个关键的语言事实（实验前就核实）：** 从 Python 3.8 起
`asyncio.CancelledError` 继承 `BaseException`，不再继承 `Exception`。因此
1.4.17 的 `not isinstance(e, Exception)` 对 `CancelledError` 已经为真，
`_is_disconnect` 已经会置位。修复新增的 `asyncio.CancelledError` 主要影响
Python 3.7（本机装不到 3.7）；在 3.9 上真正新增的是
`asyncio.TimeoutError`（3.11 前它仍是 `Exception`）。所以“纯取消路径”在
1.4.17 上不一定会复现 #6592，必须把 timeout 变体单独看（见 3.4、6.3）。

## 3. 环境与场景

### 3.1 服务端与 Python

| 案例 | 服务端 | 镜像 tag / digest | Python | venv |
|---|---|---|---|---|
| redis | Redis 8.8.0，只绑定 127.0.0.1 | `redis:8-alpine` / `redis@sha256:9d317178eceac8454a2284a9e6df2466b93c745529947f0cd42a0fa9609d7005` | 3.10.20 | `~/dropwise-py-work/venvs/redis-4.5.2`, `redis-4.5.3`, `redis-4.5.4` |
| SQLAlchemy | PostgreSQL 18.4，只绑定 127.0.0.1 | `postgres:18-alpine` / `postgres@sha256:1b1689b20d16a014a3d195653381cf2caa75a41a92d93b255a9d6ea29fd353aa` | 3.9.25 | `~/dropwise-py-work/venvs/sqlalchemy-1.4.17`, `sqlalchemy-1.4.18`（均含 asyncpg 0.23.0） |
| aiohttp | 进程内 `aiohttp.web` 服务器，127.0.0.1 随机端口 | 无容器 | 3.13.13 | `~/dropwise-py-work/venvs/aiohttp-3.14.0`, `aiohttp-3.14.1` |

所有容器只发布到 127.0.0.1，runner 退出时 `docker rm -f`；实验结束后的
`docker ps -a` 记录在 `logs/docker-ps-redis.txt`、`logs/docker-ps-postgres.txt`；
全部容器删除后的最终记录是 `logs/docker-ps-final.txt`。

### 3.2 四个层级（照 PLAN.md 第 3 节）

- **B1**：取消目标后停止。目标以 `CancelledError` 结束算正常；逃逸的其他
  异常或超时算违反。
- **B2**：取消后通过同一个池、同一参数再做一次同一个操作；返回即 `Ok`，
  异常/超时算违反，不检查值。
- **B3**：取消后通过同一个池做不同的同类操作；只看是否返回 `Ok`。
- **M1**：`Expect::Atomic`。幸存者通过同一个池对同一个 key/行做有副作用的
  操作，并在同一个池里再读一次；独立观察者绕过池读状态。观察值 =
  `{survivor: {write, own}, observer: ...}`。pre/post 各两次；不一致记
  `Unknown`，不算违反。

池大小都是 1。redis 用 `BlockingConnectionPool(max_connections=1)`：修好
的 4.5.4 用 `asyncio.shield` 让被取消的保护任务继续跑完，连接要等它
disconnect 完才 release；非阻塞池会让幸存者立即报 “Too many connections”，
这是场景的清理时序问题，不是被取消操作留给后继者的状态。阻塞池让幸存者
等这条连接，仍是“同一个池、同一条连接”。

每个层级每项 3 次重复；每个案例跑 3 轮。

### 3.3 挂起点与 settle

Rust 版在取消后有一个 settle 阶段。Python 版**不加固定 sleep**，只把目标
`Task` await 到结束：这样 `finally`/`except` 清理能跑完，但不会额外等网络
回复到达。原因：redis-py 的 `get_connection` 会用非破坏性 peek 检查缓冲
数据，如果多等一会儿，迟到的旧回复会被 peek 看到并触发重连，把要测的
窗口关掉；settle 的时长会直接决定能不能复现。因此正式结果的“计划数”和
`unrealized` 会随真实调度浮动，按 PLAN 第 4 节“只在同一轮内比较”。

### 3.4 实验前预期（正式三轮运行之前定稿）

最早的两条（R1/R2）在写引擎前定；A 和 S 的补充是在小规模探针之后、
正式三轮之前定稿的。正式数字只来自第 5 节的 `logs/`。

**R1 redis `blpop`（4.5.3 前 / 4.5.4 后）**

- 预期 `BLPOP` 在“命令已发出、回复未读”的挂起点被取消后，连接带着未读
  的 BLPOP 回复回到池里；幸存者的 `RPUSH` 会把这条回复当成自己的返回值。
- **B1**：不违反（只有取消本身）。
- **B2**：不违反。重试的 `BLPOP` 会读到上一条 BLPOP 的 nil 回复，仍是
  `Ok`；按层级定义不看值。
- **B3**：不违反。`RPUSH` 读到 BLPOP 回复也仍是 `Ok`。
- **M1**：4.5.3 **违反**，预期在最后一个挂起点（服务端回复仍在等待）；
  幸存者的 `write` 不是整数 1，而是上一条 BLPOP 的回复（`None`/list）。
  独立观察者可能仍看到 `['survivor']`（RPUSH 已生效），所以单靠观察者
  不够，`write` 必须进观察值。4.5.4 **干净**。
- 预期只有 M1 抓到；B1/B2/B3 都看不到。

**R2 redis `pipeline`（4.5.2 前 / 4.5.3 后）**

- 源码上，4.5.2 的 `Pipeline.execute` 在取消时只 `reset()` 释放连接，不读
  剩余回复；4.5.3 用 shield 并在取消时 disconnect。预期 M1 在 4.5.2 违反、
  4.5.3 干净；B1/B2/B3 不违反。
- 已知风险：pipeline 的多个回复常常在同一个 TCP 段里到达；如果取消时
  所有回复已经进入解析缓冲，`get_connection` 的 peek 会重连，M1 可能
  复现不了。预实验里 4.5.2 的 M1 没有报违反；正式运行若仍为零，按
  “挂起点枚举不到/被 peek 掩盖”分析，不把零发现当成修复有效的证据。

**S SQLAlchemy（1.4.17 前 / 1.4.18 后）**

- **纯取消**（`Task.cancel()`，Python 3.9）：预期四条层级都**不违反**。
  理由：`CancelledError` 是 `BaseException`，1.4.17 的
  `is_exit_exception` 已经为真，连接会被作废；#6592 的 commit 在
  Python 3.8+ 的取消路径上没有行为变化。
- **timeout 变体**（asyncpg `command_timeout` 抛 `asyncio.TimeoutError`）：
  这是 1.4.18 真正新增的路径。预期 1.4.17 把连接带着未结束事务还给池，
  幸存者报错或看到未提交状态；1.4.18 干净。若 timeout 变体也干净，则
  说明本机 PG 18 + asyncpg 0.23 的清理路径与 issue 不同，案例不可复现，
  按“未复现”记录原因，不硬造违反。

**A aiohttp（3.14.0 前 / 3.14.1 后）**

- 预期四条层级在 3.14.0 和 3.14.1 都**不违反**，即指定修复对“在 await
  点取消”的场景没有可观察差别。理由（实验前读源码 + 预实验）：
  `ResponseHandler.data_received` 是同步回调，`CancelledError` 只在 await
  点抛出，不可能在 `feed_data` 里被取消；真正的取消清理
  （`_connect_and_send_request` 的 `except BaseException: conn.close()`、
  `ClientResponse.read` 的 close、`release()` 的 `should_close`）在两版
  完全相同。
- 预期：aiohttp 这个案例**抓不到**，不是因为方法不灵敏，而是修复点不在
  挂起点上；issue 的 ~2 ms 症状是 idle 池化连接上的 `sock_read` 定时器，
  与取消无关（补充诊断见 5.5）。

**S 的补充（正式运行前，基于小规模预实验；保留原判断）**

后续更正：下述“同步卡住”的解释已被 §8.2 的诊断否定；保留此段仅用于说明当时为何排除 #7。

- 纯取消的目标在预热引擎后有 7 个挂起点。把目标取消在最后一个挂起点
  （#7）会让幸存者**同步卡住**（事件循环停在 select，连 `wait_for` 的定时器
  都不跑）；正式取消阶梯只跑 #2–#6，并明确记为“部分枚举”，不把卡住当成
  确认违反。
- #2 在 1.4.17 和 1.4.18 上都出现：幸存者的 `engine.begin()` 返回正常、
  同池读回 7，但独立观察者读到 0。这是**修复版上仍然存在**的现象，先记入
  `PRIVATE-NOTES.md`，不计入 #6592 的修复效果。
- **timeout-BEGIN 注入**（issue #6592 自己的复现方式：运行时把 asyncpg
  `Connection.execute` 的 BEGIN 改成 `timeout=0`，不修改文件）：
  1.4.17 预期 B1 通过、B2/B3/M1 违反（幸存者报
  `NoActiveSQLTransactionError: SAVEPOINT can only be used in transaction
  blocks`，观察者读到 0）；1.4.18 四条都干净（连接被作废，幸存者拿新连接）。
  这是修复真正新增的路径；它不是 `Task.cancel()` 的挂起点，因此与上面的
  纯取消阶梯分开统计。

## 4. 判定与记录口径

- 每个计划的一次运行是一个 trial；违反只统计 `Verdict::Neither`（或
  `Expect` 不允许的 Pre/Post）。`Unknown`（pre/post 两次样本不一致）不是
  违反，`unrealized`（计划中的取消没有落到目标）也不是违反，都单列。
- 每个层级在每个版本上跑 3 轮，每轮每个计划 3 次重复。表中 `计划/违反`
  按轮列出；同一轮内比较层级，跨轮只作稳定性检查。
- 分诊按 PLAN 第 4 节三类：**确认的问题**（能说明后继者看到了什么，并且
  有版本/补丁对照）、**契约外行为**、**测试框架错误**。本轮没有出现
  契约外行为；SQLAlchemy 纯取消的 P1/P2 后续已完成分诊，归属已知 #6652；
  P2 的永久等待还包含测试框架吞取消的放大效应（§5.6、§8.2）。
- 每个案例记录版本、环境、挂起点数（K）、每层每轮的 trial/violation/
  unknown/unrealized、后继者观察、工时和代码行数。零发现也照样报告。

## 5. 结果

### 5.1 redis R1：非 pipeline `BLPOP`（4.5.3 → 4.5.4，CVE-2023-28859）

K=4。每层每轮 3 次重复；表中 `计划/违反` 的三项依次是 R1、R2、R3。三轮
数字相同。所有轮次的 `unknown=0`；`unrealized=3/12`（`pending#3` 在每次
单独运行时目标已经完成，取消没有落到该挂起点，不算违反）。

| 版本 | 层级 | K | 计划/违反（R1, R2, R3） | unrealized | 分诊 |
|---|---|---:|---|---:|---|
| 4.5.3 | B1 | 4 | 12/0, 12/0, 12/0 | 3/3/3 | 通过（取消本身没有异常/超时） |
| 4.5.3 | B2 | 4 | 12/0, 12/0, 12/0 | 3/3/3 | 通过；#4 的重试返回 `None`，仍是 `Ok` |
| 4.5.3 | B3 | 4 | 12/0, 12/0, 12/0 | 3/3/3 | 通过；#4 的 `RPUSH` 返回 `False`，仍是 `Ok` |
| 4.5.3 | M1 | 4 | **12/3, 12/3, 12/3** | 3/3/3 | **确认的问题（已知上游 #2665/CVE-2023-28859）**：3 次都在 `pending#4` |
| 4.5.4 | B1 | 4 | 12/0, 12/0, 12/0 | 3/3/3 | 通过 |
| 4.5.4 | B2 | 4 | 12/0, 12/0, 12/0 | 3/3/3 | 通过 |
| 4.5.4 | B3 | 4 | 12/0, 12/0, 12/0 | 3/3/3 | 通过 |
| 4.5.4 | M1 | 4 | 12/0, 12/0, 12/0 | 3/3/3 | 违反消失 |

pre/post 在每一轮都相同（退化观察）：

```
pre = post = {'survivor': {'write': 1, 'own': ['survivor']}, 'observer': ['survivor']}
```

4.5.3 的 M1 违反观察（3 轮 × 3 次完全相同）：

```
{'survivor': {'write': False, 'own': ['survivor']}, 'observer': ['survivor']}
```

**一句话说明后继者看到了什么：** 幸存者对同一个 list key 的 `RPUSH` 本该
返回整数 1，却返回了被取消 `BLPOP` 的 nil 回复（`False`）；写本身生效了
（同池 `LRANGE` 和独立观察者都看到 `['survivor']`），错的是回复的归属。

日志：`logs/redis-blpop-4.5.{3,4}-r{1,2,3}-{B1,B2,B3,M1}.log`。

### 5.2 redis R2：pipeline `MULTI; SET; GET; EXEC`（4.5.2 → 4.5.3，CVE-2023-28858）

K=4。三轮、四个层级全部是 `12/0`，`unknown=0`，每轮 `unrealized=3/12`
（`pending#3` 目标已完成）。没有观察到违反，也没有版本差别。

| 版本 | B1 | B2 | B3 | M1 |
|---|---|---|---|---|
| 4.5.2 | 12/0 ×3 | 12/0 ×3 | 12/0 ×3 | 12/0 ×3 |
| 4.5.3 | 12/0 ×3 | 12/0 ×3 | 12/0 ×3 | 12/0 ×3 |

M1 的 pre/post 观察都是
`{'survivor': {'write': True, 'own': 'survivor'}, 'observer': 'survivor'}`，
没有违反。#1/#2 取消在命令发出前后，#4 取消在事务回复/清理阶段；**#4 是
唯一落在事务回复/清理阶段且实际取消的计划，但没有给幸存者留下可观察的
残留**。这与 4.5.2 的源码（取消时
`reset()` 只释放连接、不读剩余回复）不矛盾，但本场景没有把取消停在
“EXEC 回复已产生、尚未被读走”的窗口里；#3 又经常在取消前就完成。因此这是
**挂起点枚举不到/被响应批量到达掩盖**，不能反过来证明 #2641 的修复无效。

日志：`logs/redis-pipeline-4.5.{2,3}-r{1,2,3}-{B1,B2,B3,M1}.log`。

### 5.3 SQLAlchemy：timeout 在 BEGIN 内（1.4.17 → 1.4.18，#6592）

这不是 `Task.cancel()`，而是 issue #6592 自己的注入方式：运行时把 asyncpg
`Connection.execute` 对 BEGIN 的 `timeout` 设为 0（只在目标运行期间生效，
不改文件），让 `asyncio.TimeoutError` 在 `_start_transaction()` 里抛出。
每层每轮 3 次重复，三轮数字相同；`plan=begin-timeout`。

| 版本 | 层级 | 计划/违反（R1, R2, R3） | 分诊 |
|---|---|---|---|
| 1.4.17 | B1 | 3/0, 3/0, 3/0 | 通过；目标的 timeout 被场景自己 catch，B1 看不到后续污染 |
| 1.4.17 | B2 | **3/3, 3/3, 3/3** | **确认的问题（上游 #6592）**：重试同一个目标报 `NoActiveSQLTransactionError: SAVEPOINT ...` |
| 1.4.17 | B3 | **3/3, 3/3, 3/3** | **确认的问题**：换一个 `UPDATE` 也报同一个 SAVEPOINT 错误 |
| 1.4.17 | M1 | **3/3, 3/3, 3/3** | **确认的问题**：幸存者事务失败，观察者读到 0 |
| 1.4.18 | B1 | 3/0, 3/0, 3/0 | 通过 |
| 1.4.18 | B2 | 3/0, 3/0, 3/0 | 通过；幸存者拿到新连接 |
| 1.4.18 | B3 | 3/0, 3/0, 3/0 | 通过 |
| 1.4.18 | M1 | 3/0, 3/0, 3/0 | 违反消失 |

pre/post 在每一轮都相同：

```
pre = post = {'survivor': {'write': 'ok', 'own': 7}, 'observer': 7}
```

1.4.17 的 M1 违反观察（3 轮 × 3 次）：

```
{'survivor': {'write': 'ERR DBAPIError ... NoActiveSQLTransactionError:
  SAVEPOINT can only be used in transaction blocks', 'own': 'ERR ...'},
 'observer': 0}
```

**一句话：** 后继者的 `engine.begin()` 在被取消事务留下的连接上把 BEGIN
当成嵌套事务，发 SAVEPOINT 时失败，独立观察者读到 0；1.4.18 的修复让这条
连接被作废，幸存者用新连接成功提交，观察者读到 7。

日志：`logs/sqla-timeout-begin-1.4.{17,18}-r{1,2,3}-{B1,B2,B3,M1}.log`。

### 5.4 SQLAlchemy：纯 `Task.cancel()` 阶梯（1.4.17 vs 1.4.18）

预热引擎后 K=7。计划 `#7` 取消后幸存者等待池连接、测试超时失效（更正见 8.2），正式阶梯只跑
`#2`–`#6`，每层 15 个试验（5 个计划 × 3 次重复）。三轮数字相同；
`unknown=0`、`unrealized=0`。

| 版本 | B1 | B2 | B3 | M1 | 分诊 |
|---|---|---|---|---|---|
| 1.4.17 | 15/0 ×3 | 15/0 ×3 | 15/0 ×3 | **15/3 ×3** | 违反在 `pending#2`，**不是** #6592 的修复对象 |
| 1.4.18 | 15/0 ×3 | 15/0 ×3 | 15/0 ×3 | **15/3 ×3** | 同样在 `pending#2`，修复版仍有 |

1.4.17 和 1.4.18 的 M1 违反观察完全相同：

```
{'survivor': {'write': 'ok', 'own': 7}, 'observer': 0}
```

即幸存者的 `engine.begin()` 正常返回、同池读回 7，但独立观察者读到 0。
`pending#3`–`#6` 的所有计划都干净。**这是修复版 1.4.18 上仍然存在的现象，**
最初记入 `PRIVATE-NOTES.md`（P1），不计入 #6592 的修复效果；
后续补丁对照已确认这是另一个已知问题 #6652，详见 §5.6。

日志：`logs/sqla-ladder-1.4.{17,18}-r{1,2,3}-{B1,B2,B3,M1}.log`。

P1/P2 的最终分诊已同步到 §5.6；原始诊断笔记仍保留，不作为新的上游报告。

### 5.5 aiohttp 3.14.0 vs 3.14.1

三轮、每轮每层级 3 次重复，K=5。两个版本、四个层级全部：

| 版本 | 层级 | 计划/违反（R1, R2, R3） | unrealized | Unknown | 分诊 |
|---|---|---|---:|---:|---|
| 3.14.0 | B1 | 15/0, 15/0, 15/0 | 0 | 0 | 通过 |
| 3.14.0 | B2 | 15/0, 15/0, 15/0 | 0 | 0 | 通过（重试都返回 200） |
| 3.14.0 | B3 | 15/0, 15/0, 15/0 | 0 | 0 | 通过 |
| 3.14.0 | M1 | 15/0, 15/0, 15/0 | 0 | 0 | 通过（pre == post，退化观察） |
| 3.14.1 | B1 | 15/0, 15/0, 15/0 | 0 | 0 | 通过 |
| 3.14.1 | B2 | 15/0, 15/0, 15/0 | 0 | 0 | 通过 |
| 3.14.1 | B3 | 15/0, 15/0, 15/0 | 0 | 0 | 通过 |
| 3.14.1 | M1 | 15/0, 15/0, 15/0 | 0 | 0 | 通过（pre == post） |

M1 的实际观察在每轮都相同：

```
pre  = {'survivor': {'body': 'survivor', 'status': 200}, 'observer': 'survivor'}
post = 同上
```

取消点覆盖请求的 5 个挂起点；幸存者每次都拿到干净连接，独立观察者都看到
`survivor`。**3.14.0 和 3.14.1 之间没有任何可观察差别。**

**诊断插桩（test-side only）：** 每一轮对 `ResponseHandler.data_received`
做包装，记录进入时当前任务是否已有待处理的取消，以及从该回调逃逸的
`BaseException`。三轮合计：

| 版本 | data_received 调用 | 逃逸的 BaseException | 进入时任务正在取消 |
|---|---:|---:|---:|
| 3.14.0 | 777 | 0 | 0 |
| 3.14.1 | 777 | 0 | 0 |

因此 `CancelledError` **没有**在这个场景里进入 `data_received`；3.14.1 的
`except BaseException` 分支没有被走到。日志见
`logs/aiohttp-3.14.*-r*-{B1,B2,B3,M1}.log` 的 `instrument=` 行。

**补充诊断（不计入层级）：** `--mode idle` 用 `ClientTimeout(total=30,
sock_read=0.1)` 读完一个小响应，把连接放回池，空闲 150 ms 后再用同一个
session。两个版本都出现：

```
pooled_after_normal_read: read_timeout_handle_armed=true
pooled_after_idle: SocketTimeoutError('Timeout on reading data from socket'),
                   connected=true, should_close=true
survivor_1: SocketTimeoutError, 0.22–0.26 ms
survivor_2: 200 'fast', 0.63–0.69 ms
```

这正是 issue #12795 的 ~2 ms 症状，但它与取消无关，且 3.14.0/3.14.1 相同。
按上游，这是 PR #12954（“Don't re-arm read timeout on a connection returned
to the pool”，2026-06-18 合并）在 3.14.2 修的。日志见
`logs/aiohttp-3.14.*-r*-idle.log`。

### 5.6 后续 P1/P2 最终分诊（2026-10-19 交卷）

**P1 与 P2 是同一个已知问题 [SQLAlchemy #6652](https://github.com/sqlalchemy/sqlalchemy/issues/6652)，
不是新发现。** 上游修复为 `81cc682`（`greenlet_spawn` 捕获
`BaseException` 而非仅 `Exception`），发布在 1.4.19。
原 #6592 的 1.4.17/1.4.18 对比不是 #6652 的修复前后对比。

P1：取消绕过 greenlet 展开，事务状态未正确清理；幸存者实际走 SAVEPOINT，
同池读到 7，独立观察者读到 0。P2：COMMIT 取消后连接未归还池；
历史测试吞取消又放大成永久等待。两者不能重复计为两个独立缺陷。

以下是后续 **M1 分诊对照**，不是重新执行全部四层。每格为每轮 trials/违反；
所列汇总行 `Unknown=0`、`unrealized=0`：

| SQLAlchemy / 驱动 | Python | trials/违反 | 轮数 |
|---|---|---|---:|
| 1.4.18 / asyncpg 0.23.0 | 3.9.25 | 15/3（#2–#6，P1 在 #2） | 3 |
| 1.4.19 / asyncpg 0.23.0 | 3.9.25 | 15/0（#2–#6） | 3 |
| 1.4.18 + 上游一行补丁 / asyncpg | 3.9.25 | 15/0（#2–#6） | 3 |
| 2.0.55、2.1.5 / asyncpg 0.32.0（各版本） | 3.13.13 | 21/0（#1–#7） | 各 3 |
| 2.0.55、2.1.5 / psycopg 3.3.6 async（各版本） | 3.13.13 | 24/0（#1–#8） | 各 3 |
| 2.0.55、2.1.5 / asyncpg 0.32.0（各版本） | 3.12.13 | 21/0 | 各 1 |
| 2.1.5 回退一行 / asyncpg | 3.13.13 | #1：3/3；#6：3/3 | 各 1 |

依据：`logs/triage-summary.txt` 及其逐行引用的日志；环境见
`logs/environment-triage.txt`，补丁见 `patches/`。修复消除现象、回退又出现，
支持该取消清理路径的因果归属。psycopg 的回退测试未复现 P1 的可见性形状，
不能把 asyncpg 特有的 `_top_xact` 后果推广到所有驱动。

独立最小复现 `repro_6652.py` 不依赖原场景/引擎：1.4.18 的 begin 路径为
`own=7, observer=0`，commit 路径为池 `0 in / 1 out` 后硬超时；
1.4.19 和 2.1.5 两条路径均为 `own=7, observer=7`。
P2 的独立 checkout 诊断区分了池泄漏和测试侧吞取消，见 §8.2。

“当前版”仅指 **10 月 19 日测试的上述版本**，不是持续跟踪结论。校准
pre/post 在这些当前版场景中退化，零违反只表示所测观察未被污染，不证明完整
取消安全。cursor-hook、重复取消及回退版 suspended greenlet 的长期影响未验证。
没有新的上游报告需要提交；临时 Python 环境和 Postgres 容器交卷后已删除。

## 6. 分诊与“后继者看到了什么”

### 6.1 汇总

| 案例 | 修复前 | 修复后 | 哪些层级抓到 | 分诊 |
|---|---|---|---|---|
| redis R1 `BLPOP` | 4.5.3：M1 3/12 每轮 | 4.5.4：0/12 | **只有 M1**；B1/B2/B3 全 0 | 确认的问题（上游 #2665 / CVE-2023-28859，已知） |
| redis R2 `pipeline` | 4.5.2：全 0 | 4.5.3：全 0 | 无 | 未复现；不能据此评价 #2641 |
| SQLAlchemy timeout-BEGIN | 1.4.17：B2/B3/M1 各 3/3 每轮 | 1.4.18：全 0 | **B2/B3/M1**；B1 全 0 | 确认的问题（上游 #6592，已知） |
| SQLAlchemy 纯取消 | 1.4.17：M1 3/15 每轮 | 1.4.18：M1 3/15 每轮 | 只有 M1；B1/B2/B3 全 0 | 已知 #6652（P1）；1.4.18 不是此问题的修复版，1.4.19 / 补丁对照见 §5.6 |
| aiohttp | 3.14.0：四层全 0 | 3.14.1：四层全 0 | 无 | 修复点不在取消路径；issue 的真实症状是 #12954 |

### 6.2 修复前抓到、修复后消失的两处

1. **redis R1：** 被取消的 `BLPOP` 的 nil 回复仍留在连接上，幸存者对同一个
   list key 的 `RPUSH` 把它当成自己的返回值（`False`，应为 1）；写本身生效，
   同池读和独立观察者都看到 `['survivor']`。
2. **SQLAlchemy timeout-BEGIN：** 被取消事务的连接带着 asyncpg 的
   `_top_xact` 回到池里，幸存者的 `engine.begin()` 把它当嵌套事务，发
   `SAVEPOINT` 时报 `NoActiveSQLTransactionError`，独立观察者读到 0；
   1.4.18 作废该连接，幸存者用新连接成功写入 7。

### 6.3 修复前没抓到的原因

**redis R2 pipeline：**
- K=4，计划 #1/#2 取消在命令发出前后，#3 在每次单独运行时目标已经完成
  （`unrealized=3/12`），#4 取消在事务回复/清理阶段。
- pipeline 的 MULTI/SET/GET/EXEC 回复经常在同一个 TCP 段里到达，客户端在
  一次唤醒里把多条回复读完；等到 #4 真正取消时，已经没有可被幸存者读到的
  旧回复。`ConnectionPool.get_connection()` 的非破坏性 peek 又会在残留数据
  已到缓冲时直接重连。两者都把窗口关掉。
- 这是**挂起点枚举不到 + 观察不到**，不是“修复后没有差别”的证据。

**SQLAlchemy 纯取消：**
- #6592 的 1.4.18 修复只把 `asyncio.TimeoutError` 加入
  `is_exit_exception`；在 Python 3.8+，`CancelledError` 本来就是
  `BaseException`，1.4.17 已经把它当 exit exception（如果它能到达
  `_handle_dbapi_exception`）。
- 但正常 async 取消是在 `greenlet_spawn()` 里抛出的，`greenlet_spawn` 只
  catch `Exception`，`CancelledError` 绕过 SQLAlchemy 的同步异常处理；
  所以纯取消不是这条修复覆盖的入口。真正被 1.4.18 修复的是
  timeout-BEGIN（5.3）。
- 纯取消在 `pending#2` 暴露的是 #6652 的 P1；`pending#7` 是同一根因的
  池连接泄漏 P2，原测试吞取消导致超时无法结束（§8.2）。

**aiohttp：**
- `ResponseHandler.data_received` 是同步回调，取消只在 await 点交付；
  场景里 777 次 `data_received` 调用中，逃逸的 `BaseException` 为 0，
  进入时任务正在取消的次数为 0。3.14.1 的 `except BaseException` 分支
  没有机会执行。
- 两版的 `client.py` 完全相同；`client_reqrep.py` 只多了 `server_hostname`
  字段，`streams.py` 只有 `read_nowait` 的 drain 改动；取消清理路径
  （`client.py` 的 `except BaseException: conn.close()`、`client_reqrep.py`
  的 `read`/`release`）没有变化。正常取消都会关闭 transport，幸存者拿到
  新连接。
- issue #12795 的 ~2 ms 症状用 `sock_read` 定时器可以稳定复现，但它是
  idle 池化连接上的 #12954，3.14.0/3.14.1 相同，3.14.2 才修。

## 7. 和 Rust 版的差异

| 部分 | Rust / Dropwise | Python asyncio | 能否直接照搬 |
|---|---|---|---|
| 枚举打断点 | 目标 future 每次 `Pending` 是一个点；计数来自运行时 poll | 目标任务每次让出事件循环（`Task.__step` 未完成）是一个点；计数来自运行时 step | 原则可以；机制必须重写。计的是库内部 await，不是源码 `.await` |
| 取消 | 丢弃（drop）目标 future | `Task.cancel()` 向任务注入 `CancelledError`；`shield`/`uncancel` 改变清理行为 | 不能直接照搬；Python 的取消可被库拦截/延迟，Rust 的 drop 是同步的 |
| 清理 / settle | `Drop` 同步；settle 让被取消工作跑完 | `finally`/`except` 是异步的；把目标 Task await 到结束；没有固定 sleep | 原则可以；固定 settle 会改变 Redis 的 peek 窗口，不能照搬数值 |
| 幸存者 | 同一个池、同一个对象、有副作用 | 同左 | 可以 |
| 独立观察者 | 单独连接/服务端 API | 同左 | 可以 |
| 两条基线 | pre/post，各两次，不一致 Unknown | 同左 | **完全照搬** |
| 判定 | `Expect::Atomic`、`Pre/Post/Mixed/Neither/Unknown` | 同左（本轮只实现 Atomic） | **完全照搬** |
| 错误分类 | `Result`/panic/obligation | `Exception` vs `BaseException`；`CancelledError` 是后者 | 需要按语言重写；SQLAlchemy 的 greenlet 桥是 Python 特有 |
| 超时 | 取消 + 可选 timeout | `asyncio.TimeoutError` 在 3.11 前是 `Exception`，能进入普通异常处理；`CancelledError` 不能 | 不能当成同一种打断 |
| 池的自愈 | 通用池通常没有 socket peek | redis-py 的 `can_read_destructive()` 会主动重连，可能掩盖残留 | 需要按池实现调整场景 |

**结论：** 判定的一半（幸存者、独立观察者、pre/post 校准、`Neither`）是
语言无关的，可以直接照搬；打断的一半是运行时相关的。Python 的取消只在
await 点发生，所以“每个挂起点”只等于“每个 await 让出点”，不等于每个
源码位置，也不包括同步回调里的状态转移。方法不是 Rust 专属，但不能说成
“任意语言都同样有效”；准确说法是 **“对把取消实现为 await 点注入的运行时，
判定模板可移植；枚举机制和清理/settle 必须按运行时重写”**。

## 8. 局限与没做到的部分

### 8.1 挂起点数量不是稳定的

`BLPOP` 的 K=4，但 `pending#3` 在每次单独运行时目标已经完成
（`unrealized=3/12`）；pipeline 同样。SQLAlchemy 预热后 K=7，`pending#7`
会让幸存者等待泄漏的池连接，原测试又吞掉超时取消。计划数会随真实调度/网络变化，不能像 Rust 的
`CurrentThread` 那样保证每个计划都 realized。按 PLAN 第 4 节，本报告只在
同一轮内比较，并把 unrealized 单列。

### 8.2 SQLAlchemy `pending#7`：池连接泄漏与测试侧吞取消

后续诊断推翻了最初的“同步 greenlet 死锁、事件循环定时器不运行”解释：
1 秒 ticker 持续运行，未吞异常的 checkout 在约 3 秒正常抛 `TimeoutError`；
池状态仍为 `0 in / 1 checked out`。库侧是 #6652 导致 COMMIT 取消后未归还连接。

原 `cases/sqla_case.py` 的 `survivor()` 用 `except BaseException` 捕获了
超时注入的 `CancelledError`，随后又尝试从同一个耗尽的池取连接，导致外层
`wait_for` 等不到任务终止。它是测试代码放大，不是事件循环停止的证据。
本次文档收尾未改历史场景、未重跑；原正式阶梯仍只覆盖 #2–#6，不能事后补记 #7。
后续独立 checkout、最小复现与修复对照见 §5.6 和 `logs/`。

未来场景不得吞掉 runner 的取消后继续工作；须保留外部进程硬截止，不能只依赖
`wait_for`。历史 runner 仅用于重现原实验，不可未经修正直接当作 Phase 4 模板。

### 8.3 同步回调里的状态转移枚举不到

aiohttp 的修复点在 `data_received`，而 `CancelledError` 只在 await 点交付；
本方法无法在同步回调中间注入取消。这不是观察者不灵敏，而是打断模型覆盖
不到。

### 8.4 池自己的检查会掩盖残留

redis-py 的 `get_connection()` 在借出前用 `can_read_destructive()` 检查
残留并重连；如果旧回复已经到达，幸存者会拿到干净连接。为了让 BLPOP 的
旧回复还没到达，本场景依靠服务端阻塞（BLPOP timeout），不加额外 settle。
pipeline 的批量回复正好落在被掩盖的一侧。

### 8.5 Python 版本

- 本机装不到 Python 3.7；`CancelledError` 在 3.7 是 `Exception`，1.4.18 的
  新增条目在 3.7 上可能有行为差异。本轮是 3.9。
- aiohttp 的 `Task.cancelling()`/`uncancel()` 在 3.11+ 才有；本轮是 3.13，
  取消语义按 3.13 解释。
- `asyncio.TimeoutError` 在 3.11 起是 `TimeoutError` 的内建别名；本轮
  SQLAlchemy/timeout 用 3.9 跑，和 issue 的年代一致。

### 8.6 没做的可选案例

`MagicStack/asyncpg#537` 和 `aio-libs/aiohttp#3281` 没有做。原因：
- aiohttp #12795 已经证明指定修复不在取消路径上；再做 #3281 需要另一套
  “后台写 chunk 任务”场景，时间不够。
- asyncpg #537 的“取消过程中再出错”需要精确控制第二次错误，和 SQLAlchemy
  案例的 greenlet 问题重叠；本轮优先保证三个主案例。

### 8.7 timeout-BEGIN 是注入，不是自然超时

SQLAlchemy 1.4 没有把 per-query `timeout=` 传到 asyncpg 的执行选项；
asyncpg 又拒绝 `command_timeout<=0`。所以 5.3 用 issue 自己的方法：运行时
把 `Connection.execute` 对 BEGIN 的 timeout 设为 0（只在目标期间，进程内，
不改文件）。这能证明修复的判定，但不能算作“自然发生的超时”。

### 8.8 其他

- 没有做盲法；版本、场景、分诊由同一执行者完成。
- 每个案例只用了一个服务端版本（Redis 8.8.0、PostgreSQL 18.4）和一个
  客户端版本对。
- 没有评估连接复用率、性能或生产影响。
- 没有在 GitHub/PyPI 发任何东西；没有改 `src/`；没有 commit/push。

## 9. 原正式回溯的成本

以下为首次交卷的估计，不包含 §5.6 后续分诊；后续分诊没有完整独立工时统计，
不把原先的 1.9 小时当作全部工作的成本。

### 9.1 代码行数

| 文件 | 行数（含注释/空行） | 说明 |
|---|---:|---|
| `dropwise_py/engine.py` | 458 | 挂起点计数、取消、pre/post、B1/B2/B3/M1、报告 |
| `dropwise_py/__init__.py` | 35 | 导出 |
| `dropwise_py/selftest.py` | 28 | 引擎自测，不计入案例 |
| `cases/common.py` | 117 | 参数、延迟 TCP 代理、汇总 |
| `cases/redis_case.py` | 184 | R1/R2 两个目标、幸存者、观察者 |
| `cases/sqla_case.py` | 353 | 取消阶梯 + timeout-BEGIN 注入 |
| `cases/aiohttp_case.py` | 258 | 进程内服务器、四个层级、idle 诊断 |
| `run-redis.sh` / `run-postgres.sh` / `run-aiohttp.sh` | 63 / 93 / 34 | 容器、日志、轮次 |
| **合计** | **1623** | 其中引擎 458（不含自测） |

按层级拆分的场景代码没有单独统计：四个层级共用同一个 `Scenario` 类和
同一套 engine 调度，只有 `survivor`/`same`/`different` 是层级差异；
B1/B2/B3 的调度代码在 engine 里，案例侧主要是环境代码。这一点和
neo4rs 的 Rust 场景（每个层级一个函数、按函数体计数）不同。

### 9.2 工时

约 **1.9 人时**（墙钟 13:40–15:35，含写本文件；未精确计时）。大致分布：

| 花在哪 | 比例 |
|---|---:|
| 读 issue、修复 diff、核对 PyPI 版本/日期 | ~15% |
| 写引擎（Task step 计数、pre/post、B1–B3/M1） | ~20% |
| redis 场景、延迟代理、BLPOP/pipeline 调试 | ~20% |
| SQLAlchemy 复现、greenlet 卡住、timeout-BEGIN 注入 | ~30% |
| aiohttp 场景与诊断 | ~10% |
| 整理结果与写文档 | ~5% |

按案例拆分的工时（同一执行者，包含该案例的预实验/调试，不含共享引擎的
设计时间；总计约 1.9 人时）：

| 案例 | 工时（约） | 主要花在哪 |
|---|---:|---|
| redis R1/R2 | 0.6 h | BLPOP 服务端阻塞窗口、延迟代理、pipeline 类型错误返工 |
| SQLAlchemy timeout-BEGIN/纯取消 | 0.8 h | 复现入口（BEGIN）、greenlet 卡住、计划子集、注入对照 |
| aiohttp | 0.3 h | 进程内服务器、取消矩阵、`data_received` 插桩、idle 诊断 |
| 共享引擎/runner/文档 | 0.2 h | Task step 计数、pre/post、容器 runner、写结果 |

正式运行共产生 126 个每轮每层日志（redis 48、aiohttp 30、SQLAlchemy 48），
另有预实验日志未计入。所有最终数字都来自 `logs/`。

## 10. 文件

| 文件 | 内容 |
|---|---|
| `dropwise_py/engine.py` | Python 版判定引擎；`Task.__step` 计数、`Task.cancel()`、pre/post、B1/B2/B3/M1 |
| `dropwise_py/__init__.py`, `selftest.py` | 导出与自测 |
| `cases/common.py` | 参数解析、延迟 TCP 代理 |
| `cases/redis_case.py` | R1 BLPOP、R2 pipeline |
| `cases/sqla_case.py` | SQLAlchemy 取消阶梯、timeout-BEGIN 注入 |
| `cases/aiohttp_case.py` | 进程内 aiohttp 服务器、四层、idle 诊断、`data_received` 插桩 |
| `run-redis.sh`, `run-postgres.sh`, `run-aiohttp.sh` | 起容器/等待/跑轮次/写日志/删容器 |
| `logs/` | 每轮每层的原始输出、`environment.txt`、runner 控制台输出 |
| `RESULTS.md` | 本文件 |
| `PRIVATE-NOTES.md` | P1/P2 已完成的原始诊断笔记；最终结论见 §5.6，不对外发布该笔记 |

## 11. 与任务设计的偏离 / 未核实

1. **redis 的主版本对是 4.5.3 → 4.5.4，不是 4.5.2 → 4.5.3。** #2641 的
   diff 只改 `Pipeline.execute`；issue #2624 的复现是普通 `GET`，
   4.5.3 仍漏洞（#2665 明确说 #2641 只修 pipeline）。因此 R1 用
   4.5.3/4.5.4（CVE-2023-28859），R2 单独测 4.5.2/4.5.3（#2641）。
2. **SQLAlchemy 的纯取消阶梯只枚举 #2–#6**，不是全部 7 个；#7 存在池连接泄漏及测试侧吞取消。
   这是原正式阶梯的覆盖缺口；后续分诊已解释原因，但未补跑原完整四层。
3. **SQLAlchemy 的 timeout-BEGIN 是注入**，不是自然超时；它是 issue 自己的
   复现方式，见 8.7。
4. **aiohttp 是负结果。** 指定修复（3.14.1）与取消场景无关；issue 的真实
   症状是 #12954（3.14.2）。本轮没有安装 3.14.2 做动态对照，只根据上游
   PR 和 agent 的预实验记录；这一条标为“依赖上游/预实验，未在正式三轮中
   复跑 3.14.2”。
5. **没有 Python 3.7。** 因此不能动态验证 1.4.18 对 `CancelledError` 的
   3.7 差异；只做源码/语言事实说明。
6. **可选案例 asyncpg#537 / aiohttp#3281 跳过**，原因见 8.6。
7. 版本与修复归属均以本机安装的 PyPI 源码、GitHub commit/PR diff、PyPI
   release 日期和 NVD 为准；上游 issue 的“内容”只读到正文/评论，未逐条
   核对所有关联 PR 的合并状态。
8. 没有发 issue/PR/评论，没有修改 Rust `src/`，没有 commit/push。


### 11.1 预期与实际的差异

- **R1** 与预期一致：4.5.3 的 M1 在最后一个挂起点违反，4.5.4 干净；
  低层级全盲。
- **R2** 的预期是“如果停在 EXEC 回复未读的窗口就会违反”；实际没有复现，
  计划 #3 经常在取消前完成，#4 落在清理阶段。按 6.3 归为“枚举不到/被
  批量回复掩盖”，不是修复有效的证据。
- **S 纯取消** 的预期是“四层都不违反（因为 1.4.17 已把 CancelledError 当
  exit exception）”；实际 M1 在 `pending#2` 违反，且 1.4.18 同样违反。
  这超出 #6592 预期，后续已归属 #6652，不能算新发现（§5.6）。
- **S timeout-BEGIN** 与预期一致：1.4.17 的 B2/B3/M1 各 3/3，1.4.18 全 0；
  B1 盲。
- **aiohttp** 与实验前预期一致：两版四层都干净；指定修复没有可观察差别，
  issue 的真实症状由 idle 诊断复现。
