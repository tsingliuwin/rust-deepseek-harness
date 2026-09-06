# 上游更新分析法（模式 A 详细步骤）

上游仓库：`E:\aiproject\deepseek-harness`（git）。rustdsh 是其 **web 前端（packages/client/*）+ 存储行为（storage/session）+ llm-deepseek 适配层** 的 Rust/GPUI 1:1 复刻。
> **当前同步点：dsh-v0.1.3-alpha.1（d347e70390）**——2026-09-06 同步。此前 76fda729（0.1.2-rc.1，对 alpha.5 零功能变更）。
> 0.1.3-alpha.1 主面：会话日志格式 v2（breaking）+ 通用文件附件 + 可点击链接语言 + skill 芯片（rustdsh 面外）。
> **最近检查：2026-09-06（定时轮 #1）**——上游 pull 后 HEAD 仍 d347e70390（= master HEAD = 发布点），五段 diff 全空，零功能变更，无动作。

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
