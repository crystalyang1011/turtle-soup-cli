# 04 · 题源与 ETL

> 原则一句话：题目一律来自真人社区验证过的开源数据集，不让 AI 编撰。
> 依赖的数据结构见 [03 §1](03-数据结构与持久化.md)；key_facts 的玩法作用见 [01 §4](01-游戏设计.md)。

---

## 1. 原则（已拍板）

题目一律来自真人社区验证过的开源数据集，**构建期 ETL 一次性入库；运行时不做 web fetch**；AI 原创编题降级为二期补充通道且**必须人工审核后入库**。

---

## 2. 数据集

| 数据集 | 规模 | 提供什么 | 许可 | 本项目中的角色 |
|---|---|---|---|---|
| **TurtleBench1.5k**（HF: Duguce/TurtleBench1.5k；论文 arXiv:2410.05262，ICASSP 2026） | 1,532 条真人猜测标注，源自线上海龟汤平台 26,000+ 次真实对局 | staging `stories.json`：surface + bottom 配对 | Apache 2.0 | **题源主库**：surface / truth 直接采用 |
| **DeepTurtle**（GitHub: Yuikij/DeepTurtle） | 61 道人工精修 Golden Samples | logic_rules（"IF 玩家问 X THEN 判 YES/NO"） | MIT | 判定规则参考 + 补充题库 |
| **TurtleBenchmark**（GitHub: iamsk/TurtleBenchmark） | 32 个故事 + 1,537 条真人提问的人工标注（对/错/无关） | 真实玩家问题 + 人工标注答案 | 开源 | **M1 验收基准**（见 [08 §3](08-工程化与验收.md)） |

**依据等级标注**：以上来自各仓库 README 与论文原文（官方一手信息），**尚未实际拉取数据核验字段格式**——ETL 脚本落地时以真实数据为准（见 §6.2 风险）。

---

## 3. key_facts 挖掘（全项目质量命门）

数据集没有现成的 key_facts 字段，这是 ETL 的关键一步，也是整局玩法（进度/提示/胜负）的锚点：

1. TurtleBench 被人工标注为"正确（T）"的玩家猜测 = **真人验证过的真命题**。
   **数据形态（2026-10 已核验）**：CH/EN 各一个 JSONL，**每行一条"猜测-标注对"**，字段
   `id / title / surface / bottom / user_guess / label`（`label ∈ {T,F}`）。因此必须先
   **按 `surface`（辅以 `bottom`）分组**，再收集该故事下 `label=="T"` 的 `user_guess`。
   中文集全量 1,532 行、**唯一故事 32 个**（见 §6.1）。
2. 同一故事的 T 标注猜测去重、合并、改写为事实句 → `key_facts`（AI 仅做合并改写，产物可逐条人工核对）；
3. **每条 fact 打 `core` 标记**：因果链的"因"与"果"标 `core=true`（1–2 条），其余 `false`（见 [01 §4.1](01-游戏设计.md)）；
4. T 标注不足 **4 条**的题**不入库**（v1.0 是 3 条，配合 [01 §4.2](01-游戏设计.md) 收紧为 4，避免"3 条必须全中"的退化）；
   中文集每故事 T 数 5–54，**上限 6 条**：运行时 ETL 无 LLM、无法做语义"合并同类项"，按去重后的
   原序**机械截断到 6 条**（首条标 `core=true`），语义合并与 core 校准留待人工/AI 后处理；
5. **质量校验（v1.0 缺失）**：入库前对每条 fact 做可判定性自检——
   - 是否独立可判定（可被单个是/否回答）；
   - 是否与汤底矛盾（自洽性）；
   - 是否与另一条 fact 语义重复（去重）；
   - 报告不通过率，人工抽检 ≥10% 并逐条核对。

---

## 4. 内容过滤与合规

**过滤口径（已拍板，2026-09-29）**：**只过滤血腥与色情，不过滤恐怖/灵异**。恐怖、悬疑、重口味氛围题照常入库、照常参与对局；以下内容在 ETL 阶段剔除：

1. **血腥**：尸体/伤害/残肢等暴力细节描写；
2. **色情**：性描写、露骨内容。

实现：关键词初筛（血、尸、肢解、性…）+ 人工过一遍 → 命中直接**不入库**并记录过滤日志。恐怖/灵异**不设**过滤词。

- `tags` 仅作**描述性标签**（`经典`/`日常`/`反转`），不承担默认屏蔽职责（[03 §1](03-数据结构与持久化.md)）。
- 题库页提供**单条删除**，用户可自行剔除不喜欢的题。

**合规**：Apache 2.0 / MIT 均允许个人使用与修改；本项目单机自用不分发不商用。**汤面/汤底内容本身的来源为线上海龟汤社区**，个人自用范围内使用；若后续分发需重新评估内容授权。

---

## 5. 数据获取策略：直链下载 JSONL，不打包全量

已拍板，2026-09-29；**2026-10 改版**：改用仓库文件**直链下载**，废弃 datasets-server rows API。

1. 数据集**不打包进发行版**。出厂仅内置开发期预拉、人工筛选的小批题（MVP 5 题），保证开箱即玩、离线可玩。
2. 应用内 **`soup-cli fetch`**：用户手动触发，下载中文集 JSONL → 本地 ETL 清洗（按故事分组 + 字段映射 + key_facts 挖掘 + 血腥/色情过滤）→ 直接入库。个人工具不做审核 UI（T 标注挖掘已保质量底线），题库页提供单条删除。
3. **断点游标**：本地记录已消费的故事 id（`consumed_ids`）与计数（`offset`），幂等，重跑不重复入库。
4. **原始数据先落盘再清洗**：缓存到项目根 `data/raw/` 目录，ETL 可重跑。
5. **拉取端点（实测可用）**：
   - 官方：`https://huggingface.co/datasets/Duguce/TurtleBench1.5k/resolve/main/chinese/zh_data-00000-of-00001.jsonl`
   - 镜像：`https://hf-mirror.com/datasets/Duguce/TurtleBench1.5k/resolve/main/chinese/zh_data-00000-of-00001.jsonl`（`--mirror` 切换）
   - **为何不用 rows API**：`datasets-server.huggingface.co` 国内直连超时；`hf-mirror.com/datasets-server/rows` 实测返回 **401**。直链文件两个源都可达。
6. **体积事实**：中文集 JSONL 约 1.03 MB，全量一次下载即可，无需分页。

---

## 6. 质量校验、风险与题量兜底（v1.0 只标注风险，无兜底）

### 6.1 风险

1. **1,532 是"猜测-标注对"数，不是题目数**；**已核验：中文集唯一故事仅 32 个**（`surface` 去重）。
2. 有效题量可能枯竭：32 个故事里 T 数 <4 的会再被丢弃，实际入库题量有限。
3. key_facts 依赖 T 标注质量，标注噪声会传导到判定；且 >6 条被机械截断，顺序未必是因果主链。

### 6.2 兜底方案（按触发条件）

| 触发条件 | 兜底动作 |
|---|---|
| 清洗后入库题 < 30 道 | 启用 DeepTurtle 61 道 Golden Samples 作为第二题源 |
| 清洗后入库题 < 10 道 | 启用二期"AI 出题 + 人工审核"通道，**提前**到 MVP 之后立即做 |
| 有效题量枯竭（玩完无题可拉） | 扩展题源：其他社区海龟汤数据集 / 允许用户手工导入（粘贴 surface+truth，工具辅助拆 facts） |
| 数据字段与预期不符 | ETL 以真实数据为准，字段映射写成可配置；不通过则换题源 |

### 6.3 首次拉取验证清单（M2 前置）

- [x] 数据集真实唯一故事数：中文集 **32**
- [ ] T 标注覆盖率（平均每故事几条 T）：中文集 5–54，平均约 14
- [x] 字段与编码：`chinese/zh_data-00000-of-00001.jsonl`，UTF-8，`id/title/surface/bottom/user_guess/label`
- [x] 许可文件随仓库提供：`LICENSE`（Apache-2.0）

---

## 7. ETL 单一事实源（v1.0 的双份逻辑隐患）

v1.0 同时声明 `scripts/etl_turtlebench.py` 与 `dataset.rs` 的清洗规则"一致" → 两份代码人肉同步必然 drift。

**规定**：

- **`scripts/etl_turtlebench.py` 是清洗规则的唯一事实源**（开发期用于预拉内置题，也用于输出"规则基准测试用例"）。
- `dataset.rs`（运行时拉取）**只做字段映射 + 复用同一套规则**；规则以数据驱动方式表达（关键词表、core 标注规则、合并策略写成配置/常量），并与 python 版共用一份 `etl_rules.json`。
- **一致性校验**：python 产出一小批样本（含输入→输出），Rust 侧单测跑同一批输入比对结果，作为 CI 检查项。

```
raw/ ──> [field map] ──> [filter gore/nsfw] ──> [mine key_facts] ──> [mark core] ──> [validate] ──> puzzles.json
                                        └────────── etl_rules.json（两端共用） ──────────┘
```

---

## 8. 字段映射（已按真实 JSONL 核验，2026-10）

原始 JSONL 每行一条猜测记录：`{ id, title, surface, bottom, user_guess, label }`。
ETL 先按 `surface`（辅以 `bottom`）分组，再取组内 `label=="T"` 的 `user_guess` 作为 key_facts 候选。

| 目标字段 | 来源 | 说明 |
|---|---|---|
| id | 生成 | `ds-<surface 稳定哈希>`（故事级；同一故事多行共享） |
| title | JSONL `title` | 故事标题（可能为空） |
| surface | JSONL `surface` | 汤面，公开可下发（分组键） |
| truth | JSONL `bottom` | 汤底，仅 Rust 侧可见 |
| key_facts | 组内 `label=="T"` 的 `user_guess` | 见 §3，去重后 4–6 条（>6 机械截断），首条 `core=true` |
| difficulty | `fetch --difficulty` 参数 | 数据集无此字段 |
| tags | 固定 `["dataset"]` | 仅描述（过滤口径见 §4） |
| source | 固定 `dataset` | 来源标记 |
| created_at | 入库时间戳 | 溯源用 |
