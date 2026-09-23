# 独立核验流程

目的：用两个互相独立的盲标注者核验初始标注（`data/labels.jsonl`，由设计编码手册的 claude-opus-5-5 会话产生，不是盲标），报告一致性并估计漏标。

- 标注者 A：Claude Opus 5.5（high），只用 `*_A.csv`、`v3_A.csv`
- 标注者 B：ChatGPT 6 Astra（xhigh），只用 `*_B.csv`、`v3_B.csv`
- 分歧裁决：第三个模型或项目负责人，只处理分歧条目

论文中如实报告为“两个不同模型家族的独立盲标注 + 与初始标注的一致性”，并公开全部标签及理由。

## 文件

| 文件 | 内容 | 谁填 |
|---|---|---|
| `positives_A.csv` / `positives_B.csv` | 模型判为真实缺陷、相邻问题或证据不足（yes / adjacent / unclear）的全部候选，顺序已打乱，**不含模型标签** | 标注者 A / B 各填自己那份 |
| `negatives_A.csv` / `negatives_B.csv` | 模型判为无关的候选中，按置信度和仓库分层随机抽取的样本 | 同上 |

两人拿到的是同一批条目，只是顺序不同。**标注者 A 只打开 `*_A.csv`，标注者 B 只打开 `*_B.csv`。**

## 步骤

1. **先读 `../../CODEBOOK.md`**，有疑问先讨论、修改手册，再开始标注。开始后手册冻结。
2. **各自独立标注**，不要看对方的表，也不要看 `data/labels.jsonl`、`data/annotate.csv` 或 `labels/batch*.py`（里面有模型标签）。
3. 正例表：先判断 `relevant`（yes / adjacent / dup / unclear / no），只有 `yes` 才需要填其余维度。多值字段用 `;` 分隔。每条都应打开链接读原文（issue、PR、必要时读修复的 diff）。
4. 负例表：先判断 `relevant`；若判为 `yes`，其余维度同正例表一样填完。拿不准写 `unclear` 并在 `notes` 里写原因。
   标注规则见手册的 “Annotation rules (v2.1)” 一节（证据标准、取消瞬间、单一 holder、injectable 的含义、去重、独立性）。
5. 两人都完成后运行：

   ```sh
   python3 agreement.py
   ```

   得到 A 与 B、A 与模型、B 与模型三组一致性（原始一致率和 Cohen's kappa），以及分歧清单。
6. **裁决**：两人逐条讨论分歧，结论写入 `../human/adjudicated.csv`（列：`url` 加需要覆盖的字段）。人工标签优先于模型标签，`survey.py export/stats` 会自动采用。
7. 报告时注意：
   - 裁决**之前**的一致性才是独立一致性；
   - “人看了模型标签后同意”不能算独立一致；
   - 负例样本里发现的真实缺陷数，用于估计模型漏标率（附置信区间）。

## 重新生成表格

`python3 make_sheets.py [--seed N] [--negatives 60]`。已经有填写内容的表格不会被覆盖。

## 第二轮（编码手册 v3）

第一轮（v2.1）的结果：`relevant` 由 `build_consensus.py` 合并（两人一致取其值，分歧见 `adjudicated_relevant.csv`，裁决者为初始标注会话，只在 A、B 的结论之间选择并写明理由），输出 `../human/relevant.csv`。维度字段中 `tags`（κ=0.19）和 `injectable`（κ=0.30）不可靠，已从 v3 删除。

第二轮只对最终 `relevant=yes` 的 80 条重标 v3 维度：`v3_A.csv` / `v3_B.csv`。两人完成后再运行 `build_consensus.py`，一致的取值写入 `../human/v3.csv`，分歧标为 `disputed`，裁决写入 `adjudicated_v3.csv` 后重跑。

## 第二轮实际标注者

v3 重标与粗筛：A = GPT 6 Sol（medium），B = Claude Opus 5.5（low）。与第一轮（A = Claude Opus 5.5 high，B = ChatGPT 6 Astra xhigh）不同，两轮 kappa 的差异同时包含手册修订和标注者变化两个因素。

## 第三轮：精读

`full_{A,B}.csv`：两人粗筛 keep 的并集（228 条）加 neon 补检索的 22 条（未经粗筛），共 250 条，按 v3 完整标注（含 relevant）。
