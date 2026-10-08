# 海龟汤（CLI 摸鱼版）

> 终端里跑一局海龟汤：你只能问"是/否"，AI 当主持人，把碎片拼成真相。
> 设计文档见 [`design-doc/`](design-doc/)（**唯一事实源**）。命令行 `cargo run` 即随机来一局。
>
> v3.0 起**只做 CLI**，没有桌面端。

---

## 项目结构

```
turtle-soup/
├── design-doc/          # 设计文档（唯一事实源）
├── src/
│   ├── bin/soup_cli.rs  # CLI：子命令 + REPL + 渲染 + 摸鱼件
│   ├── models.rs        # Puzzle / Session / 错误码
│   ├── config.rs        # 配置加载 + 校验（多路径查找）
│   ├── secrets.rs       # API Key（env > config 内联）
│   ├── prompts.rs       # 三段 prompt 全文（带版本号）
│   ├── llm.rs           # OpenAI 兼容适配层
│   ├── engine.rs        # 判定/评分/胜负/进度（纯函数，单测覆盖）
│   ├── session.rs       # 状态机 + 原子持久化 + schema 迁移
│   ├── dataset.rs       # 分批拉题 + ETL + 断点游标
│   ├── game.rs          # 对局编排
│   └── logging.rs       # 本地日志（脱敏）
├── scripts/etl_turtlebench.py   # 开发期 ETL（清洗规则事实源）
├── assets/puzzles.json  # 出厂内置 5 题
├── config.json          # 本地配置与密钥（.env 理念，已 gitignore）
├── config.example.json  # 配置样例
└── Cargo.toml
```

---

## 环境准备

| 依赖 | 版本 | 说明 |
|---|---|---|
| Rust | 1.98 `x86_64-pc-windows-gnu` | rustup 装的 GNU 工具链 |
| MinGW GCC | 16.2 | GNU 链接器（`scoop install mingw`） |

若 `cargo` 找不到，当前终端补一次 PATH（或重启终端）：

```powershell
$env:Path = "$env:USERPROFILE\scoop\apps\mingw\current\bin;$env:USERPROFILE\.cargo\bin;$env:Path"
```

---

## 配置

把 `config.example.json` 复制为项目根的 `config.json`，填上你的厂商与 Key（`.env` 理念）：

```json
{
  "base_url": "https://ark.cn-beijing.volces.com/api/coding/v3",
  "api_key": "你的 key（也可留空，用环境变量 TURTLE_API_KEY）",
  "model": "glm-5.3-flash",
  "host_model": "glm-5.3-flash"
}
```

- 查找顺序：`config.json` → `TURTLE_CONFIG` 环境变量 → 当前目录 → 项目根 → 项目根 `data/`。
- Key 优先级：环境变量 `TURTLE_API_KEY` > `config.json` 内联。
- `config.json` 已 gitignore，**不会进 git**。

---

## 使用

```powershell
cargo run                              # 随机来一局（默认）
cargo run -- play                      # 同上（显式）
cargo run -- play classic-001          # 指定题
cargo run -- play --difficulty 3       # 按难度随机
cargo run -- list                      # 看题库
cargo run -- ask classic-001 "他在打嗝吗"   # 单次判定（脚本/冒烟）
cargo run -- fetch                     # 拉新题（下载并清洗 TurtleBench 中文题源）
cargo run -- fetch --mirror            # 国内网络走 hf-mirror 镜像
cargo run -- config                    # 打印配置与日志路径
```

对局内指令：

| 输入 | 作用 |
|---|---|
| 直接输入 | 提问（是 / 否 / 无关 / 一半） |
| `/guess <推理>` | 猜底（次数不限，失败扣分后继续） |
| `/hint` | 方向提示（不泄底、不分级、次数不限） |
| `/switch [id]` / `/hide` | 切换题目 / 标记当前题不再显示 |
| `/status` | 查看进度、已用提示、猜底失败次数 |
| `/quit` | 挂起并退出（`Ctrl+C` 同样会落盘退出） |

编译好的单文件在 `target\debug\soup-cli.exe`。

---

## 开发

```powershell
cargo test   -p turtle-soup
cargo clippy -p turtle-soup --all-targets -- -D warnings
```

约定见 [`AGENTS.md`](AGENTS.md)；日志在项目根 `data/logs/app.log`（`TURTLE_LOG=debug` 打开 DEBUG 级）。
