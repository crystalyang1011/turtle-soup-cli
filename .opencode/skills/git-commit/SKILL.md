---
name: git-commit
description: 本工程（TUI 游戏平台）的 Git 提交工作流。在用户要求提交代码（"commit"、"提交"、"提交一下代码"）时使用；生成符合本工程规范的中文 commit message 并完成提交。不负责 push（除非用户明确要求），不使用其他工程的 code-commit 规范。
license: MIT
metadata:
  scope: turtle-soup
  language: zh
---

# git-commit · 本工程提交工作流

## 规范来源（不复制，防止漂移）

提交纪律的唯一事实源是仓库根的 [`AGENTS.md`](../../../AGENTS.md) §6。执行本 skill 前必须先读它；
两者冲突时以 AGENTS.md 为准并回修本文件。

## 流程（顺序执行，不可跳步）

1. **检查现场**：`git status` + `git diff` + `git log --oneline -10`。
   - 确认改动范围与用户意图一致；无关文件（尤其 `data/`、`config.json`、日志）不得混入。
   - 敏感/运行时目录已在 `.gitignore`（AGENTS §6 清单），若发现暂存区有它们立即移除并向用户报告。
2. **门禁**：跑 AGENTS §2 的全绿命令（`cargo test -p turtle-soup` +
   `cargo clippy -p turtle-soup --all-targets -- -D warnings`）。
   - 不绿不提交；修复后重新走本流程。
   - 纯文档改动可豁免门禁，但需在 commit message 中注明"纯文档"。
3. **暂存**：只 add 本次意图内的文件，逐个确认，不用 `git add -A` 盲加。
4. **message**：中文，第一行 = `类型: 做了什么`（类型沿用仓库历史：`feat / fix / docs / refactor`），
   空一行后列要点，说明"做了什么 + 为什么"。参考 `git log` 既有风格。
5. **提交**：`git commit`（不加 `--no-verify`）；失败或钩子拒绝则修复后**另起新 commit**，不 amend。
6. **收尾**：默认**不 push**、不 amend、不改 git config；push/建 MR 需用户明确说。

## 触发词

"commit"、"提交"、"提交代码"、"提交一下"、"帮我提交"。
