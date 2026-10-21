# Phase 4 第一轮与第二轮真实客户端探索：结果

状态：**unfrozen exploration，2026-10-21**。第二轮是在读过第一轮结果之后追加的；它仍只覆盖原有的三个客户端家族，不扩展 Phase 4 主表、门槛或预算，也不是冻结的 Phase 4 研究或 preregistered 研究。第一轮日志保持原样，第二轮证据全部使用新的 round2-* 名字。

配套文件：[README.md](README.md)（选择、条件、运行）、各客户端的 SCENARIO.md、RESULTS-FRAGMENT.md、代码和 logs/。原始日志是事实来源，本文件只做汇总、修正和分诊。

## 0. 结论摘要

- **SMTP（async-smtp 0.10.2）：** 取消一次 send 后复用同一 transport 是确认的用户可见错误。具体返回码观察下，8 个计划/rep 全部违反（3 rep 一致）：18/24 个计划执行返回 Ok(354)（DATA 的中间响应），但消息实际已投递；第三封仍返回 354，不会自愈；6/24 个计划执行（#0@3）第二次、第三次 send 都 2 s 超时，命令/回复 trace 显示服务器停在 DATA/body 模式，没有消息投递。没有“声称成功但实际没投递”的 354；354 是“投递成功、返回值错误”，#0@3 是“无效果、无返回”。这是客户端在取消后的响应关联/最终响应校验缺陷；公开文档没有取消安全承诺，未找到对应上游 issue，因此是**已确认的客户端缺陷，待上游分诊**，不是已获上游确认的 bug。
- **IMAP（async-imap 0.12.0）：** 取消 SELECT 后不重试就 APPEND 会在 3 rep、10/10 个已实现计划中失败；APPEND 只读一条响应，把 SELECT 的 untagged 响应当作 continuation，返回 Err(Error::Append)，消息未写入，随后 SEARCH 超时。先重试 SELECT 再 APPEND/SEARCH/FETCH 可以恢复真实操作（published 10/10、patched 12/12 个已实现计划，3 rep），但 Mailbox 元数据不干净：@1 的 flags 是 10 个（正常 5 个），permanent_flags 是 12 个（正常 6 个）；@2 的 flags 回到 5 个，permanent_flags 仍是 12 个。服务器 trace 显示每次 SELECT 只发 5/6 个，所以这是客户端合并陈旧 untagged 数据，不是服务器重复。结论：**重试 SELECT 恢复了可用的操作序列，但没有恢复精确的 API 元数据**；不是“Retry Ok 就干净”。
- **BOLT（bolt-client 0.11.0 + bb8-bolt 0.8.0 + bb8 0.8.6）：** pooled 的 checkout 取消会丢掉 bb8-bolt 的 is_valid -> RESET future，旧连接被放回 idle；下一次借用时 RESET 读到陈旧的 Success 并把它当作验证通过，业务 RUN 随后报 “server gave unexpected response while in Ready state”。原版 23/60 计划执行违反，全部在 #0@1；使用已有的 bb8 “validation 期间标 Invalid” 修复后 **0/60**。旧连接在原版里直到业务失败后才被丢弃；修复版在取消后立即丢弃并新建连接（connections 1→0，created 1→2），不是把取消当作没发生。本轮的对照直接支持此前已报告的 bb8 验证取消缺陷（#324/#325）在 BOLT 适配器上的触发。bolt-rs#8 的历史讨论说明相关响应队列风险，但不能单凭该讨论把当前版本全部行为归为同一个已知缺陷。
- **BOLT direct：** 取消 RUN/PULL 后直接复用会在 Streaming/Defunct 状态失败（第一轮 4/4）；切点后显式 client.reset()（立即或 100 ms 后）在第一轮 0/4 恢复。**但不能据此说“调用方全责”**：公开 Rust 文档没有取消后必须 reset/reconnect 的明确要求，drop future 也没有自动清理；上游维护者在 #8 中明确说“RESET 只是读下一条消息”，并承认状态跟踪/清理仍是待办。存在 reset API 只证明“调用方有工具”，不证明“库已经把责任写清”。因此这更像**协议/所有权边界上的未文档化契约缺口**，不是确认的新库代码缺陷。

最值得带回产品化的三点：

1. **B2 的 Ok 不是充分判据。** IMAP 的 Ok 可能带着重复元数据，SMTP 的 Ok 可以是 354 中间响应；必须看具体返回值/协议状态，最好再有独立 observer。
2. **pool checkout 本身是取消边界。** 它不只是“取连接”，还包含 is_valid/RESET；只在 RUN/PULL 上做切点会漏掉它。
3. **pre/post 相等不能证明“没有效果”。** atomic 比较只能检查所选观察中的污染，不能证明全部共享状态未受影响或目标操作未发生；SMTP 的 pre/post 本来就不相等。

## 1. 范围、版本与计数方式

| # | 家族 / 版本 | 复用方式 | 操作 | 服务 |
|---|---|---|---|---|
| 1 | bolt-client 0.11.0 + bb8-bolt 0.8.0（bb8 0.8.6） | pooled Pool<Manager>::get()，以及 direct Client | RUN + PULL 一条写查询；第二轮隔离 checkout | Neo4j 5.26.0 community |
| 2 | async-imap 0.12.0 | direct Session | SELECT INBOX；survivor 对同一 mailbox APPEND/SEARCH/FETCH | GreenMail 2.1.14 |
| 3 | async-smtp 0.10.2 | direct SmtpTransport | 一次完整 send；survivor 再发一封/第三封 | GreenMail 2.1.14 |

共同条件：Flavor::MultiThread { workers: 1 }，races = [Immediate, AfterWake]，max_cancellations = 1，max_runs = 64，scenario_timeout = 20 s，零 settle（显式构造 Settle { tokio_time: ZERO, real_time: ZERO, watchdog: 10 s }）。dropwise::net::Proxy 对每个 server->client chunk 延迟 50 ms；proxy 的字节数不是协议 trace，协议 trace 来自本轮 tee/客户端 debug 日志。服务只绑定 127.0.0.1 的临时端口，runner 用 disposable 容器。

版本/镜像 digest/端口在各客户端 logs/round2-environment.txt 或 logs/*container* 中。

### 计数口径

- **plan**：一个 Pending × 一个 race 生成的执行计划；Report.trials.len()。#0@1 是 plan/Pending 编号，不是自动的协议 stage 标签。
- **violation**：某个 plan 的实际 observation 不满足该层 invariant。一个 plan 的两种 race 各算一次执行；不同 rep 也算不同执行。
- **user-visible phenomenon**：用户能看到的结果类别（例如 false 354、2 s 超时、APPEND 拒绝、连接池错误），不是 plan 数。
- **independent root cause**：本轮每个客户端只有一条主根因（SMTP 的响应关联/最终响应校验；IMAP 的 APPEND 单响应读取；BOLT 的 checkout 取消 + RESET 失步）；23/24 之类的数字不是 23/24 个独立 bug。
- **realized/unrealized**：unrealized 表示该计划没有真正实现取消/观察；计算比例时用 realized plan 作分母。plans=4 violations=3 unrealized=1 是 3/3 realized 违反，不是 3/4。

## 2. SMTP：具体返回码、第三封、trace 与修复对照

### 2.1 第一轮保留

第一轮官方 ladder（每格是 violations/plans，3 rep）：B1 0/8、0/8、0/8；B2 2/8、2/8、2/8；B3 2/8、2/8、2/8；M1 2/8、2/8、2/8。B1 无 panic/timeout/leak；B2/B3 的 2/8 违反都是 #0@3 的 20 s liveness timeout；survivor 仍可能在只看 Ok 的 B2/B3 中以 Ok(354) 通过（所以 B2/B3 的 Ok 不能证明最终 250）。M1 的 2 个违反也是超时，observer 无消息。第一轮 M1 的 Observation 只记录 count/subject/body，不记录 SmtpResult，所以**第一轮 M1 本身不能区分 354 和 250**；这是观察通道的缺口，不是 Dropwise 不能比较返回值。

### 2.2 第二轮 M1R2（加入具体返回码）

M1R2 仍用同一 explore_observed(Expect::Atomic)、同一 Pending/Config，只在 observation 中增加：

~~~text
survivor: Code(u16) | Timeout | Error(String)
third:    Code(u16) | Timeout | Error(String)
messages: REST count + subject + message-id + body
~~~

每次 send 外包一个 2 s 的 tokio::time::timeout，因此挂起会变成可记录的 Timeout，而不是把整个 20 s scenario 杀掉。**没有加入 target tag、通用 return-value channel 或框架 API。**

先做 pre/post calibration（3/3 rep，修复版同样）：无取消的 pre 是 survivor + third 各 250、REST count=2；无取消的 post 是 target + survivor + third 各 250、REST count=3；distinguishable=true。两者不相等且各自稳定，说明 observer 能区分有无 target 效果。

3 个 rep、每 rep 8 个计划，结果完全一致：

| 计划（两种 race 相同） | survivor | third | REST 消息 | 分类 |
|---|---|---|---|---|
| #0@1 | Code(354) | Code(354) | survivor + third（2） | 有投递，返回值错误 |
| #0@2 | Code(354) | Code(354) | survivor + third（2） | 有投递，返回值错误 |
| #0@3 | Timeout | Timeout | 无 | 无投递、无返回 |
| #0@4 | Code(354) | Code(354) | target + survivor + third（3） | 有投递，返回值错误；最终观察包含 target，未确定其生效相对取消的时刻 |

每 rep 汇总：plans=8 violations=8 unrealized=0 baseline_errors=0，pre=0 post=0 mixed=0 neither=8 unknown=0。3 rep 共 24 个计划执行：**18 个 false-354 + 6 个 2 s timeout**；0 个“声称成功但实际缺失”。M1R2 runner 这 3 次返回 exit 1 是预期（有 violation），不是 harness failure；status 文件是 1/1/1。24 是执行数，不是 24 个根因。

### 2.3 第三封与“自愈”问题

第三封用独立的 subject/message-id/body（不是重复 survivor），REST 逐条核对。在 18 个第二次 send 返回 354 的计划执行里，第三次 send **18/18 也都返回 354**；第三封消息都被 REST 看到。没有观察到“第二次 354，第三次读到上一封的 250”或任何自愈。第一轮曾把“extra 250”作为待验证预测；本轮具体观测否定了它：后续 send 继续返回中间响应。具体是哪次事务的回复需要逐条 wire trace，不能从相同返回码推出同一条 354 从未被消费。

### 2.4 #0@3 的真实命令/回复 trace

用 RUST_LOG=async_smtp=debug 和 2 s bound 分别诊断 #0@3:Immediate 与 #0@3:AfterWake（各 1 个 trial；不是从 Pending 编号推断 stage）：

~~~text
target:  MAIL FROM -> 250
         RCPT TO   -> 250
         DATA      -> 354
cancel
survivor: MAIL FROM -> 354   (读到了 target 的 DATA 提示)
          RCPT TO   -> 无回复
third:    MAIL FROM -> 无回复
server:   仍在 DATA/body 模式，等待 target 的正文和 "."
~~~

两次诊断都 realized=true：Immediate 的 survivor/third 是 2000/2000 ms timeout，AfterWake 是 2000/2001 ms timeout，REST 都是 0 条；命令序列形状相同（下面是 Immediate）。因此 #0@3 的 2 s timeout 不是“服务器慢”，而是**字节流已经进入 DATA body 模式，后续命令被当作正文**。本轮验证有效的恢复方式是丢弃/重连；未验证原地恢复方案。这个 trace 只覆盖 #0@3 的两个 race；#0@1/#0@2/#0@4 的协议阶段仍是从返回码和 REST 效果推断，不能把 plan 编号当作已证明的 stage。

### 2.5 旧 transport vs 丢弃/重连

对每个取消计划，在旧 transport 上继续第二、第三封，然后**另开一条新连接**做 recovery send，并用 REST 观察：

| 计划 | 旧 transport 第二/第三 | 旧 transport 的实际消息 | 新连接 recovery |
|---|---|---|---|
| #0@1, #0@2 | 354 / 354 | survivor + third | 250，消息可见 |
| #0@3 | 2 s timeout / 2 s timeout | 无 | 250，消息可见 |
| #0@4 | 354 / 354 | target + survivor + third | 250，消息可见 |

3 rep、24 个取消计划均如此。**旧 transport 从未恢复；新连接每次都恢复。** 这不是“等久一点就好”，也不是“第三封会自愈”。

### 2.6 最小依赖副本修复

patches/async-smtp-poison.diff 在 registry 源的副本上做了三件事：

1. SmtpTransport 增加 poisoned: bool；
2. send 开始时置 true，只有完整 2xx 最终回复才清 false；
3. 非最终回复变成显式 Error::Client("send ended at a non-final SMTP response")；后续 send 在 poisoned 时返回 Error::Client("previous send did not complete; reconnect the transport")。

明确语义：**它不是原地恢复，而是拒绝复用并要求重连；正常 2xx 路径不变。** 4xx/5xx 也会保持 poisoned（第二轮未测；收尾测试已确认回归，见 §9）。

对比结果（3 rep，每 rep 8 个计划）：

- 原版：24 个取消计划中 18 个 354/354、6 个 timeout/timeout；recovery 新连接 250；消息数分别是 3/3/1/4（含 recovery）。
- 修复版：24 个取消计划的第二、第三封全部 Error("client: previous send did not complete; reconnect the transport")；没有 354、没有 2 s 挂起、没有 survivor 投递；recovery 新连接 250；消息数 @1–@3 只有 recovery（1），@4 是 target + recovery（2）。
- 修复版 3/3 rep 的正常 calibration 仍是 250/250/250。

限制：只测 GreenMail；只测 DATA 事务；未测 RSET/QUIT 在 body 模式的行为；patch 是依赖副本，不是上游 PR；未打开 issue/PR。

### 2.7 分类

- **确认的用户可见缺陷**：取消后的 send 把中间 354 当成功，并在 DATA 模式挂起。
- **不是已确认的上游 bug**：async-smtp 公开文档没有取消安全承诺；未找到匹配 issue；上游是否接受“poison + reconnect”语义未知。
- **不是服务器问题**：GreenMail 的 SMTP 行为正常；问题是客户端读取/关联。
- 原有 20 s liveness 超时是第一轮的 scenario 保护；第二轮用 2 s bound 把它变成可记录结果，没有把 20 s 当作缺陷证据。

## 3. IMAP：重试 SELECT、APPEND 修复与回归测试

### 3.1 恢复链：cancel SELECT → retry SELECT → APPEND → SEARCH/FETCH → REST

用同一 Session 按顺序执行，3 rep。published retry-first 的 plans 是 4/2/4（共 10 个已实现），patched 是 4/4/4（共 12 个已实现）；plan 数随调度变化，不能把“每 rep 4 个”当固定值。

| variant / order | 取消 SELECT | retry SELECT | APPEND | SEARCH/FETCH | REST observer |
|---|---|---|---|---|---|
| published retry-first | 10/10 realized | Ok 10/10 | Ok(())，10/10 | Ok(1) / Ok，10/10 | seed + survivor，10/10 |
| patched retry-first | 12/12 realized | Ok 12/12 | Ok(())，12/12 | Ok(1) / Ok，12/12 | seed + survivor，12/12 |

所以**先重试 SELECT 确实恢复了操作序列**；这与第一轮“B2 retry Ok”只看 Ok 的印象不同，但也不等于会话完全干净。

retry-first 的元数据（reverse 的 patched 结果是 5/6，见 3.2）：

- @1：retry flags_len=10（正常 5），permanent_flags_len=12（正常 6）；
- @2：retry flags_len=5，但 permanent_flags_len=12；
- 服务器 trace 每次 SELECT 只发 5 个 flags、6 个 permanent flags；因此重复是客户端 parse_mailbox 把两次 SELECT 的 untagged 数据合并，不是服务器重复。
- recent 也可能从 1 变 0。操作（APPEND/SEARCH/FETCH）在测试的这条路径上没有因此失败，但 API 返回的 Mailbox 不精确；这是一个**同一失步机制的元数据表现**，不另算一条独立根因。

### 3.2 反向顺序：APPEND 先于 retry

把顺序改成 cancel SELECT → APPEND → retry SELECT → SEARCH/FETCH：

- unpatched：3 rep、11/12 个已实现计划（rep1 是 4/3，其余 4/4），APPEND 全部 Err(Error::Append)，retry SELECT 全部 2 s Timeout，SEARCH 全部 Timeout，observer 只有 seed（survivor 0）。
- patched：3 rep、11/12 个已实现计划（rep2 是 4/3，其余 4/4），APPEND 全部 Ok(())，retry SELECT 全部 Ok，SEARCH Ok(1)，observer seed + survivor。

这证明顺序不是装饰：**不先重试 SELECT 就直接 APPEND 会失败；修复 APPEND 的 continuation 读取后，APPEND 可以先成功，随后 retry SELECT 也能完成**。但 **patched retry-first** 的 @1 仍会看到重复 flags/permanent_flags（3.1）；patched reverse 的 retry SELECT 是 5/6 干净的。这不是 patch 修了 parse_mailbox，而是反向顺序里 APPEND 先把 stale SELECT 排空了。

### 3.3 官方 ladder（3 rep）

表中是 plans / violations / unrealized；所有行 baseline_errors=0、exhaustive=true、baseline_targets=1。

| variant | level | rep 1 | rep 2 | rep 3 |
|---|---|---:|---:|---:|
| unpatched | B1 | 4 / 0 / 2 | 4 / 0 / 2 | 4 / 0 / 0 |
| unpatched | B2 | 2 / 0 / 0 | 4 / 0 / 0 | 4 / 0 / 1 |
| unpatched | B3 | 4 / **4** / 0 | 4 / **3** / 1 | 4 / **3** / 1 |
| unpatched | M1 | 4 / **4** / 0 | 4 / **4** / 0 | 4 / **4** / 0 |
| patched | B1 | 4 / 0 / 1 | 4 / 0 / 0 | 4 / 0 / 2 |
| patched | B2 | 4 / 0 / 0 | 4 / 0 / 2 | 4 / 0 / 2 |
| patched | B3 | 4 / 0 / 0 | 4 / 0 / 0 | 2 / 0 / 0 |
| patched | M1 | 4 / 0 / 2 | 4 / 0 / 2 | 4 / 0 / 0 |

- Unpatched B3：12 个计划，2 个 unrealized，**10/10 已实现计划全部 Err(Append)，observer 0**；M1：12/12 全部 Neither，pre=0 post=0 mixed=0 neither=4 unknown=0（每 rep）。
- Patched B3：10/10 已实现计划 Ok，observer 1；patched M1：12 个观测，8 个 realized + 4 个 baseline，12/12 都是 Post，0 violations，没有 Neither。
- B3 的 plan 数会因取消时序变成 2 或 4；不要把不同 rep 的 plan 数直接相加当固定分母。
- 未加 target tag、通用 return channel；这些具体值来自客户端的 observation/打印，不是 Dropwise 自动 stage 标签。

### 3.4 补丁本身

patched/append-continuation.round2.patch 把 Session::append 的一段：

~~~rust
let Some(res) = self.read_response().await? else { return Err(Error::Append); };
let Response::Continue { .. } = res.parsed() else { return Err(Error::Append); };
~~~

改成循环：

~~~rust
loop {
    let Some(res) = self.read_response().await? else { return Err(Error::Append); };
    match res.parsed() {
        Response::Continue { .. } => break,
        Response::Done { tag, .. } if tag == &id => return Err(Error::Append),
        _ => continue,
    }
}
~~~

它跳过其他 tag 的 tagged completion（与 PR #142 的 tag-first 思路一致），在本 APPEND 的 tagged Done 或 EOF 时返回错误；不改变正常 continuation 的写入路径。审查结论：对 stale SELECT 的 untagged/tagged 响应，这个循环已经足够，不需要再额外加 tag 过滤或 timeout；真正的剩余盲点是 stale +，以及 + 之后其他 tag 的 NO/BAD（见 3.5）。

**PR #142 对照：** [chatmail/async-imap#142](https://github.com/chatmail/async-imap/pull/142) 已合并，修的是 LOGIN 收到其他命令的 NO 时不要失败；它没有改 APPEND。本补丁把同类 tag-first 逻辑用于 APPEND，但它不是 PR #142 的复制，也不是上游已接受的通用取消契约。

### 3.5 回归测试、上游测试与失败/拒绝路径

补丁测试副本（published 0.12.0 vs patched 副本，runtime-tokio）：

| 检查 | 结果 |
|---|---|
| published 上游测试（--skip round2_） | **73 passed, 0 failed** |
| published + 13 个新 round2_ 测试 | **10 passed, 3 failed**（三个 stale SELECT/BAD/NO 用例；这是期望的 fail-before） |
| patched 全量测试 | **86 passed, 0 failed** |
| cargo fmt --all -- --check | patched/unpatched 都 rc=0 |
| cargo clippy --no-default-features --features runtime-tokio --all-targets | patched/unpatched 都 rc=0；warning 集合相同（5 种唯一 / 7 条 warning 行，既有），无新增 |
| 正常 selected APPEND | Ok(())，observer 1 |
| 正常 unselected APPEND | Ok(())，observer 1 |
| 拒绝：不存在的 mailbox | Err("No")，observer 0（正常拒绝不退化） |
| 拒绝：非法 flags / 非法日期 | Err("Append")，observer 0 |
| stale SELECT 后 APPEND | unpatched Err(Append)/0；patched Ok(())/1 |
| stale SELECT 后拒绝 APPEND | unpatched Err(Append)/0；patched Err("No")/0 |
| fake EOF | Err(Append)，40 ms，不无限等待 |
| fake 本命令 tagged NO | Err(Append)，40 ms |
| fake stale continuation（只有一个陈旧的 +，之后无 tagged completion） | **2 s timeout，patched/unpatched 都一样** |

最后一行是补丁的已知盲点：continuation 没有 tag，无法区分“本命令的 +”和“上一个被取消 APPEND 遗留的 +”；若真的遇到，客户端仍可能把 body 写出去后等待 tagged completion。真实的 stale SELECT 没有 continuation，因此本轮 chain 不触发它，但不能声称补丁覆盖所有 stale +。

另外两个边界：BAD/NO 在 + **之前**（非法 flags/日期）会被统一成 Err(Append)，不再保留 No/Bad 细节；NO 在 + **之后**（不存在的 mailbox）仍是 Err(No)。+ 之后若混入**其他 tag** 的 NO/BAD，`check_done_ok_from` 先看 status、再看 tag，理论上可能误归因为本 APPEND 的失败；GreenMail 本轮没有触发，这一条是从 0.12.0 源码推断的，不是行为测试。

被阻塞/未覆盖：只跑了 runtime-tokio、default-features off 的 lib 测试；没有 async-std、压缩、docs.rs feature 组合、MSRV、完整 CI 矩阵、Dovecot/其他服务器；没有测长连接多命令并发；patch 不是上游 PR。

### 3.6 分类

- **确认的客户端取消后响应关联缺陷**：APPEND 只读一条响应，stale SELECT 的 untagged 数据导致 Error::Append，并让服务器停在等待 literal 的状态。
- **不是“服务器/网络问题”**：GreenMail trace 显示 APPEND 已收到、等待 literal；重试 SELECT 后同一连接可继续。
- **重试 SELECT 的结论**：操作可恢复，元数据不干净；第一轮“flags 在 @2 干净”只对 flags 成立，permanent_flags 仍是 12，不能推广成“会话完全干净”。
- **上游状态**：未找到与 APPEND 完全对应的 issue；PR #142 是同类先例但只覆盖 LOGIN。尚未打开 issue/PR。

### 3.7 相邻发现（非取消）：SEARCH 在 AUTHENTICATED 状态返回 Ok(0)

search-unselected 在 SELECT 之前执行 SEARCH，GreenMail 明确记录 BAD “Command 'SEARCH' not valid in this state AUTHENTICATED”，但 async-imap 返回 Ok(0)；SELECT 之后的空结果才是真正的 Ok(0)。这个现象在 patched/unpatched 都存在，和 APPEND 补丁无关，也不是本轮取消链的一部分。它是**另一个已确认的客户端错误处理问题**，本轮不修、不扩展；只作为相邻发现记录，避免把“SEARCH 0”误当成“mailbox 为空”。

## 4. BOLT：checkout 取消与 bb8 修复

### 4.1 第一轮保留与第二轮隔离

第一轮 pooled P1（6 计划/rep）和 direct D1（4 计划/rep）的结果保留：

| 模式 | 级别 | rep 1 | rep 2 | rep 3 |
|---|---|---:|---:|---:|
| P1 pooled | B1 | 0/6 | 0/6 | 0/6 |
| P1 pooled | B2 | 2/6 | 1/6 | 2/6 |
| P1 pooled | B3 | 2/6 | 2/6 | 2/6 |
| P1 pooled | M1 | 2/6 | 2/6 | 2/6 |
| D1 direct | B1 | 0/4 | 0/4 | 0/4 |
| D1 direct | B2 | 4/4 | 4/4 | 4/4 |
| D1 direct | B3 | 4/4 | 4/4 | 4/4 |
| D1 direct | M1 | 4/4 | 4/4 | 4/4 |

第一轮 pooled 的 6 个计划是 3 个 Pending × 2 race；direct 是 2 × 2。所有 rep baseline_errors=0、unsettled=0、exhaustive=true。

第二轮把 checkout 单独隔离成 checkout_only（target 只是 pool.get()），再加 pooled B2/B3/M1。原版和 fixed 各 3 rep：

| level | original r1 | r2 | r3 | fixed r1 | r2 | r3 |
|---|---:|---:|---:|---:|---:|---:|
| checkout | 2/2 | 2/2 | 2/2 | 0/2 | 0/2 | 0/2 |
| B2 | 2/6 | 2/6 | 1/6 | 0/6 | 0/6 | 0/6 |
| B3 | 2/6 | 2/6 | 2/6 | 0/6 | 0/6 | 0/6 |
| M1 | 2/6 | 2/6 | 2/6 | 0/6 | 0/6 | 0/6 |

**原版 23/60 违反；fixed 0/60。** 所有原版违反都在 #0@1:Immediate 或 #0@1:AfterWake；没有 #0@2/#0@3 违反。错误固定为：

~~~text
Err("server gave unexpected response while in Ready state.
request: Some(Reset)
response: Success(Success { metadata: {} })")
~~~

### 4.2 旧连接是被丢弃了，还是取消没实现？

代表 trace（checkout run 1）：

- 原版 canceled 后 after_target：connections=1, idle=1, created=1, proxy_connections=1。**半验证的连接被放回 idle**；取消已实现，不是“validation 没被取消”。
- 原版 survivor write 失败后：connections=0, broken=1, created=1, proxy_connections=1；直到 observer read 才 created=2, proxy_connections=2。**旧连接是在业务 Reset/Ready 错误之后才被丢弃的。**
- fixed canceled 后 after_target：connections=0, idle=0, created=1, proxy_connections=1；survivor write 前新建：connections=1, idle=1, created=2, proxy_connections=2，write Ok(7)。**取消一发生就丢弃，下一次借用重建。**
- connections_closed_invalid 在 canceled-validation 的 Invalid drop 上不会增加（bb8 只有 is_valid 返回 Err 时才记 ClosedInvalid）；所以不能用这个计数判断丢弃，应该看 connections/created/proxy。
- fixed 6/6 取消 trial 都是上述模式；原版 6/6 都是“先放回、失败后才丢”。

### 4.3 修复副本与正常路径

round2/fixed 是独立 Cargo 工程，[patch.crates-io] bb8 = { path = "vendor/bb8-0.8.6" }；解析图确认 bb8-bolt 0.8.0 -> bb8 0.8.6 (path)，其他版本不变。vendored inner.rs 的改动：

~~~rust
conn.state = ConnectionState::Invalid;
match self.inner.manager.is_valid(&mut conn).await {
    Ok(()) => {
        conn.state = ConnectionState::Present;
        return Ok(conn);
    }
    Err(e) => { /* 原有 invalid/forward_error/continue */ }
}
~~~

0.8.6 与 0.9.1 的 Pool::get 在这个 hunk 上只差 record_get_started()；手移到 0.8.6，无多版本兼容层。put_back/drop 相同。

正常/验证失败路径（两种 variant 各 3/3）：

- normal_checkout：writes [0,1,2]，last 9，proxy_connections=1、created=1、get_direct=5、get_waited=1。
- is_valid_failure（注入 FlakyManager 的 is_valid Err，不是真实服务器故障）：get=Ok、write Ok(7)、proxy_connections=2、created=2、closed_invalid=1、get_waited=2。fixed 没有破坏 bb8 的 Err→discard→retry 分支。

### 4.4 B2/B3/M1 的具体值

- 原版 B2（retry）：5 个 Err(Reset/Ready)，13 个 Ok(1)；B3：6 个 Err，6 个 Ok(0)，6 个 Ok(1)；M1 每 rep 3 Pre / 1 Post / 2 Neither，Neither 的 observed write=Err, own=0, visible=0。
- fixed B2：18 个 Ok(1)；B3：13 个 Ok(0) + 5 个 Ok(1)；M1 每 rep 5 Pre / 1 Post / 0 Neither，observed write=7, own=7, visible=7。
- fixed 的 @2/@3 计划也 clean，但**不要把它推广成“RUN/PULL 取消都被修了”**；本补丁只保护 checkout 的 validation window，@2/@3 本来就不是这个 violation 的来源。

### 4.5 replay_plan 的真实重放

在原版 checkout 重跑得到 2 plans/2 violations，取 #0@1:Immediate，源站点 ctx.target(env.checkout())（round2/shared/round2.rs:333:31），源 verdict Neither，源 observed write=Err(Reset/Ready), own=0, visible=0。

replay_observed **attempt 1 即 realized=true**：同 plan、同 site、outcome Cancelled、verdict Neither、settled true、同样的 error/values；replay report 1 trial/1 violation/unrealized 0/exhaustive true。仅尝试 1 次是因为已 realized；这是“真实 I/O 下 replay 可用”的证据，不是确定性保证。

诊断还提示：checkout 的 pre/post observation 相等（before0/write7/...），所以 atomic 只能判断“是否污染 shared state”，不能判断“操作是否发生”。这也是 pre/post 相等不能证明无效果的例子。

### 4.6 direct 的责任边界（修正第一轮）

第一轮的 direct 结果不变：取消后直接复用，B2 报 “server gave unexpected response while in Streaming state”；M1 的 write 报 RUN/状态错误，own/tag 进入 Defunct，observer 0；4/4 违反。切点后显式 reset() 立即/100 ms 后都是 0/4，说明 reset 在这个小样本里是可用的恢复工具。

但**不能**从“存在 reset + reset 后 0/4”推出“caller-only、不是库缺陷”：

- 协议上，STREAMING 的结果必须消费/丢弃后回到 READY；RESET 可以跳队列并回 READY（[server-state](https://neo4j.com/docs/bolt/current/bolt/server-state/#server-streaming)、[RESET](https://neo4j.com/docs/bolt/current/bolt/message/#messages-reset)）。这是服务器状态机要求，不是 Rust API 的取消承诺。
- 公开 API：bolt-client 有 server_state、reset、pull 等文档（[Client](https://docs.rs/bolt-client/0.11.0/bolt_client/struct.Client.html#method.reset)），reset() 发 RESET 并循环读 Success/Failure/Ignored；bb8-bolt::is_valid 正是 reset + Success::try_from，has_broken 只看 Defunct（[Manager](https://docs.rs/bb8-bolt/0.8.0/bb8_bolt/struct.Manager.html)）。README/examples 没有取消后必须 reset/reconnect 的说明或示例。**没有文档说 drop RUN/PULL future 后必须 reset/reconnect，也没有自动 Drop 清理。**
- 上游 #8 的具体评论：#issuecomment-860108448（drop 后留下未读响应）、#issuecomment-860132881（RESET 只读下一条消息，队列失步）、#issuecomment-860137879（作者承认 async Drop/状态跟踪是待办）、#issuecomment-860131118（先前认为 RESET 可以，后来被反驳）。注意 #8 的正文是 result-set iteration（PULL 多条），RESET/残留响应主要在这些评论里展开；不能只读标题/正文就断言 direct 复用的完整契约。这些评论直接说明“库承诺恢复”不成立。
- 因此准确的分类是：**direct 的失败可由 caller 用 reset 规避（有工具），但 caller 的该义务没有被公开文档明确；责任分配是设计/契约问题，尚未确认。** 不能把 reset 的存在当作库无责的证据；也不把它升级成“确认的新缺陷”。

### 4.7 BOLT 分类与上游

- **pooled**：已报告 bb8 #324/#325 的**新客户端验证**（checkout/is_valid 取消）；#8 是相关历史讨论，不是本轮因果对照的替代。bb8 层的修复已作为 [djc/bb8#324](https://github.com/djc/bb8/issues/324) / [PR #325](https://github.com/djc/bb8/pull/325) 上报（open），本轮只验证 0.8.6 兼容副本；不重复提 issue。
- **direct**：未文档化的协议/所有权边界；reset 是可用恢复手段，但不是已确认的库 bug，也不是“caller 全责”的充分证据。
- 两条线都涉及中断后的资源继续使用，但目前只对 pooled 的 bb8 validation 做了因果修复对照；direct 责任仍未决，不合并成一个已确认根因。

## 5. 统一修正：第一轮哪些认识需要改

1. **SMTP 不是“M1 无法比较返回码”。** 第一轮的 M1 observation 确实没有记录 SmtpResult；第二轮 M1R2 加了具体 code，才看到 18 个 354。第一轮的 2/8 仍对“只看 Ok”的 oracle 有效，但不能再说“无法观察返回码”。
2. **pre==post 不是“没有效果”。** SMTP 的 pre 是 survivor+third（2 条），post 是 target+survivor+third（3 条），二者不相等且各自稳定；BOLT checkout 的 pre/post 相等只说明所选观察区分不了两条基线，不能说明共享状态完全未受污染或目标操作没发生（replay 诊断明确提示）。判断效果要用独立 observer/具体返回值。
3. **BOLT direct 不是已证明的“调用方全责”。** 协议要求状态、库有 reset，但库没有取消契约，也没有自动清理；上游作者承认状态跟踪未完成。正确说法是“调用方有 reset 工具，责任/契约未文档化”，不是“库已无责”。
4. **Pending/plan 编号不是协议 stage 标签。** #0@1 只是 plan/Pending 序号；SMTP 只有 #0@3 有真实 trace，IMAP 的 @1/@2 来自 chain trace，BOLT 的 @1 来自 checkout-only 隔离。不要把编号写成“协议第二阶段”。
5. **违反计划数不是根因数。** SMTP 24 个执行、IMAP 22 个 ladder 违反、BOLT 23/60 都是执行/计划计数；每个客户端是一条主根因，现象类别另算。不能把 24 当成 24 个独立 bug。
6. **重试 SELECT 不是“Ok 就干净”。** 它恢复 APPEND/SEARCH/FETCH，但 Mailbox.flags/permanent_flags 仍被陈旧 untagged 数据污染；第一轮“@2 的 flags 干净”只对 flags 成立，permanent_flags 仍是 12。
7. **SMTP 的 354 不是“消息丢了”。** 18 个 false-354 的消息实际投递；问题是返回值和后续复用。真正无效果的是 6 个 #0@3 超时。
8. **Settle::ZERO 不是 Dropwise 缺陷。** 该版本公开 API 没有这个常量；本轮显式构造 zero settle。任务简报与 API 的差异不进入缺陷/验收结论。

## 6. 分诊与实际状态

| 线 | 状态 | 证据 | 下一步（需授权） |
|---|---|---|---|
| SMTP false 354 + DATA hang | **确认的用户可见客户端缺陷；上游待分诊** | 3 rep M1R2 8/8；18 false-354 + 6 timeout；trace；poison 对比 24/24 显式错误 | 报告确认现象；poison 补丁的拒绝响应行为已补测，见 §9 |
| IMAP APPEND stale response | **确认的客户端取消后响应关联缺陷；上游待分诊** | unpatched B3 10/10 realized Err；M1 12/12 Neither；patched 0；86 tests | APPEND 补丁上游化；stale +、metadata、feature 矩阵 |
| IMAP retry SELECT metadata | 同一失步机制的表现；非独立根因 | @1 flags 10/permanent 12，@2 flags 5/permanent 12；server 5/6 | 决定修 parse_mailbox 还是文档化/重连 |
| IMAP SEARCH unselected | 相邻的确认客户端错误；非取消 | server BAD，client Ok(0) | 另案，不并入本轮 |
| BOLT pooled checkout | **已知 bb8 #324 的新客户端验证**；修复有效 | 原版 23/60；fixed 0/60；旧连接先回池后丢；replay realized | 作为 #324/#325 的 BOLT 证据补充（需授权） |
| BOLT direct | **未文档化契约/设计边界；非确认新缺陷** | 4/4 不复用失败；reset 0/4；#8 评论；无取消文档 | 文档/上游讨论；不把它写成 caller-only 结论 |
| Settle::ZERO | 不是缺陷 | 公开 API 无此常量；显式 zero settle 可用 | 无 |

## 7. 第二轮交卷时的验收与未解决项（收尾更新见 §9）

### 可以内部验收的

- 三条线的 runbook/环境/日志可复现；BOLT 的 0.8.6 修复对照是 0/60，SMTP 是 24/24 个取消计划转为显式错误，IMAP 是 10/10 个已实现 APPEND 从失败转为成功（patched 0 违反）。
- 统一判据已落实：具体返回值 + 独立 observer + realized/unrealized + 分母说明；没有把 Ok 当成成功，也没有把 plan 数当根因数。
- BOLT pooled 的“旧连接先回池、失败后才丢”与“fixed 取消即丢、重建”有 stats/trace；replay 真实 realized 1/1。
- IMAP 的 fail-before/pass-after 测试、正常/拒绝路径、fmt/clippy/上游 73 测试都可复核。

### 还不够上游报告质量的

- SMTP：只 GreenMail；#0@1/#0@2 没有 wire trace；patch 采用“poison + reconnect”，会改变 4xx/5xx 后行为；上游未确认语义。
- IMAP：stale + 无法区分；parse_mailbox 元数据未修；只 runtime-tokio；patch 不是上游 PR；SEARCH unselected 另案。
- BOLT：bb8 修复已在上游 #325（open），本轮是 0.8.6 兼容验证，不是新代码；is_valid_failure 是注入错误；@2/@3 与一般 RUN/PULL 取消不在 patch 范围；direct 责任未决。
- 没有提交、推送、开 issue/PR/评论/邮件；没有修改 Phase 4 主表/门槛/预算/冻结状态；没有读 PRIVATE-NOTES.md。

### 仍未解决

- SMTP 在 #0@1/#0@2 的精确命令序列（目前只有 354 行为推断）；其他 SMTP 服务器；RSET/QUIT 在 DATA 模式的行为。
- IMAP 在真实 stale +、并发命令、Dovecot 等其他实现下的行为；Mailbox 元数据修复是否应独立上报；完整 feature/MSRV CI。
- BOLT 在 pool size >1、TLS、Bolt 5、连接生命周期、一般 RUN/PULL 取消下的行为；direct 的契约应由上游文档/设计明确。
- SEARCH unselected 的错误处理是相邻缺陷，本轮明确不扩展修复。

## 8. 第二轮交卷时的成本、产物与清理

- 成本：三条线第二轮各是分钟级本地运行（SMTP 原版约 2 min + fixed 约 3 min；IMAP 约 4 min；BOLT 约 3 min），主要是构建和 3 rep；没有新增长期服务或外部依赖。
- 新增代码只在 exploration 目录：SMTP tests/runner/patch、IMAP round2/patched/runner、BOLT round2/runner；Dropwise 核心未改。
- 第一轮文件、日志、环境保持原样；第二轮全部用 round2-* 名字；没有新的 archive/evidence pack/重复 validation report。
- 容器：各 runner 使用 disposable 容器并在退出时删除；收尾时确认没有本轮的 dw-p4-*/dw-explore-* 残留（IMAP 开发期容器在 agent 完成后清理）。
- 未提交、未推送、未开 issue/PR/评论/邮件。

### 关键产物

- [README.md](README.md)：两轮条件与运行入口。
- [clients/smtp/RESULTS-FRAGMENT.md](clients/smtp/RESULTS-FRAGMENT.md)、[SCENARIO.md](clients/smtp/SCENARIO.md)、[tests/levels.rs](clients/smtp/tests/levels.rs)、[run-round2.sh](clients/smtp/run-round2.sh)、[run-round2-fixed.sh](clients/smtp/run-round2-fixed.sh)、[patches/async-smtp-poison.diff](clients/smtp/patches/async-smtp-poison.diff)、[logs/](clients/smtp/logs/)。
- [clients/imap/RESULTS-FRAGMENT.md](clients/imap/RESULTS-FRAGMENT.md)、[SCENARIO.md](clients/imap/SCENARIO.md)、[src/main.rs](clients/imap/src/main.rs)、[round2/src/main.rs](clients/imap/round2/src/main.rs)、[patched/append-continuation.round2.patch](clients/imap/patched/append-continuation.round2.patch)、[run-round2.sh](clients/imap/run-round2.sh)、[logs/](clients/imap/logs/)。
- [clients/bolt/RESULTS-FRAGMENT.md](clients/bolt/RESULTS-FRAGMENT.md)、[SCENARIO.md](clients/bolt/SCENARIO.md)、[round2/shared/round2.rs](clients/bolt/round2/shared/round2.rs)、[run-round2.sh](clients/bolt/run-round2.sh)、[logs/](clients/bolt/logs/)。


## 9. 收尾复核（2026-10-21）

本节是第二轮之后的独立代码审查与必要复跑；上面保留的第二轮数字不被覆盖。
本轮没有扩大目标名单，也没有修改 Dropwise 核心或正式 Phase 4 计划。

### Runner

所有入口使用新建的日志子目录，不覆盖两轮历史输出。IMAP/SMTP 的临时工作目录
由 mktemp 独占创建；用户传入的路径只作为父目录，不再先递归删除。退出和信号
清理只处理本次拥有的资源。SMTP runner 不再无条件返回成功：有完整违反报告的
预期 exit 1 与 panic、构建失败、基线错误、外层超时分开。4 项无服务 runner
测试（含多种退出状态子案例）通过，全部 shell 入口语法检查通过。

### SMTP：普通拒绝响应实测，不把归因补丁当正式修复

新增 `clients/smtp/tests/rejections.rs`，只用进程内脚本化 peer，不需要 GreenMail。
5 项测试在发布版和 poison 副本均按各自预期通过，覆盖：正常连续发送与 PIPELINING、
MAIL/RCPT/DATA/最终回复阶段的 450/550（非 pipeline）、pipeline 最终拒绝、异常最终
354、精确 DATA 挂起后取消及重连。这里“补丁测试通过”表示**确认其行为**，不是
认定所有行为正确：

- 原版在已完整读取的普通 450/550 后可以下一次成功发送；实验补丁立即拒绝后继发送，
  且没有发出新命令。拒绝响应导致永久 poison 的回归已经证实，不再标为未测。
- 实验补丁能拒绝错误的最终 354，并在取消后阻止复用；它没有完成错误恢复策略设计，
  不直接作为上游生产修复提交。最后回复 354 是有意的负控制，不是正常服务端行为。
- 脚本化 DATA gate 在当前 upstream master 也复现了取消后重试超时，新连接正常；
  运行测试额外开启 `tokio/sync`，没有修改上游实现。
- GreenMail 原版/补丁各补跑一轮：M1R2 8/8 违反（6 个 354、2 个 timeout）；
  poison/reconnect 对照成功。合法拒绝复用是不同契约，不记为同一 Atomic 场景零违反。

新测试的 Clippy（-D warnings）通过。原实服务三轮日志仍保留。

### IMAP：通知与 BYE 修正

原 round-2 sketch 在等待 continuation 时丢弃所有其他回复，会丢失正常 unsolicited
通知，也没有及时处理 BYE。`append-continuation.review.patch` 改为使用已有
`handle_unilateral` 转交通知，BYE 返回 ConnectionLost 且不发送 literal。原始
round-2 patch 保留；apply runner 默认使用 review patch。

- 发布版原有 73 tests 通过；15 项附加测试中 11 通过、4 预期失败。
- review 副本 88/88 通过；格式通过；Clippy 成功且与原版同样五类既有警告，不宣称零警告。
- GreenMail review 三轮：原版 B3 为 12/12 已实现计划失败，M1 为 11/11 已实现计划违反
  （另 1 未实现）；review B3 为 0/11（另 1 未实现），M1 为 0/12。
- 重试 SELECT 的元数据污染没有由这个 APPEND 补丁修复；旧 continuation 的无标签歧义
  也仍未解决，不承诺任意取消后都能恢复。

上游 main 上另整理了六项小回归测试：三项在原版失败，补丁后 Tokio/async-std 各
79 tests 通过；两运行时的 compress/all-targets 检查及 Rust 1.88 Tokio 测试通过。
当前新解析的 pin-utils 0.1.1 带来 unsafe_pinned 的四个弃用警告，`RUSTFLAGS=-Dwarnings`
在**未改的上游 main 同样失败**。未改 CI、未压制警告；普通测试通过不等于严格 CI 全绿。

### BOLT 与集中验证

重新运行原版/fixed 三轮，得到原版 22/60 违反、fixed 0/60，均无未实现计划；
与第二轮 23/60 的一例差异来自原版 B2 的实时调度，未改写旧数字。验证期间取消后
丢弃连接的修复效果保持，作为已有 bb8 #324/#325 的真实适配器证据。
Dropwise 根 crate 的 net 测试再次通过。没有残留本次的实验容器；其他项目容器未动。

本轮实际运行输出：IMAP `logs/run.BoHOuF`（依赖测试）、`logs/run.RIqBa8`（真实服务）；
SMTP `logs/run.k6pHPj`（原版）、`logs/run.6kB1B2`（poison）；
BOLT `logs/run.fRK0vO`。这些是普通 runner 输出，不是重新生成的历史数据。
