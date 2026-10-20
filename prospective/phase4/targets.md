# Phase 4 目标名单：三个分支（草稿，未冻结）

> 2026-10-19 修订。只查询公开数据、README/文档/公开 API；不读被测取消实现，不跑实验、不写场景、不启动服务。下载数字不是用量/生产用户数，也不是特定版本的下载数。纳入标准与统计定义见 `PLAN.md`。
>
> 完整行级判断在 `rust-selection.csv`（815 行、分别有 pool/handle 决定）和 `python-selection.csv`（月榜前 1,500 行、下载量降序）。本文件给名单、漏斗、边界和变化，不再保留与新规则矛盾的旧草稿清单。

## 1. 总数与选择漏斗

| 分支 | 主客户端族 | 主单元 | 条件客户端族 | 条件单元 |
|---|---:|---:|---:|---:|
| R-P：Rust 池化 | 6 | 11 | 2 | 3 |
| R-H：Rust 直接持有句柄 | 7 | 12 | 1 | 2 |
| P：Python（内部 pool/direct 分开） | 11 | 27（15 pool + 12 direct） | 3 | 6（4 pool + 2 direct） |

主名单 **50 单元**，去重为 **Rust 9 + Python 11 = 20 个客户端族**。条件全部通过才是 **61 单元、Rust 11 + Python 14 = 25 个族**。Rust 的 tiberius、IMAP、bolt-client、skytable 横跨 R-P/R-H；Python 的 redis、psycopg、asyncpg、aiomysql 双模式。它们不是独立样本，也不把多个 manager 当多个 crate。

### 1.1 R-P 漏斗

`universe.csv` 815 行 → 自身下载 ≥1,000/90d 的 **208 行**（607 低于）→ 已测/核心 **28**、包装合并 **5**、API/协议/服务端排除 **167** → **6 主客户端族 + 2 条件族** → **11 主 + 3 条件单元**。

208 = 28 + 5 + 167 + 6 + 2。低于门槛的 manager/fork 可以合并到已过线族（例如 deadpool_imap），但不新增独立客户端，也不改变“208 自身过线行”的数字。815 个输入行的最终 pool 状态均可在 CSV 中核对。

### 1.2 R-H 漏斗

同一个 815 → 同一个 **208** 自身过线 → 已测/核心 **28**、包装合并 **5**、直接句柄 API/协议/服务排除 **167** → **7 主族 + 1 条件族** → **12 主 + 2 条件单元**。

208 = 28 + 5 + 167 + 7 + 1。排名 **1–69 已重新逐行判断**，不是只从 70 开始；唯一原先纯因无池排除而转入的是 66 async-imap。37 soketto、54 fastwebsockets 仍排除，但原因改为没有具体一问一答应用契约；48 rumqttc 仍因 MQTT 事件循环排除。81 tokio-modbus、91 async-smtp、166 async-pop 也按同一门槛与句柄规则转入。已测 mysql_async MY2 不重测充数。

### 1.3 P 漏斗

月榜 **15,000** → 本期逐行判断前 **1,500**（另外 **13,500 未审，不是排除**）→ 元数据职责初筛排除 **1,415** → 余 **85** 行进一步按公开设计/API 判断 → **65 排除、6 合并、11 主包、3 条件包** → **27 主 + 6 条件单元**。

1,500 = 1,415 + 65 + 6 + 11 + 3。明显工具/数据处理/框架/云 SDK 的初筛只依据公开摘要和职责，**不声称对这些包逐个深入读过所有 API**；潜在协议客户端的具体决定见 CSV 与 §5。下载排序截断不是随机抽样；边界第 1,500 名 `lance-namespace` 为 **6,554,131 次/月**，这是观察到的边界值，不是倒推设定的数值门槛。

## 2. Rust：统一门槛和原名单的变化

**规则：客户端或其专用 manager 在 universe.csv 的下载量 ≥1,000/90d；不相加，不用通用池背书。** 两分支和条件候选一样；不另加配对扫描豁免。版本下载未核实就写未核实，不把 crate 总下载当版本下载。

### 2.1 从旧主/条件名单移除

| 旧目标 | 90 天下载（universe.csv） | 新处理 |
|---|---:|---|
| nntp-rs | 61 | 低于门槛，移除旧主单元 2 个 |
| vmemcached | 516 | 低于门槛，移除旧主单元 1 个；底层客户端已覆盖也不再豁免 |
| darkredis | 633 | 低于门槛，移除旧主单元 2 个 |
| redust | 85 | 低于门槛，移除旧主单元 2 个 |
| redis-oxide | 33 | 先被门槛筛掉，移除旧主单元 2 个；不再为它检查 PoolStrategy |
| memcached | 156 | 低于门槛，移除旧条件单元 1 个 |
| riak-async | 65 | 低于门槛，移除旧条件单元 1 个 |

旧 reserve 的 nativeclick（49）、bb8-nebula（98）/mobc-nebula（10）也不过线，不能补位；它们不是下面的 **rust-nebula**（独立 crate，排名 188、1,685）。bb8-gaussdb 在 universe 为 0.1.0，但其下载不足门槛；未有可核实过线客户端数据，不能凭总下载或猜测纳入。hdbconnect_async（23,217）过线但 HANA 本地许可/环境未具备；clickhouse-rs（17,597）过线但旧 runtime 不适配，不新建兼容层。均不设为自动补位 reserve。

**保留下来的原主族：** tiberius、IMAP、bolt-client、gel-tokio、skytable，另有不在用户点名例子中的 **orientdb-client（1,074）**，照规则保留。http-client（692,413）先过线，再作条件检查。点名的五个例子全部过线；这不是调整门槛的结果。

**新增独立 Rust 主族：** tokio-modbus、async-smtp（从 reserve 转主）、async-pop；新增条件族 rust-nebula。原主 20 单元减去 9 个低用量单元为池化 **11**；直接分支另有 **12**，不能与 11 合称 23 个独立 crate。

## 3. R-P：池化主名单（6 族 / 11 单元）

表内是操作类别与公开 API，不是已编写的场景；所有服务端只做文档可行性判断，**均未在本轮拉取或启动**。

| ID | 客户端 / 版本 | 排名 / 90 天下载 | 池与版本 | 单元（数量） | 本地服务端方案 |
|---|---|---|---|---|---|
| RP-IMAP | async-imap 族：deadpool_imap 0.1.3 + imap_session 0.1.3 + async-imap 0.11.2 | 66 / **957,884**；manager 行 282 / 398 | 已发布 deadpool manager，大小 1 | `select`；普通 `search/fetch`（2） | GreenMail IMAP/IMAPS Docker |
| RP-TIB | tiberius **0.12.3** | 69 / **818,928** | bb8-tiberius 0.16.0（或 deadpool-tiberius 0.1.9），大小 1 | 会话状态 `BEGIN TRAN`；普通 query（2） | SQL Server 2022 Linux Docker，需用户接受 EULA |
| RP-GEL | gel-tokio 0.11.0 | 155 / **4,550** | Client 自带池，max_concurrency=1 | 首次事务往返；普通 query（2） | geldata/gel Docker；服务端 tag 兼容性待 setup |
| RP-BOLT | bolt-client 0.11.0 | 173 / **2,367** | bb8-bolt 0.8.0（或 mobc-bolt 0.2.0），大小 1 | `begin`；完整 `run+pull`（2） | Neo4j community Docker |
| RP-SKY | skytable 0.8.12 | 191 / **1,616** | 官方 `pool::get_async(1, ...)` | 普通 `query`（1） | skytable/skytable:v0.8.4 |
| RP-ORI | orientdb-client 0.6.0，tokio-runtime | 207 / **1,074** | aio::SessionPool（mobc），大小 1 | session checkout；普通 query 完整消费（2） | orientdb:3.0；公开测试版本 3.0.23，具体 tag 待核实 |

### 3.1 公开依据与边界

- [async-imap Session](https://docs.rs/async-imap/0.12.0/async_imap/struct.Session.html)、[deadpool_imap](https://docs.rs/deadpool_imap/0.1.3/deadpool_imap/)、[imap_session](https://docs.rs/imap_session/0.1.3/imap_session/)：一次只发一条命令，tag 不是并发 router。本族门槛来自客户端，不来自只有 398 下载的 manager。池化使用旧 0.11 线、直接路径用 0.12.0，跨版本仍同一 cluster。imap_session 自身 90 天量**未核实，本轮不用旧草稿约数**。
- [tiberius](https://docs.rs/tiberius/0.13.0/tiberius/)、[bb8-tiberius 依赖](https://crates.io/api/v1/crates/bb8-tiberius/0.16.0/dependencies)：已发布 manager 绑定 0.12 线，池化保留 0.12.3；直连用 0.13.0。818,928 是 crate 所有版本下载，**不是 0.13.0 或 0.12.3 版本下载**；后者未核实，不另造数字。不自写新 manager。
- [gel-tokio](https://docs.rs/gel-tokio/0.11.0/gel_tokio/)：公开 Client 透明池；公开 Transaction 是事务作用域，不作为额外长寿命物理连接模式。edgedb-tokio 改名 stub 不算第二客户端。
- [bolt-client Client](https://docs.rs/bolt-client/0.11.0/bolt_client/struct.Client.html)：顺序借用 `&mut self`；RUN 返回不代表完整结果结束，普通操作须含 PULL/完整消费，不交错 qid。多个 manager 合并。
- [skytable](https://docs.rs/skytable/0.8.12/skytable/)：官方 ConnectionAsync 与池；不使用只支持旧 0.6 线的 bb8-skytable。多请求 Pipeline 不作为独立目标。
- [orientdb-client README](https://github.com/wolf4ood/orientdb-rs#readme)：公开 tokio-runtime、异步 session/query 与 Docker compose。OSession 是逻辑会话，不能仅因名字叫 Session 就断言固定物理流；本期只纳入明确池路径，不纳入直接句柄。未来若公开 API 不能确认单连接独占，记可行性失败，不读取消实现强行救回。

## 4. R-H：直接句柄主名单（7 族 / 12 单元）

| ID | 当前版本 | 排名 / 90 天下载 | 用户长期持有的对象 | 单元（数量） | 服务端 |
|---|---|---|---|---|---|
| RH-IMAP | async-imap 0.12.0 | 66 / **957,884** | `Session<Stream>` | `select`；普通 `search/fetch`（2） | GreenMail Docker |
| RH-TIB | tiberius 0.13.0 | 69 / **818,928** | `Client<Stream>` | `BEGIN TRAN`；普通 query（2） | SQL Server 2022 Docker |
| RH-MOD | tokio-modbus 0.17.0 | 81 / **395,362** | TCP `client::Context` | 一个普通 register 请求（1）；`set_slave` 是同步本地状态，不伪造第二挂起操作 | 本地 Docker Modbus TCP server（pymodbus server 镜像/配置待 setup 核实；Rust 可用公开协议模型） |
| RH-SMTP | async-smtp 0.10.2 | 91 / **142,588** | `SmtpTransport<S>` | `auth`；完整 `send`（2）；禁可选 pipelining | GreenMail SMTP Docker，启用本地认证 |
| RH-POP | async-pop 1.1.3 | 166 / **3,189** | `Client<S>` | `login`；完整 `retr`（2） | GreenMail POP3 Docker |
| RH-BOLT | bolt-client 0.11.0 | 173 / **2,367** | `Client<S>` | `begin`；完整 `run+pull`（2） | Neo4j community Docker |
| RH-SKY | skytable 0.8.12 | 191 / **1,616** | `ConnectionAsync` | 普通 query（1） | Skytable Docker |

新增句柄/API 依据：[tokio-modbus Context](https://docs.rs/tokio-modbus/latest/tokio_modbus/client/struct.Context.html)、[async-smtp SmtpTransport](https://docs.rs/async-smtp/0.10.2/async_smtp/struct.SmtpTransport.html)、[SmtpClient::pipelining](https://docs.rs/async-smtp/0.10.2/async_smtp/struct.SmtpClient.html)、[async-pop Client](https://docs.rs/async-pop/1.1.3/async_pop/struct.Client.html)。三个新族都是公开可借用的持久连接 API，不再要求补一个 pool manager。

只有目标 future 被放弃，句柄必须活着；幸存者不能自行换连接/回池。若库合法把同一对象标为关闭，记录调用在这个对象上的结果，不绕开关闭重建。“同持有者连续使用”不是“池租约交接”，两层分开统计。同一 crate 双模式产生的相同根因只计一次确认问题。

### 4.1 Rust 条件候选（不计入主数，不自动补位）

| 目标 | 过线依据 | 条件单元 | 条件与来源 |
|---|---|---:|---|
| http-client 6.5.3 | 排名 73，**692,413/90d** | R-P 1 | [H1Client](https://docs.rs/http-client/6.5.3/http_client/h1/struct.H1Client.html) 的 HTTP/1.1 专用池；只在既有 Tokio1 Dropwise 下可直接驱动 async-std I/O、无需新 runtime 工作流时纳入 |
| rust-nebula 0.0.2 | 排名 188，**1,685/90d** | R-P 2 + R-H 2 | [README](https://github.com/nebula-contrib/rust-nebula#readme)、[公开类型](https://docs.rs/rust-nebula/0.0.2/rust_nebula/)有 SingleConnSession/Manager；服务端为本地 Nebula V3 Graph/Meta/Storage，尚未验证 public API 的单 in-flight/manager 配置、客户端兼容性与 4 h 环境预算；拟 session 状态往返 + 普通 execute 各一种。任何前提不满足就不纳入，不靠结果决定 |

这些检查**本轮不做**。不是从旧 bb8-nebula/mobc-nebula 借用高下载背书；它们未过线，rust-nebula 自身过线。不是宣称新发现或本机可运行。

## 5. Python 数据源与名单

### 5.1 数据与获取方式

- 来源：[hugovk/top-pypi-packages](https://github.com/hugovk/top-pypi-packages)。2026-10-19 以 HTTPS GET 获取原 [JSON URL](https://raw.githubusercontent.com/hugovk/top-pypi-packages/main/top-pypi-packages-30-days.min.json)，不运行上游脚本。同日核对规范月榜 URL `top-pypi-packages.min.json` 的全部行与该响应相同。原响应保存在 **`python-top-pypi-2026-09.json`**，含 `last_update=2026-10-01 12:40:51`、`source=ClickHouse`、`total_rows=15000`。
- [上游采集程序](https://github.com/hugovk/top-pypi-packages/blob/main/clickhouse.py) 当前查询 `pypi.pypi_downloads_per_month` 上个自然月，`SUM(count)`、下载降序、名称破同分。因此本快照的指标是 **2026-09-01–09-30 月下载数**，不是截至获取日的近 90 天或滚动 30 天；URL 的历史命名不能替代实际口径。
- 固定审前 1,500 名（源排序）；逐个 GET `https://pypi.org/pypi/<project>/json`，查询日 2026-10-19，记录公开摘要、版本和来源到 `python-selection.csv`。没有安装、导入或执行这些包。主/条件包的正式版本另核对 PyPI version JSON，不以 latest 文档中的 dev/beta 版本替代正式版。
- 第 1,501–15,000 未审；例如 asyncmy（排名 4,144，954,029/月）、aiomcache（7,958，209,141/月）**在范围外**，不是 API 不合格。数字来自同一原始快照，不向后补用户例子。

### 5.2 主名单：11 包 / 27 单元

每格的 2 表示一个会话状态类操作、一个普通完整请求，后续才写具体场景；不是预先指定任何 bug/取消切点。同包两模式按 cluster 合并。Python 服务端全部用本机 Docker，HTTP 不访问真实站点。

| 排名 | PyPI 包 / 本次查询正式版 | 2026-09 下载 | pool 单元 | direct 单元 | 公开 API 与纳入理由 / 服务端 |
|---:|---|---:|---:|---:|---|
| 33 | httpx **0.28.1** | **616,760,679** | 1 | 0 | [AsyncClient](https://www.python-httpx.org/async/) 的 HTTP/1.1 keepalive 池，HTTP/2 禁用、连接限制1；Docker HTTP/1.1 记录端点 |
| 51 | aiohttp **3.14.5** | **455,690,685** | 1 | 0 | [ClientSession/TCPConnector](https://docs.aiohttp.org/en/stable/client_reference.html)，limit=1、HTTP/1.1；Docker HTTP 端点；**已接触的当前版扩展** |
| 140 | redis（redis-py）**8.1.0** | **202,023,598** | 2 | 2 | [redis.asyncio](https://redis.readthedocs.io/en/stable/examples/asyncio_examples.html) 的池与 single_connection_client 路径；事务类与普通命令，非 PubSub/并发 push；Redis Docker；**已接触的当前版扩展** |
| 221 | psycopg **3.3.6** | **130,867,953** | 2 | 2 | [AsyncConnection](https://www.psycopg.org/psycopg3/docs/advanced/async.html) 与 psycopg-pool **3.3.3** 的 [AsyncConnectionPool](https://www.psycopg.org/psycopg3/docs/api/pool.html)；顺序事务/SQL、不启 pipeline；PostgreSQL Docker |
| 257 | asyncpg **0.32.0** | **106,776,505** | 2 | 2 | [Connection/create_pool](https://magicstack.github.io/asyncpg/current/api/index.html)，一连接不并发请求，事务状态与普通 SQL；PostgreSQL Docker；不同于 SQLAlchemy 包 |
| 352 | pymongo **4.19.0** | **66,109,460** | 2 | 0 | [AsyncMongoClient](https://www.mongodb.com/docs/languages/python/pymongo-driver/current/reference/migration/) 原生 asyncio，maxPoolSize=1、单 socket 一个请求，session/事务内首次网络往返与普通请求；MongoDB 本地单节点 replica-set Docker；ClientSession 不是物理连接句柄 |
| 556 | curl-cffi **0.16.3** | **33,081,519** | 1 | 0 | [AsyncSession](https://curl-cffi.readthedocs.io/en/latest/advanced.html)，明确 HTTP/1.1、max_clients=1 与复用诊断；Docker HTTP 端点；native 回调覆盖局限单列 |
| 606 | mysql-connector-python **26.7.0** | **29,310,972** | 0 | 2 | [mysql.connector.aio](https://dev.mysql.com/doc/connector-python/en/connector-python-asyncio.html) 长寿命连接，事务状态与 SQL；MySQL Docker；不冒用同步 pooling API |
| 1,058 | neo4j **6.4.1** | **12,493,106** | 2 | 0 | [AsyncGraphDatabase/AsyncDriver](https://neo4j.com/docs/api/python-driver/current/async_api.html) 的 Bolt 连接池，顺序事务/完整消费普通结果、max_connection_pool_size=1；Neo4j community Docker；高层 Session 不算固定物理句柄 |
| 1,065 | aiosmtplib **5.1.3** | **12,384,093** | 0 | 2 | [SMTP 实例](https://aiosmtplib.readthedocs.io/en/stable/client.html) 的登录与 send_message，始终同一连接，不用每次重连的顶层 send；GreenMail Docker |
| 1,135 | aiomysql **0.3.2** | **10,851,565** | 2 | 2 | [Connection](https://aiomysql.readthedocs.io/en/stable/connection.html)/create_pool，事务状态与普通 SQL；MySQL/MariaDB Docker，不走 SQLAlchemy 包装 |

**包数与附属包：** 11 是客户端包数；psycopg-pool 单独是池附属依赖（排名 510、38,058,124/月），不是第 12 个客户端；psycopg-binary 同理。pool 9 个客户端包、15 单元；direct 6 个包、12 单元；四个包跨模式重叠。真正首次接触的主包 **9 个 / 22 单元**；redis/aiohttp 当前版扩展 **2 个 / 5 单元**，不能混称 11 个全新盲测包。

### 5.3 其他可能合格的条件包（3 包 / 6 单元）

| 排名 / 包正式版 | 月下载 | 条件单元 | 静态判断与待检查 |
|---|---:|---:|---|
| 262 / tornado 6.5.10 | **100,712,647** | pool 1 | [公开 API](https://www.tornadoweb.org/en/stable/httpclient.html)可选择 CurlAsyncHTTPClient 接 asyncio；默认 Simple 不作为 keepalive 目标。必须用当前正式 pycurl/libcurl、强制 HTTP/1.1，并确认同连接复用和目标实际挂起；不为它加 runtime 桥 |
| 825 / oracledb 26.0.1 | **18,388,545** | pool 2 + direct 2 | [正式版 Thin asyncio API](https://python-oracledb.readthedocs.io/en/stable/user_guide/asyncio.html)有 AsyncConnection/Pool，关闭 pipelining；[Oracle Free Docker](https://github.com/gvenzl/oci-oracle-free)有公开本地方案，**不能写成不存在 Docker**；本机资源、版本兼容和许可接受仍待检查，setup 超4h即未完成 |
| 1,244 / primp 2.0.1 | **8,976,568** | pool 1 | [PyPI README](https://pypi.org/project/primp/)列 AsyncClient；HTTP/1.1-only 控制、单流复用和 native 异步边界的公开证据本轮未核实充分；未来 preflight 确认，否则不纳入。不默认当作同步库，也不默认当作合格 HTTP/1.1 池 |

条件单元未进入主数和“首次接触”主数；失败只记录，不找更低名次包补足。不是宣称原生回调内部可枚举；若只能看到一次外层 await，要按实际覆盖报告。

### 5.4 关键排除/合并（全部 1,500 行的理由见 CSV）

| 包 | 决定与一句理由 |
|---|---|
| SQLAlchemy | **已经在 python-retro 中测试** |
| httpcore、httpx2、httpcore2 | httpx 底层/同源维护 fork，合并同一族，本期代表选 httpx，不累加下载 |
| motor 3.7.1（排名 1,308，8,233,781/月） | 合并至 pymongo 驱动族，选当前原生 asyncio API，不单独充数 |
| aiobotocore、aioboto3、各云 SDK、Elasticsearch/OpenSearch async | HTTP/gRPC 或已有 asyncio 客户端包装，不重复底层独立客户端 |
| clickhouse-connect | 当前公开 AsyncClient 是原生 aiohttp I/O，不是旧 executor 包装；按底层 HTTP 包装合并规则不另列独立目标 |
| clickhouse-driver、psycopg2、pymysql、mysqlclient、pymssql、pg8000 | 同步客户端，不因名字类似 async 包而纳入 |
| aio-pika、aiormq、pika、amqp、asyncssh、aiokafka、grpclib | channel、request id 或 stream 路由/后台多路复用，不满足本期顺序路径 |
| aiosqlite、duckdb | 嵌入式，不是网络字节流 |
| websockets、python-socketio、autobahn、paho-mqtt | transport/事件或推送协议，不是指定顺序一问一答 API |
| dnspython、aiodns | 不提供本期要求的可重复持有顺序 TCP 请求句柄；解析/查询回调不等于顺序流 |
| asyncio、anyio、trio、uvloop、Twisted | 通用 runtime/框架，不作为客户端目标；不额外搭 runtime 桥 |

只依据 API/职责筛选，不根据“看起来取消实现有问题”挑选。已有上游问题并非筛选条件；当前版已知/未知的分诊放未来 Q5。

## 6. 仍未核实与需用户拍板的事

所有条目仍需要未来 reuse/挂起/观察者预检，不能把静态名单写成可运行名单。SQL Server、Gel、OrientDB、GreenMail 的版本/认证和镜像可用性尚未本机验证；Rust 两个旧 manager 线的实际依赖须未来记录，crate 级过线不证明旧版本有人用。条件项的失败保留，不补位。

最多三个待确认决定：

1. **Python frame 边界：** 本草稿审月榜前 1,500，并保留 13,500 未审；是否接受这个资源边界，而不是把名单叫作全生态普查？
2. **当前版扩展口径：** redis/aiohttp 按用户要求纳入当前正式版，但单列 2 包/5 单元的既往接触层；是否接受该口径？不改变 SQLAlchemy 排除。
3. **执行预算：** 主名单预计 65.5–88.5 AI 小时；全体含条件仍受 120 AI 小时、用户预计3h/上限6h约束。是否接受；包括将来接受 SQL Server/Oracle Free 许可与启动重服务前须用户确认？本轮不接受许可、不启动。

这些决定不影响本轮完成静态修订，也不构成运行/冻结授权。
