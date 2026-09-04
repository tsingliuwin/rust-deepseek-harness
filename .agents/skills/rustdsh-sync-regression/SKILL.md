---
name: rustdsh-sync-regression
description: rustdsh（deepseek-harness 的 Rust/GPUI 1:1 复刻）的上游同步与完整回归闭环。当官方 deepseek-harness 发布新版本或用户要求分析上游更新、同步上游变更、1:1 复刻新功能时用「上游同步」模式；当用户要求跑完整测试、回归全部功能点、发布前验证、排查 UI/互通问题时用「完整回归」模式。即使用户只说「上游更新了」「跑一遍测试」也应触发。
---

# rustdsh 上游同步 × 完整回归闭环

rustdsh（E:\rustproject\rustdsh）是 deepseek-harness（E:\aiproject\deepseek-harness）web 前端
+ 存储行为 + llm-deepseek 适配层的 Rust/GPUI 1:1 复刻。本 skill 是两个模式的操作手册：

- **模式 A「上游同步」**：官方更新 → 差异分析 → 面内/面外判定 → 1:1 修改 → 回归 → 提交。
- **模式 B「完整回归」**：随时把 A 层核心链路 + B-H 层全部功能点过一遍，定位并修复问题。

两模式共用同一套资产：`scripts/`（可执行脚本）、`references/`（细化清单）。**先读对应
reference 再动手**；同步点与既记录偏差查项目记忆（rustdsh-sync-*-scope、
rustdsh-fetch-models-feature）。

## 模式 A：上游同步

1. **差异分析**：跑 `scripts/check-upstream.sh [上次同步点] [目标]`（默认 上次同步点 → HEAD）。
   判读五段输出，**先判定发布树是否零功能变更**（只见 package.json = 版本号 bump），
   再看 master HEAD 相对发布标签的增量。详细判定表与实施顺序：
   读 `references/upstream-analysis.md`。
2. **同步面判定**：按判定表把每 组变更归为 面内（必须同步）/ 面外（记录原因，勿动手）。
   feat→revert 对先算净态（以 HEAD 为准），别同步一个已被撤销的功能。
3. **实施**：词汇表 → UI 数值 → 存储行为 → llm-deepseek，每块完成即 `cargo test --workspace`。
   存储变更必须先读上游对应 `*.spec.ts` 确认语义。
4. **回归**：跑模式 B 的 A 层（`scripts/regression-core.sh`）；UI 相关变更加跑受影响的
   B-H 功能点（实机验证套路见 `references/ui-automation.md`）。
5. **提交**：信息格式照项目历史（同步 web <版本>：<主旨>（细目）；<偏差与原因>；
   回归：<测数> + 实机验证点）。同步点推进后**更新项目记忆**（rustdsh-sync-*-scope）。

## 模式 B：完整回归

1. **A 层核心链路（每次必跑，零 UI）**：
   `bash .agents/skills/rustdsh-sync-regression/scripts/regression-core.sh`
   —— workspace 单测 + 真实 ~/.dsh 语料加载 + projcache 标题互通。失败先修这里。
2. **B-H 层功能点遍历**：打开 `references/feature-checklist.md`，按清单逐项走。
   UI 项的实机操作套路（静默桌面/坐标换算/点击输入/截图判读）与已知坑：
   读 `references/ui-automation.md`。UI 验证前先 `bash scripts/restart-host.sh`
   确保宿主是最新构建。
3. **排障**：单测断言 → examples 解 zstd 会话日志 → UI 截图对照上游源码数值
   （`packages/client/ui-*/src` 与 `*.module.css`）。排障源清单在 checklist 尾部。
4. **修复后**：只重跑失败项所在层 + 受影响面；修完跑一次完整 A 层确认无回归。

## 效率约定

- A 层脚本任何时候都可以直接跑（约 1-2 分钟），UI 层才需要静默桌面等前置。
- 上游分析只会改 `.agents/`、`crates/`、`Cargo.toml`；看到预期外的文件变动先停下核对。
- 每次模式 A 完成后：更新 `references/upstream-analysis.md` 的「已固化偏差」表
  （新偏差追加，消失的删除）与项目记忆，下次同步零重推。
