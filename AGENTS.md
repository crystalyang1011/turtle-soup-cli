# AGENTS.md · TUI 游戏平台（终端版）

面向在本仓库工作的 AI 编码代理，只讲**怎么干活 / 什么不能碰**。

- 产品定位、项目结构、配置与使用 → 见 [`README.md`](README.md)（避免重复，以它为准）。
- 玩法数值、数据结构、接口契约、安全设计 → 见 [`docs/`](docs/)（**唯一事实源**，按阶段分目录）。
- 本项目是 **TUI 游戏平台**：终端优先，无 GUI（Tauri/Vue 版**已移除**，不要再引入前端）；
  海龟汤 CLI 是阶段一已封存的第一个游戏；当前阶段二做 TUI 壳（[`docs/2-tui/`](docs/2-tui/)），
  终局支持 game pack（写配置加游戏）。

---

## 1. 动手前

1. 先读相关 `docs/` 章节，再读要改的代码。
2. 新 shell 先补 PATH（本机 Rust 为 GNU 工具链，需 MinGW 链接器）：

```powershell
$env:Path = "$env:USERPROFILE\scoop\shims;$env:USERPROFILE\scoop\apps\mingw\current\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
```

---

## 2. 改完必跑（全部要绿）

```powershell
cargo test   -p turtle-soup
cargo clippy -p turtle-soup --all-targets -- -D warnings
```

- 判定引擎改动的回归口径：`docs/1-turtle-cli/05-Prompt设计.md §6`（回放一致率、红队样本）。
- 手测：`cargo run -p turtle-soup -- ask <id> "<问题>"`；交互式 `play`。

---

## 3. 硬性约束（违反即打回）

1. **纯 CLI / 零 GUI**：不得引入 Tauri、Vue、webview 相关任何东西；不得新增前端目录。
2. **分层铁律**：`engine.rs` 是**纯函数**（无网络/文件/终端）；碰 IO/网络的放 `llm.rs`/`session.rs`/`dataset.rs`；
   终端渲染只写在 `bin/soup_cli.rs`。`game.rs` 是对局编排的**唯一实现**，勿在别处再写一份。
3. **文档即契约**：CLI 子命令 / JSON schema / 配置字段变更 → **先改 `docs/` 对应章节，再改代码**。
4. **prompt 版本化**：改 `src/prompts.rs` 必须同步回写 `docs/1-turtle-cli/05`，并递增 `PROMPT_VERSION`。
   判定行为不对**先改 prompt**，禁止在渲染层打补丁。
5. **数据结构变更**：递增 `SCHEMA_VERSION` 并写迁移（`src/session.rs::migrate_*`），迁移前自动备份。
   见 `docs/1-turtle-cli/03 §6`。
6. **密钥**：API Key 来源 = 环境变量 `TURTLE_API_KEY` > `config.json` 内联 `api_key`。
   `config.json` **必进 `.gitignore`**；Key 绝不进日志、终端回显、git 提交（`logging::redact` 兜底）。
7. **反泄底**：`truth` / `key_facts` 只存在于 Rust 侧与 system message，**结算前绝不渲染到终端**；
   LLM 返回的 `reply` 必须过 `engine::reply_leaks()`。见 `docs/1-turtle-cli/07 §2`。
8. **禁止静默降级**：LLM / 解析失败要返回明确错误码并打印日志路径，不得伪装成"无关"判定。

---

## 4. 代码风格

**通用**：注释用中文；**每个文件头一行 `// by AI.Coding`**；公共类型/函数写 `///` 文档注释；
关键业务分支引用设计文档章节（如 `见 docs/1-turtle-cli/01 §5`）。标识符/键名英文，面向玩家的文案中文。

### 4.1 核心（`src/*.rs`）

- **错误**：统一 `AppError { code, message, retryable }` + `ErrorCode`（`src/models.rs`），
  用 `thiserror`/`?` 传播；禁止在业务路径 `unwrap()/expect()`。
- **纯函数优先**：规则（评分、胜负、解析、命中计算）写进 `engine.rs` 并配 `#[cfg(test)]` 单测。
- **数据契约**：磁盘存储 `snake_case`（见 `docs/1-turtle-cli/03`）。
- **依赖**：serde / serde_json / thiserror / reqwest(rustls) / tokio / directories / tempfile
  / ratatui / crossterm；**不加** keyring / clap / tauri 等。新增依赖前先说明理由。

### 4.2 CLI（`src/bin/soup_cli.rs`）

- 终端的控制序列（清屏、改标题）**只在 stdout 是 TTY 时发**（`std::io::IsTerminal`），
  重定向时退化为纯文本。
- 长耗时调用（LLM）包一层 `with_spinner`，别让用户以为卡死。
- 退出路径（`/quit`、`Ctrl+C`、EOF）都必须先把对局落盘。

---

## 5. 改哪里（任务 → 文件）

| 想改… | 去哪（并回写对应文档） |
|---|---|
| 玩法数值 / 评分 / 胜负 | `src/engine.rs` ← `docs/1-turtle-cli/01` |
| prompt | `src/prompts.rs` ← `docs/1-turtle-cli/05` |
| 数据结构 / 持久化 | `src/models.rs`、`src/session.rs` ← `docs/1-turtle-cli/03` |
| LLM 适配 / 配置 / 密钥 | `src/llm.rs`、`src/config.rs`、`src/secrets.rs` |
| 题源 / ETL 规则 | `scripts/etl_turtlebench.py`、`scripts/etl_rules.json`、`src/dataset.rs` ← `docs/1-turtle-cli/04` |
| 对局编排 | `src/game.rs` |
| CLI 命令 / 渲染 / 摸鱼件 | `src/bin/soup_cli.rs` ← `docs/1-turtle-cli/02 §4`、`06` |
| 日志 | `src/logging.rs` ← `docs/1-turtle-cli/07 §7` |
| 内置题 | `assets/puzzles.json`（4–6 条 key_facts，1–2 条 core） |
| TUI 界面（新阶段） | `src/ui/` ← `docs/2-tui/` |

---

## 6. Git

- **只在用户明确要求时** commit / push；不得改完代码后主动顺手 commit。提交信息用中文，说明"做了什么 + 为什么"。
- 提交前 §2 全绿；`config.json`/`data/`/`sessions/`/`raw/`/`target`/`logs/` 已在 `.gitignore`，勿误提交。
