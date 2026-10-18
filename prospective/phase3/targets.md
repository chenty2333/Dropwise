# Phase 3 目标清单（与 PLAN.md 一起冻结）

按 `universe.csv` 的近 90 天下载量从高到低，逐个按 PLAN.md 第 2 节的规则判断，直到凑满 8 个。判断只依据 README、文档和公开 API 的已知设计（协议是否多路复用、有没有连接池），**没有读取消相关的实现代码**。

## 选中的 8 个目标

| # | 排名 | 目标（合并的 crate） | 复用单元 | 服务端 |
|---|---:|---|---|---|
| 1 | 1 | hyper-util 旧版 `Client` 的连接池（合并 hyper、clickhouse） | HTTP/1.1 keep-alive 连接 | 进程内 HTTP/1.1 服务器 |
| 2 | 2 | reqwest（单独计算，2026-10-19 确认） | 同上，经 reqwest 的 API | 进程内 HTTP/1.1 服务器 |
| 3 | 35 | mongodb（自带连接池） | 单条连接，一次一个操作 | `mongo` 镜像（需要拉取） |
| 4 | 51 | pingora-core 的 HTTP/1 客户端连接 + pingora-pool（合并 pingora） | HTTP/1.1 上游连接 | 进程内 HTTP/1.1 服务器 |
| 5 | 56 | mysql_async（自带连接池） | 单条 MySQL 连接 | `mariadb:11.4`（本地已有） |
| 6 | 62 | isahc（libcurl 的连接复用） | HTTP/1.1 连接 | 进程内 HTTP/1.1 服务器 |
| 7 | 65 | awc（actix 的 HTTP 客户端连接池） | HTTP/1.1 连接 | 进程内 HTTP/1.1 服务器 |
| 8 | 67 | clickhouse-arrow（bb8 manager，原生 TCP 协议） | 单条原生协议连接 | `clickhouse` 镜像（需要拉取） |

两点说明：

- mysql_async 是 diesel-async 的底层驱动。这里测的是 mysql_async 自己的池和直接持有的 `Conn`，与 diesel-async 那一层的问题无关。
- reqwest 单独计算后，第 9 名 tiberius（及 bb8-tiberius、deadpool-tiberius）不再入选。

## 逐个判断（排名 1–69）

| 排名 | crate | 结论 | 理由 |
|---:|---|---|---|
| 1 | hyper | 并入目标 1 | 连接池在 hyper-util 的旧版 Client |
| 2 | reqwest | **目标 2** | 基于 hyper-util 的连接池，但用户面对的是 reqwest 的 API；单独计算（2026-10-19 确认） |
| 3 | h2 | 排除 | HTTP/2 流多路复用 |
| 4 | hyper-util | **目标 1** | HTTP/1.1 连接池 |
| 5 | quinn | 排除 | QUIC 流多路复用 |
| 6 | tonic | 排除 | gRPC over HTTP/2，多路复用 |
| 7 | tokio-tungstenite | 排除 | 已覆盖 |
| 8 | aws-smithy-http | 排除 | HTTP 包装，复用单元由 hyper-util 决定 |
| 9 | sqlx-core | 排除 | 池的核心；两个网络驱动已覆盖 |
| 10 | sqlx | 排除 | 同上 |
| 11 | sqlx-postgres | 排除 | 已覆盖 |
| 12 | sqlx-sqlite | 排除 | 嵌入式数据库，没有字节流 |
| 13 | sqlx-mysql | 排除 | 已覆盖 |
| 14 | aws-smithy-runtime | 排除 | HTTP 包装 |
| 15 | deadpool | 排除 | 通用池，不是客户端（核心已在 RESCAN 第 5 节测过） |
| 16 | deadpool-runtime | 排除 | 池的工具 crate |
| 17 | redis | 排除 | 已覆盖 |
| 18 | wiremock | 排除 | 测试用的模拟服务器 |
| 19 | aws-sdk-s3 | 排除 | HTTP 包装 |
| 20 | object_store | 排除 | HTTP 包装 |
| 21 | tokio-postgres | 排除 | 已覆盖 |
| 22 | azure_core | 排除 | HTTP 包装 |
| 23 | diesel | 排除 | 同步 ORM |
| 24 | bb8 | 排除 | 已覆盖 |
| 25 | rdkafka | 排除 | librdkafka 封装，broker 连接由 C 库管理 |
| 26 | google-cloud-storage | 排除 | HTTP 包装 |
| 27 | lettre | 排除 | 已覆盖 |
| 28 | deadpool-postgres | 排除 | 已覆盖 |
| 29 | aws-sdk-sqs | 排除 | HTTP 包装 |
| 30 | aws-sdk-dynamodb | 排除 | HTTP 包装 |
| 31 | async-nats | 排除 | 已覆盖 |
| 32 | opendal | 排除 | HTTP 包装 |
| 33 | clickhouse | 并入目标 1 | HTTP 接口，复用单元由 hyper-util 的连接池决定 |
| 34 | diesel-async | 排除 | 已单独测试，单列一层 |
| 35 | mongodb | **目标 3** | 自带连接池；一条连接一次一个操作 |
| 36 | sea-orm | 排除 | 基于 sqlx |
| 37 | soketto | 排除 | WebSocket，没有池 |
| 38 | postgres-native-tls | 排除 | TLS 适配层 |
| 39 | jsonrpsee-core | 排除 | 组件 crate |
| 40 | postgres | 排除 | 同步客户端 |
| 41 | jsonrpsee | 排除 | HTTP 客户端基于 hyper，WebSocket 客户端按请求编号多路复用 |
| 42 | deadpool-redis | 排除 | redis 已覆盖 |
| 43 | etcd-client | 排除 | gRPC |
| 44 | jsonrpsee-client-transport | 排除 | 同 41 |
| 45 | jsonrpsee-ws-client | 排除 | 按请求编号多路复用 |
| 46 | jsonrpsee-http-client | 排除 | 基于 hyper |
| 47 | fred | 排除 | 后台任务在一条连接上流水线复用多个调用方 |
| 48 | rumqttc | 排除 | MQTT 事件循环，没有池 |
| 49 | lapin | 排除 | AMQP channel 多路复用 |
| 50 | google-cloud-pubsub | 排除 | gRPC |
| 51 | pingora-pool | **目标 4** | 与 pingora-core 的 HTTP/1 上游连接一起测 |
| 52 | scylla | 排除 | CQL stream id 多路复用 |
| 53 | gcp-bigquery-client | 排除 | HTTP 包装 |
| 54 | fastwebsockets | 排除 | WebSocket，没有池 |
| 55 | h3 | 排除 | HTTP/3 多路复用 |
| 56 | mysql_async | **目标 5** | 自带连接池；单条 MySQL 连接 |
| 57 | tarpc | 排除 | 按请求编号多路复用 |
| 58 | ldap3 | 排除 | 按 message id 多路复用 |
| 59 | bb8-redis | 排除 | redis 已覆盖 |
| 60 | pingora-core | 并入目标 4 | |
| 61 | bb8-postgres | 排除 | tokio-postgres 已覆盖 |
| 62 | isahc | **目标 6** | libcurl 复用 HTTP/1.1 连接 |
| 63 | pingora | 并入目标 4 | |
| 64 | aws-sdk-sns | 排除 | HTTP 包装 |
| 65 | awc | **目标 7** | actix 客户端的 HTTP/1.1 连接池 |
| 66 | async-imap | 排除 | 没有连接池 |
| 67 | clickhouse-arrow | **目标 8** | bb8 manager；原生协议一条连接一次一个查询 |
| 68 | libsql | 排除 | 本地是嵌入式；远程走基于 hyper 的 HTTP |
| 69 | tiberius | 未入选 | 单条 TDS 连接、可池化，符合条件，但排在第 9 |

## 冻结前需要确认

- 已确认（2026-10-19）：reqwest 单独算一个目标。
- 两个需要拉取的镜像（mongo、clickhouse）拉取时需要联网。
