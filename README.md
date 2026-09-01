# dsh-rust

[deepseek-harness](https://github.com/deepseek-ai/deepseek-harness) 的 1:1 Rust 复刻（骨架阶段）。

用 **Rust（edition 2024）+ GPUI 0.2.2 + gpui-component 0.5.1** 把 harness 的 Web 体验做成原生桌面应用，agent 层采用 **Native 手写 LLM adapter**（不用 rig-core —— 其 deepseek `reasoning_content` 在带 tool-call 的多轮循环里会丢失，见 [rig #1434](https://github.com/0xPlaygrounds/rig/issues/1434) / [#1440](https://github.com/0xPlaygrounds/rig/issues/1440)）。

## 架构

沿用参考仓库 `packages/core/*` + `packages/llm/*` 的语义，逐 crate 对齐：

| crate | 对齐参考 | 内容 |
|---|---|---|
| `dsh-llm` | `packages/llm/llm` | `ContentBlock` / `Message` / `StreamChunk` / `FinishReason` / `TokenUsage` / `LlmFailure` / `LlmAdapter` / `BlockAssembler` / `LlmRuntime` + `llm/stream` waterfall |
| `dsh-llm-deepseek` | `packages/llm/llm-deepseek` | DeepSeek HTTP adapter（reqwest + 手写 SSE；`reasoning_content` 一等 block、tool args 原始 JSON、缓存 token 回减） |
| `dsh-fs` | `packages/fs` | `fs` 工具（read/write/list/exists）+ `FsPolicy` 缝（`AllowAllPolicy` / `WorkspaceContainment` 写沙箱） |
| `dsh-shell` | `packages/shell` | `shell` 工具（`cmd /C` / `sh -c`，前台超时默认 120s + 每调用覆盖上限 600s，超时 kill） |
| `dsh-web` | `packages/web` | `web_fetch`（GET → 文本截断）+ `web_search`（DeepSeek anthropic /messages + `web_search_20250305` 服务端工具） |
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

11. **glob 工具 + 搜索卡 paths 形态** — `--files --glob --sort=modified --no-ignore --hidden` 语义（含隐藏/被忽略文件、剔除 VCS 目录、只返回文件、修改时间升序、100 条上限 + 截断脚注）；搜索卡双形态（matches 分组 / paths 扁平），`tool:glob` section；
12. **上下文压缩** — `dsh-compaction`（压缩指令/检查点帧形逐字对齐 web；chars/4 估算；工具配对安全边界）；`SessionEvent::Compaction` + 带序 derive（旧检查点被新压缩替换、检查点插在保留消息前）；agent-loop 轮次起点挂钩（默认 60k tokens/保留 6 条，0 禁用）；persist 双向映射（step/end 补落盘保 seq 对齐）；UI「上下文已压缩」分隔条；
13. **subagent 工具** — `dsh-subagent`：全新会话驱动子 ReactLoopAgent（同路由/工具/prompt），前台等待末条助手消息作为结果；深度护栏 2 层 + 300s 超时取消；路由随宿主切换同步；
14. **fs 策略/沙箱 provider** — `FsPolicy` 缝：`AllowAllPolicy` 直通 / `WorkspaceContainment` 写限定工作区根下（`~` 展开 + 尽力规范化，目标不存在回退最近存在祖先）；工作区创建/切换/删除四处同步写根；
15. **web_search 工具** — DeepSeek anthropic 兼容 `/messages` + `web_search_20250305`（端点/头/请求体逐字对齐，结果块缺失即错误）；queries 1-5 合并去重、20 条上限；输出逐字对齐 `formatSearchOutput`；无 key 不注册；
16. **shell 超时** — 前台默认 120s、每调用 `timeout_ms` 覆盖（上限 600s clamp），tokio 进程 + 三路并发读 + 超时 kill（`kill_on_drop` 兜底）；PTY 会话与参考实现一致推迟。

17. **轮次过程折叠**（同步 web 0.1.2-alpha.1 的 8b09a0be52）：已完成轮次在 compact 视图（默认）下，把最终答案之前的 Think/早前回复/工具行折叠为单一控制行（计数省零、全零「已思考」、subagent 单列），点击展开整组；答案条目折叠时隐藏本步 reasoning；打开的轮次永不折叠；设置「对话视图」= `ui-chat.transcriptView`。

18. **同步 web 0.1.2-alpha.2**（dsh-v0.1.2-alpha.1 → dsh-v0.1.2-alpha.2，2026-08-31 分析落地）：
    - **轮次尾部双药丸**（bb1df10c69 + 9effa0c6b3 + 6f16d5868c + 21d039be1b）：助手消息 footer 塌缩为「用量」药丸（database 图标 + 紧凑总数 tok）与「用时」药丸（clock 图标 + 总用时），点击各自弹出详情对话框——用量（提供方/模型、缓存命中率、未缓存输入/缓存读取/缓存写入/输出含推理、标题行精确总数）与用时（总用时、TPS、TTFT；文案「首 token 平均用时」对齐 label 修正）；日历时钟文本缀在药丸后（同日 `HH:mm` / 今年 `M月D日 HH:mm` / 跨年 `Y年M月D日 HH:mm`）；无用量数据的轮次保持纯文本用时行。TTFT 取本步首 token 延迟、TPS 按轮 LLM 时间（轮用时 − 工具时间）折算。新增 lucide `clock.svg`/`database.svg` 补齐图标集。
    - **session-projection 注册表**（新 crate `dsh-session-projection`）：`{key, stateVersion, init, apply}` 声明式投影单元，按 key 注册计数、按会话增量折叠（基线缓存）。agent-loop 注册 `turnBoundary`（openTurnStartSeq/lastStepStartSeq/lastStepBoundary/lastTurn，stateVersion 2，注册柄随 agent 存活），`ReactLoopAgent` 的 lastTurn 改从投影读取，不再 `findLast` 扫日志。
    - **llm-retry 改为事件 + 投影**：`SessionEvent` 新增 `LlmRetry`/`LlmRetryStarted`（web `llm/retry`/`llm/retry-started` 逐字形状：normal 带 maxRetries、always 不带；retryId 链内稳定）；`attach_retry(events, projections, agent_slot)` 重试计数从 `llmRetry` 投影读取（按 `[provider, policyKey]` 分桶，`step/start` 与 `turn/end` 清零），provider `retry-after` 优先于退避、封顶 `max_delay_ms`；重试历史持久进会话日志，崩溃/恢复不丢不重放。
    - **system-prompt 顺序集中分配**：`PromptSection` 增加 `order`，渲染按 `(order, name)` 升序；`SECTION_ORDERS`（上游全表 -1000…9900）与 `CONTEXT_ORDERS`（SANDBOX_POLICY 110/APPROVAL_POLICY 115/SUBAGENT_DELEGATION 120）私有化，消费方经 `get_section_order`/`get_context_order` 服务 API 查询（remove cross-package runtime relays 同语义）。
    - **ignorable 事件契约**：persist 读取路径三分支——词汇表内未映射类型（web 宿主事件）按已知忽略；词汇表外且信封带 `ignorable: true` 的纯信息记录安全跳过；词汇表外且必读的未知类型**拒绝解释整份日志**（fail-closed，多半是更新版本 harness 所写）。`KNOWN_SESSION_EVENT_TYPES` 对齐上游 alpha.2 全集。
    - **web_search 失败指引**（web-search-deepseek provider）：dispatch 之后的失败统一带 endpoint 与恢复指引（Settings > Plugins > Web search / `DEEPSEEK_SEARCH_BASE_URL`，"只有用户本人应选择或更改端点"），HTTP 错误消息改为「基础状态 + detail 追加」形状。
    - 上游同版其余变更经评估不适用或无对应物：vendor cordis 仅版本号；`brandString` 运行时品牌（Rust newtype 本就名义类型）；settings `installSection` 注入式重构（无 settings 服务）；`RemoteError` 词汇收敛（无 typert remote 面）；连接恢复指示器（原生直连无网关 websocket）；插件清单/agent-preset/权限预设 UI（无插件与权限系统）；@ 菜单 stale-while-revalidate 与 CSS 修版。

20. **同步 web 0.1.2-alpha.3**（dsh-v0.1.2-alpha.2 → dsh-v0.1.2-alpha.3，2026-09-01 分析落地）：
    - **轮次导航栏**（web `session-turn-outline` 新包 + `TurnNavigator` 特性族）：新投影单元 `turnOutline`（stateVersion 2）——`turn/start` 边界锚定条目（其 seq 即跳转落点）、首条 User 源 `user/message` 填 prompt 预览（预算 50 字符/一行）、最新带文本 `assistant/message` 进草稿、`turn/end` 提交 response 预览（120 字符/三行）、不推进轮次号的边界被顺序守卫忽略；预览规则（空格拼接、空白折叠、截断加省略号）与导航卡钳一致，轮次在加载前后显示同样文字。UI 右缘 28px 框架内固定行距（10px）标记梯：溢出在框内滚动（最高 420px，无滚动条，可滚端 24px 渐隐）、激活 tick 20px 蓝 / hover 18px 灰 / 常态 12px 边框色、hover 预览卡（300px、prompt 13/20 wt500 一行截断 + response 12/18 caption 三行夹持、对中标记并夹持框内）、点击已加载标记滚动行顶至视口顶下 24px、激活轮按读位条目跟随（贴底取最新、指针在栏内暂停跟随）、条目 <2 或中栏 <900px 整条缺席。outline-only 标记（无可见条目的轮次）降级为半透明不可点。
    - **session-projection wire 面 + 身份门控变更流**：`ProjectionDefinition` 增加可选 `view`（wire 视图：状态 → 客户端载荷，无 wire 面的单元变更流永不发布）；registry 增加 `view_of`（raw 结果双槽缓存，与上次发布相同则返回同一 Arc——上游「view 复用引用压制 Object.is 发布」的 pull 对应物）与 `on_changed`/`drive`（逐事件推进、raw view 变化才发布；一轮「边界+prompt+回复」恰好三次推送，draft-only 安静）。
    - **流式未闭合围栏渲染为代码卡**：`split_markdown` 不再把未闭合围栏回退为正文文本——CommonMark 语义下未闭合围栏延伸到文档末尾，web 流式期间同样把它作为 code 块增量渲染（alpha.3 openFence：已完成行冻结、只重析最后一行 + 当前行；d8e2ac5052「保留已完成行」），闭合前后同一张卡。
    - **上游同版其余变更经评估不落地**（rustdsh 无对应载体）：`loadThrough` 深历史分页与跳转装载（桌面端全量加载日志，所有轮次恒为 loaded 锚）；QueueDock 排队图片缩略图/回显 placement（无队列停靠条与图片附件）；connection 心跳容忍停滞宿主（无 WebSocket 网关，直连 HTTP 已有读超时）；subagent steer/follow-up 图片投递与 `subagent/attachment-invalid`（subagent 工具只收 prompt 文本）；`read_image` 无扩展名附件路径嗅探（尚无该工具）；Schedule 目录视口对齐 / Tab 补全 / 空白会话视图激活（无对应 UI 面）；`admitPromptContent` 附件准入（无附件服务）；SQLite 持久化后端移除（本就未实现）；组件级 perf（工具体延迟格式化、视口外高亮延迟——本仓代码块本就无 shiki 级高亮）。

**下一步**（可选细化）：subagent 后台运行/持久化子会话（web 的 continuation 服务）；glob 超上限的顶层轮询采样（web sampleAcrossTopLevel）；压缩的 TokenMeter 精确计价与 compaction-tool-result-pruner；PTY 会话（参考实现同样推迟）；搜索卡 paths 形态的 UI 与 glob 输出已落地，结构化元数据通道（web presentationMeta）待 dsh-tools 增设 meta 缝后切换。

## 备注

**会话单写者约束**：与 web 共享 `~/.dsh` 的会话文件遵循参考实现的"一个会话一个宿主"模型——同一个会话不要同时在桌面端与 web 端继续对话（两宿主并发追加同一日志在双方实现里都不安全）。桌面端现在把写入锚定到会话自身的桶（按日志头 cwd 规范解析，漂移的 cwd 口径不再产生跨桶副本；已存在的日志拒绝被 header 覆盖、解不开的内容拒绝静默重建）。

沙箱内 cargo 依赖拉取需 `danger-full-access`（Windows schannel 在降权上下文无法取 TLS 凭据）；已抓取的依赖之后可离线构建。