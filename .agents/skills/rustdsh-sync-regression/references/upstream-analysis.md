# 上游更新分析法（模式 A 详细步骤）

上游仓库：`E:\aiproject\deepseek-harness`（git）。rustdsh 是其 **web 前端（packages/client/*）+ 存储行为（storage/session）+ llm-deepseek 适配层** 的 Rust/GPUI 1:1 复刻。
> **当前同步点：dsh-v0.1.5-rc.2（fb2c4b9e69）**——2026-09-11 同步（rc.1+rc.2；发布点后 master 增量 30 提交全部 subprocess/desktop/ci 面外）。此前 b2e3b2a012（0.1.5-alpha.2，2026-09-09 同步）。
> 0.1.5-alpha.1 主面：**会话格式 v3**（system prompt 晋升 system/message 行 + request/header 去 system + PTC 改名 + canonical 信封）、composer 统计行改双图标 pill + 互斥统计对话框、SystemPromptRow（系统提示词折叠行）；Sidebar 工作区文件树/dockkit/textpreview/remotes 全链面外。
> **最近检查：2026-09-11（定时轮 #6，同步轮）**——上游发布 0.1.5-rc.1/rc.2（184 文件 +1293/-1144，存储零变更），面内两项实施（deepseek 模型目录 V41 Flash 首项默认 + Usage 对话框 cacheWrite 为 0 省行）。

## 1. 一键差异分析

```bash
.agents/skills/rustdsh-sync-regression/scripts/check-upstream.sh [from] [to]
```

`from` 取上次同步点（查项目记忆 `rustdsh-sync-*-scope` 或 git log 里的「同步 web x.x.x」提交）。脚本输出五段：发布树差异 / 提交清单 / 同步面 stat / 词汇表 diff / 存储行为 diff。

**首个关键判定——发布是否有实质变更**：
`git diff tagA tagB --name-only | grep -v package.json` 为空 = 两发布点之间零功能变更（只有版本号批量 bump）。此时还要看 **master HEAD 相对发布标签的增量**（功能可能在打标签后才合入 master，用户的工作区拉的是 HEAD）。

## 2. 同步面判定表

| 上游改动 | 判定 | 理由 |
|----------|------|------|
| packages/client/ui-*（chat/conversation/settings*/sidebar/primitives/theme/tool）| **面内** | UI 1:1 镜像 |
| packages/client/*/locale(s).ts 词汇 | **面内** | 词汇表逐项核对 |
| packages/storage、session-projection-cache 的**行为**变更 | **面内** | projcache/存储格式互通 |
| packages/llm/llm-deepseek（translate/types/error）| **面内** | dsh-llm-deepseek 镜像 |
| ui-settings-models 的 fetch/discovery | **面内**（已补齐，见 fetch-models 功能） | 获取可用模型 |
| packages/llm/llm-pi-ai discovery/catalog | **半面内** | discovery 语义移植；富目录（catalogModels）不移植 |
| session-persistence 内部重构（coordinator/handle/storage-contract）| 面外，**除非**动磁盘格式 | host 内部 seam；jsonl 格式稳定性用回归 A2/A3 兜底 |
| read_image 图片卡 / 新工具渲染 | 面外（除非 rustdsh 新增对应工具） | rustdsh 工具集无 read_image |
| proxy/http-proxy/app-boot/python 打包 | 面外 | Node 网络层，web 壳层无涉 |
| issue-management（.github）、CI、docs、notes | 面外 | 仓库自动化 |
| experimental/*（agent-team、cordis）| 面外 | rustdsh 无镜像 |
| message-edit 这类 feat→revert 对 | 净抵消，净零 | 与基准 diff 即可确认 |
| session 日志格式 v2（0.1.3-alpha.1：代数文件名 session.vN.jsonl、header isSeeded、chunk 内嵌 settlement.stream、assistant/attempt、tool/result message 形、compaction 富形） | **面内** | 磁盘格式互通；rustdsh 写 v2 + 读全代 + 写打开旧代先迁移（v2::migrate_to_v2） |
| session-persistence-jsonl lease（跨进程写锁） | 面外 | 读侧从不碰锁；rustdsh 单写者；仅需容忍会话目录里的 session.lock |
| file-upload 双端 RPC/Worker 传输面 | 面外 | rustdsh 本地存储一次完成，无传输；错误码/进度/断点无对应面 |
| 命令附件准入（CommandSubmitAttachment/registry） | 面外 | rustdsh 无命令系统 |
| QueueDock/Trajectory 文件计数/WorkspaceBrowser reveal/goal 命令芯片 | 面外 | rustdsh 无对应 UI 面 |
| token-meter（sourceEventSeqs → 内嵌 stream 重算）| 面外 | rustdsh 无 token-meter 镜像（TurnUsage 走实时 usage） |
| 链接语言（--dsw-alias-link + LinkIcon + hover 点状下划线）| 半面内 | 色值已同（deepseek-400/500）；LinkIcon 内联图标与逐 span hover 态 vendor TextView 不可复刻（偏差） |
| system-prompt 序位表（alpha.2：HARNESS_SOURCE 10000 / WEB_SURFACE 10100，persona 拆 prefix(0)/suffix(10200)）| **面内**（已对齐）| dsh-system-prompt SECTION_ORDERS 逐值镜像；rustdsh 无 persona 配置消费面，Config.persona→personaPrefix/Suffix 拆分不落码 |
| 模型切换公告（48cc1cf1d6：pre-step 比较上一 request/header，路由变化在消息批尾追加 user/plugin model-selection notice，notice form + summary；request/header reason=change 由既有 EpochHeader 比较自然成立）| **面内**（已实施）| dsh-agent-loop run_turn 消息批 + MessageSource::Plugin 扩 form/summary/sections（上游 plugin & ContextFormed）|
| MessageSource::Plugin 形扩展（plugin & ContextFormed：form instructions/catalog/snapshot(sections)/notice(summary)/relay/recall 可选字段）| **面内**（已实施）| serde default + skip None 向后兼容旧形；新形落盘与上游逐字节对齐 |
| open-in-app（9292dd8a2d feat workspace：web UI 经 URL scheme 唤起本地编辑器/终端/git 客户端 + 30 个 app.* 词汇 + ui-primitives Menu 依附改动）| 面外 | web→本地桥接功能；rustdsh 即本地应用，无对应面 |
| queue.sending 词汇（a3ccbd4d99 队列提交发送中态）| 面外 | QueueDock/排队系统 rustdsh 无对应面（既有判定）|
| chat 滚动 fix ×2（fa6bf62a98 settle pinned scroll before layout growth、9ef426d729 near-floor gestures pending）| 面外 | DOM 滚动/布局增长异步性专属；GPUI 列表滚动语义不同 |
| conversation-nodes perf（84c11c7243 avoid replaying settled assistant streams）+ llm stream readers（isTokenDelta/isVisibleChunk/assistantStreamFirstTokenTime 导出）| 面外 | web 重放优化；readers 无 rustdsh 消费面（session-stats/token-meter 无镜像）|
| resume 系列（6ad01cbd8d 重复 system prompt、477ff9d5e3 resumed series boundaries 等）| 面外 | rustdsh system 经 request/header 字段下发，无历史消息注入面，重复问题不存在 |
| connection 层（1bd26370cc handshake recovery、1e04ff35 retry factors）| 面外 | rustdsh 无 connection 层（本地直连）|
| subprocess native runner 批量（~80 提交：Windows Job/句柄/隔离/生命周期）| 面外 | host 进程管理内部实现，不动磁盘格式与工具可观察行为 |
| session 流式迁移（a84a8da9e1 stages、ec2f63dbdb publication、46196d6f95 v0→v2 streaming、migration-verifier Worker 校验）| 面外 | 内部性能重构，磁盘格式逐字不变（README 物理编码段仅加流式细节）；撕裂尾帧部分恢复语义 rustdsh 逐帧独立解压天然等价（未确认批次本就不可靠）|
| StateDot 第五态 idle/unloading 动画（452b2a816a + a6ef8dc97a）| 面外 | 消费面是 ui-settings-plugin-inventory 插件状态点，rustdsh 无插件库存页；state_dot 按调用方传色无枚举可扩 |
| Switch/Pill/Tag 胶囊原语重构（452b2a816a、c279350e03）| 面外 | rustdsh 无这三原语的镜像 |
| str_replace_editor 默认工具移除（36a4665144、965adbb5cf）| 面外 | rustdsh 工具集本就无 str_replace_editor |
| ui-agent-preset / ui-settings-plugin-inventory / ui-settings-plugins / SubagentModelSelectionCard / ui-trajectory / ui-skill | 面外 | rustdsh 无对应 UI 面（既有判定）|
| token-meter / session-stats / telemetry-otel / session-controller / api contract | 面外 | host 统计与遥测内部面，无镜像 |
| 会话格式 v3（0.1.5-alpha.1：header version 3 + agentPreset code→ptc + system prompt 晋升 `system/message` 行（head 保护/精确 replace/startSeq,endSeq 富形）+ request/header 去 system/空 tools/空 adapterDefaults + tool/code-dispatch→tool/ptc-dispatch + tools-code-mode→tools-ptc）| **面内**（已实施）| 磁盘互通；rustdsh 写 v3 + 读 v2/v3 + 写打开 v0/v1→v2→v3 级联迁移（v2.rs migrate_v2_to_v3：system promotion/PTC 改名/canonical/seq 重映射，id=v2-to-v3-system+sha256）；dsh-agent-loop 落盘序 step/start→system→user，head 归一化 replace（非 in-history 路线）|
| SystemPromptRow（0.1.5-alpha.1：system prompt 折叠行「系统提示词」/更新行「系统提示词更新」，展开体 141px 代码块）| **面内**（已实施）| chat.rs 渲染 system/message 为 Role::Context 折叠行；上游 surface replace 语义 rustdsh 线性显示每条（偏差）|
| composer 统计条改双图标 pill + 互斥统计对话框（3997f36999：gauge pill=轮步+TPS 开「会话统计」，database pill=总 token+缓存命中开「Token 用量」；stats.counts 去 ·；stats.dialog.* 词汇）| **面内**（已实施）| SessionStats 窗口累计（上游 deriveStats fallback fold 语义）；TTFT 会话平均；TPS 以 llm−ttft 近似 decode 窗口 |
| Sidebar 工作区文件树全链（workspace-files 双面 API/remotes/resources/file 资源订阅/dockkit 引擎/sidebar tab+文件树/textpreview 分页 tab/deliverables 产出文件/聊天点击文件改侧栏打开）| 面外 | rustdsh 无工作区文件浏览器面（既有判定先例）；聊天文件链接维持既有打开行为（7f0a613 守卫）|
| Send busy 态系列（9a5ed6fb60 跟随 busy-Enter、2f630626b8 纯文本草稿才显模式名、8935c3d725 上传 pending 保 Send）| 面外 | rustdsh 发送钮无 busy 文案形态 |
| 附件卡文件类型图标（0.1.5-alpha.2：4ee9e055d5 shared file type icons——FileCard 的 DocumentFileIcon → FileTypeIcon，扩展名/文件名分类 12+ 类，类色文件底 + 白 mark 双层 glyph）| **面内**（已实施）| 分类逻辑与色值照抄（传统类 + code 归并单类）；glyph 以 body/mark 双 svg 分层 tint 复刻（assets/filetype/），fold 与 body 同色（单 tint 白折角对比损失）、48 语言 CodeFileIcon 细分收敛通用 code glyph（资产不可得）|
| transcript 设置文案中文化（'Normal'→'标准'、'Compact'→'紧凑'）| **面内**（已实施）| rustdsh 原为「常规」，改「标准」对齐 |
| deepseek 模型目录（0.1.5-rc.1/rc.2：DEFAULT_MODELS 头部插 deepseek-flash/DeepSeek-V41-Flash（vision+in-history），目录净态 V41 Flash/V4 Flash/V4 Pro/V4 Flash Vision Exp；默认 Chat Completions → V41 Flash）| **面内**（已实施）| rustdsh 落点：设置页 deepseek 内置模型行 4 条 + 启动/AppSettings 默认模型 deepseek-chat→deepseek-flash；description 文案与 image 字段无显示面不落（CatalogModel 结构差异既有偏差）|
| Usage 对话框 cacheWrite 为 0 省行（3435dbd690 stats review 修正）| **面内**（已实施）| 统计对话框 Token 用量分支条件化 |
| category.service-stability 文案改「稳定性和速度」/ guide.description | 面外 | feedback 分类与 sidebar guide 面无镜像 |
| CodeBlock contentRef/display:contents、TurnTailNodeView actions margin-top 4px | 面外 | web DOM 缝（ref/滚动端口挂载）与 DOM 流间距补偿，vendor TextView 布局模型无对应结构 |
| sidebar guide 起始页/documentpreview 精修/preview scrollports | 面外 | Sidebar 全链面外（既有判定）|
| Mermaid/Graphviz/SVG/HTML 围栏预览（ee35e40bc8 等 feat 系列 → 58956c1a8a Revert）| 面外净零 | feat→revert 对，以 HEAD 净态为准 |
| Sidebar 图片文件预览（8870a13aa9）/ ui-deliverables 交付文件呈现（presented.* 词汇大批）/ workspace-files subagent roots / webworker file handle | 面外 | Sidebar 文件树与 deliverables 无对应面（既有判定）；api/webworker 层无镜像 |
| think 摘要去粗体（b08310ac1d）/ markdown 图片失败显原文本（88ab8e9133）| 面外 | rustdsh think 行固定标题无摘要文本面；vendor TextView 无图片加载 |
| session-controller 拒空 prompt（ae19d9383b）/ api file 边界修复系列 | 面外 | rustdsh 无 session-controller RPC 层 |
| open-in-app SSH 检测共享 / visualizer feat+revert 净零 / native node-addon-system（flock/Landlock）/ agent-instructions root marker 修复 | 面外 | 无对应面/净零/Node 原生层 |

## 3. 实施顺序

1. 词汇表先行：新增 key 落到 rustdsh 对应界面文案；删除的 key 检查 rustdsh 是否残留。
2. UI 数值变更：照抄上游 `ui-theme` 包 / `*.module.css` 的数值（px/色阶/圆角/字号行高）。
3. 存储行为变更：先读上游 spec/测试（`*/tests/*.spec.ts`）确认语义，再改 dsh-persist，配等价 Rust 单测。
4. llm-deepseek 变更：对照 translate.ts 逐语义核对 adapter.rs（注意上游可能先引入又撤销，**以 HEAD 净态为准**）。
5. 每完成一块即 `cargo test --workspace`；全部完成后跑完整回归（模式 B）。
6. 提交信息格式参照项目历史：`同步 web <版本>：<一句话主旨>（<存储/UI 细目>）；<偏差与原因>；回归：<测数> + 实机验证点`。

## 4. 已知固化的偏差（勿重复推导）

- 无 pi-ai 富目录短路（PROVIDER_CATALOG 只有单默认模型）→ 目录型提供方改走端点探测。
- 获取可用模型：编辑卡 askable 恒真；空 base 报上游完整 `pi-ai ships no catalog...` 文案；UA `dsh-rust/*`；60s 总超时；禁用态无 tooltip。
- read_image 图片卡无渲染面（rustdsh 工具集无 read_image）。
- serde_json 开 `preserve_order`（端点顺序 = JS Object.entries）。
- composer 图片捕获不复刻（0.1.3 之前既有的面，维持）：无粘贴/拖放图片、无图片规范化流水线；聊天里的图片块仅展示 web 写入日志中的（64px tile，字节反查 attachments/v1/objects）。
- 附件（0.1.3-alpha.1 同步）：本地存储瞬时完成，无上传进度/取消/断点（FILE_NOT_STAGED 等 RPC 错误码无对应面）；外部文件拖放不复刻，回形针走文件对话框；移除钮常显（web hover 显形）。
- v2 写形：迁移对 rustdsh 旧形合成 `legacy-message:{sid}:{seq}` 消息 id 与 `{kind:"model",provider:"legacy",model:"legacy"}` source（上游迁移只认自家旧形）；compaction legacy `{beforeSeq,summary}` → 富形时 shadowedTokenCount=0、provider/model="legacy"。
- 链接/内联码样式：TextView 无逐 span hover 态 → 链接保持常显下划线（上游默认无下划线 + hover 点状）；LinkIcon 前置图标不可复刻（文本流无内联图标）；inline code 0.5px 描边不可复刻，底色已对齐 neutral-50/neutral-800。
- turn-metrics contract 本区间净零（仅 import 换源）。
- open-in-app（0.1.3-alpha.2）：web→本地应用 URL-scheme 桥接，rustdsh 本体即本地应用，无对应面；30 个 app.* 词汇随之不落。
- MessageSource::Plugin 旧形消费（Message::system 等仅 plugin 字段）不变；新形 form/summary/sections 仅在 producer 显式供给时落盘（当前唯 model-selection notice）。
- 撕裂尾帧：上游部分解码恢复完整记录+写侧截断重写；rustdsh 逐帧独立解压，损坏尾帧整帧不返回——两者在"未确认 durable 批次不交付"语义上等价，边缘崩溃场景不另行实施。
- StateDot idle/unloading 五态目录：消费面（插件库存页）rustdsh 无镜像；state_dot 色值由调用方供给，不扩枚举。
- 模型切换公告显示形态：上游在转录里渲染为折叠摘要行（notice summary 骑行）；rustdsh 无 ContextInjectionRow 折叠行组件，公告按既有用户消息渲染面显示（互通面已 1:1，视觉形态偏差）。
- dsh-shell 端到端 cmd 编码测试（cmd_non_utf8_stderr_is_readable）弱化为编码无关断言：cmd 子进程输出字节随父链 console 输出代码页漂移且偶发尾字节截断（65001/936 间歇），强语义由 GBK 纯字节解码单测固定覆盖——2026-09-08 定时轮 #3 发现并修复（曾致 cargo test 中止后续 suite、总数波动 80/88/93）。

- V3 system prompt 面：上游 in-history 路线（provider 适配器声明能力）变化时 append 新节点；rustdsh 路线走请求 system 参数（非 in-history）→ 归一化 replace head。读取多节点 v3 日志（in-history 写形）时 rustdsh 线性显示每条 system/message，不做 surface 替换折叠。
- 统计 pill：IconGaugeOutline16/IconDatabaseOutline16 不可得（gpui-component-assets 86 枚无 gauge/database），近似用 LayoutDashboard/ChartPie；TPS 的 decode 窗口以 llm−ttft 近似（rustdsh 无独立解码计时）；对话框无点外关闭（strip 在文档流，无法全窗捕获；pill 再点/互斥切换关闭）+ 面板固定于 pill 行上方居中（上游逐 pill 锚定 + viewport clamp）。
