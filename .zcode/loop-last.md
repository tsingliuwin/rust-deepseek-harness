# 用-查-改-用 复查记录

每次 `/loop` 后追加一节；下一次复查先比对此处，确认旧修复未复发。

## 2026-08-31 · 第 15 轮（两侧空白仍不滚——wrapper 转发）

- **残留死区**：列表 hitbox 只覆盖内容列（gpui list 自身 bounds），px_8 挪进 List 也不改变命中链——两侧空白在列表命中范围之外。
- **修复**：`chat-scroll` wrapper 挂 `on_scroll_wheel`：指针**不在**列表 viewport_bounds 内时，把滚轮 delta 经 `ListState::scroll_by` 转发给列表并 notify；在 viewport 内时不动作（列表自身已处理，避免双滚）。ListState 改为 clone 供两处持有。
- **验证**：54 测试全绿；18:06 二进制已部署。
- **下轮重点**：① 两侧空白滚轮生效；② 卡片 8 行折叠钮展开正常；③ 全部历史验收项（路由/流式/扫光/黑窗/回放）。

## 2026-08-31 · 第 14 轮（滚动死区彻底重构）

- **用户反馈**：条件化 occlude 后仍只有 Bash 行能滚；两侧空白也不滚。
- **两个独立死区**：
  1. **长输出卡仍 occlude**（条件化只救了短卡）→ 对齐 web 语义根治：**卡内不再内滚**，term-out / io_section（输入/输出）全部改为 `CARD_MAX_LINES` 8 行折叠 + `fold_toggle` 展开钮（复用 read/diff 卡模式），`overflow_y_scroll`/`max_h`/`scroll_occlude` 全部移除——卡片不再是滚动容器，滚轮穿透到页面；terminal_card/io_card 签名加 `expanded + on_toggle`，6 个调用点接 `tool.expanded` + 折叠闭包。
  2. **两侧空白死区**：`px_8` 垫在 wrapper 上，列表 hitbox 只覆盖内容列，padding 区滚轮命不中列表 → px_8 挪进 List 元素自身（List impl Styled，内部布局已支持 padding），hitbox 覆盖全宽。
- **顺带**：read/diff 卡既有的折叠钮补了 `invalidate_chat_heights`（展开时也曾行重叠）。
- **验证**：54 测试全绿；17:56 二进制已部署。
- **下轮重点**：① 全域滚轮（正文/空白/卡片）顺滑；② 卡片 8 行折叠 + "… 其余 N 行"展开钮工作正常（点击展开、行高不叠）；③ 此前各轮验收项。

## 2026-08-31 · 第 13 轮（用户报告：只有 Bash 行上能滚页面）

- **根因**：三处工具卡片输出体（term-banner / term-out / io-scroll）**无条件 `.occlude()`**——gpui 的 occlude 把外层列表的 hitbox 从滚轮命中链剔除，卡片覆盖展开视图大半面积 → 滚轮大面积死区，只有 24px 的 Bash 头行是活区。注释显示当初是有意为之（"外层对话不跟滚"），观感差。
- **修复**：occlude 改**条件化**——`scroll_occlude(行数, max_h, 行高, 垂直padding)`：内容行数超出 max_h（真会发生内滚）才阻断外层；不足一屏保持穿透，页面随处可滚。被折行的长行会低估行数，取向是宁可少 occlude 保页面可滚。
- **顺带发现**：gpui div 的滚轮监听（paint_scroll_listener）不 stop_propagation，无 occlude 时内外层同滚——条件化后短卡片天然只剩外层滚，长卡片内滚+页面同滚，可接受。
- **验证**：54 测试全绿；17:42 二进制已部署。待用户手感确认。
- **下轮重点**：① 滚轮在卡片/正文/Think 行全域生效；② 长输出卡（>11 行）仍可内滚；③ 此前各轮验收项持续观察。

## 2026-08-31 · 第 12 轮（用户报告：展开内容后文字重叠）· 两次修复

- **真因一（结构性）**：展开态第一成员是 `control.child(render_entry(...))`——把**整个条目塞进 33px 固定高的控制行**当子元素，内容垂直溢出盖住后续所有行（web 中控制行与展开体是兄弟节点）。折叠态从未暴露（成员全是零高占位）。
- **真因二（虚拟列表）**：行高变化（展开/流式增长）未失效，列表按陈旧行高排布；第 11 轮之前被 TextView 防抖冻结正文高度所掩盖。
- **修复**：① 展开态改为 `v_flex().child(control).child(render_entry)` 兄弟结构；② `invalidate_chat_heights()`（splice 全量 Unmeasured 重测、保留滚动锚点）接入轮次组开合 / Think 行开合 / 流式 flush（渲染路径置 `heights_dirty` 脏标记，100ms tick 消费）。
- **验证**：54 测试全绿；17:28 二进制已部署。真机点击验证被背景网页动画干扰（帧持续过期），待用户点开轮次过程确认。
- **下轮重点**：① 展开轮次过程：控制行下方正常铺开成员、无重叠；② 流式期间行距正常；③ 内联代码内边距 backlog 待定。

## 2026-08-31 · 第 11 轮（用户报告：高亮文字无内边距 + 输出非流式一股脑出）

- **根因**：gpui 虚拟列表按**缓存的行高**排布条目；轮次过程展开/收起（成员条目零高占位 ↔ 完整渲染，高度大变）和流式文本增长（第 11 轮节流修复解冻了正文高度变化）都没知会列表 → 按陈旧行高排布 → 行间重叠。第 11 轮之前流式文本因 TextView 防抖冻结、高度恰好恒定，掩盖了这个问题。
- **修复**：
  1. 新增 `invalidate_chat_heights()`（splice 全范围标记 Unmeasured，重测且保留滚动锚点）；
  2. 接入三处高度变化点：轮次过程组开合、Think 行开合、流式文本 flush（渲染路径不能改列表，置 `heights_dirty` 脏标记，由 100ms tick 消费后统一失效 + notify）。
- **验证**：54 测试全绿；17:24 二进制已部署。
- **下轮重点**：① 展开轮次过程、开合 Think 后无重叠；② 长答案流式期间行间距正常、渐进渲染；③ 内联代码内边距 backlog 待用户定夺是否 vendor patch。

## 2026-08-31 · 第 11 轮（用户报告：高亮文字无内边距 + 输出非流式一股脑出）

- **查**：会话 ef3051bb 的 text-delta 时间戳证实 **adapter/agent 层是真流式**（1312 个 delta、每秒约 40 个、持续 19s）——问题在 UI 层。gpui-component TextView 的后台解析 Worker 是**重置式防抖**（`timer.set_after(200ms)` 在每条更新时重置）：连续 delta 下计时器永不触发，解析一次不跑，正文冻结到流停止后 200ms 一次性出现——正是"最后一股脑"。
- **修复（流式节流）**：AppView 增加 `stream_text_shown` 快照（RefCell），流式尾的 Text 块以 **400ms 节流**喂 TextView（间隔 > 上游 200ms 防抖窗，防抖必然触发），非尾块直接用现文；TurnStarted 清快照。正文将按 ~400ms 粒度渐进渲染。
- **backlog（上游样式，无法宿主侧修）**：内联代码背景 = `HighlightStyle.background_color`（node.rs:651，逐字符文字高亮），gpui 文字系统的背景天然无内边距，TextViewStyle 无旋钮。要改需 vendor patch gpui-component（node.rs 里给 code 高亮加左右 0.25rem padding 的思路）。观察用户是否在意，在意再做。
- **验证**：54 测试全绿；17:16 二进制已部署。
- **下轮重点**：① 长答案流式期间应 ~0.4s 粒度渐进出现（不再是最后一股脑）；② 高亮文字内边距是否可接受。

## 2026-08-31 · 第 10 轮（用户追问：只有正在执行的才该扫 + 执行中文字会动吗）

- **用户观察**：截图里历史步骤的 Think 行在扫光；问执行中的工具文字是否该动。
- **对照 web 参考语义**（ReasoningRow.tsx / GenericCommandCard.tsx / CommandNodeView）：
  1. Think 扫光条件 = `streaming && 该块是消息最后一个块`——文字流式期间扫；工具调用块出现即停止，扫光转移给工具行；
  2. Think 摘要文字运行时**跟随最新一行**（latestLine + scrollLeft 右贴），完成后回首行——文字确实会动；
  3. 工具行文字**不动**（仅扫光带）；工具行扫光 = 该工具 outcome 未决。
- **gpui 缺陷**：Think 活跃条件用了 `entry.done`——done 只在整轮结束打给最后一条，历史 Think 行整轮误扫。
- **修复**：Think 活跃改为"running 且本条是最后一条消息且本块是最后一个块"；Think 摘要运行时显示最新一行（对齐 latestLine 语义）。工具行无需改（per-tool result 判定本就正确）。
- **验证**：54 测试全绿；15:32 二进制已部署。
- **下轮重点**：① 多步对话中只有当前流的尾块扫光、历史 Think 行静止；② Think 摘要在流式期间跳动跟随最新行。

## 2026-08-31 · 第 9 轮（用户报告：工具执行时的"扫过"动效有问题）

- **用户报告**：工具执行过程中有"扫过"的动效，有问题。
- **真机观察（computer-use 驱动宿主跑了两轮对话取证）**：发现两个执行期缺陷——
  1. **扫光动画顿挫**：`row_sweep` 的位置由全局 100ms tick + 墙钟 elapsed 计算，扫光每秒只跳 10 格（web 参考是 60fps CSS ease-out 循环），观感为一次次顿挫的"扫过"。
  2. **每条 shell 命令弹出黑色控制台窗口**：dsh-shell 从 GUI 宿主 spawn bash/cmd 未设 CREATE_NO_WINDOW，每次工具执行闪一个 `bash.exe` 黑窗并抢前台（cmd 时代就有，是"执行过程观感差"的另一主犯）。
- **修复**：
  1. 扫光改为 gpui 原生动画驱动：`row_sweep_band()` + `with_animation(Animation::new(2600ms).repeat().with_easing(row_sweep_easing))`，每帧回调设置 left，Think/Tool 两处行接入，元素消失动画即停；不再依赖全局 tick。
  2. dsh-shell spawn 加 `CREATE_NO_WINDOW`（0x08000000），cmd 与 bash 两路都生效。
- **验证**：workspace 54 测试全绿；15:21 二进制已部署（restart-host.sh）。真机复验因用户正在使用前台未连续截屏，待下次使用确认。
- **下轮重点**：① 工具执行时扫光平滑循环、无黑窗闪现；② 第 7 轮路由修复在 header 中持续为 zai-coding-cn/glm-5.3-flash。

## 2026-08-31 · 第 8 轮（全绿验收，无需修改）

- **会话**：`--E-aiproject-deepseek-harness--/session-ef3051bb`（"最新版本做了哪些更新"，5 步 74.6s，6 调用，0 错误 0 重复 0 浪费）
- **全部验收通过**：
  1. **路由（第 7 轮）**：header = `zai-coding-cn/glm-5.3-flash`，对话真实可用（glm 输出 2370 tokens，结构化中文总结）；
  2. **bash-first（第 5 轮）**：模型自然使用 bash 语法全部成功——`;` 分隔、`--format='%s%n%b'` 百分号格式、单引号、`2>/dev/null`、管道，与第 5 轮 cmd 下 8 连错形成对照；
  3. **GBK（第 1 轮）**：无乱码；**glob** 用法正确（`**/CHANGELOG*` 干净返回 No files found）；**turn/end** 如实 `completed`（第 6 轮）。
- **本轮修改**：无——闭环首次达到"查无可查"状态。
- **backlog（非缺陷）**：glm 的 usage 未映射缓存字段（Zhipu 返回 prompt_tokens_details.cached_tokens，adapter 只映射 DeepSeek 字段），统计药丸缓存恒为 0；有需要再做。
- **继续观察**：第 4 轮历史回放在"打开旧会话"场景的表现；多轮对话下 header Change 事件。

## 2026-08-31 · 第 7 轮（用户报告：UI 显示 glm 实际走 deepseek）

- **用户报告**：宿主模型选择器显示 glm-5.3-flash，但 /loop 报的是 deepseek 402 余额不足——显示与实际路由不一致。
- **根因（两个叠加的 harness 缺陷）**：
  1. **启动路由短路**：只要存在 deepseek key（env/.credentials），启动解析就强制回 `deepseek/deepseek-chat`，从不调用 `set_provider_and_model`；而 UI 的 desired_model 从 settings 恢复为 glm-5.3-flash——显示与路由各算各的。
  2. **adopt 提供方不注册**：`zai-coding-cn` 在 settings 里没有 baseURL（web 由 pi-ai 目录补全），宿主 `!base_url.is_empty()` 才注册 adapter——即使路由选对也没有 adapter 可用。
- **修复**：① 启动路由改为**持久化用户选择优先**（startup_active/startup_desired），key 只决定 key 来源；deepseek 分支也应用路由；UI 的 desired_model 与路由同源（startup_model）。② 新增 `known_provider_base_url` 目录兜底（zai-coding-cn → open.bigmodel.cn/api/coding/paas/v4，zai → api.z.ai/api/coding/paas/v4，与 pi-ai 目录 1:1）。
- **验证**：workspace 54 测试全绿；已部署 14:52 二进制。
- **下轮重点**：① 新会话 header 应显示 `zai-coding-cn/glm-5.3-flash`；② 对话可用（glm key 有效）；③ bash-first、grep 相对路径、历史回放（第 4/5 轮）在真实对话中观察。

## 2026-08-31 · 第 6 轮

- **会话**：`--E-aiproject-deepseek-harness--/session-22697e10`（重问"最新版本做了哪些更新"，1 步 0.5s 即终）
- **查**：轮次秒终、零输出的根因是 **DeepSeek 余额不足**（HTTP 402 QUOTA，错误经 finish chunk 持久化，消息完全可读）——外部因素，需充值；UI 实时路径本就会弹 Role::Error 气泡。
- **验收（第 5 轮信号）**：header 照常落盘（system 18.6k 含 AGENTS.md 注入）；本会话无 shell 调用，bash-first 与 grep 修复留待下次有实际工作的对话观察。
- **改（harness 审计）**：
  1. `event_to_web_line` 曾把 turn/end reason **硬编码为 "completed"**——失败轮次在日志里永远"成功"，doctor 无法统计失败、回放无法显示。改为序列化真实 TurnEndReason；读侧对称还原，旧格式兼容。
  2. `rebuild_from_session` 补 TurnEnd 分支：Error 轮次在历史回放中渲染 `[CODE] message` 错误条目（此前重开历史只见用户气泡凭空没下文）。
- **验证**：workspace 54 测试全绿（含 error reason 往返 + 旧格式兼容测试）；已用 restart-host.sh 部署 14:38 二进制。
- **下轮重点**：① 充值后跑一个有 shell 工作的对话，确认 bash 风味下错误归零、grep 相对路径正常；② 失败轮次在日志中 reason=error 且历史回放可见错误条目。

## 2026-08-31 · 第 5 轮（首场真实对话验收 + 新问题）

- **会话**：`--E-aiproject-deepseek-harness--/session-19fbf3dd`（"帮我看看最新版本做了哪些更新"，26 步 50s，8 错误 6 重试链）
- **验收通过（第 1/2 轮修复全部到达模型）**：
  1. doctor 报出 `request headers: 1, deepseek/deepseek-chat, system=18.7k, tools=7`；header 内容 5 标记全中（Workspace root / cmd /C / AGENTS.md 注入 / 禁止重跑纪律）；
  2. GBK 修复生效：`系统找不到指定的路径` 在日志里完全可读（无 U+FFFD）；
  3. 无 `cd /e/...` 试错；"长命令先落盘再检视"纪律模型已在用（步 15/17/20：重定向到 all_merges.txt 后用 fs 读）。
- **新问题（本轮改）**：
  1. 8 个错误全是 **bash 语法经 cmd /C 碎裂**：`;` 变 git 参数、`--format="%ci %d %s"` 引号被 cmd 剥掉致 `%d` 成参数、findstr 引号分裂、`/tmp` 不存在 → **dsh-shell Windows 优先 Git Bash**（PATH → Git\bin\bash.exe 安装位次探测，找不到才退 cmd /C），公开 `shell_kind()`；工具描述与 tool:shell section 按运行时风味自适应。
  2. `grep 相对路径报找不到`：GrepTool/GlobTool 传 `path` 时按进程 cwd 解析、未按会话工作区 → 均改为 `workdir.resolve`（与工具描述承诺一致），补回归测试。
- **验证**：workspace 53 测试全绿（含 bash 三件套 `;`+引号+% 端到端测试）；gpui check 通过。
- **流程改进（用户反馈驱动）**："不要每次都让我手动重启"——新增 `scripts/restart-host.sh`（会话静默检查 → 强杀旧宿主 → 重建 → 拉起校验），以后换二进制由 agent 直接执行，已部署 14:21 二进制。
- **下轮重点**：① shell 错误数应归零（bash 风味下 `;`/引号/% 自然生效）；② grep/glob 相对路径不再误报；③ 历史回放（第 4 轮修复）用户新会话打开即完整；④ doctor 的"重复命令"对"同基命令不同管道"会过报（如步 7-10 是合理变体），观察是否需要只对完整命令告警。

## 2026-08-31 · 第 4 轮（用户报告驱动）

- **用户报告**：打开历史会话（harness 1b67362a），对话区只剩用户气泡、轨迹显示"本轮还没有工具调用记录"。
- **根因（harness/gpui）**：`rebuild_from_session` 回放历史时 `AssistantMessage` 分支只更新统计、从不把内容入列——实时对话靠 chunk 流增量入列掩盖了这一点；且 `UserMessage` 分支两处误用 `return`（应为 `continue`），遇空用户消息或上下文注入行会截断其后全部回放。
- **修复**：① AssistantMessage 回放入列（Text/Reasoning/ToolCall → Role::Assistant 条目，ToolCall 计入 stats_tools，供轨迹与 ToolResult 附着）；② 两处 `return` 改 `continue`；③ 补数据层回归测试（assistant tool-call 块落盘→读回不丢，dsh-persist）。
- **验证**：workspace 50 测试全绿；blocks_from_web 对 "tool-call" 的解析本就完好（读链无损），缺口纯在 UI 回放分支。
- **待办（用户动作）**：宿主正运行旧新混合版（13:05），exe 被锁——**关闭宿主后重新 build/run**，重开 1b67362a 应能看到完整对话与工具轨迹。
- **下轮重点**：① 历史回放：对话区有助手文本+工具行、轨迹有记录、上下文注入行不再截断后续；② 第 1/2 轮验收信号（header、cargo 不重跑、无乱码）继续观察。

## 2026-08-31 · 第 3 轮

- **查**：磁盘上最新会话仍是 12:28 的 harness 会话——新宿主 13:06 启动后**尚无新对话落盘**，无新内容可分析；前两轮修复的实弹验收仍未发生。
- **改（测试缝）**：为「header 发射→落盘」补端到端集成测试 `dsh-agent-loop/tests/persist_header.rs`（mock 一轮对话走真实 SessionRecorder 写盘，断言 request/header 存在且 system 完整）——新宿主会不会写 header 不再靠猜，链路已证明通畅。
- **验证**：workspace 49 测试全绿；gpui check 通过。
- **下轮重点（第 1 轮修复 + 第 2 轮 header 的正式验收，等用户在宿主里用一轮后）**：
  1. doctor 报告出现 `request headers ≥ 1`，system 含 `Workspace root:` 与目标项目 AGENTS.md 内容；
  2. 模型不再手动 `fs read AGENTS.md`（注入生效后属冗余信号）；
  3. shell 场景：无 `cd /e/...`、无 U+FFFD 乱码、cargo 类命令无重复执行；
  4. fs 读目录报 `is a directory; use op=list` 而非 os error 5。

## 2026-08-31 · 第 2 轮

- **会话**：`--E-aiproject-deepseek-harness--/session-1b67362a`（「熟悉一下」，6 步 15s，0 错误 0 重复 0 浪费）
- **查**：机械指标全绿；全程未用 shell（fs/glob 为主，glob 用法正确），意图达成、总结完整。
- **基线比对**：第 1 轮的 4 个信号（cargo 重跑 / GBK 乱码 / cd /e/… / fs 读目录）均未复发，但该会话没碰 shell，且宿主仍是旧二进制——**第 1 轮修复尚未被真实验证**。
- **发现与修复（harness）**：`event_to_web_line` 把 `RequestHeader` 落进 `_ => None`，「模型实际看到的 system/tools/config」从未落盘 → 「查」无法验证 prompt 修复是否生效。已在持久层补 `request/header` 写入 + `web_line_to_event` 对称读回 + 往返单测；doctor 新增 header 审计行（model / system 字符数 / 工具数 / reason）。
- **验证**：workspace 48 测试全绿；doctor 对合成 header 行正确显示 `deepseek/test-model tools=2 reason=initial`。
- **待办（用户动作）**：宿主 exe 被运行中的进程锁定，无法热替换——**关闭 gpui 宿主后重新 build/run**，使第 1 轮的 prompt sections + 本轮的 header 落盘同时生效。
- **下轮重点比对**：① doctor 应能看到 `request headers ≥1`，且 system 里含 `Workspace root:` 与 AGENTS.md 内容（验证两轮 prompt 修复到达模型）；② 模型是否还手动 `fs read AGENTS.md`（注入生效后属冗余）；③ 信号 1–4 持续观察。

## 2026-08-31 · 第 1 轮（基线）

- **会话**：`--E-rustproject-miaocr--/session-622a2761`（「熟悉一下」，20 步 273s，浪费 77.4s / 3 错误）
- **发现与修复**：
  1. `cargo test --workspace` 重跑 3 遍（浪费 77s）→ prompt 缺口：新增 `tool:shell` section（长命令先落盘再检视纪律）。
  2. `cd /e/...` 报 GBK 乱码无法自诊断 → harness：dsh-shell 输出 UTF-8 失败回退系统代码页（encoding_rs + codepage）；工具描述与 prompt 写明免 cd + Windows 路径。
  3. fs read 目录误报 os error 5 → harness：dsh-fs 显式提示 `use op=list`。
  4. 项目探索绕路、无 AGENTS.md 注入机制 → harness：dsh-system-prompt 新增 agent_instructions 模块 + `workspace:instructions` 动态 section；miaocr 补 AGENTS.md。
  5. 工具：新增 `session_doctor`（--latest / 扫描 / 单会话三种模式）。
- **验证**：rustdsh workspace 47 测试全绿；doctor 在该会话上复现全部三类问题。
- **下轮重点比对**：cargo 类命令是否还重复执行；cmd 中文报错是否可读（无 U+FFFD）；是否还出现 `cd /e/...` 或 fs 读目录的 os error 5。
