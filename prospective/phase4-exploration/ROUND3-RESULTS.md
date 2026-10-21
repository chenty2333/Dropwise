# 第三轮结果：共性作为“发现启发式”的检验

状态：2026-10-21，未冻结探索；不是 Phase 4 数据，不改写 Phase 4 表。
**二次校正：§12 与 [ADAPTER-AUDIT-ROUND3.md](ADAPTER-AUDIT-ROUND3.md) 取代前文冲突数字；旧日志保留但标注无效/语义盲。**
问题：我们总结出的共性，能否**事先**指导未知问题的发现，而不仅仅解释已经知道的案例？

本文件是主报告；预测原文在
[ROUND3-PREDICTIONS.md](ROUND3-PREDICTIONS.md)，硬规则在
[ROUND3-BRIEF.md](ROUND3-BRIEF.md)。所有结论按“确认 / 已知 / 契约外 /
阴性 / 未覆盖”分开，不把零结果写成安全证明。

---

## 0. 直接回答（先给结论，后给证据）

**能，但只能到“指导往哪看、怎么切、观察什么”这一层；不能到“预测具体缺陷”这一层。**

- 共性告诉我们：在**会话建立 / 检出 / 第一次写之后**下刀，用**后继者的具体返回值 +
  独立观察者 + 协议/资源状态**去判定，而不是只看 `Ok`。这一轮它确实把
  我们带到了五个新的事实：
  1. **三个 direct handle 家族**：psycopg direct **M1 12/18**、asyncpg direct
     **M1 9/9**、aiomysql direct **M1 9/9**，三者的 B1/B2/B3 都是 0。后继
     `write=Ok`、`own=7`，独立观察者却看到空表；asyncpg 的
     `is_in_transaction()` 在 pending#1 还报告 `False`（BEGIN 还在客户端
     队列里），aiomysql 则是 sticky `InterfaceError`（fail-fast）。这是
     “返回值/状态报告与实际效果不一致”的干净样本，且只有 M1 能看到。
  2. **skytable**：pool + direct，**35/72 ladder plans**、generated **84/120**
     违规；pooled `#0@1:Immediate` 的 survivor `write=Ok`、`own=Err("row not
     found")`、`visible=Ok(7)`，server row 正确；pool 仍返回同一条 desynced
     连接（`connections_created=1`）。这是**预测命中**（第 7 节）的 Rust
     新客户端实例，仍是 response-association 族。
  3. **aiosmtplib**：B1 0/12，B2/B3/M1 各 12/12；取消后的 DATA 留下一个
     `250`，下一次 DATA 把它当成“不是 354”，抛 `SMTPDataError: (250,'OK')`；
     3/12 直接 `TimeoutError`，NOOP 也卡住。它与第二轮 async-smtp 同族，
     是新客户端实例，不是新机制类别；但没有找到 aiosmtplib 的上游报告。
  4. **httpx**：B1 0/18，B2/B3/M1 各 3/18；取消后连接停在 `ACTIVE`，
     `max_connections=1` 时同一 client 的 retry 根本不到服务器，2 s 后
     `TimeoutError`；`max_connections=2` 时新连接可用，旧连接仍卡住。
     这是**已知上游问题**（httpx #2821 / httpcore #726），不是本轮新发现，
     但它是共性跨家族迁移的确定性复现；源码定位到
     `httpcore.PoolByteStream.aclose()` 的 shield 窗口。
  5. **Linux IPv4 multicast join**：`setsockopt(IP_ADD_MEMBERSHIP)` 返回 0，
     设备层组却没加入；v6.19 与 v7.3-rc2 都可复现，5/5 rep 稳定。
     这是**新的独立根因类别**（`____ip_mc_inc_group()` 的 `kzalloc` 失败被
     `void` 辅助函数吞掉），而且**与取消无关**——它是纯 success-without-effect。
- 共性**没有**告诉我们具体会在哪个函数、哪一行、哪个分配点出问题。
  Linux 的这个新 bug 不是预测命中的，是**按“成功但没效果 / 可失败子步骤被忽略”
  这个广义形状做源码审计**找到的；同一批预测里的 mmap/mremap/mprotect 虽然
  行为命中，却属于契约外；orientdb 预测“likely violation”但 0/120 主阶梯计划；
  skytable 在**客户端家族**这一层被预测到并命中（35/72），但没有预测到具体是
  bb8 validation 消费 stale reply。**家族级预测有命中；函数级预测命中率不高。**
- 因此更准确的说法是：共性是一个**契约设计原则 + 搜索启发式 + 判定 oracle**，
  不是一个“缺陷位置预测器”。它最有价值的地方不是“猜中下一个 bug”，而是
  让我们**不再把 `Ok` 当正确**，并且知道要把哪些状态放进观测。

---

## 1. 本轮覆盖了什么

### 1.1 客户端（Rust）

| family | 版本 / 服务 | 模式 | 级别 | 结果 | 状态 |
|---|---|---|---|---|---|
| orientdb-client | 0.6.0 / mobc 0.7.3；orientdb 3.0.23 | pool `max_size=1` | B1/B2/B3/M1，3 reps | 30+30+30+30 plans，**0 violations** | 已执行；物理流覆盖有限 |
| orientdb-client | 同上 | held session | S-B1/S-B2/S-B3/S-M1，3 reps | 6 plans/level，**0 violations** | 已执行 |
| skytable | 0.8.12 / bb8 0.9.1；skytable 0.8.4 | pool + direct | B1–M1 + generated，3 reps | **35 violations / 72 plans**；pooled B1 0/4、B2 2/4、B3 2–3/4、M1 2–3/4；direct B1 0/2、B2 0–1/2、B3 2/2、M1 2/2；generated 84/120 | **确认的客户端缺陷**；response association 新实例 |
| gel-tokio | 0.11.0；gel 7.1 | pool `max_concurrency=1` | probe + 完整 ladder | 3/3 probe；§12.10 补 96 trials 0，M1 Pre 21/Post 3 | 旧行只算 probe；以 §12.10 为准 |
| http-client | 6.5.3 | — | — | `H1Client` 需要 async-std/async-h1/deadpool；在 Tokio 下要第二 runtime，按规则排除 | 未执行 |

Rust 侧**新增一个确认的客户端缺陷**：skytable 的 response association
（见 §3.5）。orientdb 的 0 违规仍是**弱阴性**：`SessionPool` 的
`max_size=1` 只限制一个逻辑 session；实测 proxy 上稳定有 **2 条物理 TCP
流**，`ConnectionOptions::pool_size` 没有反映到内部 `Cluster`。因此“没有看到
stale reply”不能证明“连接被正确重置”——stale reply 可能被另一条流掩盖。
这一点在 `orientdb/SCENARIO.md` 和 `RESULTS-FRAGMENT.md` 里明确记录。

### 1.2 客户端（Python）

| package | 版本 | 模式 | 级别 | 结果 | 状态 |
|---|---|---|---|---|---|
| psycopg | 3.3.6 / psycopg-pool 3.3.3 | pool + direct | B1/B2/B3/M1，3 reps | pool **M1 0/18**（release 时 rollback INTRANS→IDLE，同一 backend pid，正例）；direct **M1 12/18**（#1–#4），B1/B2/B3 0/18；survivor `write=Ok`、`own=7`，observer `rows=[]` | 状态泄漏；B2/B3 的值里有 `INTRANS`，但只有 M1 判 |
| asyncpg | 0.32.0 | pool + direct | B1/B2/B3/M1，3 reps | pool **M1 0/12**（release 时 reset）；direct **M1 9/9**，B1/B2/B3 **0/9**；#1 是 `is_in_transaction()=False` 的“BEGIN 仍在队列里”，#2 是 `idle in transaction`，#3 是 stale-state-only | 强阳性；#1 比“遗留事务”更强 |
| aiomysql | 0.3.2 / MariaDB 11.4 | direct | B1/B2/B3/M1，3 reps | B1/B2/B3 **0/9**；M1 **9/9**；survivor `InterfaceError: Cancelled during execution`（sticky），observer `rows=[]` | 新客户端实例；fail-fast，不是静默写 |
| httpx | 0.28.1 / httpcore 1.0.9 | pool | B1/B2/B3/M1，3 reps | B1 **0/18**；B2 **3/18**；B3 **3/18**；M1 **3/18**；pending#6 retry `TimeoutError` | **已知上游相关**（#2821 / httpcore #726）；本轮源码定位到 `PoolByteStream.aclose` |
| aiosmtplib | 5.1.3 / GreenMail 2.1.14 | direct | B1/B2/B3/M1，3 reps | B1 **0/12**；B2 **12/12**；B3 **12/12**；M1 **12/12**；9/12 `SMTPDataError: (250,'OK')`，3/12 `TimeoutError` | **同 async-smtp 族的新客户端实例**；未找到 aiosmtplib 上游报告 |
| aiomysql pool / mysql-connector | — | — | — | 时间窗内未完成 | 未覆盖 |
| pymongo / neo4j / curl-cffi | — | — | — | 未执行（时间窗） | 未覆盖 |

Python 侧的**新实例**是 asyncpg direct 的“direct handle 未重置”类；
psycopg direct 是同一类的更早实例，aiomysql direct 是同一族里的
**fail-fast** 变体（sticky `InterfaceError`，没有静默未提交写）。
aiosmtplib 是第二轮 async-smtp 的**同族新客户端**（响应关联/desync），
httpx 是**已知上游相关**的 cancellation cleanup 缺陷。三者都不是全新的
机制类别；本轮真正新的机制类别只有 Linux multicast。

Python 批次共 **300 trials**（psycopg 72+90，asyncpg 84，aiomysql 36），
**30 个 violating trials**（12+9+9）——这是执行次数，不是 30 个 bug。

### 1.3 Linux

11 个 target，5 reps，fail-nth 或 signal；`aggregate.csv`：

| target | mech | reps | sites | trials | violations | 结论 |
|---|---|---:|---:|---:|---:|---|
| `mmap_fixed_replace` | failnth | 5 | 5 | 5 | **5** | 契约外（MAP_FIXED 破坏性） |
| `mremap_fixed_dst` | failnth | 5 | 15 | 15 | **5** | 契约外（目标洞） |
| `mprotect_multi` | failnth | 5 | 16 | 16 | **10** | 契约外（部分 VMA 已改） |
| `ip_add_membership` | failnth | 5 | 21 | 21 | **5** | **新根因**（成功但设备组未加入） |
| `tmpfs_setxattr_replace` | failnth | 5 | 15 | 15 | 0 | 阴性 |
| `tmpfs_rename_exchange` | failnth | 5 | 10 | 10 | 0 | 阴性 |
| `tmpfs_fallocate` | failnth | 5 | 85 | 85 | 0 | 阴性（17 sites/rep；80 pre + 5 post） |
| `pipe_set_size` | failnth | 5 | 5 | 5 | 0 | 阴性 |
| `so_attach_filter` | failnth | 5 | 51 | 51 | 0 | 阴性（43 pre + 8 post） |
| `semtimedop_eintr` | signal | 5 | 40 | 40 | 0 | 阴性 |
| `fcntl_setlkw_eintr` | signal | 5 | 40 | 40 | 0 | 阴性 |

`mmap/mremap/mprotect` 的 20 次 violation 是**同一类契约外行为**，不是 20 个
根因；`ip_add_membership` 的 5 次是**同一个根因**的 5 个 rep。按根因计数时
不能按 trigger/组合/失败计划数膨胀。

---

## 2. 新确认的根因：IPv4 multicast join 的“成功但没效果”

### 2.1 契约

`setsockopt(fd, IPPROTO_IP, IP_ADD_MEMBERSHIP, &mreq, sizeof(mreq))` 返回 0，
按 `ip(7)` 的语义表示 socket 已加入该组；内核应发送 IGMP 报告并在设备上建立
组记录。返回 0 但设备层没有组，意味着后续 multicast 收包不会到达该 socket，
而调用方没有任何错误可查。

### 2.2 代码路径

`net/ipv4/igmp.c`：

```c
int __ip_mc_join_group(struct sock *sk, struct ip_mreqn *imr, unsigned int mode)
{
        ...
        iml = kzalloc(sizeof(*iml), GFP_KERNEL);
        if (!iml) return -ENOMEM;
        ...
        err = ip_mc_join_group_ssm(sk, iml, mode);
        if (err) { kfree(iml); return err; }
        return 0;
}
```

`____ip_mc_inc_group()` 是 `void`，内部：

```c
im = kzalloc(sizeof(*im), GFP_KERNEL);
if (!im)
        return;          /* 失败被吞掉 */
```

`__ip_mc_join_group()` 已经把 `iml` 链入 `inet->mc_list`，随后调用这个
`void` 辅助函数；`kzalloc` 失败时 socket 级 membership 已经存在，设备级
`ip_mc_list` 没有建立，syscall 仍返回 0。结果是“socket 认为自己加入了，
设备没有加入”。

### 2.3 证据

`research/linux/logs/20261021-r4-mass/09-ip_add_membership-rep1.log`：

```text
TRIAL target=ip_add_membership mech=failnth site=1 ret=-1 errno=105 injected=1 readback=0 verdict=OK_PRE
  obs=surv_msf=-1;surv_errno=99|obs_msf=-1;obs_errno=99;igmp_group=0
TRIAL target=ip_add_membership mech=failnth site=2 ret=0 errno=0 injected=1 readback=0 verdict=VIOL_MIXED
  obs=surv_msf=0;surv_errno=0|obs_msf=0;obs_errno=0;igmp_group=0
TRIAL target=ip_add_membership mech=failnth site=3 ret=0 errno=0 injected=1 readback=0 verdict=OK_POST
  obs=surv_msf=0;surv_errno=0|obs_msf=0;obs_errno=0;igmp_group=1
```

- pre：`IP_MSFILTER` 查询 `surv_msf=-1/errno=99`，`/proc/net/igmp` 无组；
- post：`surv_msf=0`，`igmp_group=1`；
- site 2：`ret=0`（成功）、`igmp_group=0`（实际没加入）→ `VIOL_MIXED`；
- site 1：`ret=-1 errno=105 ENOBUFS`（干净的错误基线）；
- site 3–5：`ret=0` 且 `igmp_group=1`（后续非致命分配，效果完成）。

5/5 reps 都是 site 2 违规；site 数在 4–5 之间浮动，但违规点稳定。独立
initramfs 复现（v6.19 与 v7.3-rc2）：

- n=1：`ret=-1 errno=105`，无组（错误基线）；
- n=2：`ret=0 errno=0`，`/proc/net/igmp` 无 `030201EF`（成功但没效果）；
- n=3：`ret=0`，组存在（后续分配，效果完成）。

这排除了“harness 的 fail-nth 读回本身有问题”：同一个 target 在 n=1 会
干净地报错，在 n=3 会干净地成功，只有 n=2 是 success-without-effect。

### 2.4 分类与限制

- **真实契约违反**：返回 0 是“已加入”的承诺；设备层无组是 silent effect loss。
- **fault-injection-only（本 guest）**：没有 MEMCG、没有内存压力，不能声称
  真实环境经常触发。它是“注入可达”的缺陷，不是“已经观测到线上故障”的缺陷。
- **与取消无关**：没有任务取消、没有异步中断；这是纯 success-without-effect。
  它把共性从“未完成交换”推广到“报告的 result 与共享对象的实际状态不一致”，
  同时**反驳**了“共性必须涉及取消”的窄版本。
- **上游检索**：未找到同一路径的公开报告（本轮检索范围内）；按“新候选”
  记录，不声称“全社区未知”。

### 2.5 修复与 original/fixed

修复：把 `____ip_mc_inc_group()` 改为返回 `int`，`kzalloc` 失败返回
`-ENOBUFS`；`__ip_mc_join_group()` 在把 `iml` 链入 `mc_list` **之前**调用它，
失败时 `sock_kfree_s()` 释放 `iml` 并返回错误。`__ip_mc_inc_group()`（接口
up/init 路径）保留 `void` 包装，注释说明该路径无法向用户报错。

original/fixed 对比（同一个 harness、同一个 fail-nth site 2、5 reps）：

| 内核 | site 2 结果 | 判定 |
|---|---|---|
| original（v7.3-rc2，`bzImage-stock`） | `ret=0 errno=0`，`igmp_group=0` | **VIOL_MIXED，5/5** |
| fixed（`bzImage-fixed`，patch above） | `ret=-1 errno=105 ENOBUFS`，`igmp_group=0` | **OK_PRE，5/5，0 violation** |

fixed 下 site 1 仍是 `-ENOBUFS`，site 3/4 仍是 `ret=0; igmp_group=1`；
`aggregate.csv`：`done=5, stable=5, sites=20, trials=20, ok_pre=10,
ok_post=10, violations=0`。也就是说：**失败现在干净地报错，成功仍然真的成功**，
不再有 `ret=0; igmp_group=0`。

回归：同一批 26 个 target（15 个已有 + 11 个新；29 行含 failnth/failfunc/signal 变体）
在 stock 与 fixed 上各 1 rep（`stockregress` / `fixedregress`）。fixed 上 `ip_add_membership` 从 1 violation 变 0；其余
target 的 pre/post 分布与 violation 数不变（`mmap/mremap/mprotect` 仍是
契约外的 1/2/3；`epoll_mod_wakeup` 的 45 个 `ok_poisoned` 是既定分类，
不是新 violation）。没有看到 fixed 引入的新失败。`kfree`/
`sock_kfree_s` 的配对、`in_dev` 引用和 `done` 路径已按原函数检查；
没有改 `__ip_mc_inc_group()` 的接口路径。

---

## 3. 已知类别的实例：direct handle 在取消后没有重置

### 3.1 asyncpg direct（强阳性，M1-only）

`asyncpg 0.32.0`，direct，3 reps × 3 plans = 9 trials：

| level | trials | violations |
|---|---:|---:|
| B1 | 9 | 0 |
| B2 | 9 | 0 |
| B3 | 9 | 0 |
| M1 | 9 | **9** |

三个 plan 不是同一种形状：

| plan | `is_in_transaction()` | server `pg_stat_activity` | observer | 读法 |
|---|---|---|---|---|
| #1（y1） | **False** | `state='idle'`，last query `SELECT 1`（BEGIN 还在客户端队列） | `[]` | **reported-state vs actual-state divergence**：下一个写与排队的 BEGIN 一起 flush，写成功但不提交 |
| #2（y2） | True | `idle in transaction`（BEGIN） | `[]` | 遗留事务；写进了目标事务，未提交 |
| #3（y3） | True（但 stale） | last query 是 target INSERT（已有 xid） | `[survivor,target]` | value-only 看是合法 Post；只有状态字段把它变成 Neither，**不是丢写** |

- #1 的 pre/post/injected：
  ```text
  pre:  survivor={in_tx:False, write:Ok, own:7}; observer=[["survivor",7]]
  post: survivor={in_tx:False, write:Ok, own:7}; observer=[["survivor",7],["target",1]]
  inj:  survivor={in_tx:False, write:Ok, own:7}; observer=[]
  ```
  `is_in_transaction()` 和 server 的 `state='idle'` 都说“没有事务”，但
  `BEGIN` 仍在客户端队列里；survivor 的写把 `BEGIN` 一起 flush，写进了
  一个未提交事务。**状态信号本身在下一个操作之前是缺失的**，比“遗留事务”
  更强。
- #2：`in_tx=True`，server `idle in transaction`；写进目标事务，observer 空。
- #3：`in_tx=True` 但 server 已经执行了 target 的 INSERT/COMMIT；
  observer 最终有 survivor+target。value-only 分解是合法的 Post；只有
  acquire-time 状态字段让它变成 Neither。**不能把它算成 6/9 之外的丢写。**
- value-only 分解：**6/9 丢写（#1/#2）+ 3/9 stale-state-only（#3）**。
- 显式 `ROLLBACK` 在 cancel 后对 #2/#3 有效（`Ok`，in_tx False，survivor
  可见）；#1 没有做 rollback 对比。
- pool 模式 0/12：release 时打印 “Resetting connection with an active
  transaction”，即 pool 主动 reset；正例。
- 上游：asyncpg #537/#1310 已复现；层是 driver/session，不是 SQLAlchemy
  #6652；asyncpg pool 本身 clean。

**分类**：direct handle 的“取消后未 reset”类的 **reported-state 变体**；
与 psycopg direct、BOLT direct 同族。额外价值是它把“状态字段”变成 M1 的
必要部分，同时给出一个反例：状态字段也会把合法的 Post 变成 Neither，
必须把“丢写”和“stale-state-only”分开报。

### 3.2 psycopg direct（同类别，M1 12/18）

`psycopg 3.3.6` / `psycopg-pool 3.3.3`，direct，3 reps × 6 plans = 18 trials：

| level | trials | violations |
|---|---:|---:|
| B1 | 18 | 0 |
| B2 | 18 | 0 |
| B3 | 18 | 0 |
| M1 | 18 | **12**（#1–#4） |

- survivor acquire 时 `status_after_acquire=INTRANS`；单条 autocommit INSERT
  返回 `Ok('INSERT 0 1')`，same-conn readback 7，observer `rows=[]`；
  后续 `COMMIT` 把**被放弃的前一个事务**提交了。
- #5/#6 是 Post（COMMIT 已经执行），合法。
- B2/B3 都是 0/18，但它们的值里明确有 `status_after_acquire=INTRANS`、
  `BEGIN/INSERT/COMMIT` 全部 `Ok`；B2/B3 只判“是否抛错/超时”，不看返回值，
  所以**看不见**。M1 把 `in_tx` 加进 acquire-time observation 后抓到
  12/18；这是“同一根因、observation 是否含状态字段 → 不同判定”的方法证据。
- pool 模式 0/18：release 时打印 “rolling back returned connection:
  <AsyncConnection [INTRANS]>”，下一次 acquire 是 IDLE，同一 backend pid；
  正例。
- 分类：**契约缺口/调用方义务**（手写 `BEGIN`/`COMMIT`；`conn.transaction()`
  或显式 `rollback()` 是工具），不是已确认的库 bug。相关 psycopg
  discussions #555/#1208 只有搜索片段，未核实文档承诺。

### 3.3 aiomysql direct（同族，fail-fast 变体）

`aiomysql 0.3.2` + MariaDB 11.4，direct，3 reps × 3 plans = 9 trials：

| level | trials | violations |
|---|---:|---:|
| B1 | 9 | 0 |
| B2 | 9 | 0 |
| B3 | 9 | 0 |
| M1 | 9 | **9** |

- cancel 后同一个 handle 在 survivor 的 write/readback 上抛
  `InterfaceError: Cancelled during execution`（sticky flag）；observer
  `rows=[]`；`in_tx` 为 0 或 1，但 handle 已经不可用。
- 这**不是静默未提交写**：它 fail-fast，错误可见；但“同一个 handle 不能
  再用”本身仍是取消后的状态泄漏。
- B2/B3 0/9 是**检测层的假阴性**：`_tx_write` 把 `InterfaceError` 捕获后
  返回 `None`，而 `level_retry` 只把字符串 `ERR ...` 当错误；返回值里的
  错误结构没有被检查。M1 的 survivor 直接看到 error 值，所以 9/9。
- 分类：direct-handle 类的 fail-fast 变体；没有找到上游报告，未做 patch
  contrast，按“候选/未决”记录，不声称已确认库 bug。

### 3.4 aiosmtplib（SMTP 响应关联；同族新客户端）

`aiosmtplib 5.1.3` + GreenMail 2.1.14，direct，4 plans × 3 reps：

| level | trials | violations |
|---|---:|---:|
| B1 | 12 | 0 |
| B2 | 12 | **12** |
| B3 | 12 | **12** |
| M1 | 12 | **12** |

- 9/12 的后继是 `SMTPDataError: (250,'OK')`；3/12（pending#3）是 2 s
  `TimeoutError`，NOOP 也超时。
- 机制：目标在 `DATA` 之后被取消，server 的 `250` 留在流里；下一次 `DATA`
  期待 `354`，读到 `250`，于是把它当成“服务器拒绝了 DATA”抛
  `SMTPDataError`。`NOOP` 也被同一个残留回复卡住。
- M1 的具体值（pre/post/injected）：
  - pre：mailbox `['<run> survivor']`，survivor own/send `final=OK`，NOOP 250；
  - post：mailbox `['<run> survivor','<run> target']`，survivor own/send OK；
  - injected：`pending#1–#3` mailbox `[]`（target 没投递），`pending#4`
    mailbox 只有 target（target 投递了，survivor 没投递）；survivor 的
    send/own 都是上面的错误，NOOP 超时。
- **分类**：与第二轮 `async-smtp` 同一个 **SMTP DATA/354/250 响应关联**族，
  但这是一个新的客户端实例；未找到 aiosmtplib 自己的上游 cancellation 报告。
  不算全新机制类别，但“已知形状迁移到新客户端”是明确的命中。
- 修法方向（未做 patch contrast）：取消时关闭/重新同步连接，或把未读的
  DATA 回复读完后再放回；不能把连接直接留给下一次 `DATA`。

### 3.5 skytable（response association；Rust 新实例）

`skytable 0.8.12`（官方 pool = bb8 0.9.1）+ `skytable/skytable:0.8.4`，pool 和
direct 两种模式，3 reps：

| mode | B1 | B2 | B3 | M1 |
|---|---|---|---|---|
| pooled（4 plans） | 0/4 ×3 | 2/4 ×3 | 3/4, 2/4, 3/4 | 3/4, 2/4, 2/4 |
| direct（2 plans） | 0/2 ×3 | 1/2, 0/2, 1/2 | 2/2 ×3 | 2/2 ×3 |

合计 **35 violation executions / 72 ladder plans**；unrealized=0、baseline=0、
exhaustive=true。生成器（`combine.rs`）10 cases × 3 reps：**84/120 violations**，
0 skipped/unrun；所有 read-back/select successor 都 4/4 失败，plain update
1–2/4。

具体反例（pooled `#0@1:Immediate`）：

```text
pre:  before=0, write=Ok, own=Ok(7), visible=Ok(7)
post: before=1, write=Ok, own=Ok(7), visible=Ok(7)
inj:  before=0, write=Ok, own=Err("row not found (empty response)"), visible=Ok(7)
```

direct `#0@1:AfterWake`：`own=Err("server error 6")`，observer 仍看到正确的
row；server 的 row 一直是对的，坏的是**客户端自己的读**。

- 机制：被丢掉的 query future 把 reply（或 split packet 的前缀）留在连接上；
  下一次请求（pooled 里是 bb8 validation，direct 里是 survivor update）把它
  当成自己的 reply 消费。
- **pool 不自治愈**：cancel 后 `connections=1`、`connections_created=1`、
  `connections_closed_broken=0`；pool 把同一条 desynced TCP 还回来。
  这是“pool checkout/validation 把 stale reply 往后推一个请求”的干净样本。
- **B3 会返回 stale `Ok(0)` 却通过 Ok-only 规则**；B2 也有“retry Ok 但
  消费了 stale reply”的情况。M1 的 `own`/`visible` 才抓到。
  direct rep2：B2 0/2 vs M1 2/2；pooled rep1：B2 2/4 vs M1 3/4。
- 分类：**确认的客户端缺陷，一个根因**——response association 被取消破坏；
  与 BOLT/async-smtp 同族，但这是新的 Rust 客户端实例。没有文档化的
  cancellation/reset 保证；上游检索和 patch contrast **未做**，所以不声称
  “已知/新”。

### 3.6 为什么它是“契约缺口”而不是“必然库 bug”

psycopg 文档说明取消后连接状态可能不确定，调用方应使用 `rollback()` 或
丢弃连接；asyncpg 暴露 `is_in_transaction()` / `reset()`，pool 在 release
时 reset；aiomysql 则用 sticky `InterfaceError` 直接拒绝继续使用。三者都把
“恢复”留给调用方、pool 或客户端 guard。对 direct handle，调用方如果不读
这个信号，就会把后继写进前一个事务，或者拿到一个不可用的 handle。把它称为
“库缺陷”需要证明文档承诺了自动重置；本轮没有找到这样的承诺，因此按
**契约缺口/调用方义务**记录，但它满足共性最核心的形状：
**信号存在（或信号本身失真），下一个决策者没读它**。

注意：asyncpg #1 比“信号存在”更强——`is_in_transaction()` 和 server 的
`state='idle'` 都表示“没有事务”，但 `BEGIN` 还在客户端队列里；下一个写
把它一起 flush，静默不提交。这是**信号本身缺失/错误**，不是调用方没读。
#3 又是反过来：状态字段 stale，value-only 是合法的 Post；把状态字段加进
判定会把它变成 Neither，但它不是丢写。**“加状态字段”不是免费的正确性：
它既能抓到真泄漏，也能把合法结果误判，必须分开报。**

---

## 4. 已知上游：httpx 取消后连接池退化

`httpx 0.28.1` + `httpcore 1.0.9`，pool，18 trials/level：

| level | violations | 观测 |
|---|---:|---|
| B1 | 0/18 | 取消本身不报错 |
| B2 | **3/18** | pending#6 retry `ERR post-target TimeoutError` |
| B3 | **3/18** | pending#6 `ERR post-b3 TimeoutError` |
| M1 | **3/18** | survivor POST 未发出、readback 失败、observer 仍是 target |

triage（`triage-pending6.log`）：

```text
limit=1:
  after cancel: conns=[... ACTIVE, Request Count: 2], queued=1
  server rows after cancel: [('post', '...-target')]
  retry result: ERR retry TimeoutError
  server rows after retry: [('post', '...-target')]      # retry 根本没到服务器
  aclose of wedged client: done
  fresh client result: <Response [200 OK]>

limit=2:
  retry result: <Response [200 OK]>
  server rows after retry: [target, survivor]
  after retry: [ACTIVE, IDLE]                            # 旧连接仍卡住
```

- 取消后连接停在 `ACTIVE`，没有被归还；`max_connections=1` 时 retry 排队等
  一个永远不会释放的 slot；`max_connections=2` 时新连接可用，旧连接仍 ACTIVE。
- `aclose()` 只能把整个 client 关掉；fresh client 正常。
- 源码定位（`httpcore 1.0.9`）：`PoolByteStream.aclose()` 先置
  `_closed=True`，然后
  `with AsyncShieldCancellation(): await stream.aclose()`，**之后**才
  `del self._pool_request` / 把连接归还。原始 `Task.cancel()` 落在这个
  `await` 上时，`AsyncShieldCancellation` 并不屏蔽 raw cancel；异常在
  `_pool_request` 删除前抛出，httpx 随后的 `response.aclose()` 因
  `_closed=True` 变成 no-op。连接和 pool request 就永久留在 ACTIVE。
  一个最小 probe 确认 anyio 的 shield 不屏蔽 raw `Task.cancel()`。
- 上游：行为与 [httpx #2821](https://github.com/encode/httpx/discussions/2821)
  描述一致（“cancelled at precisely the wrong time … degraded state …
  PoolTimeout … 新 client 才恢复”）；相关还有 #3154、httpcore #844/#726。
  但**具体窗口是否已被上游修复、#2821 是否就是这个窗口，未核实**
  （GitHub 抓取受限）；本轮也没有做 patch contrast。
- **分类：确认的客户端 cleanup 缺陷；与已知上游讨论同族，不是全新机制类别。**
  它的价值是：我们在没读 #2821 的情况下用共性预测了“取消会留下未完成交换/
  资源”，并确定性地在 pending#6 复现；同时把窗口缩小到 `aclose()` 的
  shield 段。这是**迁移命中 + 源码定位**，不是发现一个全新类别。

---

## 5. 契约外：mmap / mremap / mprotect 的部分破坏

Linux mass 结果：

- `mmap(MAP_FIXED)`：5/5 violation；旧映射在失败前已被 unmap，返回错误时
  地址空间出现洞/新映射不完整。
- `mremap(MREMAP_FIXED)`：5/5；目标地址先被 unmap，移动失败时目标洞。
- `mprotect` 多 VMA：10/10；错误返回时前面的 VMA 已经改了保护。

这些**不是**“实现 bug”，而是 API 契约本身允许的破坏性行为。LKML 2023
“MREMAP_FIXED unmaps dest on error” 线程里 David Hildenbrand 明确说：
mapping syscalls 从不保证出错时状态不变；`MAP_FIXED` 先 unmap 是设计。
v6.19 还故意在 `MAP_FIXED` 失败时恢复一个 gap；2024 Jann Horn 的
`mm: mark mas allocation in vms_abort_munmap_vmas as __GFP_NOFAIL`
修的是 abort 路径的 UAF，不是“出错必须原子”。

**分类：契约外，不是 bug；不提交修复。** 它们是有用的反例：`ret != 0`
不等于“没有效果”；但把“部分效果”当缺陷会误报。共性在这里能解释现象，
但不能把它算成新根因。

---

## 6. 阴性结果与覆盖缺口

### 6.1 orientdb：0 违规，但物理流覆盖有限

- 24 个 level run，3 reps，B1/B2/B3/M1 主 + S-B1/S-B2/S-B3/S-M1；
- 0 violations / 0 unrealized / 0 unsettled / 0 baseline/calibration errors；
- B1 30 plans；B2 30（全部 `Ok((1,"target"))`）；B3 30（24 个 `(0,"seed")`，
  6 个 `(1,"target")`，都 `Ok`）；M1 30（pre=29/post=1，无 mixed/neither/unknown）；
- S 系列 6 plans/level；S-M1 pre=3/post=3；
- 具体值：每个 realized plan `own=visible=write=(7,"survivor")`，
  `before ∈ {(0,"seed"),(1,"target")}`；没有 attributed stale reply；
- **限制**：proxy 上稳定 2 条物理 TCP 流；`max_size=1` 只限制逻辑 session；
  `ConnectionOptions::pool_size` 未反映。stale reply 可能被另一条流掩盖，
  所以这是**弱阴性**，不是“orientdb 安全”。
- recovery contrast：5 ms cut 在 checkout 到 wire 之前触发，stale pool 没有
  状态；stale 与 fresh reconnect 都成功，**不构成** recovery contrast。

### 6.2 其他

- psycopg pool：0/18；release 时 rollback，正例。
- asyncpg pool：B1/B2 0/12；release 时 reset，正例。
- aiosmtplib：不是阴性，是 12/12 阳性；列在这里是为了和“SMTP 族”对照。
- Linux clean：`tmpfs_setxattr_replace` 15/15、`tmpfs_rename_exchange` 10/10、
  `tmpfs_fallocate` 85/85、`pipe_set_size` 5/5、`so_attach_filter` 51/51、
  `semtimedop_eintr` 40/40、`fcntl_setlkw_eintr` 40/40；全部 0 violation，
  且 pre/post 非退化（有 OK_PRE / OK_POST 分裂），说明注入点覆盖到了。
- skytable：不是阴性，是 35/72 + generated 84/120 阳性；见 §3.5，不列在这里。
- gel：3/3 probe 干净；后来 §12.10 补成 96 trials 的四层 ladder（0 violation），
  旧 probe 保留；以 §12.10 为准。
- http-client：runtime 不兼容，未执行。
- aiomysql / mysql-connector / pymongo / neo4j / curl-cffi：
  时间窗内未执行；**未覆盖**。
- Linux `TCP_ULP` / `prctl` / `io_uring` / timerfd / inotify / msgrcv 等：
  预测里列出但**没有跑**；不能写成 clean。

---

## 7. 预测对照

### 7.1 客户端

| 预测 | 实际 | 判定 |
|---|---|---|
| skytable direct 可能违规 | **35/72 ladder plans；direct B2/B3/M1 都有违规** | **命中** |
| skytable pool 可能 clean/同 direct | pool 也违规（B2 2/4、B3/M1 2–3/4）；不自治愈 | **命中“同 direct”** |
| orientdb pool 可能违规（BOLT-like） | 0/120 主阶梯 plans；但 2 条物理流 | **miss / 弱阴性** |
| gel pool 可能违规 | 3/3 probe 干净 | §12.10 后：96 trials 0（单 pooled 模式） |
| httpx 可能 confirmed/clean | B2/B3/M1 3/18 timeout | **命中（已知上游相关）** |
| aiosmtplib 可能 confirmed | B1 0/12；B2/B3/M1 12/12 | **命中（同 SMTP 族新客户端）** |
| psycopg 可能 confirmed | pool clean；direct M1 12/18 INTRANS；B2/B3 0 | **命中（direct 契约缺口）** |
| asyncpg 可能 confirmed | direct M1 9/9；pool clean | **命中（同一 direct 类）** |
| aiomysql 可能 confirmed | direct M1 9/9，B2/B3 0（sticky InterfaceError） | **命中（同族 fail-fast）** |
| mysql-connector / pymongo / neo4j | 未跑 | **未检验** |
| http-client 可能违规 | runtime 排除 | **未检验** |

### 7.2 Linux

| 预测 | 实际 | 判定 |
|---|---|---|
| mmap MAP_FIXED 可能违规，契约可能弱 | 5/5 违规 | **行为命中；契约外** |
| mremap FIXED 可能违规 | 5/5 | **行为命中；契约外** |
| mprotect 可能部分生效 | 10/10 | **行为命中；契约外** |
| TCP_ULP / prctl / io_uring 可能 | 未跑 | **未检验** |
| 其余（setxattr/rename/fallocate/pipe/BPF/semtimedop/fcntl）预测 clean | 7 个 target 全 0 | **命中** |
| **multicast join 未被预测** | **5/5 违规，新根因** | **未预测的发现** |

### 7.3 方法预测

| 预测 | 实际 | 判定 |
|---|---|---|
| B1 只抓 hang/panic | httpx B1 0/18；asyncpg 0/9；psycopg 0/18；aiomysql 0/9；aiosmtplib 0/12；skytable 0/4+0/2 | 支持 |
| B2 抓硬错误但漏 stale value | httpx B2 3/18 timeout；aiosmtplib B2 12/12；skytable pooled 2/4、direct 0–1/2；asyncpg/psycopg B2 0；aiomysql B2 0（返回值错误未被 level 看见） | 支持，但有检测层缺口 |
| B3 抓部分关联错误但仍漏 value | httpx B3 3/18；aiosmtplib B3 12/12；skytable pooled 2–3/4、direct 2/2；asyncpg/psycopg B3 0；aiomysql B3 0（同上） | 支持，但有检测层缺口 |
| M1 是唯一能看到 “Ok but wrong value” 的 | asyncpg M1 9/9；psycopg M1 12/18；aiomysql M1 9/9；skytable M1 2–3/4 + 2/2；aiosmtplib/httpx 的 B2 已先抓到 | **支持（但依赖 observation 是否含状态）** |
| 至少一个 B2 抓到而 M1 抓不到的案例 | 本轮没有：httpx/aiosmtplib 的 B2 与 M1 都是同样违规 | **未命中** |
| 至少一个 B2/B3 过、M1 失败的案例 | asyncpg 9/9、psycopg 12/18、aiomysql 9/9、skytable 都是 | **命中** |
| B2/B3 的“not-error”只看逃逸异常 | aiomysql 的 `InterfaceError` 被 case 捕获成返回值，B2/B3 0；M1 9/9 | **新发现的方法缺口** |
| Linux 多数 clean | 7/11 clean；1 新 bug；3 契约外 | 大体支持 |
| 只有分配失败、无真实 trigger 的发现不应夸大 | multicast 正是 fault-injection-only | **支持（按此措辞）** |

预测校准的结论：**行为/方法层面的预测命中率明显高于“具体缺陷位置”层面**。
7.3 里唯一明确没命中的是“B2 抓到而 M1 抓不到”——本轮没有这种案例；B2 的
价值是更便宜地先抓到**逃逸**的硬失败（httpx/aiosmtplib/skytable），不是抓到
M1 看不到的类别。skytable 还给出一个 B3 反例：B3 返回 stale `Ok(0)` 却
通过 Ok-only 规则，M1 的 `own`/`visible` 才抓到。更重要的新发现是：
**B2/B3 的“not-error”只检查逃逸异常/字符串 ERR；如果 case 把异常捕获成
返回值（aiomysql），B2/B3 会给出假阴性。**
下一轮应把 `level_retry` 改成检查 `normalize(value)` 里的错误结构，而不是只
看 `isinstance(value, str) and value.startswith("ERR ")`。

---

## 8. 方法增量（这一轮真正学到的东西）

1. **`Ok` 不是正确性，连“状态字段”也可能骗人。**
   - asyncpg direct #1：`write=Ok`、`own=7`，observer 空表；`is_in_transaction()`
     和 server `state='idle'` 都说“没有事务”，但 `BEGIN` 还在客户端队列里。
   - psycopg direct：`write=Ok`、`own=7`，observer 空表；`INTRANS` 就在
     acquire-time 状态里，但 B2/B3 不看它，M1 加进去后 12/18。
   - aiomysql direct：不静默写，而是 sticky `InterfaceError`；handle 不可用
     本身也是状态泄漏。
   - skytable：`write=Ok`、`own=Err("row not found")`、`visible=Ok(7)`；
     server 的 row 一直正确；pool 仍返回同一条连接。
2. **B2/B3 是“便宜的第一道网”，M1 是“值/观察者/状态的第二道网”。**
   - httpx：B2 已经用 `TimeoutError` 抓到；M1 只是解释“survivor 没发出”。
   - aiosmtplib：B2 12/12 已经用 `SMTPDataError: (250,'OK')` 抓到；
     M1 只是把“target 是否投递、survivor 是否投递”分开。
   - asyncpg 9/9、psycopg 12/18、aiomysql 9/9：B1/B2/B3 全 0，M1 才抓到；
     这是 M1 不可替代的样本。
   - skytable：B2/B3 各抓到一部分，但 B3 会返回 stale `Ok(0)` 却通过
     Ok-only；M1 的 `own`/`visible` 才把它变成违规。
   - **但 B2/B3 的“not-error”只检查逃逸异常/字符串 `ERR`。** aiomysql 的
     `InterfaceError` 被 case 捕获成返回值，B2/B3 就 0/9；这是**检测层的
     假阴性**，不是“重试成功”。下一轮要么把 `level_retry` 改成检查
     `normalize(value)` 的错误结构，要么让 case 重新抛出。
3. **状态字段不是免费的：它既能抓真泄漏，也能把合法 Post 变成 Neither。**
   - asyncpg #3：value-only 是合法 Post（observer 有 survivor+target），
     只有 `in_tx` stale 才变 Neither；**这不是丢写**，要单列
     “stale-state-only”。
   - psycopg：把 `in_tx` 加进 observation 后，M1 从 0/18 变 12/18；
     同一根因、不同 observation → 不同判定，**观察协议本身要审计**。
4. **Linux 需要“独立观察者字段”，不能只看 syscall 返回值。**
   - `igmp_group` 从 `/proc/net/igmp` 读，和 `surv_msf` 分开；
   - site 2 的 `ret=0` 只有和 `igmp_group=0` 一起才是违规；
   - 如果只判 `ret`，会得到 0 violation 的假阴性。
5. **pre/post 校准 + 注入点枚举是必要条件。**
   - ip target site 1 `-ENOBUFS`、site 3/4 成功、site 2 违规、site 5 无注入；
     这四点把“注入确实命中”与“效果是否完成”分开；
   - 5/5 reps 稳定；没有把 site 数浮动当成结论。
6. **零结果必须带覆盖说明。**
   - orientdb 0/120 主阶梯但有 2 条物理流，写成弱阴性；
   - gel 的旧 3 次 probe 曾写成未覆盖；§12.10 补 96 trials 0 后才写成阴性（仍限单模式）；
   - modbus/async-pop、Linux 未跑 target 写成未覆盖；
   - `mmap` 等写成契约外，不写成新 bug。
7. **`combine.rs` 的 same/different object/payload 生成器在 orientdb 上
   跑了 13 cases / 124 plans，0 violations；但 generator 的 `Obs` 缺
   pre-target 快照，9 个 update-target case 退化为 pre/post 同值。**
   这说明生成器本身也要做“观察是否退化”的元检查，不能只看 0 violation。
   skytable 的同一生成器（10 cases）跑出 84/120，说明“生成器 + 0 违规”
   并不是安全信号，必须结合 case 的观察字段和 successor 语义读。

---

## 9. 共性的哪一部分被支持、哪一部分被反驳

### 支持

- **取消后未完成交换会留下状态**：httpx（连接 ACTIVE、slot 不释放）、
  asyncpg/psycopg direct（事务未 reset；psycopg 12/18、asyncpg 9/9）、
  aiomysql direct（sticky `InterfaceError`）、skytable（stale reply 留在
  连接上；35/72）、aiosmtplib（DATA 的 250 留在流里）、BOLT direct（历史）
  都支持。
- **决定复用的一方常常不读这个状态**：psycopg direct 的 `INTRANS` 没被
  survivor 读；asyncpg 的 `is_in_transaction()` 甚至报 `False`；httpx 的 pool
  不知道连接还 ACTIVE；skytable 的 bb8 validation 把 stale reply 当成自己的
  reply；aiosmtplib 的下一次 DATA 把残留 250 当成拒绝。
- **池如果读了状态并 reset，就能修复**：psycopg pool rollback、asyncpg pool
  reset 都是正例；但 **skytable 的 pool 不修复**——`connections_created=1`、
  `closed_broken=0`，checkout/validation 只是把 stale reply 往后推一个请求。
  所以“池”本身不是修复，只有“读状态并 reset”的池才是。
- **“报告的 result 与共享对象实际状态不一致”是比“取消”更准确的抽象**：
  Linux multicast 是纯 success-without-effect，也被这个抽象覆盖。
- **状态字段本身也要被审计**：psycopg 加 `in_tx` 后 M1 12/18；asyncpg #3
  加 `in_tx` 把合法 Post 变 Neither；所以“加状态”既能抓漏，也能误判，
  必须把“丢写”和“stale-state-only”分开。

### 反驳 / 边界

- **不是所有池都会把脏连接交出去**：orientdb 0/120 主阶梯（弱阴性）、
  psycopg pool 0、asyncpg pool 0；但 skytable pool 是反例（35/72，且
  validation 消费 stale reply）；把“session pool 必然脏”或“pool 必然干净”
  当普遍规律都是错的。
- **不是所有缺陷都涉及取消**：multicast 没有取消、没有异步；窄版共性
  （“async cancellation leaves unfinished exchange”）无法解释它。
- **共性不能预测具体位置**：它预测到了 skytable 这个客户端“可能违规”，
  但没有预测到是 **bb8 validation 消费 stale reply**、也没有预测到
  `____ip_mc_inc_group()` 的 `kzalloc` 会被吞；具体函数/分支仍要靠源码
  审计和实验定位。
- **“信号存在”不等于“信号正确”**：asyncpg 的 `is_in_transaction()` 在
  手写 BEGIN 下会报 `False`（BEGIN 还在队列）；psycopg 的 `INTRANS` 是对的
  但没人读；aiomysql 直接 fail-fast。三者都说明需要**独立观察者**，不能只信
  客户端自报状态。
- **B2/B3 的“not-error”实现有缺口**：aiomysql 的返回值错误被漏掉；零违规
  不能直接读成“重试可用”。

### 一句话

共性作为**契约/oracle 理论**是成立的：`Ok` 之后要问“共享对象的实际状态
是什么、下一个决策者能不能看到”。作为**缺陷位置预测器**是不成立的：
它给的是搜索方向和观测清单，不是具体答案。

---

## 10. 修复、提交、剩余问题

### 10.1 修复

- **Linux multicast**：已在独立 v7.3-rc2 worktree `~/dropwise-linux-r4` 上
  做最小修复：`____ip_mc_inc_group()` 返回 `int`，`kzalloc` 失败返回
  `-ENOBUFS`；`__ip_mc_join_group()` 先建组再链入 `mc_list`，失败
  `sock_kfree_s()` 并返回错误。original `ret=0; igmp_group=0`（5/5
  VIOL_MIXED）；fixed `ret=-1/ENOBUFS; igmp_group=0`（5/5 OK_PRE，0
  violation）。site 1/3/4 行为不变。其他 10 个 target 尚未在 fixed 内核上
  全量重跑。
- **httpx**：上游已知相关（#2821 / httpcore #726），不在本轮提交；源码窗口
  已定位但未做 patch contrast。
- **asyncpg/psycopg/aiomysql direct**：不提交库补丁；记录为调用方/pool
  契约，给出最小恢复（`rollback()` / `reset()` / 丢弃 handle）。
- **aiosmtplib**：不提交库补丁；记录为 SMTP 响应关联族的新客户端实例；
  候选修法是取消时关闭/重同步连接。
- **skytable**：确认客户端 response-association 缺陷，不提交库补丁；候选
  修法是取消时丢弃/重连连接，pool 在 return 时校验并 discard；未做上游
  检索和 patch contrast。
- **mmap/mremap/mprotect**：契约外，不提交。

### 10.2 本地提交

本轮只做本地逻辑提交，不 push、不建 issue/PR；历史 BOLT 日志和 Phase 4
冻结数据不动。提交内容应包含：
- `prospective/phase4-exploration/ROUND3-*.md`；
- 新客户端源码、SCENARIO/RESULTS-FRAGMENT、run 脚本、小日志；
- Python cases、日志；
- `research/linux/harness/dw_harness.c`、`run.py`、新日志、`RESULTS-round4.md`；
- 不包含 `target/`、`__pycache__`、历史 BOLT 日志、Phase 4 数据。

### 10.3 剩余问题

1. skytable 已确认 35/72，但上游检索、patch contrast、pool>1、pipeline、
   TLS、server restart 未做；不能声称“已修复”或“已知/新”。
2. gel 已完成四层 ladder（§12.10）；仍只有 pooled/single 模式，无 direct handle。
3. modbus/async-pop 未开始；aiomysql pool、mysql-connector、pymongo、
   neo4j、curl-cffi 未跑。
4. orientdb 的 2 条物理流无法排除“stale reply 被另一条流掩盖”；需要
   单物理流控制或协议级 request id 观察。
5. Linux `TCP_ULP` / `prctl` / `io_uring` / timerfd / inotify / msgrcv 未跑；
   不能写成 clean。
6. multicast 只有 fault-injection 触发；没有 MEMCG/真实内存压力 trigger；
   需要真实 trigger 才能评估线上频率。
7. asyncpg/psycopg/aiomysql 的 direct 状态泄漏需要查文档/上游 issue，确定是
   “调用方义务”还是“库承诺自动 reset”；目前按契约缺口记录，aiomysql 是
   候选/未决。
8. 修复后的内核已做 26 个 target × 1 rep 的 stock/fixed 回归：ip target
   从 1 violation 变 0，site 1/3/4 行为不变；其他 target 的 pre/post 与
   violation 数不变（mmap/mremap/mprotect 仍是契约外，epoll 的
   `ok_poisoned` 是既定分类）。没有新失败；但只有 1 rep，仍建议在最终
   提交前保留这次回归记录。
9. `level_retry` 只看逃逸异常/字符串 `ERR`，aiomysql 的返回值错误被漏；
   需要修 engine 或让 case 重新抛出。

### 10.4 下一步

- 补 multicast fixed 内核上的其他 10 个 target 回归；
- 再补 skytable 的上游检索/patch contrast，以及 gel / aiomysql pool /
  mysql-connector / modbus / async-pop 的**可复现**阶梯；
  （§12 已补：gel 96 trials、mysql-connector、modbus/pop、aiomysql pool；
  skytable patch contrast 已做，upstream 仍未确认。）
- 把“状态字段”加入所有 M1 observation（`in_tx`、`transaction_status`、
  `igmp_group`、连接池 Active/Idle/Queued），同时把“丢写”和
  “stale-state-only”分开报；
- 修 `level_retry`：检查 `normalize(value)` 的错误结构，不只检查字符串；
- 在 Linux 侧把“void 辅助函数吞掉可失败分配”做成 Coccinelle/sparse 规则，
  这是本轮证明有效的搜索启发式；
- 报告时继续按**独立根因类别**计数，不按 trigger/组合/管理器/失败计划计数。

---

## 11. 对原问题的最终回答

**能指导“发现”，而且在“客户端家族”这一层做到了事先预测；但不能保证
“具体函数/具体分支”的预测。**

- 如果“指导”指的是：告诉我们去哪个**客户端家族/交接点**下刀、用什么观察、
  什么样的结果可疑，答案是**能**。这一轮的 skytable（预测 direct/pool 可能
  违规，实测 **35/72** + generated **84/120**）、asyncpg M1 9/9、psycopg
  M1 12/18、aiomysql M1 9/9、aiosmtplib B2/B3/M1 12/12、httpx 的 pool slot
  泄漏，都是这个指导的产物；其中 asyncpg/psycopg/aiomysql 是“只有 M1 能
  看到”的干净样本。
- 如果“指导”指的是：在不知道实现的情况下，事先说出“下一个 bug 在哪个
  **函数/哪一行**”，答案是**不能**。skytable 预测到了“这个客户端会坏”，
  但没预测到是 **bb8 validation 消费 stale reply**；multicast 根本没被
  预测到；orientdb 预测违规但 0/120 主阶梯（弱阴性）；mmap/mremap/mprotect
  虽然行为命中，却是契约外。**新的独立机制类别**（multicast）来自按广义
  形状做源码审计，不是盲预测。
- 所以共性的正确用法是：**把它当作契约设计原则和搜索/oracle 启发式**，
  先问“谁报告了成功、谁拥有共享状态、谁决定复用、他读不读这个状态”，
  再用独立观察者去验证。它不能替代源码阅读、契约判断和覆盖说明。

一个更精确的版本是：

> 共性预测“哪类客户端/哪类交接点值得怀疑、哪类 `Ok` 不可信、需要把哪些
> 状态放进观察”，并能在新客户端上**发现已知类别的新实例**（skytable）；
> 它不预测“具体是哪个函数/哪一行”，也不保证每个预测都命中（orientdb
> 弱阴性、mmap 契约外、multicast 未被预测）。

---

## 12. 二次校正与新增覆盖（2026-10-21 深夜）

> 本节是 append-only 校正。与 §0/§1/§7/§8/§9 冲突处，以本节和
> [ADAPTER-AUDIT-ROUND3.md](ADAPTER-AUDIT-ROUND3.md) 为准。旧的日志不删除，
> 但在表格里标明“无效/语义盲/弱阴性”，不把旧数字和新数字混加。

### 12.1 适配器错误吞噬：B2/B3 的 0 不再自动等于“没有后果”

第一轮 `aiomysql`、`asyncpg` 的 `_tx_write()` 调用了 `bound()` 却不检查
返回值：失败阶段可能返回 `None`（aiomysql/asyncpg）或只被放进普通 dict
（psycopg 旧版），`level_retry` 只找 `"ERR "` 字符串，于是把失败当成功。
这不是“取消没有后果”，是**检测层假阴性**。

修复不是改 `Expect`，而是把操作结果结构化：

- `research/python-retro/dropwise_py/engine.py` 新增
  `OpResult(ok, stage, value, error)` 与 `operation_failed()`；
  `level_retry` 按结构化的 `ok`（或显式 `{"ok": false}`）判失败，不再靠
  错误字符串猜；
- 每个 adapter 的 `target()` 在未取消时若关键阶段失败就抛出（不能让失败
  变成合法 post）；`same()/different()` 返回 `OpResult` 或抛出；
- setup/warmup/acquire 失败必须抛出；cleanup 可以 best-effort，但要记录；
- 无服务回归测试
  `research/python-retro/tests/test_ladder_outcomes.py` 10/10 通过：
  `OpResult(ok=False)`、dict `ok=False`、异常、超时都算违规；
  `ok=True` 不算；setup 异常直接传播；target 异常记 baseline error。

**不能继续把 aiomysql/asyncpg 的旧 B2/B3 0 当作“只有 M1 能抓到”的证据。**
这条结论在 `ADAPTER-AUDIT-ROUND3.md` 里有逐包审计。

### 12.2 校正后的 Python 结论（含新增包）

| 包/模式 | 结果（3 reps） | 分类 |
|---|---|---|
| aiomysql direct | B1 0/9；B2 9/9、B3 9/9 都是显式 `InterfaceError: Cancelled during execution`；M1 9/9 Neither（survivor 也报错、observer 空） | **合法 fail-fast / 契约缺口**，不是静默污染；不计协议根因 |
| aiomysql pool | B1/B2/B3/M1 全 0/12；M1 Pre 6/Post 6；失效连接被 pool 丢弃，survivor 新 cid、`own=[[7]]` | **clean self-heal 正例** |
| mysql-connector 26.7.0 direct | B1 0/12；B2 12/12、B3 12/12 结构化 fail-fast；M1 12/12 Neither：6/12 丢写/未提交，3/12 已提交但误报，3/12 反序列化错位 | **候选客户端缺陷**；M1 才看到静默后果 |
| psycopg 3.3.6 direct | B1/B2/B3 0/18；M1 12/18（6 Post、12 Neither）；B2/B3 的值里已有 `status_after_acquire=INTRANS`，但 level 只看逃逸错误 | **direct-handle 静默泄漏**；B2/B3 语义盲，M1 才是判据 |
| psycopg pool | B1/B2/B3/M1 全 0/18；release rollback INTRANS→IDLE | **clean/self-heal 正例** |
| asyncpg 0.32.0 direct | 新 B1-B3 0/9、M1 **9/9**；k1 `in_tx=False` + observer []、k2 `in_tx=True` + []、k3 `in_tx=True` + observer 有 survivor+target（仅 stale state） | direct-handle 族；6/9 丢写 + 3/9 stale-state；B2/B3 语义盲，M1 判据 |
| asyncpg pool | 新 B1/B2/B3/M1 全 0/12；M1 Pre 6/Post 6；最终`run.WcLcOp` 24/24 pool 对照无 timeout | 本阶梯 clean；不写成普遍保证 |
| Gel 0.11.0（pooled/single，`max_concurrency=1`） | 4 levels × 3 reps，24 plans/level，96 cut trials；全 0；M1 Pre 21/Post 3；observer/own 全部一致；Post 是 COMMIT reply 已处理的边界 | **阴性/契约缺口**；连接 dirty 被 pool 丢弃；不能把取消当 rollback |
| redis-py 8.1.0（pool/direct × tx/set） | 120 trials，全 0；M1 Pre/Post 合法；DISCARD/UNWATCH/reconnect 对照 10/10 | 阴性；本场景 clean |
| pymongo 4.19.0 | 4 units，144 trials，全 0；M1 Pre/Post 合法；cancel 后 socket discarded/reconnect | 阴性；self-heal 正例 |
| neo4j 6.4.1 | 4 units，144 trials，全 0；M1 Pre/Post；连接归还并复用 | 阴性；self-heal 正例 |
| aiohttp 3.14.5 | B1-B3 0/3、M1 0/3（Post）；yields=1，只覆盖已经写出的 POST | **弱阴性**；覆盖不足 |
| curl-cffi 0.16.3 | B1-B3 0/3、M1 0/3（Pre）；yields=1，cut 在 libcurl 发送前；mid-flight probe 3/3 Post 但不算阶梯 | **弱阴性**；覆盖不足 |
| httpx 0.28.1 | 已知上游 #2821/httpcore #726；B2/B3/M1 3/18 | 已知上游实例，不是新根因 |
| aiosmtplib 5.1.3 | B2/B3/M1 12/12，`SMTPDataError: (250,'OK')` 或 `TimeoutError` | 已知 SMTP 响应关联族的新客户端实例 |

两点必须同时说：

1. **B2/B3 的失败不自动等于 bug。** aiomysql direct 的 9/9 是显式拒绝，
   没有 stale success；把它计成协议缺陷会夸大共性，写成 clean 又会掩盖
   “direct handle 需要换连接”的契约缺口。正确类别是 **fail-fast /
   caller obligation**。
2. **B2/B3 的 0 也不自动等于 clean。** psycopg direct 的 B2/B3 0 是因为
   `same()` 真的成功，但值里已有 `INTRANS`；M1 才显示后继写进了未提交
   事务。B2/B3 只看逃逸错误，语义上是盲的。

### 12.3 新增 Rust 客户端：modbus 与 async-pop

**tokio-modbus 0.17.0（direct `client::Context`，MBAP server，50 ms proxy）**

| level | plans | violations |
|---|---:|---:|
| B1 | 2 | 0 |
| B2 | 2 | **2** |
| B3 | 2 | **2** |
| M1 | 2 | **2（Neither）** |

反例：取消 `write_single_register(1,1)` 后 server 已应用
（`before=[100,1]`）；延迟 reply（func 6、txid 1）留在 client；下一条请求
得到 `mismatching headers: expected txid 2, actual txid 1
Ok(WriteSingleRegister(1,1))`。M1 survivor 的写也在 server 生效
（`visible=[100,7]`），但 client 报错、own 读也错位。**确认的
response-association 候选，契约外，upstream unknown，未做最小 patch。**

**async-pop 1.1.3（direct `Client<tokio TcpStream>`，loopback POP3 server）**

| level | plans | violations | 说明 |
|---|---:|---:|---|
| B1 | 8 | 0 | — |
| B2 | 8 | **2** | plan `#0@3` retry login `Err("Did not received the expected user response")` |
| B3 | 8 | **6** | `#0@1`、`#0@2`、`#1@1` 的 capa `Err("Did not received the expected capa response")` |
| M1 | 8 | **2 clean + 6 unresolved** | 只有 2/2 retr-target 的 pre==post 干净；6/6 login-target 的 pre 是未认证 `-ERR`，明确不计违规 |

GreenMail 2.1.14 上 `async_pop::new+login` 会挂起：`PopStream::decode`
在 `Incomplete` 后设 `decode_needs`，成功后没复位，GreenMail 的两段 CAPA
让下一条短 `+OK` 永远不完整。这是**与取消无关的独立客户端 bug**，不并入
本轮共性计数；改用允许的 loopback POP3 server。`run.7q9n0j` 标为 INVALID
保留；`run.WKXVqY` 是有效运行。

### 12.4 skytable：已完成触发链/源码/修复对照（范围内确认的行为缺陷；upstream unknown）

S1 的 deep pass（`clients/skytable/RESULTS-FRAGMENT-DEEP.md`，
`logs/run.CncmjF/`）把触发链和修复对照补齐了：

- **协议无 request id**：Skyhash 2 只有位置关联（docs 明确“按发送顺序返回”）；
  没有 request/transaction id，客户端在 drop 后无法重新同步。
- **direct**：`query()` 的 response 状态是局部变量，`self.buf` 持久；drop 在
  read 阶段留下一个 pending reply（下一个请求错位）；drop 在 write 阶段留下
  半包（server error 6 = Illegal client packet，是**截断请求**，不是 stale
  read）。两种 cut 点，同一“连接协议位置未知”根因。
- **pooled**：bb8 `is_valid` 发 `sysctl report status`；它消费 stale
  `0x12`（Empty）并当成成功，然后 survivor 的 update 消费 validation 的
  reply，survivor select 读空。pool 的 `has_broken` 硬编码 `false`，所以
  `connections_closed_broken=0`，同一条流被还回。**validation 不是修复，
  而是把错位往后推一格。**
- **契约**：docs/README 没有 cancel/drop/timeout/reuse/reset 语义；
  `reset_buffer` 只是 buffer shrink；协议也没有 correlation id。属于
  **silent contract**，不是 “contract-external”（没有页面说“不保证”）。
- **upstream**：client-rust 34 issues/PRs 无匹配；crates.io 最新仍 0.8.12；
  HEAD 同版本；upstream unknown（未联系）。
- **最小修复**（dependency copy，不改 Dropwise core）：`query()` 加
  `PoisonOnDrop` guard；drop/fail 时 poison；`is_valid`/`has_broken` 拒绝
  poison；pool 丢弃并重连。patch 见 `fix/drop-poison.patch`。
- **原版/修复对照**：pooled 原版 **27/48**，修复 **0/48**；direct 原版
  **15/24**，修复仍是 18/24，但全部是显式
  `IoError(ConnectionAborted, "...connection poisoned")`，**fail-closed，
  不是 stale Ok，也不是 survivor 写成功**。修复后 direct 需要换连接，这是
  预期，不是 regression。

措辞：**现象在测试范围内确认；根因是客户端连接协议位置未知；contract
silent（契约缺口）；修复对照成立；但 upstream 未确认，所以对外仍称
“已复现并已做修复对照的复用失败候选”，不是“确认的新 upstream bug”。**
旧 `run.VlevMa` 的 35/72 与新 run 的 42/72 是同一根因的时序差异，不相加。

### 12.5 Linux multicast：从“设备记录缺失”到真实投递

新增 `research/linux/RESULTS-round4-multicast.md` 和
`logs/20261021-r4-mcast-delivery/`。在隔离 QEMU guest 的 `lo` 上真实
`sendto`/`recv`：

| 场景 | join | 设备组 | 单播 | 组播 |
|---|---|---|---|---|
| v6.19 stock normal | 0 | users=1 | 1/1 | **4/4** |
| v7.3-rc2 stock normal | 0 | users=1 | 1/1 | **4/4** |
| v7.3-rc2 fixed normal | 0 | users=1 | 1/1 | **4/4** |
| v6.19 stock N=2 | **0** | absent | 1/1 | **0/4** |
| v7.3-rc2 stock N=2 | **0** | absent | 1/1 | **0/4** |
| v7.3-rc2 fixed N=2 | **-1/ENOBUFS** | absent | 1/1 | 0/4（clean） |
| v7.3-rc2 fixed N=2 后再 normal join | 0 | users=1 | 1/1 | **2/2** |
| 三个 kernel N=1 | -1/ENOBUFS | absent | 1/1 | 0/2（clean control） |
| stock SSM N=2 | **0** | absent | 1/1 | **0/2** |
| fixed SSM N=2 | -1/ENOBUFS | absent | 1/1 | 0/2（clean） |
| stock cross-socket close | — | **other group removed** | — | 0/2（其他 socket 的组被删） |
| fixed cross-socket close | — | other group survives | — | **2/2** |

这把“没有投递”从推断变成了观测。patch audit 覆盖：

- `sock_kmalloc`/`sock_kfree_s` 的 size 配对；失败时 `iml` 尚未发布，非 RCU
  free 安全；
- RTNL 只串行化 writer，不串行化 RCU reader；真正的安全理由是“失败时
  不发布 `iml`”，不是“RTNL 让所有读者看不到半成品”；
- duplicate join（同 socket `EADDRINUSE`）与 existing-group `users++`
  （第二个 socket）分开；SSM `IP_ADD_SOURCE_MEMBERSHIP` 走同一失败路径；
- `void __ip_mc_inc_group()` 的其它调用者（`ip_mc_up`、GRE、bridge、
  IB CMA）没有错误通道，保留 void 是可辩护的最小范围；但 patch 注释
  “下一次 IGMP query 会修复”**不成立**：`igmp_heard_query()` 只遍历已存在
  的 device list，缺组不会被访问；只有新的 join/interface-up 才可能重建。
- 上游：本地 `origin/master` 仍是 void 签名和 publish-before-inc；没找到
  等价修复；lore 查询 403，不能证明“上游没有”。

分类：**fault-injection-only 路径上的确认契约违反**（成功返回但无设备组，
真实 0/4 投递），外加 cross-socket 误删的更强后果；不声称真实世界频率，
也不声称内存破坏/安全影响。措辞修正见该文件 §6。

### 12.6 阴性/弱阴性的边界

- **orientdb 0/120** 仍是弱阴性：2 条物理 TCP 流；不能写 clean。
- **gel 旧 3/3 probe** 已被 §12.10 的 96-trial ladder 取代；仍只有 pooled/single 模式。
- **redis/pymongo/neo4j** 的 0 是本场景、本 pool 大小、单次取消下的阴性；
  它们都观察到 pool 丢弃/重连或事务重置，是 clean self-heal 正例，不是
  “所有池都安全”。
- **aiohttp/curl-cffi** 的 0 是弱阴性：yields=1，cut 点分别只覆盖“已写出”
  和“发送前”；curl 的 mid-flight probe 3/3 是补充观察，不是阶梯。
- **modbus/pop/skytable** 的阳性都是“客户端 response association”族的
  新实例，不是新的独立机制类别；Linux multicast 才是本轮新的独立类别。

### 12.7 修正后的计数口径

- **新的独立机制类别：1 个**——Linux multicast 的 void 辅助函数吞掉可失败
  分配，成功返回但设备组不存在（真实 0/4 投递）。
- **协议一致性候选（静默/错误效果，不等于已确认新根因）**：
  skytable（已做修复对照的候选：原版 42/72，generated 84/120；修复
  pooled 0/48，direct 18/24 显式 fail-closed）、modbus（候选，B2/B3/M1
  各 2/2）、async-pop（候选但仅 2 个干净 retr M1 + 2/8 B2 + 6/8 B3；
  login M1 unresolved）、mysql-connector（候选，M1 12/12）、psycopg
  direct（契约缺口，M1 12/18）、asyncpg direct（有效 M1 9/9，见 §12.9）。
- **合法 fail-fast / 契约缺口，不计协议根因**：aiomysql direct
  （B2/B3/M1 全显式 `InterfaceError`；pool clean）。
- **已知上游/已知族的新实例**：httpx（#2821/#726）、aiosmtplib（SMTP
  响应关联）。
- **clean/弱阴性**：redis、pymongo、neo4j、aiohttp、curl-cffi、orientdb
  （弱）、http-client（运行时排除）。
- **执行次数不是根因数**：42/72、84/120、12/18、9/9、12/12、6/8 都是
  plans/trials；不能相加成“多少个 bug”。

### 12.8 对“预测”的措辞修正

`ROUND3-PREDICTIONS.md` 是同一工作期的**工作记录**，不是预注册，也不是
盲测；不能从同一 commit 的文档计算“预测准确率”。本节只把它当作搜索
记录来反思：

- 家族级“skytable/direct handle 可能有问题”被想到，且确实找到了；
- 但具体到“bb8 validation 消费 stale reply”“mysql-connector 的
  `unread_result` 在 socket read 之后才置位”“pop 的 `decode_needs`
  不复位”，都不是预测出来的，而是源码审计/实验定位的；
- Linux multicast 完全未被预测，是按“成功但没效果/可失败子步骤被忽略”
  的广义形状做源码审计找到的；
- 因此可以写“共性指导了搜索方向和观测清单”，不能写“我们准确预测了
  下一个 bug”。

### 12.9 asyncpg/psycopg 重跑与责任边界对照（阶梯 `run.1HLY38`；对照 `run.WcLcOp`）

**有效阶梯（修复 `OpResult`/诊断后）：**

| 包/模式 | B1 | B2 | B3 | M1 |
|---|---:|---:|---:|---:|
| psycopg 3.3.6 direct | 0/18 | 0/18 | 0/18 | **12/18**（12 Neither，6 Post） |
| psycopg 3.3.6 pool | 0/18 | 0/18 | 0/18 | 0/18（Pre 12，Post 6） |
| asyncpg 0.32.0 direct | 0/9 | 0/9 | 0/9 | **9/9**（Neither） |
| asyncpg 0.32.0 pool | 0/12 | 0/12 | 0/12 | 0/12（Pre 6，Post 6） |

- asyncpg direct 的 9/9 是**有效**的，且要拆开：**6/9 真丢写**
  （k1/k2 observer `[]`；k1 `in_tx=False` 是客户端字段，server 还看不到
  BEGIN；survivor 写后 `in_tx=True`/server `idle in transaction`），
  **3/9 仅 stale state**（k3 observer 已有 survivor+target，Post 形状，
  但 acquire `in_tx=True` 是 stale）。k1 是“`BEGIN` 还在队列里”的强样本，
  不是“没有事务”。B2/B3 0 是因为 `same()` 确实成功（不是
  旧 `_tx_write` 返回 `None` 的假阴性）；B2/B3 的 value 里可见状态，但
  level 只看逃逸错误，语义盲。
- asyncpg pool 0/12：M1 Pre/Post 合法；最终 `run.WcLcOp` 的 24/24 pool
  对照全部 acquire 成功、无 timeout。早先 `run.1HLY38` 的“timeout”是
  **采样顺序伪影**（server sample 放在 release 前，第二次取消跳过 release），
  该旧对照作废；不要把它写成 pool 的时序噪声或缺陷。pool clean 仍只对
  本场景/本阶梯，不写成普遍保证。
- psycopg pool 的 `cm_target` k=1 有一次 target error：`putconn()` 发现
  cancelled transaction block 留下不可用连接，`returns_bad=1` 丢弃并重连
  （新 pid），survivor/observer 正常。这是 **pool 自修复正例**，不是 target
  违规。
- psycopg direct 12/18；B1-B3 0 也是语义盲：B2/B3 的值里已有
  `status_after_acquire=INTRANS`，`same()` 却成功。pool 0/18，release
  rollback INTRANS→IDLE。

**责任边界/恢复对照（BOUNDARY 事件 + 真实 cut）：**

- **手写 SQL、什么也不做**：psycopg 的 cut yields=8–11（plans #1–#4）
  留在 `INTRANS`，survivor write `Ok`、observer `[]`（12/18 的 Neither
  就是这个形状）；yields=12–13（plans #5–#6）cut 在 COMMIT 之后，
  `IDLE`、observer 有 target+survivor（合法 Post）。
  asyncpg k1 `in_tx=False` + observer `[]`（`BEGIN` 还未发出/在队列里），
  k2 `in_tx=True` + `[]`；k3 `in_tx=True` + observer 有 target+survivor
  （合法 Post）。
- **显式 `rollback()`**：psycopg `INTRANS→IDLE`，observer 只有 survivor；
  asyncpg `ROLLBACK` 同样恢复。**这是有效的应用层修复。**
- **`reset()`**：asyncpg 的 `reset()` 在 k1（`in_tx` 字段 False、BEGIN
  还在队列）**没有清掉**：`in_tx_after=True`、observer `[]`；k2/k3 能清。
  所以“有 reset API”不等于“reset 覆盖所有 cut 点”，必须按状态采样点
  分开报。**显式 SQL `ROLLBACK` 在 k1 也能恢复**（会 flush/终止 queued
  BEGIN），不要把 reset 失败写成“无法恢复”。
- **官方 transaction context manager**：psycopg `cm_target` plans #1/#2
  仍是 `INTRANS` + observer `[]`；更严重的是 `cm_retry` plans #1–#4 返回
  **`OpResult(ok=True)` 但只提交了 savepoint**，observer `[]`，即官方块
  在已经 `INTRANS` 的句柄上**静默地只保存点提交**。asyncpg `cm_retry`
  k2/k3 返回**显式** `InterfaceError: cannot use Connection.transaction()
  in a manually started transaction`（fail-fast，不是静默）；k1 的
  `cm_retry` Ok 且 b3 可见；但 `cm_target` k1 仍 observer `[]`（丢写）。
  **context manager 不是 cancellation 后的自动恢复保证**；这与“用官方
  推荐写法就安全”的直觉相反，必须按“静默丢写” vs “显式拒绝”分开。
- **close+reconnect**：psycopg/asyncpg 都恢复（psycopg 新 backend pid；
  asyncpg 新连接），observer 只有 survivor；这是最后的安全退路。
- **时序**：`BOUNDARY` 记录 target acquire → BEGIN → cancel → bounded
  cleanup（先 release；direct 的 release 是 no-op）→
  `server_after_release`（`pg_stat_activity`）→ cut returned → survivor。
  `in_tx` 字段与 server 查询是两次异步采样，**不是原子快照**。asyncpg
  k1 的 `False` 是客户端字段，server 在 release 时还看不到 BEGIN，
  survivor 写之后才看到 `idle in transaction`；不能把“字段 False”推成
  “没有事务”，也不能把“字段变了”直接判成 bug。
  `bound()` 用 `asyncio.wait_for`，但 CPython 3.13.13 正常路径
  `child_task=False`（environment.txt），cut 投递给同一个 `_CutTask`；
  这排除了“wait_for 创建子任务改变取消点”的解释。

**结论**：两边要分开归因，不能都写成“driver 状态 bug”：

- **psycopg direct**：**应用组合遗漏 / contract gap**——手写
  BEGIN→INSERT→COMMIT 没有 rollback；官方 CM 也不是所有 cut 点的修复，
  `cm_retry` 在已 `INTRANS` 句柄上甚至只提交 savepoint。它不证明
  psycopg driver 违反取消契约；正确结论是“调用方必须 rollback/reconnect，
  而文档没有把取消后的恢复义务写清”。
- **asyncpg direct**：**driver reported-state error + contract gap**——
  queued BEGIN 下 `is_in_transaction()` 报 `False`，但 survivor 写实际进入
  未提交事务；k2/k3 是字段 stale/已 in_tx。与已知 #537/#1310 的形状一致，
  但本实验不能证明 upstream 未修/已修。
- pool 的 release rollback/reset 是**正例**；psycopg release 时 server
  last query=ROLLBACK、IDLE、同 pid；asyncpg release 时 server 执行
  reset 查询、`in_tx=False`、同 pid。psycopg pool cm_target k1 的
  `the connection is closed` + `returns_bad=1` + 新 pid 是 pool 丢弃坏连接
  的正例，不计违规。
- B2/B3 的 0 是**检测/契约盲**：只说明没有异常逃逸，不说明事务状态干净；
  B2/B3 的报错也不自动等于协议缺陷（对照 aiomysql 的 fail-fast）。

### 12.10 Gel 补全：从 probe 到四层 ladder（阴性，但取消不等于 rollback）

Gel 0.11.0 之前只有 3 次 probe；本轮补成正式四层（`clients/gel/
RESULTS-FRAGMENT-LADDER.md`，`logs/run.Zlfofp/`）：

| level | plans/trials | violations | unknown | unrealized | concrete |
|---|---:|---:|---:|---:|---|
| B1 | 24/24 | 0 | 0 | 0 | 24 cancelled；baseline 3/3 |
| B2 | 24/24 | 0 | 0 | 0 | 24/24 retry `Ok((1,"target"))` |
| B3 | 24/24 | 0 | 0 | 0 | 24/24 read `Ok`；21 seed + 3 target |
| M1 | 24/24 | 0 | 0 | 0 | **Pre 21 / Post 3**，Mixed/Neither/Unknown 0 |

96 个 realized cut trials，12 个 level/rep 全部 `exhaustive=true`，baseline
errors 0；6/6 手工 calibration 两次采样一致；observer/own 的
`(7,"survivor")` 完全一致。**没有 violation。**

但要写清限制：Gel 0.11.0 **没有公开 direct handle**；`Client` 内部就是
pool，`max_concurrency(1)` 只是单连接池。取消后连接处于 dirty，
`PoolConnection::drop` 不归还，survivor 重新连接（`proxy_connections=2`）。
所以这是**单模式 pooled/single 的阴性**，不是 pooled vs direct 对照。

契约：`Client::transaction` 只说明闭包 `Ok` 提交、`Err` 重试/中止，对
**外层 future 被取消** 没有承诺。3/24 的 Post 都是 `#0@4:AfterWake`：
COMMIT 已被 server 处理，caller 的 future 被 drop，observer 看到
`(1,"target")`。这是“committed-despite-cancelled”的**提交边界二义性**，
不是 stale reply、不是 wrong value、也不是已确认 upstream bug；没有 patch。
调用方义务：不能把 timeout/cancel 当 rollback 证据，必须对账或做幂等。

结论上，Gel 是 **clean/negative**（本版本、本场景、本组 cut），但它把
“clean”与“取消后可安全假设回滚”区分开了。旧 probe 仍保留，新的 ladder
文件只追加 supersede 指针。

## 附录 A：关键文件

- 预测：[ROUND3-PREDICTIONS.md](ROUND3-PREDICTIONS.md)
- 硬规则：[ROUND3-BRIEF.md](ROUND3-BRIEF.md)
- orientdb：[SCENARIO.md](clients/orientdb/SCENARIO.md)、
  [RESULTS-FRAGMENT.md](clients/orientdb/RESULTS-FRAGMENT.md)、
  [logs/run.CI4tHK](clients/orientdb/logs/run.CI4tHK)
- gel：[RESULTS-FRAGMENT.md](clients/gel/RESULTS-FRAGMENT.md)
- skytable：[SCENARIO.md](clients/skytable/SCENARIO.md)、
  [RESULTS-FRAGMENT.md](clients/skytable/RESULTS-FRAGMENT.md)、
  [logs/run.VlevMa](clients/skytable/logs/run.VlevMa)
- Python PG/MySQL：[SCENARIO.md](python/pg-mysql/SCENARIO.md)、
  [RESULTS-FRAGMENT.md](python/pg-mysql/RESULTS-FRAGMENT.md)、
  [logs/run.KqRbB4](python/pg-mysql/logs/run.KqRbB4)
- Python HTTP：[SCENARIO.md](python/http-mongo-neo4j-smtp/SCENARIO.md)、
  [RESULTS-FRAGMENT.md](python/http-mongo-neo4j-smtp/RESULTS-FRAGMENT.md)、
  [httpx logs](python/http-mongo-neo4j-smtp/logs/run.uyIk2F)、
  [aiosmtplib logs](python/http-mongo-neo4j-smtp/logs/run.RpMq5v)
- Linux mass：[logs/20261021-r4-mass](research/linux/logs/20261021-r4-mass)、
  `aggregate.csv`、`summary.csv`
- Linux original/fixed：[fixed-orig](research/linux/logs/20261021-r4-fixed-orig)、
  [fixed-patched](research/linux/logs/20261021-r4-fixed-patched)
- Linux 报告：[RESULTS-round4.md](research/linux/RESULTS-round4.md)、
  [RESULTS-round4-multicast.md](research/linux/RESULTS-round4-multicast.md)
- 适配器审计：[ADAPTER-AUDIT-ROUND3.md](ADAPTER-AUDIT-ROUND3.md)
- 新增 Python：[pg-mysql RESULTS-FRAGMENT-round3b.md](python/pg-mysql/RESULTS-FRAGMENT-round3b.md)、
  [redis RESULTS-FRAGMENT.md](python/redis/RESULTS-FRAGMENT.md)、
  [mongo/neo4j RESULTS-FRAGMENT.md](python/mongo-neo4j/RESULTS-FRAGMENT.md)、
  [http/smtp RESULTS-FRAGMENT-ROUND3-TARGETS.md](python/http-mongo-neo4j-smtp/RESULTS-FRAGMENT-ROUND3-TARGETS.md)
- 新增 Rust：[modbus RESULTS-FRAGMENT.md](clients/modbus/RESULTS-FRAGMENT.md)、
  [pop RESULTS-FRAGMENT.md](clients/pop/RESULTS-FRAGMENT.md)、
  [skytable deep](clients/skytable/RESULTS-FRAGMENT-DEEP.md)、
  [Gel ladder](clients/gel/RESULTS-FRAGMENT-LADDER.md)
- Linux 投递/补丁审计：[RESULTS-round4-multicast.md](research/linux/RESULTS-round4-multicast.md)

## 附录 B：计数口径

> 第一轮口径，保留；冲突以 §12.7 与附录 C 为准。

- **独立根因类别**：按“同一个机制”计，不按 target/trigger/rep/客户端/管理器计。
- 本轮的机制表：

| 机制 | 本轮状态 | 证据 | 计数 |
|---|---|---|---|
| void 辅助函数吞掉可失败分配 → success-without-effect | **新类别** | Linux `ip_add_membership` 5/5；original/fixed | **1 个新类别** |
| direct handle 留下未提交事务 | 已知类（BOLT direct 历史）；新实例 | psycopg 12/18、asyncpg M1 9/9（k2/k3 6/9 遗留；k1 3/9 queued BEGIN） | 同一类，不算 3 个 |
| direct handle 的 queued BEGIN / 状态报告为 False | asyncpg #1 的子机制；比“遗留事务”更强 | asyncpg 3/9 | 归入 direct-handle 类，单列子机制 |
| direct handle 的 sticky cancel flag / fail-fast | aiomysql 新实例；无静默写 | aiomysql 9/9 | 同一族的 fail-fast 变体 |
| SMTP DATA/354/250 响应关联 | 已知族（async-smtp 第二轮）；新客户端 | aiosmtplib 12/12 | 同一类的新实例 |
| 客户端 response association 被取消破坏 | 已知族（BOLT/SMTP）；新 Rust 客户端 + pool 不自治愈 | skytable 原版 42/72 + generated 84/120；修复 pooled 0/48、direct 18/24 fail-closed | 同一类的新实例（已做修复对照） |
| httpcore cancellation cleanup 窗口 | 已知上游相关（#2821/#726）；新源码定位 | httpx 3/18 | 同一类的新实例 |
| 部分破坏性失败（mmap/mremap/mprotect） | 契约外 | 5/5、5/5、10/10 | **不是 bug**，不算根因 |
| pool reset/rollback | clean 正例 | psycopg/asyncpg pool 0 | 不是 bug |

- **一句话计数**：
  - **真正新的独立机制类别：1 个**（Linux multicast 的 void 辅助函数）。
  - **新观察到的客户端实例/候选（行为可复现，根因确认程度不同）：8 个**——
    psycopg direct 12/18（契约缺口）、asyncpg direct 9/9（已知上游相关）、
    aiomysql direct 9/9（fail-fast，非静默）、aiosmtplib 12/12（同 SMTP 族）、
    skytable 原版 42/72（response association，客户端缺陷 + patch）、
    mysql-connector M1 12/12（候选）、modbus B2/B3/M1 各 2/2（候选）、
    async-pop B2 2/8 + B3 6/8 + 2 clean M1（部分候选）；httpx 是已知上游
    相关的第 9 个实例；Gel 96 trials 0 是阴性，不计入候选。
    **“实例”不等于“新根因”**。
  - **不要**把 psycopg/asyncpg/aiomysql 的 3 个家族直接数成 3 个新根因：
    它们同属 direct-handle 族，只是三个变体（遗留事务 / queued BEGIN /
    fail-fast）。其中 asyncpg #1 和 aiomysql sticky 可以单列**子机制**，
    但不是三个互不相干的新类别。
  - **skytable 原版 42 个 violating plans** / 72 是执行次数，不是 42 个
    bug（旧 35/72 是同一根因的时序差异）；**30 个 Python violating
    trials**（12+9+9）也是执行次数，不是 30 个 bug；
    `mmap` 5 次、`mremap` 5 次、`mprotect` 10 次是同一契约类的 20 次执行，
    不是 20 个 bug；ip 的 5 个 rep 是同一个 bug 的 5 次复现。
- 弱阴性/未覆盖：orientdb 0/120（2 条物理流）、aiohttp/curl-cffi
  （yields=1）；Gel 96 trials 0，但只有 pooled/single 模式；aiomysql pool、
  pymongo、neo4j、psycopg/asyncpg pool 是 clean 正例。mysql-connector/
  modbus/async-pop 是候选，不是 clean。以上都不写成“已证明安全”。
## 附录 C：二次校正后的计数（取代附录 B 的冲突项）

> 附录 B 保留第一轮口径；与本节冲突时以本节为准。**执行次数不是根因数**。

| 类别 | 对象 | 证据 | 计数 |
|---|---|---|---|
| 新的独立机制类别 | Linux `ip_add_membership`/SSM 的 void 辅助函数吞掉分配失败 | 真实 UDP：normal 4/4；stock N=2 **0/4**；fixed N=2 `-ENOBUFS` 0/4；cross-socket close 误删 | **1 类** |
| 协议一致性（已做修复对照） | skytable 0.8.12 | 原版 pooled 27/48、direct 15/24；修复 pooled 0/48、direct 18/24 显式 fail-closed；35/72 与 42/72 是同一根因时序差异 | 范围内确认的客户端缺陷；upstream unknown |
| 协议一致性候选 | tokio-modbus 0.17.0 | B2 2/2、B3 2/2、M1 2/2；stale txid；survivor 写已生效但报错 | 候选，契约外 |
| 协议一致性候选 | async-pop 1.1.3 | B2 2/8、B3 6/8；M1 仅 2/2 retr 干净，6/6 login unresolved | 候选，部分 |
| 协议一致性候选 | mysql-connector 26.7.0 | B2/B3 12/12 fail-fast；M1 12/12 Neither：6 丢写、3 误报、3 错位 | 候选 |
| direct-handle 静默泄漏 | psycopg 3.3.6 direct | B1-B3 0/18；M1 12/18；`INTRANS` 在值里但 B2/B3 不看 | 契约缺口/实例 |
| direct-handle 静默泄漏 | asyncpg 0.32.0 direct | 新 B1-B3 0/9、M1 9/9；k1 `in_tx=False` + observer []; B2/B3 语义盲 | 契约缺口/实例 |
| 合法 fail-fast（不计协议根因） | aiomysql 0.3.2 direct | B2/B3/M1 全显式 `InterfaceError`；observer 空；无 stale success | 契约缺口 |
| clean/self-heal 正例 | aiomysql pool、psycopg pool、asyncpg pool、redis、pymongo、neo4j | 0 违规；pool 丢弃/重连或 rollback/reset | 阴性，不是“绝对安全” |
| 阴性（单模式） | Gel 0.11.0 | 96 trials 0；M1 Pre 21/Post 3；COMMIT 边界 Post 允许；无 stale/wrong | 阴性；不是 pooled/direct 对照 |
| 已知上游/已知族 | httpx、aiosmtplib | httpx #2821/#726；SMTP DATA/354/250 | 已知，不是新根因 |
| 弱阴性/未覆盖 | orientdb、aiohttp、curl-cffi、http-client H1Client | 2 条物理流/yields=1/运行时边界排除 | 不计 clean |

- **协议一致性候选的“根因”数**：不能把 skytable/modbus/pop/mysql-connector/
  psycopg/asyncpg 各算一个新根因；它们大多是 **response association /
  direct-handle state** 两个已知族的新实例。真正新的独立类别仍只有
  **Linux multicast 1 个**。
- **执行次数**：skytable 原版 42/72 + generated 84/120、psycopg 12/18、
  asyncpg 9/9、mysql-connector 12/12、modbus 6 个 plan violations、pop 10 个
  plan violations、aiosmtplib 36、httpx 9、Gel 96 clean trials 都是
  trials/plans，不能相加成 bug 数。
- **不把“B2/B3 报错”统一算违规**：aiomysql 的显式 `InterfaceError` 是
  fail-fast；mysql-connector 的 M1 静默后果才算候选缺陷。

