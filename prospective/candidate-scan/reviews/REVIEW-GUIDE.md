# 人工复核指南（候选扫描）

目的：判断每条静态命中是否属于“出错路径 / 取消路径不一致”，并且是否落在**会被复用的连接或会话**上。扫描器只负责高召回，本步骤负责精度。

## 输入

- 工作清单：`prospective/candidate-scan/reviews/worklist-<组>.csv`
- 源码：`~/dropwise-scan-src/src/<crate>-<version>/<file>`（`file` 列给的是相对 crate 根目录的路径）
- 只读。不要 build、不要运行、不要联网、不要修改 `prospective/candidate-scan/reviews/` 以外的文件。

## 每条命中必须回答的四个问题

1. **这个 `.await` 是不是一次真正的网络/会话往返？**
   - 是：写请求后等回复、握手、BEGIN/COMMIT、订阅、认证、`is_valid`/`recycle` 的探测。
   - 不是：`Mutex::lock().await`、channel send/recv、task join、`sleep`、文件 I/O、`spawn_blocking`。
   - 不是网络往返的，直接判误报。

2. **被标记的“坏”状态是否属于一个会活过 future、并且会被复用的对象？**
   - 是：池中的 `Connection`、`PooledConnection`、multiplexed client、共享 session、远端事务。
   - 不是：局部变量（`processed_count`、`found`、`retry`、`exhausted`）、任务句柄（`JoinHandle::abort`）、文件/通道（`File::close`、`Sender::close`）、测试代码、纯 UI 状态、集群节点健康标记。
   - 找不到复用路径的，判误报或不确定。

3. **取消路径有没有等价保护？**
   - 悲观标记：await 之前置坏，完成后恢复（如 `conn.state = Invalid` → `Present`，`sending = true` → `false`）。
   - 带 `Drop` 的 guard（`Rollback`、`scopeguard`、`defer!`），且 Drop 里丢弃/回滚而不是放回池。
   - `Option::take()` 把连接移进 future，完成后再放回。
   - 池的 `has_broken` 检查一个在 await 之前就置位的“进行中”标志。
   - 有保护 → `review=已防护`（如果扫描器已标 guarded，也写已防护）。

4. **如果 future 恰好在这个 await 被 drop，下一个使用者会看到什么？**
   - 要具体：未读的回复留在 socket 上、服务端仍在事务里、会话标志仍是旧值、半截帧、连接被放回池。
   - 说不清后继者状态的，判不确定。

## 结论

- `可疑`：四个问题都成立，且能描述后继者看到的具体污染。note 里写：调用点、取消后残留、后继者影响、建议的动态场景（可选）。
- `误报`：至少一个问题不成立。note 里写清是哪一条（例如“局部计数器，不是连接状态”“multiplexed 连接由驱动内部处理”“这是测试代码”“await 是 lock 不是往返”）。
- `不确定`：证据不足（例如复用关系在别的 crate、看不到 Drop 实现、版本 feature 开关不明）。不要猜。

## 输出

- 复制 worklist 为 `reviewed-<组>.csv`，保留全部行与原列，只填写 `review` 和 `note` 两列。
- `review` 只能是 `可疑` / `误报` / `不确定` / `已防护`。
- `note` 用中文，不超过 300 字，引用实际代码（可写 `file:line` 和关键表达式）。
- 同一 crate/文件里重复的模式可以合并说明，但每一行都要有结论。
- 最后回复：处理行数、可疑/误报/不确定各多少、最可疑的 3 条（id + 一句话理由）。

## 参考：已确认的实例（校准）

- lettre 0.11.23：`try_smtp!` 的 `Err` 分支调用 `abort()` 置 `panic`；取消时不会置位，`has_broken()` 只看 `panic`。
- bb8-memcached 0.7.0：`taint()` 只在自身 timeout 的 `Err(_elapsed_err)` 分支调用；`has_broken` 只看 `tainted`。
- sqlx-mysql 0.9.0 `begin`：`conn.execute(statement).await?` 之后才 `transaction_depth += 1`，无 guard。
- bb8 0.9.1 `Pool::get()`：`is_valid().await` 期间连接仍是 `Present`，取消后 `PooledConnection::drop` 把它放回池。

这些实例的详细后果见 `prospective/RESCAN.md` 第 4、6 节。
