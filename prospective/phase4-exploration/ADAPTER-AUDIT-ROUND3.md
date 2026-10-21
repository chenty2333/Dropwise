# 第三轮适配器错误吞噬审计（二次校正）

状态：2026-10-22 审查收尾，探索记录；不是 Phase 4 数据。数字与
[ROUND3-RESULTS.md](ROUND3-RESULTS.md) 一起看；总报告是统一后的当前结论。

## 1. 结论

第一轮的 B2/B3 表里，**aiomysql、psycopg、asyncpg 的 0 违规不能作为
“只有 M1 能看到”的证据**。原因是 case 的 `_tx_write()` 调用了 `bound()`，
但没有检查返回值：失败阶段被记录成普通 dict（psycopg）或直接返回 `None`
（aiomysql/asyncpg），`level_retry` 只看到“没有异常字符串”，于是把失败当
成功。修复后：

- **aiomysql direct**：B2/B3/M1 都变成显式 `InterfaceError`。这不是静默
  污染，而是 **fail-fast / 要求换连接**；在契约未承诺自动 reset 时，不应
  计为协议一致性缺陷。pool 读到了失效连接并丢弃/重连，是 clean self-heal。
- **mysql-connector direct**：B2/B3 是结构化 fail-fast，但 M1 显示
  **真实静默后果**：6/12 后继写报告成功却只拿到 target 的 stale reply，
  3/12 已提交但报告失败，3/12 反序列化错位。它是**候选客户端缺陷**。
- **psycopg / asyncpg**：旧 B2/B3 0 不可用；修复后重跑的 B2 在 psycopg
  direct 仍为 0（`same()` 确实成功，不是被吞），M1 才是语义判据；
  asyncpg 重跑见 §6。
- 统一接口：`research/python-retro/dropwise_py/engine.py` 新增
  `OpResult(ok, stage, value, error)` 与 `operation_failed()`；
  新适配器按结构化的 `ok`（或显式 `{"ok": false}`）判失败；历史适配器的
  `"ERR "` 字符串判据仍保留。无服务回归测试
  `research/python-retro/tests/test_ladder_outcomes.py` 14/14 通过：已实现 cut 后的
  后继失败（结构化、抛异常或超时）可被阶梯检出；setup 失败直接抛出，
  target baseline 或 post 校准失败不计取消发现，未实现 cut 不判违反。

## 2. 逐包审计

| 包 | 旧 B2/B3 0 是否可信 | 现实现 | 修复后结果 | 分类 |
|---|---|---|---|---|
| aiomysql 0.3.2 direct | **否**：`_tx_write` 可返回 None，失败阶段被当完成 | `target` 失败抛出；`same/different` 返回 `OpResult`；`close()` 同步调用 | B1 0/9；B2 9/9、B3 9/9 显式 `InterfaceError`；M1 9/9 Neither（survivor 也报错、observer 空） | **fail-fast / 要求重连，不是静默缺陷**；不计协议根因 |
| aiomysql pool | 旧数字不可用（适配器同一 bug） | 同上；pool release 丢弃 | B1/B2/B3/M1 全 0/12；M1 Pre 6/Post 6；cid 300→301 等，survivor `ok 1`、own `[[7]]` | **clean self-heal 正例** |
| mysql-connector 26.7.0 direct | 旧未跑；新 case 一开始就结构化 | `target` 失败抛出；`same/different` `OpResult` | B1 0/12；B2 12/12、B3 12/12 显式 fail-fast；M1 12/12 Neither：6 次外部不可见、3 次已提交但返回失败、3 次错位 | **候选客户端缺陷**（M1 的静默后果），不是单纯 fail-fast |
| psycopg 3.3.6 direct | **旧 0 不可靠**：`_tx_write` 只把 stage 结果放进 dict，B2 看到 dict 就放行；但修复后重跑仍 0，因为 `same()` 确实成功 | 已修复为 `OpResult`；`same` 成功返回 `ok`，失败阶段结构化 | 新 B1 0/18、B2 0/18、B3 0/18、M1 **12/18**（6 Post，12 Neither）；B2/B3 的 value 里可见 `status_after_acquire=INTRANS`，但 level 只看逃逸错误 | 手写事务未回滚的应用组合现象；完整观察补充简单错误检测，但未证明驱动违约 |
| psycopg pool | 旧 0 未证明是“看到了 rollback”还是“没看到错误” | 已修复为 `OpResult` | 新 B1/B2/B3/M1 全 0/18；M1 Pre 12/Post 6；release rollback INTRANS→IDLE | clean/self-heal 正例（重跑确认） |
| asyncpg 0.32.0 direct | **否**：`_tx_write` 返回 None，失败阶段被当完成 | 已修复为 `OpResult`；有效 run `run.1HLY38` | 新 B1-B3 0/9、M1 9/9；6 次后继效果对独立连接不可见，3 次仅 acquire-time 状态差异；不是旧 None 假阴性 | 手写事务恢复待分诊；状态差异不能单独证明驱动错误，外部不可见不等于永久丢写 |
| asyncpg pool | 旧 0 同上有风险 | 已修复；第一次重跑因“released connection 上调用 is_in_transaction()”产生 B1 9/12、B2 12/12 假阳性 + baseline error，该 run 无效；修复后 `run.1HLY38` 有效 | 新 B1/B2/B3/M1 全 0/12；M1 Pre 6/Post 6；最终 `run.WcLcOp` 24/24 pool 对照无 timeout（旧对照的 timeout 是 server sample 早于 release 的伪影） | 本阶梯 clean；不写成普遍保证 |
| redis-py 8.1.0 | 旧 run 被适配器变更跨过；新 run 结构化 | `target` 抛；`same/different` `OpResult`；`selftest_adapter.py` | pool/direct × tx/set 120 trials，全 0；M1 Pre/Post 合法；DISCARD/UNWATCH/reconnect 对照 10/10 | 阴性；pool/direct 都 clean（本场景） |
| pymongo 4.19.0 | 旧 run 是 dict，新 run 结构化 | `op_from()` `OpResult`；`test_structural.py` 7/7 | 2 units 72 trials 全 0；M1 Pre/Post 合法；cancel 后 socket discarded/reconnect | 阴性；self-heal 正例 |
| neo4j 6.4.1 | 同上 | 同上 | 2 units 72 trials 全 0；M1 Pre/Post；连接归还并复用 | 阴性；self-heal 正例 |
| aiohttp 3.14.5 | 0 可信（无阶段被吞） | `bounded` 错误以字符串返回；setup 现检查 warm/health 并抛出 | B1-B3 0/3、M1 0/3（Post）；yields=1，只覆盖已经写出的 POST | 弱阴性；覆盖不足 |
| curl-cffi 0.16.3 | 0 可信 | 同上；setup 检查 | B1-B3 0/3、M1 0/3（Pre）；yields=1，cut 在 libcurl 发送前 | 弱阴性；mid-flight probe 3/3 Post 但不属于阶梯 |
| httpx 0.28.1 | 0 部分可信（旧 case 把 bounded 错误转字符串） | 未改 | 已知上游 #2821/httpcore #726；3/18 | 已知上游实例 |
| aiosmtplib 5.1.3 | 0 可信（B2 抓到 `SMTPDataError`） | 未改 | 12/12 同 SMTP 响应关联族 | 已知族新实例 |

## 3. Rust 侧（不是 Python 适配器问题，但一起校正）

- **skytable 0.8.12**：深入批次原版 pooled 27/48、direct 15/24；实验修复
  pooled 0/48、direct 18/24 为显式拒绝。契约未明确、上游未知，保持候选分级。
- **tokio-modbus 0.17.0**：B2 2/2、B3 2/2、M1 2/2 Neither。取消的写已到
  server（`before=[100,1]`），延迟 reply 留在 client；下一条请求读错
  txid；survivor 写也在 server 生效（`visible=[100,7]`）但 client 报错。
  **可复现的响应错配候选；取消契约和上游状态仍待核实。**
- **async-pop 1.1.3**：B2 2/8、B3 6/8；M1 每 rep 有 2 个 retr-target 明确失败（pre/post 均成功），6 个 login-target
  的认证部分完成契约尚未确定，明确记为
  **unresolved，不计已确认缺陷**。GreenMail 的 hang 是 async-pop 解码器
  `decode_needs` 不复位的独立 bug，和取消无关；改用允许的 loopback POP3
  server。
- **orientdb 0.6.0**：0/120 主阶梯，但实测 2 条物理 TCP 流，弱阴性。
- **gel-tokio 0.11.0**：已有 96-trial 四级阶梯，零违反；仅 pooled/single 模式。

## 4. 旧日志与有效性

保留、不覆盖：

- `python/pg-mysql/logs/run.KqRbB4`：旧 aiomysql/psycopg/asyncpg。其中
  aiomysql B2/B3 0 无效；psycopg/asyncpg B2/B3 0 不得作为 M1 对比证据；
  M1 的具体值仍可作现象记录，但需与重跑一起读。
- `python/pg-mysql/logs/run.Pio5vb`：aiomysql pool/mysql-connector
  预修复；B2/B3 的 0/12 不作为结论。
- `python/pg-mysql/logs/run.ya23Xb`：aiomysql direct 修复后但 `close()`
  有 `CLOSE_ERROR`；数字可用于 fail-fast 现象，cleanup 不作为证据。
- `python/pg-mysql/logs/run.GWTJsc`：aiomysql pool/direct + mysql-connector
  的最终 3b 运行；无 CLOSE_ERROR。
- `python/pg-mysql/logs/run.nfnJcD`：psycopg 修复后；补齐 B3/M1 后作为
  §6 依据。
- `python/mongo-neo4j/logs/run.ghhvJH`、`run.VIRLyE`：旧 dict 适配器；
  标记无效；有效为 `run.Qhri8J`、`run.xu9AhN`。
- `python/redis/logs/run.d72cdb`：旧适配器；有效为 `run.b4e53b`。

## 5. 方法含义

1. **“B2/B3 0”只有在 case 保证失败阶段会逃逸或结构化时才可读。**
   让 case 返回 `None`、吞掉 `bound()` 结果，会把“连接已经坏了”伪装成
   “重试成功”。这是**检测层假阴性**，不是“取消没有后果”。
2. **“违规”与“合法拒绝复用”必须分开。**
   aiomysql direct 的 `InterfaceError` 是显式 fail-fast：实际状态没有与
   “成功承诺”冲突，只是调用方必须换连接。把它计成协议缺陷会夸大共性；
   应同时注明该 handle 已失效、调用方需重连。是否存在文档缺口另行核实，
   不能从 fail-fast 本身推导出 contract gap。
3. **完整观察提供了简单错误检测没有的信息，但不是普遍优越性定理。**
   mysql-connector 的 B2/B3 是 fail-fast，完整观察还看到 6/12 外部不可见与
   3/12 已提交但返回失败；psycopg 的 B2 0 但 M1 可能阳性；aiomysql 的 B2/B3 全失败但
   **本场景未见静默污染**。B1/B2/B3/M1 不单调，必须按“失败是否逃逸、失败是否
   一致、后继是否真的发生”分层读。
4. **状态字段既可能抓漏，也可能误判。**
   asyncpg #3 的 value-only 是合法 Post，加入 acquire-time `in_tx` 才变 Neither；
   要单独报“stale-state-only”，不要把它和“丢写”合并。
5. **诊断代码也会制造假阳性。**
   asyncpg pool 的第一次修复后重跑在 cancellation 后调用
   `Connection.is_in_transaction()`，但 pool 已把连接 release，得到
   `InterfaceError: cannot call ... released back to the pool`；B1 因此
   出现 9/12 “target error”，B2 12/12，且 uncancelled target 也
   `baseline_error`。这不是 asyncpg 的协议后果，是**诊断在错误时机读已释放
   对象**。规则：诊断失败不能变成 target 失败/realized cut；baseline error
   出现时该 level 作废，不能计入违规。修复后重跑前，不把该 run 写进结论。
6. **每个 batch 至少一个 no-service 测试。**
   现在的 `test_ladder_outcomes.py`、`selftest_adapter.py`、
   `test_structural.py`、`test_adapter_contract.py` 检查已实现 cut 后的失败判据，
   以及成功不能被误杀。无效 baseline 和未实现 cut 不得计取消违反。

## 6. 本轮已补 / 仍未覆盖

已补：

- asyncpg direct/pool 修复后的完整表：`run.1HLY38`；direct B1-B3 0/9、
  M1 9/9；pool 全 0/12；第一次“released connection”诊断作废（见 §5）。
- psycopg/asyncpg 的事务上下文、显式 rollback/reset、close/reconnect 对照：
  所测切点的 rollback 和重连恢复了可见性。已有外层事务时，psycopg 上下文
  可只管理 savepoint，不能把外层未提交归咎于上下文假成功；asyncpg 可显式拒绝
  嵌套手写事务。asyncpg k1 的 `reset()` 未恢复可见性；“BEGIN 仍排队”是解释，
  不是异步采样已直接证明的内部时序。官方上下文覆盖 target 的未恢复切点需另行定位。
  见 PG/MySQL 报告和主报告的事务恢复小节。
- skytable 契约/触发链/修复对照：`clients/skytable/RESULTS-FRAGMENT-DEEP.md`；
  pooled 原版 27/48、修复 0/48；direct 原版 15/24、修复 18/24 显式
  fail-closed；contract silent、upstream unknown。
- Linux multicast 真实投递与 patch audit：
  `research/linux/RESULTS-round4-multicast.md` 与
  `research/linux/logs/20261021-r4-mcast-delivery/`。

仍未覆盖（下一轮）：

- mysql-connector 的最小修复/原版对照，或 upstream 检索的正式结论；
- asyncpg/psycopg 的多 cut 点、多 pool 大小、并发 borrower；
- skytable 的真实服务 pipeline、TLS、multi-thread 路径和 pool 大小 >1；
  本次已补 pipeline 共享 guard 和无服务回归，原始服务日志仍是 query-only。
- Linux 的 veth/NIC、IGMP 抓包、wrapper/all-hosts 注入。

## 7. 本次复核补充

- Python 引擎将返回 `OpResult(ok=False)` 的 target 当作失败，B1/B2/B3 对失败 baseline
  不再继续输出可误读的试验。M1 会验证每次 post 校准的 target，未实现 cut 不判违反。
- runner 现在传播子命令失败；结束标志不再代替完整性检查。原始正常完成日志不改写，
  旧无效日志也不重新包装成新版本验证。
- MongoDB 与 Neo4j **各 72 trials、合计 144**。POP 的“2 clean”曾被误译成阴性，
  实际是每 rep 2 个校准干净的失败现象。
