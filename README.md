# rustdsh

[deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) Web 端的 **Rust / GPUI 1:1 原生复刻** —— 把 DeepSeek Harness 的 agent 体验做成 Windows 桌面应用，存储层与 Web 版共享 `~/.dsh`：会话日志（zstd JSONL v3）、附件内容寻址存储、项目缓存（projcache）双向互通。

技术栈：**Rust（edition 2024）+ GPUI 0.2.2 + gpui-component 0.5.1**（vendored 微调，见[备注](#备注)）。agent 层为手写 LLM adapter —— 不用 rig-core，其 deepseek `reasoning_content` 在带 tool-call 的多轮循环里会丢失（[rig #1434](https://github.com/0xPlaygrounds/rig/issues/1434) / [#1440](https://github.com/0xPlaygrounds/rig/issues/1440)）。

## 特性

- **1:1 界面**（数值/文案逐项照抄 `ui-theme` 包与 `*.module.css`）：三栏 shell、会话 header（对话/轨迹 tab）、流式 markdown、Think/工具折叠行（运行扫光）、工具专属展开卡（终端 / 文件读取 / 差异 / 网页获取 / 搜索 / 8 行折叠）、轮次过程折叠 + 右缘轮次导航栏、轮尾「用量 / 用时」双药丸与详情对话框、composer 统计双 pill + 互斥统计对话框（会话统计 / Token 用量）、轮次切换公告、系统提示词折叠行、hero 空态光晕；亮/暗主题 + 跟随系统。
- **agent 能力**：turn/step 状态机、多步工具调用（`fs` / `shell` / `web_fetch` / `web_search` / `grep` / `glob` / `subagent`…）、上下文压缩（默认 60k tokens 阈值、检查点帧形逐字对齐）、subagent 子会话驱动（深度护栏 2 层 + 300s 超时）、指数退避重试（`llm/retry` 事件持久化，崩溃恢复不丢）、模型切换公告（user/plugin notice 进模型历史）、可插拔 fs 沙箱（`WorkspaceContainment` 写限定工作区根）。
- **存储互通**：会话日志 v3 读写（Web 写 v2/v0 自动级联迁移，源文件字节永不重写）、fail-closed 未知事件门（更新版本 harness 的必读事件拒绝解释而非静默跳过）、附件 sha256 内容寻址 + 硬链接别名、projcache 双布局（per-record 树 / 旧整档）标题互通。
- **每日自动同步上游**：凌晨定时任务拉取官方仓库 → 五段差异分析（发布树 / 提交清单 / 同步面 stat / 词汇表 / 存储行为）→ 面内 1:1 复刻 / 面外逐组判定固化 → 全量回归 + 实机验证（skill 与脚本见 `.agents/skills/rustdsh-sync-regression`）。

## 架构

沿用参考仓库 `packages/core/*` + `packages/llm/*` + `packages/session/*` 的语义，逐 crate 对齐：

| crate | 对齐参考 | 内容 |
|---|---|---|
| `cordis` | Cordis | `Context`（服务仓库 + 可逆 `Effect`）+ 类型化 `EventBus`（emit / waterfall / serial）+ `Fiber`/`Scope`/`Plugin`/`PluginManager`（inject 依赖排序 + 可补丁 config） |
| `dsh-llm` | `packages/llm/llm` | `ContentBlock` / `Message` / `StreamChunk` / `TokenUsage` / `LlmFailure` / `LlmAdapter` / `BlockAssembler` / `LlmRuntime` + `llm/stream` waterfall + assistant 流累加器（v2/v3 内嵌 settlement 形） |
| `dsh-llm-deepseek` | `packages/llm/llm-deepseek` | DeepSeek HTTP adapter（reqwest + 手写 SSE；`reasoning_content` 一等 block、tool args 原始 JSON、缓存 token 回减）+ 模型发现（OpenAI 兼容 / anthropic 双协议） |
| `dsh-fs` | `packages/fs` | `fs` 工具（read/write/list/exists）+ `FsPolicy` 缝（`AllowAllPolicy` / `WorkspaceContainment` 写沙箱） |
| `dsh-shell` | `packages/shell` | `shell` 工具（`cmd /C` / `sh -c`，前台超时默认 120s + 每调用覆盖上限 600s，超时 kill） |
| `dsh-web` | `packages/web` | `web_fetch`（GET → 文本截断）+ `web_search`（DeepSeek anthropic `/messages` + `web_search_20250305` 服务端工具） |
| `dsh-search` | `packages/fs/tool-fs-search` | `grep` / `glob` 工具（进程内 ripgrep：`ignore` walk + `regex` + `globset`，输出格式逐字对齐 `formatGrepOutput`） |
| `dsh-session` | `packages/core/session` | append-only `SessionEvent` 日志 + `derive_messages()` + `request/header` epoch + `system/message` 面节点 |
| `dsh-session-projection` | `packages/session/session-projection` | 声明式投影单元（`{key, stateVersion, init, apply}` + wire 视图 + 变更流）：`turnBoundary` / `turnOutline` |
| `dsh-tools` | `packages/core/tools` | `Tool`（JSON-schema + async execute）+ `ToolRegistry` |
| `dsh-system-prompt` | `packages/core/system-prompt` | prompt section 装配（`SECTION_ORDERS` 集中序位）+ tool schema 组装 |
| `dsh-compaction` | web compaction 事务 | 上下文压缩事务（压缩指令 / 检查点帧形逐字对齐、chars/4 估算、工具配对安全边界） |
| `dsh-agent` | `packages/core/agent` | `Inbox` + `AgentStatus` + `agent/*` 事件词汇表（pre-step / request / request-error / turn-stopping…）+ 模型选择公告 |
| `dsh-agent-loop` | `packages/core/agent-loop` | `ReactLoopAgent` turn/step 状态机，扩展点全部经 `EventBus` 派发；`attach_retry` 退避重试 |
| `dsh-subagent` | web subagent 工具 | 全新会话驱动子 `ReactLoopAgent`（同路由/工具/prompt），前台等待末条助手消息作为结果 |
| `dsh-persist` | `packages/session/session-persistence*` + `packages/storage` | 会话日志 JSONL+zstd（v3 写 / v2/v3 读 / 级联迁移、`KNOWN` 事件门）、附件内容寻址存储、projcache 双布局互通 |
| `dsh-gpui` | `apps/web` + `packages/client/ui-*` | GPUI 原生桌面壳：三栏 shell、聊天流全渲染面、设置页（提供方/模型配置 + 获取可用模型 + 外观）、侧栏、轨迹视图 |

## 运行

```sh
# 桌面应用（设置页可配提供方/模型；有 DEEPSEEK_API_KEY 走真模型，否则内置 mock 流式）
cargo run -p dsh-gpui

# 无 key 的 headless 冒烟测试（mock adapter + calculator 工具，打印完整日志）
cargo run -p dsh-agent-loop --example mock_chat

# 事件/waterfall 拦截演示（agent/request、agent/pre-step、llm/stream 短路、emit/serial）
cargo run -p dsh-agent-loop --example intercept

# 退避重试（RATE_LIMIT 失败 → attach_retry 重试成功）
cargo run -p dsh-agent-loop --example retry

# fiber / inject 注入 + 可补丁 config
cargo run -p dsh-agent-loop --example fiber_demo

# 能力工具（fs 读文件回传）
cargo run -p dsh-agent-loop --example tools_demo

# 全量单测（workspace ~100 测）
cargo test --workspace

# A 层核心链路回归（单测 + 真实 ~/.dsh 语料加载 + projcache 互通，约 1-2 分钟）
bash .agents/skills/rustdsh-sync-regression/scripts/regression-core.sh
```

环境变量：`DEEPSEEK_API_KEY`、`DEEPSEEK_BASE_URL`、`DSH_MODEL`（默认 `deepseek-chat`）、`DSH_PROMPT`（默认演示问题）、`DEEPSEEK_SEARCH_BASE_URL`（web_search 端点，也可在设置页配置）。

## 与上游同步

rustdsh 跟随官方 deepseek-harness 的发布节奏逐版 1:1 对齐（当前同步点 **dsh-v0.1.5-alpha.1**，`5dda764ed3`）。每轮同步走固定闭环：`check-upstream.sh` 五段差异分析 → 按判定表归面内/面外 → 面内实施（UI 数值照抄 ui-theme、存储与词汇严格对齐、不可复刻项固化 `upstream-analysis.md` 偏差表）→ workspace 单测 + 真实语料回归 + 实机验证。操作手册见 `.agents/skills/rustdsh-sync-regression/`。

## 同步历史

| 上游版本 | 主面 |
|---|---|
| `0.1.5-alpha.1` | **会话格式 v3**（system prompt 晋升 `system/message` 行、request/header 去 system、PTC 词汇改名、canonical 信封）· composer 统计双 pill + 互斥对话框 · SystemPromptRow |
| `0.1.3-alpha.2` | system-prompt 序位重排 + persona 前缀/后缀拆分 · 模型切换公告 · `MessageSource::Plugin` 扩 form/summary/sections |
| `0.1.3-alpha.1` | **会话日志 v2**（代数文件名、流内嵌 settlement、消息全形）+ 通用文件附件（回形针 → 内容寻址存储 → 文件卡/图片 tile）+ 可点击链接语言 |
| `0.1.2-alpha.1…3` | 轮尾用量/用时双药丸 + 详情对话框 · session-projection 注册表 + 轮次导航栏 · llm-retry 事件化 · ignorable 事件契约 · 流式未闭合围栏代码卡 · 轮次过程折叠 |
| 初版 → alpha.0 | Spine 端到端流式、Cordis 事件层、UI 三栏 shell、全部工具链（fs/shell/web/grep/glob）、上下文压缩、subagent、fs 沙箱、会话持久化（全量清单见 git log） |

## 备注

**gpui-component vendor 补丁**：`[patch.crates-io]` 把 `gpui-component` 指向 `vendor/gpui-component`（0.5.1 原样拷贝 + `[dsh]` 注释标记的改动），因为 TextView 的 markdown 排版有多处与 web 契约不符且 `TextViewStyle` 不暴露：`strong` 700→**600**（web `.markdown strong`；Segoe UI 有真实 Semibold 面）、列表项间距 0→**6px**（web `li+li`）、marker `▪`→**`•` 且次级色**（web disc + `li::marker` label-secondary）、标题边距 pb 0.3rem→**h1-h3 32/16、h4-h6 16/16**（web `.markdown h*` margin）+ h2/h3 字重 600→700、新增 `heading_line_height`（30/28/26px）；另 hr 1px/上下 32px、blockquote 2px 左线 14px 内边距不压暗。工作区其余部分保持 registry 版本语义，升级 gpui-component 时需重新比对这份 diff。

**会话单写者约束**：与 web 共享 `~/.dsh` 的会话文件遵循参考实现的「一个会话一个宿主」模型——同一个会话不要同时在桌面端与 Web 端继续对话（两宿主并发追加同一日志在双方实现里都不安全）。桌面端把写入锚定到会话自身的桶（按日志头 cwd 规范解析，漂移的 cwd 口径不再产生跨桶副本；已存在的日志拒绝被 header 覆盖、解不开的内容拒绝静默重建）。

沙箱内 cargo 依赖拉取需 `danger-full-access`（Windows schannel 在降权上下文无法取 TLS 凭据）；已抓取的依赖之后可离线构建。
