# dsh-rust

[deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) 的 1:1 Rust 复刻（骨架阶段）。

用 **Rust（edition 2024）+ GPUI 0.2.2 + gpui-component 0.5.1** 把 harness 的 Web 体验做成原生桌面应用，agent 层采用 **Native 手写 LLM adapter**（不用 rig-core —— 其 deepseek `reasoning_content` 在带 tool-call 的多轮循环里会丢失，见 [rig #1434](https://github.com/0xPlaygrounds/rig/issues/1434) / [#1440](https://github.com/0xPlaygrounds/rig/issues/1440)）。

## 架构

沿用参考仓库 `packages/core/*` + `packages/llm/*` 的语义，逐 crate 对齐：

| crate | 对齐参考 | 内容 |
|---|---|---|
| `dsh-llm` | `packages/llm/llm` | `ContentBlock` / `Message` / `StreamChunk` / `FinishReason` / `TokenUsage` / `LlmFailure` / `LlmAdapter` / `BlockAssembler` / `LlmRuntime` + `llm/stream` waterfall |
| `dsh-llm-deepseek` | `packages/llm/llm-deepseek` | DeepSeek HTTP adapter（reqwest + 手写 SSE；`reasoning_content` 一等 block、tool args 原始 JSON、缓存 token 回减） |
| `dsh-fs` | `packages/fs` | `fs` 工具（read/write/list/exists） |
| `dsh-shell` | `packages/shell` | `shell` 工具（`cmd /C` / `sh -c`，spawn_blocking） |
| `dsh-web` | `packages/web` | `web_fetch` 工具（reqwest GET → 文本，截断） |
| `dsh-search` | `packages/fs/tool-fs-search` | `grep` 工具（进程内 ripgrep：`ignore` walk + `regex` + `globset`，输出格式逐字对齐 `formatGrepOutput`） |
| `dsh-session` | `packages/core/session` | append-only `SessionEvent` 日志 + `derive_messages()` + `request/header` epoch |
| `dsh-tools` | `packages/core/tools` | `Tool`(JSON-schema + async execute) + `ToolRegistry` |
| `dsh-system-prompt` | `packages/core/system-prompt` | prompt section 装配 + tool schema 组装 |
| `dsh-agent` | `packages/core/agent` | `Inbox` + `AgentStatus` + `agent/*` 事件词汇表（pre-step / request / request-error / turn-stopping…） |
| `dsh-agent-loop` | `packages/core/agent-loop` | `ReactLoopAgent` turn/step 状态机，扩展点全部经 `EventBus` 派发；`attach_retry` 退避重试 |
| `dsh-cordis` | Cordis | `Context`（服务仓库 + 可逆 `Effect`）+ 类型化 `EventBus`（emit / waterfall / serial）+ `Fiber`/`Scope`/`Plugin`/`PluginManager`（inject 依赖排序 + 可补丁 config） |
| `dsh-gpui` | `apps/web` | GPUI 原生聊天壳（文本输入框 + 流式渲染） |

## 运行

```sh
# 桌面聊天壳（设了 DEEPSEEK_API_KEY 走真模型，否则走内置 mock 流式）
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
```

环境变量：`DEEPSEEK_API_KEY`、`DEEPSEEK_BASE_URL`、`DSH_MODEL`（默认 `deepseek-chat`）、`DSH_PROMPT`（默认演示问题）。

## 状态

**已完成**：
1. **Spine（端到端流式跑通）** — LLM 词汇表/adapter seam、DeepSeek adapter、session 日志与 derive、工具注册、agent loop（含多步工具循环）、GPUI 聊天壳 + tokio/flume 桥接；
2. **Cordis 事件/waterfall 层** — 类型化 `EventBus`（emit / waterfall / serial），已接入 `llm/stream` 短路、`agent/pre-step`（reject/改写）、`agent/request`（替换 config）、`agent/request-error`（retry）、`agent/turn-stopping`（serial），以及 `agent/status` / `agent/error` / `agent/inbox/*` emit；
3. **UI 1:1 对齐 web 版** — `theme.rs` 落地参考 `packages/client/ui-theme` 暗色全量 token（bg-base #151517、sidebar #1B1B1C、表面 #2C2C2E、品牌蓝 #679EFE、白 6%/12% 边框、`--dsw-font-*` 字号标尺），并注入 gpui-component 全局 Theme（含 dark 代码高亮）；三栏 shell（列宽契约照抄 `ui-layout/columns.ts`）；侧栏（品牌行/新建会话 38px r12/工作区/32px 会话行/设置）；会话区 header（标题 + 标准模式 chip + 对话/轨迹 tab，激活 2px 蓝条）；消息流（748px 内容列、用户气泡 r22 右对齐、Think/工具折叠行 14/24 + 2×2 分隔点、markdown 正文 16/28、用时 footer、复制按钮、`Deep diving…` 流式状态行）；右侧详情面板（工具行点击联动 输入/输出 卡）；底部 composer（780px r22 输入卡、多行 auto-grow、+ / Workspace Write / 模型 / 34px 蓝色圆形发送·停止按钮、轮次统计行）；
4. **图标资源** — gpui-component 的 crates.io 包不含 SVG，已从 v0.5.1 源码取 `assets/icons`（86 个）嵌入 `dsh-gpui/assets`，经 `src/assets.rs` 的 `AssetSource` 注册（`Application::with_assets`）；
5. **退避重试** — `ResolvedRetryPolicy` 判定/指数退避、`LlmRuntime.provider_retry_policy`、`attach_retry`（attempt 计数 + turn 清理）；
6. **fiber/inject 注入 + 可补丁 config** — `Scope`/`Fiber`/`Plugin`/`PluginManager`，`inject` 依赖排序挂载，`Patch` 按 id 替换/插入/删除，dispose 逆序回收；
7. **fs/shell/web 工具包 + 会话持久化** — `dsh-fs`/`dsh-shell`/`dsh-web` 注册进 `ToolRegistry`；`dsh-persist` JSONL 落盘，启动恢复最近会话，会话列表可切换/新建。
8. **ToolRow 收尾 + hero 光晕** — 工具行折叠模型对齐 web `toolRowModel`（标题按工具/op 定名、摘要取 command/path/url、失败行摘要替换为输出首行）；fs read|write 的 path 下划线链接（剥工作区根 + `~` 缩写，点击宿主应用打开）；Inspect pill（hover 显现，点击跳轨迹 tab 并 scroll_to_item 定位）；Think/工具行运行扫光恢复；hero 空态蓝色光晕（figma 313:14109 椭圆高斯模糊预渲染资产，宽随卡缩放、中心锚卡面）；代码块 banner、亮色主题（设置外观分段 + 跟随系统）此前已落地；
9. **工具专属展开卡** — shell → 终端卡（web TerminalBlock：cwd prompt banner + 30px gutter 状态点 + 输出 224px 内滚动，running 只画 banner）；fs read → 读取卡（web ReadBlock：banner 底 + 48px 行号 gutter）；fs write → 差异卡（web DiffBlock：全 + 行、footer `└ +N -0 · 1 个文件`）；web_fetch → 获取卡（URL 链接 open_url + 截断注记）；read/diff/search 8 行折叠（`… 其余 N 行`/收起）；错误行回退通用 IO 卡；IO 卡输入 pretty JSON（web deriveBody）；
10. **grep 工具 + 搜索卡** — `dsh-search`（对齐 `tool-fs-search/grep.ts`）：进程内 ripgrep（`ignore` walk 尊重 .gitignore/跳隐藏 + `globset` include 过滤 + `regex` 匹配），250 条上限，输出逐字对齐 web `formatGrepOutput`（`Found N matches` / `Found K of N matches` / `No matches found` + 按文件分组 `Line N: text`）；UI 搜索卡（web SearchBlock matches 形态：摘要头「N 处匹配 · M 个文件」+ 复制、文件头 600 weight 可点击折叠组、行号 tertiary 前缀、8 行头 4 尾 4 + 尾片组头补还）；`tool:grep` prompt section；折叠行模型「搜索」+ pattern 摘要。

**下一步**（其余能力）：compaction / subagent；fs 策略与沙箱 provider、web 搜索 provider、shell 超时与 PTY 等能力细化；glob 工具（搜索卡 paths 形态）。

## 备注

沙箱内 cargo 依赖拉取需 `danger-full-access`（Windows schannel 在降权上下文无法取 TLS 凭据）；已抓取的依赖之后可离线构建。